# Audit: what third-party emulator source is present

Audit date: 2026-11-21. Commit: see `git rev-parse HEAD`.

This file exists so that the claim "no other emulator's source is in here" is
checkable by someone who does not trust us. Every check below is a command you
can run against a fresh clone. The recorded results are the real output of
those commands on the audit date.

This audit covers **what is present in this repository**. It does not and
cannot speak to historical knowledge exposure, which is a different claim and
is documented separately and unfavourably in [PROVENANCE.md](PROVENANCE.md).
Read both or neither.

## Check 1 — no FreeDO/Opera notices or authorship

The FreeDO licence notice names its authors. Any file carrying FreeDO-derived
code carries that notice, or at minimum those names.

    grep -rilE 'freedo|troosh|grishin|lazarev|sammons|allen wright|opera-libretro' \
      app/src app/examples app/tests android/src

Result: **0 matches.**

## Check 2 — no `opera_*` identifiers

opera-libretro prefixes its public functions and types `opera_`. A port or
paste leaves those names behind, even after reformatting.

    grep -rnE '\bopera_[a-z0-9_]+' app/src app/examples app/tests android/src

Result: **0 matches.**

Note: the word "Opera" does appear in prose in `app/src/xbus.rs`, where it is
The 3DO Company's own codename for the hardware. See the "On the word Opera"
section of [PROVENANCE.md](PROVENANCE.md).

## Check 3 — no C or C++ translation units beyond our own interface

opera-libretro is C. If any of it were vendored here it would be C files.

    find app android -name '*.c' -o -name '*.cpp' -o -name '*.cc' -o -name '*.h'

Result: exactly two files, both Crown Park Computing's own:

- `app/include/retro3do.h` — our public C ABI header
- `app/tests/c/abi_c_consumer.c` — our smallest C consumer, used to test that ABI

## Check 4 — no C is compiled or linked

A `build.rs`, a `cc` crate dependency or a `*-sys` crate would be how C got
pulled in without appearing in the tree.

    ls app/build.rs android/build.rs
    grep -rn 'cc::|bindgen' app/Cargo.toml android/Cargo.toml

Result: **no build scripts, no `cc`, no `bindgen`.** The CHD codecs are the
pure-Rust `miniz_oxide`, `lzma-rs` and `ruzstd` crates; libchdr itself is not
linked. See [THIRD_PARTY.md](THIRD_PARTY.md).

## Check 5 — the implementation's size and test coverage

Independent reimplementation should look like independent work: its own
structure, its own tests.

    find app/src android/src -name '*.rs' | wc -l
    find app/src android/src -name '*.rs' -exec cat {} + | wc -l
    grep -rc '#\[test\]' app/src app/tests

Result: **52 Rust files, 27,903 lines, 393 `#[test]` functions.**

## What these checks do and do not establish

They establish that no opera-libretro, FreeDO or 4DO source code is present in
or linked into this repository. That is the question of literal copying, and
the answer is no.

They do not establish that this is a clean-room implementation, and nothing in
this repository should be read as claiming that. The implementation was ported
from Crown Park Computing's own C++ core, whose own development did involve
reading Opera code as a compatibility reference. That history is set out in
[PROVENANCE.md](PROVENANCE.md), which also folds in the provenance record that
used to live in the now-removed `cpp-reference/` tree. Anyone assessing this
project should read it too.

## Re-running everything

    bash docs/audit.sh

The script runs all five checks and prints pass/fail per check. If a check
starts failing, that is information, not a bug to be silenced.
