SDK quick reference (Lua behaviors; Rust SDK and Brain):

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
- Observed DigStage::Start proves mining began or an observed block-state change.
  Start plus sleeping does not finish a noninstant block. After an Observed Start,
  if the fresh target still needs finishing, submit one DigStage::Finish with its
  own durable ID. Drain that ID with action_status/wait_for_action while Running.
  Observed Finish proves a block-state change, not pickup.
  Inspect Uncertain through its original ID and explicit reconciliation; never
  duplicate the mutation. Reconciliation does not prove the historical effect.
  mining.active=false is not completion proof: Finish can queue delayed native
  destruction. Even an Expired observer does not exclude a later block change.
- A block-state change does not prove retained loot; tool and server drop rules
  still apply. Pickup happens automatically near dropped item entities when
  inventory capacity and pickup delay allow it; Interact is not a pickup action.
  When minecraft:fresh_observation is advertised, use menu::observe_fresh and
  compare menu::item_count before/after.
  Visible item entities and a successful mining-range movement are not inventory.
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
  A repeated act request can return evidence.gateway_delivery with
  action_id_already_recorded=true and new_intention_dispatched=false. This
  delivers the old receipt, including any old verified effect; it does not
  execute that intention again. A learned program must inspect its existing
  IDs and current postconditions, then use fresh IDs for distinct new work.
- Print compact action ID/outcome/error and relevant before/after facts, not full
  world receipts every frame. Authoritative evidence stays in the gateway journal.
- Generic menu builders: menu::prepare_recipe(&observation, recipe_id, make_all)
  builds Action::Craft for the observed menu. This prepares a recipe; it does not
  take its output or establish inventory gain. Inspect observation.menu.slots and
  use menu::click_slot(&observation, slot, button, click_mode::QUICK_MOVE) with a
  fresh observation when appropriate. The caller chooses slots and verifies effects.
  SelectSlot accepts hotbar indices0..8, not storage indices. To equip a stored
  stack, use an observed menu-slot SWAP and verify the displaced stack as well;
  docs/menus.md describes the slot mapping and fresh postconditions.
  menu::item_count(&observation, item_id) counts retained player inventory only,
  excluding a prepared output or carried cursor stack. Do not print every air slot.
  The default brain menu.non_player_stacks view preserves bounded occupied slot
  bindings, separate from player.inventory and menu.carried. Follow its complete
  and omitted_occupied_slots fields; absent slots are not proved empty. Inspect
  native roles and fresh menu/state IDs before clicking. Prepare, take output,
  and verify retained inventory before preparing a dependent recipe. A prepared
  ingredient in the output/grid has not yet been acquired to unlock later recipes.
  Optional `menu::craft_once(&client, recipe_id, unique_prefix, &journal_path,
  Duration::from_secs(15)).await?` composes one preparation, one output PICKUP,
  and storage in an empty permitted own slot, then checks retained item/count gain.
  The optional `craft_once` brain tool calls this same helper without a compile
  or a model turn for each click. Input is only
  `{"recipe":"namespace:installed_recipe","operation_id":"unique_batch_1"}`;
  leave `path` empty. The harness resolves its journal under this bot's state,
  binds the actor/server, and runs through the existing per-action body arbiter.
  Reuse the SAME operation ID after preemption/interruption, including after a
  worker timeout. A different ID means distinct new work, never a retry shortcut.
  Recipe changes with the same operation or life changes stop at the journal's
  binding check. Prepare/take/store can pause independently; the cursor may hold
  the output until the body permits continuation. Inspect original IDs when
  requested. `historical_completion` does not prove current retention or a new
  batch. This built-in client helper is optional, not a learned skill or a server
  transaction; keep the existing gameplay tools and editable program strategy.
  Supply ingredients and an appropriate open menu with empty grid/cursor. It uses
  native preparation metadata, not guessed output slots. It never uses make_all
  or QUICK_MOVE. Each intended batch needs its own durable prefix/journal; repeat
  the same journal after interruption to inspect the same requests without replay.
  Native player.life_id is required and persisted; a life change stops the journal
  even if menu IDs match. Old providers or version 1 journals require inspection,
  never replacement action IDs. Use the matching life-identity module/adapter.
  The adapter must also advertise minecraft:fresh_observation. The helper uses
  menu::observe_fresh(&client) for its existing reads: one explicit bounded native
  query, without cached fallback or polling. Full terrain/entities are included,
  so this is heavier than the cached default body observation. Fresh revisions
  can still change before a click; rejection stops rather than rebasing or retrying.
  LocalPreempted means no submission and permits a later retry. Pending/NotReady
  can still be running; Uncertain/Reconciled/NeedsInspection require original-ID
  inspection/reconciliation and fresh facts. Do not discard state to retry. A
  completed journal returns AlreadyCompleted without new crafting; check current
  inventory separately. This is not atomic or native benchmark certification;
  concurrent inventory/menu changes cause conservative stops. See the SDK
  `craft_once` example. Custom component semantics remain unverified.
