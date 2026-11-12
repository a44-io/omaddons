#!/usr/bin/env bash
# Publishes a tagged release on GitHub (omad-sylh): builds the tag from a clean clone,
# packs the x86_64 binary under a name mise's github backend picks for Linux x86_64,
# writes checksums.txt, and creates the release with notes from `br changelog`.
#
# Run it after the tag commit (version bump and the contract release step) is pushed
# with its tag. Nothing is published until every check passes.
#
# Usage: scripts/release.sh <tag> [--draft] [--notes <file>]
#   --draft          publish as a draft (mise ignores drafts: use it to look first)
#   --notes <file>   a summary to put above the changelog
set -euo pipefail

repo=a44-io/omaddons
tag=""
draft=()
summary=""
while (($#)); do
    case $1 in
        --draft) draft=(--draft) ;;
        --notes)
            summary=${2:?release: --notes needs a file}
            shift
            ;;
        -*) echo "release: unknown option $1" >&2; exit 2 ;;
        *) tag=$1 ;;
    esac
    shift
done
[[ $tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "release: a tag like v1.2.3 is required, got '$tag'" >&2; exit 2; }
[[ -z $summary || -f $summary ]] || { echo "release: no notes file $summary" >&2; exit 2; }
version=${tag#v}
root=$(git rev-parse --show-toplevel)
fail() { echo "release: $*" >&2; exit 1; }

# stdin to stdout with < and > escaped as HTML entities outside `code spans`: the
# backtick-separated fields at odd positions are prose.
escape_outside_code() {
    awk 'BEGIN { FS = OFS = "`" } {
        for (i = 1; i <= NF; i += 2) { gsub(/</, "\\&lt;", $i); gsub(/>/, "\\&gt;", $i) }
        print
    }'
}

git -C "$root" rev-parse -q --verify "refs/tags/$tag^{commit}" >/dev/null || fail "no tag $tag in this checkout"
git -C "$root" ls-remote --exit-code --tags origin "refs/tags/$tag" >/dev/null || fail "push the tag first: git push origin $tag"
if gh release view "$tag" -R "$repo" >/dev/null 2>&1; then fail "$tag already has a release"; fi
previous=$(git -C "$root" describe --tags --abbrev=0 "$tag^" 2>/dev/null) || previous=""

work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
git clone -q --depth 1 --branch "$tag" "file://$root" "$work/src"
declared=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$work/src/Cargo.toml" | head -n1)
[[ $declared == "$version" ]] || fail "$tag's Cargo.toml says version $declared"

echo "release: building $tag from a clean clone" >&2
(cd "$work/src" && cargo build --release --locked)
binary=$work/src/target/release/omaddons
said=$("$binary" --version)
[[ $said == "omaddons $version" ]] || fail "the build says '$said', not 'omaddons $version'"

# The binary at the archive's root, beside the license and README: mise links
# <installs>/<tool>/<version>/omaddons, which `omarchy install` stamps by its latest link.
name=omaddons-$version-x86_64-unknown-linux-gnu
mkdir "$work/pack"
cp "$binary" "$work/src/LICENSE" "$work/src/README.md" "$work/pack/"
tar -C "$work/pack" --owner=0 --group=0 --numeric-owner -czf "$work/$name.tar.gz" omaddons LICENSE README.md
(cd "$work" && sha256sum "$name.tar.gz" >checksums.txt)

# The notes: the summary, how to install, and the beads closed since the previous tag.
# br prints JSON when BR_OUTPUT_FORMAT asks for it (agent sessions do), so it is unset.
{
    [[ -n $summary ]] && { cat "$summary"; echo; }
    echo "## Install"
    echo
    echo "Linux x86_64, glibc 2.39 or newer (Arch, Omarchy). On Omarchy:"
    echo
    echo '```bash'
    echo "omarchy-mise-install github:$repo omaddons"
    echo '```'
    echo
    echo "Or download \`$name.tar.gz\`, check it with \`sha256sum -c checksums.txt\` and put \`omaddons\` on your \`PATH\`."
    echo
    if [[ -n $previous ]]; then
        echo "## Changes since $previous"
        echo
        echo "From the project's issue tracker (\`br changelog --since-tag $previous\`)."
        echo
        # A title's <word> outside backticks would be read as an HTML tag and vanish;
        # inside a code span GitHub shows it as written, so only the rest is escaped.
        (cd "$root" && env -u BR_OUTPUT_FORMAT br changelog --since-tag "$previous" --no-color) |
            sed -e '1d' -e 's/^\([A-Z][A-Za-z ]*\):$/### \1/' | escape_outside_code
    fi
} >"$work/notes.md"

gh release create "$tag" -R "$repo" --verify-tag ${draft[@]+"${draft[@]}"} \
    --title "omaddons $version" --notes-file "$work/notes.md" \
    "$work/$name.tar.gz" "$work/checksums.txt"
echo "release: $tag published: https://github.com/$repo/releases/tag/$tag" >&2
