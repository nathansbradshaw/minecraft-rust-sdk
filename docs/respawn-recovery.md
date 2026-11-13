# Ordinary respawn and life bindings

The `minecraft:life_identity` native feature adds `player.life_id`. It is stable
for the current native player object and changes when Minecraft replaces that
object on respawn. Minecraft 1.21.1 deliberately reuses both the connection and
numeric `entity_id`; neither identifies a life. The character UUID, gateway
controller session, credentials and authoritative journals remain unchanged.

`world.sample_clock.epoch` combines the JVM and life epochs. Physics, movement
previews, body perception and other cached observations cannot cross that lane.
Native effect and hearing inboxes start fresh. Delayed events from the replaced
player object are rejected on the server thread. Conversation history remains
connection-scoped.

The matching Rust adapter requires this feature at startup. An older module
without it fails compatibility discovery explicitly. Installing changed Java
classes requires an intentional saved server restart; these source checks do
not demonstrate a hot installation or live recovery.

## Source and controlled transport checks

Use the existing shared Cargo target and Java 21 installation:

```sh
cargo test -p minecraft-sdk-java-1-21-1 --bin minecraft-sdk-java-1-21-1
cargo clippy -p minecraft-sdk-java-1-21-1 --all-targets -- -D warnings
server-neoforge/gradlew -p server-neoforge build --offline --console=plain
```

The native life test uses distinct equal-ID objects on one connection. The Rust
TCP/native-HTTP fixture changes only the native life lane during a running
control, retaining its connection, UUID, entity ID and correction counter. It
requires the old control to become uncertain, rejects its stale observation,
and completes a fresh movement on the same controller. These are transport
regressions, not in-game Survival evidence.

## Required live gate

Use one disposable non-operator Survival character and record its source/build
hashes, server instance, UUID, current session and complete native receipts.
Use only the matching installed integration and an exclusively owned controller.

1. Observe a live character and record `life_id`, `entity_id`, clock epoch,
   inventory, menu and owned control. Record one ordinary death during gameplay
   as a failed Survival attempt. Do not reset the world or restore inventory.
2. Observe health at or below zero. Inspect any interrupted control by its
   original durable ID. Reconcile uncertainty through the API; never replay
   the interrupted mutation under a new ID.
3. Submit exactly one ordinary `respawn` action with a fresh durable action ID.
   Inspect that ID. If interrupted, reconcile it explicitly rather than
   resubmitting. Retain uncertainty as uncertainty; reconciliation does not
   certify a historical effect.
4. Require positive native health, the same UUID/session and a different
   nonempty `life_id` and clock epoch. Numeric entity ID may remain identical.
   A natively observed respawn requires the dead-to-alive transition and changed
   life, not healing or another character's observation.
5. Require that no pre-death owned control resumes, old effect/hearing rows are
   absent, and old menu/target/preview bindings cannot authorize new work.
   Obtain fresh observations and complete a new finite movement plus a useful
   inventory-confirmed gameplay action using fresh IDs on that same controller.
6. Retain the death, recovery, gains/losses, interventions and inactive time in
   the evaluation. Recovery passes only recovery; it does not turn the death
   into a successful Survival trial or establish autonomous competence.

Do not close `SURVIVE-01` on source or transport checks alone. Its live evidence
also requires eating and combat under their separate native outcome verifiers.
