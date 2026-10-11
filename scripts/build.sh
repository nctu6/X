#!/bin/sh
# Build the release binary (target/release/xfeed). Extra arguments go to cargo,
# e.g. scripts/build.sh -j2. Restart scripts/run.sh afterwards to serve it.
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
. "$ROOT/scripts/toolchain.sh"
xfeed_use_toolchain
exec cargo build --release --locked "$@"
