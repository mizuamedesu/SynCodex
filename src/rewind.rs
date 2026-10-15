use crate::{
    editor,
    rollout::{self, Loader, Message, Record},
    sessions::{self, Catalog},
    storage::{self, Change},
    ui,
    writer_lock::{self, WriterLocks},
};
use anyhow::{Context, Result, ensure};
use clap::Args;
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, IsTerminal, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Args)]
pub struct RewindArgs {
    /// Session to edit; omit to open a searchable session picker
    pub thread_id: Option<String>,
    /// 1-based user/assistant message number from `messages`; omit to select interactively
    #[arg(long)]
    pub message: Option<usize>,
    /// Replacement text (supports multiline); omit to edit with nano
    #[arg(long, conflicts_with = "text_file")]
    pub text: Option<String>,
    /// Read replacement text from a UTF-8 file
    #[arg(long)]
    pub text_file: Option<PathBuf>,
    /// Editor command (default: nano), e.g. vim, 'code --wait', or builtin
    #[arg(long, conflicts_with_all = ["text", "text_file"])]
    pub editor: Option<String>,
    /// Use Vim instead of nano
    #[arg(long, conflicts_with_all = ["editor", "text", "text_file"])]
    pub vim: bool,
    /// Include archived sessions in the picker
    #[arg(long)]
    pub archived: bool,
    /// Include automatic approval reviews and other internal sessions in the picker
    #[arg(long)]
    pub include_internal: bool,
    /// Filter the session picker by name, id, preview, or working directory
    #[arg(long)]
    pub search: Option<String>,
    /// Prepare and validate all changes without writing the selected Codex home
    #[arg(long)]
    pub dry_run: bool,
    /// Print the result as JSON (non-terminal output is always JSON)
    #[arg(long)]
    pub json: bool,
}

fn require_terminal() -> Result<()> {
    ensure!(
        io::stdin().is_terminal() && io::stderr().is_terminal(),
        "Interactive selection/editing requires a terminal. Use rewind THREAD_ID --message N --text-file PATH (or --text TEXT)"
    );
    Ok(())
}

fn cancelled() -> Result<()> {
    println!("Cancelled / 取り消しました。履歴は変更していません。");
    Ok(())
}

pub fn messages(home: &Path, sqlite_home: &Path, id: &str, json: bool) -> Result<()> {
    let catalog = Catalog::read(home, sqlite_home)?;
    let records = Loader::new(&catalog).materialize(id)?;
    let messages = rollout::messages(&records);
    if json {
        println!("{}", serde_json::to_string_pretty(&messages)?);
    } else {
        for message in &messages {
            println!(
                "[{}] {}  {}\n{}\n",
                message.index,
                message.role,
                sessions::snippet(&message.timestamp, 40),
                message
                    .text
                    .chars()
                    .map(|c| if c.is_control() && c != '\n' && c != '\t' {
                        ' '
                    } else {
                        c
                    })
                    .collect::<String>()
            );
        }
        if messages.is_empty() {
            println!("No user/assistant messages found");
        }
    }
    Ok(())
}

