# Changelog

All notable changes to the `still` project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Compare one explicitly selected app's verified signed App Group declaration against an exact ID without enumerating applications or treating the result as ownership or current-use proof.
- Optionally inventory immediate folders in five macOS Library locations for read-only review. Ownership and data purpose remain unverified; results may overlap known locations.
- Show project `node_modules` as protected, review-only storage, and exclude nested package outputs from automatic cleanup candidates.
- Record desktop-generated-output cleanup results locally and expose them through `still cleanup-history`.
- Add read-only home, system-volume and selected-folder scans, with size-ordered folder navigation in the Mac app.
- Add a Mac Docker screen action to review and prune unused default-builder cache after a user-selected retention period.
- Offer an explicit, non-overwriting restore of remaining staged data to its recorded sibling path on macOS. A restored payload may already be incomplete.
- Allow the desktop old-file search to include recognized project build outputs when requested.
- Keep compact per-scan workspace totals and provide a timeline in the Mac app and `still review-history`.
- Show comparable Library and Docker changes in the Mac overview and link incomplete cleanup results to stage review.
- Show filename-based Library clues and allow a user to record, update, or revoke their own scoped uninstall confirmation; neither authorizes deletion.
- Compare an explicitly selected signed app's App Group declaration against one specified group ID without enumerating apps or exposing its other entitlements.

### Changed

- Start newly scanned CLI plans with every entry kept; show recommendations separately in the review TUI. Existing saved plans are unchanged.
- Record available scan-time allocated size, start-of-cleanup keep settings, and the original-path check time with cleanup history.
- Record SDK, Docker container, and builder-cache outcomes in cleanup history, including partial or unverified SDK results.
- Confirm the number and size of candidates before selecting all eligible project outputs.
- Show project-by-project counts and sizes before bulk selection, and offer a direct view of independently reviewable Cargo subfolders.
- Include all unused builder cache in the age-filtered Docker cache cleanup, with the expanded scope shown before confirmation.
- Avoid waiting for per-file open-process checks in the desktop old-file search; the results explicitly state that use was not checked.
- Compare regular-file contents as well as metadata during output review on Unix. This reads selected files and can make review slower; it does not guarantee detection of every concurrent change.
- Include bounded Docker CLI error details when an inspection fails.
- Match Buildx cache records against Engine metadata when Buildx returns human-readable dates, and verify filter behavior before attempting an exact cache removal.

### Fixed

- Avoid rejecting a staged file solely because its metadata change time changed after staging; verify its identity and contents before removal.
- Keep stage review controls visible for long paths and allow carefully verified Cargo cache subfolders to be reviewed when target markers were lost.
- Display recognized Docker sizes in binary units and translate known relative dates on the Japanese screen.
- Inspect stopped containers with bind mounts or null labels without losing Docker metadata, while continuing to protect managed and explicitly retained containers.

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
