# Minecraft Rust SDK

Rust gameplay SDK, CLI, stdio MCP, authenticated gateway and a NeoForge server
integration for coding agents. Bot programs run on their author's compute;
operators do not execute public users' code.

**Development alpha.** Java 1.21.1 / NeoForge 21.1.233 is the first adapter.
See [docs/acceptance.md](docs/acceptance.md) for observed release checks and limits.
Vanilla without the integration, Paper, Fabric and Bedrock are not supported.

The core contracts contain no server address, block/item catalogs, bot strategies,
modpack requirements or bot-count ceiling. Discover capabilities and deployment
limits at runtime. Normal player capacity and existing protections still apply.

## Build

```sh
cargo build --workspace --release --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cd server-neoforge
./gradlew build
```

Rust 1.89+ and Java 21 are required. The pinned NeoForge adapter is Java 767;
install the matching server module before starting the gateway. Java is used
only for required Minecraft/NeoForge hooks; all tools and services use Rust.

## Connect your bot

Obtain the operator's HTTPS gateway origin, then keep the created profile
private. Registration needs no human account or GitHub sign-in.

For an operator-managed private certificate authority, set `MC_SDK_CA_CERT`
to a PEM certificate bundle inside the client environment. The SDK adds those
roots to its normal trust store and keeps hostname and TLS verification enabled.
Missing, empty, malformed, or oversized files fail closed; the file is limited
to 1 MiB. Mount only the public CA certificate in a coding container; keep its
private signing key with the operator.

```sh
mc-sdk discover https://your-sdk.example
mc-sdk register https://your-sdk.example builder ./builder.key
mc-sdk connect ./builder.key first-controller-request
mc-sdk observe ./builder.key
mc-sdk act ./builder.key ./action.json
mc-sdk status ./builder.key look-001
mc-sdk disconnect ./builder.key
```

`action.json`:

```json
{"action_id":"look-001","action":{"kind":"look","yaw":90,"pitch":0}}
```

Reconnect with another connection request ID after disconnecting. The same key
resumes the same UUID, inventory and location. One controller owns a connected
character. Repeating a connection request ID recovers its established session;
a different concurrent controller is refused. Profiles are mode 600 and bind
the credential to the discovered server instance. Losing a key requires an
operator to recover it. Disconnect before `mc-sdk rotate-key PROFILE`.

The Rust library exports `Client`, `Profile`, `Action`, `Observation`, and
`Receipt`. [The example](crates/sdk/examples/observe.rs) shows library usage.
Coding agents can run `mc-sdk mcp PROFILE` as a stdio MCP server after connecting.
It exposes discovery, observation, typed gameplay actions, status and explicit
reconciliation. Code editing/execution uses the agent's container terminal.

The experimental [container-local Rust brain](crates/brain/README.md) adds a
Luna/Sol tool loop that can inspect, edit, compile and run Rust programs against
the SDK. Its source checks and individual pilot actions do not establish
autonomous Survival, retained learning or competitive agent performance.

## Gameplay and receipts

Actions cover collision-limited walking, short drops, explicit jumping movement,
looking, sprint/sneak, staged
mining, block use/placement, held item use, entity interaction/combat, hotbar
selection, menu/inventory clicks (including equipment), recipe placement, chat,
own-player commands and respawn. Runtime recipe/block/item IDs remain namespaced
strings. Mod menus use ordinary server-defined menu IDs and slot rules;
specialized operations require an installed namespaced capability adapter.

Every mutation needs a stable unique action ID. The same ID with the same action
returns its original receipt. Reusing it for another action is an error.
`observed` means the adapter obtained action-specific native evidence.
`dispatched` is distinct from a verified game effect. `uncertain` blocks further
mutations until explicitly reconciled:

```sh
mc-sdk reconcile ./builder.key ACTION_ID
```

Reconciliation never replays the mutation. `reconciled` records settled current
state and explicitly **does not prove the historical action's effect**.
No transparent retry creates a new mutation ID. Gateway restart clears controller
sessions and makes unfinished actions uncertain. Character and journal persist.

Strategies such as farming, shelter, food and defense belong in bot programs.
The trusted player adapter retains connection keepalives and validated movement;
it does not impose a Survival strategy.

## Operator security

See [runner setup](docs/runner-admin-setup.md) and [architecture](docs/architecture.md).
The [operator installation guide](docs/operator-setup.md) covers deployment,
key recovery, existing-character imports and editable workspaces.
The gateway and native credentials belong outside coding containers. Put the
loopback gateway behind a TLS proxy. Never expose port 19135 or its native token.
Install plugins as an operator; capability manifests and bot requests cannot
install code. Adapters use a versioned JSON-line process protocol, not Rust's
unstable native plugin ABI.

Public requests cannot choose the acting UUID, send arbitrary packets, execute
console/host commands, access files, reset characters or upload plugins. Bots
use genuine non-operator Survival players and ordinary Minecraft packet
handlers, including claim and permission checks. Java online-mode remains on.

The optional rootless container kit supplies isolated coding workspaces and a
bounded HTTPS egress proxy. Use bot-scoped model access; do not mount personal
SSH, GitHub, Codex, server credentials or Docker sockets. External clients are
untrusted regardless of their claims about container isolation.

MIT for this project's source. See [third-party notices](THIRD_PARTY_NOTICES.md)
for dependency/build-tool/data provenance. No worlds, Minecraft assets,
installed modpack, credentials or private repository history are distributed.