pub fn rewind(home: &Path, sqlite_home: &Path, args: RewindArgs) -> Result<()> {
    let home = std::path::absolute(home)?;
    let sqlite_home = std::path::absolute(sqlite_home)?;
    let catalog = Catalog::read(&home, &sqlite_home)?;
    let (id, descendants, affected, mut loader, records, original_messages, index, replacement) = 'select_session: loop {
        let id = if let Some(id) = &args.thread_id {
            id.clone()
        } else if let Some(id) = ui::pick_session(
            &catalog,
            args.archived,
            args.include_internal,
            args.search.as_deref(),
        )? {
            id
        } else {
            return cancelled();
        };
        protect_current_session(&home, &id)?;
        let session = catalog.get(&id)?;
        let descendants = catalog.descendants(&id);
        let affected: Vec<_> = std::iter::once(id.clone())
            .chain(descendants.iter().cloned())
            .collect();
        writer_lock::ensure_unused(&home, &affected)?;
        let mut loader = Loader::new(&catalog);
        let records = loader.materialize(&id)?;
        let original_messages = rollout::messages(&records);
        ensure!(
            !original_messages.is_empty(),
            "Session has no editable user/assistant messages"
        );
        let mut selected = original_messages.len() - 1;
        loop {
            let index = if let Some(index) = args.message {
                ensure!(
                    index > 0 && index <= original_messages.len(),
                    "Message must be between 1 and {}",
                    original_messages.len()
                );
                index - 1
            } else if let Some(index) = ui::pick_message(session, &original_messages, selected)? {
                index
            } else if args.thread_id.is_none() {
                continue 'select_session;
            } else {
                return cancelled();
            };
            selected = index;
            let message = &original_messages[index];
            let replacement = if let Some(text) = &args.text {
                Some(text.clone())
            } else if let Some(path) = &args.text_file {
                Some(
                    fs::read_to_string(path).with_context(|| {
                        format!("Cannot read replacement text: {}", path.display())
                    })?,
                )
            } else if args.editor.as_deref() == Some("builtin") {
                ui::edit_message(
                    session,
                    message,
                    original_messages.len(),
                    descendants.len(),
                    args.dry_run,
                )?
            } else {
                let command = if args.vim {
                    "vim"
                } else {
                    args.editor.as_deref().unwrap_or("nano")
                };
                require_terminal()?;
                ensure!(!command.trim().is_empty(), "Empty editor command");
                eprintln!(
                    "#{} {} を編集します。保存して終了すると、この後の {} 件を{}。保存せず終了すると取り消します。",
                    index + 1,
                    ui::role_label(&message.role),
                    original_messages.len() - index - 1,
                    if args.dry_run {
                        "削除する変更を検証します (dry-run)"
                    } else {
                        "削除します"
                    }
                );
                match command {
                    "nano" => eprintln!(
                        "nano: そのまま編集 → Ctrl+O → Enter で保存 → Ctrl+X で終了。保存せず取り消すには Ctrl+X → N。"
                    ),
                    "vim" => {
                        eprintln!("Vim: i で編集 → Esc → :wq で保存・終了。取消は Esc → :q!。")
                    }
                    _ => {}
                }
                let text = editor::edit(&message.text, command)?;
                if text.is_none() {
                    return cancelled();
                }
                text
            };
            let Some(replacement) = replacement else {
                if args.message.is_some() {
                    return cancelled();
                }
                continue;
            };
            break 'select_session (
                id,
                descendants,
                affected,
                loader,
                records,
                original_messages,
                index,
                replacement,
            );
        }
    };
    let session = catalog.get(&id)?;
    let message = &original_messages[index];
    ensure!(
        !replacement.trim().is_empty(),
        "Replacement text cannot be empty; no history changed"
    );
    let rewound = rollout::edit(records, message, &replacement)?;
    let mut retained = original_messages[..=index].to_vec();
    retained[index].text = replacement;
    let staging = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir())?)?;
    let mut changes = Vec::new();
    for affected in std::iter::once(&id).chain(descendants.iter()) {
        let records = if affected == &id {
            rewound.clone()
        } else {
            loader.materialize(affected)?
        };
        let target = &catalog.get(affected)?.path;
        let mut bytes = rollout::encode(&records)?;
        if target.extension().is_some_and(|e| e == "zst") {
            bytes = zstd::stream::encode_all(bytes.as_slice(), 3)?;
        }
        let staged = staging.path().join(format!("rollout-{}", changes.len()));
        fs::write(&staged, bytes)?;
        changes.push(Change {
            target: target.clone(),
            staged,
            before: Some(loader.before(affected)?),
            database: false,
        });
    }
    let edit = Edit {
        id: &id,
        session_id: session.meta["payload"]["session_id"].as_str(),
        affected: &affected,
        original: &original_messages,
        retained: &retained,
        records: &rewound,
    };
    stage_databases(&sqlite_home, staging.path(), &edit, &mut changes)?;
    loader.validate()?;
    validate_catalog(&home, &sqlite_home, &catalog, &edit)?;
    let history = home.join("history.jsonl");
    storage::check_path(&history)?;
    let history_changes = if history.try_exists()? {
        let mut file = fs::File::open(&history)?;
        file.try_lock_shared()
            .context("Input history is being appended; retry shortly")?;
        let mut old = Vec::new();
        file.read_to_end(&mut old)?;
        input_history(&old, &id, edit.session_id, &retained)? != old
    } else {
        false
    };
    let (recovery, changed_files) = if args.dry_run {
        writer_lock::ensure_unused(&home, &affected)?;
        (None, changes.len() + usize::from(history_changes))
    } else {
        let (path, count) = apply_rewind(&home, &sqlite_home, &catalog, &loader, &changes, &edit)?;
        (Some(path), count)
    };
    if args.json || !io::stdout().is_terminal() {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "dry_run":args.dry_run,"thread_id":id,"message_index":index + 1,"role":message.role,
                "kept_messages":retained.len(),"removed_messages":original_messages.len() - retained.len(),
                "preserved_forks":descendants.len(),"changed_files":changed_files,"recovery":recovery,
            }))?
        );
    } else {
        println!(
            "{}\n#{} {} を更新 · 残す {} 件 / 削除 {} 件",
            if args.dry_run {
                "✓ 検証できました (履歴は変更していません)"
            } else {
                "✓ 巻き戻しました"
            },
            index + 1,
            ui::role_label(&message.role),
            retained.len(),
            original_messages.len() - retained.len()
        );
        if let Some(path) = recovery {
            println!("バックアップ: {}", path.display());
        }
        if !args.dry_run {
            println!("会話を再開: codex resume {id}");
        }
    }
    Ok(())
}

