#!/usr/bin/env bash
# Fetches opencore-aacdec and applies the theo-pod patches (t-169).
#
# Usage: fetch.sh DEST
# Downloads the PacketVideo AAC decoder (Apache-2.0) from AOSP at tag
# android-4.0.4_r2.1, applies tools/opencore-aacdec/patches/*.patch in order
# and writes the patch set's SHA-256 to DEST/.theo-patches. Does nothing when
# DEST already holds this patch set; refetches when the patches change.
# The sources are never committed (ADR-0026).
set -euo pipefail
dest=$1
root=$(cd "$(dirname "$0")/../.." && pwd)
patches=("$root"/tools/opencore-aacdec/patches/*.patch)
stamp=$(cat "${patches[@]}" | shasum -a 256 | cut -d' ' -f1)
if [ -f "$dest/pvmp4audiodecoder_api.h" ] && [ "$(cat "$dest/.theo-patches" 2>/dev/null)" = "$stamp" ]; then
	exit 0
fi
mkdir -p "$(dirname "$dest")"
tmp=$(mktemp -d "$dest.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
curl -sSfL https://android.googlesource.com/platform/frameworks/base/+archive/refs/tags/android-4.0.4_r2.1/media/libstagefright/codecs/aacdec.tar.gz | tar xz -C "$tmp"
for patch in "${patches[@]}"; do
	patch -s -p1 -d "$tmp" -i "$patch"
done
if find "$tmp" -name '*.orig' -o -name '*.rej' | grep -q .; then
	echo "fetch.sh: a patch did not apply cleanly" >&2
	exit 1
fi
echo "$stamp" > "$tmp/.theo-patches"
rm -rf "$dest"
mv "$tmp" "$dest"
echo "opencore-aacdec: fetched into $dest with ${#patches[@]} patch(es)"
