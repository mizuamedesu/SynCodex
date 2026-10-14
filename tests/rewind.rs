use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tempfile::TempDir;

const ID: &str = "thread-example";
const TS: &str = "2026-10-14T00:00:00Z";

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_syncodex"))
        .arg("--codex-home")
        .arg(home)
        .env_remove("CODEX_SQLITE_HOME")
        .env_remove("CODEX_THREAD_ID")
        .args(args)
        .output()
        .unwrap()
}

fn success(home: &Path, args: &[&str]) -> Value {
    let output = run(home, args);
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

fn record(kind: &str, payload: Value) -> Value {
    json!({"timestamp":TS,"type":kind,"payload":payload})
}

fn meta(id: &str) -> Value {
    record(
        "session_meta",
        json!({"id":id,"session_id":id,"cwd":"/work/project","timestamp":TS,"history_mode":"paginated"}),
    )
}

fn model(role: &str, id: &str, text: &str, turn: &str, phase: Option<&str>) -> Value {
    let mut result = record(
        "response_item",
        json!({"type":"message","id":id,"role":role,
        "content":[{"type":if role == "user" {"input_text"} else {"output_text"},"text":text}],
        "internal_chat_message_metadata_passthrough":{"turn_id":turn,"content_item_kinds":[if role == "user" {"user.text"} else {"unknown"}]}}),
    );
    if let Some(phase) = phase {
        result["payload"]["phase"] = phase.into();
    }
    result
}

fn item(role: &str, id: &str, text: &str, turn: &str, phase: Option<&str>) -> Value {
    let mut result = record(
        "event_msg",
        json!({"type":"item_completed","thread_id":ID,"turn_id":turn,
        "item":{"type":if role == "user" {"UserMessage"} else {"AgentMessage"},"id":id,
            "content":[{"type":if role == "user" {"text"} else {"Text"},"text":text}]},
        "started_at_ms":1791936000000i64,"completed_at_ms":1791936000000i64}),
    );
    if let Some(phase) = phase {
        result["payload"]["item"]["phase"] = phase.into();
    }
    result
}

fn conversation(id: &str) -> Vec<Value> {
    let mut user = model("user", "model-user-1", "はじめの依頼", "turn-1", None);
    user["payload"]["content"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"input_image","image_url":"data:image/png;base64,TEST"}));
    let mut ui_user = item("user", "ui-user-1", "はじめの依頼", "turn-1", None);
    ui_user["payload"]["item"]["content"][0]["text_elements"] =
        json!([{"byte_range":{"start":0,"end":3},"placeholder":"x"}]);
    ui_user["payload"]["item"]["content"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"local_image","path":"/attachments/image.png"}));
    vec![
        meta(id),
        record(
            "event_msg",
            json!({"type":"task_started","turn_id":"turn-1"}),
        ),
        record(
            "response_item",
            json!({"type":"message","role":"system","content":[{"type":"input_text","text":"system instructions"}]}),
        ),
        record(
            "response_item",
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>private config</environment_context>"}],
            "internal_chat_message_metadata_passthrough":{"content_item_kinds":["environments.environment_context"]}}),
        ),
        record(
            "turn_context",
            json!({"turn_id":"turn-1","cwd":"/work/project"}),
        ),
        user,
        ui_user,
        item(
            "assistant",
            "assistant-1",
            "途中の回答",
            "turn-1",
            Some("commentary"),
        ),
        model(
            "assistant",
            "assistant-1",
            "途中の回答",
            "turn-1",
            Some("commentary"),
        ),
        record(
            "response_item",
            json!({"type":"function_call","call_id":"tool-1","name":"exec","arguments":"{}"}),
        ),
        record(
            "response_item",
            json!({"type":"function_call_output","call_id":"tool-1","output":"future tool output"}),
        ),
        item(
            "assistant",
            "assistant-2",
            "最初の最終回答",
            "turn-1",
            Some("final_answer"),
        ),
        model(
            "assistant",
            "assistant-2",
            "最初の最終回答",
            "turn-1",
            Some("final_answer"),
        ),
        record(
            "event_msg",
            json!({"type":"task_complete","turn_id":"turn-1"}),
        ),
        record(
            "compacted",
            json!({"message":"future summary","replacement_history":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"future summary"}]}]}),
        ),
        record(
            "event_msg",
            json!({"type":"task_started","turn_id":"turn-2"}),
        ),
        record(
            "turn_context",
            json!({"turn_id":"turn-2","cwd":"/work/project"}),
        ),
        model("user", "model-user-2", "後続の依頼", "turn-2", None),
        item("user", "ui-user-2", "後続の依頼", "turn-2", None),
        item(
            "assistant",
            "assistant-3",
            "後続の回答",
            "turn-2",
            Some("final_answer"),
        ),
        model(
            "assistant",
            "assistant-3",
            "後続の回答",
            "turn-2",
            Some("final_answer"),
        ),
        record(
            "event_msg",
            json!({"type":"task_complete","turn_id":"turn-2"}),
        ),
    ]
}

