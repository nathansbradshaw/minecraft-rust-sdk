# Proposed QA time/weather gate

Source-only proposal for Root review, reusing the operator bench lifecycle and
typed RCON transport. No action is implemented or authorized here. Bot programs,
SDK/MCP and models receive neither raw-console capability nor operator credentials.

## Bind the entire disposable QA server

Require an explicit operator-owned disposable allocation. Freeze proposed loopback
ports: game `25578`, native HTTP `19136`, gateway `19148`, RCON `25576`. Reject the
current Root server/world, aliases, RCON `25575`, and endpoints outside this
allocation. Ports alone confer no authority; never modify Root services to pass.

The private binding receipt must join:

- Listener PID/start identities, executables and QA routes; Java PID/start,
  canonical CWD and launch identity.
- Exact `server.properties` hash and parsed `level-name`, resolved against the
  effective world/universe root with containment and alias checks.
- The actually opened save: canonical root and server-open `session.lock` identity,
  or equivalent trusted server-side proof, tied to the approved allocation.

CWD, configured/display world name or matching seed cannot prove the loaded save.
Unaccounted launch overrides or missing active-save proof mean `BindingUnavailable`.
A restart, listener reassignment, config/save change or lost binding invalidates
the receipt. The binding covers **all server levels**: vanilla time-set changes
every level; weather-set changes the Overworld regardless of command dimension.

## Proposed typed operations

Each request binds an immutable receipt hash and durable operation ID. Reject raw
commands, arbitrary predicates/paths/dimensions/endpoints and unknown fields.

| Operation | Bound |
| --- | --- |
| `ReadTime` | Fixed Overworld `daytime`, `gametime`, `day` queries; three commands maximum. Query integers are not the full native clock. |
| `WeatherProbe` | Fixed rain/thunder predicates below, bracketed by game-time queries; four commands maximum. |
| `SetTime` | Pre-arm only, ticks `0..23999`, one attempt. Disclose reset of absolute day time across all levels. |
| `SetWeather` | Pre-arm only, `Clear`/`Rain`/`Thunder`, explicit `20..12000` ticks, one attempt. |

Use a five-second **total operation** deadline and replies capped at 512 bytes.
Persist intent before mutation; retain acknowledged, rejected, partial and unknown
outcomes. Never replay an uncertain setter. Time applied/weather failed leaves a
partial assist: disarm and preserve both records, without automatic rollback.

Setters require `PreArm` and zero server players, including spectators. Hold an
exclusive QA admission fence through guard and mutation. RCON `list` then set is
not atomic zero-player proof. Without admission prevention or a separately reviewed
same-server-thread guard, refuse the setter. No setters after arm or GO.

## Weather read capability and GO evidence

The fixed leaf probe is
`execute in minecraft:overworld if predicate {"condition":"minecraft:weather_check","raining":true}`,
plus the same predicate with `thundering:true`; accept no `run` action. Pinned
English leaf replies are `Test passed`/`Test failed`. Require complete framed
replies and verified runtime command/locale support. Empty replies, parse errors,
unknown locale or missing registry support mean unknown.

Cached 1.21.1 bytecode verifies `ExecuteCommand` uses
`ResourceOrIdArgument.lootPredicate` and its inline NBT parser/`LootItemCondition`
codec. Registered `weather_check` optional boolean `raining`/`thundering` fields
read the context level. This route needs no datapack or public API change.
Vanilla `/weather` has no direct query. Existing `NativeObservation` exposes time,
not weather. These are source facts; no live probe was performed.

Freeze settings, durations, cycle rules and other assists before arm. Do not
silently disable cycles. At actual native GO retain actor/world identity, advancing
native clock, `world.day_time`/`game_time`, and bracketed server-side weather probes.
Record UTC/game-time bounds, wrap/transition ambiguity and relation to native GO.
Separate probes are not one atomic weather snapshot, weather levels, or proof of
precipitation at the actor's location. Keep requested, acknowledged, observed and
unknown values distinct; disclose drift/mismatches externally, never infer a
perfect match from settings. Unverified runtime support or GO association blocks
a **weather-verified** GO; Root must decide whether another trial may proceed with
that limitation disclosed. Saved `level.dat` and setter replies are insufficient.

## Private receipts, checks and remaining decisions

Keep receipts/secrets outside every bot mount: directory `0700`, new record `0600`,
atomic publication plus directory sync; preserve prior attempts. Retain binding,
source/build hashes, operation IDs, timestamps, declared assists, actual observations
and uncertainty. Never retain passwords, credentials, full command lines or
environment dumps. Give bots only the receipt hash and disclosed assist description.

Tests should cover wrong ports/PID starts/CWD/save aliases, launch overrides,
mounted secrets/receipts, admission races/nonzero players, post-arm setters,
partial writes/timeouts without replay, restart invalidation, fixed rendering,
true/false rain/thunder combinations, unknown locale, wrapped/changing brackets
and GO mismatch disclosure. Never invent `/weather query` or infer false from empty.

Root must review the exact disposable allocation/port roles, active-save proof,
admission fence, runtime predicate calibration and GO drift/mismatch limits.
This proposal establishes no current Root-world intervention, trial acceptance,
survival result or autonomous capability.
