#!/usr/bin/env bash
# argc widget-bench: the bar widget's cost baseline (omad-widget-user-first-1n2p.2), the
# numbers the performance pass (1n2p.38) reruns to prove its gains. Never part of the gate.
#
#   cli    the reads the widget makes, timed with hyperfine (p50, p95 and peak RSS, cold
#          and warm) on argc bench's synthetic home (tests/perf.rs bench_home: retail,
#          classic and era, 30 records, 5,040 managed files), and with --real once more
#          on this machine's own home;
#   open   the panel opening: the real Service.qml and PanelView.qml under qmltestrunner,
#          each child they start run by the real binary on the synthetic home
#          (widget-bench/runner.js through the bench's Process stand-in);
#   lists  Installed, Updates, Backups and Search's catalog built at 20, 150 and 500 rows
#          on the test stubs' mock Process, and the runner's RSS around 50 opens and
#          closes of the panel;
#   --window-events MIN  only: Hyprland's window events for MIN minutes, counted by name
#          (never a window's class or title), and the status runs the widget would make.
#
# Read-only. Besides building its own synthetic home in its output directory (installs,
# as argc bench's builder makes them), it runs reads only and writes only under its
# output directory: the runner answers the widget's silent checkpoint as skipped and
# refuses any other change, and --real points omaddons' cache at a copy in the output
# directory, so not even the discovery cache or the doorbell is written outside it.
# Refuses while a World of Warcraft client runs.
#
# Usage: scripts/widget-bench.sh [--out DIR] [--real] [--quick] [--only cli|open|lists]
#        scripts/widget-bench.sh --window-events MIN [--out DIR]
set -euo pipefail

usage="usage: scripts/widget-bench.sh [--out DIR] [--real] [--quick] [--only cli|open|lists]
       scripts/widget-bench.sh --window-events MIN [--out DIR]

  --out DIR            where everything goes (default target/widget-bench); each part
                       replaces only its own folder there
  --real               also time the CLI reads on this machine's home (read-only)
  --quick              few runs and one size: a smoke test of the script, not a baseline
  --only PART          one part: cli, open or lists
  --window-events MIN  listen to Hyprland's window events for MIN minutes, nothing else"

