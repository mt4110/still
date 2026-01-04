# Release Checklist (Manual Verification)

> **Role**: The Human Validator (Standard Output) acts as the final safety gate.
> **Goal**: Confirm that the "v0.1.0" binary behaves exactly as documented and enforces all safety invariants.

## Pre-Release Roll Call (Safety & Truth)

> **Rule:** If any item in this section fails, do not tag a release.

### 0) Source of Truth: Scopes

Confirm the authoritative scope list from the binary (docs may drift):

* [ ] Run: `still scan --list-scopes`
* [ ] Confirm:

  * [ ] `library-safe` is the default
  * [ ] `library` is an alias of `library-safe`
  * [ ] `library-unsafe` is clearly marked as UNSAFE
  * [ ] roots shown match current policy (safe subset vs full library)
* [ ] Update docs only if the binary output changed intentionally

---

## Dry-run vs Tests (Boundary Discipline)

This project treats **dry-run** and **tests** as different safety layers.

### Dry-run (runtime safety check)

Dry-run verifies that **the actual binary** performs *no destructive IO* while still exercising the execution path.

* [ ] Run: `still scan --scope library-safe --out dryrun_plan.json`
* [ ] Run: `still tui dryrun_plan.json`

  * [ ] Ensure `reason` is visible in the details panel
  * [ ] Ensure actions cycle only: Keep ↔ Quarantine ↔ Trash (no Purge)
* [ ] Run: `still apply --dry-run --vault-dir ./.still_smoke/vault dryrun_plan.json`

  * [ ] Confirm:

    * [ ] Summary is printed (counts/sizes)
    * [ ] **No filesystem changes occur**
    * [ ] **No journal is written** (per Hard Safety policy)
  * [ ] (Optional) Use `library-unsafe` only if intentionally testing advanced scope

    * [ ] Confirm WARNING is printed

### Tests (CI/Regression safety)

Tests verify invariants stay enforced as code changes.

* [ ] Run: `cargo test`
* [ ] Confirm coverage (minimum):

  * [ ] Malicious plan containing Purge is rejected
  * [ ] Scope violation rejected by default (allow flag works as intended)
  * [ ] Symlink/special files rejected at execution time (JIT)
  * [ ] Integration workflow exists: scan → plan → apply --dry-run

---

## Release Safety Smoke (Non-destructive first)

> Recommended order: **Dry-run → Tests → Real apply (in a lab)**

* [ ] Dry-run completed successfully (no IO, no journal)
* [ ] `cargo test` passed locally
## Lab Run (Sandbox Smoke) — scripts/lab_smoke.sh

> Goal: Validate the full workflow **without touching real user data**.
> This lab run operates only inside `~/still_lab` and is safe-by-default (dry-run only).

### Preconditions

* [ ] Ensure the script exists: `scripts/lab_smoke.sh`
* [ ] Make it executable: `chmod +x scripts/lab_smoke.sh`
* [ ] Confirm you are **not** pointing LAB_ROOT at a real directory:

  * [ ] LAB_ROOT must **not** be `/`
  * [ ] LAB_ROOT must **not** be `$HOME`
  * [ ] LAB_ROOT must **not** be `$HOME/Library`

### Step 1: Dry-run only (default)

* [ ] Run:

  * [ ] `bash scripts/lab_smoke.sh`

**Expected results**

* [ ] Sandbox is created at: `~/still_lab/Library`
* [ ] The script prints scope truth-check (via `still scan --list-scopes`)
* [ ] A plan file is created at: `~/still_lab/plan.json`
* [ ] TUI opens (unless disabled) and shows `reason`, and action cycle excludes Purge
* [ ] `still apply --dry-run` runs and:

  * [ ] prints a Summary
  * [ ] performs **NO filesystem changes**
  * [ ] performs **NO journal writes**

### Step 2: Dry-run without TUI (optional)

* [ ] Run:

  * [ ] `DO_TUI=0 bash scripts/lab_smoke.sh`

**Expected results**

* [ ] Same as Step 1, but TUI does not open

### Step 3: Real apply (explicit opt-in only)

> WARNING: This performs real quarantine/trash actions — still **sandbox-only**.

* [ ] Run:

  * [ ] `DO_APPLY=1 bash scripts/lab_smoke.sh`

**Expected results**

* [ ] The script pauses for 3 seconds before apply (Ctrl+C abort window)
* [ ] Apply affects only the sandbox (under `~/still_lab/Library`)
* [ ] Vault is used at: `~/still_lab/vault` (via `--vault-dir`)
* [ ] `still vault list --vault-dir ~/still_lab/vault` shows quarantined items
* [ ] The symlink candidate is rejected/skipped according to policy (no traversal)

### Notes

* The script creates:

  * a large dummy cache file (sparse via python truncate when available)
  * an old log file (via `touch -t`)
  * a symlink (must be rejected/skipped by Hard Safety)
