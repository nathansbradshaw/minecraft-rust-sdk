# Bot development workflow

- Use Rust for code, tools, test fixtures and automation. Preserve unrelated
  work, worlds, credentials and active controllers. Never infer gameplay success
  from compilation, a process exit, replay or model claims.
- Begin a bot failure investigation with one bounded evidence read. State the
  hypothesis and the smallest command that could disprove it, then run it. Batch
  independent reads. Reread unchanged source/menu/log evidence only to answer a
  named missing fact; advancing observation revisions alone are not new evidence.
- Reproduce behavior offline before live retesting. Prefer a small captured
  fixture against production code. Retain the original failed evidence locally;
  publish only reviewed, sanitized fixtures with provenance and explicit limits.
- For the latest recorded menu stall, run the one-command check documented in
  `docs/bot-development-workflow.md`. Its source is `crates/xtask/src/workflow/`;
  `crates/brain/src/development_replay.rs` exercises real menu projection.
- Follow the verification ladder: focused regression, affected package checks,
  then a bounded integration scenario when needed. Repeat checks only after a
  relevant change or failure. End inspection loops by running a discriminating
  check, editing a candidate, or recording a concrete blocker; avoid repeatedly
  re-planning the same diagnosis.
- Use `bot-dev capture` for owned bounded launchers. Every run gets a new private
  directory, stdout/stderr, hashes, duration, timeout/exit status and process-group
  cleanup. It does not stage a world or acquire an actor. Coordinate exclusive
  ownership before live use; do not restart shared services or reuse another
  controller's actor. Keep raw evidence under ignored `.state/` or outside source.
- Reuse the existing compatible Cargo target; disable incremental compilation
  for disposable checks. Check free space before builds and retain at least
  500 GB free. Delete only owned, regenerable temporary output, preserving useful
  failure evidence. Never make a full checkout or target per test.
- When chats share a Cargo cache, agree on one compiler owner and queue builds.
  Do not launch overlapping timing attempts or interrupt another owner's build.
  Report cache waits separately from cached diagnostic time.
- Record setup, build/test, capture and model timing separately. Missing model
  timing is unknown, not zero. Offline replay establishes reproducibility and
  diagnostic speed; claim fewer model calls or faster bot completion only after
  a matched live comparison with unchanged acceptance criteria.
- Check account usage before substantial work and between batches (about five
  minutes). Stop at 15% remaining unless the user explicitly authorizes reserve
  use for this task. Missing usage data permits authorized work to continue.
- Complete verification, commit, push and merge authorized implementation into
  `main`, respecting required checks. Preserve other branches and recovery history;
  never force-push or bypass required approvals. State any concrete blocker.
