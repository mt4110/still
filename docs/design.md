# Design

## Architecture

1. **Scanner**: Uses `ignore` crate to walk directories efficiently but respects boundaries. Produces a list of `Entry`.
2. **Plan**: JSON representation of potential operations. Intermediate state allows manual review.
3. **TUI**: Ratatui-based interface to modify the Plan (e.g., change action for specific files).
4. **Executor**: Reads the Plan and performs actions (Quarantine, Trash). Use strictly local FS operations. **Purge (permanent deletion) is NEVER performed by the apply command.** Purging is exclusively a manual maintenance action on the Vault.

## Data Flow
`FS -> Scanner -> Plan(JSON) -> TUI -> Modified Plan(JSON) -> Executor -> Vault + Journal`

## Key Decisions
- **Rust**: For safety and performance.
- **Quarantine Vault**: To allow "undo" without relying on OS Trash (which might be emptied).
- **Two-Phase Commit**: Scan/Plan is separate from Apply to prevent "run and regret".
