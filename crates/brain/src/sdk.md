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
  Profile is a Serde data type: deserialize the JSON file rather than calling
  Profile::load (that method does not exist). This connected-session bootstrap
  uses the same API as crates/sdk/examples/observe.rs:

  ```rust
  use minecraft_sdk::{Client, Profile};
  #[tokio::main]
  async fn main() -> Result<(), Box<dyn std::error::Error>> {
      let path = std::env::var_os("MC_SDK_PROFILE").ok_or("MC_SDK_PROFILE missing")?;
      let profile: Profile = serde_json::from_slice(&std::fs::read(path)?)?;
      let client = Client::with_profile(&profile)?;
      client.verify_instance(&profile.instance_id).await?;
      let observed = client.observe().await?;
      println!("revision={} health={}", observed.revision, observed.player["health"]);
      Ok(())
  }
  ```

  Keep profile/key/session contents private. Source lookup: Profile and Client
  are defined near the beginning of crates/sdk/src/lib.rs; Action and Observation
  are re-exported from crates/core/src/lib.rs. Read narrow relevant ranges when a
  broad search preview omits the middle of its saved output.
- Observation.player and .world are serde_json::Value; .revision is String.
  player.position is exactly [x,y,z]. A compact context is a sample; a program can
  inspect client.observe().await? for full local facts and collision geometry.
  Both use world.blocks[].position for block coordinates, world.entities for
  entities, and menu.slots for full observed menus. Never invent abbreviated
  field aliases. Current compact views retain the SDK's revision/player/world/menu
  structure and original inventory/menu slot identifiers; omitted cells are
  unknown. Historical journal views may have an older compact format: consult
  the current SDK types and a fresh runtime observation when writing programs.
- Compact and focused menu views add menu.summary from the complete received
  SDK slot snapshot. It counts all non-player slots and their occupied slots,
  with up to sixteen item totals; omitted_item_types reports other types. Null
  means unknown, never empty. A full SDK Observation still exposes menu.slots.
  inspected_containers in model context contains scoped historical inspections,
  with stable entity UUIDs or clicked block positions, receipt hashes and sample
  times. Transient menu/entity IDs are not saved as reusable handles. Contents
  may change; use new observations when evidence warrants another inspection.
  `minecraft-sdk-brain container-memory PRIVATE_CONFIG.json` indexes existing
  receipts and reads historical memory without connecting or calling a model.
- Action::StartMove { target: Position{x,y,z}, jump: bool, max_ticks: u16 } is
  finite, ordinary movement. Action::CancelControl { action_id:String } cancels
  that control. Legacy Move, Jump and MotionFrame are disabled in protected
  coding deployments; old examples using them require adaptation.
- If discovery advertises minecraft:waypoint_passthrough, Action::StartMoveThrough
  { target: Position{x,y,z}, max_ticks:u16 } crosses a dry level-ground waypoint
  without braking to rest. Its native passage receipt is not a precise stopped
  arrival. Passive momentum continues after release; require known safe ground
  beyond the target. Use StartMove for interaction positioning and jumps.
- Block interaction JSON uses kind `place` or `use_block`, not `place_block`.
  block is the clicked block's integer world position. hit is a block-local
  [x,y,z] offset with every coordinate in0..1, not an absolute world position.
  Face numbers are0 down,1 up,2 north,3 south,4 west,5 east; for example a
  top-face hit can be [0.5,1.0,0.5]. Use a fresh observed target and normal reach;
  a valid request still needs its own native effect verification.
- client.act(&ActionRequest{action_id:String,action}).await? returns Receipt.
  Outcome::Running means poll client.action_status(&id).await? until terminal.
  client.perform(&request, Duration::from_secs(10)).await? dispatches once and
  waits using status reads only. wait_for_action(&id, duration) resumes that wait.
  A wait deadline returns the latest receipt, possibly still Running; it never
  replays or cancels the action. Use this for staged mining, crafting, placement,
  inventory transfer and entity interactions as well as finite controls.
  Observed verifies the requested effect. Cancelled, Expired, Failed and Rejected
  are feedback, not success. Reobserve/replan after defensive cancellation.
- Observed dig start proves mining began; observed finish proves a block-state
  change. Neither proves item acquisition. Player inventory is the evidence for
  retained drops; visible item-entity positions are observations, not inventory.
