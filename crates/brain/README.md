# Experimental container-local coding brain

This Rust harness observes through the public SDK and gives Luna/Sol a real tool
loop: inspect files, generate Rust, compile/run, act, inspect outcomes and repair.
It runs only in a non-root container. The trusted inference service returns data
and source; it never receives host tool execution permission. SDK gateway/server
credentials and authoritative journals remain outside the container.

Local schema/range validation runs before a pending intention is recorded. For
pre-fix invalid intentions, reconciliation requires a fresh authenticated gateway
`not_found`, the matching character/instance/run journal, and an intention that
still fails SDK validation. It records local bookkeeping recovery, never a game
receipt or verified historical effect, and never replays the request. Valid but
unconfirmed intentions retain normal uncertainty handling. The model's reconcile
tool uses this path; `minecraft-sdk-brain reconcile-pending PRIVATE_CONFIG.json`
also performs one scoped reconciliation without inference, with normal body and
disconnect cleanup. It requires exclusive ownership of that controller state.

The opt-in body setting `experimental_ranged_boundary_turns` (default `false`)
uses recent reached positions to preserve a separated ranged turn at a known wall
along the boundary. This preference keeps existing collision/support checks and
falls back when only the previous lane is viable. Immediate projectiles, mixed
melee, fire and blast escape retain their existing scoring. Local replay is not
a Survival result. The initial enabled candidate died in all three declared
120-second native cover layouts (one death per character). Its real compilation
lasted only 1.25 seconds, providing limited overlap evidence. The maneuver remains
experimental; neither this change nor compilation establishes Survival competence.
Decision captures retain the selected experiment setting for reproducible replay.

Ranged guards distinguish proximity from an open firing route. A close archer
does not invalidate already shielded ground while every candidate and transition
remains known, supported and fully occluded. An unshielded close enemy or an
observed melee threat retains ordinary escape. Guard preferences include covered
room along the wall and bounded separation, rather than driving toward its end
for distance alone. Recalled enemy poses remain explicitly last-seen estimates;
this geometry does not establish their unseen current location or Survival.

Luna handles routine choices. Explicit development, code writes and failed
routine experiments can request a Sol session of at most four decisions. The
remaining session is saved before each inference request, so restarting does not
renew it. Inspections, successful existing programs and further writes during
that session do not replenish it. The next routine decision returns to Luna;
`develop` can explicitly request further coding work. Provider admission budgets
and body/conversation execution remain independent of this scheduling hint.

The local action proxy waits up to ten seconds for nonmovement interaction
effects after releasing the survival mutation gate. Child programs and experiment
feedback receive the resulting native receipt rather than mistaking dispatch for
completion. Finite movement and cancellation remain prompt and inspectable;
longer effects can still return `Running`. `interaction-probe` exercises this
boundary inside the container without model calls. It is a declared integration
diagnostic and does not establish autonomous gameplay or Survival competence.

