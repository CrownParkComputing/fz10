#!/usr/bin/env bash
# Re-runs the provenance audit documented in docs/AUDIT.md.
# Exits non-zero if any check fails. Run from the repository root.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

fail=0
check() { # name expected actual
  if [ "$2" = "$3" ]; then
    printf 'pass  %s\n' "$1"
  else
    printf 'FAIL  %s (expected %s, got %s)\n' "$1" "$2" "$3"
    fail=1
  fi
}

SRC=(app/src app/examples app/tests android/src)

check "1 no FreeDO notices or authors" 0 \
  "$(grep -rilE 'freedo|troosh|grishin|lazarev|sammons|allen wright|opera-libretro' "${SRC[@]}" 2>/dev/null | wc -l)"

check "2 no opera_* identifiers" 0 \
  "$(grep -rnE '\bopera_[a-z0-9_]+' "${SRC[@]}" 2>/dev/null | wc -l)"

check "3 only our own C interface files" 2 \
  "$(find app android \( -name '*.c' -o -name '*.cpp' -o -name '*.cc' -o -name '*.h' \) 2>/dev/null | wc -l)"

check "4 no build scripts linking C" 0 \
  "$(ls app/build.rs android/build.rs 2>/dev/null | wc -l)"

rs=$(find app/src android/src -name '*.rs' 2>/dev/null | wc -l)
tests=$(grep -rc '#\[test\]' app/src app/tests 2>/dev/null | awk -F: '{s+=$2} END {print s+0}')
printf 'info  %s Rust files, %s #[test] functions\n' "$rs" "$tests"

exit "$fail"
