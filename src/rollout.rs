use crate::{sessions::Catalog, storage};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub(crate) struct Record {
    pub value: Value,
    raw: Vec<u8>,
    end: u64,
}

impl Record {
    pub fn new(value: Value) -> Self {
        Self {
            value,
            raw: Vec::new(),
            end: 0,
        }
    }
    pub fn edit(&mut self) -> &mut Value {
        self.raw.clear();
        &mut self.value
    }
}

pub(crate) fn parse(bytes: &[u8]) -> Result<Vec<Record>> {
    ensure!(
        bytes.is_empty() || bytes.ends_with(b"\n"),
        "Incomplete JSONL record; the selected session may still be writing"
    );
    let mut end = 0;
    bytes
        .split_inclusive(|b| *b == b'\n')
        .map(|line| {
            end += line.len() as u64;
            let value = if line.iter().all(u8::is_ascii_whitespace) {
                Value::Null
            } else {
                serde_json::from_slice(line).context("Invalid JSONL record")?
            };
            Ok(Record {
                value,
                raw: line.to_vec(),
                end,
            })
        })
        .collect()
}

pub(crate) fn encode(records: &[Record]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for record in records {
        if record.raw.is_empty() {
            if !record.value.is_null() {
                serde_json::to_writer(&mut bytes, &record.value)?;
                bytes.push(b'\n');
            }
        } else {
            bytes.extend_from_slice(&record.raw);
        }
    }
    Ok(bytes)
}

struct Loaded {
    records: Vec<Record>,
    path: PathBuf,
    before: (u64, String),
    limit: Option<u64>,
}

pub(crate) struct Loader<'a> {
    catalog: &'a Catalog,
    loaded: BTreeMap<String, Loaded>,
}

impl<'a> Loader<'a> {
    pub fn new(catalog: &'a Catalog) -> Self {
        Self {
            catalog,
            loaded: BTreeMap::new(),
        }
    }

    fn load(&mut self, id: &str) -> Result<&Loaded> {
        self.load_to(id, None)
    }

    fn load_to(&mut self, id: &str, limit: Option<u64>) -> Result<&Loaded> {
        let cached = self.loaded.get(id).is_some_and(|old| {
            old.limit.is_none() || limit.is_some_and(|n| old.limit.is_some_and(|old_n| n <= old_n))
        });
        if !cached {
            let session = self.catalog.get(id)?;
            storage::check_path(&session.path)?;
            let sibling = if session.path.extension().is_some_and(|e| e == "zst") {
                session.path.with_extension("")
            } else {
                let mut name = session.path.as_os_str().to_owned();
                name.push(".zst");
                PathBuf::from(name)
            };
            storage::check_path(&sibling)?;
            ensure!(
                !sibling.try_exists()?,
                "Compressed/plain rollout conflict: {}; no history changed",
                session.path.display()
            );
            let raw = read_range(&session.path, limit)?;
            let before = (raw.len() as u64, format!("{:x}", Sha256::digest(&raw)));
            let bytes = if limit.is_none() && session.path.extension().is_some_and(|e| e == "zst") {
                zstd::stream::decode_all(raw.as_slice())?
            } else {
                raw
            };
            if let Some(old) = self.loaded.get(id) {
                let size = old.before.0 as usize;
                ensure!(
                    bytes.len() >= size
                        && format!("{:x}", Sha256::digest(&bytes[..size])) == old.before.1,
                    "Inherited history changed while reading {id}; retry the edit"
                );
            }
            let records = parse(&bytes)?;
            let meta = records
                .iter()
                .find(|r| !r.value.is_null())
                .context("Empty rollout")?;
            ensure!(
                meta.value == session.meta,
                "Selected session changed while reading; retry the edit"
            );
            ensure!(
                records
                    .iter()
                    .filter(|r| r.value["type"] == "session_meta")
                    .count()
                    == 1,
                "Multiple session metadata records are unsupported"
            );
            if session.meta["payload"]["history_mode"] == "paginated" {
                let mut ordinal = session.meta["payload"]["history_base"]["end_ordinal_exclusive"]
                    .as_u64()
                    .unwrap_or(0);
                for record in records.iter().filter(|r| !r.value.is_null()) {
                    ensure!(
                        record.value["ordinal"].as_u64() == Some(ordinal),
                        "Invalid paginated ordinal in {id}"
                    );
                    ordinal = ordinal.checked_add(1).context("Ordinal overflow")?;
                }
            }
            self.loaded.insert(
                id.to_owned(),
                Loaded {
                    records,
                    path: session.path.clone(),
                    before,
                    limit,
                },
            );
        }
        Ok(&self.loaded[id])
    }

