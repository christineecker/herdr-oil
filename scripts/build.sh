#!/bin/sh
# herdr runs this once at install/link time. Builds the release binary with
# cargo (rustup's env is sourced when present).
set -eu
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
if ! command -v cargo >/dev/null 2>&1; then
  echo "herdr-oil needs Rust (cargo) from https://rustup.rs" >&2
  exit 1
fi
cd "$script_dir/.."
exec cargo build --release
