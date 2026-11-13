# Repair after a run

Process success and gameplay success are separate. A Rust program can exit zero
after a rejected move, expired interaction or delivery of historical receipts.
The reference brain supplies an explicit repair transition when the recorded
experiment, active goal and current workspace still match.

Compiler repair retains its existing source/diagnostic guards. Gameplay repair
requires a `run` or gameplay-bearing `terminal` experiment with complete local
action-trace accounting and a definite rejected/expired control, an explicit
invalid-request/capability/intent-conflict refusal, or an entirely historical set of intended
mutations. Fresh successful actions, status-only runs, chat/look-only traces,
missing records and pending/uncertain controls do not force this transition.
An uncertain mutation must be inspected or reconciled, never blindly replayed.

Intervening observations or SDK reference reads do not hide an unchanged failed
program. A report leaves ordinary tools available. When context compaction has
omitted action rows, the worker can recover the exact matching experiment from
the same character's private journal in read-only mode. Goal, source signature,
output path/hash and full trace accounting must agree. Truly missing records
remain unknown. The bounded failure view prioritizes rejected controls over
successful setup actions, while the complete private trace remains available.

The container-local worker can recover Cargo's actual binary entry from the
hash-checked private stdout capture. It reads no more than the stream's existing
4 MiB limit and supplies automatic failure-repair source up to 4 KiB. It does not
guess a source filename, execute Cargo metadata/build scripts or ask the inference host
to read files. A complete explicit Rust source read is also accepted. Paths must
resolve within the bot workspace; source contents, file hash, workspace hash and
goal revision are checked again before selecting the transition. Changed,
partial, foreign or unavailable source leaves ordinary tools available.

Active coding sessions can retain one complete `active_program_source` while
later tools read SDK documentation. It reuses the same Cargo-entry and raw-output
hash checks, requires the unchanged workspace and goal revision, and adds no
model calls or development turns. The active source is limited to 8 KiB and its
complete serialized view to 12,000 bytes; existing failure repairs retain their
4 KiB default. The explicit engine state-directory exclusion must match the
retained capture location, so nested state changes cannot masquerade as source
changes. Existing verified repair views for the same entry take precedence;
an optional source view that cannot fit the context budget is omitted. This does
not classify successful no-op programs as failures or certify gameplay progress.

For the structured `run` tool, the stdout preview hides recognized Cargo prelude
records and keeps output after `build-finished` before the ordinary bounded
head/tail preview, with bounded warnings and preview truncation labelled
in `cargo_output_view`. Compiler errors still come from the complete raw stream.
The saved stdout/stderr bytes and hashes are unchanged, including the artifact
records used for entry recovery. Generic `terminal` output is unchanged.

Accepted model usage includes a bounded `context_attestation` for the final
serialized request context. It repeats `context_sha256` and records only source
hash/byte/completeness checks plus the actual `sdk://reference` page hash and
range. It emits no source, prompt or private path text. This can establish that
complete source and a reference page coexisted in one accepted request; it does
not establish model understanding, code execution or gameplay success.
`last_model_context` is saved before model preparation and cannot alone prove
that the optional active-source field reached that request.

The model receives the actual errors/outcomes and must author an exact source
edit or report the specific missing fact/interface. Edits retain the normal
compare-and-swap hash guard. Changing request IDs alone cannot fix invalid
geometry or expired interaction sequencing. A changed program must compile,
execute and verify its effects; no skill reliability or task success is awarded
by this transition. Local traces are diagnostics, not authoritative game evidence.

The existing model allocation and shared inference/compilation budgets apply.
The independent body and conversation continue during reasoning and development.
`repair-probe PRIVATE_CONTEXT.json` inspects applicability without inference,
gameplay or source writes. Native retained progress remains a separate gate.

# Exhausted navigation

Two exhausted routes within three blocks, with the same goal, source, anchored
local terrain and inventory, open a coding checkpoint. The worker supplies the
complete unchanged conventional `src/main.rs` (up to 4 KiB) for a model-authored
edit or a specific missing-interface report. Other layouts keep ordinary tools.
The current source is a candidate, not evidence that it caused the route failure
or compiled. The model still chooses the method and must compile, execute and
verify it. New goals, changed source, changed anchored terrain/inventory, leaving
the area, or pending intentions prevent the checkpoint. This adds no model
allowance, gameplay authority, mandatory digging policy or task-success credit.

## Local development result

A minimal NeoForge 1.21.1 Survival diagnostic at `3f19901` retained one newly
mined oak log after the model escaped a locally exhausted route area. The native
inventory gain occurred 343 seconds into the final episode. It made 35 decisions,
three source edits and three Rust runs, with 37 Luna admissions including chat
and zero Sol admissions. The configured backend was `gpt-5.6-luna`, with high
development and low routine reasoning. A normal player status follow-up elicited
an accurate inventory report. The character disconnected at the 480-second cap
with the log saved, 20 health, and no native damage or death increments.

This is an operator-attested development observation, not a public benchmark.
Two preceding failed log diagnostics and intervening engineering deployments
remain part of its history. Starting inventory already included a wooden pickaxe,
crafting table, sticks, one plank, one cobblestone and two dirt blocks. No route,
gameplay script, items, healing or rollback was supplied during the task. The
quiet episode establishes neither empty-start progression nor hostile Survival
competence, and does not isolate the contribution of individual changes.

## Bounded provider-error investigation

The direct controller can investigate a typed provider 5xx once through the
other allocated model. Eligible classes are `provider_failed`,
`provider_unavailable`, `provider_timeout`, and `model_unavailable_no_retry`.
This changes the context and model; it does not resend the failed request.
Authentication, schema, permission, quota and opaque worker errors do not grant
this transition. Existing broker admission, transport and daily limits apply.

The journal consumes the episode before transport. A consumed episode without
an accepted answer remains blocked after restart. An accepted answer pins
ordinary work to that healthy role until the goal, editable workspace source,
worker or request contract changes. Ambient observations do not reset it; an
unavailable healthy allocation cannot fall back to the known failed role.
Any later nonquota error blocks that episode. No allowance is restored.

Automatic recovery requires a separate state directory so journal activity is
excluded from the source binding. A legacy configuration with state equal to
the workspace keeps responsive observation and explicit blocked reasoning;
journal writes cannot turn into automatic model retries.

Inspect `provider_investigation_reserved`, `provider_investigation_result`,
`provider_investigation_failed`, and `provider_investigation_blocked` events.
Reservation records the original typed failure and consumed lease. Tests cover
the real direct-controller helper, role gate and local HTTP transport, including
SDK refresh during inference; they do not establish provider or gameplay success.
