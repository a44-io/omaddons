#!/usr/bin/env bash
# The widget pipeline (M3-D41), run by the gate after `cargo test` and by
# `argc widget-test`. Four ordered steps over one fresh $OMADDONS_WIDGET_OUT, removed
# afterwards, so nothing generated is ever read from or left in the tree:
#   1. the real binary writes envelopes for the reducers (write_widget_fixtures);
#   2. bun tests omarchy/Model.js over them and writes built-argv.json;
#   3. the binary's clap parses exactly the argv bun built (validate_built_argv);
#   4. with --qml, qmltestrunner runs omarchy/tests/qml over a copy of the plugin with
#      the hand-written fixture Build.js.
# Usage: scripts/widget-test.sh [--qml]
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

qml=0
for arg in "$@"; do
    case $arg in
        --qml) qml=1 ;;
        *) echo "widget-test: unknown argument: $arg (usage: scripts/widget-test.sh [--qml])" >&2; exit 2 ;;
    esac
done

step() { printf '\n==> %s\n' "$*" >&2; }

missing() {
    echo "widget-test: $1 is missing: install bun / qt6-declarative (qmltestrunner) to run the widget tests" >&2
    exit 1
}
command -v bun >/dev/null || missing bun
# Qt 6's runner: a `qmltestrunner` on PATH can be Qt 5's (qt5-declarative), which loads
# nothing of Qt 6 and exits 1 without a word. $QML_TEST_RUNNER overrides.
runner=${QML_TEST_RUNNER:-}
if ((qml)) && [[ -z $runner ]]; then
    for candidate in /usr/lib/qt6/bin/qmltestrunner qmltestrunner6 qmltestrunner-qt6; do
        if command -v "$candidate" >/dev/null; then
            runner=$candidate
            break
        fi
    done
fi
if ((qml)) && ! command -v "${runner:-qmltestrunner6}" >/dev/null; then
    missing "qmltestrunner (Qt 6)"
fi

OMADDONS_WIDGET_OUT=$(mktemp -d)
export OMADDONS_WIDGET_OUT
trap 'rm -rf "$OMADDONS_WIDGET_OUT"' EXIT

step "widget 1/4: envelopes from the real binary"
cargo test --locked --test omarchy_widget -- --ignored --exact write_widget_fixtures

step "widget 2/4: bun test omarchy/tests"
# From inside omarchy/: bun's test discovery walks its working directory first, and
# target/ can hold hundreds of gigabytes.
(cd omarchy && bun test tests)

step "widget 3/4: the argv bun built, parsed by clap"
cargo test --locked --test omarchy_widget -- --ignored --exact validate_built_argv

if ((qml)); then
    step "widget 4/4: qmltestrunner omarchy/tests/qml"
    plugin=$OMADDONS_WIDGET_OUT/plugin
    cp -r omarchy "$plugin"
    cp omarchy/tests/fixtures/Build.js "$plugin/Build.js"
    imports=()
    [[ -d $plugin/tests/qml/imports ]] && imports=(-import "$plugin/tests/qml/imports")
    QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software \
        "$runner" -input "$plugin/tests/qml" "${imports[@]}" -o -,txt
else
    step "widget 4/4: QML skipped (nothing under omarchy/ changed; --qml or gate --all runs it)"
fi
