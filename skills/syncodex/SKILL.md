---
name: syncodex
description: Back up, restore, merge, verify, and rewind local Codex conversation history with the SynCodex CLI. Use when moving Codex conversations between machines, importing snapshots, listing sessions, or editing a user/assistant message and discarding its later history.
---

# SynCodex

Use `syncodex` from PATH, or `$HOME/.local/bin/syncodex` for the default installer location. This skill works in Codex and Claude Code; the data format it handles is **Codex history**, not Claude Code history.

## Choose the operation

- Backup: `syncodex dump /path/to/new-snapshot`. The output directory must not exist and must be outside the source Codex home.
- Inspect: `syncodex list /path/to/snapshot`. This prints thread IDs and paths without conversation text.
- Browse local sessions: `syncodex sessions --json`, with optional `--search TEXT` and `--archived`.
- Inspect editable messages: `syncodex messages THREAD_ID --json`. Message indices start at 1 and include user and assistant messages.
- Edit and rewind: `syncodex rewind THREAD_ID --message N --text-file /path/to/edited-message.md`. The selected message is retained with its new text; all later conversation history is discarded. `--text TEXT` also works. In a user's terminal, `syncodex rewind` opens a session browser, a message browser with full previews, and nano by default, regardless of VISUAL/EDITOR. In nano, Ctrl+O then Enter saves; Ctrl+X exits and applies the saved edit. Ctrl+X then N cancels an unsaved edit. `--vim` or `--editor vim` selects Vim; `--editor COMMAND` selects another editor. `--editor builtin` opts into the built-in editor: Ctrl+S opens the before/after review with removed-message counts and Enter there applies the edit. Esc returns to the previous picker screen. Automatic approval reviews and other internal sessions are hidden initially; F2 or `--include-internal` shows them. F3 toggles archived sessions. Use `--json` for a machine-readable result in a terminal; piped output is always JSON.
- Restore all conversation metadata into a new home: `syncodex --codex-home /path/to/new-home inject /path/to/snapshot`.
- Merge into an existing home: `syncodex --codex-home /path/to/existing-home inject /path/to/snapshot --history-only`. This retains local DB metadata and schedules indexing of imported history on the next Codex startup.
- Verify with the real Codex engine: `syncodex --codex-home /path/to/restored-home verify THREAD_ID --expect-text 'known message fragment'`. Requires `codex`; it resumes the thread and reads its items without sending a model turn. Its local DB may be updated.

Run the intended inject or rewind with `--dry-run` first. For rewind, resolve the user's chosen session and message, prepare the exact replacement text, and report the retained/removed message counts. Ask for missing target or replacement information; do not infer a session or cut point. If the user already authorized the import or the specific edit, proceed after a successful preflight without asking again. Use explicit source and destination homes when operating across machines or testing restoration. Global `--sqlite-home` overrides a separate database location; SynCodex does not read Codex's `sqlite_home` from config.toml.

## Preserve history

Stop the **destination** Codex before inject or verify. Rewind can edit an unused session while other Codex sessions in the same home keep running. The CLI checks and acquires Codex's native per-thread writer locks for the selected session and dependent forks. Close/unload only those sessions if marked in use. Do not stop or rewind the session hosting the current agent. For inject/verify from inside Codex, prepare a separate destination. For a consistent backup, prefer a stopped source; dump detects changes in persisted history but cannot capture unflushed records.

Rollout merges only accept identical files or complete-line prefix extensions. Divergent conversations and ambiguous names are conflicts, not a reason to concatenate files or force-replace databases. Inspect the error and retain both snapshots. Existing file replacements are backed up under the destination's `.syncodex/recovery-*` with a journal.

Rewind is a deliberate history rewrite, separate from inject's prefix-only merge. It updates model-visible and displayed text, preserves attachments and assistant phases, truncates later records, rebuilds the selected session's input history, and invalidates its SQLite history projection. It applies SQL changes only to affected thread rows and updates the current input history under Codex's file lock; other sessions' live updates remain intact. Forks referencing modified rollouts are materialized with their original content. Report recovery paths and preserved fork counts. Rewinding does not reverse tool side effects in the working tree or generate a new model response. A recovery journal is not an inject snapshot. Rewind recovery databases contain affected rows only: restore `thread_ids` according to `restore_scope=thread_rows`, never restore the whole database. Input history recovery likewise uses the journal's `session_ids`.

The snapshot includes active/archived rollouts, input history, names, and the supported conversation SQLite databases. It does not include authentication, configuration, working-tree files, externally referenced attachments, or Claude Code conversations. JSONL working directories are not rewritten. Transfer those resources only when the user requests them, and do not publish snapshots as part of a code push.

Report the snapshot/destination paths, actual checks completed, and any unresolved conflicts. A successful `verify` proves local resume and message retrieval, not a newly generated model response or GUI behavior.
