# SynCodex

Codex のローカル会話履歴をバックアップ・復元・編集する Rust CLI。セッション一覧からユーザー／Assistant の発言を選び、本文を変更してその位置まで巻き戻せます。JSONL、圧縮済み rollout、入力履歴、会話名、会話用 SQLite を扱います。復元は Codex 0.154.0、巻き戻しは Codex 0.162.1 の実履歴で本文取得・resume を確認しました。[検証記録](docs/verification.md)

## インストール

Apple Silicon Mac と x86_64 Linux 向けに、GitHub Releases でビルド済みバイナリを配布します。Rust は不要です。

```sh
curl -fsSL https://github.com/mizuamedesu/SynCodex/releases/latest/download/install.sh | sh
```

このコマンドで本体・Codex 用スキル・Claude Code 用スキル・今後のシェルの PATH 設定をインストールします。標準エディターの nano がない場合も自動導入します。Linux は apt-get / apk / dnf / yum / pacman を使い、root またはパスワード入力なしで利用できる sudo が必要です。Mac は通常付属の nano を使い、ない場合は既存の Homebrew で導入します。nano が既にある環境では sudo は不要です。

現在のシェルには、最後に表示される `. "$HOME/.local/share/syncodex/env.sh"` を実行するか、ターミナルを開き直すと PATH が反映されます。Codex / Claude Code も再起動するとスキルを検出します。

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
  sh -s -- --version v0.2.1 --no-modify-path

# スキルを省略・本体の配置先を変更
curl -fsSL https://github.com/mizuamedesu/SynCodex/releases/latest/download/install.sh | \
  sh -s -- --bin-dir "$HOME/bin" --no-skills

# ソースからビルドする場合
cargo install --path . --locked
```

Vim や内蔵エディターを使う場合は、インストーラーに `--no-install-editor` を指定すると nano の導入を省略できます。起動時は `syncodex rewind --vim` または `syncodex rewind --editor builtin` を指定してください。

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

## 発言の編集と巻き戻し (rewind)

Codex CLI・アプリを起動したまま、**使用されていないセッションを編集できます**。一覧の `[使用中]` は Codex が読み込み・書き込み用に保持している会話です。その会話と、参照を解消する必要がある分岐が使用中の場合だけ編集を拒否します。対象の会話を終了・アンロードすれば編集でき、他のセッションを閉じる必要はありません。ソースから使う場合は `cargo run -- rewind` でも起動できます。

```sh
# 会話選択 → 発言選択 → nano で編集・保存
syncodex rewind

# Vim を使う場合
syncodex rewind --vim
# 同じ指定: syncodex rewind --editor vim

# 会話名・ID・作業ディレクトリで絞り込み。アーカイブも選択対象に含める
syncodex rewind --search SynCodex --archived
```

画面上に操作キーと現在の手順を表示します。通常の会話を最初に表示し、承認レビューなど Codex が自動生成した内部会話は F2、アーカイブは F3 で表示を切り替えます。会話一覧には会話名・作業フォルダー・更新日時・使用状態を表示し、詳細側には最初の依頼を表示します。発言一覧では User / Assistant を色で区別し、選択した発言の全文を折り返して表示します。狭い端末では一覧とプレビューを上下に並べます。

| 操作 | キー |
| --- | --- |
| 一覧を検索 | 文字を入力、Ctrl+U で検索を消す |
| 会話・発言を選ぶ | ↑↓ → Enter |
| 発言の全文をスクロール | PgUp / PgDn、またはプレビュー上のホイール |
| 本文を編集 | 標準の nano でそのまま入力 |
| nano で保存 | Ctrl+O → Enter |
| nano を終了して変更を反映 | Ctrl+X |
| 保存せず取り消す | Ctrl+X → N |
| 一覧で前の画面へ戻る | Esc |

nano で本文を保存して終了すると、選んだ発言を編集後の内容で残し、**それより後の発言・ツールの履歴・圧縮された要約を削除**します。同じ本文を保存した場合も後続の巻き戻しは実行します。標準エディターは常に nano です。`--vim` / `--editor vim` で Vim、`--editor 'code --wait'` 等で別のエディターを選べます。Vim は `i` で編集、Esc → `:wq` で保存・終了、Esc → `:q!` で保存せず終了します。

内蔵エディターを使う場合は `--editor builtin` を指定します。Ctrl+A で全選択、Ctrl+Z / Ctrl+Y で元に戻す／やり直す、Ctrl+S で変更前後と削除件数の比較、Enter で反映します。Esc で前の画面に戻り、編集中の内容があれば破棄を確認します。

番号と本文を指定した操作や、反映前の検証もできます。

```sh
syncodex sessions --search SynCodex
syncodex sessions --archived --json
syncodex messages <THREAD_ID>