    // Flatten only inherited rollouts. Ordinary sessions retain their original prefix bytes.
    pub fn materialize(&mut self, id: &str) -> Result<Vec<Record>> {
        let original = self.load(id)?.records.clone();
        let meta_index = original
            .iter()
            .position(|r| r.value["type"] == "session_meta")
            .unwrap();
        let base = original[meta_index].value["payload"]["history_base"].clone();
        if base.is_null() {
            return Ok(original);
        }
        let mut seen = HashSet::from([id.to_owned()]);
        let parent = base["thread_id"]
            .as_str()
            .context("Invalid history_base thread_id")?;
        let inherited = self.body(parent, Some(&base), &mut seen)?;
        let mut meta = original[meta_index].clone();
        let old_start = meta.value["payload"]["subagent_history_start_ordinal"].as_u64();
        meta.edit()["payload"]
            .as_object_mut()
            .unwrap()
            .remove("history_base");
        let mut result = vec![meta];
        result.extend(inherited);
        let own_start = result.iter().filter(|r| !r.value.is_null()).count() as u64;
        if let Some(start) = old_start {
            let prior_count = original[meta_index + 1..]
                .iter()
                .filter(|r| r.value["ordinal"].as_u64().is_some_and(|n| n < start))
                .count() as u64;
            result[0].edit()["payload"]["subagent_history_start_ordinal"] =
                (own_start + prior_count).into();
        }
        result.extend_from_slice(&original[meta_index + 1..]);
        for (ordinal, record) in result.iter_mut().filter(|r| !r.value.is_null()).enumerate() {
            let v = record.edit();
            v["ordinal"] = (ordinal as u64).into();
            if v["payload"]["thread_id"].is_string() {
                v["payload"]["thread_id"] = id.into();
            }
        }
        Ok(result)
    }

    fn body(
        &mut self,
        id: &str,
        cutoff: Option<&Value>,
        seen: &mut HashSet<String>,
    ) -> Result<Vec<Record>> {
        ensure!(seen.insert(id.to_owned()), "Cycle in history_base for {id}");
        let limit = cutoff
            .map(|v| {
                v["end_byte_offset"]
                    .as_u64()
                    .context("Invalid history_base byte offset")
            })
            .transpose()?;
        let records = self.load_to(id, limit)?.records.clone();
        let meta = &self.catalog.get(id)?.meta;
        ensure!(
            meta["payload"]["history_mode"] == "paginated",
            "Inherited source is not paginated: {id}"
        );
        let local_end = if let Some(cutoff) = cutoff {
            let offset = cutoff["end_byte_offset"]
                .as_u64()
                .context("Invalid history_base byte offset")?;
            let ordinal = cutoff["end_ordinal_exclusive"]
                .as_u64()
                .context("Invalid history_base ordinal")?;
            let end = records
                .iter()
                .position(|r| r.end == offset)
                .context("history_base is outside its source or not on a record boundary")?;
            let last = records[..=end]
                .iter()
                .rev()
                .find(|r| !r.value.is_null())
                .context("Empty history_base")?;
            ensure!(
                last.value["ordinal"]
                    .as_u64()
                    .and_then(|n| n.checked_add(1))
                    == Some(ordinal),
                "history_base ordinal and byte offset disagree"
            );
            end + 1
        } else {
            records.len()
        };
        let base = meta["payload"]["history_base"].clone();
        let mut result = if base.is_null() {
            Vec::new()
        } else {
            let parent = base["thread_id"]
                .as_str()
                .context("Invalid history_base thread_id")?;
            self.body(parent, Some(&base), seen)?
        };
        result.extend(
            records[..local_end]
                .iter()
                .filter(|r| r.value["type"] != "session_meta")
                .cloned(),
        );
        seen.remove(id);
        Ok(result)
    }

