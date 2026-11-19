#!/bin/sh
# One command that says whether the tree is healthy. Run it before every install /
# commit:   dist/check.sh          (full: build, tests, headless smoke test)
#           dist/check.sh --quick  (skip the smoke test)
#
# Fails on: compile errors, ANY compiler warning, failing unit tests, and a headless
# end-to-end run that crashes, panics or restarts the shell. rustfmt / clippy run when
# installed (sudo dnf install rustfmt clippy).
set -eu
cd "$(dirname "$0")/.."

quick=0
[ "${1:-}" = "--quick" ] && quick=1

say() { printf '\n\033[1m== %s\033[0m\n' "$*"; }

# RUSTFLAGS replaces the rustflags from .cargo/config.toml, so repeat the shim path here.
# A separate target dir keeps these flags from invalidating the normal build cache.
export RUSTFLAGS="-D warnings -L $PWD/.devshim/lib"
export CARGO_TARGET_DIR=target/check

say "build (warnings are errors)"
cargo build --workspace --all-targets

say "unit tests"
cargo test --workspace --quiet

if cargo fmt --version >/dev/null 2>&1; then
    say "rustfmt"
    cargo fmt --all --check
fi
if cargo clippy --version >/dev/null 2>&1; then
    say "clippy"
    cargo clippy --workspace --all-targets -- -D warnings
fi

if [ "$quick" = 0 ]; then
    say "headless smoke test"
    python3 dist/smoke-test.py target/check/debug
fi

say "all checks passed"