- If advertised, minecraft:recipes discovers exact installed recipe IDs using
  recipe, output item or type selectors and bounded offset/limit pagination.
  The Rust recipes::Query helper builds that same read-only query. Recipe IDs
  are distinct from output item IDs or material categories. Inspect known,
  result_known, ingredient omissions and custom/dynamic limitations; follow
  next_offset/complete and retry_after_ms. Queries do not unlock recipes, prove
  ingredients suffice or establish menu compatibility. Unknown recipe IDs and
  locked book entries are refused before dispatch by the matching integration.
  recipes::material_summary(&observation, &recipe_row) groups runtime ingredient
  representatives and pairs them with retained inventory counts. Recipe queries
  returning one row add this as client_materials when it fits the model view.
  A partial search can have other candidates; follow its original pagination. Compare
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
  matches is not proof that the area contains no resources. A match need not be
  within interaction range. Its item is the block's item form, not a loot promise.
  It cannot load code.
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
  In generated programs, use navigation::Options { eager_progress:true,
  ..Default::default() } for short handoffs between validated movement steps.
  This opt-in can act on a fresh, strictly improving preview prefix instead of
  waiting for the entire local graph; native admission and full-search fallback
  still apply. The brain's navigate tool already enables it.
  For caller-chosen acquisition, Options also accepts until_inventory:
  Some(navigation::RetainedInventory { item:actual_runtime_item_id, minimum:n }).
  A fresh same-life retained count can end that route as inventory_satisfied,
  after original pending input is settled. This is not positional arrival or
  parent-task completion. Cursor/menu outputs do not count. Bare goals remain
  unchanged; unrelated item gains never globally stop navigation. The brain tool
  exposes the same opt-in as {"goal":NORMAL_GOAL,"until_inventory":{"item":ACTUAL_ID,"minimum":n}}.
  inventory_missing means the approach did not satisfy collection; inspect the
  actual resource. Preserve/reconcile any needs_reconciliation original ID.
  After no_known_route, the brain's observe tool accepts {"view":"terrain"}.
  This read-only view groups current observed cells around the character into
  support, feet, head and above_head layers, with absolute positions and native
  collision/permission facts. Missing cells remain unknown, not air or safe.
  Follow page.next_offset while stationary. Diagnose the local constraint before
  choosing a new method; changing distant targets cannot open the same graph.
  This view neither chooses blocks to break nor predicts a safe passage.
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
  The foreground navigate helper permits these native water predictions instead
  of treating every wet boundary as impassable. Its other hazard exclusions stay
  active. Defensive geometry retains its own water policy; drowning protection
  can still preempt a foreground swim. Neither prediction nor dispatch proves arrival.
- Put its journal under MC_SDK_STATE, using a distinct file per goal. Reuse that
  journal to resume; pending IDs are inspected rather than replayed. Options
  controls operation deadline, actions, nodes and caller-supplied forbidden block
  IDs. Inspect status/remaining_distance/pending_action in the report. reached
  for BlockReach means a candidate interaction distance, not an obtained item.
  Keep the defensive arbiter enabled and verify the requested game effect.
- Lua behaviors (AI-edited scripts; resident Rust execution):

The Rust Brain embeds Lua 5.4 using mlua. Use write/edit for relative `.lua`
files, then `run` with that path. No interpreter install or Rust build is needed.

```lua
return {
  version = 1,
  step = function(ctx)
    if ctx.state.looked then
      -- Inspect the original receipt, rather than assuming dispatch succeeded.
      return { state = ctx.state, done = true }
    end
    return {
      state = { looked = true },
      command = { tool = "sdk", input = { kind = "look", yaw = 0, pitch = 0 } }
    }
  end
}
```