The `observe` tool accepts an empty input for a compact overview, or a JSON query
such as `{"view":"menu","limit":16}` or
`{"view":"entities","match_id":"minecraft:item"}`. Views `menu`, `inventory`,
`entities` and `blocks` read only the current character's existing observation.
Pages preserve runtime IDs, output and empty-slot bindings, menu/state IDs and
observation revision. Limits are 1–32 records, additionally bounded by bytes;
fields retain SDK nesting (`menu.slots`, `player.inventory`, `world.entities`,
`world.blocks`).
`page.next_offset` and `total_in_snapshot` disclose omissions. Pages are live,
not an atomic multi-page snapshot: match stable entity IDs/slot numbers and
restart menu paging after a menu/state change. Item components are omitted from
these summaries; programs retain the full SDK interface when needed. The compact
overview prioritizes hostiles and loot over old stationary projectiles. These
views add no gameplay action, filesystem access or operator capability.
The `query` command accepts the SDK's closed `{capability,input}` observation
query type for providers advertised by the current server. It can request wider
visible-block searches without compiling a diagnostic program first. Complete
responses up to 5,000 bytes are retained in the model view; larger queries need
smaller pages or a Rust SDK program. Capability errors, origin changes and
deployment pacing remain visible. Read-only queries are inspections, not gameplay
progress, and cannot install plugins or override the acting identity or endpoint.
The brain retains a bounded cache of historical block-search pages across goals
and restarts. `spatial_searches` supplies up to six recent records within 4 KiB,
including repeat counts, page completeness, original match counts, and contextual
life/clock/terrain bindings. Complete empty scans cover loaded, ray-visible block
centers; they do not exclude hidden resources. Query responses and contextual
observations are not atomic, and old locations require fresh observation.
The 256-entry lookup cache can discard old entries while original tool history
remains intact. Recollection grants no gameplay success or investigation credit.
The `navigate` tool composes the existing Rust SDK navigator in a protected child
using the current character session. It accepts the SDK's closed position or
block-reach goal, keeps an actor/server/goal-bound route journal, and returns a
report after at most thirty seconds (or the shorter deployment tool timeout).
The responsive body can preempt its finite movement. The reference client shares
the body's known-hazard exclusions; the core SDK imposes no behavior policy.
Inspect remaining distance, outcome and pending IDs before resuming. Reaching a
pose proves no collection or task completion. This built-in helper is not a
model-generated skill and does not count toward learning evaluations.
Local control protocol version 2 distinguishes a body rejection before gateway
dispatch (`local_preempted`) from an ambiguous transport failure. The navigator
preserves that rejected local intention without inventing a native receipt or
blocking forever on an absent server ID. Network errors retain the original
pending ID for inspection. Recompile older protected clients against the matching
SDK; version mismatches fail explicitly before dispatch.

Build `cargo build --locked -p minecraft-sdk-brain` and run inside the isolated
coding container:

```text
minecraft-sdk-brain run /workspace/brain/config.json
```

The private mode-600 JSON config supplies `profile`, `model_key`, `model_endpoint`,
`workspace`, `state`, `goal`, optional `cargo_target`, `max_turns` (default 64),
`tool_timeout_secs` (240), `model_interval_secs` (31), and `exit_on_complete`
(false). An optional `instructions` string (default empty, at most 2,048 UTF-8
bytes) supplies standing client behavior preferences, such as preserving other
players' builds. It stays separate from replaceable player goals and appears
intact in planning and conversation context. It grants no server or host
privileges and imposes no mandatory SDK behavior policy.
Paths and endpoint are deployment configuration, never compiled server
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
bounded `retry_at_utc`. Daily exhaustion is recorded per model across restarts;
the planner can use the other available allocated model. When both are exhausted,
it waits for a reset while the body remains active. Conversation retains unacknowledged input for
that retry. Authentication, schema, queue, provider and clock-rollback failures
require investigation rather than unchanged automatic requests.
An inference socket failure or incomplete transport body permits exactly one
retry of the same data request. No gameplay mutation is replayed. A lost response
may already have consumed an admission; the broker ledger remains authoritative.
Usage records include attempt count and separate prompt/context hashes. HTTP
refusals and model JSON/schema errors do not receive this transport retry.

`model-probe HTTPS_ENDPOINT ALLOCATED_KEY routine|develop LABEL` checks an
allocated inference capability without connecting a character or executing a
returned tool. Record its actual admission and outcome; it proves inference,
not gameplay intelligence. The key stays in its private local file.
Usage also retains the inference service's `provider_model` revision when
reported. This distinguishes a deployment's explicit provider mapping from the
requested model role and its unchanged allocation ceiling; missing metadata is
recorded as unknown.
Append a private mode-600 context file to replay a saved bounded context with the
same prompt and schema. Returned commands remain data and are not executed by
this diagnostic. Disclose changed contexts and prompts in comparisons.
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

Memory search uses bounded ranked word/stem matches instead of requiring the
entire goal to occur as one substring. Results retain their origin, instance,
dimension and historical observation revision. Agent-authored notes remain
notes even when called facts; remembered locations need fresh observation, and
live entity/menu/session handles are not retained as reusable observations.
Recall reserves at most two slots for relevant executed source versions before
searching general memory. Historical receipt references take priority within
that pool, while duplicate versions share a slot. These references do not
establish a reliable skill. Program excerpts lead with exit status, retained
inventory changes and sampled typed outcomes, rather than Cargo output. Under
context pressure, saved stdout and the retrievable SDK manual are compacted
before recalled outcomes are discarded; current observations and action/error
bindings keep their existing protections.

