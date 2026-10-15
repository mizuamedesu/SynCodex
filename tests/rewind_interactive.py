#!/usr/bin/env python3
"""Exercise the real terminal pickers/editor using private synthetic histories."""
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import shutil
import sqlite3
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unittest

BINARY = str(Path(os.environ.get("SYNCODEX_TEST_BINARY", "target/debug/syncodex")).resolve())
ANSI_CSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")


class InteractiveRewind(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="syncodex-terminal-", dir=str(Path(tempfile.gettempdir()).resolve()))
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.home = self.root / "home"
        self.rollout = self.home / "sessions" / "rollout-test.jsonl"
        self.rollout.parent.mkdir(parents=True)
        self.values = [
            {"type": "session_meta", "payload": {"id": "terminal-test", "cwd": "/project", "history_mode": "paginated"}},
            {"type": "event_msg", "payload": {"type": "task_started", "turn_id": "turn-test"}},
            {"type": "response_item", "payload": {"type": "message", "id": "user-model", "role": "user", "content": [{"type": "input_text", "text": "first request"}]}},
            {"type": "event_msg", "payload": {"type": "item_completed", "turn_id": "turn-test", "item": {"type": "UserMessage", "id": "user-ui", "content": [{"type": "text", "text": "first request"}]}}},
            {"type": "event_msg", "payload": {"type": "item_completed", "turn_id": "turn-test", "item": {"type": "AgentMessage", "id": "assistant-model", "phase": "final_answer", "content": [{"type": "Text", "text": "last answer"}]}}},
            {"type": "response_item", "payload": {"type": "message", "id": "assistant-model", "role": "assistant", "phase": "final_answer", "content": [{"type": "output_text", "text": "last answer"}]}},
        ]
        for ordinal, value in enumerate(self.values):
            value.update(timestamp="2026-10-14T00:00:00Z", ordinal=ordinal)
        self.original = ("\n".join(json.dumps(v) for v in self.values) + "\n").encode()
        self.rollout.write_bytes(self.original)
        self.editor = self.root / "editor.py"

    def terminal(self, actions, *, builtin=False, default_editor=False, extra_args=(), size=(24, 110), env_overrides=None):
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", *size, 0, 0))
        env = dict(os.environ, TERM="xterm-256color")
        env.pop("CODEX_SQLITE_HOME", None)
        env.pop("CODEX_THREAD_ID", None)
        if env_overrides:
            env.update(env_overrides)

        def controlling_terminal():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        command = [BINARY, "--codex-home", str(self.home), "rewind", "--json", *extra_args]
        if builtin:
            command += ["--editor", "builtin"]
        elif not default_editor:
            command += ["--editor", f"{sys.executable} {self.editor}"]
        child = subprocess.Popen(command, stdin=slave, stdout=slave, stderr=slave, env=env, preexec_fn=controlling_terminal)
        os.close(slave)
        output = bytearray()
        pending = list(actions)
        search_from = 0
        deadline = time.monotonic() + 15
        try:
            while time.monotonic() < deadline:
                if select.select([master], [], [], 0.1)[0]:
                    try:
                        chunk = os.read(master, 65536)
                    except OSError as error:
                        if error.errno == errno.EIO:
                            break
                        raise
                    if not chunk:
                        break
                    output.extend(chunk)
                    # Wide Japanese glyphs are separated by cursor-position escapes.
                    text = ANSI_CSI.sub("", output.decode(errors="replace"))
                    found = text.find(pending[0][0], search_from) if pending else -1
                    if found >= 0:
                        search_from = found + len(pending[0][0])
                        _, keys = pending.pop(0)
                        if callable(keys):
                            keys = keys()
                        os.write(master, keys)
                elif child.poll() is not None:
                    break
            if child.poll() is None:
                child.kill()
            status = child.wait()
            text = ANSI_CSI.sub("", output.decode(errors="replace"))
            self.assertFalse(pending, text)
            return status, text
        finally:
            if child.poll() is None:
                child.kill()
                child.wait()
            os.close(master)

    def choose_first_message(self):
        return [("セッションを選択", b"terminal\r"), ("発言を選択", b"\x1b[A\r")]

    def result(self, output):
        # Alternate-screen output need not end in a newline before stdout JSON.
        starts = list(re.finditer(r'\{\r?\n\s*"changed_files"', output))
        self.assertTrue(starts, output)
        return json.loads(output[starts[-1].start():].strip())

    def test_search_select_and_save_multiline_edit(self):
        self.editor.write_text("from pathlib import Path\nimport sys\nPath(sys.argv[-1]).write_text('変更した依頼😀\\n二行目\\n')\n")
        status, output = self.terminal(self.choose_first_message())
        self.assertEqual(status, 0, output)
        result = self.result(output)
        self.assertEqual(result["kept_messages"], 1)
        self.assertEqual(result["removed_messages"], 1)
        values = [json.loads(line) for line in self.rollout.read_text().splitlines()]
        self.assertEqual(values[2]["payload"]["content"][0]["text"], "変更した依頼😀\n二行目\n")
        self.assertTrue(Path(result["recovery"]).joinpath("journal.json").exists())

    def test_failed_editor_does_not_apply_even_if_it_wrote_the_tempfile(self):
        self.editor.write_text("from pathlib import Path\nimport sys\nPath(sys.argv[-1]).write_text('must not be applied')\nsys.exit(1)\n")
        status, output = self.terminal(self.choose_first_message())
        self.assertNotEqual(status, 0, output)
        self.assertIn("no history changed", output)
        self.assertEqual(self.rollout.read_bytes(), self.original)
        self.assertFalse(self.home.joinpath(".syncodex").exists())

    @unittest.skipUnless(shutil.which("nano"), "nano is not installed")
    def test_default_nano_handles_non_utf8_and_invalid_locales(self):
        for locale in ["C", "syncodex_missing_locale.UTF-8"]:
            with self.subTest(locale=locale):
                self.rollout.write_bytes(self.original)
                actions = self.choose_first_message() + [
                    ("^X", b"\x0b" + "続けて\n日本語😀".encode() + b"\x0f"),
                    ("File Name", b"\r\x18"),
                ]
                status, output = self.terminal(actions, default_editor=True,
                    env_overrides={"LC_ALL":locale, "LC_CTYPE":"C", "LANG":"C"})
                self.assertEqual(status, 0, output)
                values = [json.loads(line) for line in self.rollout.read_text().splitlines()]
                self.assertEqual(values[2]["payload"]["content"][0]["text"].rstrip("\n"), "続けて\n日本語😀")
                self.assertEqual(self.result(output)["removed_messages"], 1)

    @unittest.skipUnless(shutil.which("nano"), "nano is not installed")
    def test_default_nano_uses_utf8_without_locale_utility(self):
        directory = self.root / "only-nano"
        directory.mkdir()
        (directory/"nano").symlink_to(shutil.which("nano"))
        actions = self.choose_first_message() + [
            ("^X", b"\x0b" + "続けて😀".encode() + b"\x0f"),
            ("File Name", b"\r\x18"),
        ]
        status, output = self.terminal(actions, default_editor=True,
            env_overrides={"PATH":str(directory), "LC_ALL":"C", "LANG":"C"})
        self.assertEqual(status, 0, output)
        values = [json.loads(line) for line in self.rollout.read_text().splitlines()]
        self.assertEqual(values[2]["payload"]["content"][0]["text"].rstrip("\n"), "続けて😀")

    @unittest.skipUnless(shutil.which("nano"), "nano is not installed")
    def test_default_nano_saves_and_overrides_editor_environment(self):
        actions = self.choose_first_message() + [
            ("^X", b"\x0b" + "nano で編集した依頼\n二行目".encode() + b"\x0f"),
            ("File Name", b"\r\x18"),
        ]
        status, output = self.terminal(actions, default_editor=True, env_overrides={"EDITOR":"vim", "VISUAL":"vim", "LC_ALL":"en_US.UTF-8", "LANG":"en_US.UTF-8"})
        self.assertEqual(status, 0, output)
        self.assertIn("nano:", output)
        self.assertEqual(self.result(output)["removed_messages"], 1)
        values = [json.loads(line) for line in self.rollout.read_text().splitlines()]
        self.assertEqual(values[2]["payload"]["content"][0]["text"].rstrip("\n"), "nano で編集した依頼\n二行目")

    @unittest.skipUnless(shutil.which("nano"), "nano is not installed")
    def test_default_nano_can_cancel_without_saving(self):
        actions = self.choose_first_message() + [
            ("^X", b"discard this edit\x18"),
            ("Save modified", b"n"),
        ]
        status, output = self.terminal(actions, default_editor=True, env_overrides={"LC_ALL":"en_US.UTF-8", "LANG":"en_US.UTF-8"})
        self.assertEqual(status, 0, output)
        self.assertIn("Cancelled", output)
        self.assertEqual(self.rollout.read_bytes(), self.original)
        self.assertFalse(self.home.joinpath(".syncodex").exists())

    def test_missing_nano_explains_install_and_preserves_history(self):
        directory = self.root / "empty-path"
        directory.mkdir()
        status, output = self.terminal(self.choose_first_message(), default_editor=True,
            env_overrides={"PATH":str(directory)})
        self.assertNotEqual(status, 0, output)
        self.assertIn("nano が見つかりません", output)
        self.assertIn("releases/latest/download/install.sh", output)
        self.assertIn("syncodex rewind --editor builtin", output)
        self.assertEqual(self.rollout.read_bytes(), self.original)
        self.assertFalse(self.home.joinpath(".syncodex").exists())

    def test_vim_flag_selects_vim_instead_of_nano(self):
        directory = self.root / "bin"
        directory.mkdir()
        vim = directory / "vim"
        vim.write_text(f"#!{sys.executable}\nfrom pathlib import Path\nimport sys\nPath(sys.argv[-1]).write_text('edited with optional vim')\n")
        vim.chmod(0o755)
        status, output = self.terminal(self.choose_first_message(), default_editor=True,
            extra_args=("--vim",), env_overrides={"PATH":str(directory)+os.pathsep+os.environ["PATH"]})
        self.assertEqual(status, 0, output)
        self.assertIn("Vim:", output)
        self.assertIn("edited with optional vim", self.rollout.read_text())

    def test_builtin_editor_paste_and_preview_before_commit(self):
        replacement = "変更した依頼😀\n二行目\n"
        def confirm():
            self.assertEqual(self.rollout.read_bytes(), self.original)
            self.assertFalse(self.home.joinpath(".syncodex").exists())
            return b"\r"
        actions = self.choose_first_message() + [
            ("本文を編集", b"\x01\x1b[200~" + replacement.encode() + b"\x1b[201~\x1a\x19\x13"),
            ("変更を確認", confirm),
        ]
        status, output = self.terminal(actions, builtin=True)
        self.assertEqual(status, 0, output)
        self.assertIn("変更前", output)
        self.assertIn("変更後", output)
        self.assertIn("この後の 1 件を削除", output)
        result = self.result(output)
        self.assertEqual(result["kept_messages"], 1)
        self.assertEqual(result["removed_messages"], 1)
        messages = json.loads(subprocess.check_output([BINARY, "--codex-home", str(self.home), "messages", "terminal-test", "--json"], env={k:v for k,v in os.environ.items() if k not in {"CODEX_SQLITE_HOME", "CODEX_THREAD_ID"}}))
        self.assertEqual(messages[0]["text"], replacement)

    def test_builtin_discard_returns_to_message_list_without_writing(self):
        actions = self.choose_first_message() + [
            ("本文を編集", b"discard this edit\x1b"),
            ("編集内容を破棄しますか", b"\r"),
            ("発言を選択", b"\x1b"),
            ("セッションを選択", b"\x1b"),
        ]
        status, output = self.terminal(actions, builtin=True)
        self.assertEqual(status, 0, output)
        self.assertIn("Cancelled", output)
        self.assertEqual(self.rollout.read_bytes(), self.original)
        self.assertFalse(self.home.joinpath(".syncodex").exists())
        self.assertFalse(self.home.joinpath("thread-writer-locks").exists())

    def test_builtin_confirmation_can_return_to_editor_and_save_new_text(self):
        actions = self.choose_first_message() + [
            ("本文を編集", b"\x01temporary replacement\x13"),
            ("変更を確認", b"\x1b"),
            ("本文を編集", b"\x01\x1b[200~final replacement\x1b[201~\x13"),
            ("変更を確認", b"\r"),
        ]
        status, output = self.terminal(actions, builtin=True)
        self.assertEqual(status, 0, output)
        self.assertIn("final replacement", self.rollout.read_text())
        self.assertNotIn("temporary replacement", self.rollout.read_text())

    def test_internal_approval_sessions_are_hidden_until_toggled(self):
        guardian = self.home / "sessions" / "rollout-guardian.jsonl"
        head = {**self.values[0], "payload": {**self.values[0]["payload"], "id": "guardian-test", "source": {"subagent": {"other": "guardian"}}}}
        guardian.write_text(json.dumps(head) + "\n")
        status, output = self.terminal([("セッションを選択", b"\x1b")], builtin=True)
        self.assertEqual(status, 0, output)
        self.assertIn("内部会話 1 件を非表示", output)
        self.assertNotIn("自動生成", output)
        status, output = self.terminal([
            ("セッションを選択", b"\x1bOQ"),  # F2
            ("自動生成", b"\x1b"),
        ], builtin=True)
        self.assertEqual(status, 0, output)
        self.assertEqual(guardian.read_text(), json.dumps(head) + "\n")

    def test_narrow_terminal_wraps_full_text_and_commits_without_external_editor(self):
        actions = self.choose_first_message() + [
            ("本文を編集", b"\x01\x1b[200~" + "横幅が狭くても編集できます\n次の行".encode() + b"\x1b[201~\x13"),
            ("変更を確認", b"\r"),
        ]
        status, output = self.terminal(actions, builtin=True, size=(24, 80))
        self.assertEqual(status, 0, output)
        self.assertIn("横幅が狭くても編集できます", self.rollout.read_text())
        self.assertIn("次の行", self.rollout.read_text())

    def test_builtin_dry_run_does_not_write_history_or_locks(self):
        actions = self.choose_first_message() + [
            ("本文を編集", b"\x01\x1b[200~dry run edit\x1b[201~\x13"),
            ("変更を確認", b"\r"),
        ]
        status, output = self.terminal(actions, builtin=True, extra_args=("--dry-run",))
        self.assertEqual(status, 0, output)
        self.assertTrue(self.result(output)["dry_run"])
        self.assertEqual(self.rollout.read_bytes(), self.original)
        self.assertFalse(self.home.joinpath(".syncodex").exists())
        self.assertFalse(self.home.joinpath("thread-writer-locks").exists())

    def test_comparison_keeps_original_scrollable_when_replacement_is_short(self):
        text = "\n".join(f"original line {i}" for i in range(40)) + "\nORIGINAL_CONFIRM_END"
        self.values[2]["payload"]["content"][0]["text"] = text
        self.values[3]["payload"]["item"]["content"][0]["text"] = text
        self.rollout.write_text("\n".join(json.dumps(v) for v in self.values) + "\n")
        actions = self.choose_first_message() + [
            ("本文を編集", b"\x01\x1b[200~short replacement\x1b[201~\x13"),
            ("変更を確認", b"\x1b[6~" * 8),
            ("ORIGINAL_CONFIRM_END", b"\r"),
        ]
        status, output = self.terminal(actions, builtin=True)
        self.assertEqual(status, 0, output)
        self.assertEqual(self.result(output)["kept_messages"], 1)
        self.assertIn("short replacement", self.rollout.read_text())

    def test_cancel_session_picker_and_editor_without_saving(self):
        self.editor.write_text("pass\n")
        for actions in [[("セッションを選択", b"\x1b")], self.choose_first_message()]:
            status, output = self.terminal(actions)
            self.assertEqual(status, 0, output)
            self.assertIn("Cancelled", output)
            self.assertEqual(self.rollout.read_bytes(), self.original)
            self.assertFalse(self.home.joinpath(".syncodex").exists())

    def test_detects_source_changes_while_editor_is_open(self):
        self.editor.write_text("from pathlib import Path\nimport sys\n"
            f"source = Path({str(self.rollout)!r})\n"
            "source.write_bytes(source.read_bytes() + b'\\n')\n"
            "Path(sys.argv[-1]).write_text('should never be applied')\n")
        status, output = self.terminal(self.choose_first_message())
        self.assertNotEqual(status, 0, output)
        self.assertIn("Session changed during editing", output)
        self.assertEqual(self.rollout.read_bytes(), self.original + b"\n")
        self.assertFalse(self.home.joinpath(".syncodex").exists())

    def test_other_session_updates_and_new_sessions_during_edit_are_preserved(self):
        other = self.home / "sessions" / "rollout-other.jsonl"
        other.write_text(json.dumps({"timestamp":"2026-10-14T00:00:00Z","ordinal":0,"type":"session_meta",
            "payload":{"id":"other","cwd":"/other","history_mode":"paginated"}}) + "\n")
        history = self.home / "history.jsonl"
        history.write_text(json.dumps({"session_id":"terminal-test","ts":1791936000,"text":"first request"}) + "\n")
        state = self.home / "state_5.sqlite"
        with sqlite3.connect(state) as db:
            db.execute("PRAGMA journal_mode=WAL")
            db.execute("CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT, title TEXT, preview TEXT, first_user_message TEXT, is_pinned INTEGER)")
            db.executemany("INSERT INTO threads VALUES (?, ?, ?, ?, ?, 0)", [
                ("terminal-test", str(self.rollout), "first request", "first request", "first request"),
                ("other", str(other), "old other title", "old preview", "old first")])
        self.editor.write_text("from pathlib import Path\nimport sys,json,sqlite3,fcntl\n"
            f"root = Path({str(self.home)!r})\n"
            "with sqlite3.connect(root/'state_5.sqlite') as db:\n"
            "    db.execute(\"UPDATE threads SET title='new other title', preview='new preview', is_pinned=1 WHERE id='other'\")\n"
            "with (root/'history.jsonl').open('a') as f:\n"
            "    fcntl.flock(f,fcntl.LOCK_EX)\n"
            "    f.write(json.dumps({'session_id':'other','ts':1791936001,'text':'live other input'})+'\\n')\n"
            "with (root/'sessions/rollout-other.jsonl').open('a') as f: f.write('\\n')\n"
            "(root/'sessions/rollout-new.jsonl').write_text(json.dumps({'timestamp':'2026-10-14T00:00:00Z','ordinal':0,'type':'session_meta','payload':{'id':'new-session','cwd':'/new','history_mode':'paginated'}})+'\\n')\n"
            "Path(sys.argv[-1]).write_text('edited while another session is running')\n")
        status, output = self.terminal(self.choose_first_message())
        self.assertEqual(status, 0, output)
        with sqlite3.connect(state) as db:
            self.assertEqual(db.execute("SELECT title,preview,is_pinned FROM threads WHERE id='other'").fetchone(),
                ("new other title", "new preview", 1))
            self.assertEqual(db.execute("SELECT first_user_message FROM threads WHERE id='terminal-test'").fetchone()[0],
                "edited while another session is running")
        self.assertIn("live other input", history.read_text())
        self.assertTrue(other.read_text().endswith("\n\n"))
        self.assertTrue(self.home.joinpath("sessions/rollout-new.jsonl").exists())

    def test_inherited_prefix_can_be_edited_while_parent_appends(self):
        parent = self.home / "sessions" / "rollout-parent.jsonl"
        parent_values = json.loads(self.original.decode().splitlines()[0])
        parent_values["payload"]["id"] = "parent-session"
        lines = self.original.decode().splitlines(keepends=True)
        lines[0] = json.dumps(parent_values) + "\n"
        parent_bytes = ''.join(lines).encode()
        parent.write_bytes(parent_bytes)
        head = self.values[0].copy()
        head["payload"] = dict(head["payload"], history_base={"thread_id":"parent-session",
            "end_ordinal_exclusive":len(self.values), "end_byte_offset":len(parent_bytes)})
        head["ordinal"] = len(self.values)
        self.rollout.write_text(json.dumps(head) + "\n")
        self.editor.write_text("from pathlib import Path\nimport sys\n"
            f"parent = Path({str(parent)!r})\n"
            "with parent.open('ab') as f: f.write(b'{incomplete record still being appended')\n"
            "Path(sys.argv[-1]).write_text('edited inherited request')\n")
        status, output = self.terminal(self.choose_first_message())
        self.assertEqual(status, 0, output)
        self.assertEqual(parent.read_bytes(), parent_bytes + b'{incomplete record still being appended')
        self.assertIn("edited inherited request", self.rollout.read_text())
        self.assertNotIn("history_base", self.rollout.read_text())

    def test_target_opened_during_editor_is_rejected(self):
        lock_dir = self.home / "thread-writer-locks"
        lock_dir.mkdir()
        lock_path = lock_dir / "terminal-test.lock"
        self.editor.write_text("from pathlib import Path\nimport subprocess,sys\n"
            f"lock = {str(lock_path)!r}\n"
            "program = 'import fcntl,sys,time; f=open(sys.argv[1],\"w\"); fcntl.flock(f,fcntl.LOCK_EX); print(\"ready\",flush=True); time.sleep(20)'\n"
            "child=subprocess.Popen([sys.executable,'-c',program,lock],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL)\n"
            "child.stdout.readline()\n"
            f"Path({str(self.root/'holder.pid')!r}).write_text(str(child.pid))\n"
            "Path(sys.argv[-1]).write_text('must not overwrite a session opened during editing')\n")
        try:
            status, output = self.terminal(self.choose_first_message())
            self.assertNotEqual(status, 0, output)
            self.assertIn("is in use by Codex", output)
            self.assertEqual(self.rollout.read_bytes(), self.original)
        finally:
            pid_path = self.root / "holder.pid"
            if pid_path.exists():
                try: os.kill(int(pid_path.read_text()), 15)
                except ProcessLookupError: pass


if __name__ == "__main__":
    unittest.main()
