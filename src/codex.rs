use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

struct Server {
    child: Child,
    input: ChildStdin,
    messages: Receiver<std::io::Result<String>>,
    next_id: u64,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Server {
    fn start(home: &Path, sqlite_home: &Path, executable: &str) -> Result<Self> {
        let mut child = Command::new(executable)
            .args(["app-server", "--stdio"])
            .env("CODEX_HOME", home)
            .env("CODEX_SQLITE_HOME", sqlite_home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("Cannot start Codex app-server")?;
        let input = child.stdin.take().context("Missing stdin")?;
        let output = child.stdout.take().context("Missing stdout")?;
        let (sender, messages) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let mut server = Self {
            child,
            input,
            messages,
            next_id: 0,
        };
        server.rpc("initialize", json!({"clientInfo":{"name":"syncodex","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}))?;
        writeln!(server.input, "{}", json!({"method":"initialized"}))?;
        server.input.flush()?;
        Ok(server)
    }
    fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        writeln!(
            self.input,
            "{}",
            json!({"id":self.next_id,"method":method,"params":params})
        )?;
        self.input.flush()?;
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .context("Codex RPC timed out")?;
            let line = self
                .messages
                .recv_timeout(remaining)
                .with_context(|| format!("Codex RPC {method} timed out or disconnected"))??;
            let response: Value = serde_json::from_str(&line)?;
            if response["id"] == self.next_id {
                if let Some(error) = response.get("error") {
                    bail!("Codex {method}: {error}");
                }
                return response
                    .get("result")
                    .cloned()
                    .context("Missing RPC result");
            }
            // Verification never approves server-initiated requests or executes a model turn.
            if response.get("id").is_some() && response.get("method").is_some() {
                writeln!(
                    self.input,
                    "{}",
                    json!({"id":response["id"],"error":{"code":-32601,"message":"Unsupported during verification"}})
                )?;
                self.input.flush()?;
            }
        }
    }
    fn pages(&mut self, method: &str, mut params: Value) -> Result<Vec<Value>> {
        let mut data = Vec::new();
        let mut seen = HashSet::new();
        loop {
            let result = self.rpc(method, params.clone())?;
            data.extend(
                result["data"]
                    .as_array()
                    .context("Missing page data")?
                    .iter()
                    .cloned(),
            );
            let Some(cursor) = result["nextCursor"].as_str() else {
                break;
            };
            ensure!(seen.insert(cursor.to_owned()), "Repeated pagination cursor");
            params["cursor"] = cursor.into();
        }
        Ok(data)
    }
}

pub fn verify(
    home: &Path,
    sqlite_home: &Path,
    executable: &str,
    thread_id: &str,
    expected: &[String],
) -> Result<()> {
    ensure!(home.is_dir(), "Restored Codex home does not exist");
    let mut server = Server::start(home, sqlite_home, executable)?;
    let before = server.rpc(
        "thread/read",
        json!({"threadId":thread_id,"includeTurns":false}),
    )?;
    ensure!(
        before["thread"]["id"] == thread_id,
        "Read returned a different thread"
    );
    let rollout_path = before["thread"]["path"]
        .as_str()
        .context("Codex did not report a local rollout path")?;
    ensure!(
        std::fs::canonicalize(rollout_path)?.starts_with(std::fs::canonicalize(home)?),
        "Codex is reading history outside the selected home"
    );
    let resumed = server.rpc("thread/resume", json!({"threadId":thread_id,"excludeTurns":true,"approvalPolicy":"never","sandbox":"read-only"}))?;
    ensure!(
        resumed["thread"]["id"] == thread_id,
        "Resume returned a different thread"
    );
    let turns = server.pages(
        "thread/turns/list",
        json!({"threadId":thread_id,"limit":100}),
    )?;
    let items = server.pages(
        "thread/items/list",
        json!({"threadId":thread_id,"limit":100}),
    )?;
    ensure!(
        !turns.is_empty() && !items.is_empty(),
        "Restored conversation has no turns or items"
    );
    let messages: Vec<_> = items
        .iter()
        .filter(|v| {
            matches!(
                v["item"]["type"].as_str(),
                Some("userMessage" | "agentMessage")
            )
        })
        .collect();
    let mut text = String::new();
    for message in &messages {
        collect_text(&message["item"], &mut text);
    }
    for phrase in expected {
        ensure!(
            text.contains(phrase),
            "Expected text absent from restored user/agent messages"
        );
    }
    let listed = server
        .pages("thread/list", json!({"modelProviders":[],"limit":100}))?
        .iter()
        .any(|t| t["id"] == thread_id);
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "thread_id":thread_id,"codex_home":home,"read":true,"resume":true,"listed":listed,"rollout_inside_home":true,
            "turns":turns.len(),"items":items.len(),"message_items":messages.len(),
            "expected_text_checks":expected.len(),"model_turn_sent":false
        }))?
    );
    Ok(())
}

fn collect_text(value: &Value, output: &mut String) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                if key == "text" {
                    if let Some(text) = value.as_str() {
                        output.push_str(text);
                        output.push('\n');
                    }
                } else {
                    collect_text(value, output);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_text(value, output);
            }
        }
        _ => {}
    }
}