# messages の 1 始まりの番号を指定。ユーザー／Assistant のどちらも編集可能
syncodex rewind <THREAD_ID> --message 3 --text '編集後の本文' --dry-run
syncodex rewind <THREAD_ID> --message 3 --text '編集後の本文'

# 改行を含む UTF-8 ファイルをそのまま使用
syncodex rewind <THREAD_ID> --message 3 --text-file ./edited-message.md

# 変更後の履歴を再開
codex resume <THREAD_ID>
```

`--dry-run` は書き換え対象をすべて準備・検証し、残す発言数・削除する発言数・保全する分岐数を表示します。端末では日本語で結果とバックアップ先・会話の再開コマンドを表示し、`--json` 指定時や出力をパイプした場合は JSON で出力します。通常の実行も同じ検証を行い、変更前の rollout・入力履歴・対象行の SQLite を `<CODEX_HOME>/.syncodex/recovery-*/` に退避してから反映します。保存時は Codex と同じセッション別 writer lock を取得するので、編集中に対象セッションが開かれた場合も反映を拒否します。Codex 0.162.1 で同時利用を検証しました。

モデルが読む本文と表示用の発言記録を一緒に更新し、対象の表示用 SQLite の索引をリセットします。次に対象の会話を読み込むと、新しい履歴を再投影します。SQLite は対象の行だけをトランザクションで更新し、他セッションの新しい変更や共通の backfill 状態を維持します。入力履歴は Codex と共有するファイルロックの中で最新内容を取得し、対象の入力だけを作り直します。同じファイルを更新するため、別セッションがすでに開いている追記用のファイルにも変更が反映されます。プロジェクト・ピン留め・ユーザー指定の会話名、添付のテキスト以外の部分と Assistant の `phase` も維持します。

分岐元の履歴を参照する会話は、変更前の内容を各分岐の rollout に取り込んで参照を解消します。そのため、元セッションの編集が別の分岐に伝播しません。分岐内の継承した発言を編集した場合も、その分岐だけを巻き戻します。参照元が欠けている、参照位置が不正、同じ rollout の圧縮版／通常版が重複する、DB から現在の物理 rollout を特定できない場合は書き込まずに停止します。

対象は会話履歴で、過去のツール操作による作業ファイルの変更は残ります。自動でモデルに発言を送ることはありません。処理全体は複数ファイルをまたぐトランザクションではないため、途中の I/O エラーでは一部が反映される場合があります。元に戻す場合は退避先の `journal.json` に記録された `restore_scope` に従って、rollout は元パスへ、SQLite は `thread_ids` の行だけ、入力履歴は `session_ids` の入力だけを戻してください。rewind の SQLite 退避には対象行だけが入っているため、DB 全体への restore API は使用できません。

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
python3 tests/rewind_interactive.py

# 任意: 実際の自分の履歴で検証。Python 3 と codex が必要
python3 scripts/live_e2e.py \
  --source ~/.codex \
  --output ./backups/my-e2e \
  --thread <THREAD_ID> \
  --expect-text '会話中の文章'

# 任意: 実 Codex の別会話を開き、共有 DB を更新しながら未使用会話を編集・再開
python3 scripts/live_rewind_e2e.py \
  --source-rollout /path/to/self-contained-rollout.jsonl \
  --output ./backups/concurrent-rewind-e2e
```

実データのスクリプトは新しいバックアップ・復元先を作成し、ファイルのハッシュ、DB パス、実際の Codex による再開を検証して `report.json` を保存します。元の Codex home に inject / resume は行いません。`backups/` は Git 管理対象外です。
