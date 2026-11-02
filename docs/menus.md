# Observed menu intentions

The optional Rust `menu` helpers build actions; they do not dispatch, select a
strategy or replay a mutation. Supply runtime recipe/item IDs and observed slot
identifiers. No menu layout, material, server address or mod list is built in.

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
