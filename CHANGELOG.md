# Changelog

All notable changes to the `still` project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0-beta.1] - 2026-11-16

### Added

- `still review` explores explicitly selected roots with parallel scanning, recursive search, and sorting by total size or largest file.
- The explorer proposes Cargo, Next.js, Vite, and explicitly declared script outputs. Selected outputs require a deletion preview and confirmation. The tool rechecks Git, filesystem identity, and output contents before removal.
- `still worktrees` reviews linked Git worktrees against a chosen integration ref and can remove an eligible worktree after confirmation.
- `still docker` previews unused images, labelled disposable stopped containers, and stale BuildKit cache on a selected local context.

### Verification

- The storage explorer and output removal were tested on Apple Silicon macOS. Linked worktree removal was tested with a local fixture repository.
- Docker is a preview tested through simulated Engine and Buildx responses. Windows x64 passed a compile check and real-device testing is pending.

## [0.1.0] - "Still Water" - 2026-01-03

### Added
- **Core Scanning**: `still scan` command to identify large/old files based on `rules.toml`.
- **Review TUI**: Interactive terminal interface (`still tui`) to review, filter, and sort scan results.
- **Safety**: 
  - Non-destructive default actions (Quarantine).
  - "Ritual Phrase" confirmation required for permanent `purge`.
  - JIT (Just-In-Time) protection against Symlinks and scope violations.
- **Vault**:
  - `still vault list`: View quarantined items.
  - `still vault restore`: Restore items to original location.
  - `still vault purge --expired`: Clean up old quarantined items based on TTL policy.
- **Policy Control**: Centralized `src/policy.rs` for configurable constants (TTL, keybinds).
- **Journaling**: JSON-based audit log for all major actions (Apply, Purge).

### Security
- **Purge Ritual**: Requires typing "PURGE STILL" to authorize permanent deletion.
- **Symlink Protection**: Symlinks are strictly rejected during execution to prevent symlink attacks.
- **Scope Validation**: Operations restricted to safe directories (e.g., `~/Library`), blocking `/` or `~`.

### Fixed
- Addressed various race conditions in scanning logic.
- Hardened timestamp comparisons for Vault expiry.
