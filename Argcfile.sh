#!/usr/bin/env bash
# shellcheck disable=SC2154  # $argc_* vars are assigned by the eval line at EOF
set -euo pipefail

# @describe Dev tasks for omaddons. Run `argc` for the list.
#
# `argc gate` must pass before any commit that changes code (vision D16).
# @meta version 0.1.0
# @meta require-tools cargo,git

# @cmd Run the full local gate: fmt, clippy, tests, the widget pipeline, ubs on changed files, cargo deny on dependency changes
# @flag --all  Run cargo deny even when Cargo.toml, Cargo.lock and deny.toml are unchanged, and the widget's QML step when omarchy/ is
# @meta require-tools ubs,cargo-deny,bun
gate() {
    scripts/gate.sh ${argc_all:+--all}
}

# @cmd Run the widget pipeline as the gate does, QML step included: envelopes from the binary, bun test, the argv check, qmltestrunner
# @meta require-tools bun
widget-test() {
    scripts/widget-test.sh --qml
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

# @cmd Run the end-to-end suites (each test crate marked `//! e2e suite:`) with verbose test logs, then print where the logs are
# @arg filter  Only the suites whose name contains this; `all` or nothing runs every suite
# @arg args~   Passed to the test binaries after `--` (a test name, --exact, --test-threads=1, --nocapture)
e2e() {
    local suites=() selected=() tests=() file name filter target root start status=0
    while IFS= read -r file; do
        name=${file#tests/}
        name=${name%.rs}
        name=${name%/main}
        suites+=("$name")
    done < <({ grep -l '^//! e2e suite:' tests/*.rs tests/*/main.rs 2>/dev/null || true; } | sort)

    filter=${argc_filter:-all}
    for name in ${suites[@]+"${suites[@]}"}; do
        if [[ $filter == all || $name == *"$filter"* ]]; then
            selected+=("$name")
            tests+=(--test "$name")
        fi
    done
    if ((${#selected[@]} == 0)); then
        echo "e2e: no end-to-end suite matches '$filter'. The suites: ${suites[*]:-none}. To pass test arguments to every suite: argc e2e all <args>" >&2
        exit 2
    fi

    target=$(cargo metadata --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')
    root="$target/tmp/test-logs"
    # Fractional, so a log an earlier run wrote within this second is not counted.
    start=$(date +%s.%N)
    printf '==> e2e: %d suite(s): %s (OMADDONS_TEST_LOG=verbose)\n' "${#selected[@]}" "${selected[*]}" >&2
    OMADDONS_TEST_LOG=verbose cargo test --locked "${tests[@]}" -- ${argc_args[@]+"${argc_args[@]}"} || status=$?

    _e2e_summary "$root" "$start" "${selected[@]}"
    exit "$status"
}

# Counts the test logs this run wrote (newer than $start) in each selected suite's
# directory, cases included, and names each one that panicked or has no end record (a
# killed test). Only a log's last line is read: its end record, which starts with
# `{"kind":"end",` and holds no text a test wrote. Deletes nothing: each test replaces
# its own log.
_e2e_summary() {
    local root=$1 start=$2 written=0 log name last
    shift 2
    local panicked=() incomplete=()
    for name in "$@"; do
        [[ -d $root/$name ]] || continue
        while IFS= read -r log; do
            written=$((written + 1))
            last=$(tail -n 1 "$log")
            if [[ $last != '{"kind":"end",'* ]]; then
                incomplete+=("${log%steps.jsonl}transcript.md")
            elif [[ $last == *'"outcome":"panicked"'* ]]; then
                panicked+=("${log%steps.jsonl}transcript.md")
            fi
        done < <(find "$root/$name" -name steps.jsonl -newermt "@$start" | sort)
    done
    printf '==> e2e: %d test log(s) written, %d panicked, %d without an end record\n' \
        "$written" "${#panicked[@]}" "${#incomplete[@]}" >&2
    for log in ${panicked[@]+"${panicked[@]}"}; do printf '    panicked: %s\n' "${log#"$PWD"/}" >&2; done
    for log in ${incomplete[@]+"${incomplete[@]}"}; do printf '    incomplete: %s\n' "${log#"$PWD"/}" >&2; done
    printf '==> test logs: %s\n' "${root#"$PWD"/}"
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