fn protect_current_session(home: &Path, id: &str) -> Result<()> {
    if std::env::var("CODEX_THREAD_ID").ok().as_deref() == Some(id) {
        let live_home = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex")));
        if let Some(live_home) = live_home {
            ensure!(
                fs::canonicalize(home)? != fs::canonicalize(live_home)?,
                "Cannot rewind the Codex session hosting this command. Close this session and run from a separate terminal, or use a copied home"
            );
        }
    }
    Ok(())
}

fn input_history(
    bytes: &[u8],
    id: &str,
    session_id: Option<&str>,
    retained: &[Message],
) -> Result<Vec<u8>> {
    let old = rollout::parse(bytes)?;
    let matches = |r: &Record| {
        r.value["session_id"] == id || session_id.is_some_and(|sid| r.value["session_id"] == sid)
    };
    let mut result: Vec<_> = old.iter().filter(|r| !matches(r)).cloned().collect();
    for message in retained.iter().filter(|m| m.role == "user") {
        let timestamp = chrono::DateTime::parse_from_rfc3339(&message.timestamp)
            .with_context(|| {
                format!(
                    "Missing or invalid timestamp for user message {}",
                    message.index
                )
            })?
            .timestamp();
        result.push(Record::new(
            json!({"session_id":id,"ts":timestamp,"text":message.text}),
        ));
    }
    rollout::encode(&result)
}

struct Edit<'a> {
    id: &'a str,
    session_id: Option<&'a str>,
    affected: &'a [String],
    original: &'a [Message],
    retained: &'a [Message],
    records: &'a [Record],
}

fn validate_catalog(
    home: &Path,
    sqlite_home: &Path,
    before: &Catalog,
    edit: &Edit<'_>,
) -> Result<()> {
    let current = Catalog::read(home, sqlite_home)?;
    ensure!(
        current.descendants(edit.id) == before.descendants(edit.id),
        "Dependent forks changed during editing; retry the edit"
    );
    for id in edit.affected {
        let old = before.get(id)?;
        let new = current.get(id)?;
        ensure!(
            old.path == new.path && old.meta == new.meta,
            "Selected session or a dependent fork changed during editing: {id}; retry the edit"
        );
    }
    Ok(())
}

