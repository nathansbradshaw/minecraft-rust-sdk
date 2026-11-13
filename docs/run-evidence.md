# Operator-side run evidence

`mc-sdk-evidence` captures selected authoritative action rows directly from an
operator-owned gateway SQLite journal, using a read-only connection and one
consistent transaction. It never opens the gateway's writer Store, migrates a
database, connects a player or performs gameplay. Run it outside bot containers,
as the gateway's Linux account. It has no public HTTP/MCP endpoint.

```text
mc-sdk-evidence capture GATEWAY.sqlite PRIVATE_SPEC.json NEW_PRIVATE_REPORT.json
```

The journal and specification must be private regular files owned by the caller.
The output is a new mode-600 file; overwrite and symlink destinations fail.
Keep reports private: native receipts include world/player state and chat may
appear in the separately bound controller trace. Never publish those artifacts
with reusable source or install packages.

The specification binds a run/goal, server instance, player UUID, full source
commit, controller/program/source/trace SHA-256 hashes, model request counts,
labelled resource measurements, environmental assists, interventions, failures
and expected typed action IDs/intents/outcomes. Read the `Spec` Rust structure for
its closed JSON schema. Hash actual artifacts independently; measure resource
usage before/after runs where deltas matter. An end-of-run snapshot is not peak
memory or per-run CPU usage. Preserve unmeasured values explicitly as null.

Capture resolves the actor from the authoritative identity registry and compares
every requested action with that actor's real journal row. Unknown IDs, other
actors/instances, changed intents and different outcomes fail. `observed` also
requires the authoritative native-effect flag and no error. Reconciled state is
recordable but contributes zero observed effects.

This is narrow evidence capture. Native rows come from the operator-selected
gateway journal. Source/build/model/resource metadata and interventions remain
operator attestations, not independently verified claims. The tool does not
certify that a candidate executable ran, that an overall goal succeeded, that
the run occurred at a particular time, or that an agent is competent. Inspect
controller and trusted inference logs, binaries and native observations
separately. A journal controlled by a bot is not an authoritative input. A full
benchmark still needs its frozen conditions, independent outcome checks,
complete failure accounting and comparison procedures.

For finite movement, the initial receipt also carries diagnostic dispatch
phases. `gateway_dispatch_timing` separates character-lane admission, refreshing
the previous control, durable intent preparation and the adapter call.
`adapter_dispatch_timing` separates the native observation from trajectory
planning. These durations use local monotonic clocks; never subtract timestamps
from different hosts. The gateway timing ends before its final durable receipt
save and excludes HTTP transport. The adapter timing excludes packet delivery
and game-effect verification. A `running` receipt still does not prove movement.

The reference body's `body_reaction.reaction_ms` measures local observation
receipt through the returned start-control acknowledgement, including policy
and round-trip dispatch. Retain that measurement and its failed thresholds
independently of eventual native movement, survival and final health. Diagnostic
phase fields explain a delay; they do not turn an acknowledgement into a verified
effect or excuse a failed acceptance limit.

NeoForge integration alpha.7 adds `minecraft:native_effect_clock`. Each native
snapshot includes `world.sample_clock` with a JVM epoch and unsigned nanoseconds
encoded as a string. Finite controls sample up to three native observations on
a separate bounded worker after their first submitted frame. A verified
`first_effect` must match a frame already submitted for that character and
dimension, with the same clock epoch and no intervening server correction.
Movement submission continues while sampling. Terminal publication waits for
the bounded worker, preserving its evidence in the authoritative receipt.
First movement and final target verification remain separate facts.

With `minecraft:life_identity`, the clock epoch also contains the current native
player life. It changes on respawn even when the numeric entity ID and connection
are reused. See [ordinary respawn recovery](respawn-recovery.md) for compatibility,
life fencing and the separate live acceptance procedure.

```text
mc-sdk-evidence reaction-latency PRIVATE_TRACE.jsonl NATIVE_REPORT.json NEW_SUMMARY.json
```

Capture every attempted movement ID in the native report and bind it to the
complete trace. Capture accepts at most 128 selected IDs; the existing 64 KiB
manifest and 8 MiB report budgets still apply. Exceeding a budget fails rather
than dropping attempts. The Rust summary reports acknowledgement timing separately
from a conservative physical-reaction bound: the native observation sample
precedes local receipt, and its same-JVM clock is compared with the first
verified movement sample. Missing, rejected, unverified or incompatible-clock
effects stay visible; never omit them to pass a limit. Clock arithmetic retains
integer precision and handles unsigned wrap. Different epochs cannot be compared.
Older integrations continue gameplay but cannot supply this measurement.

Native effect clocks are server evidence; controller observation clocks remain
operator-attested through source/build/trace binding. This is not a cryptographic
attestation of observation delivery or a competence benchmark. Declare the
measurement method before a trial, preserve earlier acknowledgement failures,
and report both metrics alongside deaths, outcomes and interventions.

## Observe an allocated compiler process tree

`mc-sdk-workload-observer` is a Linux operator binary in the evidence crate.
It reads only an allocated root process and its current descendants, without
starting, signalling or keeping workloads alive. All options are required:

```text
mc-sdk-workload-observer --root-pid PID --root-start-jiffies START_JIFFIES \
  --seconds SECONDS --interval-ms INTERVAL_MS --expected-rustc ABSOLUTE_EXECUTABLE \
  --source-sha256 CALLER_DECLARED_SHA256
```

Bind the root to field 22 of its `/proc/PID/stat` in the same PID namespace;
never select a shared container root containing unrelated work. The total budget
is 1–600 seconds including preflight/output, with the final 100 ms reserved for
completion records; sampling cadence is 25–50 ms. The expected executable must be an
independently reviewed regular executable on a read-only filesystem mount.
Canonical path and device/inode/metadata establish the internal comparison;
file permissions alone do not satisfy the mount check. Records contain only
`exe_matches_expected`, process identities, CPU ticks and UTC sample brackets,
without executable paths, command names, arguments or environment. Redirect stdout
to one private pipe/file ledger; terminals are refused. No per-process files or
endpoints are created.

Exit, root PID reuse and inaccessible root state stop sampling. Traversal and
historical identities are bounded; inaccessible/absent observations, races,
limits, partial trees, clock regressions and actual sample gaps remain explicit.
Missed periods are skipped. Nonblocking writes/poll use the remaining monotonic
budget and terminal cycle/gap measurements include output time. An **external
hard timeout targeting only the observer PID is required**, because regular-file
or kernel I/O can still block. For GNU `timeout`, use `--foreground --signal=KILL`
with the same seconds budget; never signal the workload or its process group.
Require exit 0, complete `observer_finished` and `observer_output_complete`
records, and external wall time within budget. Missing/truncated completion,
timeout or output failure means incomplete evidence. The trailing record measures
post-summary elapsed time, final sample gap and overshoot; it is not a substitute
for the external deadline. Apply declared sampling-gap limits separately.

Matching samples establish executable presence at sampled instants. Inspect CPU
counters alongside each sample's executable match: aggregate first/last counters
span the whole process identity, including nonmatching or inaccessible intervals,
and cannot establish rustc-only CPU use. Executables may also change between
samples; even matching endpoints do not certify exclusive execution in between.
Short processes may be missed. Independently retain observer
source/build identity and hash the actual program before and after: the supplied
SHA is caller declared and does not certify compiler inputs. Join native effects
and accepted model-request evidence using conservative timestamp bounds. Cargo
success, global model-busy counts and blocked callers alone do not prove actual
rustc work, an allocated model request, or precise GPU inference overlap.
