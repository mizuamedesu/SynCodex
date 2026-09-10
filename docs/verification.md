# この開発セッションの復元検証

2026-09-10、`codex-cli 0.154.0`、Linux x86_64 で実施。

対象は SynCodex を開発している実際の会話 `01a0891b-fa94-7910-b149-0641a07c3a6c`。元の保存先は `/home/coder/.codex`、履歴形式は paginated。合成の会話データではなく、ユーザーの最初の依頼と、このアシスタントの返答を含むディスク上の履歴を使用した。

```sh
python3 scripts/live_e2e.py \
  --output backups/e2e-20260910 \
  --thread 01a0891b-fa94-7910-b149-0641a07c3a6c \
  --expect-text 'ここにcodexの履歴' \
  --expect-text '上流コードを調査し'
```

実行結果:

| 検証 | 結果 |
| --- | --- |
| dump | 5 ファイル: rollout、入力履歴、会話名、state DB、表示用 DB |
| dry-run | 復元先を作らずに検証完了 |
| inject | 新しい home に 5 ファイル復元 |
| 復元直後の履歴ファイル | 3 ファイルとも manifest の SHA-256 と一致 |
| state DB | quick_check 正常、全 rollout パスが復元先配下 |
| 認証ファイル | コピーなし |
| Codex `thread/read` | 成功、取得元パスが復元先 home 内 |
| Codex `thread/resume` | 元の thread ID で成功 |
| Codex `thread/list` | 復元した会話が一覧に存在 |
| Codex `thread/turns/list` | 2 ターン取得 |
| Codex `thread/items/list` | 100 項目取得、うち発言 11 項目 |
| 本文の照合 | 元のユーザー依頼・アシスタント返答の 2 箇所が一致 |
| 新しいモデル生成ターン | 送信なし |

ローカルの成果物:

- `backups/e2e-20260910/snapshot/`: バックアップ
- `backups/e2e-20260910/restored/`: 実際に Codex が再開した復元先
- `backups/e2e-20260910/report.json`: 機械可読な検証結果

これらは会話本文を含み得るため Git の管理対象外。上記ディレクトリは実施環境に残してある。別の環境ではスクリプトに新しい `--output` を指定して同じ手順を実行できる。

今回の会話は実行中のため、検証コマンドが走っている間の保存済み履歴を取得した。JSONL のコピー前後・SQLite 取得後に内容が変わっていないことを検査している。ただし未 flush の内容や、ここに記載した検証後の発言はこのスナップショットには含まれない。通常運用では Codex を終了してからバックアップ・注入する。

`resume` は実際の Codex エンジンによる履歴読み込み・再開初期化までを意味する。新しい質問へのモデル応答、GUI 表示、別 OS での作業ディレクトリ移行は検証していない。
