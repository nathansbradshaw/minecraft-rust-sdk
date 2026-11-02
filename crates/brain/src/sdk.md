Rust SDK quick reference (documentation, not a gameplay strategy):

- SDK types are re-exported from minecraft_sdk. Their source is crates/core/src/lib.rs
  beside crates/sdk/src/lib.rs under the SDK source root. The crate directory is
  core, not sdk-core. Rust tools in the workspace include Cargo; if rg is absent,
  use grep/find or the harness read/list tools. Terminal output_files retain
  complete bounded stdout/stderr; inspect their missing ranges using sed/tail
  instead of rerunning an experiment just because its preview is abbreviated.
- Import Client, Profile, Action, ActionRequest, Position, Outcome. Load Profile
  from MC_SDK_PROFILE; Client::with_profile(&profile) uses its existing session.
  Generated programs must not connect/disconnect the harness's character.
- Observation.player and .world are serde_json::Value; .revision is String.
  player.position is exactly [x,y,z]. A compact context is a sample; a program can
  inspect client.observe().await? for full local facts and collision geometry.
- Action::StartMove { target: Position{x,y,z}, jump: bool, max_ticks: u16 } is
  finite, ordinary movement. Action::CancelControl { action_id:String } cancels
  that control. Legacy Move, Jump and MotionFrame are disabled in protected
  coding deployments; old examples using them require adaptation.
- client.act(&ActionRequest{action_id:String,action}).await? returns Receipt.
  Outcome::Running means poll client.action_status(&id).await? until terminal.
  Observed verifies the requested effect. Cancelled, Expired, Failed and Rejected
  are feedback, not success. Reobserve/replan after defensive cancellation.
- The inherited MC_SDK_ARBITER defers ordinary gameplay during danger while code
  and chat remain available. A deferred action's old target may be stale; inspect
  its receipt and fresh observation. Do not bypass the local body scheduler.
- Keep unique durable IDs per intention. An interrupted/Uncertain mutation is
  inspected with action_status, pending_actions and reconcile; never replay it
  under a new ID. Reconciliation does not prove a historical effect.
- Print compact action ID/outcome/error and relevant before/after facts, not full
  world receipts every frame. Authoritative evidence stays in the gateway journal.
- Generic menu builders: menu::prepare_recipe(&observation, recipe_id, make_all)
  builds Action::Craft for the observed menu. This prepares a recipe; it does not
  take its output or establish inventory gain. Inspect observation.menu.slots and
  use menu::click_slot(&observation, slot, button, click_mode::QUICK_MOVE) with a
  fresh observation when appropriate. The caller chooses slots and verifies effects.
  menu::item_count(&observation, item_id) counts retained player inventory only,
  excluding a prepared output or carried cursor stack. Do not print every air slot.
- Rust ClickSlot.mode remains u8; click_mode constants cover PICKUP=0,
  QUICK_MOVE=1, SWAP=2, CLONE=3, THROW=4, QUICK_CRAFT=5, PICKUP_ALL=6.
  SDK JSON also accepts the corresponding snake_case names; serialization sends
  the same canonical numeric intent. Unknown modes fail explicitly. Normal player
  authority and stale-menu rejection still apply. A creative-only mode grants
  no creative permissions. Programs retain IDs and inspect uncertainty as usual.
- Use tokio::time for bounded waits; it is enabled in the coding project. Paths,
  model access and SDK endpoints come from deployment, never invented credentials.
- Optional read-only query schemas are advertised in server.limits.observation_queries.
  Call client.read_observation(&ObservationQuery{capability, input}).await?. A
  block_search query accepts a runtime block ID or tag, radius, limit and offset.
  It returns only loaded blocks hit by an unobstructed targeting ray. Follow
  next_offset until complete, respecting retry_after_ms. A partial page with no
  matches is not proof that the area contains no resources. It cannot load code.
- If advertised, minecraft:movement_options returns paged, collision/gravity-
  admitted local waypoints. These are predictions; only executed receipts prove
  movement. Prefer the optional Rust navigation helper over guessing each step:
  navigation::navigate(&client, goal, journal_path, options).await?. Goal::Position
  takes dimension, target:[x,y,z], distance. Goal::BlockReach takes dimension,
  block:[x,y,z], distance and eye_height supplied for the character (upright Java
  player:1.62; use an appropriate interaction range). Choose the block from fresh
  observations. The helper uses a persistent local graph and finite controls;
  it never mines, teleports, breaks obstacles or chooses your task.
- Put its journal under MC_SDK_STATE, using a distinct file per goal. Reuse that
  journal to resume; pending IDs are inspected rather than replayed. Options
  controls operation deadline, actions, nodes and caller-supplied forbidden block
  IDs. Inspect status/remaining_distance/pending_action in the report. reached
  for BlockReach means a candidate interaction distance, not an obtained item.
  Keep the defensive arbiter enabled and verify the requested game effect.
