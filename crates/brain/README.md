# Experimental container-local coding brain

This Rust harness observes through the public SDK and gives Luna/Sol a real tool
loop: inspect files, generate Rust, compile/run, act, inspect outcomes and repair.
It runs only in a non-root container. The trusted inference service returns data
and source; it never receives host tool execution permission. SDK gateway/server
credentials and authoritative journals remain outside the container.

Build `cargo build --locked -p minecraft-sdk-brain` and run inside the isolated
coding container:

```text
minecraft-sdk-brain run /workspace/brain/config.json
```

The private mode-600 JSON config supplies `profile`, `model_key`, `model_endpoint`,
`workspace`, `state`, `goal`, optional `cargo_target`, `max_turns` (default 64),
`tool_timeout_secs` (240), `model_interval_secs` (31), and `exit_on_complete`
(false). Paths and endpoint are deployment configuration, never compiled server
addresses. Use the existing per-bot Cargo cache/target where compatible; do not
create duplicate caches for each command. Model credentials must be a separately
allocated per-bot key accepted by a compatible HTTPS inference proxy. SDK public
registration does not grant model access.

New player tasks start with Luna; it can escalate to Sol through `develop`.
The proxy remains authoritative for daily allowances. Its operator usage report
must include legacy admissions whose model identity is unknown; these are charged
conservatively against each model ceiling. Do not infer remaining allowance from
identified Sol/Luna rows alone or reset a ledger to continue an evaluation.
Explicit `allocation_daily_limit` and `allocation_pacing` responses may supply a
bounded `retry_at_utc`. The brain waits for that clock event while its body and
other model lane remain available. Conversation retains unacknowledged input for
that retry. Authentication, schema, queue, provider and clock-rollback failures
require investigation rather than unchanged automatic requests.

`model-probe HTTPS_ENDPOINT ALLOCATED_KEY routine|develop LABEL` checks an
allocated inference capability without connecting a character or executing a
returned tool. Record its actual admission and outcome; it proves inference,
not gameplay intelligence. The key stays in its private local file.
The operator can run `model-probe watch http://LOOPBACK:PORT/health SAMPLES`
outside the bot containers to sample the independent inference service's `busy`
counter. It accepts1–240 samples, checks at250ms intervals and fails above two
active jobs. Correlate it with actual completed requests; an idle sample alone
does not verify concurrent operation or remote provider/GPU utilization.

Terminal builds default to one job, no incremental compilation or development
debug information, and development debug symbols are stripped. Keep the harness
executable outside disposable Cargo output before cleaning that output. Use one
compatible profile/cache in a storage-limited workspace; building release and
development dependency trees simultaneously may exceed a deployment's budget.
The shared development profile optimizes this brain package's reactive geometry;
it leaves generated bot programs and dependency profiles unchanged. Body reaction
records separate policy, dispatch and observation durations so a transport stall
cannot be mistaken for a planning improvement.

The workspace should contain an ordinary Rust Cargo project with the SDK and
Tokio dependencies and an appropriate dependency cache/vendor configuration.
The supplied coding image includes Rust `rg` and sets `MC_SDK_SOURCE` to its
readable SDK tree. An operator using an editable workspace copy should set this
environment variable to that copy when launching the harness. This is a source
discovery path inside the container, not a host-directory mount or permission.
The harness sets `MC_SDK_PROFILE` for programs, with the already-connected private
profile. Programs can use `Client::with_profile`, `observe`, and typed actions;
they must preserve action IDs and inspect receipts instead of replaying uncertain
mutations. No private companion format is required by the harness.

The terminal's output, execution time and memory retained for results are bounded.
It drains ordinary compiler output instead of breaking the compiler's pipes after
the displayed excerpt. Larger output floods fail. Linux container/kernel budgets
remain the security boundary; workspace path checks are an accidental-write guard,
not a sandbox against arbitrary terminal code. Keep host networking, Docker
sockets, private directories and personal credentials out of the container.

`brain.sqlite` retains source-labelled facts/reports/hypotheses/episodes/skills,
tool results and action receipts. One local file lock fences duplicate harnesses
using the same state directory; operators must still stop any legacy controller
before assigning the same character. The gateway enforces controller identity.
The harness records its own pending action before dispatch and offers explicit
status/reconciliation tools. Current state does not establish a historical effect.
Search includes older journal events and action receipts, so a short context
window does not prevent the agent retrieving its actual action IDs. Source
editing uses separate path and UTF-8 content fields without nested JSON source
encoding. Completed tool activity persists across reasoning turns.
The latest tool result has a separate context budget so ordinary source output
is not silently replaced by its abbreviated history. `minecraft-sdk-brain
context STATE_DIRECTORY` prints the last exact model context for local debugging;
keep that output private because it includes observations and player conversation.