fn write_rollout(path: &Path, values: &mut [Value]) -> Vec<u8> {
    let start = values[0]["payload"]["history_base"]["end_ordinal_exclusive"]
        .as_u64()
        .unwrap_or(0);
    let paginated = values[0]["payload"]["history_mode"] == "paginated";
    let mut bytes = Vec::new();
    for (i, value) in values.iter_mut().enumerate() {
        if paginated {
            value["ordinal"] = (start + i as u64).into();
        }
        bytes.extend(serde_json::to_vec(value).unwrap());
        bytes.push(b'\n');
    }
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, &bytes).unwrap();
    bytes
}

struct Fixture {
    _temp: TempDir,
    home: PathBuf,
    rollout: PathBuf,
}
fn fixture(databases: bool) -> Fixture {
    let temp = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
    let home = temp.path().join("home");
    let rollout = home.join("sessions/2026/10/14/rollout-example.jsonl");
    write_rollout(&rollout, &mut conversation(ID));
    fs::write(
        home.join("history.jsonl"),
        format!(
            "{}\n{}\n{}\n",
            json!({"session_id":ID,"ts":1791936000,"text":"はじめの依頼"}),
            json!({"session_id":"other","ts":1791936000,"text":"他の会話"}),
            json!({"session_id":ID,"ts":1791936000,"text":"後続の依頼"})
        ),
    )
    .unwrap();
    fs::write(
        home.join("session_index.jsonl"),
        format!(
            "{}\n",
            json!({"id":ID,"thread_name":"会話の名前","updated_at":TS})
        ),
    )
    .unwrap();
    fs::write(home.join("auth.json"), "private credentials").unwrap();
    if databases {
        let db = Connection::open(home.join("state_5.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT, title TEXT, preview TEXT, first_user_message TEXT, project_id TEXT, is_pinned INTEGER, tokens_used INTEGER, updated_at INTEGER, updated_at_ms INTEGER, history_mode TEXT); CREATE TABLE backfill_state (id INTEGER PRIMARY KEY, status TEXT, last_watermark TEXT, last_success_at INTEGER, updated_at INTEGER); INSERT INTO backfill_state VALUES (1, 'complete', 'watermark', 3, 4);").unwrap();
        db.execute("INSERT INTO threads VALUES (?, ?, ?, ?, ?, 'project-private', 1, 999, 4, 4000, 'paginated')",
            rusqlite::params![ID, rollout.to_str().unwrap(), "はじめの依頼", "はじめの依頼", "はじめの依頼"]).unwrap();
        db.execute("INSERT INTO threads VALUES ('other', '/other/path', 'other title', 'other preview', 'other first', 'other project', 0, 100, 5, 5000, 'paginated')", []).unwrap();
        let db = Connection::open(home.join("thread_history_1.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE thread_items (thread_id TEXT, item_json TEXT); CREATE TABLE thread_turns (thread_id TEXT, turn_id TEXT); CREATE TABLE thread_history_projection_state (thread_id TEXT PRIMARY KEY, next_rollout_byte_offset INTEGER, next_rollout_ordinal INTEGER); CREATE TABLE thread_realtime_items (thread_id TEXT, item_json TEXT);").unwrap();
        for table in ["thread_items", "thread_turns", "thread_realtime_items"] {
            db.execute(
                &format!(
                    "INSERT INTO {table} VALUES (?, 'stale later data'), ('other', 'untouched')"
                ),
                [ID],
            )
            .unwrap();
        }
        db.execute(
            "INSERT INTO thread_history_projection_state VALUES (?, 99999, 999), ('other', 10, 2)",
            [ID],
        )
        .unwrap();
    }
    Fixture {
        _temp: temp,
        home,
        rollout,
    }
}

fn records(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}

fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .map(Result::unwrap)
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            (
                e.path().strip_prefix(root).unwrap().to_owned(),
                fs::read(e.path()).unwrap(),
            )
        })
        .collect()
}

