nano が入っていない環境で `syncodex rewind` がエディター起動エラーになる問題を修正しました。

- インストーラーが nano の有無を確認し、未導入ならパッケージマネージャーで自動導入
- Linux は apt-get / apk / dnf / yum / pacman に対応。root またはパスワード入力なしの sudo を使用
- Mac は付属の nano を使用し、ない場合は既存の Homebrew で導入
- Vim / 内蔵エディターを使う場合は `--no-install-editor` で nano の導入を省略可能
- nano が見つからない場合のエラーに、インストーラーの再実行と内蔵エディターの起動方法を表示

```sh
curl -fsSL https://github.com/mizuamedesu/SynCodex/releases/latest/download/install.sh | sh
. "$HOME/.local/share/syncodex/env.sh"
syncodex rewind
```

標準エディターは nano、Vim は `syncodex rewind --vim` で選べます。nano では Ctrl+O → Enter で保存、Ctrl+X で終了・反映します。

Apple Silicon Mac と x86_64 Linux 向けのバイナリ、SHA-256、本体・Codex / Claude Code 用スキル・PATH 設定用インストーラーを配布します。Rust は不要です。