Finite terminal jobs clean up their process group, including background children.
Normal completion, run-budget exit and handled termination disconnect the SDK
character rather than leaving it unattended. Abrupt process/container loss still
needs deployment supervision; this is not a proven background Survival controller.

Model `complete` is recorded as a claim with an observation. It cannot certify an
evaluation or evidence gate. This initial harness has not established autonomous
Survival, comprehensive chat delivery, transfer, endurance, fair player-like
perception, or competitive performance. Those need their own integration and
native evaluations. The current observation projection inherits the installed
SDK provider's perception; it must not be presented as a player-visibility proof.

All model/tool source is MIT. Do not include generated worlds, keys, personal bot
memory or deployment journals in a release or source publication.

`control-latency-probe PRIVATE_CONFIG.json` is a model-free native diagnostic for
an otherwise inactive test character. It selects one observed flat, clear walking
route, compares idle observation latency with a concurrent observation during the
walk, records the native receipt and disconnects. It does not establish Survival
competence or a latency percentile. It fails rather than inventing missing terrain
or treating unverified movement/disconnection as successful. Run it only after
stopping any other controller for that character.

`control-latency-probe PRIVATE_CONFIG.json watch SAMPLE_COUNT` instead collects
1–120 own-character observations without movement or model calls. Each sample
records its Unix send time, duration, UUID and revision; the summary reports the
measured latency distribution. Run separate instances in separate bot containers
to inspect cross-character progress during another character's long action.
Each process has only its own profile, and disconnects on exit. An observation
failure is retained and fails the diagnostic; it never silently reconnects or
replays work. Operators can use that behavior in a declared socket-failure test,
then start a separate diagnostic to prove reconnection. These are native
transport measurements, not evidence of tactical behavior or survival.

`control-latency-probe PRIVATE_CONFIG.json finite` checks the optional finite
locomotion capability: five walks with concurrent observations, a concurrent
look, duplicate IDs, ground/jump cancellation, expiry, a completed jump and an
interrupted control followed by same-identity reconnect and explicit
reconciliation. It uses no models and refuses missing observed flat routes.
Its initial observation boundary is measured p95 <=500 ms and maximum <=1 s.
All receipts and failures stay in the private brain journal. The character must
otherwise be inactive. It does not prove defensive decisions, eating/combat,
claim denials or full Survival physics.

Build with a toolchain and libc compatible with the runtime container. A Linux
host build can acquire newer glibc symbol requirements even when source checks
pass. `Cargo.container.toml` defines a minimal three-crate workspace for compiling
inside the existing coding image. Reuse its per-bot Cargo target and reviewed
vendor configuration; do not download another toolchain or dependency bundle.

The default `body` configuration is `{ "enabled": true, "period_ms": 150 }`.
It requires finite locomotion, public survival facts and pending-action inspection.
The current body also requires `minecraft:ranged_observation`: visible entities
within the installed observation radius include ranged-attack classification and
visible hand item identifiers/animations. It distinguishes a ranged threat's
24-block retreat boundary from the melee boundary, values observed cover, and
checks safe one-block descents instead of treating a step as a diagonal ramp.
These are replaceable client behaviors; they are not SDK gameplay policy or proof
that every ranged mod entity, terrain trap or projectile is supported.
Terminal children inherit `MC_SDK_ARBITER` and run at reduced CPU priority so the
independent body can preempt gameplay while compilation continues. Use finite
`start_move` and inspect its eventual receipt. Blocked code requests can resume
after danger clears; they do not retain a character-wide action lease.

The `conversation` option defaults to true and requires the responsive body.
Conversation output explicitly chooses `goal_action: "continue"` or `"replace"`.
Continuation leaves goal revisions, development mode, completion and inspection
state intact, even if a model repeats the goal text. Repeating an active goal is
also continuation; a fresh request for the same completed task creates new work.
Replacement is conditional on the revision seen by conversation inference, so
a delayed reply cannot overwrite a newer task. All delivered chat still reaches
conversation context, including join/leave notices and ordinary player requests.
If a new-task proposal became stale, its input remains pending for reconsideration
against the changed revision; an unaccepted proposal sends no acknowledgement.
Acknowledging a gameplay request in chat does not execute it: accepting new work
must hand a concrete goal to the engine. To iterate conversation instructions
without replacing a connected controller, set `MC_SDK_CONVERSATION_PROMPT` to a
container-local UTF-8 file. It is read on each conversation request, with a
16 KiB limit and explicit read/validation errors. Without it the compiled prompt
is used. Usage records include the actual prompt hash. Editing this client text
cannot change SDK authority, host access or inference allocation.

