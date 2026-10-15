`LC_ALL=C` などの環境で、日本語を入力すると `^Z` などの制御文字として扱われる問題に対応しました。

- 外部エディターの文字コードを確認し、非 UTF-8 または無効なロケールでは利用可能な UTF-8 ロケールを自動選択
- 子プロセスにだけ設定を渡し、既存の UTF-8 設定と現在のシェルの環境を維持
- `locale` コマンドがない最小構成の環境にも対応
- エディターがエラー終了した場合は、一時ファイルに編集があっても履歴の巻き戻しを拒否
- nano の自動導入、標準 nano / 任意の Vim、セッション別の使用状態判定を継続

```sh
curl -fsSL https://github.com/mizuamedesu/SynCodex/releases/latest/download/install.sh | sh
. "$HOME/.local/share/syncodex/env.sh"
syncodex rewind
```

標準エディターは nano、Vim は `syncodex rewind --vim` で選べます。nano では Ctrl+O → Enter で保存、Ctrl+X で終了・反映します。

Apple Silicon Mac と x86_64 Linux 向けのバイナリ、SHA-256、本体・Codex / Claude Code 用スキル・PATH 設定用インストーラーを配布します。Rust は不要です。
