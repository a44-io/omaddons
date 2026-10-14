#!/usr/bin/env bash
# shellcheck disable=SC2154  # $argc_* vars are assigned by the eval line at EOF
set -euo pipefail

# @describe Dev tasks for omaddons. Run `argc` for the list.
#
# `argc gate` must pass before any commit that changes code (vision D16).
# @meta version 0.1.0
# @meta require-tools cargo,git

# @cmd Run the full local gate: fmt, clippy, tests, ubs on changed files, cargo deny on dependency changes
# @flag --all  Run cargo deny even when Cargo.toml, Cargo.lock and deny.toml are unchanged
# @meta require-tools ubs,cargo-deny
gate() {
    scripts/gate.sh ${argc_all:+--all}
}

# @cmd Format the code in place
fmt() {
    cargo fmt
}

# @cmd Run clippy with warnings denied, as the gate does
lint() {
    cargo clippy --locked --all-targets -- -D warnings
}

# @cmd Run the test suite
# @arg filter  Only run tests whose name contains this
test_() {
    cargo test --locked ${argc_filter:+"$argc_filter"}
}

eval "$(argc --argc-eval "$0" "$@")"
