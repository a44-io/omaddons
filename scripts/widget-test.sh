#!/usr/bin/env bash
# The widget pipeline (M3-D41), run by the gate after `cargo test` and by
# `argc widget-test`. Four ordered steps over one fresh $OMADDONS_WIDGET_OUT, removed
# afterwards, so nothing generated is ever read from or left in the tree:
#   1. the real binary writes envelopes for the reducers (write_widget_fixtures);
#   2. bun tests omarchy/Model.js over them and writes built-argv.json;
#   3. the binary's clap parses exactly the argv bun built (validate_built_argv);
#   4. with --qml, qmltestrunner runs omarchy/tests/qml over a copy of the plugin with
#      the hand-written fixture Build.js (tst_snapshots.qml renders every state into
#      $OMADDONS_WIDGET_OUT/snapshots, removed with the rest).
#
# `--snapshots <dir>` (`argc widget-snapshots`) runs step 1, then only tst_snapshots.qml,
# and keeps its PNGs in <dir>. It draws with Omarchy's own shell kit and the current
# theme when the kit is installed ($OMARCHY_SHELL_DIR, else /usr/share/omarchy/shell),
# through the stand-ins in omarchy/tests/qml/kit; `--stub-kit` draws with the test stubs,
# and `--theme <name>` with another installed Omarchy theme instead of the current one.
# Usage: scripts/widget-test.sh [--qml] | --snapshots <dir> [--stub-kit | --theme <name>]
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