fn stage_databases(
    sqlite_home: &Path,
    staging: &Path,
    edit: &Edit<'_>,
    changes: &mut Vec<Change>,
) -> Result<()> {
    for name in ["state_5.sqlite", "thread_history_1.sqlite"] {
        let target = sqlite_home.join(name);
        storage::check_path(&target)?;
        if !target.try_exists()? {
            continue;
        }
        let staged = staging.join(name);
        let mut source = Connection::open_with_flags(&target, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        source.busy_timeout(Duration::from_secs(5))?;
        let read = source.transaction_with_behavior(TransactionBehavior::Deferred)?;
        snapshot_rows(&read, &staged, name, edit.affected)?;
        read.commit()?;
        let mut db = Connection::open(&staged)?;
        let transaction = db.transaction()?;
        patch_database(&transaction, name, edit)?;
        transaction.commit()?;
        drop(db);
        changes.push(Change {
            target,
            staged,
            before: None,
            database: true,
        });
    }
    Ok(())
}

fn patch_database(db: &Connection, name: &str, edit: &Edit<'_>) -> Result<()> {
    if name == "thread_history_1.sqlite" {
        for table in [
            "thread_items",
            "thread_turns",
            "thread_history_projection_state",
            "thread_realtime_items",
        ] {
            let columns = sessions::columns(db, table)?;
            if columns.is_empty() && table == "thread_realtime_items" {
                continue;
            }
            ensure!(
                columns.contains("thread_id"),
                "Unsupported history database schema: {table}"
            );
            for id in edit.affected {
                db.execute(&format!("DELETE FROM {table} WHERE thread_id = ?"), [id])?;
            }
        }
    } else {
        let columns = sessions::columns(db, "threads")?;
        ensure!(columns.contains("id"), "Unsupported state database schema");
        let first = edit
            .retained
            .iter()
            .find(|m| m.role == "user")
            .map(|m| m.text.as_str())
            .unwrap_or_default();
        let old_first = edit
            .original
            .iter()
            .find(|m| m.role == "user")
            .map(|m| m.text.as_str())
            .unwrap_or_default();
        for column in ["first_user_message", "preview"] {
            if columns.contains(column) {
                db.execute(
                    &format!("UPDATE threads SET {column} = ? WHERE id = ?"),
                    [first, edit.id],
                )?;
            }
        }
        if columns.contains("title") {
            db.execute(
                "UPDATE threads SET title = ? WHERE id = ? AND title = ?",
                [first, edit.id, old_first],
            )?;
        }
        let now = chrono::Utc::now();
        for (column, value) in [
            ("updated_at", now.timestamp()),
            ("updated_at_ms", now.timestamp_millis()),
        ] {
            if columns.contains(column) {
                db.execute(
                    &format!("UPDATE threads SET {column} = ? WHERE id = ?"),
                    rusqlite::params![value, edit.id],
                )?;
            }
        }
        if columns.contains("tokens_used") {
            let tokens = edit
                .records
                .iter()
                .rev()
                .find_map(|r| {
                    r.value["payload"]["thread_token_usage"]["total_tokens"]
                        .as_i64()
                        .or_else(|| {
                            r.value["payload"]["info"]["total_token_usage"]["total_tokens"].as_i64()
                        })
                })
                .unwrap_or(0);
            db.execute(
                "UPDATE threads SET tokens_used = ? WHERE id = ?",
                rusqlite::params![tokens, edit.id],
            )?;
        }
    }
    Ok(())
}

// Recovery databases contain the original schema/values of affected rows only.
// This avoids copying every other session while holding the SQLite writer slot.
fn snapshot_rows(
    source: &Connection,
    output: &Path,
    name: &str,
    affected: &[String],
) -> Result<()> {
    let mut target = Connection::open(output)?;
    target.execute_batch("PRAGMA foreign_keys = OFF")?;
    let copy = target.transaction()?;
    let tables: &[(&str, &str)] = if name == "state_5.sqlite" {
        &[("threads", "id")]
    } else {
        &[
            ("thread_turns", "thread_id"),
            ("thread_items", "thread_id"),
            ("thread_history_projection_state", "thread_id"),
            ("thread_realtime_items", "thread_id"),
        ]
    };
    let ids = std::iter::repeat_n("?", affected.len())
        .collect::<Vec<_>>()
        .join(",");
    for (table, id_column) in tables {
        let columns = sessions::columns(source, table)?;
        if columns.is_empty() && *table == "thread_realtime_items" {
            continue;
        }
        ensure!(
            columns.contains(*id_column),
            "Unsupported history database schema: {table}"
        );
        let schema: String = source.query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?",
            [table],
            |row| row.get(0),
        )?;
        copy.execute_batch(&schema)?;
        let mut query = source.prepare(&format!(
            "SELECT * FROM {table} WHERE {id_column} IN ({ids})"
        ))?;
        let count = query.column_count();
        let values = std::iter::repeat_n("?", count)
            .collect::<Vec<_>>()
            .join(",");
        let mut insert = copy.prepare(&format!("INSERT INTO {table} VALUES ({values})"))?;
        let mut rows = query.query(rusqlite::params_from_iter(affected))?;
        while let Some(row) = rows.next()? {
            let values = (0..count)
                .map(|i| row.get::<_, rusqlite::types::Value>(i))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            insert.execute(rusqlite::params_from_iter(&values))?;
        }
    }
    copy.commit()?;
    Ok(())
}

