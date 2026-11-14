//! One prepared recipe batch, taken into the cursor and stored in own inventory.
//! This client composition is not a server transaction or native certification.
use super::{FRESH_OBSERVATION_CAPABILITY, click_slot, item_count, observe_fresh, prepare_recipe};
use crate::*;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
mod inspection;
pub use inspection::{CraftInspection, CraftInspectionStatus, inspect_craft_journal};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CraftStatus {
    Completed,
    AlreadyCompleted,
    Pending,
    LocalPreempted,
    NeedsInspection,
    Stopped,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CraftPhase {
    Prepare,
    Take,
    Store,
    Completed,
}
impl CraftPhase {
    fn suffix(self) -> &'static str {
        match self {
            Self::Prepare => "prepare",
            Self::Take => "take",
            Self::Store => "store",
            Self::Completed => "store",
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CraftReport {
    pub status: CraftStatus,
    pub phase: CraftPhase,
    pub action_id: Option<String>,
    pub item: Option<String>,
    pub output_count: Option<u64>,
    /// Historical count just before storing this cursor stack.
    pub inventory_before_store: Option<u64>,
    /// Fresh count for this invocation; absent when no observation was available.
    pub current_inventory_count: Option<u64>,
    pub receipt_outcome: Option<Outcome>,
    pub error: Option<ApiError>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    menu_id: i32,
    menu_type: String,
    slot: i16,
    item: String,
    count: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u8,
    instance: String,
    actor: String,
    life_id: String,
    recipe: String,
    prefix: String,
    phase: CraftPhase,
    pending: Option<ActionRequest>,
    output: Option<Output>,
    before_store: Option<u64>,
}
struct Journal {
    path: PathBuf,
    state: State,
    _lock: File,
}
static NEXT: AtomicU64 = AtomicU64::new(0);
fn error(code: ErrorCode, message: impl Into<String>) -> ApiError {
    ApiError::new(code, message)
}
fn local(e: impl std::fmt::Display) -> ApiError {
    error(ErrorCode::Internal, format!("Local craft journal: {e}"))
}
fn require(ok: bool, message: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(error(ErrorCode::NotReady, message))
    }
}
fn private_options() -> OpenOptions {
    let mut o = OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o
}
fn check_file(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path).map_err(local)?;
    require(
        meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= 16384,
        "Expected a bounded regular craft journal",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        require(
            meta.permissions().mode() & 0o077 == 0,
            "Craft journal must be private",
        )?;
    }
    Ok(())
}
impl Journal {
    fn open(
        path: &Path,
        instance: &str,
        actor: &str,
        life_id: &str,
        recipe: &str,
        prefix: &str,
    ) -> Result<Self> {
        require(
            !life_id.is_empty(),
            "Native per-life identity required for crafting",
        )?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent).map_err(local)?;
        let lock_path = path.with_extension("craft-lock");
        if fs::symlink_metadata(&lock_path).is_ok() {
            check_file(&lock_path)?;
        }
        let mut opts = OpenOptions::new();
        opts.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let lock = opts.open(lock_path).map_err(local)?;
        lock.try_lock().map_err(|_| {
            error(
                ErrorCode::Conflict,
                "Another craft invocation owns this journal",
            )
        })?;
        let state = if fs::symlink_metadata(path).is_ok() {
            check_file(path)?;
            let state: State =
                serde_json::from_slice(&fs::read(path).map_err(local)?).map_err(local)?;
            require(
                state.version == 2
                    && state.instance == instance
                    && state.actor == actor
                    && state.life_id == life_id
                    && state.recipe == recipe
                    && state.prefix == prefix,
                "Craft journal belongs to another operation, instance, character or life",
            )?;
            if let Some(request) = &state.pending {
                require(
                    request.action_id == format!("{}-{}", state.prefix, state.phase.suffix()),
                    "Craft pending action identity changed",
                )?;
            }
            state
        } else {
            State {
                version: 2,
                instance: instance.into(),
                actor: actor.into(),
                life_id: life_id.into(),
                recipe: recipe.into(),
                prefix: prefix.into(),
                phase: CraftPhase::Prepare,
                pending: None,
                output: None,
                before_store: None,
            }
        };
        let journal = Self {
            path: path.to_owned(),
            state,
            _lock: lock,
        };
        journal.save()?;
        Ok(journal)
    }
    fn save(&self) -> Result<()> {
        let bytes = serde_json::to_vec(&self.state).map_err(local)?;
        require(bytes.len() <= 16384, "Craft journal exceeds storage budget")?;
        let temp = self.path.with_extension(format!(
            "craft-next-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| {
            let mut f = private_options().open(&temp).map_err(local)?;
            f.write_all(&bytes).map_err(local)?;
            f.sync_all().map_err(local)?;
            fs::rename(&temp, &self.path).map_err(local)?;
            #[cfg(unix)]
            File::open(
                self.path
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new(".")),
            )
            .and_then(|f| f.sync_all())
            .map_err(local)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result
    }
    fn report(
        &self,
        status: CraftStatus,
        receipt: Option<&Receipt>,
        current: Option<u64>,
        err: Option<ApiError>,
    ) -> CraftReport {
        CraftReport {
            status,
            phase: self.state.phase,
            action_id: self
                .state
                .pending
                .as_ref()
                .map(|r| r.action_id.clone())
                .or_else(|| receipt.map(|r| r.action_id.clone())),
            item: self.state.output.as_ref().map(|o| o.item.clone()),
            output_count: self.state.output.as_ref().map(|o| o.count),
            inventory_before_store: self.state.before_store,
            current_inventory_count: current,
            receipt_outcome: receipt.map(|r| r.outcome.clone()),
            error: err.or_else(|| receipt.and_then(|r| r.error.clone())),
        }
    }
}
fn cursor_empty(o: &Observation) -> bool {
    o.menu["carried"]["item"] == "minecraft:air" && o.menu["carried"]["count"] == 0
}
fn actor(o: &Observation, expected: &str, life_id: &str) -> Result<()> {
    require(
        o.player["uuid"] == expected && o.player["life_id"] == life_id,
        "Observed craft character or native life changed; inspect without replay",
    )
}
fn empty_grid(o: &Observation) -> Result<()> {
    require(
        cursor_empty(o),
        "Resolve the current cursor before preparing another recipe",
    )?;
    let slots = o.menu["slots"]
        .as_array()
        .filter(|s| !s.is_empty() && s.len() <= 512)
        .ok_or_else(|| error(ErrorCode::NotReady, "Native menu slots unavailable"))?;
    let mut non_player = 0;
    for row in slots {
        let player = row["player_inventory"]
            .as_bool()
            .ok_or_else(|| error(ErrorCode::NotReady, "Native slot ownership unavailable"))?;
        if !player {
            non_player += 1;
            require(
                row["count"] == 0 && row["item"] == "minecraft:air",
                "Resolve existing menu inputs/output before preparing a single new batch",
            )?;
        }
    }
    require(non_player > 0, "No observed recipe menu slots")
}
impl Output {
    fn bind(recipe: &str, receipt: &Receipt) -> Result<Self> {
        let e = &receipt.evidence["effect"];
        if e["kind"] != "recipe_preparation" || e["recipe"] != recipe {
            return Err(error(
                ErrorCode::UnsupportedCapability,
                "Matching native recipe-preparation effect metadata required",
            ));
        }
        let result = Self {
            menu_id: e["menu_id"]
                .as_i64()
                .and_then(|n| i32::try_from(n).ok())
                .filter(|n| *n >= 0)
                .ok_or_else(|| error(ErrorCode::NotReady, "Preparation menu identity missing"))?,
            menu_type: e["menu_type"]
                .as_str()
                .filter(|s| registry_id(s))
                .ok_or_else(|| error(ErrorCode::NotReady, "Preparation menu type missing"))?
                .into(),
            slot: e["output_slot"]
                .as_i64()
                .and_then(|n| i16::try_from(n).ok())
                .filter(|n| *n >= 0)
                .ok_or_else(|| error(ErrorCode::NotReady, "Preparation output slot missing"))?,
            item: e["result"]["item"]
                .as_str()
                .filter(|s| registry_id(s) && *s != "minecraft:air")
                .ok_or_else(|| error(ErrorCode::NotReady, "Preparation result item missing"))?
                .into(),
            count: e["result"]["count"]
                .as_u64()
                .filter(|n| *n > 0 && *n <= u64::from(u32::MAX))
                .ok_or_else(|| error(ErrorCode::NotReady, "Preparation result count missing"))?,
        };
        Ok(result)
    }
    fn menu(&self, o: &Observation) -> Result<()> {
        require(
            o.menu["menu_id"] == self.menu_id && o.menu["type"] == self.menu_type,
            "Recipe menu changed; inspect current materials before a new operation",
        )
    }
    fn ready(&self, o: &Observation) -> Result<()> {
        self.menu(o)?;
        require(
            cursor_empty(o),
            "Cursor is occupied before taking the recipe result",
        )?;
        let row = o.menu["slots"]
            .as_array()
            .and_then(|s| s.iter().find(|r| r["slot"] == self.slot))
            .ok_or_else(|| error(ErrorCode::NotReady, "Prepared output slot is absent"))?;
        require(
            row["player_inventory"] == false
                && row["item"] == self.item
                && row["count"] == self.count
                && row["may_pickup"] == true,
            "Prepared output or its pickup permission changed",
        )
    }
    fn carried(&self, o: &Observation) -> Result<()> {
        self.menu(o)?;
        require(
            o.menu["carried"]["item"] == self.item && o.menu["carried"]["count"] == self.count,
            "Cursor does not contain the single prepared output batch",
        )
    }
}
fn build(state: &mut State, o: &Observation) -> Result<ActionRequest> {
    let action = match state.phase {
        CraftPhase::Prepare => {
            empty_grid(o)?;
            prepare_recipe(o, &state.recipe, false)?
        }
        CraftPhase::Take => {
            let output = state
                .output
                .as_ref()
                .ok_or_else(|| local("Prepared output binding missing"))?;
            output.ready(o)?;
            click_slot(o, output.slot, 0, click_mode::PICKUP)?
        }
        CraftPhase::Store => {
            let output = state
                .output
                .as_ref()
                .ok_or_else(|| local("Prepared output binding missing"))?;
            output.carried(o)?;
            let slot=o.menu["slots"].as_array().and_then(|slots|slots.iter().find(|r|r["player_inventory"]==true&&r["item"]=="minecraft:air"&&r["count"]==0&&r["may_place_carried"]==true))
                .and_then(|r|r["slot"].as_i64()).and_then(|n|i16::try_from(n).ok()).ok_or_else(||error(ErrorCode::NotReady,"No fresh empty player slot permits the carried output; preserve cursor and inspect inventory"))?;
            state.before_store = Some(item_count(o, &output.item)?);
            click_slot(o, slot, 0, click_mode::PICKUP)?
        }
        CraftPhase::Completed => return Err(local("Completed craft has no next mutation")),
    };
    Ok(ActionRequest {
        action_id: format!("{}-{}", state.prefix, state.phase.suffix()),
        action,
    })
}
fn advance(state: &mut State, receipt: &Receipt, o: &Observation) -> Result<()> {
    match state.phase {
        CraftPhase::Prepare => {
            let output = Output::bind(&state.recipe, receipt)?;
            output.ready(o)?;
            state.output = Some(output);
            state.phase = CraftPhase::Take;
        }
        CraftPhase::Take => {
            state
                .output
                .as_ref()
                .ok_or_else(|| local("Prepared output binding missing"))?
                .carried(o)?;
            state.phase = CraftPhase::Store;
        }
        CraftPhase::Store => {
            let output = state
                .output
                .as_ref()
                .ok_or_else(|| local("Prepared output binding missing"))?;
            output.menu(o)?;
            require(
                cursor_empty(o),
                "Stored result still occupies the cursor; inspect without replay",
            )?;
            let before = state
                .before_store
                .ok_or_else(|| local("Inventory baseline missing"))?;
            let after = item_count(o, &output.item)?;
            require(
                after.checked_sub(before) == Some(output.count),
                "Fresh retained inventory does not show exactly the stored output batch; inspect without replay",
            )?;
            state.phase = CraftPhase::Completed;
        }
        CraftPhase::Completed => return Err(local("Completed craft cannot advance")),
    }
    state.pending = None;
    Ok(())
}

/// Prepare exactly one recipe placement (`make_all=false`), PICKUP one output
/// batch, store its cursor stack in an empty permitted player slot and verify
/// the retained count. Native preparation metadata supplies the output binding.
///
/// Keep one private journal and unique prefix per intended batch. Every exact
/// request is saved before dispatch. Resuming a pending step reads its original
/// status, never replays it. LocalPreempted means the local body submitted
/// nothing and permits a later retry. Uncertain/reconciled/rejected outcomes stop
/// for explicit inspection/reconciliation; they do not become a new mutation.
/// A wait-budget error leaves the journal intact and does not establish failure.
/// Repeat with the same journal to inspect; do not discard it or invent new IDs.
///
/// No recipe IDs, server address or menu layout is built in. This requires a
/// matching native recipe-preparation verifier and an initially empty grid and
/// cursor. The adapter must advertise `minecraft:fresh_observation`; each
/// existing observation is one explicit bounded native query, with no polling
/// or cached fallback. Full terrain/entities are included. The default cached
/// body read remains separate; genuinely concurrent revision changes still stop.
/// Native `player.life_id` is required; the journal binds its initial
/// life and refuses continuation after death/respawn even if menu IDs match.
/// Pre-life-identity integrations and version 1 journals require operator
/// inspection rather than silent migration. Dynamic/custom component semantics
/// are not certified. This is not atomic: concurrent player/body changes can cause a conservative stop with
/// materials still in the menu/cursor. A completed journal reports history rather
/// than producing another batch. Use distinct prefixes/journals to compose N batches.
pub async fn craft_once(
    client: &Client,
    recipe: &str,
    prefix: &str,
    path: &Path,
    wait: Duration,
) -> Result<CraftReport> {
    if !registry_id(recipe) || !valid_id(prefix) || prefix.len() > 96 || wait.is_zero() {
        return Err(error(
            ErrorCode::InvalidRequest,
            "Runtime recipe ID, unique prefix(up to96bytes), private journal and nonzero wait budget required",
        ));
    }
    let deadline = tokio::time::Instant::now()
        .checked_add(wait)
        .ok_or_else(|| error(ErrorCode::InvalidRequest, "Craft wait exceeds clock bounds"))?;
    tokio::time::timeout_at(deadline,async {
        let info=client.discover().await?;
        if client.expected_instance.as_deref().is_some_and(|id|id!=info.instance_id) {return Err(error(ErrorCode::Conflict,"Gateway instance changed before crafting"));}
        if !info.capabilities.iter().any(|c|c==FRESH_OBSERVATION_CAPABILITY) {return Err(error(ErrorCode::UnsupportedCapability,"Journaled crafting requires advertised minecraft:fresh_observation; cached observations are not a fallback"));}
        let own=client.credential.as_ref().ok_or_else(||error(ErrorCode::Unauthorized,"Authenticated craft character required"))?.player_uuid.clone();
        let initial=observe_fresh(client).await?;
        let life_id=initial.player["life_id"].as_str().filter(|s|!s.is_empty()).ok_or_else(||error(ErrorCode::UnsupportedCapability,"Native per-life identity required for crafting"))?;
        actor(&initial,&own,life_id)?;
        let mut journal=Journal::open(path,&info.instance_id,&own,life_id,recipe,prefix)?;
        loop {
            let observation=observe_fresh(client).await?;actor(&observation,&own,&journal.state.life_id)?;
            if journal.state.phase==CraftPhase::Completed {
                let current=journal.state.output.as_ref().map(|o|item_count(&observation,&o.item)).transpose()?;
                return Ok(journal.report(CraftStatus::AlreadyCompleted,None,current,None));
            }
            let dispatching=journal.state.pending.is_none();
            let receipt=if let Some(request)=&journal.state.pending {
                client.wait_for_action(&request.action_id,deadline.saturating_duration_since(tokio::time::Instant::now())).await
            }else{
                let request=build(&mut journal.state,&observation)?;journal.state.pending=Some(request.clone());journal.save()?;
                client.perform(&request,deadline.saturating_duration_since(tokio::time::Instant::now())).await
            };
            let receipt=match receipt {
                Ok(receipt)=>receipt,
                Err(error) if dispatching && error.code==ErrorCode::LocalPreempted=>{
                    journal.state.pending=None;journal.save()?;return Ok(journal.report(CraftStatus::LocalPreempted,None,None,Some(error)));
                },
                Err(error)=>return Ok(journal.report(CraftStatus::NeedsInspection,None,None,Some(error))),
            };
            require(journal.state.pending.as_ref().is_some_and(|r|r.action_id==receipt.action_id),"Craft receipt identity changed")?;
            if dispatching && receipt.evidence["gateway_delivery"]["action_id_already_recorded"]==true {
                return Ok(journal.report(CraftStatus::NeedsInspection,Some(&receipt),None,None));
            }
            if receipt.outcome!=Outcome::Observed || receipt.evidence["native_effect_verified"]!=true {
                let status=match receipt.outcome {Outcome::Prepared|Outcome::Dispatched|Outcome::Running=>CraftStatus::Pending,Outcome::Uncertain|Outcome::Reconciled|Outcome::Observed=>CraftStatus::NeedsInspection,_=>CraftStatus::Stopped};
                return Ok(journal.report(status,Some(&receipt),None,None));
            }
            let after=observe_fresh(client).await?;actor(&after,&own,&journal.state.life_id)?;
            advance(&mut journal.state,&receipt,&after)?;journal.save()?;
            if journal.state.phase==CraftPhase::Completed {
                let current=journal.state.output.as_ref().map(|o|item_count(&after,&o.item)).transpose()?;
                return Ok(journal.report(CraftStatus::Completed,Some(&receipt),current,None));
            }
        }
    }).await.map_err(|_|error(ErrorCode::NotReady,"Craft wait budget reached; the journal may contain a running request. Resume the same journal to inspect its original ID, without replay"))?
}

#[cfg(test)]
mod tests;
