# Sourced by run.sh and build.sh to find a Rust toolchain. POSIX sh.
# Order:
#   1. RUSTUP_HOME / CARGO_HOME already set by the caller: use them.
#   2. XFEED_RUST_ENV names an env file, or ../.toolchains/rust/env.sh exists
#      next to the repository: source it (a toolchain shared by several repos).
#   3. .venv/rustup and .venv/cargo inside the repository.
#   4. Whatever cargo is on PATH (for example from ~/.cargo/env).
xfeed_use_toolchain() {
  if [ -n "${RUSTUP_HOME:-}" ] || [ -n "${CARGO_HOME:-}" ]; then
    XFEED_TOOLCHAIN="environment"
  else
    shared="${XFEED_RUST_ENV:-$ROOT/../.toolchains/rust/env.sh}"
    if [ -f "$shared" ]; then
      # shellcheck disable=SC1090
      . "$shared"
      XFEED_TOOLCHAIN="$shared"
    elif [ -d "$ROOT/.venv/cargo" ]; then
      export RUSTUP_HOME="$ROOT/.venv/rustup"
      export CARGO_HOME="$ROOT/.venv/cargo"
      XFEED_TOOLCHAIN="$ROOT/.venv"
    else
      XFEED_TOOLCHAIN="PATH"
    fi
  fi
  if [ -n "${CARGO_HOME:-}" ] && [ -d "$CARGO_HOME/bin" ]; then
    case ":$PATH:" in
      *":$CARGO_HOME/bin:"*) ;;
      *) export PATH="$CARGO_HOME/bin:$PATH" ;;
    esac
  fi
  if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo not found (toolchain: $XFEED_TOOLCHAIN); install Rust or set RUSTUP_HOME/CARGO_HOME" >&2
    return 1
  fi
  echo "using Rust toolchain from $XFEED_TOOLCHAIN" >&2
}
