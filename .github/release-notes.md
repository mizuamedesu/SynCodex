Codex の会話履歴をバックアップ・復元・マージする CLI。

- Apple Silicon Mac: `aarch64-apple-darwin`
- x86_64 Linux: `x86_64-unknown-linux-musl`（静的リンク）
- 各アーカイブに Codex / Claude Code 用の SynCodex スキルを同梱

```sh
curl -fsSL https://github.com/mizuamedesu/SynCodex/releases/latest/download/install.sh | sh
```

本体を `~/.local/bin`、スキルを `~/.agents/skills/syncodex` と `~/.claude/skills/syncodex` にインストールします。インストーラーの案内に従って現在のシェルへ PATH を読み込み、Codex / Claude Code を再起動してください。Rust・sudo は不要です。

Claude Code 用スキルも操作対象は Codex の履歴です。Claude Code 自体の会話形式を変換する機能ではありません。
