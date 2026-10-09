#!/bin/sh
# Run the xfeed server with the project-local Rust toolchain in .venv.
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export RUSTUP_HOME="$ROOT/.venv/rustup"
export CARGO_HOME="$ROOT/.venv/cargo"
export PATH="$CARGO_HOME/bin:$PATH"
cd "$ROOT"
if [ ! -f config.yml ]; then
  echo "config.yml not found in $ROOT" >&2
  echo "create it first: cp config.example.yml config.yml, then set x_bearer_token" >&2
  exit 1
fi
# Cargo rebuilds only when sources changed; this is a no-op otherwise.
cargo build --release --locked
exec target/release/xfeed config.yml