Rust experiments save content-addressed source before execution and associate
the version with the resulting client diagnostics. Identical files share storage.
Successful older `write` events can recover historical file candidates; a write
is not proof that its source compiled or achieved a goal. New versions retain the
Rust crate sources and Cargo manifests/lockfile within a 1 MiB capture budget.
Capture failure is reported and does not prevent an experiment. Source existence
and a successful exit never grant benchmark success or independent reliability.

`minecraft-sdk-brain memory-index STATE` indexes existing successful writes
without opening a character connection or calling a model. `memory-search STATE
QUERY` retrieves labelled memories. `skill-inspect STATE VERSION` lists exact
source/blob references; `skill-restore STATE WORKSPACE VERSION` first preserves
current source, validates every hash, and restores files without executing them.
Inspect dependencies, compile, obtain current observations and use new intention
IDs for new work. Inspect/reconcile genuine interrupted intentions without replay.

The latest tool result has a separate context budget so ordinary source output
is not silently replaced by its abbreviated history. `minecraft-sdk-brain
context STATE_DIRECTORY` prints the last exact model context for local debugging;
keep that output private because it includes observations and player conversation.

Finite terminal jobs clean up their process group, including background children.
Normal completion, run-budget exit and handled termination disconnect the SDK
character rather than leaving it unattended. Abrupt process/container loss still
needs deployment supervision; this is not a proven background Survival controller.

Use `report` for incomplete progress; it leaves the current goal active.
`complete` requires JSON `{"status":"succeeded","summary":"verified result"}`.
Optional config `completion_inventory` maps runtime item IDs to positive minimum
counts (at most eight entries). These checks apply only to the configured `goal`,
and never carry over to a different player goal. Completion checks a fresh player
inventory; placed blocks, menu outputs and carried stacks do not count. A newly
changed contract reopens an incompatible old completion while preserving its
claim in history. Unchanged criteria preserve historical completion after later
item loss. These are client behavior checks; editable client code can bypass
them. Goals without criteria still have only a model success claim.

Model completion cannot certify an evaluation or evidence gate. This initial harness has not established autonomous
Survival, comprehensive chat delivery, transfer, endurance, fair player-like
perception, or competitive performance. Those need their own integration and
native evaluations. The current observation projection inherits the installed
SDK provider's perception; it must not be presented as a player-visibility proof.

All model/tool source is MIT. Do not include generated worlds, keys, personal bot
memory or deployment journals in a release or source publication.

## Resident engine and replaceable decision processes

Set optional `decision_worker` to the absolute path of a compiled Rust executable
inside the container. The provided `minecraft-sdk-brain` executable implements
this interface itself. The engine snapshots the initial image, owns the player
session, actor/state locks, Survival thread, conversation, journals and tool
execution. A decision child receives one bounded version1 JSON request on stdin
and returns one response on stdout, then exits. It never acquires those locks or
opens a player connection. Per-model permits remain in the parent and proxy
allowances remain authoritative.

The model's local `replace_worker` tool accepts a compiled executable path.
Stage writable-workspace build output with `chmod 700` before activation; shared
write permissions and symlinks are rejected. Read-only root-owned image binaries
are also accepted.
Operators can also run `minecraft-sdk-brain replace-worker PRIVATE_CONFIG.json
NEW_EXECUTABLE` inside the container. Activation snapshots the image by SHA-256
and atomically updates a private selection file. Pending decisions/tools settle
before the new image is used; superseded decision output is discarded. A failed
worker leaves the parent/body active and waits for changed worker selection or a
new goal rather than repeatedly polling the same failure. Invalid candidates do
not replace the prior selection. This replaces decision code; upgrading the
resident engine/body itself requires its own lifecycle procedure.

Protocol fields are defined in `src/worker.rs`. Frames are limited to64KiB,
context to32KiB, executable snapshots to32MiB and an exchange to260seconds
(including stdin/stdout and exit). These are reference-deployment limits, not
SDK bot counts. Snapshots consume the bot's workspace budget and retain recovery
history. Only private stdin carries the allocated model-key path; no private
native credentials are supplied. Host-side inference tools remain disabled.

