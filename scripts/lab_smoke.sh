#!/usr/bin/env bash
set -euo pipefail

# still Lab Smoke Script
# - Operates ONLY inside a sandbox directory
# - Default: dry-run only
# - Real apply requires explicit opt-in

# === Config (adjustable) ===
LAB_ROOT="${LAB_ROOT:-$HOME/still_lab}"
SCOPE="${SCOPE:-lab}"                 # Not directly used by still CLI yet, but implies usage context
PLAN_PATH="${PLAN_PATH:-$LAB_ROOT/plan.json}"
VAULT_DIR="${VAULT_DIR:-$LAB_ROOT/vault}"
DO_APPLY="${DO_APPLY:-0}"             # 0 = dry-run only, 1 = perform apply
DO_TUI="${DO_TUI:-1}"                 # 1 = open TUI for manual review

# Threshold knobs (only used to create dummy files)
BIG_MB="${BIG_MB:-64}"
OLD_TOUCH="${OLD_TOUCH:-202401010000}" # YYYYMMDDhhmm

# Ensure still binary is in path or use cargo run
if [[ -n "${STILL_BIN:-}" ]] && [[ -f "$STILL_BIN" ]]; then
    echo "[lab] using STILL_BIN: $STILL_BIN"
    alias still="$STILL_BIN"
elif ! command -v still >/dev/null 2>&1; then
    # Fallback to cargo build binaries
    SEARCH_LOCATIONS=(
        "./target/release/still"
        "./target/debug/still"
    )
    FOUND=""
    for loc in "${SEARCH_LOCATIONS[@]}"; do
        if [[ -f "$loc" ]]; then
            FOUND="$loc"
            break
        fi
    done

    if [[ -n "$FOUND" ]]; then
        echo "[lab] 'still' not in PATH, using: $FOUND"
        alias still="$FOUND"
    else
        echo "Error: 'still' binary not found in PATH or standard target dirs."
        echo "Build it with: cargo build --release"
        exit 1
    fi
fi
shopt -s expand_aliases

# === Safety pre-flight ===
echo "[lab] root:      $LAB_ROOT"
echo "[lab] plan:      $PLAN_PATH"
echo "[lab] vault_dir: $VAULT_DIR"
echo "[lab] DO_APPLY:  $DO_APPLY (0=dry-run only, 1=real apply)"
echo

if [[ "$LAB_ROOT" == "/" || "$LAB_ROOT" == "$HOME" || "$LAB_ROOT" == "$HOME/Library" ]]; then
  echo "[FATAL] LAB_ROOT looks unsafe: $LAB_ROOT"
  exit 1
fi

mkdir -p "$LAB_ROOT"
mkdir -p "$VAULT_DIR"

# === 1) Build sandbox (fake Library) ===
echo "[lab] building sandbox..."
SANDBOX="$LAB_ROOT/Library"
mkdir -p "$SANDBOX"/{Caches,Logs,Containers,"Application Support"}

# big dummy file (sparse if possible; fallback to dd)
BIG_FILE="$SANDBOX/Caches/big.cache"
if command -v python3 >/dev/null 2>&1; then
  python3 - <<PY
import os
path = r"$BIG_FILE"
size = ${BIG_MB} * 1024 * 1024
with open(path, "wb") as f:
    f.truncate(size)  # sparse on many FS, fast and CI-friendly
print("created sparse:", path, size)
PY
else
  dd if=/dev/zero of="$BIG_FILE" bs=1m count="$BIG_MB" status=none
fi

# old dummy file
OLD_FILE="$SANDBOX/Logs/old.log"
printf "old log\n" > "$OLD_FILE"
touch -t "$OLD_TOUCH" "$OLD_FILE"

# normal file (should usually remain Keep)
printf "keep me\n" > "$SANDBOX/Application Support/keep.txt"

# symlink (must be rejected / skipped)
ln -sf "$SANDBOX/Caches/big.cache" "$SANDBOX/Caches/link_to_big.cache"

echo "[lab] sandbox built at: $SANDBOX"
echo

# === 2) Scope list truth-check (optional but recommended) ===
echo "[lab] list scopes (source of truth):"
still scan --list-scopes
echo

# === 3) Scan -> plan ===
echo "[lab] scanning sandbox -> plan..."
# still 'custom' scope scans the CURRENT directory.
# So we cd into the sandbox.
(
  cd "$SANDBOX"
  still scan --scope custom --out "$PLAN_PATH"
)

# Verify plan creation
if [[ ! -f "$PLAN_PATH" ]]; then
    echo "[FATAL] Plan file not found at $PLAN_PATH"
    exit 1
fi
echo "[lab] plan created: $PLAN_PATH"
echo

# === 4) Optional TUI review ===
if [[ "$DO_TUI" == "1" ]]; then
  echo "[lab] opening TUI for review (no destructive IO in TUI)..."
  still tui "$PLAN_PATH"
  echo
fi

# === 5) Dry-run apply (must always be safe) ===
echo "[lab] apply --dry-run (must perform NO filesystem changes, NO journal writes)..."
# We add --allow-outside-scope because the sandbox is likely not in ~/Library
still apply --dry-run --allow-outside-scope --vault-dir "$VAULT_DIR" "$PLAN_PATH"
echo

# === 6) Real apply (explicit opt-in only) ===
if [[ "$DO_APPLY" == "1" ]]; then
  echo "[lab] REAL APPLY enabled. This should only affect the sandbox."
  echo "[lab] continuing in 3 seconds... Ctrl+C to abort."
  sleep 3
  still apply --allow-outside-scope --vault-dir "$VAULT_DIR" "$PLAN_PATH"
  echo
  echo "[lab] vault contents:"
  still vault list --vault-dir "$VAULT_DIR" || true
else
  echo "[lab] skipping real apply (set DO_APPLY=1 to enable)."
fi

echo
echo "[lab] done."