fn apply_rewind(
    home: &Path,
    sqlite_home: &Path,
    catalog: &Catalog,
    loader: &Loader<'_>,
    changes: &[Change],
    edit: &Edit<'_>,
) -> Result<(PathBuf, usize)> {
    let _syncodex_lock = storage::write_lock(home)?;
    let _writers = WriterLocks::acquire(home, edit.affected)?;
    loader.validate()?;
    validate_catalog(home, sqlite_home, catalog, edit)?;

    // Hold brief SQLite write transactions; apply only the selected thread rows.
    // Another session's committed writes are never replaced by our preflight snapshot.
    let databases: Vec<_> = changes.iter().filter(|c| c.database).collect();
    let mut connections = Vec::new();
    for change in &databases {
        storage::check_path(&change.target)?;
        let db = Connection::open_with_flags(&change.target, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        db.busy_timeout(Duration::from_secs(5))?;
        connections.push(db);
    }
    let mut transactions = Vec::new();
    for db in &mut connections {
        transactions.push(
            db.transaction_with_behavior(TransactionBehavior::Immediate)
                .context("Conversation database is busy; retry shortly")?,
        );
    }

    // Snapshot current databases before mutating their rows. The transactions
    // keep these recovery originals consistent with what we are about to edit.
    let recovery = storage::backup_changes_with(home, changes, |path, backup| {
        let index = databases
            .iter()
            .position(|c| c.target == path)
            .context("Missing database plan")?;
        snapshot_rows(
            &transactions[index],
            backup,
            path.file_name()
                .unwrap()
                .to_str()
                .context("Invalid database name")?,
            edit.affected,
        )
    })?;
    let history_path = home.join("history.jsonl");
    storage::check_path(&history_path)?;
    let mut history = if history_path.try_exists()? {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&history_path)?;
        file.try_lock()
            .context("Input history is being appended; retry shortly")?;
        Some(file)
    } else {
        None
    };
    let mut input_bytes = None;
    if let Some(file) = &mut history {
        let mut old = Vec::new();
        file.read_to_end(&mut old)?;
        let new = input_history(&old, edit.id, edit.session_id, edit.retained)?;
        if new != old {
            let backup = recovery.join(changes.len().to_string());
            let mut saved = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(backup)?;
            saved.write_all(&old)?;
            saved.sync_all()?;
            input_bytes = Some(new);
        }
    }
    annotate_recovery(
        &recovery,
        changes.len(),
        &history_path,
        input_bytes.is_some(),
        edit,
    )?;
    for (change, transaction) in databases.iter().zip(&transactions) {
        patch_database(
            transaction,
            change
                .target
                .file_name()
                .unwrap()
                .to_str()
                .context("Invalid database name")?,
            edit,
        )?;
    }
    // The selected session files remain unchanged since the editor opened;
    // unrelated sessions may have appended throughout that time.
    loader.validate()?;
    for change in changes.iter().filter(|c| !c.database) {
        storage::apply_file(change)?;
    }
    if let Some(bytes) = &input_bytes {
        let file = history.as_mut().unwrap();
        file.seek(SeekFrom::Start(0))?;
        file.write_all(bytes)?;
        file.set_len(bytes.len() as u64)?;
        file.sync_all()?;
    }
    for transaction in transactions {
        transaction.commit()?;
    }
    Ok((recovery, changes.len() + usize::from(input_bytes.is_some())))
}

fn annotate_recovery(
    recovery: &Path,
    history_index: usize,
    history_path: &Path,
    history_changed: bool,
    edit: &Edit<'_>,
) -> Result<()> {
    let path = recovery.join("journal.json");
    let mut journal: Vec<Value> = serde_json::from_slice(&fs::read(&path)?)?;
    for entry in &mut journal {
        if entry["database"] == true {
            entry["restore_scope"] = "thread_rows".into();
            entry["thread_ids"] = json!(edit.affected);
        }
    }
    if history_changed {
        journal.push(json!({"target":history_path,"backup":history_index.to_string(),"database":false,
            "restore_scope":"session_entries","session_ids":[edit.id,edit.session_id.unwrap_or(edit.id)]}));
    }
    let mut file = fs::File::create(path)?;
    serde_json::to_writer_pretty(&mut file, &journal)?;
    file.sync_all()?;
    Ok(())
}
