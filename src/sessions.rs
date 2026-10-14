use crate::{storage, writer_lock};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Session {
    pub id: String,
    pub name: String,
    pub cwd: String,
    pub updated_at: String,
    pub archived: bool,
    pub path: PathBuf,
    pub preview: String,
    pub in_use: bool,
    #[serde(skip)]
    pub meta: Value,
}

pub(crate) struct Catalog {
    pub sessions: BTreeMap<String, Session>,
}

impl Session {
    pub fn internal(&self) -> bool {
        let source = &self.meta["payload"]["source"];
        source.get("subagent").is_some()
            || source.get("internal").is_some()
            || source
                .as_str()
                .is_some_and(|s| s.starts_with("subagent_") || s.starts_with("internal_"))
    }

    pub fn kind(&self) -> &str {
        let source = &self.meta["payload"]["source"];
        let subagent = &source["subagent"];
        let other = subagent["other"].as_str().unwrap_or_default();
        let old_source = source.as_str().unwrap_or_default();
        if matches!(other, "guardian" | "exec_review" | "approval")
            || matches!(old_source, "subagent_guardian" | "subagent_exec_review")
        {
            "承認レビュー"
        } else if subagent == "review" || old_source == "subagent_review" {
            "コードレビュー"
        } else if subagent.get("thread_spawn").is_some()
            || old_source.starts_with("subagent_thread_spawn")
        {
            "サブエージェント"
        } else if self.internal() {
            "内部会話"
        } else {
            "会話"
        }
    }
}

impl Catalog {
    pub fn read(home: &Path, sqlite_home: &Path) -> Result<Self> {
        let home = std::path::absolute(home)?;
        let metadata = metadata(sqlite_home)?;
        let mut names = BTreeMap::new();
        let index = home.join("session_index.jsonl");
        storage::check_path(&index)?;
        if index.try_exists()? {
            for line in BufReader::new(fs::File::open(index)?).lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let v: Value = serde_json::from_str(&line)?;
                let id = v["id"].as_str().context("Missing session index id")?;
                let at = v["updated_at"].as_str().context("Missing updated_at")?;
                let at = chrono::DateTime::parse_from_rfc3339(at)?;
                let name = v["thread_name"].as_str().context("Missing thread name")?;
                if names.get(id).is_none_or(|(old, _)| at >= *old) {
                    names.insert(id.to_owned(), (at, name.to_owned()));
                }
            }
        }
        let mut physical: BTreeMap<String, Vec<Session>> = BTreeMap::new();
        for relative in storage::inventory(&home)? {
            if !relative.starts_with("sessions/") && !relative.starts_with("archived_sessions/") {
                continue;
            }
            let path = home.join(&relative);
            let meta =
                read_head(&path).with_context(|| format!("Cannot inspect {}", path.display()))?;
            ensure!(
                meta["type"] == "session_meta",
                "Missing session metadata: {relative}"
            );
            let payload = &meta["payload"];
            let id = payload["id"]
                .as_str()
                .context("Missing thread id")?
                .to_owned();
            let db = metadata.get(&id);
            let name = names
                .get(&id)
                .map(|(_, name)| name.clone())
                .or_else(|| db.map(|d| d.name.clone()))
                .unwrap_or_default();
            let modified: chrono::DateTime<chrono::Utc> = fs::metadata(&path)?.modified()?.into();
            physical.entry(id.clone()).or_default().push(Session {
                in_use: writer_lock::in_use(&home, &id)?,
                id,
                name,
                cwd: payload["cwd"].as_str().unwrap_or_default().to_owned(),
                updated_at: modified.to_rfc3339(),
                archived: relative.starts_with("archived_sessions/"),
                path,
                preview: db.map(|d| d.preview.clone()).unwrap_or_default(),
                meta,
            });
        }
        let mut sessions = BTreeMap::new();
        for (id, mut candidates) in physical {
            let chosen = if candidates.len() == 1 {
                candidates.remove(0)
            } else {
                let index = metadata.get(&id)
                    .and_then(|db| {
                        candidates.iter().position(|s| s.path == db.path).or_else(|| {
                            // Codex can compress a rollout without changing its indexed .jsonl path.
                            candidates.iter().position(|s| {
                                uncompressed_path(&s.path) == uncompressed_path(&db.path)
                            })
                        })
                    })
                    .with_context(|| format!("Multiple physical rollouts for {id}; cannot identify the current one from state_5.sqlite"))?;
                candidates.remove(index)
            };
            sessions.insert(id, chosen);
        }
        Ok(Self { sessions })
    }

    pub fn get(&self, id: &str) -> Result<&Session> {
        self.sessions
            .get(id)
            .with_context(|| format!("Session not found: {id}"))
    }

    pub fn filtered(&self, archived: bool, search: Option<&str>) -> Vec<&Session> {
        let search = search.unwrap_or_default().to_lowercase();
        let mut sessions: Vec<_> = self
            .sessions
            .values()
            .filter(|s| {
                (archived || !s.archived)
                    && [&s.id, &s.name, &s.cwd, &s.preview]
                        .iter()
                        .any(|field| field.to_lowercase().contains(&search))
            })
            .collect();
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
        sessions
    }

    // Every descendant's frozen prefix must be detached before its source bytes change.
    pub fn descendants(&self, id: &str) -> Vec<String> {
        let mut affected = HashSet::from([id.to_owned()]);
        loop {
            let previous = affected.len();
            for session in self.sessions.values() {
                if session.meta["payload"]["history_base"]["thread_id"]
                    .as_str()
                    .is_some_and(|parent| affected.contains(parent))
                {
                    affected.insert(session.id.clone());
                }
            }
            if previous == affected.len() {
                break;
            }
        }
        affected.remove(id);
        let mut result: Vec<_> = affected.into_iter().collect();
        result.sort();
        result
    }
}

