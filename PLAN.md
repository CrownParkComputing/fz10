# Plan

## State (2026-11-21)
IP cleanup (asset removal + provenance docs) done in working tree, uncommitted.
Build clean (`cargo build --release`), tests pass (28/28, `cargo test --bin retro3do-gui`).
README had a stale "Contains unresolved items" note contradicting THIRD_PARTY.md — fixed.

## Milestone 1 — commit the cleanup (DONE)
- Fix README stale THIRD_PARTY note.
- Verify build + test on the current tree.
- Commit: asset removals, cpc-mark.png, case.glb regen, docs (AUDIT/PROVENANCE/SOURCES/THIRD_PARTY/CASE-ARTWORK), LICENSE, launch-road-rash.sh.

## Milestone 2 — fresh-history publish decision (needs user call)
PROVENANCE.md says the removed third-party assets remain in git history, so
publishing requires a fresh initial commit (history rewrite), not a push of
current history. This is irreversible/destructive — do not do it without
explicit confirmation. Ask the user when they want to publish.

## Milestone 3 — ongoing
- Keep `docs/task-progress.md` as the running log (already has entries through
  item 8, AI Deck registration).
- Re-run `docs/audit.sh` if more third-party-sourced files are ever added.
