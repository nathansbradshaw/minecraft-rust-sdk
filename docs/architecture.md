# Boundaries and compatibility

```mermaid
flowchart LR
  B[Untrusted coding agent and bot program] --> C[Rust SDK / CLI / stdio MCP]
  C -->|Scoped key, HTTPS| G[Trusted per-server gateway]
  G --> J[Private SQLite identity and action journal]
  G -->|Versioned process protocol| P[Trusted version adapter / persistent player sockets]
  P -->|Ordinary player packets| M[Minecraft server]
  P -->|Private admission and observations| N[Installed NeoForge module]
  N --- M
```

`minecraft-sdk-core` defines version-independent operations and errors.
`minecraft-sdk` has no Java/modpack dependencies. `minecraft-sdk-gateway` owns
identity resolution, key hashing, session fencing, durable idempotency and HTTP.
`minecraft-sdk-java-1-21-1` owns wire encoding and native state conversion.
The Java module implements only local admission and own-player observations;
normal player handlers perform mutations. Mod behavior remains server-owned.

Registration labels cannot select player names or UUIDs. The gateway generates
UUIDs and `.S_` names, which cannot be authenticated as ordinary Java usernames.
The native registry is instance-owned and persistent; registered identities are
separate from connected player capacity. Existing identities can be reserved.
Keys contain 256 bits of OS randomness; only domain-separated SHA-256 hashes
bound to server instance and bot ID are kept in the gateway database.

A native ticket expires after ten seconds, binds the exact registered UUID,
name and local TCP source port, and is consumed once. It never changes global
Java authentication. Operators must keep the native directory/token private.

The gateway orders each character's reads, lifecycle and mutations in its own
lane. Each connected character has a separate operator-configured adapter process
and player socket. A long action or failed transport for one character does not
hold another character's transport lock. A bounded deployment queue limits
in-flight work; normal server admission determines connected player capacity.
An empty adapter process serves initial discovery without owning a character.

Since Rust alpha.4, the gateway and Rust adapter must both support process protocol
2. The initial bare `Discover` advertises `limits.process_protocol`; subsequent
JSON-line frames carry an OS-random channel ID, increasing request ID and actor
binding. Replies must match the channel, request, instance, bot, UUID, operation
and applicable action ID. Transport errors or mismatches close only that lane
and invalidate its controller. Reconnection does not replay an interrupted action.
Replies drain even when an HTTP caller disappears, retaining their queue permit
and lifecycle ordering until completion. Active sessions pin the adapter version;
new sessions reject a changed binary until the gateway is deliberately restarted.

HTTP API 1 and the private NeoForge native API 1 are unchanged. This Rust transport
change does not require replacing the alpha.3 NeoForge module. Deploy the matching
Rust gateway and adapter together; restarting the gateway disconnects SDK players
but does not restart the Minecraft server. Legacy `move` still blocks its own
character's observations while executing. Independent lanes do not yet provide
finite cancellable locomotion, a tactical Survival controller or survival proof.

The journal commits intent before dispatch, keeps action IDs permanently and
fences multiple gateway processes with a file lock.

No HTTP or MCP operator endpoint exists. Operator recovery/revocation uses a
mode-600 Unix socket beside the gateway journal, with same-UID peer checks.
Revocation disables authentication before attempting the native disconnect.
Recovery requires a confirmed disconnected controller and does not start it.

Limits on registration rate, resource use, observation bounds, and normal server
capacity are deployment constraints, not a fixed SDK bot registry ceiling.
Adapters can reject unsupported versions/actions explicitly. A namespace in a
remote manifest never grants permission to load or execute code.

Operator-installed Java modules can register namespaced read-only providers with
`SdkService.registerObservationProvider`. The callback receives the actual admitted
non-operator Survival player and a bounded JSON object. Providers must validate
their exact input fields and retain own-player visibility/permission rules. They
must not accept an actor override, filesystem path or executable code. Registration
happens on the server thread after SDK startup; discovery advertises installed
providers. Untrusted requests cannot register or load a provider.

`POST /v1/bots/{bot}/observations/query` accepts `{capability,input}` under the same
key/controller binding as normal observations. Rust `Client::read_observation`,
`mc-sdk query PROFILE QUERY.json`, and MCP `minecraft_observation_query` expose it.
Input is at most 8 KiB and an object; responses are at most 8 MiB. Default observations
retain their 1 MiB client bound. Missing providers return `unsupported_capability`.

`motion_frame` is an optional typed action for programs simulating individual
walking frames. The Java adapter checks swept collision, ordinary per-frame
distance, gravity and continuous airborne velocity, and derives ground contact.
A caller cannot supply its own ground flag. Unsupported airborne states fail
closed. `move` remains the higher-level supported-waypoint operation.

Rust alpha.5 additionally advertises `minecraft:finite_locomotion`. `start_move`
accepts a waypoint, `jump` and positive `max_ticks` (fifty milliseconds per tick).
Discovery advertises the deployment's `finite_control_max_ticks`; the Rust
adapter configuration defaults to 100 and permits an operator budget of 1–600.
It returns `running` immediately, then drives ordinary player position packets
in a separate bounded control thread. Own observations and non-conflicting
actions remain available. `cancel_control` references that same character's
durable action ID and has its own durable request ID. Inspect both IDs through
normal action status; cancellation can remain running while gravity settles.

`observed` means the requested target was observed, `cancelled`/`expired` mean
the corresponding finite control ended, and `failed` means execution ended
without the goal effect. Evidence separately states whether its native effect
was verified. Missing an immediate effect is not interruption. Genuine transport
or verification loss remains uncertain and requires explicit reconciliation,
which never replays or retrospectively certifies a historical effect. A running
control is separate from that uncertainty fence. Gateway restart or controller
disconnection converts uncaptured running results to uncertain records.

The initial finite adapter requires grounded starts, observed collision geometry
and a gravity-only release path for every planned frame. It refuses unsupported
terrain rather than freezing a character in the air. A second locomotion control
must wait for cancellation/completion; look and other ordinary actions can
continue. These conservative client trajectories are not full water, fluid,
vehicle, knockback or modded movement physics conformance. Those need independent
capabilities and native tests. No NeoForge module change is needed for this
optional Rust capability. Older clients cannot decode the new outcome variants
for actions created by newer clients. Keep a matching gateway/adapter pair and
preserve the journal through upgrades; never restore an old journal to replay work.
