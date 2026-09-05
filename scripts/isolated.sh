#!/usr/bin/env bash
# Run test/coverage commands without touching the caller's HOME or sessions.
set -euo pipefail
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
scratch=$(mktemp -d /tmp/ezpn-gate.XXXXXX)
trap 'rm -rf "$scratch"' EXIT
export HOME="$scratch/home"
export XDG_CONFIG_HOME="$scratch/config"
export XDG_DATA_HOME="$scratch/data"
export XDG_STATE_HOME="$scratch/state"
export XDG_CACHE_HOME="$scratch/cache"
export XDG_RUNTIME_DIR="$scratch/run"
export EZPN_TEST_SOCKET_DIR="$scratch/run"
mkdir -m 700 "$HOME" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME" "$XDG_RUNTIME_DIR"
unset EZPN
"$@"
