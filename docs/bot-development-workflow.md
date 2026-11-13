# Reproduce a bot failure in one command

From the SDK repository, reuse your existing compatible Cargo cache:

```sh
export CARGO_TARGET_DIR=/absolute/path/to/existing/shared-target
export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0
cargo run --offline --locked -p minecraft-sdk-xtask --bin bot-dev -- \
  check latest-menu .state/dev-runs/menu-001
```

Choose a **new** destination for each run; parent directories are created for you.
The command launches a recorded-failure
worker, confirms its expected diagnostic exit, then runs the focused production
test through the actual SDK menu projection and inspection counter. It captures
each subprocess's stdout/stderr, elapsed time, exit/timeout, SHA256 hashes and
owned-process cleanup. The top-level `summary.json` links the two stages. Captured
menu snapshots and their hashes appear under `regression/artifacts/`.

Only reviewed recorded inputs and synthetic player identity/position are used.
No model, credentials, gateway, Minecraft server or world is needed. The first
run can compile SDK binaries; later runs reuse that cache. Startup compilation
of `bot-dev` occurs before its own timer starts; include Cargo's reported time
when comparing total developer wait. This is a development check, not a live
gameplay launch or proof that the bot completes crafting.

## The recorded failure

The 2026-11-13 E provider-recovery continuation ended without the full retained
table-and-pickaxe goal. After its last mutation, 20 model choices consumed a
summed 285,186 ms: 19 menu reads and one recipe query, with zero program runs.
The menu contained a birch button output, one plank input, three cursor planks,
two inventory planks and four sticks. These roles must stay separate.

The sanitized fixture preserves the 20 original tool inputs and elapsed durations,
observation revisions, and both deduplicated native menu pages. Query output is
deliberately omitted; the query's input and timing remain. Raw journal SHA256 and
source revision are recorded alongside the fixture. Identities, coordinates,
reasoning, credentials and private paths are excluded. The original failed log
remains private; regenerating the fixture is a deliberate review step, not an
automatic publication of live logs.

There are 17 repeated menu results. The first occurs at result sequence 367,
after 41,248 ms of recorded model time. The initial test incorrectly compared
observation revisions and found zero repeats; it failed before the semantic
comparison was implemented. The working diagnostic ignores observation revision
but retains menu/state IDs, page bindings, slots, permissions, components and
cursor. A new page or changed menu is different evidence. The recipe query does
not erase already seen menu pages. An execution/non-read boundary resets the
diagnosis without claiming that execution succeeded.

This establishes that the failure can be reproduced and diagnosed offline.
It does **not** establish that context size, model choice or any specific runtime
guard caused it, or that a faster replay makes the bot finish faster.

## Analyze an actual journal

```sh
cargo run --offline --locked -p minecraft-sdk-xtask --bin bot-dev -- \
  analyze /absolute/private/native.log
```

Input is increasing-sequence JSONL, bounded to 64 MiB and 1 MiB per event. Output
reports decisions, summed model time, missing timing, maximum read-only streak,
repeated menu results, read errors and the first repeated result. A zero-repeat
report means only that this diagnostic did not find identical menu pages. It
does not prove progress, success or the absence of another kind of loop.

## Capture a bounded launcher or test

```sh
cargo run --offline --locked -p minecraft-sdk-xtask --bin bot-dev -- \
  capture .state/dev-runs/scenario-001 120 -- /absolute/owned-launcher ARGUMENT
```

Arguments are passed directly, without shell interpolation. The child receives
`BOT_DEV_CAPTURE_DIR`, an absolute private artifact directory. A cooperating
launcher can write observation JSON, receipts or screenshots there. The runner
hashes up to 64 ordinary files/64 MiB, rejects symlinks from the artifact manifest,
and reports unsupported artifacts. It monitors stdout/stderr at an 8 MiB threshold
per stream every 10 ms; a fast writer can overshoot between checks. Raw files stay
private and are never auto-published. Command arguments and environment values
are not copied into the summary.

Failures, missing executables, interruption and timeouts return nonzero while
retaining evidence. Normal exit, timeout, SIGINT and SIGTERM clean up the child
process group and reap the direct child. Processes that deliberately detach
into another group, SIGKILL of the runner, or a host crash are outside that
cleanup guarantee. The runner never signals shared processes by name or port.

This wrapper does not provision a server, stage a world, acquire an actor, or
connect to an existing bot. Use a known isolated launcher with explicit ownership
for live tests. Offline `check latest-menu` is fully automated without those
resources. Raw artifacts are disposable once reviewed, except evidence worth
retaining for a failure; never delete source, saves or unrelated run history.

## Develop from evidence

1. Read the bounded failure once; state a hypothesis and a disconfirming check.
2. Add a minimal failing fixture against the production boundary involved.
3. Edit and run that focused test; expand to affected package checks after it passes.
4. Use an isolated native scenario only for behavior offline tests cannot prove.
5. Record source revision, cache state, commands, setup/build/test time and outcome.

For model-latency claims, compare matched tasks with the same acceptance criteria
and comparable starting states, source/model settings and budgets. Record model
calls/time, tool time, completion and human setup. Report failures too. Replaying
the recorded 20 decisions is useful evidence, but supplies no counterfactual
model choices and therefore no measured model-time saving.

## Measured development feedback (2026-11-13)

The final filtered-library command passed twice sequentially with an existing
compatible cache: 554 ms and 636 ms inside the runner, plus Cargo launcher time
of 40-50 ms. Each run reproduced the diagnostic and captured all 19 real SDK
menu projections, without inference or manual fixture setup. A run that rebuilt
dependencies took 44.835 seconds. A contended earlier run took 107.681 seconds;
two overlapping attempts were interrupted and retained as failed measurements,
not counted as cached successes. Cache state and other active builds matter.

The journal analyzer independently matched the original successful run's 80
main decisions / 1,029,636 ms, and the failed span's 20 decisions / 285,186 ms.
The cached command proves a short, reproducible development feedback path.
It does not prove fewer decisions or faster completion in a new live bot run.
Coordinate one compiler owner across chats sharing the cache; do not overlap
timing attempts or count build-lock waiting as cached diagnostic time.

## Verification

The workflow's tests cover the recorded 285.186-second stall, legitimate new
pages/changed menus, missing timing, invalid records, execution boundaries, real
SDK menu projection, failed process capture, absent executables, immutable output
destinations, automatic private parent directories, artifact hashes, private permissions, timeout and interruption
cleanup including an active descendant. Run them with:

```sh
cargo test --offline --locked -p minecraft-sdk-xtask -j 2 -- --test-threads=2
cargo test --offline --locked -p minecraft-sdk-brain --lib development_replay -j 2 -- --test-threads=2
cargo clippy --offline --locked -p minecraft-sdk-xtask --all-targets -j 2 -- -D warnings
```
