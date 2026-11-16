#!/usr/bin/env bash
# Builds the opencore-aacdec libFuzzer target with ASan and UBSan (t-169).
#
# Usage: build-fuzzer.sh CXX OUT_DIR [SOURCE_DIR]
#   CXX: a clang++ that ships libFuzzer (Homebrew or upstream LLVM; Apple's
#        clang has ASan/UBSan but no libFuzzer runtime).
#   SOURCE_DIR: the decoder sources, by default the fetched and patched
#        target/opencore-aacdec (another tree, e.g. unpatched, to check that
#        a regression input crashes it).
# Compiles the sources with the defines of the firmware build (root build.rs
# `opencore`), plus the bench shim and the driver, then links fuzz_aacdec
# into OUT_DIR. Keep DEFINES and SANITIZE in step with tools/aacdec-fuzz/build.rs.
set -euo pipefail
cxx=$1
out=$2
root=$(cd "$(dirname "$0")/../.." && pwd)
src=${3:-$root/target/opencore-aacdec}
harness=$root/tools/aacdec-fuzz/harness
defines=(-DAAC_PLUS -DHQ_SBR -DPARAMETRICSTEREO
	-DOSCL_IMPORT_REF= -DOSCL_EXPORT_REF= '-DOSCL_UNUSED_ARG(x)=(void)(x)')
# Negative left shifts (shift-base) are this fixed-point code's idiom:
# defined in C++20 and two's complement in GCC, which builds the firmware.
# Signed overflow wraps (-fwrapv, as in the firmware build, ADR-0027).
sanitize=(-fsanitize=address,undefined -fno-sanitize-recover=undefined -fno-sanitize=shift-base
	-fno-sanitize=signed-integer-overflow)
flags=(-g -O1 -fno-omit-frame-pointer -fwrapv -fno-exceptions -fno-rtti -Wno-narrowing -w
	"${defines[@]}" "${sanitize[@]}" -fsanitize=fuzzer-no-link "-I$src")
mkdir -p "$out/obj"
rm -f "$out"/obj/*.o
sources=()
for source in "$src"/*.cpp; do
	[ "$(basename "$source")" = SoftAAC.cpp ] || sources+=("$source")
done
sources+=("$root/tools/audio-bench/shim/theo_opencore.cpp" "$harness/driver.cpp")
jobs=$(getconf _NPROCESSORS_ONLN)
for source in "${sources[@]}"; do
	while [ "$(jobs -rp | wc -l)" -ge "$jobs" ]; do wait -n; done
	"$cxx" "${flags[@]}" -c "$source" -o "$out/obj/$(basename "$source" .cpp).o" &
done
while [ "$(jobs -rp | wc -l)" -gt 0 ]; do wait -n; done
objects=("$out"/obj/*.o)
[ "${#objects[@]}" -eq "${#sources[@]}" ] || { echo "build-fuzzer: some sources failed to compile" >&2; exit 1; }
"$cxx" -fsanitize=fuzzer,address,undefined "${objects[@]}" "$harness/fuzz_aacdec.cpp" -o "$out/fuzz_aacdec"
