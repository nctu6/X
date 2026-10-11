#!/bin/sh
# Run the xfeed server in the foreground.
# Builds first only when target/release/xfeed is missing or XFEED_BUILD=1;
# otherwise it starts the existing binary right away. Use scripts/build.sh
# to rebuild after pulling changes, then restart this script.
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
if [ ! -f config.yml ]; then
  echo "config.yml not found in $ROOT" >&2
  echo "create it first: cp config.example.yml config.yml, then set x_bearer_token" >&2
  exit 1
fi
if [ ! -x target/release/xfeed ] || [ "${XFEED_BUILD:-0}" = "1" ]; then
  . "$ROOT/scripts/toolchain.sh"
  xfeed_use_toolchain
  cargo build --release --locked
fi
exec target/release/xfeed config.yml
