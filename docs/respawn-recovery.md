# Ordinary respawn and life bindings

NeoForge module and Rust adapter `0.1.0-alpha.15` add the
`minecraft:life_identity` native feature and `player.life_id`. It is stable
for the current native player object and changes when Minecraft replaces that
object on respawn. Minecraft 1.21.1 deliberately reuses both the connection and
numeric `entity_id`; neither identifies a life. The character UUID, gateway
controller session, credentials and authoritative journals remain unchanged.

`world.sample_clock.epoch` combines the JVM and life epochs. Controller-owned
motion state, movement-preview/perception caches and native-effect checks use
this lane.
Native effect and hearing inboxes start fresh. Delayed events from the replaced
player object are rejected on the server thread. Conversation history remains
connection-scoped.

The matching Rust adapter requires this feature at startup. An older module
without it fails compatibility discovery explicitly. Installing changed Java
classes requires an intentional saved server restart; these source checks do
not demonstrate a hot installation or live recovery.

Deploy `minecraft-sdk-neoforge-0.1.0-alpha.15.jar` together with the alpha.15
Rust adapter. Keep the alpha.14 artifact and its hash as the previous build; do
not replace its bytes under the old version. With controllers disconnected and
zero players online, save and stop the server, back up the world and module
configuration, remove the previous SDK jar from the active mods directory, and
install exactly one SDK module jar. Start Java, require discovery to advertise
`minecraft:life_identity`, then start the matching adapter and verify discovery
reports adapter version `0.1.0-alpha.15` before reconnecting any controller.
Rollbacks require stopping controllers and Java and restoring the matching
module/adapter pair; never downgrade only one half.

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
5. Require that no pre-death owned control resumes and effect/hearing inboxes
   use new life epochs without retained pre-death events. Obtain fresh
   observations, discard pre-death menu/target/preview data in the caller, and
   complete a new finite movement plus a useful inventory-confirmed gameplay
   action using fresh IDs on that same controller.
6. Retain the death, recovery, gains/losses, interventions and inactive time in
   the evaluation. Recovery passes only recovery; it does not turn the death
   into a successful Survival trial or establish autonomous competence.

These guarantees cover adapter-owned caches/effect verification and the checks
provided by particular SDK helpers. `craft_once` binds its journal to the native
life; navigation validates cached preview scenes against fresh state. Generic
menu builders and raw actions do not carry a life or observation token.
`ClickSlot` checks current menu/state numbers, which may be reused after respawn.
`StartMove` revalidates its target against current native state without requiring
a preview token. Callers must obtain fresh state after respawn: a newly submitted
raw request cannot be recognized as old intent solely from its numeric fields.
Repeating the same durable ID and payload retrieves its original history rather
than authorizing replay; a new ID represents a new intention.

## Additional missed-death gate (unrun)

Ordinary recovery above does not establish the fallback when the physical
adapter misses the dead interval. Use a separately owned disposable character
and adapter lane, with bounded native-observation and correction-packet holds
outside production code.
Agree on that fixture and its controller/service window before running it.
No observation-hold mechanism is supplied by these changes.

1. Start a finite control and retain its pre-death observation, native life,
   clock epoch, numeric entity ID, correction counter and durable receipt ID.
2. Hold only that adapter's incoming native observations and queue its respawn
   correction packet. Complete one ordinary death and respawn, recording direct
   native before/dead/after samples through the fixture operator. Preserve UUID,
   connection and controller session. Record all operator interventions
   separately from agent gameplay.
3. Resume adapter observations after positive health returns, keeping the
   correction queued until the life fence has been inspected. Capture enough
   adapter-side observation/packet evidence to prove it received neither a
   nonpositive-health sample nor a correction-counter change during the gap.
   A client polling gap alone does not prove the physical adapter missed death;
   if it saw either fallback signal, this gate remains unproved.
4. Require changed native `life_id` and clock epoch with the same numeric entity
   ID. The original control must become uncertain without replay. Reconcile the
   original ID explicitly, obtain fresh state, and complete a new finite movement
   on the same controller. Preserve old/new effect and hearing epochs and require no
   retained pre-death events. Release and acknowledge the queued correction
   before attempting new movement; preserve that separate receipt/packet lane.

Separately, the controlled transport regression supplies a captured pre-death
observation directly to the internal physical controller and requires rejection.
Public raw actions do not accept an observation object; this internal stale-sample
check is source/transport evidence, not a separate native public-API gate.

This gate is blocked until the matching integration is installed and the
isolated observation-hold fixture is owned and available. The controlled
transport test covers this state transition but cannot close the native gate.

Do not close `SURVIVE-01` on source or transport checks alone. Its live evidence
also requires eating and combat under their separate native outcome verifiers.
