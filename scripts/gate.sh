#!/usr/bin/env bash
# The local gate (vision D16): every commit that changes code runs this first and is
# committed only when it passes. Usage: scripts/gate.sh [--all]
#   --all   also run `cargo deny check` when no dependency file changed, and the widget's
#           QML step when nothing under omarchy/ changed
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

all=0
for arg in "$@"; do
    case $arg in
        --all) all=1 ;;
        *) echo "gate: unknown argument: $arg (usage: scripts/gate.sh [--all])" >&2; exit 2 ;;
    esac
done

step() { printf '\n==> %s\n' "$*" >&2; }

# Changed files relative to HEAD: staged, unstaged and untracked, minus the tracker.
changed=()
while IFS= read -r f; do
    [[ -n $f && -f $f && $f != .beads/* ]] && changed+=("$f")
done < <({ git diff --name-only HEAD 2>/dev/null || git diff --name-only --cached; git ls-files --others --exclude-standard; } | sort -u)

step "cargo fmt --check"
cargo fmt --check

step "cargo clippy --locked --all-targets -- -D warnings"
cargo clippy --locked --all-targets -- -D warnings

step "cargo test --locked"
cargo test --locked

# Steps 1-3 run on every gate: a clap flag or an envelope key can break the widget as
# surely as a QML edit. Step 4 (QML) runs when omarchy/ changed, deletions included.
widget=()
if ((all)) || [[ -n $({ git diff --name-only HEAD -- omarchy 2>/dev/null; git ls-files --others --exclude-standard -- omarchy; }) ]]; then
    widget=(--qml)
fi
step "widget pipeline"
scripts/widget-test.sh "${widget[@]}"

# ubs gets every changed file but Cargo's manifests: given Cargo.toml (ubs 5.5.26 on),
# it builds a copy of the changed files alone, which cannot compile, and ends partial
# (exit 2) without scanning more. The cargo steps above and cargo deny below check them.
# The list was made before the tests ran, and a test may have deleted a file since (insta
# removes a stale `.snap.new` once its snapshot passes): only files that still exist are
# scanned, and never insta's working files, which are not committed.
scan=()
for f in ${changed[@]+"${changed[@]}"}; do
    [[ -f $f ]] || continue
    case $f in Cargo.toml | Cargo.lock | *.snap.new | *.pending-snap) ;; *) scan+=("$f") ;; esac
done
if ((${#scan[@]})); then
    step "ubs on ${#scan[@]} changed file(s)"
    # Exit 3 is ubs's "nothing scanned": no changed file is in a language it scans (a
    # snapshot-, fixture- or Markdown-only change). Not a pass and nothing to fix, so the
    # gate says so and goes on (CLAUDE.md); every other non-zero exit fails it. The exit is
    # read, never hidden with UBS_ALLOW_NO_SCAN.
    ubs_exit=0
    ubs "${scan[@]}" || ubs_exit=$?
    case $ubs_exit in
        0) ;;
        3) echo "ubs: no file in a language UBS scans (exit 3: nothing scanned, nothing to fix)" >&2 ;;
        *) echo "gate: ubs failed (exit $ubs_exit)" >&2; exit "$ubs_exit" ;;
    esac
else
    step "ubs: no changed files, skipped"
fi

deps_changed=0
for f in "${changed[@]}"; do
    case $f in Cargo.toml | Cargo.lock | deny.toml) deps_changed=1 ;; esac
done
if ((deps_changed || all)); then
    step "cargo deny check"
    cargo deny check
else
    step "cargo deny: no dependency change, skipped (use --all to force)"
fi

step "gate passed"
