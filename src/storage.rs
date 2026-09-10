use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};
use walkdir::WalkDir;

const DATABASES: &[&str] = &["state_5.sqlite", "thread_history_1.sqlite"];

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format_version: u32,
    #[serde(default)]
    source_home: Option<PathBuf>,
    files: Vec<Entry>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    path: String,
    size: u64,
    sha256: String,
    #[serde(default)]
    modified: Option<std::time::SystemTime>,
}

fn private_dir(path: impl AsRef<Path>) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    Ok(())
}

fn allowed(path: &str) -> bool {
    if let Some(name) = path.strip_prefix("sqlite/") {
        return DATABASES.contains(&name);
    }
    if path.contains('\\')
        || path.contains(':')
        || path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return false;
    }
    if Path::new(path)
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return false;
    }
    matches!(path, "history.jsonl" | "session_index.jsonl")
        || ((path.starts_with("sessions/") || path.starts_with("archived_sessions/"))
            && (path.ends_with(".jsonl") || path.ends_with(".jsonl.zst")))
}

// Reject symlinks in every existing component, including the supplied root.
fn check_path(path: &Path) -> Result<()> {
    let absolute = std::path::absolute(path)?;
    let mut current = PathBuf::new();
    for component in absolute.components() {
        ensure!(
            !matches!(component, Component::ParentDir),
            "Parent traversal is unsupported"
        );
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(meta) => ensure!(
                !meta.file_type().is_symlink(),
                "Symlink unsupported: {}",
                current.display()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn fingerprint(path: &Path) -> Result<(u64, String)> {
    check_path(path)?;
    ensure!(
        fs::metadata(path)?.is_file(),
        "Not a regular file: {}",
        path.display()
    );
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut size = 0;
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        size += n as u64;
    }
    Ok((size, format!("{:x}", hash.finalize())))
}

fn inventory(home: &Path) -> Result<Vec<String>> {
    check_path(home)?;
    ensure!(
        home.is_dir(),
        "Codex home does not exist: {}",
        home.display()
    );
    let mut paths = Vec::new();
    for root in [
        "sessions",
        "archived_sessions",
        "history.jsonl",
        "session_index.jsonl",
    ] {
        let start = home.join(root);
        check_path(&start)?;
        if !start.try_exists()? {
            continue;
        }
        for entry in WalkDir::new(start).follow_links(false) {
            let entry = entry?;
            ensure!(
                !entry.file_type().is_symlink(),
                "Symlink unsupported: {}",
                entry.path().display()
            );
            if entry.file_type().is_dir() {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(home)?
                .to_str()
                .context("Non-UTF-8 path")?
                .replace('\\', "/");
            if allowed(&relative) {
                ensure!(
                    entry.file_type().is_file(),
                    "Not a regular file: {relative}"
                );
                paths.push(relative);
            }
        }
    }
    paths.sort();
    Ok(paths)
}

fn write_new(path: &Path, source: &Path) -> Result<()> {
    check_path(path)?;
    let parent = path.parent().context("Missing parent")?;
    private_dir(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    std::io::copy(&mut fs::File::open(source)?, &mut temp)?;
    temp.as_file()
        .set_times(fs::FileTimes::new().set_modified(fs::metadata(source)?.modified()?))?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(path)
        .with_context(|| format!("Cannot create {}", path.display()))?;
    Ok(())
}

pub fn dump(home: &Path, sqlite_home: &Path, output: &Path, history_only: bool) -> Result<()> {
    check_path(output)?;
    ensure!(
        !output.try_exists()?,
        "Output already exists: {}",
        output.display()
    );
    let paths = inventory(home)?;
    ensure!(!paths.is_empty(), "No history files found");
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    private_dir(parent)?;
    ensure!(
        !fs::canonicalize(parent)?.starts_with(fs::canonicalize(home)?),
        "Dump output must be outside Codex home"
    );
    let staging = tempfile::tempdir_in(parent)?;
    let mut manifest = Manifest {
        format_version: 2,
        source_home: Some(fs::canonicalize(home)?),
        files: Vec::new(),
    };
    for path in &paths {
        let source = home.join(path);
        let before = fingerprint(&source)?;
        let dest = staging.path().join("files").join(path);
        write_new(&dest, &source)?;
        ensure!(
            before == fingerprint(&dest)? && before == fingerprint(&source)?,
            "Source changed during dump: {path}; stop Codex and retry"
        );
        manifest.files.push(Entry {
            path: path.clone(),
            size: before.0,
            sha256: before.1,
            modified: Some(fs::metadata(source)?.modified()?),
        });
    }
    ensure!(
        paths == inventory(home)?,
        "History inventory changed during dump"
    );
    for entry in &manifest.files {
        verify(&home.join(&entry.path), entry)?;
    }
    if !history_only {
        for name in DATABASES {
            let source = sqlite_home.join(name);
            check_path(&source)?;
            if !source.try_exists()? {
                continue;
            }
            let path = format!("sqlite/{name}");
            let dest = staging.path().join("files").join(&path);
            private_dir(dest.parent().unwrap())?;
            snapshot_db(&source, &dest)?;
            let (size, sha256) = fingerprint(&dest)?;
            manifest.files.push(Entry {
                path,
                size,
                sha256,
                modified: Some(fs::metadata(&source)?.modified()?),
            });
        }
    }
    ensure!(
        paths == inventory(home)?,
        "History inventory changed during database snapshot"
    );
    for entry in &manifest.files {
        if !entry.path.starts_with("sqlite/") {
            verify(&home.join(&entry.path), entry)?;
        }
    }
    let mut file = tempfile::NamedTempFile::new_in(staging.path())?;
    serde_json::to_writer_pretty(&mut file, &manifest)?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.persist_noclobber(staging.path().join("manifest.json"))?;
    // Reserve the destination so an existing snapshot can never be replaced.
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(output)?;
    for name in ["files", "manifest.json"] {
        fs::rename(staging.path().join(name), output.join(name))?;
    }
    println!(
        "Dumped {} files to {}",
        manifest.files.len(),
        output.display()
    );
    Ok(())
}

fn verify(path: &Path, entry: &Entry) -> Result<()> {
    let (size, hash) = fingerprint(path)?;
    ensure!(
        size == entry.size && hash == entry.sha256,
        "Checksum mismatch: {}",
        entry.path
    );
    Ok(())
}

fn snapshot_db(source: &Path, dest: &Path) -> Result<()> {
    check_path(source)?;
    let connection = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.backup("main", dest, None)?;
    let db = Connection::open(dest)?;
    ensure!(
        db.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0))? == "ok",
        "Invalid SQLite snapshot"
    );
    Ok(())
}

fn load(input: &Path) -> Result<Manifest> {
    check_path(&input.join("manifest.json"))?;
    let manifest: Manifest = serde_json::from_reader(fs::File::open(input.join("manifest.json"))?)?;
    ensure!(
        matches!(manifest.format_version, 1 | 2),
        "Unsupported snapshot version"
    );
    let mut seen = HashSet::new();
    for entry in &manifest.files {
        ensure!(
            allowed(&entry.path),
            "Unsupported snapshot path: {}",
            entry.path
        );
        ensure!(
            seen.insert(&entry.path),
            "Duplicate snapshot path: {}",
            entry.path
        );
        verify(&input.join("files").join(&entry.path), entry)?;
    }
    Ok(manifest)
}

fn lines(bytes: &[u8]) -> Result<Vec<serde_json::Value>> {
    std::str::from_utf8(bytes)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}

fn merge(path: &str, local: &[u8], incoming: &[u8]) -> Result<Vec<u8>> {
    if path == "history.jsonl" {
        let mut result = lines(local)?;
        let mut seen: HashSet<String> = result
            .iter()
            .map(serde_json::to_string)
            .collect::<std::result::Result<_, _>>()?;
        for entry in lines(incoming)? {
            ensure!(
                entry.get("session_id").and_then(|v| v.as_str()).is_some()
                    && entry.get("ts").and_then(|v| v.as_u64()).is_some()
                    && entry.get("text").and_then(|v| v.as_str()).is_some(),
                "Invalid history entry"
            );
            if seen.insert(serde_json::to_string(&entry)?) {
                result.push(entry);
            }
        }
        return encode_lines(result);
    }
    if path == "session_index.jsonl" {
        let mut entries: BTreeMap<
            String,
            (chrono::DateTime<chrono::FixedOffset>, serde_json::Value),
        > = BTreeMap::new();
        for value in lines(local)?.into_iter().chain(lines(incoming)?) {
            let id = value["id"].as_str().context("Missing index id")?.to_owned();
            ensure!(value["thread_name"].is_string(), "Missing thread name");
            let at = chrono::DateTime::parse_from_rfc3339(
                value["updated_at"].as_str().context("Missing updated_at")?,
            )?;
            if let Some((previous, old)) = entries.get(&id) {
                ensure!(
                    at != *previous || value == *old,
                    "Conflicting names at the same timestamp for {id}"
                );
                if at < *previous {
                    continue;
                }
            }
            entries.insert(id, (at, value));
        }
        return encode_lines(entries.into_values().map(|(_, v)| v).collect());
    }
    let (a, b) = if path.ends_with(".zst") {
        (
            zstd::stream::decode_all(local)?,
            zstd::stream::decode_all(incoming)?,
        )
    } else {
        (local.to_vec(), incoming.to_vec())
    };
    if a.starts_with(&b) && (b.is_empty() || b.ends_with(b"\n")) {
        return Ok(local.to_vec());
    }
    if b.starts_with(&a) && (a.is_empty() || a.ends_with(b"\n")) {
        return Ok(incoming.to_vec());
    }
    bail!("Conflict: divergent rollout {path}; no files written")
}

fn encode_lines(values: Vec<serde_json::Value>) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for value in values {
        serde_json::to_writer(&mut bytes, &value)?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}

struct Change {
    target: PathBuf,
    staged: PathBuf,
    before: Option<(u64, String)>,
    database: bool,
}

fn rebase_state(path: &Path, source_home: Option<&Path>, home: &Path) -> Result<()> {
    let db = Connection::open(path)?;
    let source_home =
        source_home.context("Snapshot is missing source_home for SQLite relocation")?;
    let paths = db
        .prepare("SELECT id, rollout_path FROM threads")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (id, old) in paths {
        let relative = Path::new(&old)
            .strip_prefix(source_home)
            .with_context(|| format!("Rollout path outside source Codex home for {id}"))?;
        ensure!(
            allowed(relative.to_str().context("Non UTF-8 rollout path")?)
                && !relative.starts_with("sqlite"),
            "Unsupported rollout path in database"
        );
        db.execute(
            "UPDATE threads SET rollout_path = ? WHERE id = ?",
            rusqlite::params![home.join(relative).to_string_lossy(), id],
        )?;
    }
    Ok(())
}

fn schedule_backfill(path: &Path) -> Result<()> {
    let db = Connection::open(path)?;
    db.execute("UPDATE backfill_state SET status = 'pending', last_watermark = NULL, last_success_at = NULL, updated_at = 0 WHERE id = 1", [])?;
    Ok(())
}

pub fn inject(
    input: &Path,
    home: &Path,
    sqlite_home: &Path,
    dry_run: bool,
    history_only: bool,
) -> Result<()> {
    let manifest = load(input)?;
    check_path(home)?;
    check_path(sqlite_home)?;
    let home = std::path::absolute(home)?;
    let sqlite_home = std::path::absolute(sqlite_home)?;
    // macOS exposes its temporary directory through the /var system symlink.
    // Our own staging path must be physical before the symlink checks below.
    let staging = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir())?)?;
    let mut changes = Vec::new();
    let mut skipped = 0;
    let mut kept_databases = 0;
    for (i, entry) in manifest.files.iter().enumerate() {
        let database = entry.path.starts_with("sqlite/");
        if database && history_only {
            kept_databases += 1;
            continue;
        }
        let target = if database {
            sqlite_home.join(entry.path.strip_prefix("sqlite/").unwrap())
        } else {
            home.join(&entry.path)
        };
        check_path(&target)?;
        let before = if target.try_exists()? {
            Some(fingerprint(&target)?)
        } else {
            None
        };
        if !database && before.as_ref() == Some(&(entry.size, entry.sha256.clone())) {
            skipped += 1;
            continue;
        }
        ensure!(
            !database || before.is_none(),
            "Destination database already exists: {}. Use --history-only to preserve it and reindex, or select a new Codex home.",
            target.display()
        );
        if !database && entry.path.contains('/') {
            let sibling = if entry.path.ends_with(".zst") {
                target.with_extension("")
            } else {
                PathBuf::from(format!("{}.zst", target.display()))
            };
            check_path(&sibling)?;
            ensure!(
                !sibling.try_exists()?,
                "Compressed/plain rollout conflict: {}",
                target.display()
            );
        }
        let source = input.join("files").join(&entry.path);
        let staged = staging.path().join(i.to_string());
        write_new(&staged, &source)?;
        verify(&staged, entry)?;
        if database {
            ensure!(
                Connection::open(&staged)?
                    .query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0))?
                    == "ok",
                "Invalid database"
            );
            if entry.path == "sqlite/state_5.sqlite" {
                rebase_state(&staged, manifest.source_home.as_deref(), &home)?;
            }
        } else if before.is_some() {
            let local = fs::read(&target)?;
            let merged = merge(&entry.path, &local, &fs::read(&staged)?)
                .with_context(|| format!("Cannot merge {}; no files written", entry.path))?;
            if merged == local {
                skipped += 1;
                continue;
            }
            fs::write(&staged, merged)?;
        }
        if let Some(modified) = entry.modified {
            fs::File::options()
                .write(true)
                .open(&staged)?
                .set_times(fs::FileTimes::new().set_modified(modified))?;
        }
        changes.push(Change {
            target,
            staged,
            before,
            database,
        });
    }
    let state = sqlite_home.join("state_5.sqlite");
    if changes.iter().any(|c| !c.database) && state.try_exists()? {
        check_path(&state)?;
        let staged = staging.path().join("reindexed-state.sqlite");
        snapshot_db(&state, &staged)?;
        let before = Some(fingerprint(&staged)?);
        schedule_backfill(&staged)?;
        changes.push(Change {
            target: state,
            staged,
            before,
            database: true,
        });
    }
    if dry_run {
        println!(
            "Would apply {} files; skipped {skipped}; kept {kept_databases} databases",
            changes.len()
        );
        return Ok(());
    }
    if changes.is_empty() {
        println!("No changes; skipped {skipped}; kept {kept_databases} databases");
        return Ok(());
    }
    private_dir(&home)?;
    let private = home.join(".syncodex");
    check_path(&private)?;
    private_dir(&private)?;
    let lock_path = private.join("inject.lock");
    check_path(&lock_path)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path)?;
    fs2::FileExt::try_lock_exclusive(&lock).context("Another SynCodex inject is running")?;
    // Recheck under the SynCodex lock. Codex itself must be stopped by the caller.
    for (i, change) in changes.iter().enumerate() {
        check_path(&change.target)?;
        let current = if change.target.try_exists()? {
            if change.database {
                let check = staging.path().join(format!("check-{i}.sqlite"));
                snapshot_db(&change.target, &check)?;
                Some(fingerprint(&check)?)
            } else {
                Some(fingerprint(&change.target)?)
            }
        } else {
            None
        };
        ensure!(
            current == change.before,
            "Destination changed during preflight"
        );
    }
    let recovery = tempfile::Builder::new()
        .prefix("recovery-")
        .tempdir_in(&private)?;
    let mut journal = Vec::new();
    for (i, change) in changes.iter().enumerate() {
        if change.target.try_exists()? {
            let backup = recovery.path().join(i.to_string());
            if change.database {
                snapshot_db(&change.target, &backup)?;
            } else {
                write_new(&backup, &change.target)?;
            }
            journal.push(serde_json::json!({"target":change.target,"backup":i.to_string(),"database":change.database}));
        } else {
            journal.push(serde_json::json!({"target":change.target,"backup":null,"database":change.database}));
        }
    }
    fs::write(
        recovery.path().join("journal.json"),
        serde_json::to_vec_pretty(&journal)?,
    )?;
    let recovery = recovery.keep();
    for change in &changes {
        if change.database && change.target.try_exists()? {
            // Restore API updates the destination transactionally, including WAL mode.
            let mut db = Connection::open(&change.target)?;
            db.restore(
                "main",
                &change.staged,
                None::<fn(rusqlite::backup::Progress)>,
            )?;
        } else if change.before.is_some() {
            let parent = change.target.parent().unwrap();
            let mut temp = tempfile::NamedTempFile::new_in(parent)?;
            std::io::copy(&mut fs::File::open(&change.staged)?, &mut temp)?;
            temp.as_file().set_times(
                fs::FileTimes::new().set_modified(fs::metadata(&change.staged)?.modified()?),
            )?;
            temp.as_file().sync_all()?;
            temp.persist(&change.target)?;
        } else {
            write_new(&change.target, &change.staged)?;
        }
    }
    println!(
        "Applied {} files; skipped {skipped}; kept {kept_databases} databases. Recovery originals: {}",
        changes.len(),
        recovery.display()
    );
    Ok(())
}

pub fn list(input: &Path) -> Result<()> {
    let manifest = load(input)?;
    for entry in manifest.files {
        if !entry.path.starts_with("sessions/") && !entry.path.starts_with("archived_sessions/") {
            continue;
        }
        let bytes = fs::read(input.join("files").join(&entry.path))?;
        let bytes = if entry.path.ends_with(".zst") {
            zstd::stream::decode_all(bytes.as_slice())?
        } else {
            bytes
        };
        let first = bytes
            .split(|b| *b == b'\n')
            .next()
            .context("Empty rollout")?;
        let meta: serde_json::Value = serde_json::from_slice(first)?;
        println!(
            "{}",
            serde_json::json!({"id":meta["payload"]["id"], "cwd":meta["payload"]["cwd"], "history_base":meta["payload"]["history_base"], "path":entry.path,"bytes":entry.size})
        );
    }
    Ok(())
}
