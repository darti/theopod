#!/usr/bin/env bash
# Backticks in patterns below are literal Markdown, not substitutions.
# shellcheck disable=SC2016
#
# Diff vendor/esp-radio against the published crates.io crate (t-178,
# ADR-0027). Only the files that start a bullet of the "Change" section of
# THEO-PATCH.md may differ, be added or be removed; any other drift fails
# (exit 1). Tooling or input errors exit 2. Usage: just vendor-check
#
# ALLOWLIST_REF (CI sets it to the pull request's base commit): read the
# allowed paths from THEO-PATCH.md at that commit, so a change cannot both
# patch a new file and allow it; the note change has to land first.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
vendor=$root/vendor/esp-radio
patch=$vendor/THEO-PATCH.md
die() { echo "$*" >&2; exit 2; }

version=$(sed -n 's/^- Upstream: crates\.io `esp-radio` \([^ ,]*\).*/\1/p' "$patch")
[[ $version =~ ^[0-9A-Za-z.+-]+$ ]] || die "no valid upstream version in $patch"
sha=$(grep -o '`[0-9a-f]\{64\}`' "$patch" | tr -d '`' | head -n 1)
[ -n "$sha" ] || die "no upstream sha256 in $patch"

# The firmware must build from this directory, or the check proves nothing.
grep -q '^esp-radio = { path = "vendor/esp-radio" }' "$root/Cargo.toml" ||
    die "Cargo.toml no longer patches esp-radio to vendor/esp-radio"

work=$(mktemp -d "${TMPDIR:-/tmp}/esp-radio-check.XXXXXX")
trap 'rm -rf "$work"' EXIT
curl -sSfL --proto '=https' --proto-redir '=https' --retry 3 --max-time 120 \
    "https://static.crates.io/crates/esp-radio/esp-radio-$version.crate" -o "$work/crate.tgz"
echo "$sha  $work/crate.tgz" | shasum -a 256 -c --status ||
    die "esp-radio-$version.crate does not match the sha256 in $patch"
mkdir "$work/x"
tar xzf "$work/crate.tgz" -C "$work/x"
upstream=$work/x/esp-radio-$version
[ -d "$upstream" ] || die "unexpected crate layout"

# Allowed: the paths starting a "Change" bullet (of the note at
# ALLOWLIST_REF if set), the note itself and the
# dropped `.cargo_vcs_info.json`.
if [ -n "${ALLOWLIST_REF:-}" ]; then
    git -C "$root" show "$ALLOWLIST_REF:vendor/esp-radio/THEO-PATCH.md" > "$work/note" ||
        die "cannot read THEO-PATCH.md at $ALLOWLIST_REF"
else
    cp "$patch" "$work/note"
fi
{
    sed -n '/^## Change/,/^## /p' "$work/note" |
        sed -n 's/^- `\(src\/[A-Za-z0-9_/-]*\.rs\)`.*/\1/p'
    echo THEO-PATCH.md
    echo .cargo_vcs_info.json
} | sort -u > "$work/allowed"

# Every differing file, relative to the crate root. diff exits 1 when the
# trees differ (expected) and 2 on trouble (fatal). A directory present on
# one side only is expanded into its files.
status=0
diff -rq "$upstream" "$vendor" > "$work/raw" || status=$?
[ "$status" -le 1 ] || die "diff failed ($status)"
while IFS= read -r line; do
    case $line in
        "Files "*" differ")
            p=${line#Files }
            p=${p%% and *}
            echo "${p#"$upstream"/}"
            ;;
        "Only in "*)
            d=${line#Only in }
            dir=${d%%: *}
            f=${d#*: }
            for base in "$upstream" "$vendor"; do
                case $dir in "$base"*) rel=${dir#"$base"}; rel=${rel#/}; top=$base ;; esac
            done
            path=${rel:+$rel/}$f
            if [ -d "$top/$path" ]; then
                (cd "$top" && find "$path" -type f)
            else
                echo "$path"
            fi
            ;;
        *) echo "$line" ;;
    esac
done < "$work/raw" | sort -u > "$work/differing"

bad=$(comm -13 "$work/allowed" "$work/differing")
if [ -n "$bad" ]; then
    echo "vendor/esp-radio differs from esp-radio $version outside THEO-PATCH.md:" >&2
    echo "$bad" >&2
    exit 1
fi
echo "vendor/esp-radio matches esp-radio $version except: $(paste -sd' ' "$work/differing")"
