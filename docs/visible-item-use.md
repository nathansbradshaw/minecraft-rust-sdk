# Visible item-use observations

NeoForge integration `0.1.0-alpha.13` adds the optional discovery capability
`minecraft:visible_item_use`. Older installations do not advertise it. The Rust
adapter forwards native discovery features and observation fields without
requiring this feature for existing player sessions.

Each **visible living entity** has `item_use`:

```json
{"format":"java767-visible-item-use-v1","using":true,
 "hand":"MAIN_HAND","item":{"item":"minecraft:bow","use_animation":"BOW"},
 "elapsed_ticks":7,"remaining_ticks":71993}
```

Inactive use reports `using:false`, zero elapsed/remaining ticks, an empty active
item, and no hand. Missing capability or missing fields mean unavailable, not
inactive. The existing 32-block radius, line-of-sight filter and 128-entity bound
remain in force. Hidden entities do not gain rows. This reports current native
animation timing; it does not expose AI targets, hidden inventory, future attacks
or a promise that use will complete. A bow's remaining use duration is its generic
item-use duration, **not** the number of ticks until a skeleton shoots.

Client programs may combine these facts with observed geometry and movement to
investigate anticipatory defense. A completed SDK observation or a source check
does not establish that a bot can dodge or survive.

For a bounded operator diagnostic, connect a dedicated character with `mc-sdk`,
then run `ranged-telegraph-probe PRIVATE_CONNECTED_PROFILE.json SECONDS ENTITY_UUID`
(1–30 seconds), and disconnect with `mc-sdk`. The probe only observes; it does not
move, attack, heal, respawn, run inference or own session cleanup. Its JSON lines
contain scoped observations and hashes, never the profile's private key. Use a
private output file. Installing the changed Java hook requires a saved server
restart on installations without class replacement support.