- StartUseItem { hand:Hand, max_ticks:u16 } holds use for a finite native-tick
  budget; CancelUseItem { action_id:String } releases that action's held input.
  Legacy UseItem is also bounded by the deployment control budget. Using an item
  and remaining_use_ticks are progress, not proof of consumption. Item completion
  and attributed attack damage use own-character native post-effect evidence.
  Cancelled/Expired held use can have verified release without a completed effect.
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
- If advertised, minecraft:recipes discovers exact installed recipe IDs using
  recipe, output item or type selectors and bounded offset/limit pagination.
  The Rust recipes::Query helper builds that same read-only query. Recipe IDs
  are distinct from output item IDs or material categories. Inspect known,
  result_known, ingredient omissions and custom/dynamic limitations; follow
  next_offset/complete and retry_after_ms. Queries do not unlock recipes, prove
  ingredients suffice or establish menu compatibility. Unknown recipe IDs and
  locked book entries are refused before dispatch by the matching integration.
  recipes::material_summary(&observation, &recipe_row) groups runtime ingredient
  representatives and pairs them with retained inventory counts. Exact-ID recipe
  queries add this as client_materials when it fits the model view. Compare
  occurrences and representative_count with inventory_count; inventory excludes
  grid, output and cursor. Groups share supplies, alternatives may be incomplete,
  and custom/component matching is not proved. This is planning feedback, not a
  craftability or effect verifier. Missing inventory remains unknown. Recipe and
  inventory samples may differ in time; reobserve before mutating.
- Rust ClickSlot.mode remains u8; click_mode constants cover PICKUP=0,
  QUICK_MOVE=1, SWAP=2, CLONE=3, THROW=4, QUICK_CRAFT=5, PICKUP_ALL=6.
  SDK JSON also accepts the corresponding snake_case names; serialization sends
  the same canonical numeric intent. Unknown modes fail explicitly. Normal player
  authority and stale-menu rejection still apply. A creative-only mode grants
  no creative permissions. Programs retain IDs and inspect uncertainty as usual.
- Use tokio::time for bounded waits; it is enabled in the coding project. Paths,
  model access and SDK endpoints come from deployment, never invented credentials.
- Optional read-only query schemas are advertised in server.limits.observation_queries.
  The brain's query tool calls this same typed interface directly. Use it for a
  discriminating read; prefer a bounded Rust program for a multi-page experiment.
  Call client.read_observation(&ObservationQuery{capability, input}).await?. A
  block_search query accepts a runtime block ID or tag, radius, limit and offset.
  It returns only loaded blocks hit by an unobstructed targeting ray. Follow
  next_offset until complete, respecting retry_after_ms. A partial page with no
  matches is not proof that the area contains no resources. It cannot load code.
- If advertised, minecraft:movement_options returns paged, collision/gravity-
  admitted local waypoints. These are predictions; only executed receipts prove
  movement. Direct query input accepts offset, optional nearby target and limit
  (1–64, default64). For bounded diagnostics use a small limit such as4; follow
  next_offset while scene/origin match, even if an intermediate page is empty.
  Prefer the optional Rust navigation helper over guessing each step:
  navigation::navigate(&client, goal, journal_path, options).await?. Goal::Position
  takes dimension, target:[x,y,z], distance. Goal::BlockReach takes dimension,
  block:[x,y,z], distance and eye_height supplied for the character (upright Java
  player:1.62; use an appropriate interaction range). Choose the block from fresh
  observations. The helper uses a persistent local graph and finite controls;
  it never mines, teleports, breaks obstacles or chooses your task.
  The brain's navigate tool composes this same helper through its body arbiter;
  use it for a bounded route rather than one model turn per local waypoint.
  Completed read pages survive operation deadlines. Resume the same goal and
  journal; cached pages are validated against the current scene before use.
  A normal brain-tool slice returns navigation_slice.can_resume=true with its
  typed goal while success/completed remain false. It is not a tool error or
  proof of progress. Reuse that goal for continuation rather than discarding the
  graph; inspect genuine failures and repeated slices without useful new work.
  A read cursor is not gameplay progress, and pending action IDs still require
  inspection/reconciliation rather than replay. The matching adapter prunes
  endpoints that cannot contain a legal player pose before trajectory search.
  With alpha12 minecraft:fluid_facts, the adapter predicts still-water medium
  transitions and offers observed surface-water waypoints. Fluid height/tags/flow
  come from native observations, including waterlogged cells; currents, lava or
  unsupported fluid physics fail explicitly. A finite swim releases input on
  completion; it does not hold position while planning. The independent body
  permits active surface swimming only with native eyes-clear/full-air facts,
  and protects a submerged character while reasoning or compilation continues.
- Put its journal under MC_SDK_STATE, using a distinct file per goal. Reuse that
  journal to resume; pending IDs are inspected rather than replayed. Options
  controls operation deadline, actions, nodes and caller-supplied forbidden block
  IDs. Inspect status/remaining_distance/pending_action in the report. reached
  for BlockReach means a candidate interaction distance, not an obtained item.
  Keep the defensive arbiter enabled and verify the requested game effect.
