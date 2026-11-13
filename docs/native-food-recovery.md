# Supplied-food recovery diagnostic

The body can prepare a known safe meal from player inventory slots9–35 by
swapping it into an explicitly observed empty hotbar slot. The current inventory
menu, empty cursor, source stack and destination binding must agree. The body
then observes again, selects the meal and starts finite item use. It does not
replace a held tool, invent an empty slot or retry an unchanged uncertain swap.

Foreground SDK dispatch through either the coding-program proxy or direct brain
actions invalidates earlier hand snapshots while holding the shared mutation
gate. A cancelled or transport-failed dispatch leaves an unresolved food fence;
a later successful action cannot certify that earlier intention. Only a terminal
receipt for every exact tracked ID releases the fence. Direct inspection,
explicit reconciliation and bounded background status inspection can observe
that resolution, including completed finite movement. Running, uncertain,
unrelated or missing receipts retain the fence. This does not bound HTTP dispatch
latency. Transport regressions verify snapshot invalidation and recovery through
exact-ID status; they do not establish eating.

Direct brain actions reject a capability missing from verified server discovery
before recording an intention or acquiring the mutation gate. This does not
change existing pending IDs. Proxy requests and other pre-admission errors still
need a matching terminal receipt; a missing receipt never clears the food fence.

## Native fixture

Coordinate a separate disposable Survival character and service window with the
current live owner. Do not reuse another trial's character, restart its services,
or supply fixture items during an autonomous acquisition trial. Preserve the
character and world before changing the declared fixture.

Prepare a quiet, dry, supported location with health20, maximum health20, food12,
no active movement/item use, no sprint and an empty inventory menu/cursor. Put a
wooden pickaxe in selected slot0 and exactly two bread in one main inventory
slot9–35. Every other player inventory slot must be explicitly empty. Record the
ordinary Survival mode and saved inventory through the trusted operator path.
These supplied items are fixture material, never autonomous acquisition.

Freeze an owner-private mode600 spec with the separate character's actual UUID,
position and bread slot:

```json
{
  "player_uuid": "00000000-0000-4000-8000-000000000001",
  "player_position": [0.5, 1.0, 0.5],
  "source_slot": 9
}
```

Run inside the existing bounded runner with its private profile/config and
compatible native module:

```text
food-probe CONFIG.json blocked 20 SPEC.json
food-probe CONFIG.json compile 20 SPEC.json
```

Use a separate freshly prepared fixture and state directory for each mode.
`blocked` stalls the caller without making a model request. `compile` invokes the
real Rust compiler in the configured workspace and preserves its output; its
terminal timeout comes from the config. Reuse the deployment's configured Cargo
cache. A failed compiler is useful resilience evidence but must remain recorded
as failed. Require a native eating interval overlapping actual compilation before
claiming compilation independence; a build that ends before eating proves no
such overlap.

`food_probe_admitted` retains the actual SDK observation before the body starts.
A wrong character, changed food/inventory/pose, occupied cursor or observed
hostile/projectile rejects admission. The probe does not repair the fixture.
It records the final native scene, verifies food12→17, exactly one consumed bread,
an unchanged pickaxe/cursor and the same sample-clock epoch, then stops its body
and disconnects. Position tolerance is0.25blocks. It makes zero model calls.

Retain source/binary hashes, both brain/body journals, native receipts for swap,
selection and finite use, and an independent saved-player after-state. Verify the
use receipt's native food delta and released input. The state check alone is not
historical proof of the complete action chain. Source tests, fixtures and a
single supplied-food success do not establish gameplay competence, food
acquisition or a surviving water→wood→tools chain.

For a refusal control, independently verify no empty hotbar space, carried cursor
material or mismatched menu binding produces no swap. The policy's source tests
cover these cases; the strict positive native probe deliberately rejects them
before starting the body. Do not call that admission rejection a native policy
refusal result.
