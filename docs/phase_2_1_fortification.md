# Phase 2.1 Fortification

Goal: "Hard Safety" — hardening the system against operational accidents.

## Hard Safety Policies

1.  **Purge Separation**: `still apply` NEVER performs permanent deletion (Purge).
2.  **Validator Barrier**: No IO operations unless the plan passes validation.
3.  **No Symlink Traversal**: Symlinks are never followed; they are skipped or rejected.
4.  **No Network / No Telemetry**: Strict local-only operation.
5.  **Default Deny (Scope)**: Apply is rejected if paths are outside permitted scopes.

## Implementation Details

### Validator Extended
- **Scope Containment**: Enforces that all plan entries reside within `allowed_scopes` using lexical path normalization.
    - Bypass: `--allow-outside-scope`
- **Schema Enforcement**: Strict `schema_version` check.
- **Symlink/Special File Rejection**: Just-in-time `lstat` checks before action execution.

### Vault TTL
- Items in Vault have an expiry (e.g., via `still_meta.json`).
- `still vault purge --expired` cleans up old items (Ritual "PURGE STILL" required).

### Robust Copy
- File Sync: `dest.sync_all()` after copy.
- Directory Sync: Best-effort sync.

## Verification
- Unit tests for validator scope and schema checks.
- Integration tests for vault expiry and ritual.
- Validated via `cargo test`.