`decision-probe` is a declared zero-inference lifecycle fixture. Its optional
workspace-local `.worker-probe.json` can specify `delay_ms` (at most10000),
`fail`, or `wrong_identity`. It is not an intelligence or gameplay baseline.
Native threat/worker-replacement acceptance remains separate from subprocess
tests and configured cadence.

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
Observed primed creepers prioritize distance and observed cover. The body uses
the adapter's advertised `finite_move_waypoint_blocks` limit for a longer
continuous retreat, falling back to the older 3.5-block limit when absent.
The Java adapter still validates the swept body against known collision
geometry on admission and during execution. The body skips an opportunistic
attack on a primed creeper. These controls do not guarantee that a late fuse
can be escaped: local native trials survived a charged 30-tick fuse from
4.5 blocks but failed from 3 blocks. Those failures remain open work.
Evasion candidates include observed collision-top centres and jump trajectories
to level or one-block-lower landings, allowing retreat from narrow ledges across
gaps. Unknown terrain, hazards and collisions remain excluded; the server still
admits and verifies actual finite movement. Local gameplay admission waits at
most three seconds for the body; expiration returns an explicit unsubmitted
Conflict, with no later dispatch. Actual dispatched requests retain ordinary
uncertainty and reconciliation semantics. The read-only
`body-probe inspect-observation PRIVATE_OBSERVATION.json` diagnoses a copied
observation without connecting or performing gameplay. Its predicted route is
not evidence of successful movement.
It requires finite locomotion, public survival facts and pending-action inspection.
The current body also requires `minecraft:ranged_observation`: visible entities
within the installed observation radius include ranged-attack classification and
visible hand item identifiers/animations. It distinguishes a ranged threat's
24-block retreat boundary from the melee boundary, values observed cover, and
checks safe one-block descents instead of treating a step as a diagonal ramp.
These are replaceable client behaviors; they are not SDK gameplay policy or proof
that every ranged mod entity, terrain trap or projectile is supported.
The body tracks up to 256 visible projectile identities across advancing native
ticks. A projectile stationary for two ticks can retain reported velocity without
requiring another dodge. Actual motion or a substantial velocity change restores
the flight response immediately; missing identity/clock facts stay conservative.
History resets across a changed life, dimension, native epoch, clock rollback or
observation gap over twenty ticks. This is labelled client inference and leaves
native observations intact. Single-frame diagnostic helpers remain conservative;
these checks do not establish ranged Survival performance.
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

`fixture-workload NEW_DIRECTORY` creates ten std-only Rust binaries with 1024
non-inlined generic instantiations each, for reproducible compiler pressure
without copying another bot's workspace or cached executables. Record its source,
fresh/warm target state, actual build result and overlap duration. This is a
synthetic compiler workload, not a model-generated skill or intelligence result.

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
body and chat continue, and a changed task remains available. Source fingerprints exclude compiler
caches. These cost hints are not evidence of useful gameplay; native evidence
still decides gates.

A previously unseen read-only tool error can arrive on the final diagnostic
turn. Before stalling, the journal permits one further finite investigation for
that feedback per source/experiment/task/interface contract. It does not renew
provider allowances or replay gameplay. Restart, different read tools, changed
error text and position changes cannot replenish this opportunity. Errors already
followed by a model decision, stale-task feedback and mutation errors cannot
obtain it. The notice is client diagnostic evidence, not a new world fact or
task success.

The active diagnostic retains its error and original bounded request across
subsequent inspections and restart. A changed experiment or task removes it from
the active model context; the historical record remains in the journal. Existing
notices are restored only when their recorded experiment/task/interface epoch
matches, without issuing another investigation allowance.

`decision_session` identifies the current reasoning role, work mode and remaining
decisions. During development/investigation the response schema excludes the
redundant `develop` routing command. Gameplay, observation, editing, execution
and reconciliation tools remain available. Source edits during an active session
do not renew it, including when Luna is handling a Sol-allocation fallback.
## Recovery during coding

The resident body can eat ordinary prepared food from the hotbar or offhand
when native observations show hunger and a safe opening. It checks the
selected hand again on the next observation after changing slots, uses the
item's observed duration to bound the action, and releases held use if
danger returns. An interrupted or rejected action is not replayed. Food
gain is reported separately from item-use dispatch. This default policy
does not forage, move food from the backpack, or assume modded foods are
safe; those strategies can be added in the bot workspace.
