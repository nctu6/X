#!/bin/sh
# Refresh posts from the console, including image downloads.
# This bypasses the four-slot daily limit.
# Arguments are passed to `xfeed update` (for example `--tab News`).
set -eu
cd "$(dirname "$0")"
if [ -x ./target/release/xfeed ]; then
  BIN=./target/release/xfeed
elif [ -x ./target/debug/xfeed ]; then
  BIN=./target/debug/xfeed
else
  echo "build xfeed first: cargo build --release" >&2
  exit 1
fi
exec "$BIN" update "$@"