    pub fn before(&mut self, id: &str) -> Result<(u64, String)> {
        Ok(self.load(id)?.before.clone())
    }

    pub fn validate(&self) -> Result<()> {
        for loaded in self.loaded.values() {
            let current = if loaded.limit.is_some() {
                let bytes = read_range(&loaded.path, loaded.limit)?;
                (bytes.len() as u64, format!("{:x}", Sha256::digest(&bytes)))
            } else {
                storage::fingerprint(&loaded.path)?
            };
            ensure!(
                current == loaded.before,
                "Session changed during editing: {}; retry the edit",
                loaded.path.display()
            );
        }
        Ok(())
    }
}

fn read_range(path: &Path, limit: Option<u64>) -> Result<Vec<u8>> {
    storage::check_path(path)?;
    let Some(limit) = limit else {
        return Ok(fs::read(path)?);
    };
    let file = fs::File::open(path)?;
    let input: Box<dyn Read> = if path.extension().is_some_and(|e| e == "zst") {
        Box::new(zstd::stream::read::Decoder::new(file)?)
    } else {
        Box::new(file)
    };
    let mut bytes = Vec::new();
    input.take(limit).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 == limit,
        "Inherited prefix was truncated: {}",
        path.display()
    );
    Ok(bytes)
}

#[derive(Clone, Serialize)]
pub(crate) struct Message {
    pub index: usize,
    pub role: String,
    pub text: String,
    pub timestamp: String,
    pub turn_id: Option<String>,
    #[serde(skip)]
    response: Option<usize>,
    #[serde(skip)]
    mirrors: Vec<usize>,
    #[serde(skip)]
    group: usize,
}

impl Message {
    fn checkpoint(&self) -> usize {
        self.response
            .into_iter()
            .chain(self.mirrors.iter().copied())
            .max()
            .unwrap()
    }
}

