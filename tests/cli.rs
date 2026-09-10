use std::{
    fs,
    path::Path,
    process::{Command, Output},
};
use tempfile::TempDir;

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_syncodex"))
        .arg("--codex-home")
        .arg(home)
        .args(args)
        .output()
        .unwrap()
}

fn fixture() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("source");
    fs::create_dir_all(home.join("sessions/2026/09/10")).unwrap();
    fs::create_dir_all(home.join("archived_sessions")).unwrap();
    fs::write(
        home.join("sessions/2026/09/10/rollout-test.jsonl"),
        b"{\"type\":\"session_meta\",\"payload\":{\"id\":\"example\"}}\n",
    )
    .unwrap();
    // Opaque bytes: compressed payloads must be copied, never parsed/re-encoded.
    fs::write(
        home.join("archived_sessions/rollout-old.jsonl.zst"),
        [0, 255, 1, 3],
    )
    .unwrap();
    fs::write(
        home.join("history.jsonl"),
        b"{\"session_id\":\"example\",\"ts\":1,\"text\":\"hello\"}\n",
    )
    .unwrap();
    fs::write(
        home.join("session_index.jsonl"),
        b"{\"id\":\"example\",\"thread_name\":\"name\",\"updated_at\":\"2026-09-10T00:00:00Z\"}\n",
    )
    .unwrap();
    for name in ["auth.json", "config.toml", "state_5.sqlite"] {
        fs::write(home.join(name), b"excluded").unwrap();
    }
    let backup = temp.path().join("backup");
    let result = run(&home, &["dump", backup.to_str().unwrap(), "--history-only"]);
    assert!(result.status.success(), "{:?}", result);
    (temp, home, backup)
}

#[test]
fn round_trip_dry_run_and_repeat_without_credentials() {
    let (temp, home, backup) = fixture();
    let target = temp.path().join("target");
    assert!(
        run(&target, &["inject", backup.to_str().unwrap(), "--dry-run"])
            .status
            .success()
    );
    assert!(!target.exists());
    for _ in 0..2 {
        let result = run(&target, &["inject", backup.to_str().unwrap()]);
        assert!(result.status.success(), "{:?}", result);
    }
    for name in [
        "history.jsonl",
        "session_index.jsonl",
        "sessions/2026/09/10/rollout-test.jsonl",
        "archived_sessions/rollout-old.jsonl.zst",
    ] {
        assert_eq!(
            fs::read(home.join(name)).unwrap(),
            fs::read(target.join(name)).unwrap()
        );
    }
    for name in ["auth.json", "config.toml", "state_5.sqlite"] {
        assert!(!target.join(name).exists());
        assert!(!backup.join("files").join(name).exists());
    }
    assert!(
        !run(&home, &["dump", backup.to_str().unwrap(), "--history-only"])
            .status
            .success()
    );
}

#[test]
fn conflicts_are_detected_before_any_write() {
    let (temp, _, backup) = fixture();
    let target = temp.path().join("target");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("history.jsonl"), "local history").unwrap();
    let result = run(&target, &["inject", backup.to_str().unwrap()]);
    assert!(!result.status.success());
    assert_eq!(
        fs::read_to_string(target.join("history.jsonl")).unwrap(),
        "local history"
    );
    assert_eq!(fs::read_dir(target).unwrap().count(), 1);
}

#[test]
fn corruption_is_detected_before_any_write() {
    let (temp, _, backup) = fixture();
    fs::write(backup.join("files/history.jsonl"), "corruption").unwrap();
    let target = temp.path().join("target");
    assert!(
        !run(&target, &["inject", backup.to_str().unwrap()])
            .status
            .success()
    );
    assert!(!target.exists());
}

