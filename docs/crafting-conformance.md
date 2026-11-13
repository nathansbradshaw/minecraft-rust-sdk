# Recipe batch fixture

`menu::craft_once` composes native recipe preparation, one output pickup and
storage into an empty permitted inventory slot. Its private journal persists
each exact request before dispatch; resumption queries that original action ID.
It never retries an uncertain or reconciled mutation. This client composition
does not provide an atomic server transaction.

The matching integration must publish native `player.life_id` and verified
recipe-preparation metadata. The journal binds the native life as well as the
gateway instance, character, recipe and operation prefix. Respawn stops the
operation even if numeric entity IDs, menu IDs and matching outputs recur.
Old integration alpha.14 lacks life identity; use the matching alpha.15 or later
module/adapter pair. Version 1 craft journals stop for operator inspection;
never delete them or replace their IDs to force completion.

`craft_conformance` is an operator fixture for a separately assigned, exclusively
owned, already-connected character. Coordinate character and service ownership
before running it. It never connects a character, supplies items, opens a menu,
or restarts services. Provide a safe Survival location, a fresh empty cursor and
crafting grid, unlocked recipes and enough empty inventory slots. Fixture inputs
and environmental preparation are operator interventions, never autonomous
resource acquisition.

The runtime plan selects at least three distinct recipes and two input material
types. This example uses the player's 2×2 menu with operator-supplied oak and birch
logs. Expected native batch counts are explicit caller data:

```json
{
  "prefix": "unique-operator-craft-trial",
  "materials": [
    {"item": "minecraft:oak_log", "minimum": 2},
    {"item": "minecraft:birch_log", "minimum": 1}
  ],
  "batches": [
    {"recipe": "minecraft:oak_planks", "result": "minecraft:oak_planks", "count": 4},
    {"recipe": "minecraft:birch_planks", "result": "minecraft:birch_planks", "count": 4},
    {"recipe": "minecraft:stick", "result": "minecraft:stick", "count": 4},
    {"recipe": "minecraft:crafting_table", "result": "minecraft:crafting_table", "count": 1}
  ]
}
```

Use a new private fixture directory. Run interruption first, then resume the
identical plan and directory in a new process:

```text
cargo run --locked -p minecraft-sdk --example craft_conformance -- CONNECTED_PROFILE PLAN.json PRIVATE_DIRECTORY interrupt
cargo run --locked -p minecraft-sdk --example craft_conformance -- CONNECTED_PROFILE PLAN.json PRIVATE_DIRECTORY run
```

Interruption drops only the client future after observing a persisted pending
request and reading its original status from the gateway. The native action may
already be complete; the report explicitly records that uncertainty. If all
batches finish before the interruption is observed,
the fixture fails the interruption condition and preserves the journals.
Resumption resolves original pending IDs and never submits those requests again.
Separate completed journals return history while later batches continue.

Keep `binding.json`, all batch journals and reports private: they contain native
observations. The fixture validates declared initial materials, expected output
item/batch counts, native verified receipts and life continuity. Retained gain is
checked at each batch's store; later recipes may consume earlier outputs.
Capture authoritative gateway rows for every `PREFIX-INDEX-prepare`, `-take` and
`-store` ID using `mc-sdk-evidence`, and inspect them with final native inventory.
Require one original row per intention, no duplicate preparation or pickup, and
explicit accounting for consumed materials and retained outputs.

Source tests and this fixture are mechanics evidence. They do not establish
independent gathering, tool acquisition, survival competence or benchmark success.
