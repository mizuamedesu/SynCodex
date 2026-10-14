#!/usr/bin/env python3
"""Opt-in rewind verification with another thread loaded in a real Codex process."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import queue
import subprocess
import threading
import time
import uuid

parser = argparse.ArgumentParser()
parser.add_argument("--source-rollout", type=Path, required=True, help="Self-contained paginated JSONL to copy; source is never edited")
parser.add_argument("--output", type=Path, required=True, help="New private directory for test artifacts")
parser.add_argument("--binary", type=Path, default=Path("target/debug/syncodex"))
parser.add_argument("--codex", default="codex")
args = parser.parse_args()
raw = args.source_rollout.read_bytes()
source_hash = hashlib.sha256(raw).hexdigest()
meta = json.loads(raw.splitlines()[0])["payload"]
assert meta.get("history_mode") == "paginated" and not meta.get("history_base"), "Use a self-contained paginated JSONL"
args.output.mkdir(mode=0o700, parents=True, exist_ok=False)
root = args.output.resolve()
home = root / "home"
(home / "sessions").mkdir(parents=True)
target_id = meta["id"]
other_id = str(uuid.uuid4())
(home / "sessions" / args.source_rollout.name).write_bytes(raw)
other_lines = []
for line in raw.splitlines():
    value = json.loads(line)
    if value.get("type") == "session_meta":
        value["payload"].update(id=other_id, session_id=other_id)
    if value.get("payload", {}).get("thread_id") == target_id:
        value["payload"]["thread_id"] = other_id
    other_lines.append(json.dumps(value, ensure_ascii=False))
(home / "sessions" / f"rollout-2026-10-14T00-00-00-{other_id}.jsonl").write_text("\n".join(other_lines) + "\n")
(home / "history.jsonl").write_text(json.dumps({"session_id": other_id, "ts": 1791936000, "text": "live other input"}) + "\n")
env = dict(os.environ, CODEX_HOME=str(home), CODEX_SQLITE_HOME=str(home))
env.pop("CODEX_THREAD_ID", None)
binary = str(args.binary.resolve())
server = subprocess.Popen([args.codex, "app-server", "--stdio"], env=env, stdin=subprocess.PIPE,
    stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, bufsize=1)
messages = queue.Queue()
request_id = 0
rpc_mutex = threading.Lock()
stop = threading.Event()
updates = []
errors = []


def read_output():
    for line in server.stdout:
        messages.put(json.loads(line))


threading.Thread(target=read_output, daemon=True).start()


def rpc(method, params):
    global request_id
    with rpc_mutex:
        request_id += 1
        wanted = request_id
        server.stdin.write(json.dumps({"id": wanted, "method": method, "params": params}) + "\n")
        server.stdin.flush()
        deadline = time.monotonic() + 25
        while time.monotonic() < deadline:
            response = messages.get(timeout=deadline - time.monotonic())
            if response.get("id") == wanted:
                if "error" in response:
                    raise RuntimeError(response["error"])
                return response["result"]
        raise TimeoutError(method)


def update_other():
    try:
        for counter in range(100):
            if stop.is_set():
                break
            branch = f"syncodex-live-update-{counter}"
            rpc("thread/metadata/update", {"threadId": other_id, "gitInfo": {"branch": branch}})
            updates.append(branch)
            time.sleep(0.005)
    except Exception as error:
        errors.append(str(error))


try:
    rpc("initialize", {"clientInfo": {"name": "syncodex-live-test", "version": "0.1.0"},
        "capabilities": {"experimentalApi": True}})
    server.stdin.write(json.dumps({"method": "initialized"}) + "\n")
    server.stdin.flush()
    rpc("thread/read", {"threadId": target_id})
    rpc("thread/resume", {"threadId": other_id, "excludeTurns": True, "approvalPolicy": "never", "sandbox": "read-only"})
    listed = json.loads(subprocess.check_output([binary, "--codex-home", str(home), "sessions", "--json"], env=env, text=True))
    assert next(s for s in listed if s["id"] == target_id)["in_use"] is False
    assert next(s for s in listed if s["id"] == other_id)["in_use"] is True
    busy = subprocess.run([binary, "--codex-home", str(home), "rewind", other_id,
        "--message", "1", "--text", "must be rejected"], env=env, capture_output=True, text=True)
    assert busy.returncode and "is in use by Codex" in busy.stderr
    worker = threading.Thread(target=update_other, daemon=True)
    worker.start()
    rewound = subprocess.run([binary, "--codex-home", str(home), "rewind", target_id,
        "--message", "2", "--text", "LIVE_UNUSED_SESSION_EDIT_WITH_CODEX_RUNNING"],
        env=env, capture_output=True, text=True, timeout=25)
    stop.set()
    worker.join(timeout=30)
    assert not worker.is_alive(), "Metadata updater timed out"
    assert rewound.returncode == 0, rewound.stderr
    assert not errors, errors
    assert updates, "No concurrent metadata update was performed"
    current = rpc("thread/read", {"threadId": other_id})["thread"]
    assert current["gitInfo"]["branch"] == updates[-1]
    rpc("thread/resume", {"threadId": target_id, "excludeTurns": True,
        "approvalPolicy": "never", "sandbox": "read-only"})
    result = rpc("thread/items/list", {"threadId": target_id, "limit": 100})
    message_items = [v for v in result["data"] if v["item"]["type"] in ("userMessage", "agentMessage")]
    assert len(message_items) == 2
    assert "LIVE_UNUSED_SESSION_EDIT_WITH_CODEX_RUNNING" in json.dumps(message_items)
    assert "live other input" in (home / "history.jsonl").read_text()
    assert server.poll() is None
    assert hashlib.sha256(args.source_rollout.read_bytes()).hexdigest() == source_hash
    report = {"codex_version": subprocess.check_output([args.codex, "--version"], text=True).strip(),
        "home": str(home), "target_thread": target_id, "active_other_thread": other_id,
        "active_target_rejected": True, "other_session_updates_preserved": len(updates),
        "other_git_branch": updates[-1], "rewind": json.loads(rewound.stdout),
        "edited_message_items": len(message_items), "same_app_server_resume": True,
        "source_hash_unchanged": True, "model_turn_sent": False}
    (root / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))
finally:
    stop.set()
    server.kill()
    server.wait()