* If the scan/apply CLI arguments change, update the single placeholder line in the script:

  * `(cd "$SANDBOX" && still scan --scope custom --out "$PLAN_PATH")`

## If Lab Run Fails (Top 3 Causes + Safe Triage)

> Rule: Debug must remain sandbox-only. Do **not** point LAB_ROOT at real directories.

### 1) `still scan` fails / plan.json not created

**Symptoms**

* Script stops at scan step
* `~/still_lab/plan.json` is not generated

**Likely causes**

* CLI arguments drift from implementation (`--root` / `--out` etc.)
* `--list-scopes` works but scan root logic is broken
* Embedded rules parse error / rules validation error (crash on startup)

**Safe triage**

* [ ] Verify `still scan --list-scopes` runs (check binary integrity).
* [ ] Run the specific scan line from the script manually to see stderr:
  * [ ] `cd ~/still_lab/Library && still scan --scope custom --out ~/still_lab/plan.json`
* [ ] If rules error suspected:
  * [ ] Run `still init` (if available) to regenerate rules and retry.
  * [ ] Check for `scope` / `empty pattern` errors in logs.

### 2) `still apply --dry-run` fails (validator / scope / schema)

**Symptoms**

* Dry-run fails with Validation Error despite no action taken.
* Error mentions “outside-scope”, “schema mismatch”, or “missing metadata”.

**Likely causes**

* `library-safe` scan target and validator allowed_scopes mismatch.
* Plan `schema_version` / header mismatch.
* Validator erroneously scope-checking `Keep` entries (Design deviation).

**Safe triage**

* [ ] Run `still apply --dry-run ~/still_lab/plan.json --vault-dir ~/still_lab/vault` manually to see full error.
* [ ] Compare `still scan --list-scopes` roots with current policy.
* [ ] If fails only when `Keep` entries are present:
  * [ ] Verify validator logic enforces scope check only for `action != Keep` (Trust Integrity fix).

### 3) Real apply (`DO_APPLY=1`) fails / quarantine behaves oddly

**Symptoms**

* Apply marks specific entries as `failed` / `skipped`.
* Symlink is copied or followed (Critical failure).
* Item not moved to vault or unexpected item moved.

**Likely causes**

* JIT safety blocking symlink/special file (Correct behavior).
* EXDEV fallback incomplete (rename vs copy+delete).
* Plan `reason` matches but execution target drifted.

**Safe triage**

* [ ] Ensure dry-run passes first.
* [ ] Check vault: `still vault list --vault-dir ~/still_lab/vault`.
* [ ] Verify symlink handling:
  * [ ] Symlink entry should be `skipped` / `failed` and **target NOT followed**.
* [ ] Keep debugging inside sandbox (do not expand LAB_ROOT).

## Log / Journal Viewing Order (1-line)

When debugging, read in this order: **CLI error output → pre-flight summary → journal entries for the latest `run_id` (apply_start → per-item results → apply_end)**. (Applies to real apply runs; dry-run writes no journal.)

## 1. Build & Environment
- [ ] Clean build: `cargo clean && cargo build --release`
- [ ] Binary exists: `./target/release/still` (or `still` if installed)
- [ ] Version check: `still --version` matches `v0.1.0`

## 2. Policy & Configuration
- [ ] **Ritual Phrase**: Verify `still vault purge` demands `PURGE STILL`.
- [ ] **Default Safe Scope**: `still scan` targets safe subset (`~/Library/Caches` etc.) by default. `Containers` excluded.
- [ ] **Symlinks**: Verify `still apply` rejects execution on symlinks (JIT check).

## 3. Documentation
- [ ] `README.md` and `README_EN.md` match CLI syntax (e.g., `still tui plan.json` positional arg).
- [ ] `docs/tui_guide.md` keys match implementation (`j`/`k`, `/`, `Space`, `T`).
- [ ] `CHANGELOG.md` has correct date.

## 4. Final Smoke Test (The "User Path")
0.  **Isolated vault**:
    * `rm -rf ./.still_smoke && mkdir -p ./.still_smoke/vault`

1.  **Scan**: `still scan --scope library-safe --out checklist_plan.json`
    *   Verify output JSON exists.
2.  **Tui**: `still tui checklist_plan.json`
    *   Navigate, Filter (`/`), Sort (`s`).
    *   Mark item for Quarantine (`Space` or `Q`).
    *   Mark item for Trash (`T` or `Space`), then unmark.
    *   **Safety Check**: Verify `Purge` action is NOT available in TUI cycle.
    *   Save & Quit (`q`).
3.  **Dry Run**: `still apply --dry-run checklist_plan.json`
    *   Verify output summary matches plan.
    *   Verify NO files deleted/moved.
4.  **Vault List**: `still vault list --vault-dir ./.still_smoke/vault`
    *   MUST be empty (dry-run writes no journal and performs no IO).

## 5. Release
- [ ] Git Tag: `git tag v0.1.0`
- [ ] Push: `git push origin v0.1.0`
- [ ] Create GitHub Release with changelog content.