usage="usage: scripts/widget-test.sh [--qml] | --snapshots <dir> [--stub-kit | --theme <name>]"
qml=0
snapshots=""
stub_kit=0
theme=""
while (($#)); do
    case $1 in
        --qml) qml=1 ;;
        --snapshots)
            [[ $# -ge 2 && -n $2 ]] || { echo "widget-test: --snapshots needs a directory ($usage)" >&2; exit 2; }
            snapshots=$2
            shift
            ;;
        --stub-kit) stub_kit=1 ;;
        --theme)
            [[ $# -ge 2 && -n $2 ]] || { echo "widget-test: --theme needs a theme's folder name ($usage)" >&2; exit 2; }
            theme=$2
            shift
            ;;
        *) echo "widget-test: unknown argument: $1 ($usage)" >&2; exit 2 ;;
    esac
    shift
done
if [[ -n $snapshots ]]; then
    qml=1
elif ((stub_kit)) || [[ -n $theme ]]; then
    echo "widget-test: --stub-kit and --theme go with --snapshots ($usage)" >&2
    exit 2
fi
if ((stub_kit)) && [[ -n $theme ]]; then
    echo "widget-test: --theme draws with Omarchy's kit, --stub-kit without it: pick one ($usage)" >&2
    exit 2
fi
# The theme's folder: the user's installed themes first, then Omarchy's own.
theme_dir=""
if [[ -n $theme ]]; then
    for candidate in "$HOME/.local/share/omarchy/themes/$theme" /usr/share/omarchy/themes/"$theme"; do
        if [[ -f $candidate/colors.toml ]]; then
            theme_dir=$candidate
            break
        fi
    done
    [[ -n $theme_dir ]] || { echo "widget-test: no installed Omarchy theme called $theme" >&2; exit 2; }
fi

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

# Omarchy's shell kit as an import tree for tst_snapshots.qml: its qs.Ui and qs.Commons,
# the test stubs for the layer-shell KeyboardPanel and Panel, its audio and input
# singletons left out, and the stub Quickshell with the kit-mode stand-ins laid over it.
# Prints the tree's path; fails when no kit is installed.
kit_imports() {
    local kit=${OMARCHY_SHELL_DIR:-/usr/share/omarchy/shell}
    [[ -f $kit/Ui/qmldir && -f $kit/Commons/qmldir ]] || return 1
    local tree=$OMADDONS_WIDGET_OUT/kit-imports
    local stubs=$plugin/tests/qml/imports
    mkdir -p "$tree/qs"
    cp -r "$kit/Ui" "$tree/qs/Ui"
    cp "$stubs/qs/Ui/KeyboardPanel.qml" "$stubs/qs/Ui/Panel.qml" "$tree/qs/Ui/"
    cp -r "$kit/Commons" "$tree/qs/Commons"
    grep -v -E '^(singleton )?(AudioNodes|InputDevices|InputMethodState|UntypedInput) ' \
        "$kit/Commons/qmldir" >"$tree/qs/Commons/qmldir"
    cp -r "$stubs/Quickshell" "$tree/Quickshell"
    cp -r "$plugin/tests/qml/kit/Quickshell/." "$tree/Quickshell/"
    printf 'Singleton 1.0 Singleton.qml\n' >>"$tree/Quickshell/qmldir"
    printf 'FileView 1.0 FileView.qml\n' >>"$tree/Quickshell/Io/qmldir"
    # The kit reads the theme at $HOME/.local/state/omarchy/current/theme: --theme gives it
    # a home of its own whose current theme is the one named.
    local kit_home=$HOME
    if [[ -n $theme_dir ]]; then
        kit_home=$OMADDONS_WIDGET_OUT/theme-home
        mkdir -p "$kit_home/.local/state/omarchy/current"
        ln -s "$theme_dir" "$kit_home/.local/state/omarchy/current/theme"
    fi
    {
        echo ".pragma library"
        echo "// Generated by scripts/widget-test.sh for one snapshot run; never committed."
        echo "var ENV = $(KIT_HOME=$kit_home bun -e 'console.log(JSON.stringify({ HOME: process.env.KIT_HOME || "" }))')"
    } >"$tree/Quickshell/KitEnv.js"
    echo "$tree"
}

step "widget 1/4: envelopes from the real binary"
cargo test --locked --test integration -- --ignored --exact omarchy_widget::write_widget_fixtures

if [[ -n $snapshots ]]; then
    step "widget 2/4 and 3/4: skipped (--snapshots renders only)"
else
    step "widget 2/4: bun test omarchy/tests"
    # From inside omarchy/: bun's test discovery walks its working directory first, and
    # target/ can hold hundreds of gigabytes. In UTC, as the QML step below: the History
    # fixture's newest change is moved to 12:00 UTC (history_fixtures), and its tests read
    # its day half an hour later, so no midnight falls between them, whatever the hour or
    # the machine's zone.
    (cd omarchy && TZ=UTC bun test tests)

    step "widget 3/4: the argv bun built, parsed by clap"
    cargo test --locked --test integration -- --ignored --exact omarchy_widget::validate_built_argv
fi

if ((qml)); then
    # The renders: the folder --snapshots names, kept; else beside the plugin copy,
    # removed with the run.
    renders=${snapshots:-$OMADDONS_WIDGET_OUT/snapshots}
    mkdir -p "$renders"
    find "$renders" -maxdepth 1 -name '*.png' -type f -delete
    plugin=$OMADDONS_WIDGET_OUT/plugin
    cp -r omarchy "$plugin"
    cp omarchy/tests/fixtures/Build.js "$plugin/Build.js"
    # The scenes step 1 wrote, for tst_plans.qml and tst_snapshots.qml: into the
    # temporary copy only.
    bun omarchy/tests/qml_fixtures.js "$OMADDONS_WIDGET_OUT/fixtures" "$plugin/tests/qml/Fixtures.js" "$renders"
    input=$plugin/tests/qml
    imports=()
    [[ -d $plugin/tests/qml/imports ]] && imports=(-import "$plugin/tests/qml/imports")
    tree=""
    if [[ -n $snapshots ]]; then
        step "widget 4/4: tst_snapshots.qml into $snapshots"
        input=$plugin/tests/qml/tst_snapshots.qml
        if ((!stub_kit)); then
            tree=$(kit_imports) || tree=""
            [[ -n $tree ]] || echo "widget-test: no Omarchy shell kit found; drawing with the test stubs" >&2
        fi
        [[ -n $tree ]] && imports=(-import "$tree")
    else
        step "widget 4/4: qmltestrunner omarchy/tests/qml"
    fi
    # In UTC, as the bun step: days and times read the same in every zone.
    TZ=UTC QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software QML_XHR_ALLOW_FILE_READ=1 \
        "$runner" -input "$input" "${imports[@]}" -o -,txt
    if [[ -n $snapshots ]]; then
        count=$(find "$renders" -maxdepth 1 -name '*.png' -type f | wc -l)
        drawn_with="test stubs"
        [[ -n $tree ]] && drawn_with="Omarchy shell kit"
        printf '\n==> %d renders in %s (%s)\n' "$count" "$snapshots" "$drawn_with" >&2
    fi
else
    step "widget 4/4: QML skipped (nothing under omarchy/ changed; --qml or gate --all runs it)"
fi
