use crate::policy::APP_LANG;

pub struct HelpText {
    pub about: &'static str,
    pub scan: &'static str,
    pub tui: &'static str,
    pub apply: &'static str,
    pub vault: &'static str,
    pub init: &'static str,
    pub scan_scope: &'static str,
    pub scan_out: &'static str,
    pub scan_rules: &'static str,
    pub scan_json: &'static str,
    pub scan_list_scopes: &'static str,
    pub scan_format: &'static str,
    pub tui_plan: &'static str,
    pub apply_plan: &'static str,
    pub apply_yes: &'static str,
    pub apply_dry_run: &'static str,
    pub apply_allow_outside: &'static str,
    pub apply_vault_dir: &'static str,
    pub vault_dir: &'static str,
    pub vault_list_expired: &'static str,
    pub vault_restore_target: &'static str,
    pub vault_purge_target: &'static str,
    pub vault_purge_expired: &'static str,
}

pub const EN: HelpText = HelpText {
    about: "Explore local storage and review developer cleanup candidates",
    scan: "Scan directories and generate a cleanup plan",
    tui: "Open the Text User Interface to review plans",
    apply: "Apply a plan (Quarantine/Trash) - Permanent deletion is still vault purge only",
    vault: "Manage the quarantine vault",
    init: "Initialize configuration (rules.toml)",
    scan_scope: "Scan target preset (safe-by-default). library-safe: safe subset of ~/Library (default). library-unsafe: all ~/Library.",
    scan_out: "Path to output the plan JSON",
    scan_rules: "Path to a custom rules.toml file",
    scan_json: "Print JSON to stdout",
    scan_list_scopes: "List available scopes and exit",
    scan_format: "Output format for list-scopes (text | json)",
    tui_plan: "Path to the plan JSON",
    apply_plan: "Path to the plan JSON",
    apply_yes: "Auto-confirm actions (skip y/n prompt)",
    apply_dry_run: "Dry run: simulate without changes",
    apply_allow_outside: "Allow applying actions to files outside the initial scan scope (DANGEROUS)",
    apply_vault_dir: "Custom vault directory (overrides default)",
    vault_dir: "Custom vault directory (overrides default)",
    vault_list_expired: "Show only expired items",
    vault_restore_target: "Path or ID to restore",
    vault_purge_target: "Vault item ID to purge (delete permanently). Optional if --expired is set.",
    vault_purge_expired: "Purge all expired items",
};

pub const JP: HelpText = HelpText {
    about: "ローカルストレージを調べ、開発時の掃除候補を確認するツール",
    scan: "ディレクトリをスキャンしてクリーンアップ計画（plan.json）を作成します",
    tui: "TUI（テキストUI）を開いて計画を確認・編集します",
    apply: "計画を適用します（隔離/ゴミ箱へ移動）。完全削除は vault purge でのみ行われます",
    vault: "隔離ボールド（保管庫）を管理します",
    init: "設定ファイル（rules.toml）を初期化します",
    scan_scope: "スキャン対象のプリセット。library-safe: ~/Libraryの安全なサブセット（デフォルト）。library-unsafe: ~/Library全体。",
    scan_out: "計画JSONの出力先パス",
    scan_rules: "カスタムrules.tomlのパス",
    scan_json: "JSONを標準出力に表示します",
    scan_list_scopes: "利用可能なスコープ一覧を表示して終了します",
    scan_format: "スコープ一覧の出力形式 (text | json)",
    tui_plan: "読み込む計画JSONのパス",
    apply_plan: "適用する計画JSONのパス",
    apply_yes: "確認プロンプトをスキップして自動承認します (y/n)",
    apply_dry_run: "ドライラン：変更を行わずにシミュレーションします",
    apply_allow_outside: "スキャンスコープ外のファイルへの適用を許可します（危険）",
    apply_vault_dir: "カスタムVaultディレクトリ（デフォルトを上書き）",
    vault_dir: "カスタムVaultディレクトリ（デフォルトを上書き）",
    vault_list_expired: "期限切れのアイテムのみを表示します",
    vault_restore_target: "復元するパスまたはID",
    vault_purge_target: "完全削除するアイテムID（--expired指定時は省略可）",
    vault_purge_expired: "期限切れのアイテムをすべて完全削除します",
};

pub const fn text() -> &'static HelpText {
    if const_str_eq(APP_LANG, "JP") {
        &JP
    } else {
        &EN
    }
}

const fn const_str_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let a = a.as_bytes();
    let b = b.as_bytes();
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}
