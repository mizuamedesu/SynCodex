# Codex の会話保存形式とバックアップ設計

調査日: 2026-09-10。`openai/codex` を clone し、commit [`a62e98d18c6550e3bea152ed1b89d1e931dca961`](https://github.com/openai/codex/tree/a62e98d18c6550e3bea152ed1b89d1e931dca961) の実装を確認した。以下はそのコミットの仕様であり、インストール済みの Codex や非公開のデスクトップアプリ実装と一致するとは限らない。

## 保存場所と役割

標準のデータディレクトリは `~/.codex`。`CODEX_HOME` で変更できる。[公式設定ドキュメント](https://learn.chatgpt.com/docs/config-file/config-advanced) と [設定実装](https://github.com/openai/codex/blob/a62e98d18c6550e3bea152ed1b89d1e931dca961/codex-rs/core/src/config/mod.rs) を参照。SQLite は `sqlite_home` / `CODEX_SQLITE_HOME` により別ディレクトリに置かれる場合もある。

| 対象 | 内容 | 今回の dump |
| --- | --- | --- |
| `sessions/YYYY/MM/DD/*.jsonl` | 会話ごとの rollout。メッセージ、ツール呼び出し・結果、コンテキスト、圧縮履歴など | 含む |
| `sessions/**/*.jsonl.zst` | Zstandard 圧縮済み rollout | 含む |
| `archived_sessions/**/*.jsonl[.zst]` | アーカイブ済みの会話。分岐元になっている場合もある | 含む |
| `history.jsonl` | 入力履歴。`session_id`, `ts`, `text` のレコード | 存在すれば含む |
| `session_index.jsonl` | 会話名の追記式索引。`id`, `thread_name`, `updated_at` | 存在すれば含む |
| `state_5.sqlite` | 会話一覧のメタデータ、rollout パス、DB にしかない管理情報 | backup API で含む |
| `thread_history_1.sqlite` | rollout から作る表示用履歴と処理位置 | backup API で含む |
| goals / memories / queue 系 SQLite | 目標、メモリ、キュー等の別機能の状態 | 対象外 |
| `auth.json`, `config.toml` | 認証・環境設定 | 対象外 |
| ログ、キャッシュ、外部ファイル、作業リポジトリ | 診断情報、生成物、添付の実体、作業内容など | 対象外 |

根拠: [rollout の保存と読み込み](https://github.com/openai/codex/blob/a62e98d18c6550e3bea152ed1b89d1e931dca961/codex-rs/rollout/src/recorder.rs)、[圧縮ファイルの処理](https://github.com/openai/codex/blob/a62e98d18c6550e3bea152ed1b89d1e931dca961/codex-rs/rollout/src/compression.rs)、[入力履歴](https://github.com/openai/codex/blob/a62e98d18c6550e3bea152ed1b89d1e931dca961/codex-rs/message-history/src/lib.rs)、[名前索引](https://github.com/openai/codex/blob/a62e98d18c6550e3bea152ed1b89d1e931dca961/codex-rs/rollout/src/session_index.rs)、[DB 定義](https://github.com/openai/codex/blob/a62e98d18c6550e3bea152ed1b89d1e931dca961/codex-rs/state/src/sqlite.rs)。

## 会話本体の構造

rollout は通常 `rollout-日時-UUID.jsonl` という名前で保存される。現在の実装には thread ID と物理 rollout ID を区別したファイル名もあり、UUID だけを見て重複排除すると履歴を失う可能性がある。

各行は `timestamp`、任意の `ordinal` と、`type` / `payload` を中心とするレコード。先頭の `session_meta` は ID、作業ディレクトリ、CLI バージョン、provider、分岐情報などを持つ。後続は `response_item`、`event_msg`、`turn_context`、`compacted` 等で、現在の形式には他の種類もある。ユーザーとアシスタントのテキストだけを抜き出しても、元の再開用履歴には戻せない。[履歴型](https://github.com/openai/codex/blob/a62e98d18c6550e3bea152ed1b89d1e931dca961/codex-rs/history/src/lib.rs)、[wire 形式](https://github.com/openai/codex/blob/a62e98d18c6550e3bea152ed1b89d1e931dca961/codex-rs/history/src/rollout_payload.rs)。

`SessionMeta.history_base` には別 rollout の履歴の一部を参照する仕組みがある。個別会話だけを取り出すには依存関係の解決が必要なので、初期版は両方の履歴ディレクトリをまとめて保存する。分岐元が元から欠けている場合の修復までは行わない。[SessionMeta](https://github.com/openai/codex/blob/a62e98d18c6550e3bea152ed1b89d1e931dca961/codex-rs/protocol/src/protocol.rs)、[rollout 参照索引](https://github.com/openai/codex/blob/a62e98d18c6550e3bea152ed1b89d1e931dca961/codex-rs/rollout/src/rollout_reference_index.rs)。

履歴は追記され、一部は明示的な persist / flush までメモリ上に保持される。ディスクをコピーしても未保存分は取れない。dump / inject 中は、その Codex home を使う CLI・アプリを終了させる必要がある。SynCodex はプロセスの自動停止や Codex の writer lock 取得を行わない。

## SQLite の移行方法

`state_5.sqlite` は単なる捨ててよいキャッシュとは扱えない。rollout から会話メタデータを再構築する backfill はあるが、DB 側が所有する属性を保持する処理があり、プロジェクト・会話の分類や添付メタデータなども DB に存在する。一方、保存された rollout パスは移行先と異なる可能性がある。[メタデータの調停](https://github.com/openai/codex/blob/a62e98d18c6550e3bea152ed1b89d1e931dca961/codex-rs/state/src/model/thread_metadata.rs)、[添付管理](https://github.com/openai/codex/blob/a62e98d18c6550e3bea152ed1b89d1e931dca961/codex-rs/thread-store/src/local/thread_attachments.rs)。

backfill は完了状態なら再走査をスキップする。そのため、既存環境に JSONL をコピーして再起動するだけで必ず一覧が更新される、とは言えない。SynCodex は新しい保存先には DB を復元し、`threads.rollout_path` を元の home から復元先へ付け替える。既存保存先では `--history-only` により転送元 DB を省略し、転送先のメタデータを維持しながら backfill を pending に戻す。変更前の DB は backup API で退避する。別の Codex home では初期 backfill の対象になる設計だが、Codex のバージョン、provider、cwd、会話の source、アーカイブ状態による表示条件も考慮が必要。[backfill](https://github.com/openai/codex/blob/a62e98d18c6550e3bea152ed1b89d1e931dca961/codex-rs/rollout/src/metadata.rs)、[一覧と検索](https://github.com/openai/codex/blob/a62e98d18c6550e3bea152ed1b89d1e931dca961/codex-rs/rollout/src/list.rs)。

`thread_history_1.sqlite` は durable JSONL の byte offset / ordinal に沿って表示用データを投影する。通常の dump にはこの DB も含める。[履歴の projection](https://github.com/openai/codex/blob/a62e98d18c6550e3bea152ed1b89d1e931dca961/codex-rs/thread-store/src/local/thread_history.rs)。

端末状態の完全バックアップが必要なら、履歴に加えて実際の SQLite 保存先、必要な設定・メモリ・添付の実体・作業リポジトリ等を別途保全する。稼働中の SQLite 本体だけをコピーすると WAL 内の更新が欠け得るので、SQLite backup API による整合スナップショット、または全 writer を終了した上で関連ファイルを含むコールドバックアップが必要。認証情報の保管は履歴とは別に管理する。SynCodex は会話用の 2 DB に backup API を適用する。goals/memories/queue、外部ファイル、認証・設定まで含む端末全体のバックアップは対象外。

## 形式と保証範囲

```text
snapshot/
  manifest.json       # format_version=2、元の home、相対パス、サイズ、SHA-256、更新時刻
  files/
    sessions/...
    archived_sessions/...
    history.jsonl
    session_index.jsonl
    sqlite/
      state_5.sqlite
      thread_history_1.sqlite
```

- dump は元ファイルとコピーのハッシュを比較し、コピー後にも全体の一覧と内容を再確認する。稼働中の整合スナップショットを保証するものではない。
- inject は全件の検証後に書き込む。同一内容はスキップ。rollout は完全な行単位の前方一致でのみ延長を許し、分岐した内容は拒否する。入力履歴はレコードの和集合、会話名は ID ごとの更新日時で調停する。同じ時刻の異なる会話名は衝突とする。
- 既存ファイルは `.syncodex/recovery-*/` に退避する。全体はトランザクションではなく、ディスク不足や途中終了では一部のファイルが残り得る。退避の `journal.json` に元パスと DB かどうかを記録する。SQLite は停止中に backup/restore API で戻す必要がある。
- SHA-256 は転送時の破損検出用。署名・暗号化・Codex レコード全体の意味検証は行わない。信頼できるバックアップを使用し、処理中に入力元・出力先を他のプロセスが変更しないこと。SynCodex 同士は注入ロックを使うが、Codex の writer lock と共通ではない。
- JSONL 内の cwd、絶対パス、workspace roots 等は書き換えない。別 OS / 別ディレクトリの作業環境は別途用意する。履歴に記録されたコード変更も自動適用しない。SQLite の rollout パスの変更と、作業環境の移行は異なる。
- 更新時刻は v2 manifest に保存するため、転送時にバックアップファイルの時刻を失っても復元できる。
- 同じ履歴の `.jsonl` と `.jsonl.zst` が転送先で競合する場合は停止する。アーカイブ状態の双方向調停、削除の同期、任意の DB スキーマ間のマイグレーション、外部ファイルの取り込み、リモートストレージへの転送は行わない。
- `--history-only` は元 DB のプロジェクトや分類を既存環境へマージしない。これらを含む同一状態の移行は新しい home への通常 inject を使う。

## 実機検証

Codex 0.154.0 と、この開発セッションの実ファイルで検証した。コピー先を指定した app-server が会話を一覧に出し、履歴本文を取得し、`thread/resume` を完了した。実行手順・照合内容・件数は [検証記録](verification.md) を参照。新しいモデルの生成ターンや GUI の操作はこの検証には含めない。
