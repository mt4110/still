# still

[README_EN](./README_EN.md) 

**ローカルストレージの容量調査と開発生成物の掃除**

容量順のツリーを開いて調査し、生成物やマージ済みワークツリー、Docker 資産を確認できます。削除は選択した対象を明示的に確認してから実行します。

## 特徴
- **容量順の探索と再帰検索**: 指定したディレクトリを Rust で並列走査します。
- **Library の隔離**: 保管庫（Vault）へ移した項目を復元できます。
- **ローカル優先**: GitHub の PR 照合は `--github-pr` を指定した時に実行します。

## インストール

```bash
# Nixを使用する場合
nix develop
cargo build --release

# 通常のCargo
cargo install --path .
```

## 使い方：3ステップ・ワークフロー

### 開発ディレクトリの容量調査と掃除

```sh
# 容量を調べる
still review --root ~/_workspace

# 指定した開発ルートで生成物の掃除も有効にする
still review --cleanup-root ~/_workspace

# 独自スクリプト専用の出力先を登録する
still review --cleanup-root ~/_workspace --output ~/_workspace/my-project/generated

# 再帰検索を構造化出力で取得する
still review --root ~/_workspace --search .next --format json
```

`Enter` でディレクトリを開き、`←` で親に戻ります。`/` で現在地以下を検索し、`o` で別の調査先を開きます。`s` で合計容量順と最大ファイル順を切り替えます。

`Tab` に Cargo、Next.js、Vite の生成物候補が出ます。`Space` で選ぶと、Git の追跡状態と配下のファイルを確認します。`D` で実際に削除する対象を確認し、`y` で実行します。削除はゴミ箱を経由しません。`--output` は再生成できる専用出力先に指定してください。

選択後に中身が変わった出力、リンクや保護対象を含む出力は保留されます。エラーで `.still-stage-*` が残った場合、`original-path.txt` が元の場所、`payload` が残ったデータです。空き容量の増減はストレージ全体の観測値です。

`--docker-context colima` などを追加すると、指定したローカルの Docker 接続先にあるコンテナの bind mount と重なる出力を保護します。

### マージ済み Git ワークツリー

```sh
still worktrees --repo ~/projects/app --root ~/projects/app-worktrees
still worktrees --repo ~/projects/app --root ~/projects/app-worktrees \\
  --base refs/remotes/origin/main --github-pr 42
still worktrees --repo ~/projects/app --root ~/projects/app-worktrees \\
  --remove ~/projects/app-worktrees/task-done
```

`--base` の既定はローカルの `refs/heads/main` です。`--github-pr` は `gh` を使って `origin` の PR を照合します。削除時も、確認したときと同じ `--base` と `--github-pr` を指定してください。未コミット・未追跡・ignored ファイルのあるワークツリーは保留されます。生成物だけを掃除するときは `still review` を使えます。Codex が管理するワークツリーの全体削除は、そのチャットの管理画面から行います。

### Docker の掃除候補

```sh
still docker --context colima --include-cache
still docker --context colima --remove 'image:sha256:表示された完全なID'
```

停止中も含むコンテナから参照されていない、取得元の digest があるイメージを表示します。一時用途のコンテナは作成時の `still.disposable=true` ラベルで識別します。`--include-cache` はローカルの組み込みビルダーの古いキャッシュを表示します。削除は表示された個別 ID を対象にします。サイズは Docker 内の値です。

### Library の既存ワークフロー

安全にクリーンアップを行うための基本的な3ステップです。

### 1. Scan (スキャン)
ディスクをスキャンし、削除・隔離候補を洗い出して「計画ファイル (`plan.json`)」を作成します。この段階ではファイルは一切変更されません。

```bash
# デフォルト（安全なセット）でスキャン
still scan

# スコープ一覧を確認
still scan --list-scopes
```

### 2. Review (確認・選別)
作成された計画ファイルをTUIで開き、アイテムごとにアクションを決定します。

```bash
still tui
```

- **操作**:
    - `j` / `k`: 移動
    - `Space`: アクション切り替え (Keep -> Quarantine -> Trash)
    - `b`: バルクモード（まとめて変更）
    - `q`: 保存して終了

### 3. Apply (適用)
TUIで決定したアクション（隔離やゴミ箱への移動）を一括で実行します。

```bash
still apply
```

- **Quarantine (隔離)**: `~/Library/Application Support/still/vault` に移動されます。後で復元可能です。
- **Trash (ゴミ箱)**: macOSのシステムゴミ箱へ移動されます。
- **Keep**: 何もしません。

### 保管庫の管理 (Vault)
隔離（Quarantine）されたアイテムを管理します。

```bash
# 隔離アイテムの一覧
still vault list

# アイテムの復元
still vault restore <ID>

# 期限切れアイテムの完全削除（ディスクから消去）
still vault purge --expired
```

## ライセンス
Source Available License.
営利目的の再配布やForkの公開配布は禁止されています。改変は本リポジトリへのPRとしてのみ許可されます。
詳細は [LICENSE](./LICENSE) ファイルを参照してください。
