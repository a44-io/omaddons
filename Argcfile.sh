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

# @cmd Benchmark the checkpoint's timing targets with hyperfine on a synthetic home (never part of the gate)
# @meta require-tools hyperfine,jq
bench() {
    scripts/bench.sh
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

# @cmd Fuzz one parser (vision §5.12; never part of the gate). The M1 checkpoint runs every target for the default 600 s
# @arg target![`_choice_fuzz_target`]  The fuzz target (fuzz/fuzz_targets/<target>.rs)
# @option --time=600  Seconds to run
# @option --toolchain=nightly  The nightly toolchain to run cargo-fuzz with
fuzz() {
    _fuzz_check "$argc_toolchain"
    _fuzz_run "$argc_target" "$argc_time" "$argc_toolchain"
}

# @cmd Fuzz every parser in turn, each for --time seconds
# @option --time=600  Seconds per target
# @option --toolchain=nightly  The nightly toolchain to run cargo-fuzz with
fuzz-all() {
    _fuzz_check "$argc_toolchain"
    local target
    while IFS= read -r target; do
        _fuzz_run "$target" "$argc_time" "$argc_toolchain"
    done < <(_choice_fuzz_target)
}

_choice_fuzz_target() {
    local f
    for f in fuzz/fuzz_targets/*.rs; do
        f=${f##*/}
        printf '%s\n' "${f%.rs}"
    done
}

# cargo-fuzz needs nightly sanitizer flags, and the nightly must be at least the
# crate's rust-version.
_fuzz_check() {
    local toolchain=$1 need version
    if ! command -v cargo-fuzz >/dev/null; then
        echo "fuzz: cargo-fuzz is missing: cargo install cargo-fuzz" >&2
        exit 1
    fi
    if ! version=$(rustc "+$toolchain" --version 2>/dev/null); then
        echo "fuzz: no $toolchain toolchain: rustup toolchain install nightly" >&2
        exit 1
    fi
    need=$(sed -n 's/^rust-version = "\(.*\)"$/\1/p' Cargo.toml)
    version=$(awk '{print $2}' <<<"$version")
    version=${version%%-*}
    if [[ $(printf '%s\n%s\n' "$need" "$version" | sort -V | head -n1) != "$need" ]]; then
        echo "fuzz: $toolchain is rustc $version, older than omaddons' $need: rustup update nightly, or pass --toolchain nightly-<date>" >&2
        exit 1
    fi
}

# A crash leaves its input in fuzz/artifacts/<target>/: make it a regression unit test in
# the owning parser, fix, and run again.
_fuzz_run() {
    local target=$1 time=$2 toolchain=$3
    mkdir -p "fuzz/corpus/$target"
    cargo "+$toolchain" fuzz run "$target" "fuzz/corpus/$target" "fuzz/seeds/$target" \
        -- "-max_total_time=$time" -rss_limit_mb=2048
}

eval "$(argc --argc-eval "$0" "$@")"