root="$(cd "$(dirname "$0")/.." && pwd)"
here="$root/scripts/widget-bench"
out="$root/target/widget-bench"
real=0
quick=0
only=""
window_minutes=""
while (($#)); do
    case $1 in
        -h | --help) echo "$usage"; exit 0 ;;
        --out) [[ $# -ge 2 && -n $2 ]] || { echo "widget-bench: --out needs a directory" >&2; exit 2; }; out=$2; shift ;;
        --real) real=1 ;;
        --quick) quick=1 ;;
        --only)
            [[ $# -ge 2 && $2 =~ ^(cli|open|lists)$ ]] || { echo "widget-bench: --only takes cli, open or lists" >&2; exit 2; }
            only=$2
            shift
            ;;
        --window-events)
            [[ $# -ge 2 && $2 =~ ^[0-9]+$ && $2 -gt 0 ]] || { echo "widget-bench: --window-events takes a number of minutes" >&2; exit 2; }
            window_minutes=$2
            shift
            ;;
        *) echo "widget-bench: unknown argument: $1" >&2; echo "$usage" >&2; exit 2 ;;
    esac
    shift
done
mkdir -p "$out"
out="$(cd "$out" && pwd)"

step() { printf '\n==> %s\n' "$*" >&2; }
need() { command -v "$1" >/dev/null || { echo "widget-bench: $1 is missing" >&2; exit 1; }; }
need bun
need jq

if [[ -n $window_minutes ]]; then
    step "Hyprland's window events for $window_minutes min (names only) → $out/window-events.json"
    exec bun "$here/window-events.js" --minutes "$window_minutes" --out "$out/window-events.json"
fi

need cargo
need hyperfine
part() { [[ -z $only || $only == "$1" ]]; }

# --------------------------------------------------------------------------- the build

step "building the release and bench-fixtures binaries"
cargo build --locked --release --quiet
cargo build --locked --profile bench-fixtures --quiet
release="$root/target/release/omaddons"
fixtures="$root/target/bench-fixtures/omaddons"
version="$("$release" --version | awk '{print $2}')"
commit="$(git -C "$root" rev-parse --short=12 HEAD)"
dirty="$(git -C "$root" status --porcelain -- omarchy src Cargo.toml Cargo.lock | wc -l)"

# summary.running of a status --cached in `env…`, as JSON ([] when nothing runs).
running_in() {
    # shellcheck disable=SC2086  # the env words are one KEY=value each
    env -i PATH="$PATH" $1 "$release" status --cached --json 2>/dev/null | jq -c '.data.summary.running // []'
}
refuse_running() {
    local where=$1 now=$2
    if [[ $now != "[]" ]]; then
        echo "widget-bench: a client runs on $where ($now): measure while nothing is played" >&2
        exit 1
    fi
}
real_env=""
if ((real)); then
    real_env="HOME=$HOME"
    for v in XDG_CONFIG_HOME XDG_STATE_HOME XDG_DATA_HOME; do
        [[ -n ${!v:-} ]] && real_env+=" $v=${!v}"
    done
    refuse_running "this machine" "$(running_in "$real_env")"
fi

# --------------------------------------------------------------------------- the home

home_root="$out/home"
step "the synthetic home at $home_root (tests/perf.rs bench_home)"
rm -rf "$home_root" "$out/home.cold"
BENCH_HOME="$home_root" cargo test --locked --quiet --test integration perf::bench_home \
    -- --ignored --exact >/dev/null
envs="$(tr '\n' ' ' < "$home_root/bench.env")"
cache="$home_root/cache"
# Cold: the home as built, no cache at all (no discovery cache, no memo, no inventory).
cp -a "$cache" "$out/home.cold"
# Warm: what the widget meets on an ordinary day, from reads alone. outdated runs on the
# bench-fixtures binary, which replays the home's fixtures: 30 updates remembered.
step "warming its cache with reads: outdated (fixtures), status, errors, backup list"
# shellcheck disable=SC2086
env -i PATH="$PATH" $envs "$fixtures" outdated --json >/dev/null || true
for words in "status --json" "errors --json" "backup list --json" "list -v --json"; do
    # shellcheck disable=SC2086
    env -i PATH="$PATH" $envs "$release" $words >/dev/null || true
done
refuse_running "the synthetic home" "$(running_in "$envs")"

# --------------------------------------------------------------------------- 1. CLI

runs_warm=30
runs_cold=10
if ((quick)); then runs_warm=3; runs_cold=2; fi

# The reads, by name: the widget's templates (omarchy/Model.js COMMANDS) and history.
reads=(
    "status_cached|status --cached -v --json"
    "status_full|status -v --json"
    "list|list -v --json"
    "errors|errors --limit 10 --json"
    "backups|backup list --limit 50 --json"
    "history|history --limit 50 --json"
    "catalog|search --catalog --json"
)

# hyperfine over each read in one home: name of the home, its env words, its cache dir
# and the cold copy of it.
time_reads() {
    local home=$1 words_env=$2 cache_dir=$3 cold_copy=$4
    local dir="$out/cli/$home"
    rm -rf "$dir"
    mkdir -p "$dir/warm" "$dir/cold"
    local restore="rm -rf '$cache_dir' && cp -a '$cold_copy' '$cache_dir'"
    local warm_copy="$out/cli/$home.warm-cache"
    rm -rf "$warm_copy"
    cp -a "$cache_dir" "$warm_copy"
    for read in "${reads[@]}"; do
        local name=${read%%|*} words=${read#*|}
        local line="env -i PATH=$PATH $words_env $release $words"
        printf '  %-14s warm' "$name" >&2
        hyperfine --style none -N --warmup 3 --runs "$runs_warm" --export-json "$dir/warm/$name.json" "$line" >/dev/null
        printf ', cold\n' >&2
        hyperfine --style none --runs "$runs_cold" --prepare "$restore" --export-json "$dir/cold/$name.json" "$line" >/dev/null
        # Back to warm for the next read.
        rm -rf "$cache_dir"
        cp -a "$warm_copy" "$cache_dir"
    done
    rm -rf "$warm_copy"
}

if part cli; then
    step "1. CLI reads on the synthetic home (warm: $runs_warm runs, cold: $runs_cold)"
    time_reads synthetic "$envs" "$cache" "$out/home.cold"
    if ((real)); then
        # This machine's clients, config and state, read in place; omaddons' cache is a
        # copy of its small files (no downloads), so every write stays in $out.
        real_cache="$out/real-cache"
        rm -rf "$real_cache" "$out/real-cache.cold"
        mkdir -p "$real_cache/omaddons" "$out/real-cache.cold"
        src="${XDG_CACHE_HOME:-$HOME/.cache}/omaddons"
        if [[ -d $src ]]; then
            find "$src" -maxdepth 1 -type f -name '*.json' -exec cp -p {} "$real_cache/omaddons/" \;
            [[ -d $src/wowi ]] && cp -a "$src/wowi" "$real_cache/omaddons/"
        fi
        step "1. CLI reads on this machine's home (cache copied into $real_cache)"
        time_reads real "$real_env XDG_CACHE_HOME=$real_cache" "$real_cache" "$out/real-cache.cold"
        refuse_running "this machine" "$(running_in "$real_env")"
    fi
fi

# --------------------------------------------------------------------------- QML

runner_qml() {
    local qmltestrunner=${QML_TEST_RUNNER:-}
    if [[ -z $qmltestrunner ]]; then
        for candidate in /usr/lib/qt6/bin/qmltestrunner qmltestrunner6 qmltestrunner-qt6; do
            if command -v "$candidate" >/dev/null; then qmltestrunner=$candidate; break; fi
        done
    fi
    [[ -n $qmltestrunner ]] || { echo "widget-bench: qmltestrunner (Qt 6) is missing" >&2; exit 1; }
    echo "$qmltestrunner"
}

# A copy of the plugin at $1 with $2 as its Build.js and the bench test $3 in tests/bench/.
plugin_copy() {
    local dir=$1 build=$2 test=$3
    rm -rf "$dir"
    cp -r "$root/omarchy" "$dir"
    cp "$build" "$dir/Build.js"
    mkdir -p "$dir/tests/bench"
    cp "$here/qml/$test" "$dir/tests/bench/"
}

# The WIDGET_BENCH line of a qmltestrunner log, as JSON.
bench_line() {
    grep -o 'WIDGET_BENCH {.*}$' "$1" | sed 's/^WIDGET_BENCH //' | jq -c "select(.kind == \"$2\")"
}

qml_dir="$out/qml"
if part open || part lists; then
    qmltestrunner="$(runner_qml)"
    mkdir -p "$qml_dir"
fi

if part open; then
    step "2. panel open: the real Service and PanelView, children run by $release"
    work="$qml_dir/open"
    rm -rf "$work"
    mkdir -p "$work"
    # The Build.js omarchy install would render for the synthetic home.
    home_var() { grep "^$1=" "$home_root/bench.env" | cut -d= -f2-; }
    cat >"$work/Build.js" <<EOF
.pragma library
// Generated by scripts/widget-bench.sh for one run; never committed.
var BINARY = $(jq -Rn --arg v "$release" '$v')
var TOOL_VERSION = $(jq -Rn --arg v "$version" '$v')
var PLUGIN_ID = "a44.omaddons-bench"
var CONTEXT = {
  "HOME": $(jq -Rn --arg v "$(home_var HOME)" '$v'),
  "XDG_CONFIG_HOME": $(jq -Rn --arg v "$(home_var XDG_CONFIG_HOME)" '$v'),
  "XDG_STATE_HOME": $(jq -Rn --arg v "$(home_var XDG_STATE_HOME)" '$v'),
  "XDG_CACHE_HOME": $(jq -Rn --arg v "$(home_var XDG_CACHE_HOME)" '$v'),
  "XDG_DATA_HOME": $(jq -Rn --arg v "$(home_var XDG_DATA_HOME)" '$v')
}
var CACHE_DIR = $(jq -Rn --arg v "$(home_var XDG_CACHE_HOME)/omaddons" '$v')
var CONTENT_SHA256 = "0000000000000000000000000000000000000000000000000000000000000000"
EOF
    plugin_copy "$work/plugin" "$work/Build.js" tst_bench_open.qml
    imports="$work/imports"
    cp -r "$root/omarchy/tests/qml/imports" "$imports"
    cp "$here/qml/imports/Quickshell/Io/Process.qml" "$imports/Quickshell/Io/Process.qml"
    rm -f "$work/port"
    bun "$here/runner.js" --binary "$release" --tool-version "$version" \
        --env-file "$home_root/bench.env" --port-file "$work/port" 2>"$work/runner.log" &
    runner_pid=$!
    trap 'kill "$runner_pid" 2>/dev/null || true' EXIT
    for _ in $(seq 100); do [[ -s $work/port ]] && break; sleep 0.05; done
    [[ -s $work/port ]] || { echo "widget-bench: the runner did not start ($work/runner.log)" >&2; exit 1; }
    url="http://127.0.0.1:$(cat "$work/port")"
    printf '.pragma library\nvar BINARY = %s\nvar RUNNER_URL = %s\n' \
        "$(jq -Rn --arg v "$release" '$v')" "$(jq -Rn --arg v "$url" '$v')" >"$imports/Quickshell/Io/BenchEnv.js"
    opens=10
    ((quick)) && opens=3
    printf '.pragma library\nvar RUNNER_URL = %s\nvar OPENS = %d\n' "$(jq -Rn --arg v "$url" '$v')" "$opens" \
        >"$work/plugin/tests/bench/BenchConfig.js"
    QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software QML_XHR_ALLOW_FILE_READ=1 \
        "$qmltestrunner" -input "$work/plugin/tests/bench/tst_bench_open.qml" -import "$imports" -o -,txt \
        >"$work/qmltestrunner.log" 2>&1 || { tail -30 "$work/qmltestrunner.log" >&2; exit 1; }
    kill "$runner_pid" 2>/dev/null || true
    trap - EXIT
    bench_line "$work/qmltestrunner.log" open >"$qml_dir/open.json"
    [[ -s $qml_dir/open.json ]] || { echo "widget-bench: no open result ($work/qmltestrunner.log)" >&2; exit 1; }
fi

if part lists; then
    step "3 and 5. lists at 20, 150 and 500 rows, and 50 opens and closes, on the mock Process"
    work="$qml_dir/lists"
    rm -rf "$work"
    mkdir -p "$work"
    export OMADDONS_WIDGET_OUT="$work/widget-out"
    mkdir -p "$OMADDONS_WIDGET_OUT"
    cargo test --locked --quiet --test integration -- --ignored --exact omarchy_widget::write_widget_fixtures >/dev/null
    plugin_copy "$work/plugin" "$root/omarchy/tests/fixtures/Build.js" tst_bench_lists.qml
    bun "$root/omarchy/tests/qml_fixtures.js" "$OMADDONS_WIDGET_OUT/fixtures" "$work/plugin/tests/bench/Fixtures.js"
    counts="[20, 150, 500]"
    repeats=3
    cycles=50
    if ((quick)); then counts="[20]"; repeats=1; cycles=5; fi
    printf '.pragma library\nvar COUNTS = %s\nvar REPEATS = %d\nvar CYCLES = %d\n' "$counts" "$repeats" "$cycles" \
        >"$work/plugin/tests/bench/BenchConfig.js"
    QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software QML_XHR_ALLOW_FILE_READ=1 \
        "$qmltestrunner" -input "$work/plugin/tests/bench/tst_bench_lists.qml" -import "$work/plugin/tests/qml/imports" -o -,txt \
        >"$work/qmltestrunner.log" 2>&1 || { tail -30 "$work/qmltestrunner.log" >&2; exit 1; }
    bench_line "$work/qmltestrunner.log" lists >"$qml_dir/lists.json"
    bench_line "$work/qmltestrunner.log" leak >"$qml_dir/leak.json"
    [[ -s $qml_dir/lists.json && -s $qml_dir/leak.json ]] || { echo "widget-bench: no list results ($work/qmltestrunner.log)" >&2; exit 1; }
fi

# --------------------------------------------------------------------------- the report

# A home's shape and what ran there, from status --cached in its environment.
shape_of() {
    # shellcheck disable=SC2086  # the env words are one KEY=value each
    env -i PATH="$PATH" $1 "$release" status --cached --json | jq -c '{running: .data.summary.running,
        clients: (.data.targets | length), present: ([.data.targets[] | select(.missing != true)] | length),
        addons: ([.data.targets[].addons.total] | add)}'
}
synthetic_shape="$(shape_of "$envs")"
machine_shape=null
((real)) && machine_shape="$(shape_of "$real_env XDG_CACHE_HOME=$out/real-cache")"
jq -n --arg commit "$commit" --argjson dirty "$dirty" --arg version "$version" \
    --arg measured_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --argjson quick "$quick" --argjson real "$real" \
    --argjson runs_warm "$runs_warm" --argjson runs_cold "$runs_cold" \
    --arg kernel "$(uname -r)" --arg cpu "$(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ //')" \
    --argjson synthetic "$synthetic_shape" --argjson machine "$machine_shape" \
    '{commit: $commit, dirty_files: $dirty, version: $version, measured_at: $measured_at, quick: ($quick == 1),
      runs: {warm: $runs_warm, cold: $runs_cold}, kernel: $kernel, cpu: $cpu,
      homes: {synthetic: $synthetic, real: $machine}}' >"$out/meta.json"
bun "$here/report.js" "$out"
