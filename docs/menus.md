# Observed menu intentions

The optional Rust `menu` helpers build actions; they do not dispatch, select a
strategy or replay a mutation. Supply runtime recipe/item IDs and observed slot
identifiers. No menu layout, material, server address or mod list is built in.

Alpha.11 adds the read-only `minecraft:recipes` capability. Its advertised input
schema accepts exact runtime recipe, output-item and recipe-type selectors, plus
bounded pagination. `recipes::Query::request()` builds the same request for Rust
clients. A recipe ID is distinct from its output item ID; discover it rather than
assuming an item, tag or material category is a recipe. Results include this
character's recipe-book knowledge, representative matching ingredient stacks,
result metadata and shaped-grid dimensions where available. No recipes are
unlocked by querying. Dynamic results and custom ingredient quantities/components
can require a mod adapter; omitted alternatives or ingredients remain unknown.
Respect `retry_after_ms`, `next_offset` and `complete`. Pages are live across data
reloads; an output-filter miss cannot exclude dynamic recipes.

The alpha.11 adapter requires that matching server capability before dispatching
recipe-book crafting. It rejects an unknown ID or a recipe absent from this
character's book before sending a gameplay packet. It does not claim that a
known recipe fits the open menu or that available ingredients suffice. Installing
the Java provider requires a saved restart; replacing the Rust adapter alone
cannot add it. Existing finite controls and ordinary player permissions apply.

`menu::prepare_recipe(&observation, recipe_id, make_all)` builds `Action::Craft`
for the observed menu. This is recipe placement into a grid. A prepared result
is not yet an item retained by the character.

`menu::click_slot(&observation, slot, button, click_mode::QUICK_MOVE)` binds an
intention to the observed menu ID and revision, refusing a missing slot or bad
revision. Observe again between interactions. The server still rejects stale
state and enforces its normal player permissions; a client view is not authority.

`menu::item_count(&observation, item_id)` counts exact IDs in player inventory.
Menu result slots and carried cursor stacks are excluded. Compare actual before
and after observations and receipts before reporting acquisition or crafting.

Rust `ClickSlot.mode` remains `u8`, with named `click_mode` constants. SDK JSON
accepts `pickup`, `quick_move`, `swap`, `clone`, `throw`, `quick_craft` and
`pickup_all`, as well as numeric0–6. Serialization always uses the existing
numeric form, preserving wire compatibility and duplicate-ID intent equality.
Unknown modes fail explicitly; aliases do not grant creative/operator authority.
Raw clients should use the canonical numeric form for older gateways.

Give each intended action a durable ID before dispatch. Inspect running or
uncertain IDs and reconcile after interruption; never repeat a mutation just
because an output preview is abbreviated or a menu changed unexpectedly.

NeoForge integration alpha.8 advertises
`minecraft:session_material_preservation`. Before a registered SDK player is
marked disconnected and saved, it closes menus using their ordinary removal
handlers. This returns personal crafting inputs and cursor stacks to available
inventory space, including personal inputs behind another open menu. A virtual
recipe result is never collected by cleanup. Full inventory uses the game's
normal item-drop behavior; this is not a guarantee against ordinary world loss.
World menus keep their own removal semantics, including persistent modded
workbenches. The hook does not extract their contents or change human players.

This covers normal disconnect, broken player connections and saved server
shutdown. It cannot make a JVM crash or unsaved world storage durable. The
reference Survival brain requires the capability before starting. Installing
the new Java hook requires a saved server restart; swapping a Rust controller
alone cannot install it.