#[test]
fn lists_local_named_sessions_and_message_pairs_without_starting_codex() {
    let f = fixture(true);
    let list = success(&f.home, &["sessions", "--json"]);
    assert_eq!(list[0]["id"], ID);
    assert_eq!(list[0]["name"], "会話の名前");
    assert_eq!(
        success(&f.home, &["sessions", "--search", "PROJECT", "--json"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        success(&f.home, &["sessions", "--search", "missing", "--json"]),
        json!([])
    );
    let messages = success(&f.home, &["messages", ID, "--json"]);
    assert_eq!(messages.as_array().unwrap().len(), 5);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["text"], "はじめの依頼");
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["text"], "途中の回答");
    assert_eq!(messages[4]["index"], 5);
}

#[test]
fn user_edit_truncates_model_and_display_history_and_recovers_originals() {
    let f = fixture(true);
    let original = fs::read(&f.rollout).unwrap();
    let result = success(
        &f.home,
        &[
            "rewind",
            ID,
            "--message",
            "1",
            "--text",
            "変更後の依頼\n二行目",
        ],
    );
    assert_eq!(result["removed_messages"], 4);
    assert_eq!(result["kept_messages"], 1);
    let values = records(&f.rollout);
    assert_eq!(values.len(), 8);
    assert_eq!(values[5]["payload"]["role"], "user");
    assert_eq!(
        values[5]["payload"]["content"][0]["text"],
        "変更後の依頼\n二行目"
    );
    assert_eq!(values[5]["payload"]["content"][1]["type"], "input_image");
    assert_eq!(
        values[6]["payload"]["item"]["content"][0]["text"],
        "変更後の依頼\n二行目"
    );
    assert_eq!(
        values[6]["payload"]["item"]["content"][0]["text_elements"],
        json!([])
    );
    assert_eq!(
        values[6]["payload"]["item"]["content"][1]["path"],
        "/attachments/image.png"
    );
    assert_eq!(values.last().unwrap()["payload"]["type"], "turn_aborted");
    assert_eq!(values.last().unwrap()["ordinal"], 7);
    let original_prefix: Vec<_> = original
        .split_inclusive(|b| *b == b'\n')
        .take(5)
        .flatten()
        .copied()
        .collect();
    assert!(fs::read(&f.rollout).unwrap().starts_with(&original_prefix));
    assert!(!fs::read_to_string(&f.rollout).unwrap().contains("future"));
    let history = records(&f.home.join("history.jsonl"));
    assert_eq!(history.len(), 2);
    assert_eq!(history[0]["session_id"], "other");
    assert_eq!(history[1]["text"], "変更後の依頼\n二行目");
    let db = Connection::open(f.home.join("state_5.sqlite")).unwrap();
    let row: (String, String, String, i64) = db
        .query_row(
            "SELECT first_user_message, preview, project_id, is_pinned FROM threads WHERE id = ?",
            [ID],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        row,
        (
            "変更後の依頼\n二行目".into(),
            "変更後の依頼\n二行目".into(),
            "project-private".into(),
            1
        )
    );
    assert_eq!(
        db.query_row("SELECT title FROM threads WHERE id = ?", [ID], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "変更後の依頼\n二行目"
    );
    assert_eq!(
        db.query_row("SELECT status FROM backfill_state", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "complete"
    );
    assert_eq!(
        db.query_row("SELECT title FROM threads WHERE id = 'other'", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        "other title"
    );
    let db = Connection::open(f.home.join("thread_history_1.sqlite")).unwrap();
    for table in [
        "thread_items",
        "thread_turns",
        "thread_history_projection_state",
        "thread_realtime_items",
    ] {
        assert_eq!(
            db.query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE thread_id = ?"),
                [ID],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE thread_id = 'other'"),
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }
    let recovery = Path::new(result["recovery"].as_str().unwrap());
    let journal: Vec<Value> =
        serde_json::from_slice(&fs::read(recovery.join("journal.json")).unwrap()).unwrap();
    assert_eq!(journal.len(), 4);
    let saved = journal
        .iter()
        .find(|v| v["target"] == f.rollout.to_str().unwrap())
        .unwrap();
    assert_eq!(
        fs::read(recovery.join(saved["backup"].as_str().unwrap())).unwrap(),
        original
    );
    let saved = journal
        .iter()
        .find(|v| v["target"] == f.home.join("state_5.sqlite").to_str().unwrap())
        .unwrap();
    let db = Connection::open(recovery.join(saved["backup"].as_str().unwrap())).unwrap();
    assert_eq!(saved["restore_scope"], "thread_rows");
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM threads", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT first_user_message FROM threads WHERE id = ?",
            [ID],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "はじめの依頼"
    );
    assert_eq!(
        fs::read_to_string(f.home.join("auth.json")).unwrap(),
        "private credentials"
    );
    assert_eq!(
        success(&f.home, &["messages", ID, "--json"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn assistant_can_be_edited_in_the_middle_of_a_turn_preserving_its_phase() {
    let f = fixture(false);
    let result = success(
        &f.home,
        &[
            "rewind",
            ID,
            "--message",
            "2",
            "--text",
            "書き直した途中の回答",
        ],
    );
    assert_eq!(result["role"], "assistant");
    assert_eq!(result["removed_messages"], 3);
    let values = records(&f.rollout);
    assert_eq!(
        values[7]["payload"]["item"]["content"][0]["text"],
        "書き直した途中の回答"
    );
    assert_eq!(
        values[8]["payload"]["content"][0]["text"],
        "書き直した途中の回答"
    );
    assert_eq!(values[8]["payload"]["phase"], "commentary");
    assert!(
        !values
            .iter()
            .any(|v| v["payload"]["type"] == "function_call" || v["type"] == "compacted")
    );
    assert_eq!(
        records(&f.home.join("history.jsonl")).last().unwrap()["text"],
        "はじめの依頼"
    );
}

#[test]
fn dry_run_and_bad_selection_do_not_write_and_text_file_preserves_utf8() {
    let f = fixture(true);
    let before = tree(&f.home);
    let result = success(
        &f.home,
        &[
            "rewind",
            ID,
            "--message",
            "4",
            "--text",
            "新しい依頼",
            "--dry-run",
        ],
    );
    assert_eq!(result["dry_run"], true);
    assert_eq!(result["recovery"], Value::Null);
    assert_eq!(tree(&f.home), before);
    for args in [
        vec!["rewind", ID, "--message", "0", "--text", "test"],
        vec!["rewind", ID, "--message", "6", "--text", "test"],
        vec!["rewind", "unknown", "--message", "1", "--text", "test"],
        vec!["rewind", ID, "--message", "1", "--text", ""],
        vec!["rewind"],
    ] {
        assert!(!run(&f.home, &args).status.success());
        assert_eq!(tree(&f.home), before);
    }
    let path = f._temp.path().join("edit.md");
    fs::write(&path, "あいう😀\n\n本文\n").unwrap();
    success(
        &f.home,
        &[
            "rewind",
            ID,
            "--message",
            "4",
            "--text-file",
            path.to_str().unwrap(),
        ],
    );
    assert_eq!(
        success(&f.home, &["messages", ID, "--json"])[3]["text"],
        "あいう😀\n\n本文\n"
    );
}

#[test]
fn invalid_rollout_or_database_fails_before_any_write() {
    for database in [false, true] {
        let f = fixture(database);
        if database {
            let db = Connection::open(f.home.join("thread_history_1.sqlite")).unwrap();
            db.execute("DROP TABLE thread_history_projection_state", [])
                .unwrap();
        } else {
            fs::OpenOptions::new()
                .append(true)
                .open(&f.rollout)
                .unwrap();
            let mut bytes = fs::read(&f.rollout).unwrap();
            bytes.extend_from_slice(b"{broken");
            fs::write(&f.rollout, bytes).unwrap();
        }
        let before = tree(&f.home);
        assert!(
            !run(&f.home, &["rewind", ID, "--message", "1", "--text", "test"])
                .status
                .success()
        );
        assert_eq!(tree(&f.home), before);
    }
}

#[test]
fn compressed_rollout_is_resolved_from_its_uncompressed_database_path() {
    let f = fixture(true);
    let original = fs::read(&f.rollout).unwrap();
    let compressed = f.rollout.with_extension("jsonl.zst");
    fs::write(
        &compressed,
        zstd::stream::encode_all(original.as_slice(), 3).unwrap(),
    )
    .unwrap();
    fs::remove_file(&f.rollout).unwrap();
    // Another old rollout has the same logical ID, but a different physical path.
    let old = f.rollout.with_file_name("rollout-old.jsonl.zst");
    fs::write(
        &old,
        zstd::stream::encode_all(original.as_slice(), 3).unwrap(),
    )
    .unwrap();
    let old_bytes = fs::read(&old).unwrap();
    let listed = success(&f.home, &["sessions", "--json"]);
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["path"], compressed.to_str().unwrap());
    success(
        &f.home,
        &[
            "rewind",
            ID,
            "--message",
            "1",
            "--text",
            "compressed edited",
        ],
    );
    assert_eq!(fs::read(&old).unwrap(), old_bytes);
    let messages = success(&f.home, &["messages", ID, "--json"]);
    assert_eq!(messages.as_array().unwrap().len(), 1);
    assert_eq!(messages[0]["text"], "compressed edited");
}

#[test]
fn compressed_plain_duplicates_are_rejected_even_when_the_database_selects_one() {
    let f = fixture(true);
    let bytes = fs::read(&f.rollout).unwrap();
    fs::write(
        f.rollout.with_extension("jsonl.zst"),
        zstd::stream::encode_all(bytes.as_slice(), 3).unwrap(),
    )
    .unwrap();
    let before = tree(&f.home);
    let output = run(&f.home, &["rewind", ID, "--message", "1", "--text", "test"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Compressed/plain rollout conflict"));
    assert_eq!(tree(&f.home), before);
}

#[test]
fn compressed_archived_sessions_can_be_listed_and_rewound() {
    let f = fixture(false);
    let path = f.home.join("archived_sessions/rollout-archive.jsonl.zst");
    let plain = f._temp.path().join("plain.jsonl");
    let bytes = write_rollout(&plain, &mut conversation("archived"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        zstd::stream::encode_all(bytes.as_slice(), 3).unwrap(),
    )
    .unwrap();
    assert_eq!(
        success(&f.home, &["sessions", "--json"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        success(&f.home, &["sessions", "--archived", "--json"])
            .as_array()
            .unwrap()
            .len(),
        2
    );
    success(
        &f.home,
        &[
            "rewind",
            "archived",
            "--message",
            "3",
            "--text",
            "アーカイブの修正",
        ],
    );
    let bytes = zstd::stream::decode_all(fs::read(path).unwrap().as_slice()).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains("アーカイブの修正"));
    assert!(!text.contains("後続の依頼"));
}

fn fork(home: &Path, id: &str, parent: &str, parent_path: &Path, through: usize) -> PathBuf {
    let source = fs::read(parent_path).unwrap();
    let prefix: Vec<_> = source
        .split_inclusive(|b| *b == b'\n')
        .take(through)
        .flatten()
        .copied()
        .collect();
    let last: Value = serde_json::from_slice(
        prefix
            .split(|b| *b == b'\n')
            .rfind(|s| !s.is_empty())
            .unwrap(),
    )
    .unwrap();
    let mut values = vec![
        meta(id),
        record(
            "event_msg",
            json!({"type":"task_started","turn_id":format!("turn-{id}")}),
        ),
        model(
            "user",
            &format!("model-{id}"),
            &format!("{id} の依頼"),
            &format!("turn-{id}"),
            None,
        ),
        item(
            "user",
            &format!("ui-{id}"),
            &format!("{id} の依頼"),
            &format!("turn-{id}"),
            None,
        ),
    ];
    values[0]["payload"]["history_base"] = json!({"thread_id":parent,"end_ordinal_exclusive":last["ordinal"].as_u64().unwrap()+1,"end_byte_offset":prefix.len()});
    let path = home.join(format!("sessions/rollout-{id}.jsonl"));
    write_rollout(&path, &mut values);
    path
}

#[test]
fn rewinding_a_source_preserves_frozen_prefixes_in_all_descendant_forks() {
    let f = fixture(false);
    let child = fork(&f.home, "child", ID, &f.rollout, 14);
    let grandchild = fork(&f.home, "grandchild", "child", &child, 4);
    let child_before = success(&f.home, &["messages", "child", "--json"]);
    let grandchild_before = success(&f.home, &["messages", "grandchild", "--json"]);
    let result = success(
        &f.home,
        &["rewind", ID, "--message", "1", "--text", "元会話の編集"],
    );
    assert_eq!(result["preserved_forks"], 2);
    assert_eq!(
        success(&f.home, &["messages", "child", "--json"]),
        child_before
    );
    assert_eq!(
        success(&f.home, &["messages", "grandchild", "--json"]),
        grandchild_before
    );
    for path in [&child, &grandchild] {
        let values = records(path);
        assert!(values[0]["payload"]["history_base"].is_null());
        for (i, value) in values.iter().enumerate() {
            assert_eq!(value["ordinal"], i);
        }
        assert!(fs::read_to_string(path).unwrap().contains("はじめの依頼"));
    }
}

#[test]
fn editing_an_inherited_message_changes_only_the_selected_fork() {
    let f = fixture(false);
    let original = fs::read(&f.rollout).unwrap();
    let child = fork(&f.home, "child", ID, &f.rollout, 14);
    let result = success(
        &f.home,
        &[
            "rewind",
            "child",
            "--message",
            "2",
            "--text",
            "分岐内だけの修正",
        ],
    );
    assert_eq!(result["kept_messages"], 2);
    assert_eq!(fs::read(&f.rollout).unwrap(), original);
    assert_eq!(records(&child)[0]["payload"]["id"], "child");
    let messages = success(&f.home, &["messages", "child", "--json"]);
    assert_eq!(messages.as_array().unwrap().len(), 2);
    assert_eq!(messages[1]["text"], "分岐内だけの修正");
}

#[test]
fn malformed_inherited_cutoffs_are_rejected_before_writing() {
    let f = fixture(false);
    let child = fork(&f.home, "child", ID, &f.rollout, 14);
    let mut values = records(&child);
    values[0]["payload"]["history_base"]["end_byte_offset"] = 1.into();
    write_rollout(&child, &mut values);
    let before = tree(&f.home);
    assert!(
        !run(&f.home, &["rewind", ID, "--message", "1", "--text", "test"])
            .status
            .success()
    );
    assert_eq!(tree(&f.home), before);
}

#[test]
fn legacy_history_pairs_are_edited_and_later_compaction_is_removed() {
    let f = fixture(false);
    let mut head = meta(ID);
    head["payload"]
        .as_object_mut()
        .unwrap()
        .remove("history_mode");
    let mut values = vec![
        head,
        record(
            "event_msg",
            json!({"type":"user_message","message":"legacy request","images":[],"local_images":[]}),
        ),
        model(
            "user",
            "legacy-model",
            "legacy request",
            "legacy-turn",
            None,
        ),
        model(
            "assistant",
            "legacy-assistant",
            "legacy reply",
            "legacy-turn",
            None,
        ),
        record(
            "event_msg",
            json!({"type":"agent_message","message":"legacy reply"}),
        ),
        record("compacted", json!({"message":"later obsolete summary"})),
    ];
    write_rollout(&f.rollout, &mut values);
    assert_eq!(
        success(&f.home, &["messages", ID, "--json"])
            .as_array()
            .unwrap()
            .len(),
        2
    );
    success(
        &f.home,
        &["rewind", ID, "--message", "2", "--text", "legacy edited"],
    );
    let values = records(&f.rollout);
    assert_eq!(values[3]["payload"]["content"][0]["text"], "legacy edited");
    assert_eq!(values[4]["payload"]["message"], "legacy edited");
    assert!(!values.iter().any(|v| v["type"] == "compacted"));
    assert!(values.iter().all(|v| v["ordinal"].is_null()));
}

#[test]
fn already_rolled_back_turns_stay_hidden() {
    let f = fixture(false);
    let mut values = conversation(ID);
    values.push(record(
        "event_msg",
        json!({"type":"thread_rolled_back","num_turns":1}),
    ));
    values.push(record(
        "event_msg",
        json!({"type":"task_started","turn_id":"turn-3"}),
    ));
    values.push(model("user", "user-3", "新たな分岐", "turn-3", None));
    values.push(item("user", "ui-3", "新たな分岐", "turn-3", None));
    write_rollout(&f.rollout, &mut values);
    let messages = success(&f.home, &["messages", ID, "--json"]);
    assert_eq!(messages.as_array().unwrap().len(), 4);
    assert_eq!(messages[3]["text"], "新たな分岐");
    success(
        &f.home,
        &["rewind", ID, "--message", "4", "--text", "新分岐の修正"],
    );
    let messages = success(&f.home, &["messages", ID, "--json"]);
    assert_eq!(messages.as_array().unwrap().len(), 4);
    assert!(!messages.to_string().contains("後続の依頼"));
}

#[test]
fn write_lock_is_shared_with_inject() {
    let f = fixture(false);
    fs::create_dir(f.home.join(".syncodex")).unwrap();
    let lock = fs::File::create(f.home.join(".syncodex/inject.lock")).unwrap();
    fs2::FileExt::try_lock_exclusive(&lock).unwrap();
    let before = tree(&f.home);
    let output = run(&f.home, &["rewind", ID, "--message", "1", "--text", "test"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Another SynCodex write"));
    assert_eq!(tree(&f.home), before);
}

fn native_lock(home: &Path, id: &str) -> fs::File {
    let directory = home.join("thread-writer-locks");
    fs::create_dir_all(&directory).unwrap();
    let file = fs::File::create(directory.join(format!("{id}.lock"))).unwrap();
    file.try_lock().unwrap();
    file
}

#[test]
fn busy_target_is_rejected_but_another_codex_session_can_stay_open() {
    let f = fixture(true);
    let target_lock = native_lock(&f.home, ID);
    let before = tree(&f.home);
    let listed = success(&f.home, &["sessions", "--json"]);
    assert_eq!(listed[0]["in_use"], true);
    let output = run(&f.home, &["rewind", ID, "--message", "1", "--text", "test"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("is in use by Codex"));
    assert_eq!(tree(&f.home), before);
    drop(target_lock);
    // A stale lock file is not a running session.
    assert_eq!(
        success(&f.home, &["sessions", "--json"])[0]["in_use"],
        false
    );
    let _other_lock = native_lock(&f.home, "other");
    success(
        &f.home,
        &[
            "rewind",
            ID,
            "--message",
            "1",
            "--text",
            "allowed with another session open",
        ],
    );
    assert_eq!(
        success(&f.home, &["messages", ID, "--json"])[0]["text"],
        "allowed with another session open"
    );
    assert!(
        !f.home
            .join("thread-writer-locks")
            .join(format!("{ID}.lock"))
            .exists()
    );
    let db = Connection::open(f.home.join("state_5.sqlite")).unwrap();
    assert_eq!(
        db.query_row("SELECT status FROM backfill_state", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "complete"
    );
}

#[test]
fn busy_dependent_fork_is_rejected_before_changing_its_source() {
    let f = fixture(false);
    fork(&f.home, "child", ID, &f.rollout, 14);
    let _child_lock = native_lock(&f.home, "child");
    let before = tree(&f.home);
    let output = run(&f.home, &["rewind", ID, "--message", "1", "--text", "test"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Session child is in use"));
    assert_eq!(tree(&f.home), before);
}

#[test]
fn input_history_keeps_the_inode_used_by_another_open_appender() {
    let f = fixture(true);
    let history = f.home.join("history.jsonl");
    let mut appender = fs::OpenOptions::new().append(true).open(&history).unwrap();
    success(
        &f.home,
        &["rewind", ID, "--message", "1", "--text", "edited"],
    );
    appender.try_lock().unwrap();
    writeln!(appender, "{}", json!({"session_id":"other","ts":1791936001,"text":"appended after rewind on the original descriptor"})).unwrap();
    appender.sync_all().unwrap();
    drop(appender);
    let entries = records(&history);
    assert_eq!(
        entries.last().unwrap()["text"],
        "appended after rewind on the original descriptor"
    );
    assert!(
        entries
            .iter()
            .any(|r| r["session_id"] == ID && r["text"] == "edited")
    );
}
