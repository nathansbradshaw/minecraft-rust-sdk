//! Read-only client bookkeeping. Never native settlement or permission to replay.
use super::*;
use std::io::Read;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CraftInspectionStatus {
    BoundPending,
    BoundCheckpoint,
    CompletedHistory,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CraftInspection {
    pub status: CraftInspectionStatus,
    pub binding_verified: bool,
    pub instance: String,
    pub actor: String,
    pub recipe: String,
    pub prefix: String,
    pub recorded_life_id: Option<String>,
    pub current_life_verified: bool,
    pub phase: Option<CraftPhase>,
    pub pending_action_id: Option<String>,
    pub source_bytes: Option<u64>,
    /// Always unknown here. Only authoritative status/reconciliation can settle.
    pub native_settled: Option<bool>,
    pub error: Option<ApiError>,
}

fn readonly(path: &Path) -> Result<File> {
    check_file(path)?;
    #[cfg(unix)]
    let before = fs::symlink_metadata(path).map_err(local)?;
    let file = File::open(path).map_err(local)?;
    let opened = file.metadata().map_err(local)?;
    require(
        opened.is_file() && opened.len() <= 16384,
        "Expected a bounded regular craft journal",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        require(
            opened.permissions().mode() & 0o077 == 0
                && before.dev() == opened.dev()
                && before.ino() == opened.ino(),
            "Craft journal changed during read-only open",
        )?;
    }
    let current = fs::symlink_metadata(path).map_err(local)?;
    require(
        current.is_file() && !current.file_type().is_symlink(),
        "Craft journal path changed during read-only open",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        require(
            current.dev() == opened.dev() && current.ino() == opened.ino(),
            "Craft journal path changed during read-only open",
        )?;
    }
    Ok(file)
}

/// Inspect a previously recorded operation without creating directories, lock
/// files, journals or new intentions. The SDK caller supplies its own local
/// journal path; closed gameplay tools must derive it from their validated
/// operation ID, never accept a model-provided filesystem path.
///
/// Missing, busy, invalid and foreign journals are Unknown, never "not started"
/// or settled. A bound no-pending checkpoint is recorded client state only.
/// CompletedHistory does not prove current retention. Recorded life is historical;
/// normal craft_once still requires a fresh matching life before execution.
pub fn inspect_craft_journal(
    path: &Path,
    instance: &str,
    actor: &str,
    recipe: &str,
    prefix: &str,
) -> CraftInspection {
    let mut result = CraftInspection {
        status: CraftInspectionStatus::Unknown,
        binding_verified: false,
        instance: instance.into(),
        actor: actor.into(),
        recipe: recipe.into(),
        prefix: prefix.into(),
        recorded_life_id: None,
        current_life_verified: false,
        phase: None,
        pending_action_id: None,
        source_bytes: None,
        native_settled: None,
        error: None,
    };
    let inspect = || -> Result<(State, u64)> {
        require(
            !instance.is_empty()
                && !actor.is_empty()
                && registry_id(recipe)
                && valid_id(prefix)
                && prefix.len() <= 96,
            "Invalid expected craft operation binding",
        )?;
        // Unlike Journal::open, open only an EXISTING lock read-only and never
        // save. Holding the same lock makes a cooperative writer's snapshot stable.
        let lock = readonly(&path.with_extension("craft-lock"))?;
        lock.try_lock().map_err(|_| {
            error(
                ErrorCode::Conflict,
                "Craft journal is busy or unavailable for inspection",
            )
        })?;
        let file = readonly(path)?;
        let mut bytes = Vec::new();
        file.take(16385).read_to_end(&mut bytes).map_err(local)?;
        require(
            bytes.len() <= 16384,
            "Craft journal exceeds inspection budget",
        )?;
        let state: State = serde_json::from_slice(&bytes).map_err(|_| {
            error(
                ErrorCode::NotReady,
                "Craft journal is invalid; original operation remains unknown",
            )
        })?;
        require(
            state.version == 2
                && state.instance == instance
                && state.actor == actor
                && state.recipe == recipe
                && state.prefix == prefix
                && !state.life_id.is_empty(),
            "Craft journal belongs to another operation, instance or character",
        )?;
        if let Some(request) = &state.pending {
            require(
                state.phase != CraftPhase::Completed
                    && request.action_id == format!("{}-{}", state.prefix, state.phase.suffix()),
                "Craft pending action identity or phase changed",
            )?;
            request.action.validate()?;
            let compatible = match (&state.phase, &request.action) {
                (
                    CraftPhase::Prepare,
                    Action::Craft {
                        recipe: actual,
                        make_all,
                        ..
                    },
                ) => actual.as_str() == recipe && !*make_all,
                (CraftPhase::Take | CraftPhase::Store, Action::ClickSlot { mode, button, .. }) => {
                    *mode == click_mode::PICKUP && *button == 0
                }
                _ => false,
            };
            require(compatible, "Craft pending action does not match its phase")?;
        }
        // No raw file bytes/paths or unvalidated foreign fields leave this reader.
        Ok((state, bytes.len() as u64))
    };
    match inspect() {
        Ok((state, size)) => {
            result.status = if state.pending.is_some() {
                CraftInspectionStatus::BoundPending
            } else if state.phase == CraftPhase::Completed {
                CraftInspectionStatus::CompletedHistory
            } else {
                CraftInspectionStatus::BoundCheckpoint
            };
            result.binding_verified = true;
            result.recorded_life_id = Some(state.life_id);
            result.phase = Some(state.phase);
            result.pending_action_id = state.pending.map(|r| r.action_id);
            result.source_bytes = Some(size);
        }
        Err(error) => result.error = Some(error),
    }
    result
}
