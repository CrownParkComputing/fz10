# Plan

## State (2026-11-21)
Milestone 1 (IP cleanup commit) landed in `08f9e0b`; tree is clean, 24 commits
ahead of `origin/master`.
Build clean (`cargo build --release`), tests pass (28/28, `cargo test --bin retro3do-gui`).

## Milestone 1 — commit the cleanup (DONE, committed as 08f9e0b)
- Fix README stale THIRD_PARTY note.
- Verify build + test on the current tree.
- Commit: asset removals, cpc-mark.png, case.glb regen, docs (AUDIT/PROVENANCE/SOURCES/THIRD_PARTY/CASE-ARTWORK), LICENSE, launch-road-rash.sh.

## Milestone 1b — multi-platform Rust frontend check (DONE, 2026-11-21)
Tester feedback asked to "bring the new frontend rust in as part of project,
multi-platform". Checked: it already is. `app/` is the single egui/eframe
core; desktop links it against SDL3 for audio/pads (`retro3do-gui`), `android/` links the same core as a cdylib
(`retro3do-android`, no SDL3) loaded by `Fz10Activity`. Both are tracked in
git, not just on disk.
- `cargo test --manifest-path app/Cargo.toml --bin retro3do-gui --release`: 28/28 pass.
- `android/build-apk.sh release`: clean, produced
  `android/build/retro3do-fz10-release.apk`.
Nothing to merge in — no gap found.

## Milestone 2 — fresh-history publish decision (needs user call)
PROVENANCE.md says the removed third-party assets remain in git history, so
publishing requires a fresh initial commit (history rewrite), not a push of
current history. This is irreversible/destructive — do not do it without
explicit confirmation. Ask the user when they want to publish.

## Milestone 3 — ongoing
- Keep `docs/task-progress.md` as the running log (already has entries through
  item 8, AI Deck registration).
- Re-run `docs/audit.sh` if more third-party-sourced files are ever added.