fn content_text(content: &Value) -> String {
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn response_role(v: &Value) -> Option<&str> {
    if v["type"] != "response_item" || v["payload"]["type"] != "message" {
        return None;
    }
    let payload = &v["payload"];
    let role = payload["role"].as_str()?;
    if !matches!(role, "user" | "assistant") {
        return None;
    }
    if role == "user" {
        if let Some(kinds) =
            payload["internal_chat_message_metadata_passthrough"]["content_item_kinds"].as_array()
        {
            if !kinds
                .iter()
                .any(|k| k.as_str().is_some_and(|s| s.starts_with("user.")))
            {
                return None;
            }
        } else {
            let text = content_text(&payload["content"]);
            if text.trim_start().starts_with("<environment_context>")
                || text.starts_with("# AGENTS.md instructions for")
            {
                return None;
            }
        }
    }
    Some(role)
}

fn event_message(v: &Value) -> Option<(&str, String, Option<&str>)> {
    if v["type"] != "event_msg" {
        return None;
    }
    let payload = &v["payload"];
    match payload["type"].as_str()? {
        "item_completed" => {
            let item = &payload["item"];
            let role = match item["type"].as_str()? {
                "UserMessage" | "userMessage" => "user",
                "AgentMessage" | "agentMessage" => "assistant",
                _ => return None,
            };
            Some((role, content_text(&item["content"]), item["id"].as_str()))
        }
        "user_message" => Some(("user", payload["message"].as_str()?.to_owned(), None)),
        "agent_message" => Some(("assistant", payload["message"].as_str()?.to_owned(), None)),
        _ => None,
    }
}

pub(crate) fn messages(records: &[Record]) -> Vec<Message> {
    let explicit_turns = records.iter().any(|r| {
        r.value["type"] == "event_msg"
            && matches!(
                r.value["payload"]["type"].as_str(),
                Some("task_started" | "turn_started")
            )
    });
    let legacy_user_events = records
        .iter()
        .any(|r| r.value["type"] == "event_msg" && r.value["payload"]["type"] == "user_message");
    let has_user_responses = records
        .iter()
        .any(|r| response_role(&r.value) == Some("user"));
    let mut group = 0;
    let mut turn_id: Option<String> = None;
    let mut groups = Vec::new();
    let mut turn_ids = Vec::new();
    let mut visible_groups = vec![0];
    for record in records {
        let v = &record.value;
        let p = &v["payload"];
        let explicit_start = v["type"] == "event_msg"
            && matches!(p["type"].as_str(), Some("task_started" | "turn_started"));
        let fallback_start = !explicit_turns
            && if legacy_user_events {
                v["type"] == "event_msg" && p["type"] == "user_message"
            } else {
                response_role(v) == Some("user")
                    || (event_message(v).is_some_and(|(role, _, _)| role == "user")
                        && !has_user_responses)
            };
        if explicit_start || fallback_start {
            group += 1;
            visible_groups.push(group);
            turn_id = p["turn_id"].as_str().map(str::to_owned);
        }
        if v["type"] == "turn_context" {
            turn_id = p["turn_id"].as_str().map(str::to_owned).or(turn_id);
        }
        if v["type"] == "event_msg" && p["type"] == "thread_rolled_back" {
            let n = p["num_turns"].as_u64().unwrap_or(0) as usize;
            visible_groups.truncate(visible_groups.len().saturating_sub(n));
        }
        groups.push(group);
        turn_ids.push(turn_id.clone());
    }
    let mut result = Vec::<Message>::new();
    for (i, record) in records.iter().enumerate() {
        if let Some(role) = response_role(&record.value) {
            result.push(Message {
                index: 0,
                role: role.to_owned(),
                text: content_text(&record.value["payload"]["content"]),
                timestamp: record.value["timestamp"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                turn_id:
                    record.value["payload"]["internal_chat_message_metadata_passthrough"]["turn_id"]
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| turn_ids[i].clone()),
                response: Some(i),
                mirrors: Vec::new(),
                group: groups[i],
            });
        }
    }
    for (i, record) in records.iter().enumerate() {
        let Some((role, text, id)) = event_message(&record.value) else {
            continue;
        };
        let matching = result
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == role && m.group == groups[i])
            .filter_map(|(n, m)| {
                let same_id = id.is_some_and(|id| {
                    m.response
                        .is_some_and(|r| records[r].value["payload"]["id"] == id)
                        || m.mirrors
                            .iter()
                            .any(|r| records[*r].value["payload"]["item"]["id"] == id)
                });
                if same_id || m.text == text {
                    Some((n, (!same_id, m.checkpoint().abs_diff(i))))
                } else {
                    None
                }
            })
            .min_by_key(|(_, score)| *score)
            .map(|(n, _)| n)
            .or_else(|| {
                // A public user item can omit model-only skill expansion text, and its ID
                // differs from the Responses message ID. Pair the nearest unclaimed input.
                if role != "user" {
                    return None;
                }
                result
                    .iter()
                    .enumerate()
                    .filter(|(_, m)| {
                        m.role == role
                            && m.group == groups[i]
                            && m.mirrors.is_empty()
                            && m.response.is_some()
                    })
                    .min_by_key(|(_, m)| m.checkpoint().abs_diff(i))
                    .map(|(n, _)| n)
            });
        if let Some(n) = matching {
            result[n].mirrors.push(i);
            // The public user item is authoritative for the editable text.
            result[n].text = text;
            result[n].turn_id = record.value["payload"]["turn_id"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| result[n].turn_id.clone());
        } else {
            result.push(Message {
                index: 0,
                role: role.to_owned(),
                text,
                timestamp: record.value["timestamp"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                turn_id: record.value["payload"]["turn_id"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| turn_ids[i].clone()),
                response: None,
                mirrors: vec![i],
                group: groups[i],
            });
        }
    }
    result.retain(|m| visible_groups.contains(&m.group));
    // Inherited subagent context is model-only; it is excluded from the public picker.
    let start = records
        .iter()
        .find(|r| r.value["type"] == "session_meta")
        .and_then(|r| r.value["payload"]["subagent_history_start_ordinal"].as_u64());
    if let Some(start) = start {
        result.retain(|m| {
            records[m.checkpoint()].value["ordinal"]
                .as_u64()
                .is_none_or(|n| n >= start)
        });
    }
    result.sort_by_key(Message::checkpoint);
    for (i, message) in result.iter_mut().enumerate() {
        message.index = i + 1;
    }
    result
}

fn replace_content(content: &mut Value, text: &str, kind: &str) -> Result<()> {
    let parts = content.as_array_mut().context("Invalid message content")?;
    let mut replaced = false;
    parts.retain_mut(|part| {
        if part["text"].is_string() {
            if replaced {
                return false;
            }
            part["text"] = text.into();
            if part.get("text_elements").is_some() {
                part["text_elements"] = json!([]);
            }
            if part.get("textElements").is_some() {
                part["textElements"] = json!([]);
            }
            replaced = true;
        }
        true
    });
    if !replaced {
        parts.insert(0, json!({"type":kind,"text":text}));
    }
    Ok(())
}

pub(crate) fn edit(mut records: Vec<Record>, message: &Message, text: &str) -> Result<Vec<Record>> {
    let end = message.checkpoint();
    records.truncate(end + 1);
    let kind = if message.role == "user" {
        "input_text"
    } else {
        "output_text"
    };
    if let Some(response) = message.response {
        replace_content(
            &mut records[response].edit()["payload"]["content"],
            text,
            kind,
        )?;
    }
    for &mirror in &message.mirrors {
        let payload = &mut records[mirror].edit()["payload"];
        if payload["type"] == "item_completed" {
            let kind = if message.role == "user" {
                "text"
            } else {
                "Text"
            };
            replace_content(&mut payload["item"]["content"], text, kind)?;
        } else {
            payload["message"] = text.into();
            if payload.get("text_elements").is_some() {
                payload["text_elements"] = json!([]);
            }
        }
    }
    let mut ordinal = records
        .iter()
        .rev()
        .find_map(|r| r.value["ordinal"].as_u64());
    if message.response.is_none() {
        let mut value = json!({"timestamp":message.timestamp,"type":"response_item",
            "payload":{"type":"message","role":message.role,"content":[{"type":kind,"text":text}]}});
        if let Some(n) = ordinal {
            let next = n.checked_add(1).context("Ordinal overflow")?;
            value["ordinal"] = next.into();
            ordinal = Some(next);
        }
        records.push(Record::new(value));
    }
    // Closing the retained partial turn prevents a stale in-progress indicator on restart.
    if let Some(turn_id) = &message.turn_id {
        let mut value = json!({"timestamp":chrono::Utc::now().to_rfc3339(),"type":"event_msg",
            "payload":{"type":"turn_aborted","turn_id":turn_id,"reason":"interrupted"}});
        if let Some(n) = ordinal {
            value["ordinal"] = n.checked_add(1).context("Ordinal overflow")?.into();
        }
        records.push(Record::new(value));
    }
    Ok(records)
}