`model-probe ENDPOINT ALLOCATED_KEY conversation LABEL PRIVATE_CONTEXT.json`
replays a bounded conversation context through the actual allocated Luna service
without a game connection or tool execution. Its output is reasoning evidence;
verify the installed controller and real gameplay separately.
It uses Luna only when new chat arrives and runs independently of Sol and terminal
work. `await_chat_goal: true` waits for a player-supplied goal and waits again after
completion, instead of spending inference on an unchanged idle condition. Its
separate conversation journal preserves reports, replies, cursor and model errors.
The trusted model service must allow one pending job per bot/model while retaining
its global jobs and per-bot daily ceilings. No additional allowance is created.
Routine Luna planning and Luna conversation share a local permit, so concurrent
callers wait inside the container rather than colliding at that broker limit.
Sol uses a separate permit. Waiting for either model leaves body observations
and gameplay active; a returned error releases the permit before investigation.

`body-probe PRIVATE_CONFIG.json blocked|compile SECONDS` is a declared native
execution diagnostic. Blocked mode injects a stalled reasoning future and calls
no provider; compile mode runs the real offline release compiler and records its
actual duration and failures. Both keep the body active and disconnect on exit.
`peer-probe PRIVATE_CONFIG.json chat|command MESSAGE SECONDS` provides an ordinary
non-operator SDK peer for native recipient/chat tests. Neither certifies competence.
Use separate private state directories and record initial conditions, real threat
fixtures, health/deaths, latency, interventions, source and binary hashes.

Model context preserves numeric position/velocity/collision vectors and the
nearest geometry/threats when compacting large observations. A short SDK reference
and factual `notes` persist API knowledge between calls; the original player
request accompanies the shorter goal so task constraints are retained.
Compact observations use the same `revision`, `player`, `world` and `menu`
structure as SDK observations, including `world.blocks[].position`; compaction
does not rename fact fields. The context labels its omitted data and retains
original inventory/menu slot IDs and chronological recent chat. Read fresh full
SDK observations for complete local geometry or menu slots. Earlier journal
views remain intact and may have older compact field names.

A changed observation/documentation contract reopens a stalled inspection once,
without replacing its goal, clearing pending action IDs or changing inference
allowances. Restarting with the same contract does not reset that cost guard.

`max_inspections_without_execution` defaults to eight (deployment range 1–64).
The first unchanged inspection of a new goal counts toward that ceiling.
At that ceiling or a tool failure, Sol gets a bounded investigation phase
(`investigation_turns`, default4, range1–8). It receives current observations and
`last_experiment`, retained across subsequent file/log inspections. Terminal
choices mark compilation/execution with `experiment: true`; existing callers
without that field decode as false. Local-arbiter gameplay also identifies an
experiment, independently of that declaration.

Terminal feedback includes typed local action requests, current receipt outcomes,
preserved IDs and inventory-count deltas. Pending receipt refresh is read-only
and bounded to two seconds; it never reconciles or replays a mutation. The local
trace keeps at most64records and returns at most16 per command, reporting omitted
records. It covers only that arbiter: direct clients can bypass it. Neither this
editable trace nor process exit can certify game effects or benchmark success.
The authoritative action journal remains outside the container.
The `run` tool takes an exact Cargo binary name, uses explicit `--bin` selection
and returns up to four structured Rust compiler errors plus complete output-file
references. Compilation and execution use the ordinary bounded container process
group; this does not grant host execution or select a gameplay strategy.

The per-subject diagnostic budget persists in SQLite, including when code is
reverted or a controller restarts. Receipt IDs, elapsed time and printed clocks
do not make a repeated failure novel. Changed code, meaningful game facts, a
changed interface contract or a new task can open a different subject. Exhausted
unchanged diagnosis records a defect boundary and stops polling that subject;
body/chat and unrelated tasks continue. Source fingerprints exclude compiler
caches. These cost hints are not evidence of useful gameplay; native evidence
still decides gates.