#[test]
fn manifest_rejects_traversal_credentials_duplicates_and_unknown_versions() {
    for path in [
        "../auth.json",
        "/tmp/auth.json",
        "sessions/../../auth.json",
        "auth.json",
        "sessions\\x.jsonl",
    ] {
        let (temp, _, backup) = fixture();
        let manifest_path = backup.join("manifest.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["files"][0]["path"] = path.into();
        fs::write(manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let target = temp.path().join("target");
        assert!(
            !run(&target, &["inject", backup.to_str().unwrap()])
                .status
                .success()
        );
        assert!(!target.exists());
    }
    for duplicate in [true, false] {
        let (temp, _, backup) = fixture();
        let manifest_path = backup.join("manifest.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        if duplicate {
            let first = manifest["files"][0].clone();
            manifest["files"].as_array_mut().unwrap().push(first);
        } else {
            manifest["format_version"] = 99.into();
        }
        fs::write(manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let target = temp.path().join("target");
        assert!(
            !run(&target, &["inject", backup.to_str().unwrap()])
                .status
                .success()
        );
        assert!(!target.exists());
    }
}

#[cfg(unix)]
#[test]
fn source_and_destination_symlinks_are_rejected() {
    use std::os::unix::fs::symlink;
    let (temp, home, backup) = fixture();
    let outside = temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, home.join("sessions/link")).unwrap();
    assert!(
        !run(
            &home,
            &["dump", temp.path().join("second").to_str().unwrap()]
        )
        .status
        .success()
    );
    let target = temp.path().join("target");
    fs::create_dir(&target).unwrap();
    symlink(&outside, target.join("archived_sessions")).unwrap();
    assert!(
        !run(&target, &["inject", backup.to_str().unwrap()])
            .status
            .success()
    );
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
}

#[test]
fn append_merge_preserves_newer_rollout_and_unions_history() {
    let (temp, home, backup) = fixture();
    let target = temp.path().join("target");
    assert!(
        run(&target, &["inject", backup.to_str().unwrap()])
            .status
            .success()
    );
    let rollout = "sessions/2026/09/10/rollout-test.jsonl";
    let extended = format!(
        "{}{{\"type\":\"event_msg\",\"payload\":{{}}}}\n",
        fs::read_to_string(home.join(rollout)).unwrap()
    );
    fs::write(home.join(rollout), &extended).unwrap();
    fs::write(
        home.join("history.jsonl"),
        "{\"session_id\":\"example\",\"ts\":2,\"text\":\"new\"}\n",
    )
    .unwrap();
    fs::write(home.join("session_index.jsonl"), "{\"id\":\"example\",\"thread_name\":\"renamed\",\"updated_at\":\"2026-09-11T00:00:00Z\"}\n").unwrap();
    let newer = temp.path().join("newer");
    assert!(
        run(&home, &["dump", newer.to_str().unwrap(), "--history-only"])
            .status
            .success()
    );
    for input in [&newer, &backup, &newer] {
        let result = run(&target, &["inject", input.to_str().unwrap()]);
        assert!(result.status.success(), "{:?}", result);
    }
    assert_eq!(fs::read_to_string(target.join(rollout)).unwrap(), extended);
    assert_eq!(
        fs::read_to_string(target.join("history.jsonl"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    assert!(
        fs::read_to_string(target.join("session_index.jsonl"))
            .unwrap()
            .contains("renamed")
    );
    assert!(target.join(".syncodex").is_dir());
}

#[test]
fn divergent_rollout_and_equal_time_name_conflict_do_not_write() {
    for name_conflict in [true, false] {
        let (temp, _, backup) = fixture();
        let target = temp.path().join("target");
        assert!(
            run(&target, &["inject", backup.to_str().unwrap()])
                .status
                .success()
        );
        let path = if name_conflict {
            "session_index.jsonl"
        } else {
            "sessions/2026/09/10/rollout-test.jsonl"
        };
        let local = if name_conflict {
            "{\"id\":\"example\",\"thread_name\":\"other\",\"updated_at\":\"2026-09-10T00:00:00Z\"}\n"
        } else {
            "{\"type\":\"different\"}\n"
        };
        fs::write(target.join(path), local).unwrap();
        let before = fs::read(target.join("history.jsonl")).unwrap();
        assert!(
            !run(&target, &["inject", backup.to_str().unwrap()])
                .status
                .success()
        );
        assert_eq!(fs::read_to_string(target.join(path)).unwrap(), local);
        assert_eq!(fs::read(target.join("history.jsonl")).unwrap(), before);
    }
}

#[test]
fn sqlite_wal_backup_relocates_paths_and_preserves_metadata() {
    let (temp, home, _) = fixture();
    fs::remove_file(home.join("state_5.sqlite")).unwrap();
    let db = rusqlite::Connection::open(home.join("state_5.sqlite")).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
        CREATE TABLE threads(id TEXT PRIMARY KEY, rollout_path TEXT, title TEXT);
        CREATE TABLE backfill_state(id INTEGER PRIMARY KEY, status TEXT, last_watermark TEXT, last_success_at INTEGER, updated_at INTEGER);
        INSERT INTO backfill_state VALUES(1,'complete','old',1,1);").unwrap();
    let relative = "sessions/2026/09/10/rollout-test.jsonl";
    db.execute(
        "INSERT INTO threads VALUES('example',?,'custom title')",
        [home.join(relative).to_str().unwrap()],
    )
    .unwrap();
    assert!(home.join("state_5.sqlite-wal").exists());
    let snapshot = temp.path().join("with-state");
    let result = run(&home, &["dump", snapshot.to_str().unwrap()]);
    assert!(result.status.success(), "{:?}", result);
    let target = temp.path().join("target");
    let result = run(
        &target,
        &["inject", snapshot.to_str().unwrap(), "--dry-run"],
    );
    assert!(result.status.success(), "{:?}", result);
    assert!(!target.exists());
    let result = run(&target, &["inject", snapshot.to_str().unwrap()]);
    assert!(result.status.success(), "{:?}", result);
    let restored = rusqlite::Connection::open(target.join("state_5.sqlite")).unwrap();
    let (path, title): (String, String) = restored
        .query_row("SELECT rollout_path,title FROM threads", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(path, target.join(relative).to_str().unwrap());
    assert_eq!(title, "custom title");
    assert!(
        !run(&target, &["inject", snapshot.to_str().unwrap()])
            .status
            .success()
    );
    // Add a file to an already-indexed destination: existing metadata survives reindex scheduling.
    fs::remove_file(target.join("history.jsonl")).unwrap();
    let result = run(
        &target,
        &["inject", snapshot.to_str().unwrap(), "--history-only"],
    );
    assert!(result.status.success(), "{:?}", result);
    let status: String = restored
        .query_row("SELECT status FROM backfill_state", [], |r| r.get(0))
        .unwrap();
    assert_eq!(status, "pending");
    assert_eq!(
        restored
            .query_row("SELECT title FROM threads", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "custom title"
    );
}

#[test]
fn modified_time_survives_snapshot_file_timestamp_loss() {
    let (temp, home, backup) = fixture();
    let original = fs::metadata(home.join("history.jsonl"))
        .unwrap()
        .modified()
        .unwrap();
    fs::File::options()
        .write(true)
        .open(backup.join("files/history.jsonl"))
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
        .unwrap();
    let target = temp.path().join("target");
    assert!(
        run(&target, &["inject", backup.to_str().unwrap()])
            .status
            .success()
    );
    assert_eq!(
        fs::metadata(target.join("history.jsonl"))
            .unwrap()
            .modified()
            .unwrap(),
        original
    );
}
