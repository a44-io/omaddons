#!/usr/bin/env bash
# argc bench: hyperfine medians for the M2 checkpoint's timing targets, on a synthetic
# home of three clients, 30 records and 5,040 managed files (tests/perf.rs bench_home),
# never on real clients.
#
# The true release binary times what needs no network: status --cached, status (the
# drift check) and the warm runs, which must add no net.log line. Cold runs need the
# network to be repeatable, and release builds ignore OMADDONS_HTTP_FIXTURES (DEC-7), so
# they use the bench-fixtures profile: the release profile with debug assertions on,
# which replays fixtures and is, if anything, slower.
#
# The home is restored from a pristine copy at the same path for every cold run: its
# registry holds absolute paths.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
work="$root/target/bench"
home="$work/home"
pristine="$work/home.pristine"
results="$work/results"

echo "building the release and bench-fixtures binaries…" >&2
cargo build --locked --release --quiet
cargo build --locked --profile bench-fixtures --quiet
release="$root/target/release/omaddons"
fixtures="$root/target/bench-fixtures/omaddons"

echo "generating the bench home at $home…" >&2
rm -rf "$work"
mkdir -p "$results"
# A debug build: its installs replay the fixtures.
BENCH_HOME="$home" cargo test --locked --quiet --test integration perf::bench_home \
    -- --ignored --exact >/dev/null
cp -a "$home" "$pristine"

# hyperfine runs these strings through a shell (or splits them as one would), so every
# path in them is quoted as one word: a checkout under a path with a space in it would
# otherwise have `rm -rf` take the path's pieces, the first of them a directory above
# the repository.
quoted() {
    printf '%q' "$1"
}
envs=""
while IFS= read -r line; do
    [[ -n $line ]] && envs+="$(quoted "$line") "
done < "$home/bench.env"
restore="rm -rf $(quoted "$home") && cp -a $(quoted "$pristine") $(quoted "$home")"
netlog="$home/state/omaddons/net.log"

# The command line hyperfine runs: the binary in the home's environment alone.
run() {
    local words=() word
    for word in "$@"; do
        words+=("$(quoted "$word")")
    done
    echo "env -i PATH=$(quoted "$PATH") $envs${words[*]}"
}

lines() {
    if [ -f "$netlog" ]; then wc -l < "$netlog"; else echo 0; fi
}

# name, binary label, then hyperfine's own arguments.
declare -a rows=()
measure() {
    local name="$1" label="$2"
    shift 2
    local json="$results/${name// /_}.json"
    hyperfine --style none --export-json "$json" "$@" >/dev/null
    local median
    # hyperfine 1.x exports `median`; 2.x (schema_version 2) a per-metric `summary`.
    median="$(jq -r '.results[0] | (.median // .summary.time_wall_clock.median)
        | . * 1000 | . * 100 | round / 100' "$json")"
    rows+=("$name|$label|$median")
    printf '%-28s %-16s %10s ms\n' "$name" "$label" "$median" >&2
}

# Cold: no memo, no blobs; the fixtures binary, a fresh home per run.
measure "outdated cold" bench-fixtures --runs 5 --prepare "$restore" \
    "$(run "$fixtures" outdated --json)"
# Warm: inside the 15-minute window after the last cold run, no request.
before="$(lines)"
measure "outdated warm" release -N --warmup 1 --runs 20 "$(run "$release" outdated --json)"
outdated_new=$(( $(lines) - before ))
measure "status --cached" release -N --warmup 3 --runs 50 \
    "$(run "$release" status --cached --json)"
measure "status (drift)" release -N --warmup 3 --runs 20 "$(run "$release" status --json)"
# One fixtures warmup caches the blobs and candidate facts; the timed release runs then
# make no request.
eval "$(run "$fixtures" update --all --dry-run --json)" >/dev/null # ubs:ignore — the command line run() built, as hyperfine gets it
before="$(lines)"
measure "update --all --dry-run warm" release --warmup 1 --runs 10 \
    "$(run "$release" update --all --dry-run --json)"
update_new=$(( $(lines) - before ))
measure "update --all cold" bench-fixtures --runs 5 --prepare "$restore" \
    "$(run "$fixtures" update --all --json)"

echo
echo "| Run | Binary | Median (ms) | Target |"
echo "|---|---|---|---|"
miss=0
for row in "${rows[@]}"; do
    IFS='|' read -r name label median <<< "$row"
    case "$name" in
        "status --cached") target=5 ;;
        # The whole status, discovery included, holds the drift check's 30 ms.
        "status (drift)") target=30 ;;
        "update --all --dry-run warm") target=2000 ;;
        "update --all cold") target=10000 ;;
        *) target="" ;;
    esac
    verdict=""
    if [ -n "$target" ]; then
        if awk -v m="$median" -v t="$target" 'BEGIN { exit !(m < t) }'; then
            verdict="< $target ✓"
        else
            verdict="< $target ✗"
            miss=1
        fi
    fi
    echo "| $name | $label | $median | $verdict |"
done
echo
echo "net.log lines added by the warm runs: outdated $outdated_new, update --all --dry-run $update_new"
if [ "$outdated_new" -ne 0 ] || [ "$update_new" -ne 0 ]; then
    miss=1
fi
exit "$miss"
