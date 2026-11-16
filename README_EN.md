# still

**Local storage explorer and review-first developer cleanup tool**

Browse directory sizes, search local paths, and review generated outputs, merged Git worktrees, and unused Docker resources. Cleanup actions require explicit confirmation.

## Features
- **Parallel storage scan**: Browse by total size or largest file and search recursively.
- **Library quarantine**: Move selected Library items to a restorable Vault.
- **Local by default**: GitHub PR lookup runs when explicitly requested with `--github-pr`.

## Installation

```bash
# Using Nix
nix develop
cargo build --release

# Using Cargo
cargo install --path .
```

## Usage: 3-Step Workflow

### Developer storage explorer

```sh
still review --root ~/projects
still review --cleanup-root ~/projects
still review --root ~/projects --search target --format json
```

Press Enter to open a directory, `/` to search, or `o` to choose another root. With a cleanup root configured, Tab shows generated outputs. Space checks and selects an output. `D` opens the deletion preview, and `y` confirms permanent removal. Use `--output` to declare a dedicated reproducible script output directory.

`still worktrees --help` and `still docker --help` show the options for the other cleanup sources.

### Library workflow

The standard workflow for safe cleanup.

### 1. Scan
Scans directories to identify cleanup candidates and generates a "Plan File (`plan.json`)". No files are modified at this stage.

```bash
# Scan with default safe settings
still scan

# List available scopes
still scan --list-scopes
```

### 2. Review (TUI)
Open the generated plan in the TUI to decide actions for each item.

```bash
still tui
```

- **Controls**:
    - `j` / `k`: Navigate
    - `Space`: Cycle Action (Keep -> Quarantine -> Trash)
    - `b`: Bulk Mode (Apply to all filtered items)
    - `q`: Save and Quit

### 3. Apply
Executes the actions (Move to Vault / Trash) selected in the TUI.

```bash
still apply
```

- **Quarantine**: Moves files to `~/Library/Application Support/still/vault`. Can be restored later.
- **Trash**: Moves files to the macOS system Trash.
- **Keep**: Does nothing.

### Vault Management
Manage items in the quarantine vault.

```bash
# List quarantined items
still vault list

# Restore an item
still vault restore <ID>

# Purge expired items (Permanent Deletion)
still vault purge --expired
```

## License
Source Available License.
Commercial redistribution and public distribution of forks are prohibited. Modifications are permitted only as PRs to this repository.
See [LICENSE](./LICENSE) for details.
