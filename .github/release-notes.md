セッション一覧から User / Assistant の発言を編集し、その位置まで履歴を巻き戻せるようになりました。

- 会話・発言の検索、全文プレビュー、操作キーを表示する端末画面
- Codex を起動したまま未使用のセッションを編集。使用中の対象・必要な分岐だけ拒否
- 標準エディターは nano。`--vim` / `--editor vim` で Vim を選択
- 変更前の履歴を自動退避し、他セッションの更新と分岐の内容を保全
- 承認レビュー等の内部会話は通常一覧から非表示。F2 で切り替え

バックアップ・復元・マージ機能も引き続き使用できます。

- Apple Silicon Mac: `aarch64-apple-darwin`
- x86_64 Linux: `x86_64-unknown-linux-musl`（静的リンク）
- 各アーカイブに Codex / Claude Code 用の SynCodex スキルを同梱

```sh
curl -fsSL https://github.com/mizuamedesu/SynCodex/releases/latest/download/install.sh | sh

# 会話と発言を選んで nano で編集
syncodex rewind

# Vim を使う場合
syncodex rewind --vim
```

本体を `~/.local/bin`、スキルを `~/.agents/skills/syncodex` と `~/.claude/skills/syncodex` にインストールします。インストーラーの案内に従って現在のシェルへ PATH を読み込み、Codex / Claude Code を再起動してください。Rust・sudo は不要です。

nano は Ctrl+O → Enter で保存、Ctrl+X で終了・反映します。nano がない環境では用意するか、`--editor builtin` で内蔵エディターを使用できます。

Claude Code 用スキルも操作対象は Codex の履歴です。Claude Code 自体の会話形式を変換する機能ではありません。
