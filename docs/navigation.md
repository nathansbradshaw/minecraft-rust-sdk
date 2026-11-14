# Optional navigation

For a collection approach, callers may explicitly set
`Options::until_inventory` to `Some(RetainedInventory { item, minimum })`.
`item` is one runtime registry ID and `minimum` is a positive retained count.
The CLI accepts the same optional field in its existing `OPTIONS.json`:

```json
{"until_inventory":{"item":"yourmod:resource","minimum":1}}
```

The brain's `navigate` tool accepts either its original goal JSON or
`{"goal":POSITION_OR_BLOCK_REACH_GOAL,"until_inventory":{"item":"yourmod:resource","minimum":1}}`.
Choose the actual observed resource and required count; no harvesting strategy
or item list is imposed. Other pickups never stop an ordinary bare position goal.
Lua's existing closed command table accepts the same input shape:
`command={tool='navigate',input={goal=NORMAL_GOAL,until_inventory={item=ACTUAL_ID,minimum=n}}}`.
Bare Lua navigation remains valid. The bridge uses the same typed brain request
and validation; it does not add a general tool or host execution interface.

While its saved movement is Running, the opt-in checks retained counts at a
bounded250ms read cadence. A fulfilled count must be confirmed by a fresh sample
bound to that movement's actor, life, dimension and epoch, with the exact current
control ID. It then persists one `CancelControl` intention targeting only that
ID before dispatch. A lost response is inspected/reconciled by the saved cancel
ID, never reposted. Bare goals perform none of these reads or cancellations.

After original pending input settles, a fresh same-actor/life observation can
satisfy this condition without reaching the old drop coordinate. The report
uses `inventory_satisfied` with `inventory_completion` count/revision/identity
evidence and the original positional gap. This is neither positional arrival nor
movement-effect or parent-task success. Cursor and menu outputs do not count.
Reaching the approach without the resource returns `inventory_missing`; inspect
the actual drop or choose another approach. Missing/unreleased original input
returns `needs_reconciliation` with its exact ID; it is never replayed or hidden.

`inventory_cancellation` records the cancellation request and outcome separately
from the original movement. A cancelled movement remains Cancelled, never
Observed. `actions` retains its completed-movement-intention count; the distinct
cancellation request is separately exposed and recorded in the server journal.

The opt-in uses journal version3, so older clients cannot silently drop its
cancellation record; ordinary journals remain version1. Existing version2 opt-in
journals upgrade without replay. Their already-pending movements lack a saved
life basis, so they settle normally instead of authorizing an in-flight cancel.
Changing/removing the condition on the same SDK
journal path is a conflict; preserve that state. The brain uses a separate key
for each condition, but a different journal cannot bypass an observed held-input
lane. Fresh-observation support is required only for this opt-in. Budgets and
normal native admission remain unchanged.

The Rust client supplies `navigation::navigate` and the CLI supplies
`mc-sdk navigate PROFILE GOAL.json CLIENT_JOURNAL.json [OPTIONS.json]`.
The matching adapter must advertise `minecraft:movement_options`. It computes
paged, read-only predictions using the same collision, gravity and cancellation
admission as finite controls. No prediction is an executed game effect.

A busy control or the adapter's read interval can produce a valid pending page:
`origin: null`, an empty `scene` and `options`, `complete: false`, and no next
offset. The navigator waits for its bounded `retry_after_ms` within the original
operation deadline. It does not treat this as version incompatibility, dispatch
a movement, or replay a pending action. Ready pages still require a scene and
valid origin. Reports include the journal's cumulative `preview_waits` count;
that client counter is diagnostic evidence, not authoritative gameplay proof.
The reference Survival body's background escape discovery also retries pending
pages without waiting for the character to move or blocking urgent defense.

For a declared zero-inference native diagnostic, run
`body-probe PRIVATE_CONFIG.json navigate SECONDS GOAL.json` inside the isolated
worker. It starts the body, primes the real preview interval with read-only
queries, executes the normal navigation helper, and retains responses and
results. Supply an observed, permitted goal and retain native receipts. This
operator-supplied traversal does not establish autonomous planning or hostile
Survival competence.

For direct diagnostic queries, `minecraft:movement_options` accepts `offset`,
an optional nearby `target`, and `limit` (1–64, default 64). For example,
`{"offset":0,"limit":4}` bounds a page to four evaluated candidates and at most
four returned waypoints. Follow `next_offset` until `complete`, retaining pages
only while origin and scene match. A candidate may yield no waypoint, so an
empty page does not imply completion. Smaller pages bound output, not a new
movement capability or evidence that the goal is reachable.

An explicit `target` orders candidate evaluation nearest that target, including
when a distant target cannot itself be a local waypoint. Omitting it preserves
nearest-player ordering. Responses label `ordering`; the target and ordering
contract are bound to the scene, so changing them invalidates old page cursors.
Ordering changes neither the local candidate bounds nor native collision,
gravity, known-terrain, tick and action-admission checks. The body can pin its
existing defensive choice as a first-prefix ordering preference after a native
refusal. That hint is client behavior, not a guarantee of a longer safe route,
a current hidden enemy position, or Survival performance.
The hint stays pinned through an in-flight read and its bounded busy retry. A
completed prefix gets at most one same-pose refresh if the existing close-threat
rule rejects its old hint and current defensive behavior supplies a different
one. Normal read spacing remains in force. This does not establish general
liveness for repeated threat changes or an empty/unusable native prefix.

