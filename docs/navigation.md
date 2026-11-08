# Optional navigation

The Rust client supplies `navigation::navigate` and the CLI supplies
`mc-sdk navigate PROFILE GOAL.json CLIENT_JOURNAL.json [OPTIONS.json]`.
The matching adapter must advertise `minecraft:movement_options`. It computes
paged, read-only predictions using the same collision, gravity and cancellation
admission as finite controls. No prediction is an executed game effect.

For direct diagnostic queries, `minecraft:movement_options` accepts `offset`,
an optional nearby `target`, and `limit` (1–64, default 64). For example,
`{"offset":0,"limit":4}` bounds a page to four evaluated candidates and at most
four returned waypoints. Follow `next_offset` until `complete`, retaining pages
only while origin and scene match. A candidate may yield no waypoint, so an
empty page does not imply completion. Smaller pages bound output, not a new
movement capability or evidence that the goal is reachable.

```json
{"kind":"position","dimension":"example:dimension","target":[12.5,64.0,8.5],"distance":0.25}
```

`block_reach` instead takes `block: [x,y,z]`, `distance`, and a caller-supplied
`eye_height`. It measures distance to the nearest face. Supply dimensions and
coordinates from observations and character parameters from the relevant
integration. Reaching this distance does not prove visibility, mining, pickup
or inventory gain; the caller still chooses and verifies an interaction.

The client explores a directed local graph. It may walk, jump or descend through
already traversable routes; it does not open doors, break terrain, build bridges,
choose resources, or prescribe Survival behavior. `Options` configures deadline,
action/node budgets and forbidden runtime block IDs. No block policy is imposed
by the core helper. Unknown terrain cannot be assumed to be air. No known route
is a report about the explored graph, not proof that the world has no route.

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
