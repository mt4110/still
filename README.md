# still

[README_EN](./README_EN.md) 

**macOS用の非破壊的・安全第一のディスククリーナー**

「掃除機」ではなく「観測＋隔離装置」としての立ち位置を取り、決して勝手にファイルを削除しません。
初期状態では Dry-run（計画作成）のみを行い、ユーザーが確認した操作のみを実行します。

## 特徴
- **非破壊的デフォルト**: 基本は常に Dry-run（計画作成）です。
- **Quarantine First**: いきなり削除せず、まずは専用の保管庫（Vault）へ隔離します。
- **TUI搭載**: 削除候補を視覚的に確認・選別できます。
- **完全ローカル**: ネットワーク通信を一切行いません。

## インストール

```bash
# Nixを使用する場合
nix develop
cargo build --release

# 通常のCargo
cargo install --path .
```

## 使い方：3ステップ・ワークフロー

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