`ctx` supplies `state` (initially `{}`), `last_result`, the full SDK `observation`,
`goal`, and `goal_revision`. The return contains `state` plus exactly one of
`command` or `done=true`. Commands: `observe` (no input), `query` (SDK
ObservationQuery table), `sdk` (SDK Action table), `status` / `reconcile` (original
action ID string), and `navigate` (SDK navigation Goal table). Action/query fields
match the JSON forms in this reference; use Lua tables, not JSON strings.
SDK results retain `request`, `receipt`, and `after`; status/reconcile return a bare
receipt. Results above32 KiB keep the same receipt layout with exact action ID,
outcome, verification/release flags (unknown is null), and bounded errors.
`feedback_view` marks omitted native evidence and hashes the original view;
`after` may be a focused historical view. Full SDK receipts remain in the Brain
journal. Use `ctx.observation` for current inventory/scene, never infer an effect
from omission, dispatch, or a terminal non-success outcome. Pending IDs are still
inspected without replay before Lua advances.
Observe returns a compact overview; `ctx.observation` retains the full snapshot.
In particular, raw sounds are in `ctx.observation.world.auditory.events`.
The compact model/observe view calls its limited sound list `recent_emissions`;
that name is not the raw SDK field. See the Lua delivered sounds section below.

Each step is a pure decision. Rust validates and executes its one operation.
Scripts cannot directly call the network, filesystem or shell. Only saved JSON
state persists; local/global Lua variables do not. State is scoped to character,
instance, workspace path and task revision. Source updates preserve that state.
Use a new path for a deliberately new program; a new task revision also starts
fresh state. Historical source and state never authorize replaying an uncertain
game action. The Rust executor still assigns and journals every action ID.

Changed files are validated before activation between settled actions. An invalid
candidate retains the previous source and state, disclosed as `rejected_update`.
Replacing source never replaces the Brain/body/session. Pending controls must
settle before another script step or update. Each invocation is a slice of at
most128 operations and the configured tool timeout; `done=false` with
`slice_exhausted=true` means run the same path to continue. Runtime/dispatch
failure fences the checkpoint: inspect/reconcile original IDs, then write a
reviewed successor without repeating unverified mutations. Lua errors are
returned with current `lua_source` for exact edits.

The VM allows math/string/table/utf8 and ordinary base operations. Limits are
32768 source bytes,16384 saved JSON state bytes,16 MiB VM memory,100000 Lua
instructions and a checked100ms computation deadline per step. Host operations
use the separate tool timeout. No `os`, `io`, `package`, `debug`, `require`,
dynamic code loading, `pcall`/`xpcall`, or coroutine library is exposed.

- Lua delivered sounds (full auditory.events, not compact recent_emissions):

### Capturing delivered sound facts

This one-shot recorder saves at most eight recent delivered emissions. It does
not run continuously while the model is thinking; the independent Rust body
handles urgent protection. Run the script again for another snapshot, or return
bounded commands from a behavior to keep sampling between its settled actions.

```lua
return {
  version = 1,
  step = function(ctx)
    local observation = ctx.observation
    local player = observation.player
    local frame = observation.world.auditory
    local hearing = { available = false }
    if frame and frame.format == "minecraft-sdk-auditory-v1"
        and type(frame.events) == "table" then
      hearing = {
        available = true,
        epoch = frame.epoch,
        listener_uuid = frame.listener_uuid,
        dimension = frame.dimension,
        sample_game_time = frame.sample_game_time,
        latest_sequence = frame.latest_sequence,
        retention_ticks = frame.retention_ticks,
        dropped = frame.dropped,
        retained_count = #frame.events,
        omitted_count = math.max(0, #frame.events - 8),
        events = {}
      }
      for i = math.max(1, #frame.events - 7), #frame.events do
        hearing.events[#hearing.events + 1] = frame.events[i]
      end
    end
    return {
      state = {
        player_uuid = player.uuid,
        life_id = player.life_id,
        dimension = player.dimension,
        health = player.health,
        observed_game_time = observation.world.game_time,
        auditory = hearing
      },
      done = true
    }
  end
}
```

Each event retains `sequence`, `sound_id`, `category`, `origin`, `game_time`,
`volume`, `pitch`, `delivery`, `server_range`, and `distance_at_delivery`.
`origin` is the delivered emission location, not an unseen entity's identity or
current position. Do not invent a shooter or a current enemy path from a sound.
`available=false` means the expected frame was unavailable, not silence.
`dropped` reports server-reported cue loss; `omitted_count` reports this recorder's
bound. Even an available empty frame only describes its retained time window.

For repeated cue processing, retain an epoch/sequence cursor scoped to player,
`life_id` and dimension; reset it when that scope changes. Process newer
sequences once, check age using `sample_game_time` and event `game_time`, and
retain loss/availability information. Lua state survives respawn and dimension
changes within a task; old plans or health snapshots do not prove survival of
the original life. Projectile motion is separate native evidence in
`observation.world.entities`: inspect `projectile`, `position` and `velocity`
when provided, instead of reducing every cue to an entity type or sound ID.
