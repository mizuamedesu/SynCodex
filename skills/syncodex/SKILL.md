---
name: syncodex
description: Back up, restore, merge, and verify local Codex conversation history with the SynCodex CLI. Use when moving Codex conversations between machines or importing a SynCodex snapshot into an existing Codex home.
---

# SynCodex

Use `syncodex` from PATH, or `$HOME/.local/bin/syncodex` for the default installer location. This skill works in Codex and Claude Code; the data format it handles is **Codex history**, not Claude Code history.

## Choose the operation

- Backup: `syncodex dump /path/to/new-snapshot`. The output directory must not exist and must be outside the source Codex home.
- Inspect: `syncodex list /path/to/snapshot`. This prints thread IDs and paths without conversation text.
- Restore all conversation metadata into a new home: `syncodex --codex-home /path/to/new-home inject /path/to/snapshot`.
- Merge into an existing home: `syncodex --codex-home /path/to/existing-home inject /path/to/snapshot --history-only`. This retains local DB metadata and schedules indexing of imported history on the next Codex startup.
- Verify with the real Codex engine: `syncodex --codex-home /path/to/restored-home verify THREAD_ID --expect-text 'known message fragment'`. Requires `codex`; it resumes the thread and reads its items without sending a model turn. Its local DB may be updated.

Run the intended inject with `--dry-run` first. If the user already authorized the import, proceed after a successful preflight without asking again. Use explicit source and destination homes when operating across machines or testing restoration. Global `--sqlite-home` overrides a separate database location; SynCodex does not read Codex's `sqlite_home` from config.toml.

## Preserve history

Stop the **destination** Codex before inject or verify. Do not stop the agent hosting the current conversation or overwrite its live home; prepare a separate destination when working from inside Codex. For a consistent backup, prefer a stopped source; dump detects changes in persisted history but cannot capture unflushed records.

Rollout merges only accept identical files or complete-line prefix extensions. Divergent conversations and ambiguous names are conflicts, not a reason to concatenate files or force-replace databases. Inspect the error and retain both snapshots. Existing file replacements are backed up under the destination's `.syncodex/recovery-*` with a journal.

The snapshot includes active/archived rollouts, input history, names, and the supported conversation SQLite databases. It does not include authentication, configuration, working-tree files, externally referenced attachments, or Claude Code conversations. JSONL working directories are not rewritten. Transfer those resources only when the user requests them, and do not publish snapshots as part of a code push.

Report the snapshot/destination paths, actual checks completed, and any unresolved conflicts. A successful `verify` proves local resume and message retrieval, not a newly generated model response or GUI behavior.