The adapter excludes endpoints that necessarily intersect solid terrain before
running trajectory search. The test accounts for arrival tolerance and keeps
grazing contacts that might still admit a nearby legal pose. Diagnostic fields
`total_candidates` and `pruned_blocked_endpoints` describe this read work; neither
is evidence of movement or task completion.

Completed prediction pages are saved in the client's bounded navigation journal
when a wall-time budget expires. A later invocation validates a small current
page before resuming its saved cursor. Actor life, clock, pose, physical state
and adapter changes discard stale read work; the adapter's scene identifier also
covers the exact start and movement physics. The advertised retry interval is
retained across invocations. The current blocked-block policy is applied when
using cached predictions. These are read-only caches: pending action IDs retain
their existing inspection/reconciliation rules and are never automatically
replayed. An old journal without cached pages remains compatible.

```json
{"kind":"position","dimension":"example:dimension","target":[12.5,64.0,8.5],"distance":0.25}
```

`block_reach` instead takes `block: [x,y,z]`, `distance`, and a caller-supplied
`eye_height`. It measures distance to the nearest face. Supply dimensions and
coordinates from observations and character parameters from the relevant
integration. Reaching this distance does not prove visibility, mining, pickup
or inventory gain; the caller still chooses and verifies an interaction.
A valid mining pose can remain outside item pickup range; this helper does not
automatically approach drops after a block changes.

The client explores a directed local graph. It may walk, jump or descend through
already traversable routes; it does not open doors, break terrain, build bridges,
choose resources, or prescribe Survival behavior. `Options` configures deadline,
action/node budgets and forbidden runtime block IDs. No block policy is imposed
by the core helper. Unknown terrain cannot be assumed to be air. No known route
is a report about the explored graph, not proof that the world has no route.

`Options::eager_progress` is an experimental opt-in, disabled by default. The
reference brain enables it to reduce idle time between finite controls. A fresh
partial preview may supply a strictly goal-improving, policy-permitted edge;
the client may dispatch it before reading every local candidate. Eager shortcuts
skip already arrived frontiers, including after another controller returns the
player to an earlier pose. Full bounded graph search can revisit partial nodes
to finish their unread candidates and find detours. This changes client route
selection, never ordinary native admission,
body preemption, goal tolerances or pending-ID reconciliation.

Partial nodes remain unexpanded. The resumable read cursor is retained, and a
`prefixes` entry binds the origin, scene and unfinished cursor to the exact action
ID even after moving invalidates that cached expansion. Reports include cumulative
`preview` counters for ready pages, busy/other pending envelopes, scene restarts
and eager intentions. An intention can be refused locally before gateway dispatch;
none of these counters is native movement or retained-goal evidence.

The alpha12 NeoForge integration adds `minecraft:fluid_facts`: versioned native
fluid tags, occupied height, and known flow for each observed cell, plus the
character's `eyes_in_water`. The matching Rust predictor changes medium at each
predicted body position and offers observed still-water surface waypoints.
Flowing/unknown currents, lava transitions and fluids without supported physics
return explicit admission errors; a water tag does not promise compatibility
with arbitrary modded fluid behavior. Older installations retain their existing
admission and do not gain transition support from the client alone.

A completed finite swim releases input. It does not keep a character afloat
while its caller is idle. The reference body permits surface swimming only
with native eyes-clear/full-air facts; submersion, reduced air or missing facts
retain protective behavior independently of reasoning and compilation.

`cargo run -p minecraft-sdk --example cancel_waypoint -- PROFILE PREFIX X Y Z`
is a bounded conformance probe for a connected disposable character. It submits
a finite waypoint, samples after 150 ms, cancels in the same process, verifies
the original cancelled outcome and native input release, then disconnects.
Use fresh IDs and a permitted observed target that cannot complete before the
sample. It does not prepare a world or establish autonomous gameplay competence.

The client journal is a private local file, distinct from the trusted server
journal. It binds instance, character and goal, takes an exclusive invocation
lock, and persists an intention before dispatch. Resume with the same file;
pending IDs are read and explicitly reconciled when needed, never automatically
replayed. A deadline may return a pending ID while its finite server control
finishes or is cancelled. Inspect it before starting another intention.
The CLI returns status2 for an unfinished goal and prints the structured report.

Scene/origin changes invalidate paged route accumulation. Normal server
permissions remain authoritative when executing a route, including denials that
arise after prediction. Client journals, graphs and reports remain editable
client data and cannot certify benchmark success or character ownership.

After a verified arrival, small residual drift (up to 0.35 blocks) can cross the
local graph's rounding boundary. The helper expands the reached waypoint when
the current observation matches that arrival's native entity life, dimension
and server clock epoch. This prevents repeated corrective visits to a waypoint
that remains marked unexplored. It does not change exact goal tolerances,
simulate a game effect, or relax native admission. Missing/legacy lane facts,
larger displacement, a new life, or a changed epoch use the current pose key.
Pending intentions retain their selected source node so failed edges remain
fenced under the same graph identity after interruption and reopening.
