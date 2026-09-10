# SynCodex

Codex のローカル会話履歴をバックアップ・復元する Rust CLI。JSONL、圧縮済み rollout、入力履歴、会話名、会話用 SQLite を扱います。Codex 0.154.0 で、この開発セッションそのものを別の Codex home に復元し、一覧・本文取得・resume を確認しました。[検証記録](docs/verification.md)

## インストール

Apple Silicon Mac と x86_64 Linux 向けに、GitHub Releases でビルド済みバイナリを配布します。Rust・sudo は不要です。

```sh
curl -fsSL https://github.com/mizuamedesu/SynCodex/releases/latest/download/install.sh | sh
```

このコマンドで本体・Codex 用スキル・Claude Code 用スキル・今後のシェルの PATH 設定をインストールします。現在のシェルには、最後に表示される `. "$HOME/.local/share/syncodex/env.sh"` を実行するか、ターミナルを開き直すと PATH が反映されます。Codex / Claude Code も再起動するとスキルを検出します。

| 内容 | 既定の配置先 |
| --- | --- |
| バイナリ | `~/.local/bin/syncodex` |
| Codex スキル | `~/.agents/skills/syncodex/SKILL.md` |
| Claude Code スキル | `~/.claude/skills/syncodex/SKILL.md` |

スキルは Codex では `$syncodex`、Claude Code では `/syncodex` で呼び出せます。**両方とも操作対象は Codex の履歴**です。Claude Code の会話履歴を変換するものではありません。配置先は [Codex の公式スキル仕様](https://learn.chatgpt.com/docs/build-skills) と [Claude Code の公式スキル仕様](https://code.claude.com/docs/en/skills) に従っています。

インストーラーは OS / CPU を判定し、バージョン固定のアーカイブと SHA-256 をダウンロード・照合してから配置します。Linux は musl の静的リンク版、Mac は arm64 版です。Intel Mac、ARM Linux、Windows はこの配布の対象外です。

```sh
# バージョン固定、PATH の自動設定を省略
curl -fsSL https://github.com/mizuamedesu/SynCodex/releases/latest/download/install.sh | \
  sh -s -- --version v0.1.0 --no-modify-path

# スキルを省略・本体の配置先を変更
curl -fsSL https://github.com/mizuamedesu/SynCodex/releases/latest/download/install.sh | \
  sh -s -- --bin-dir "$HOME/bin" --no-skills

# ソースからビルドする場合
cargo install --path . --locked
```

`SYNCODEX_CODEX_SKILLS_DIR`、`SYNCODEX_CLAUDE_SKILLS_DIR` で各スキルの親ディレクトリを変更できます。Claude Code の `CLAUDE_CONFIG_DIR` も尊重します。再実行すると同じインストーラーが管理する本体・スキルを更新します。同名の自作スキルが既にある場合は停止して保持します。既存スキルの編集内容は、管理対象のスキルを更新すると置き換わります。

GitHub Actions は両プラットフォームで Rust のテスト、clippy、インストーラーテストを実行します。Cargo.toml と一致する `vX.Y.Z` タグを push すると、両方のビルド完了後にアーカイブ・チェックサム・インストーラーをまとめて公開します。[ワークフロー](.github/workflows/release.yml)

## バックアップと復元

```sh
# 通常は対象の Codex を終了して実行。新しい出力先を指定する
syncodex dump ./backups/laptop-2026-09-10
syncodex list ./backups/laptop-2026-09-10

# ディレクトリ全体を別端末に転送し、新しい Codex home へ復元
syncodex --codex-home ~/.codex-import inject ./backups/laptop-2026-09-10 --dry-run
syncodex --codex-home ~/.codex-import inject ./backups/laptop-2026-09-10

# 実際の Codex で読み込み・再開を検証。UUID は list の出力から選ぶ
syncodex --codex-home ~/.codex-import verify <THREAD_ID> --expect-text '会話中の文章'

# 対話を続ける場合は移行先で認証・設定を別途用意する
CODEX_HOME=~/.codex-import codex resume --all
```

`verify` は Codex app-server を起動し、`thread/read`、`thread/resume`、ターン・項目の全ページ取得を行います。履歴が指定した home 内にあることも検証します。`--expect-text` はユーザー・アシスタント発言の本文に対して照合し、何度でも指定できます。新しいモデルへの発言は送らず、認証情報のコピーも不要です。ローカルの表示用 DB は Codex によって更新されます。検証対象の Codex は終了しておいてください。

## 既存環境への同期

```sh
# 復元先の Codex を終了してから実行
syncodex inject ./backups/laptop-2026-09-10 --history-only --dry-run
syncodex inject ./backups/laptop-2026-09-10 --history-only
```

- 同じ rollout はスキップ。片方がもう片方の完全な行単位の前方一致なら長い履歴を採用し、古いバックアップで巻き戻しません。
- 同じ会話を両端末で別々に続けた場合は衝突として停止します。会話の時系列を勝手に混ぜません。
- `history.jsonl` はレコードの重複を除いて結合します。
- `session_index.jsonl` は ID ごとに `updated_at` が新しい会話名を採用。同時刻の異なる内容は停止します。
- `--history-only` は転送元の SQLite を注入せず、転送先の既存メタデータを維持します。履歴が変わった場合は既存 `state_5.sqlite` の backfill を予約し、次回 Codex 起動時に追加履歴を索引化します。
- 既存 SQLite に対する丸ごとの置換は拒否します。転送元の分類・プロジェクト等の DB 管理状態まで復元する場合は新しい保存先を使います。

書き換える既存ファイルは `<CODEX_HOME>/.syncodex/recovery-*/` に退避し、`journal.json` に元パスを記録します。SQL の退避にも backup API を使います。注入全体は複数ファイルをまたぐトランザクションではありません。途中の I/O エラーでは一部が反映される場合があります。退避データは自動削除しません。

## 保存先と内容

Codex home: `--codex-home` → `CODEX_HOME` → `$HOME/.codex`。

SQLite home: `--sqlite-home` → `CODEX_SQLITE_HOME` → 選択した Codex home。Codex の `config.toml` に `sqlite_home` を設定している場合、SynCodex にも `--sqlite-home` で同じ保存先を指定してください。

通常の `dump` は以下を保存します。`dump --history-only` で SQLite を省略できます。

| 対象 | 内容 |
| --- | --- |
| `sessions/**/*.jsonl[.zst]` | 会話本体 |
| `archived_sessions/**/*.jsonl[.zst]` | アーカイブ・分岐元 |
| `history.jsonl` | 入力履歴 |
| `session_index.jsonl` | 会話名 |
| `state_5.sqlite` | 会話メタデータ・分類・プロジェクト等 |
| `thread_history_1.sqlite` | 表示用履歴 |

SQLite は backup API で WAL 内の確定済み更新を含めて保存し、復元時には `threads.rollout_path` を新しい home に付け替えます。rollout の JSON 内容は変更しません。`manifest.json` にサイズ・SHA-256・更新時刻・元の home を記録します。v1 の履歴バックアップも読み込めます。

このバックアップは会話保存用です。認証、設定、作業リポジトリ、ファイルパスで参照された画像等の実体、goals/memories/queue の別機能の DB、ログは対象外です。作業ディレクトリや外部ファイルは別途用意してください。バックアップには会話中の秘密情報が含まれ得ます。暗号化は行わないため非公開で管理してください。Unix では新しく作る保存ディレクトリを 0700 にします。

保存形式の根拠と制約は [調査資料](docs/storage.md) にまとめています。

## テスト

```sh
cargo fmt --check
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings

# 任意: 実際の自分の履歴で検証。Python 3 と codex が必要
python3 scripts/live_e2e.py \
  --source ~/.codex \
  --output ./backups/my-e2e \
  --thread <THREAD_ID> \
  --expect-text '会話中の文章'
```

実データのスクリプトは新しいバックアップ・復元先を作成し、ファイルのハッシュ、DB パス、実際の Codex による再開を検証して `report.json` を保存します。元の Codex home に inject / resume は行いません。`backups/` は Git 管理対象外です。
