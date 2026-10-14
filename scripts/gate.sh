#!/usr/bin/env bash
# The local gate (vision D16): every commit that changes code runs this first and is
# committed only when it passes. Usage: scripts/gate.sh [--all]
#   --all   also run `cargo deny check` when no dependency file changed
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

if ((${#changed[@]})); then
    step "ubs on ${#changed[@]} changed file(s)"
    ubs "${changed[@]}"
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
