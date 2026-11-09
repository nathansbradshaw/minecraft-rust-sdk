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
4 MiB limit and supplies automatic source only up to 4 KiB. It does not guess a
source filename, execute Cargo metadata/build scripts or ask the inference host
to read files. A complete explicit Rust source read is also accepted. Paths must
resolve within the bot workspace; source contents, file hash, workspace hash and
goal revision are checked again before selecting the transition. Changed,
partial, foreign or unavailable source leaves ordinary tools available.

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
