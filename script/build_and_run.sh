#!/usr/bin/env bash
set -euo pipefail

if [[ $(uname -s) != Darwin ]]; then
    printf 'This script requires macOS.\n' >&2
    exit 1
fi
if ! command -v cargo >/dev/null 2>&1; then
    printf 'Rust and Cargo are required: https://www.rust-lang.org/tools/install\n' >&2
    exit 1
fi

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
cd "$repo_root"
version=$(awk '/^\[package\]/{in_package=1; next} /^\[/{in_package=0} in_package && /^version[[:space:]]*=/ {gsub(/"/, "", $3); sub(/-.*/, "", $3); print $3; exit}' Cargo.toml)
if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    printf 'Could not determine the package version from Cargo.toml.\n' >&2
    exit 1
fi

# Keep build outputs in a known location even when Cargo is configured elsewhere.
export CARGO_TARGET_DIR="$repo_root/target"
cargo build --locked --release --features desktop --bins
app_dir=$(mktemp -d "$CARGO_TARGET_DIR/still-app.XXXXXX")
app="$app_dir/still.app"
scripts/build-macos-app.sh "$CARGO_TARGET_DIR/release/still" "$CARGO_TARGET_DIR/release/still-desktop" "$version" "$app"
printf 'Opening %s\n' "$app"
open -n "$app"