fn uncompressed_path(path: &Path) -> PathBuf {
    if path.extension().is_some_and(|extension| extension == "zst") {
        path.with_extension("")
    } else {
        path.to_owned()
    }
}

fn read_head(path: &Path) -> Result<Value> {
    let file = fs::File::open(path)?;
    let input: Box<dyn Read> = if path.extension().is_some_and(|e| e == "zst") {
        Box::new(zstd::stream::read::Decoder::new(file)?)
    } else {
        Box::new(file)
    };
    for line in BufReader::new(input).lines() {
        let line = line?;
        if !line.trim().is_empty() {
            return Ok(serde_json::from_str(&line)?);
        }
    }
    anyhow::bail!("Empty rollout")
}

struct Metadata {
    path: PathBuf,
    name: String,
    preview: String,
}

pub(crate) fn columns(db: &Connection, table: &str) -> Result<HashSet<String>> {
    // table is always a program-owned identifier, never user input.
    Ok(db
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |row| row.get(1))?
        .collect::<std::result::Result<_, _>>()?)
}

fn metadata(sqlite_home: &Path) -> Result<BTreeMap<String, Metadata>> {
    let path = sqlite_home.join("state_5.sqlite");
    storage::check_path(&path)?;
    if !path.try_exists()? {
        return Ok(BTreeMap::new());
    }
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let fields = columns(&db, "threads")?;
    ensure!(
        fields.contains("id") && fields.contains("rollout_path"),
        "Unsupported threads schema"
    );
    let title = if fields.contains("title") {
        "title"
    } else {
        "''"
    };
    let name = if fields.contains("name") {
        "name"
    } else {
        "NULL"
    };
    let first = if fields.contains("first_user_message") {
        "first_user_message"
    } else {
        "''"
    };
    let preview = if fields.contains("preview") {
        format!("COALESCE(NULLIF(preview, ''), {first}, '')")
    } else {
        format!("COALESCE({first}, '')")
    };
    let sql = format!("SELECT id, rollout_path, COALESCE({name}, {title}), {preview} FROM threads");
    Ok(db
        .prepare(&sql)?
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                Metadata {
                    path: PathBuf::from(row.get::<_, String>(1)?),
                    name: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    preview: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                },
            ))
        })?
        .collect::<std::result::Result<_, _>>()?)
}

pub(crate) fn snippet(text: &str, length: usize) -> String {
    let clean: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut chars = clean.chars();
    let mut result: String = chars.by_ref().take(length).collect();
    if chars.next().is_some() {
        result.push('…');
    }
    result
}

pub fn list(
    home: &Path,
    sqlite_home: &Path,
    archived: bool,
    search: Option<&str>,
    json: bool,
) -> Result<()> {
    let catalog = Catalog::read(home, sqlite_home)?;
    let sessions = catalog.filtered(archived, search);
    if json {
        println!("{}", serde_json::to_string_pretty(&sessions)?);
    } else {
        for session in &sessions {
            println!(
                "{}  {}  {}{}{}  {}",
                snippet(&session.id, 64),
                &session.updated_at[..19],
                snippet(&session.name, 65),
                if session.archived { " [archived]" } else { "" },
                if session.in_use { " [使用中]" } else { "" },
                snippet(&session.cwd, 65)
            );
        }
        if sessions.is_empty() {
            println!("No sessions found");
        }
    }
    Ok(())
}
