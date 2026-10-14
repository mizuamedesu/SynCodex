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

履歴は追記され、一部は明示的な persist / flush までメモリ上に保持される。ディスクをコピーしても未保存分は取れない。dump / inject 中は、その Codex home を使う CLI・アプリを終了させる必要がある。dump / inject は Codex の writer lock を取得しない。rewind は後述のセッション別ロックを使い、他のセッションが起動中でも実行できる。

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

## 発言編集と巻き戻し

2026-10-14 追記。以下の処理は Codex 0.162.1 での実履歴による検証も行った。

`sessions` / `messages` / `rewind` は選択した Codex home の保存済み rollout を扱う。Codex app-server は起動しない。[公式 app-server 仕様](https://learn.chatgpt.com/docs/app-server) の `thread/revert` はターン境界の削除なので、発言単位の本文編集では使用中でない会話の rollout と表示用データを直接更新する。

- ユーザー／Assistant の `response_item` と対応する `item_completed`・旧形式の `user_message` / `agent_message` を同じ発言として扱い、両方の本文を更新する。環境コンテキストや system/developer 発言、ツール呼び出しは選択一覧から除外する。
- 保存位置は選択した発言のモデル用・表示用記録の末尾。その位置より後のレコードを切り捨てる。直前までの通常 rollout のバイト列は保持し、保持した途中ターンは `turn_aborted` で閉じる。これにより再開時の表示に実行中のターンが残らない。
- 選択位置以降の compaction を切り捨て、過去に削除した履歴を後続の要約から復活させない。既存の旧 `thread_rolled_back` によって非表示になったターンも選択対象から除く。
- multipart content は本文のテキスト部分を置き換え、画像等の部分は維持する。変更した入力本文の `text_elements` は古い範囲情報を取り除く。Assistant の `phase` と発言 ID は保持する。
- `history_base` は byte offset / ordinal を持つ参照なので、元ファイルの本文長が変わると分岐を壊す。編集対象と、そのファイルを参照する全子孫の継承部分を事前に読み、各会話のメタデータを先頭にした独立 rollout に展開して参照を除く。ordinal を連番に付け直し、subagent の表示開始位置も調整する。子孫の会話本文は編集前の状態を保つ。
- 対象の `history.jsonl` レコードを保持したユーザー発言から再構築し、他セッションの入力記録は維持する。会話名の索引は保持する。
- `state_5.sqlite` と `thread_history_1.sqlite` は対象行とそのスキーマだけをステージングし、変更の検証と復旧用 SQLite を作る。反映には現行 DB の `BEGIN IMMEDIATE` トランザクションを使用する。プレビュー・先頭入力・自動タイトル等を更新し、対象と参照を解消した子孫だけの turn / item / projection / realtime 行を削除する。DB 全体を置換せず、他のセッションの更新や backfill 状態、ユーザー指定の名前・プロジェクト・分類・ピン留めを維持する。
- Codex 0.162.1 と同じ `<CODEX_HOME>/thread-writer-locks/<THREAD_ID>.lock` と `.coordination.lock` を使う。ロックファイルの存在だけでは使用中と判定せず、`std::fs::File::try_lock` の取得可否を見る。実際に書き換える会話と参照を解消する子孫だけの writer lock を保持し、Codex による同じ会話の resume / write と排他する。他の会話の writer は停止させない。`--dry-run` と一覧は既存ロックだけを調べ、新しいロックファイルを作らない。
- 再検証は対象のファイル・参照解除する子孫の集合・継承した prefix に限定する。編集中の別セッションの追記・新規作成・会話名の変更で一律に失敗しない。読み取りだけに使う継承元は frozen prefix の byte range のみをハッシュで確認するため、継承元が後続の行を追記していても編集できる。
- `history.jsonl` は Codex と同じファイルロックを取得し、反映直前の最新内容から対象入力だけを置き換える。ファイルの inode を維持して書き換え、既存の追記用 descriptor を使う別セッションの入力を失わない。
- 全 JSONL・継承参照・SQLite の検証後、inject と共有する SynCodex ロック・Codex writer lock・変更前の退避・rollout の原子的置換・対象行だけの SQL 更新で反映する。複数ファイル全体の原子性は保証しない。退避用 SQLite は対象行のみを含むので、復旧は journal の `restore_scope=thread_rows` と `thread_ids` に従って行単位で行う。

## 復元の実機検証

Codex 0.154.0 と、この開発セッションの実ファイルで検証した。コピー先を指定した app-server が会話を一覧に出し、履歴本文を取得し、`thread/resume` を完了した。実行手順・照合内容・件数は [検証記録](verification.md) を参照。新しいモデルの生成ターンや GUI の操作はこの検証には含めない。
