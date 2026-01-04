# still

**Non-destructive, safety-first disk cleaner for macOS**

Designed as an "Observation + Quarantine Device" rather than a simple cleaner. It never deletes files without your explicit permission.
By default, it operates in Dry-run mode (creating plans), executing only the actions you have confirmed.

## Features
- **Non-destructive Defaults**: Always starts with a Dry-run plan.
- **Quarantine First**: Moves files to a dedicated Vault instead of deleting them immediately.
- **TUI Interface**: Visually review and select candidates for deletion.
- **Local Only**: No network communication.

## Installation

```bash
# Using Nix
nix develop
cargo build --release

# Using Cargo
cargo install --path .
```

## Usage: 3-Step Workflow

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
