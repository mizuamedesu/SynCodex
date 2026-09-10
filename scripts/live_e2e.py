#!/usr/bin/env python3
"""Opt-in integration test against an actual local Codex history. No model turn."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess

parser = argparse.ArgumentParser()
parser.add_argument("--source", type=Path, default=Path.home() / ".codex")
parser.add_argument("--sqlite-home", type=Path)
parser.add_argument("--output", type=Path, required=True, help="New directory for private test artifacts")
parser.add_argument("--thread", required=True)
parser.add_argument("--expect-text", action="append", default=[])
parser.add_argument("--binary", type=Path, default=Path("target/debug/syncodex"))
args = parser.parse_args()
args.output.mkdir(mode=0o700, parents=True, exist_ok=False)
root = args.output.resolve()
snapshot, restored = root / "snapshot", root / "restored"
binary = str(args.binary.resolve())
env = dict(os.environ)
env.pop("CODEX_SQLITE_HOME", None)

def run(home, *command, sqlite_home=None):
    cmd = [binary, "--codex-home", str(home)]
    if sqlite_home:
        cmd += ["--sqlite-home", str(sqlite_home)]
    return subprocess.check_output(cmd + list(command), env=env, text=True)

print(run(args.source, "dump", str(snapshot), sqlite_home=args.sqlite_home), end="")
print(run(restored, "inject", str(snapshot), "--dry-run"), end="")
assert not restored.exists(), "dry-run wrote the destination"
print(run(restored, "inject", str(snapshot)), end="")
manifest = json.loads((snapshot / "manifest.json").read_text())
checked = 0
for entry in manifest["files"]:
    if entry["path"].startswith("sqlite/"):
        continue
    restored_file = restored / entry["path"]
    assert hashlib.sha256(restored_file.read_bytes()).hexdigest() == entry["sha256"]
    checked += 1
state = restored / "state_5.sqlite"
if state.exists():
    with sqlite3.connect(f"file:{state}?mode=ro", uri=True) as db:
        assert db.execute("PRAGMA quick_check").fetchone()[0] == "ok"
        for (path,) in db.execute("SELECT rollout_path FROM threads"):
            assert Path(path).is_relative_to(restored)
assert not (restored / "auth.json").exists()
expected = []
for text in args.expect_text:
    expected += ["--expect-text", text]
report = json.loads(run(restored, "verify", args.thread, *expected))
report.update({"files_verified_before_resume": checked, "snapshot_files": len(manifest["files"]),
               "codex_version": subprocess.check_output(["codex", "--version"], text=True).strip(),
               "snapshot": str(snapshot), "credentials_copied": False})
(root / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
print(json.dumps(report, ensure_ascii=False, indent=2))
