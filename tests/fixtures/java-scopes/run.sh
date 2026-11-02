#!/usr/bin/env bash
# Golden runner for the java-scopes fixture corpus.
#
#   ./run.sh            regenerate expected/ from the current binary
#   ./run.sh --check    diff expected/ against the current binary
#
# The binary is $FILER if set, else target/{release,debug}/filer of the
# checkout this script lives in, else `filer` on $PATH. Build it with
# `cargo build --release` first.
#
# The goldens are normalized so they are machine-independent:
#   - the absolute fixture root becomes <root>
#   - the branch name in the state TOML becomes <branch>
#   - `created` in the state TOML becomes <timestamp>
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
mode=generate
for arg in "$@"; do
    case "$arg" in
        --check) mode=check ;;
        -h|--help) sed -n '2,14p' "$0"; exit 0 ;;
        *) echo "run.sh: unknown argument: $arg" >&2; exit 2 ;;
    esac
done

filer=""
if [[ -n "${FILER:-}" ]]; then
    filer="$FILER"
elif [[ -x "$repo/target/release/filer" ]]; then
    filer="$repo/target/release/filer"
elif [[ -x "$repo/target/debug/filer" ]]; then
    filer="$repo/target/debug/filer"
elif command -v filer >/dev/null 2>&1; then
    filer="$(command -v filer)"
fi
if [[ -z "$filer" || ! -x "$filer" ]]; then
    echo "run.sh: no filer binary; run 'cargo build --release' or set FILER=" >&2
    exit 2
fi
echo "run.sh: binary: $filer" >&2

normalize() {
    local fixture="$1"
    sed -e "s|$fixture|<root>|g" \
        -e 's|^name = ".*"$|name = "<branch>"|' \
        -e 's|^created = [0-9][0-9]*$|created = <timestamp>|'
}

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
status=0
checked=0

while read -r case_name fixture seed; do
    case "$case_name" in ''|'#'*) continue ;; esac
    fixture_dir="$here/$fixture"
    state="$work/$case_name.toml"
    expected_dir="$here/expected"
    actual="$work/$case_name"
    mkdir -p "$actual"

    if ! "$filer" --root "$fixture_dir" bundle --depth 2 --state "$state" \
            "$seed" >"$actual/bundle.md" 2>"$actual/bundle.err"; then
        echo "run.sh: bundle failed for $case_name" >&2
        cat "$actual/bundle.err" >&2
        exit 1
    fi
    if ! "$filer" --root "$fixture_dir" search --state "$state" \
            >"$actual/search.jsonl" 2>"$actual/search.err"; then
        echo "run.sh: search failed for $case_name" >&2
        cat "$actual/search.err" >&2
        exit 1
    fi
    # The state file is the source of truth for the v3 contract; the search
    # rows above mark included entries from it.
    normalize "$fixture_dir" <"$state" >"$actual/state.toml"
    normalize "$fixture_dir" <"$actual/bundle.md" >"$actual/bundle.norm.md"
    normalize "$fixture_dir" <"$actual/search.jsonl" >"$actual/search.norm.jsonl"

    for kind in bundle.norm.md search.norm.jsonl state.toml; do
        out="$expected_dir/$case_name"
        case "$kind" in
            bundle.norm.md) out="$out.md" ;;
            search.norm.jsonl) out="$out.search.jsonl" ;;
            state.toml) out="$out.state.toml" ;;
        esac
        checked=$((checked + 1))
        if [[ "$mode" == generate ]]; then
            cp "$actual/$kind" "$out"
        elif ! diff -u "$out" "$actual/$kind"; then
            echo "run.sh: mismatch: ${out#$here/}" >&2
            status=1
        fi
    done
done <"$here/cases.txt"

echo "run.sh: $mode: $checked golden files" >&2
exit "$status"
