//! Optional bounded navigation. Goals/policies belong to callers; predictions
//! never replace ordinary permissions or native action receipts.
use crate::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const CAPABILITY: &str = "minecraft:movement_options";
mod acquisition;
pub use acquisition::{InventoryCompletion, RetainedInventory};
fn error(code: ErrorCode, message: impl Into<String>) -> ApiError {
    ApiError::new(code, message)
}
fn local(e: impl std::fmt::Display) -> ApiError {
    error(
        ErrorCode::Internal,
        format!("Local navigation journal: {e}"),
    )
}
fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f64>().sqrt()
}
fn key(p: [f64; 3]) -> String {
    format!(
        "{}:{}:{}",
        (p[0] * 4.).round() as i64,
        (p[1] * 16.).round() as i64,
        (p[2] * 4.).round() as i64
    )
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Goal {
    Position {
        dimension: String,
        target: [f64; 3],
        distance: f64,
    },
    /// Eye height/range are caller-supplied character parameters, not core policy.
    BlockReach {
        dimension: String,
        block: [i32; 3],
        distance: f64,
        eye_height: f64,
    },
}
impl Goal {
    fn dimension(&self) -> &str {
        match self {
            Self::Position { dimension, .. } | Self::BlockReach { dimension, .. } => dimension,
        }
    }
    fn gap(&self, mut p: [f64; 3]) -> f64 {
        match self {
            Self::Position {
                target,
                distance: r,
                ..
            } => (distance(p, *target) - r).max(0.),
            Self::BlockReach {
                block,
                distance: r,
                eye_height,
                ..
            } => {
                p[1] += eye_height;
                let nearest = std::array::from_fn(|i| {
                    p[i].clamp(f64::from(block[i]), f64::from(block[i]) + 1.)
                });
                (distance(p, nearest) - r).max(0.)
            }
        }
    }
    /// Validate a caller-supplied goal before activating a client program.
    pub fn validate(&self) -> Result<()> {
        let (r, valid) = match self {
            Self::Position {
                target, distance, ..
            } => (*distance, target.iter().all(|v| v.is_finite())),
            Self::BlockReach {
                distance,
                eye_height,
                ..
            } => (
                *distance,
                eye_height.is_finite() && *eye_height > 0. && *eye_height < 10.,
            ),
        };
        if !registry_id(self.dimension()) || !r.is_finite() || r < 0. || !valid {
            return Err(error(ErrorCode::InvalidRequest, "Invalid navigation goal"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Options {
    pub max_steps: usize,
    pub max_nodes: usize,
    pub timeout_ms: u64,
    pub blocked_blocks: Vec<String>,
    /// Experimental client policy: act on a fresh, strictly improving preview
    /// prefix rather than waiting for full local enumeration. Off by default.
    pub eager_progress: bool,
    /// Explicit caller-owned collection condition. Unrelated pickups never stop
    /// ordinary position navigation. Requires fresh-observation capability.
    pub until_inventory: Option<RetainedInventory>,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            max_steps: 128,
            max_nodes: 1024,
            timeout_ms: 120000,
            blocked_blocks: Vec::new(),
            eager_progress: false,
            until_inventory: None,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub elapsed_ms: u64,
    pub status: String,
    pub position: [f64; 3],
    pub remaining_distance: f64,
    pub actions: usize,
    /// Valid transient previews encountered by this durable navigation journal.
    #[serde(default)]
    pub preview_waits: u64,
    #[serde(default)]
    pub preview: PreviewStats,
    pub pending_action: Option<String>,
    pub journal: PathBuf,
    pub note: String,
    /// Caller condition evidence, distinct from arrival and action effects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inventory_completion: Option<InventoryCompletion>,
}
/// Cumulative diagnostic counters for this durable navigation journal.
/// Read predictions and local intentions never certify retained game progress.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PreviewStats {
    pub ready_pages: u64,
    pub pending_busy: u64,
    pub pending_other: u64,
    pub scene_restarts: u64,
    pub eager_intentions: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Origin {
    position: [f64; 3],
    dimension: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Route {
    target: [f64; 3],
    jump: bool,
    max_ticks: u16,
    duration_ticks: u32,
    touched_blocks: Vec<String>,
}
#[derive(Debug, Deserialize)]
struct Page {
    format: String,
    origin: Option<Origin>,
    scene: String,
    options: Vec<Route>,
    complete: bool,
    next_offset: Option<usize>,
    retry_after_ms: u64,
    #[serde(default)]
    busy: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Edge {
    route: Route,
    to: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Node {
    position: [f64; 3],
    expanded: bool,
    edges: Vec<Edge>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Attempt {
    request: ActionRequest,
    origin: [f64; 3],
    #[serde(default)]
    source_node: Option<String>,
    scene: String,
    target: [f64; 3],
    outcome: Option<Outcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    arrival_verification: Option<serde_json::Value>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Expansion {
    origin: Origin,
    scene: String,
    next_offset: usize,
    routes: Vec<Route>,
    basis: serde_json::Value,
    #[serde(default)]
    ready_after_unix_ms: u64,
}
#[derive(Serialize, Deserialize)]
struct Prefix {
    origin: Origin,
    scene: String,
    next_offset: usize,
    routes_read: usize,
}
fn unix_ms() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(local)?
        .as_millis()
        .min(u128::from(u64::MAX)) as u64)
}
#[derive(Serialize, Deserialize)]
struct State {
    version: u32,
    instance: String,
    actor: String,
    goal: Goal,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    until_inventory: Option<RetainedInventory>,
    prefix: String,
    sequence: u64,
    #[serde(default)]
    preview_waits: u64,
    #[serde(default)]
    preview: PreviewStats,
    // Keep the read provenance associated with an exact durable intention even
    // after moving invalidates the single resumable expansion at the old pose.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    prefixes: BTreeMap<String, Prefix>,
    nodes: BTreeMap<String, Node>,
    blocked: BTreeSet<String>,
    pending: Option<Attempt>,
    history: Vec<Attempt>,
    position: [f64; 3],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expansion: Option<Expansion>,
}
struct Journal {
    path: PathBuf,
    state: State,
    _lock: File,
}
static NEXT: AtomicU64 = AtomicU64::new(0);
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
impl Journal {
    #[cfg(test)]
    fn open(path: &Path, goal: Goal, instance: &str, actor: &str, p: [f64; 3]) -> Result<Self> {
        Self::open_with_condition(path, goal, instance, actor, p, None)
    }
    fn open_with_condition(
        path: &Path,
        goal: Goal,
        instance: &str,
        actor: &str,
        p: [f64; 3],
        until_inventory: Option<RetainedInventory>,
    ) -> Result<Self> {
        // Older clients accept only v1 and must reject an acquisition journal
        // rather than silently forgetting its condition during a downgrade.
        let version = if until_inventory.is_some() { 2 } else { 1 };
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent).map_err(local)?;
        let mut opts = OpenOptions::new();
        opts.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let lock = opts
            .open(path.with_extension("navigation-lock"))
            .map_err(local)?;
        lock.try_lock().map_err(|_| {
            error(
                ErrorCode::Conflict,
                "Another navigation invocation owns this journal",
            )
        })?;
        let mut state = if path.exists() {
            let m = fs::symlink_metadata(path).map_err(local)?;
            if !m.is_file() || m.file_type().is_symlink() || m.len() > 16 * 1024 * 1024 {
                return Err(local("Expected a bounded regular navigation state file"));
            }
            let s: State =
                serde_json::from_slice(&fs::read(path).map_err(local)?).map_err(local)?;
            if s.version != version
                || s.instance != instance
                || s.actor != actor
                || s.goal != goal
                || s.until_inventory != until_inventory
                || !valid_id(&s.prefix)
            {
                return Err(error(
                    ErrorCode::Conflict,
                    "Navigation journal belongs to another goal, inventory condition, character, instance or version; preserve it and use a new path",
                ));
            }
            s
        } else {
            State {
                version,
                instance: instance.into(),
                actor: actor.into(),
                goal,
                until_inventory,
                prefix: format!(
                    "nav-{:x}-{:x}-{:x}",
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(local)?
                        .as_nanos(),
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ),
                sequence: 0,
                preview_waits: 0,
                preview: PreviewStats::default(),
                prefixes: BTreeMap::new(),
                nodes: BTreeMap::new(),
                blocked: BTreeSet::new(),
                pending: None,
                history: Vec::new(),
                position: p,
                expansion: None,
            }
        };
        // Older clients classified cancellation as a geometric failure. Repair
        // only edges whose retained history contains cancellation alone; keep
        // every edge with another outcome and preserve all intention history.
        let mut cancelled_only: BTreeSet<_> = state
            .history
            .iter()
            .filter(|attempt| attempt.outcome == Some(Outcome::Cancelled))
            .map(attempt_edge_key)
            .collect();
        for attempt in &state.history {
            if attempt.outcome != Some(Outcome::Cancelled) {
                cancelled_only.remove(&attempt_edge_key(attempt));
            }
        }
        state.blocked.retain(|edge| !cancelled_only.contains(edge));
        let j = Self {
            path: path.to_owned(),
            state,
            _lock: lock,
        };
        j.save()?;
        Ok(j)
    }
    fn save(&self) -> Result<()> {
        let bytes = serde_json::to_vec(&self.state).map_err(local)?;
        if bytes.len() > 16 * 1024 * 1024 {
            return Err(local("Navigation state exceeds storage budget"));
        }
        let temp = self.path.with_extension(format!(
            "navigation-next-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
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
    }
    fn report(&self, status: &str, note: &str) -> Report {
        Report {
            elapsed_ms: 0,
            status: status.into(),
            position: self.state.position,
            remaining_distance: self.state.goal.gap(self.state.position),
            actions: self.state.history.len(),
            preview_waits: self.state.preview_waits,
            preview: self.state.preview.clone(),
            pending_action: self
                .state
                .pending
                .as_ref()
                .map(|p| p.request.action_id.clone()),
            journal: self.path.clone(),
            note: note.into(),
            inventory_completion: None,
        }
    }
}
fn observed_pose(o: &Observation) -> Result<Origin> {
    let p = &o.player["position"];
    let position = [p[0].as_f64(), p[1].as_f64(), p[2].as_f64()];
    let [Some(x), Some(y), Some(z)] = position else {
        return Err(error(ErrorCode::NotReady, "Player position unavailable"));
    };
    if ![x, y, z].iter().all(|v| v.is_finite()) {
        return Err(error(ErrorCode::NotReady, "Player position invalid"));
    }
    Ok(Origin {
        position: [x, y, z],
        dimension: o.player["dimension"]
            .as_str()
            .ok_or_else(|| error(ErrorCode::NotReady, "Dimension unavailable"))?
            .into(),
    })
}
fn blocked_key(from: &str, to: &str, scene: &str) -> String {
    format!("{from}>{to}@{scene}")
}
fn attempt_edge_key(attempt: &Attempt) -> String {
    blocked_key(
        &attempt
            .source_node
            .clone()
            .unwrap_or_else(|| key(attempt.origin)),
        &key(attempt.target),
        &attempt.scene,
    )
}
fn current_node(state: &State, observed: &Observation, position: [f64; 3]) -> String {
    // A verified arrival can coast across the graph's rounding boundary after
    // input release (notably on ice). Expand the reached waypoint rather than
    // leaving it perpetually unexplored beside a new rounded origin. This is
    // only graph identity: exact goal checks and native admission stay intact.
    if let Some(last) = state.history.last()
        && last.outcome == Some(Outcome::Observed)
        && let Some(arrival) = &last.arrival_verification
        && arrival["target_reached"] == true
        && arrival["entity_id"].as_i64().is_some()
        && arrival["entity_id"] == observed.player["entity_id"]
        && arrival["dimension"] == observed.player["dimension"]
        && arrival["epoch"].as_str().is_some_and(|s| !s.is_empty())
        && arrival["epoch"] == observed.world["sample_clock"]["epoch"]
        && (distance(position, last.target) <= 0.35
            || released_swim_arrival(last, observed, position))
    {
        return key(last.target);
    }
    key(position)
}
fn released_swim_arrival(last: &Attempt, observed: &Observation, position: [f64; 3]) -> bool {
    // A released vertical swim can settle below its verified waypoint before
    // the next preview. Re-expand from that waypoint using the fresh pose;
    // otherwise the same short rise is selected forever. This changes graph
    // identity only, never goal distance or native movement admission.
    observed.player["in_water"] == true
        && observed.player["control"]["action_id"] == last.request.action_id
        && observed.player["control"]["outcome"] == "observed"
        && observed.player["control"]["input_released"] == true
        && matches!(last.request.action, Action::StartMove { jump: false, .. })
        && last.target[1] > last.origin[1]
        && (last.target[0] - last.origin[0]).hypot(last.target[2] - last.origin[2]) <= 0.15
        && (position[0] - last.target[0]).hypot(position[2] - last.target[2]) <= 0.15
        && (position[1] - last.target[1]).abs() <= 0.6
}
fn arrived(state: &State, target: &str) -> bool {
    state.history.iter().any(|a| {
        a.outcome == Some(Outcome::Observed)
            && a.arrival_verification
                .as_ref()
                .is_some_and(|v| v["target_reached"] == true)
            && key(a.target) == target
    })
}
fn eager_route(
    state: &State,
    current: &str,
    origin: [f64; 3],
    scene: &str,
    routes: &[Route],
    options: &Options,
) -> Option<Route> {
    let gap = state.goal.gap(origin);
    routes
        .iter()
        .filter(|route| {
            let to = key(route.target);
            to != current
                && state.goal.gap(route.target) < gap - 1e-6
                && !route
                    .touched_blocks
                    .iter()
                    .any(|b| options.blocked_blocks.contains(b))
                && !arrived(state, &to)
                && !state.blocked.contains(&blocked_key(current, &to, scene))
                && state.nodes.get(&to).is_none_or(|n| !n.expanded)
        })
        .min_by(|a, b| {
            state
                .goal
                .gap(a.target)
                .total_cmp(&state.goal.gap(b.target))
                .then_with(|| a.duration_ticks.cmp(&b.duration_ticks))
        })
        .cloned()
}
fn choose(state: &State, start: &str) -> Option<Route> {
    let mut costs: BTreeMap<String, f64> = BTreeMap::from([(start.to_owned(), 0.)]);
    let mut first = BTreeMap::<String, Route>::new();
    let mut done = BTreeSet::new();
    let mut best: Option<(f64, f64, Route)> = None;
    loop {
        let next = costs
            .iter()
            .filter(|(k, _)| !done.contains(*k))
            .min_by(|a, b| a.1.total_cmp(b.1))
            .map(|(k, c)| (k.clone(), *c));
        let Some((id, cost)) = next else {
            break;
        };
        done.insert(id.clone());
        let node = state.nodes.get(&id)?;
        if id != start && !node.expanded {
            let h = state.goal.gap(node.position);
            let score = cost + h;
            if best.as_ref().is_none_or(|(s, old_h, _)| {
                score < *s - 1e-6 || ((score - *s).abs() < 1e-6 && h < *old_h)
            }) {
                best = Some((score, h, first.get(&id)?.clone()));
            }
        }
        for e in &node.edges {
            let n = cost + distance(node.position, e.route.target);
            if costs.get(&e.to).is_none_or(|old| n < *old) {
                costs.insert(e.to.clone(), n);
                first.insert(
                    e.to.clone(),
                    if id == start {
                        e.route.clone()
                    } else {
                        first.get(&id)?.clone()
                    },
                );
            }
        }
    }
    best.map(|(_, _, r)| r)
}

/// Resume a persisted navigation intention without replay. A reached block
/// distance is a candidate interaction pose, not proof of mining or collection.
pub async fn navigate(
    client: &Client,
    goal: Goal,
    path: &Path,
    options: Options,
) -> Result<Report> {
    let started = std::time::Instant::now();
    goal.validate()?;
    if let Some(condition) = &options.until_inventory {
        condition.validate()?;
    }
    if options.max_steps == 0
        || options.max_nodes == 0
        || options.timeout_ms == 0
        || options.blocked_blocks.iter().any(|s| !registry_id(s))
    {
        return Err(error(
            ErrorCode::InvalidRequest,
            "Invalid navigation budget or registry ID",
        ));
    }
    let expected = client.expected_instance.as_deref().ok_or_else(|| {
        error(
            ErrorCode::Unauthorized,
            "Navigation requires a scoped client profile",
        )
    })?;
    let info = client.verify_instance(expected).await?;
    if !info.capabilities.iter().any(|c| c == CAPABILITY) {
        return Err(error(
            ErrorCode::UnsupportedCapability,
            "Server does not advertise movement options",
        ));
    }
    if options.until_inventory.is_some()
        && !info
            .capabilities
            .iter()
            .any(|c| c == menu::FRESH_OBSERVATION_CAPABILITY)
    {
        return Err(error(
            ErrorCode::UnsupportedCapability,
            "Inventory-conditioned navigation requires fresh observations",
        ));
    }
    let observation = client.observe().await?;
    let pose = observed_pose(&observation)?;
    let actor = observation.player["uuid"]
        .as_str()
        .ok_or_else(|| error(ErrorCode::NotReady, "Player identity unavailable"))?;
    let mut journal = Journal::open_with_condition(
        path,
        goal,
        expected,
        actor,
        pose.position,
        options.until_inventory.clone(),
    )?;
    let run = tokio::time::timeout(
        Duration::from_millis(options.timeout_ms),
        run(client, &mut journal, &options),
    )
    .await;
    let mut report = match run {
        Ok(result) => result,
        Err(_) => {
            journal.save()?;
            Ok(journal.report("budget_exhausted","Operation deadline reached; completed read pages can resume after fresh scene validation. Inspect the preserved pending ID before any new intention. Finite controls retain their server expiry."))
        }
    }?;
    report.elapsed_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    Ok(report)
}
async fn run(client: &Client, j: &mut Journal, options: &Options) -> Result<Report> {
    loop {
        let mut cancelled = false;
        if let Some(pending) = &j.state.pending {
            let id = pending.request.action_id.clone();
            let receipt = match client.action_status(&id).await {
                Ok(r) => r,
                Err(e) if e.code == ErrorCode::NotFound => {
                    return Ok(j.report(
                        "needs_reconciliation",
                        "Pending ID is absent from the current journal; do not replay it",
                    ));
                }
                Err(e) => return Err(e),
            };
            let receipt = if matches!(
                receipt.outcome,
                Outcome::Prepared | Outcome::Dispatched | Outcome::Uncertain
            ) {
                client.reconcile(&id).await?
            } else {
                receipt
            };
            if receipt.action_id != id {
                return Err(error(ErrorCode::Conflict, "Navigation receipt ID mismatch"));
            }
            if !receipt.outcome.terminal() {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            let mut arrival = None;
            if receipt.outcome == Outcome::Observed {
                let after = &receipt.evidence["after"]["player"]["position"];
                let actual = [after[0].as_f64(), after[1].as_f64(), after[2].as_f64()];
                if receipt.evidence["native_effect_verified"] != true
                    || !matches!(actual,[Some(x),Some(y),Some(z)] if [x,y,z].into_iter().all(f64::is_finite))
                {
                    return Ok(j.report("needs_reconciliation","Observed receipt lacks usable native pose evidence; preserve the intention and investigate without replay"));
                }
                if receipt.evidence["after"]["player"]["uuid"].as_str() != Some(&j.state.actor)
                    || receipt.evidence["after"]["player"]["dimension"].as_str()
                        != Some(j.state.goal.dimension())
                {
                    return Err(error(
                        ErrorCode::Conflict,
                        "Movement receipt actor or dimension differs from the saved intention",
                    ));
                }
                if let [Some(x), Some(y), Some(z)] = actual {
                    arrival = Some((
                        [x, y, z],
                        movement_target_reached([x, y, z], pending.target),
                    ));
                }
            }
            let mut attempt = j
                .state
                .pending
                .take()
                .ok_or_else(|| local("Pending intention vanished"))?;
            attempt.outcome = Some(receipt.outcome.clone());
            attempt.arrival_verification=arrival.map(|(actual,reached)|serde_json::json!({"target_reached":reached,"native_after_position":actual,"tolerance_blocks":MOVEMENT_TARGET_TOLERANCE,"native_receipt_outcome":receipt.outcome,
                "entity_id":receipt.evidence["after"]["player"]["entity_id"],
                "dimension":receipt.evidence["after"]["player"]["dimension"],
                "epoch":receipt.evidence["after"]["world"]["sample_clock"]["epoch"]}));
            // Cancellation is a control handoff, not a physical route verdict.
            // Retain its exact intention without poisoning an otherwise feasible
            // edge or immediately fighting the controller that cancelled it.
            cancelled = receipt.outcome == Outcome::Cancelled;
            if (!matches!(receipt.outcome, Outcome::Observed | Outcome::Cancelled))
                || arrival.is_some_and(|(_, reached)| !reached)
            {
                j.state.blocked.insert(attempt_edge_key(&attempt));
            }
            j.state.history.push(attempt);
            // A prefix may still have unread predictions at its old origin.
            // Keep that cursor until fresh pose/physical/scene validation can
            // retire it, and retain action-bound provenance independently.
            if !j.state.prefixes.contains_key(&id) {
                j.state.expansion = None;
            }
            if let Some((actual, false)) = arrival {
                j.state.position = actual;
                j.save()?;
                return Ok(j.report("target_not_reached","The server ended this intention at a different verified pose; native outcome and target mismatch are retained. Reobserve and plan a new intention; this ID was not replayed."));
            }
            j.save()?;
        }
        let mut observation = client.observe().await?;
        let pose = observed_pose(&observation)?;
        j.state.position = pose.position;
        if observation.player["uuid"].as_str() != Some(&j.state.actor) {
            return Err(error(
                ErrorCode::Conflict,
                "Observed character differs from navigation journal",
            ));
        }
        if pose.dimension != j.state.goal.dimension() {
            j.save()?;
            return Ok(j.report(
                "dimension_changed",
                "Replan the goal explicitly in the observed dimension",
            ));
        }
        if let Some(report) = acquisition::check(client, j, &mut observation).await? {
            return Ok(report);
        }
        // The optional condition may have fetched a newer native frame. All
        // subsequent route validation must use that frame, not its old trigger.
        let pose = observed_pose(&observation)?;
        if j.state.until_inventory.is_none() && j.state.goal.gap(pose.position) <= 0.001 {
            j.save()?;
            return Ok(j.report("reached","Observed player pose satisfies the requested distance; gameplay effects need their own verification"));
        }
        if cancelled {
            j.save()?;
            return Ok(j.report("preempted","The previous intention was cancelled. Its exact ID and fresh pose are retained, without marking the route impossible or dispatching another mutation. Resume explicitly after control conditions change; the cancelled ID is not replayed."));
        }
        if j.state.history.len() >= options.max_steps || j.state.nodes.len() > options.max_nodes {
            j.save()?;
            return Ok(j.report("budget_exhausted","Navigation exploration budget reached; inspect saved routes and outcomes before extending it"));
        }
        let mut offset = 0;
        let mut origin: Option<Origin> = None;
        let mut scene = String::new();
        let mut routes = Vec::new();
        let mut eager = None;
        let mut expansion_complete = false;
        if let Some(saved) = &j.state.expansion {
            let wait = saved.ready_after_unix_ms.saturating_sub(unix_ms()?);
            // Honor the last page's interval across process boundaries. A
            // large wall-clock reversal must not impose an unbounded wait.
            if wait <= 5000 {
                tokio::time::sleep(Duration::from_millis(wait)).await;
            }
        }
        let basis = serde_json::json!({"entity":observation.player["entity_id"],
            "clock":observation.world["sample_clock"]["epoch"],
            "physics":observation.player["physics"],"velocity":observation.player["velocity"],
            "on_ground":observation.player["on_ground"],"speed":observation.player["movement_speed"],
            "sneaking":observation.player["sneaking"],"sprinting":observation.player["sprinting"],
            "adapter":client.session.as_ref().map(|s|s.adapter_version.as_str())});
        let mut resume = j.state.expansion.clone().filter(|saved| {
            saved.basis == basis
                && saved.origin.dimension == pose.dimension
                && saved.origin.position == pose.position
                && saved.routes.len() <= 1024
                && (1..=1024).contains(&saved.next_offset)
        });
        if resume.is_none() && j.state.expansion.is_some() {
            j.state.preview.scene_restarts = j.state.preview.scene_restarts.saturating_add(1);
            j.state.expansion = None;
            j.save()?;
        }
        loop {
            let mut input = serde_json::json!({"offset":offset});
            if resume.is_some() {
                // Never trust a persisted offset without checking the current
                // scene. A small first page is a read-only validation probe.
                input["limit"] = serde_json::json!(1);
            }
            if let Goal::Position { target, .. } = j.state.goal {
                input["target"] = serde_json::json!(target);
            }
            let value = client
                .read_observation(&ObservationQuery {
                    capability: CAPABILITY.into(),
                    input,
                })
                .await?;
            let page: Page = serde_json::from_value(value).map_err(|_| {
                error(
                    ErrorCode::UnsupportedVersion,
                    "Movement option schema is incompatible",
                )
            })?;
            if page.format != "minecraft-movement-options-v1"
                || page.scene.len() > 128
                || page.options.len() > 64
            {
                return Err(error(
                    ErrorCode::UnsupportedVersion,
                    "Movement option version is incompatible",
                ));
            }
            if page.origin.is_none() {
                // The adapter returns this envelope while a finite control or
                // its preview interval is still active. It is read readiness,
                // not an incompatible adapter or an uncertain mutation.
                if !page.scene.is_empty()
                    || !page.options.is_empty()
                    || page.complete
                    || page.next_offset.is_some()
                {
                    return Err(error(
                        ErrorCode::UnsupportedVersion,
                        "Invalid pending movement preview",
                    ));
                }
                j.state.preview_waits = j.state.preview_waits.saturating_add(1);
                if page.busy {
                    j.state.preview.pending_busy = j.state.preview.pending_busy.saturating_add(1);
                } else {
                    j.state.preview.pending_other = j.state.preview.pending_other.saturating_add(1);
                }
                j.save()?;
                tokio::time::sleep(Duration::from_millis(page.retry_after_ms.clamp(25, 5000)))
                    .await;
                continue;
            }
            if page.scene.is_empty() {
                return Err(error(
                    ErrorCode::UnsupportedVersion,
                    "Ready movement preview has no scene",
                ));
            }
            if page.options.iter().any(|r| {
                !r.target.iter().all(|n| n.is_finite())
                    || r.max_ticks == 0
                    || r.duration_ticks == 0
                    || r.duration_ticks > u32::from(r.max_ticks)
            }) {
                return Err(error(
                    ErrorCode::UnsupportedVersion,
                    "Invalid route prediction",
                ));
            }
            // A validation probe is still a page. Reject contradictory or
            // nonadvancing pagination before reusing any cached useful edge.
            let next = if page.complete {
                if page.next_offset.is_some() {
                    return Err(error(
                        ErrorCode::UnsupportedVersion,
                        "Complete movement preview has a continuation cursor",
                    ));
                }
                None
            } else {
                Some(
                    page.next_offset
                        .filter(|n| *n > offset && *n <= 1024)
                        .ok_or_else(|| {
                            error(
                                ErrorCode::NotReady,
                                "Movement option pagination did not advance",
                            )
                        })?,
                )
            };
            let now = page.origin.expect("Validated ready movement preview");
            j.state.preview.ready_pages = j.state.preview.ready_pages.saturating_add(1);
            if now.dimension != pose.dimension || distance(now.position, pose.position) > 0.15 {
                j.state.preview.scene_restarts = j.state.preview.scene_restarts.saturating_add(1);
                origin = None;
                j.state.expansion = None;
                j.save()?;
                break;
            }
            if let Some(mut saved) = resume.take() {
                if !page.complete
                    && saved.scene == page.scene
                    && saved.origin.position == now.position
                {
                    offset = saved.next_offset;
                    scene = saved.scene.clone();
                    origin = Some(saved.origin.clone());
                    routes = saved.routes.clone();
                    if options.eager_progress {
                        let current = current_node(&j.state, &observation, saved.origin.position);
                        eager = eager_route(
                            &j.state,
                            &current,
                            saved.origin.position,
                            &saved.scene,
                            &saved.routes,
                            options,
                        );
                    }
                    saved.ready_after_unix_ms =
                        unix_ms()?.saturating_add(page.retry_after_ms.clamp(25, 5000));
                    j.state.expansion = Some(saved);
                    j.save()?;
                    if eager.is_some() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(page.retry_after_ms.clamp(25, 5000)))
                        .await;
                    continue;
                }
                j.state.preview.scene_restarts = j.state.preview.scene_restarts.saturating_add(1);
            }
            if let Some(old) = &origin {
                if distance(old.position, now.position) > 0.01 || scene != page.scene {
                    j.state.preview.scene_restarts =
                        j.state.preview.scene_restarts.saturating_add(1);
                    origin = None;
                    j.state.expansion = None;
                    j.save()?;
                    break;
                }
            } else {
                scene = page.scene.clone();
                origin = Some(now);
            }
            if options.eager_progress && !page.complete {
                let p = origin.as_ref().expect("Validated preview origin").position;
                let current = current_node(&j.state, &observation, p);
                eager = eager_route(&j.state, &current, p, &scene, &page.options, options);
            }
            for r in page.options {
                routes.push(r);
            }
            if routes.len() > 1024 {
                return Err(error(
                    ErrorCode::UnsupportedVersion,
                    "Movement preview exceeds the bounded route budget",
                ));
            }
            if page.complete {
                expansion_complete = true;
                j.state.expansion = None;
                j.save()?;
                break;
            }
            offset = next.expect("Validated incomplete page cursor");
            j.state.expansion = Some(Expansion {
                origin: origin
                    .as_ref()
                    .ok_or_else(|| local("Preview origin vanished"))?
                    .clone(),
                scene: scene.clone(),
                next_offset: offset,
                routes: routes.clone(),
                basis: basis.clone(),
                ready_after_unix_ms: unix_ms()?.saturating_add(page.retry_after_ms.clamp(25, 5000)),
            });
            j.save()?;
            if eager.is_some() {
                // Query cooldown still governs further reads. Dispatch has its
                // own ordinary fresh native admission and durable pending ID.
                break;
            }
            tokio::time::sleep(Duration::from_millis(page.retry_after_ms.clamp(25, 5000))).await;
        }
        let Some(origin) = origin else {
            continue;
        };
        let current = current_node(&j.state, &observation, origin.position);
        let mut edges = Vec::new();
        if !j.state.nodes.contains_key(&current) && j.state.nodes.len() >= options.max_nodes {
            return Ok(j.report("budget_exhausted", "Navigation node budget reached"));
        }
        j.state.nodes.entry(current.clone()).or_insert(Node {
            position: origin.position,
            expanded: false,
            edges: Vec::new(),
        });
        for r in routes {
            // Deployment policy may change between invocations. Retain raw
            // predictions and apply the current block policy when using them.
            if r.touched_blocks
                .iter()
                .any(|id| options.blocked_blocks.contains(id))
            {
                continue;
            }
            let to = key(r.target);
            if to == current
                || j.state
                    .blocked
                    .contains(&blocked_key(&current, &to, &scene))
            {
                continue;
            }
            if !j.state.nodes.contains_key(&to) && j.state.nodes.len() >= options.max_nodes {
                j.save()?;
                return Ok(j.report("budget_exhausted", "Navigation node budget reached"));
            }
            j.state.nodes.entry(to.clone()).or_insert(Node {
                position: r.target,
                expanded: false,
                edges: Vec::new(),
            });
            edges.push(Edge { route: r, to });
        }
        j.state.nodes.insert(
            current.clone(),
            Node {
                position: origin.position,
                expanded: expansion_complete,
                edges,
            },
        );
        let was_eager = eager.is_some();
        let Some(route) = eager.or_else(|| choose(&j.state, &current)) else {
            j.save()?;
            return Ok(j.report("no_known_route","No reachable unexplored frontier remains. Inspect local support, foot space, headroom and goal constraints before selecting another distant target. This is not proof that the world has no route, or permission to change terrain."));
        };
        j.state.sequence = j
            .state
            .sequence
            .checked_add(1)
            .ok_or_else(|| local("Action sequence exhausted"))?;
        let request = ActionRequest {
            action_id: format!("{}-{}", j.state.prefix, j.state.sequence),
            action: Action::StartMove {
                target: Position {
                    x: route.target[0],
                    y: route.target[1],
                    z: route.target[2],
                },
                jump: route.jump,
                max_ticks: route.max_ticks,
            },
        };
        if was_eager {
            let read = j
                .state
                .expansion
                .as_ref()
                .ok_or_else(|| local("Eager read provenance missing"))?;
            j.state.prefixes.insert(
                request.action_id.clone(),
                Prefix {
                    origin: read.origin.clone(),
                    scene: read.scene.clone(),
                    next_offset: read.next_offset,
                    routes_read: read.routes.len(),
                },
            );
            j.state.preview.eager_intentions = j.state.preview.eager_intentions.saturating_add(1);
        }
        j.state.pending = Some(Attempt {
            request: request.clone(),
            origin: origin.position,
            source_node: Some(current),
            scene,
            target: route.target,
            outcome: None,
            arrival_verification: None,
        });
        j.save()?;
        // Persist before dispatch. Even an interrupted HTTP request is inspected
        // on the next invocation; it is never automatically submitted again.
        if let Err(error) = client.act(&request).await {
            if let Some(report) = retire_local_preemption(client, j, &error)? {
                return Ok(report);
            }
            // Network errors and remote failures cannot prove non-dispatch.
            // Retain the exact pending ID for inspection/reconciliation.
            return Err(error);
        }
    }
}

fn retire_local_preemption(
    client: &Client,
    j: &mut Journal,
    error: &ApiError,
) -> Result<Option<Report>> {
    if client.local_arbiter.is_none() || error.code != ErrorCode::LocalPreempted {
        return Ok(None);
    }
    let mut attempt = j
        .state
        .pending
        .take()
        .ok_or_else(|| local("Local rejected intention vanished"))?;
    attempt.outcome = Some(Outcome::Rejected);
    attempt.arrival_verification = Some(
        serde_json::json!({"kind":"local_pre_dispatch_rejection","native_effect_verified":false,"gateway_dispatch_attempted":false,"error":error}),
    );
    j.state.history.push(attempt);
    j.save()?;
    Ok(Some(j.report("preempted","Body rejected this intention before gateway dispatch. It was retained as a local rejection, not a native receipt. Resume after changed conditions with a new intention; this ID was not replayed.")))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn acquisition_condition() -> RetainedInventory {
        RetainedInventory {
            item: "fixture:resource".into(),
            minimum: 1,
        }
    }
    fn acquisition_sample(p: [f64; 3], count: u64) -> serde_json::Value {
        let mut o = observation(p);
        o["player"]["life_id"] = serde_json::json!("life1");
        o["player"]["inventory"] =
            serde_json::json!([{"slot":0,"item":"fixture:resource","count":count}]);
        o["player"]["control"] = serde_json::Value::Null;
        o["world"]["sample_clock"] = serde_json::json!({"epoch":"clock","nanos":"3"});
        o
    }
    fn acquisition_info() -> serde_json::Value {
        let mut i = info("instance");
        i["capabilities"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!(menu::FRESH_OBSERVATION_CAPABILITY));
        i
    }
    fn acquisition_pending(j: &mut Journal) {
        j.state.pending = Some(Attempt {
            request: ActionRequest {
                action_id: "original-pickup-leg".into(),
                action: Action::StartMove {
                    target: Position {
                        x: 0.5,
                        y: 1.,
                        z: 0.5,
                    },
                    jump: false,
                    max_ticks: 60,
                },
            },
            origin: [0.5, 1., 0.5],
            source_node: None,
            scene: "fixture-scene".into(),
            target: [0.5, 1., 0.5],
            outcome: None,
            arrival_verification: None,
        });
        j.save().unwrap();
    }
    #[test]
    fn acquisition_resume_changed_criterion_cannot_mix_existing_state() {
        let root = Temp::new();
        let path = root.0.join("condition.json");
        let condition = acquisition_condition();
        let mut j = Journal::open_with_condition(
            &path,
            goal(),
            "instance",
            "actor",
            [0.5, 1., 0.5],
            Some(condition.clone()),
        )
        .unwrap();
        acquisition_pending(&mut j);
        assert_eq!(
            j.state.version, 2,
            "old clients must reject an opt-in journal rather than erase its condition"
        );
        drop(j);
        let original = fs::read(&path).unwrap();
        for changed in [
            None,
            Some(RetainedInventory {
                minimum: 2,
                ..condition.clone()
            }),
            Some(RetainedInventory {
                item: "fixture:other".into(),
                ..condition.clone()
            }),
        ] {
            assert!(
                matches!(Journal::open_with_condition(&path,goal(),"instance","actor",[0.5,1.,0.5],changed),Err(e) if e.code==ErrorCode::Conflict)
            );
            assert_eq!(
                fs::read(&path).unwrap(),
                original,
                "criterion conflict cannot retire original pending ID"
            );
        }
        let j = Journal::open_with_condition(
            &path,
            goal(),
            "instance",
            "actor",
            [0.5, 1., 0.5],
            Some(condition),
        )
        .unwrap();
        assert_eq!(
            j.state.pending.unwrap().request.action_id,
            "original-pickup-leg"
        );
    }
    #[tokio::test]
    async fn acquisition_resume_unrequested_pickup_does_not_finish_bare_position() {
        let root = Temp::new();
        let mut journal = Journal::open(
            &root.0.join("bare.json"),
            goal(),
            "instance",
            "actor",
            [0.5, 1., 0.5],
        )
        .unwrap();
        let mut observation: Observation =
            serde_json::from_value(acquisition_sample([0.5, 1., 0.5], 99)).unwrap();
        let client = client("http://127.0.0.1:1/".into());
        assert!(
            acquisition::check(&client, &mut journal, &mut observation)
                .await
                .unwrap()
                .is_none(),
            "no condition means no inventory check, network request or early finish"
        );
        assert_eq!(journal.state.version, 1);
        assert!(journal.state.goal.gap(journal.state.position) > 1.);
    }
    #[tokio::test]
    async fn acquisition_resume_f09_gain_stops_only_opt_in_without_another_movement() {
        for pending in [false, true] {
            let root = Temp::new();
            let path = root.0.join("condition.json");
            if pending {
                let mut j = Journal::open_with_condition(
                    &path,
                    goal(),
                    "instance",
                    "actor",
                    [0.5, 1., 0.5],
                    Some(acquisition_condition()),
                )
                .unwrap();
                acquisition_pending(&mut j);
            }
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let c = client(format!("http://{}/", listener.local_addr().unwrap()));
            let server = tokio::spawn(async move {
                let sample = acquisition_sample([0.5, 1., 0.5], 1);
                let receipt = serde_json::json!({"action_id":"original-pickup-leg","outcome":"observed","adapter_version":"fixture","error":null,
                    "evidence":{"native_effect_verified":true,"input_released":true,"after":sample}});
                respond(&listener, "GET /v1/server ", acquisition_info(), 200).await;
                respond(
                    &listener,
                    "GET /v1/bots/fixture/observations ",
                    sample.clone(),
                    200,
                )
                .await;
                if pending {
                    respond(
                        &listener,
                        "GET /v1/bots/fixture/actions/original-pickup-leg ",
                        receipt.clone(),
                        200,
                    )
                    .await;
                }
                respond(
                    &listener,
                    "GET /v1/bots/fixture/observations ",
                    sample.clone(),
                    200,
                )
                .await;
                let (stream, query) =
                    request(&listener, "POST /v1/bots/fixture/observations/query ").await;
                assert_eq!(query["capability"], menu::FRESH_OBSERVATION_CAPABILITY);
                reply(stream,serde_json::json!({"format":"minecraft-fresh-observation-v1","observation":sample}),200).await;
                if pending {
                    respond(
                        &listener,
                        "GET /v1/bots/fixture/actions/original-pickup-leg ",
                        receipt,
                        200,
                    )
                    .await;
                }
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), listener.accept())
                        .await
                        .is_err(),
                    "satisfied collection cannot start another mutation or route preview"
                );
            });
            let report = navigate(
                &c,
                goal(),
                &path,
                Options {
                    until_inventory: Some(acquisition_condition()),
                    ..Options::default()
                },
            )
            .await
            .unwrap();
            server.await.unwrap();
            assert_eq!(report.status, "inventory_satisfied");
            assert!(
                report.remaining_distance > 1.,
                "retain actual unmet positional gap"
            );
            assert!(report.pending_action.is_none());
            let proof = report.inventory_completion.unwrap();
            assert_eq!(proof.observed_count, 1);
            assert!(proof.owned_input_settled);
            assert_eq!(
                proof.settled_action_id.as_deref(),
                pending.then_some("original-pickup-leg")
            );
        }
    }
    #[tokio::test]
    async fn acquisition_resume_never_bypasses_missing_or_held_original_input() {
        for missing in [false, true] {
            let root = Temp::new();
            let path = root.0.join("condition.json");
            if missing {
                let mut j = Journal::open_with_condition(
                    &path,
                    goal(),
                    "instance",
                    "actor",
                    [0.5, 1., 0.5],
                    Some(acquisition_condition()),
                )
                .unwrap();
                acquisition_pending(&mut j);
            }
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let c = client(format!("http://{}/", listener.local_addr().unwrap()));
            let server = tokio::spawn(async move {
                let mut sample = acquisition_sample([0.5, 1., 0.5], 1);
                if !missing {
                    sample["player"]["control"] = serde_json::json!({"action_id":"original-pickup-leg","outcome":"running","input_released":false});
                }
                respond(&listener, "GET /v1/server ", acquisition_info(), 200).await;
                respond(
                    &listener,
                    "GET /v1/bots/fixture/observations ",
                    sample.clone(),
                    200,
                )
                .await;
                if missing {
                    respond(
                        &listener,
                        "GET /v1/bots/fixture/actions/original-pickup-leg ",
                        serde_json::to_value(ApiError::new(
                            ErrorCode::NotFound,
                            "Preserved original ID missing",
                        ))
                        .unwrap(),
                        404,
                    )
                    .await;
                } else {
                    respond(
                        &listener,
                        "GET /v1/bots/fixture/observations ",
                        sample.clone(),
                        200,
                    )
                    .await;
                    respond(&listener,"POST /v1/bots/fixture/observations/query ",serde_json::json!({"format":"minecraft-fresh-observation-v1","observation":sample}),200).await;
                }
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), listener.accept())
                        .await
                        .is_err(),
                    "no new movement, cancel or replay may erase the unresolved lane"
                );
            });
            let report = navigate(
                &c,
                goal(),
                &path,
                Options {
                    until_inventory: Some(acquisition_condition()),
                    ..Options::default()
                },
            )
            .await
            .unwrap();
            server.await.unwrap();
            assert_eq!(report.status, "needs_reconciliation");
            assert_eq!(
                report.pending_action.as_deref(),
                Some("original-pickup-leg")
            );
            assert!(
                report
                    .inventory_completion
                    .is_none_or(|p| !p.owned_input_settled)
            );
        }
    }
    #[tokio::test]
    async fn acquisition_resume_cursor_and_output_do_not_satisfy_retained_condition() {
        let root = Temp::new();
        let path = root.0.join("condition.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let c = client(format!("http://{}/", listener.local_addr().unwrap()));
        let server = tokio::spawn(async move {
            let mut sample = acquisition_sample([5.5, 1., 0.5], 0);
            sample["menu"] = serde_json::json!({"carried":{"item":"fixture:resource","count":9},"slots":[{"item":"fixture:resource","count":9}]});
            respond(&listener, "GET /v1/server ", acquisition_info(), 200).await;
            for _ in 0..2 {
                respond(
                    &listener,
                    "GET /v1/bots/fixture/observations ",
                    sample.clone(),
                    200,
                )
                .await;
            }
            respond(
                &listener,
                "POST /v1/bots/fixture/observations/query ",
                serde_json::json!({"format":"minecraft-fresh-observation-v1","observation":sample}),
                200,
            )
            .await;
            assert!(
                tokio::time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err()
            );
        });
        let report = navigate(
            &c,
            goal(),
            &path,
            Options {
                until_inventory: Some(acquisition_condition()),
                ..Options::default()
            },
        )
        .await
        .unwrap();
        server.await.unwrap();
        assert_eq!(report.status, "inventory_missing");
        let proof = report.inventory_completion.unwrap();
        assert_eq!(proof.observed_count, 0);
        assert!(!proof.condition_satisfied);
    }
    #[tokio::test]
    async fn acquisition_resume_fresh_displacement_and_lost_count_replace_stale_route_basis() {
        for cached_at_goal in [false, true] {
            let root = Temp::new();
            let path = root.0.join("fresh.json");
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let c = client(format!("http://{}/", listener.local_addr().unwrap()));
            let fresh_position = if cached_at_goal {
                [0.5, 1., 0.5]
            } else {
                [2.5, 1., 0.5]
            };
            let server = tokio::spawn(async move {
                let cached = acquisition_sample(
                    if cached_at_goal {
                        [5.5, 1., 0.5]
                    } else {
                        [0.5, 1., 0.5]
                    },
                    u64::from(!cached_at_goal),
                );
                let mut fresh = acquisition_sample(fresh_position, 0);
                fresh["revision"] = serde_json::json!("newer-native");
                fresh["world"]["sample_clock"]["nanos"] = serde_json::json!("4");
                respond(&listener, "GET /v1/server ", acquisition_info(), 200).await;
                for _ in 0..2 {
                    respond(
                        &listener,
                        "GET /v1/bots/fixture/observations ",
                        cached.clone(),
                        200,
                    )
                    .await;
                }
                respond(&listener,"POST /v1/bots/fixture/observations/query ",serde_json::json!({"format":"minecraft-fresh-observation-v1","observation":fresh}),200).await;
                let (stream, query) = tokio::time::timeout(
                    Duration::from_secs(2),
                    request(&listener, "POST /v1/bots/fixture/observations/query "),
                )
                .await
                .expect("fresh outside-goal facts must continue route validation");
                assert_eq!(query["capability"], CAPABILITY);
                reply(stream,serde_json::json!({"format":"minecraft-movement-options-v1","origin":{"position":fresh_position,"dimension":"fixture:world"},"scene":"new-native-basis","options":[],"complete":true,"next_offset":null,"retry_after_ms":0}),200).await;
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), listener.accept())
                        .await
                        .is_err(),
                    "new native pose must be used rather than treating it as an origin mismatch"
                );
            });
            let report = tokio::time::timeout(
                Duration::from_secs(3),
                navigate(
                    &c,
                    goal(),
                    &path,
                    Options {
                        until_inventory: Some(acquisition_condition()),
                        ..Options::default()
                    },
                ),
            )
            .await
            .unwrap()
            .unwrap();
            server.await.unwrap();
            assert_eq!(report.status, "no_known_route");
            assert_eq!(report.position, fresh_position);
            assert!(report.inventory_completion.is_none());
        }
    }
    #[tokio::test]
    async fn acquisition_resume_missing_resource_cannot_hide_held_input() {
        let root = Temp::new();
        let path = root.0.join("held.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let c = client(format!("http://{}/", listener.local_addr().unwrap()));
        let server = tokio::spawn(async move {
            let mut sample = acquisition_sample([5.5, 1., 0.5], 0);
            sample["player"]["control"] = serde_json::json!({"action_id":"earlier-held-id","outcome":"running","input_released":false});
            respond(&listener, "GET /v1/server ", acquisition_info(), 200).await;
            for _ in 0..2 {
                respond(
                    &listener,
                    "GET /v1/bots/fixture/observations ",
                    sample.clone(),
                    200,
                )
                .await;
            }
            respond(
                &listener,
                "POST /v1/bots/fixture/observations/query ",
                serde_json::json!({"format":"minecraft-fresh-observation-v1","observation":sample}),
                200,
            )
            .await;
            assert!(
                tokio::time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err()
            );
        });
        let report = navigate(
            &c,
            goal(),
            &path,
            Options {
                until_inventory: Some(acquisition_condition()),
                ..Options::default()
            },
        )
        .await
        .unwrap();
        server.await.unwrap();
        assert_eq!(report.status, "needs_reconciliation");
        assert_eq!(report.pending_action.as_deref(), Some("earlier-held-id"));
        assert!(!report.inventory_completion.unwrap().owned_input_settled);
    }
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "mc-sdk-navigation-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn goal() -> Goal {
        Goal::Position {
            dimension: "fixture:world".into(),
            target: [5.5, 1., 0.5],
            distance: 0.1,
        }
    }
    fn route(p: [f64; 3]) -> Route {
        Route {
            target: p,
            jump: false,
            max_ticks: 60,
            duration_ticks: 10,
            touched_blocks: Vec::new(),
        }
    }
    fn eager_options() -> Options {
        Options {
            eager_progress: true,
            max_steps: 1,
            timeout_ms: 3000,
            ..Options::default()
        }
    }
    fn eager_page(
        routes: Vec<Route>,
        complete: bool,
        next: Option<usize>,
        retry: u64,
    ) -> serde_json::Value {
        serde_json::json!({"format":"minecraft-movement-options-v1",
            "origin":{"position":[0.5,1.,0.5],"dimension":"fixture:world"},
            "scene":"prefix-scene","options":routes,"complete":complete,
            "next_offset":next,"retry_after_ms":retry})
    }
    async fn eager_open(listener: &tokio::net::TcpListener) {
        respond(listener, "GET /v1/server ", info("instance"), 200).await;
        for _ in 0..2 {
            respond(
                listener,
                "GET /v1/bots/fixture/observations ",
                observation([0.5, 1., 0.5]),
                200,
            )
            .await;
        }
    }
    async fn eager_finish(listener: &tokio::net::TcpListener, id: &str, position: [f64; 3]) {
        respond(
            listener,
            &format!("GET /v1/bots/fixture/actions/{id} "),
            serde_json::json!({"action_id":id,"outcome":"observed","adapter_version":"fixture",
                "error":null,"evidence":{"native_effect_verified":true,"after":{"player":{
                    "uuid":"actor","dimension":"fixture:world","position":position}}}}),
            200,
        )
        .await;
        respond(
            listener,
            "GET /v1/bots/fixture/observations ",
            observation(position),
            200,
        )
        .await;
    }
    #[test]
    fn eager_prefix_requires_new_strict_progress_and_obeys_current_policy() {
        assert!(!Options::default().eager_progress);
        assert!(
            !serde_json::from_str::<Options>("{}")
                .unwrap()
                .eager_progress
        );
        let root = Temp::new();
        let p = [0.5, 1., 0.5];
        let current = key(p);
        let mut j =
            Journal::open(&root.0.join("nav.json"), goal(), "instance", "actor", p).unwrap();
        let mut options = eager_options();
        options.blocked_blocks = vec!["fixture:denied".into()];
        let mut denied = route([4.5, 1., 0.5]);
        denied.touched_blocks = options.blocked_blocks.clone();
        j.state
            .blocked
            .insert(blocked_key(&current, &key([3.5, 1., 0.5]), "prefix-scene"));
        let routes = vec![
            route(p),
            route([-0.5, 1., 0.5]),
            route([0.5, 1., 1.5]),
            denied,
            route([3.5, 1., 0.5]),
            route([2.5, 1., 0.5]),
        ];
        assert_eq!(
            eager_route(&j.state, &current, p, "prefix-scene", &routes, &options)
                .unwrap()
                .target,
            [2.5, 1., 0.5]
        );
        j.state.history.push(serde_json::from_value(serde_json::json!({
            "request":{"action_id":"already-arrived","action":{"kind":"start_move","target":{"x":2.5,"y":1.,"z":0.5},"jump":false,"max_ticks":60}},
            "origin":p,"scene":"previous-scene","target":[2.5,1.,0.5],"outcome":"observed",
            "arrival_verification":{"target_reached":true}})).unwrap());
        assert!(
            eager_route(&j.state, &current, p, "prefix-scene", &routes, &options).is_none(),
            "Returned-to-origin control must not repeatedly select an already arrived frontier"
        );
        j.state.nodes.insert(
            current.clone(),
            Node {
                position: p,
                expanded: false,
                edges: vec![Edge {
                    route: route([2.5, 1., 0.5]),
                    to: key([2.5, 1., 0.5]),
                }],
            },
        );
        j.state.nodes.insert(
            key([2.5, 1., 0.5]),
            Node {
                position: [2.5, 1., 0.5],
                expanded: false,
                edges: vec![],
            },
        );
        assert!(
            choose(&j.state, &current).is_some(),
            "Full search must revisit arrived nodes whose preview is unfinished"
        );
    }
    #[tokio::test]
    async fn eager_dispatch_preserves_partial_provenance_and_never_replays_a_lost_reply() {
        for lost in [false, true] {
            let root = Temp::new();
            let path = root.0.join("nav.json");
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let c = client(format!("http://{}/", listener.local_addr().unwrap()));
            let saved_path = path.clone();
            let server = tokio::spawn(async move {
                eager_open(&listener).await;
                respond(
                    &listener,
                    "POST /v1/bots/fixture/observations/query ",
                    eager_page(vec![route([1.5, 1., 0.5])], false, Some(8), 5000),
                    200,
                )
                .await;
                // A second page would wait five seconds. The fresh useful edge
                // must dispatch before that read interval, without polling it.
                let (stream, value) = tokio::time::timeout(
                    Duration::from_secs(2),
                    request(&listener, "POST /v1/bots/fixture/actions "),
                )
                .await
                .unwrap();
                let action: ActionRequest = serde_json::from_value(value).unwrap();
                let state: serde_json::Value =
                    serde_json::from_slice(&fs::read(&saved_path).unwrap()).unwrap();
                assert_eq!(state["pending"]["request"]["action_id"], action.action_id);
                assert_eq!(state["expansion"]["next_offset"], 8);
                assert_eq!(state["prefixes"][&action.action_id]["next_offset"], 8);
                assert_eq!(state["nodes"][key([0.5, 1., 0.5])]["expanded"], false);
                if lost {
                    drop(stream);
                } else {
                    reply(stream,serde_json::json!({"action_id":action.action_id,"outcome":"running","adapter_version":"fixture","error":null,"evidence":{}}),200).await;
                    eager_finish(&listener, &action.action_id, [1.5, 1., 0.5]).await;
                }
                (listener, action.action_id)
            });
            let result = navigate(&c, goal(), &path, eager_options()).await;
            let (listener, id) = server.await.unwrap();
            if lost {
                assert_eq!(result.unwrap_err().code, ErrorCode::UncertainOutcome);
                let saved: serde_json::Value =
                    serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                assert_eq!(saved["pending"]["request"]["action_id"], id);
                let server = tokio::spawn(async move {
                    respond(&listener, "GET /v1/server ", info("instance"), 200).await;
                    respond(
                        &listener,
                        "GET /v1/bots/fixture/observations ",
                        observation([0.5, 1., 0.5]),
                        200,
                    )
                    .await;
                    eager_finish(&listener, &id, [1.5, 1., 0.5]).await;
                    assert!(
                        tokio::time::timeout(Duration::from_millis(100), listener.accept())
                            .await
                            .is_err(),
                        "Resume reads the original ID, never another POST"
                    );
                });
                let report = navigate(&c, goal(), &path, eager_options()).await.unwrap();
                server.await.unwrap();
                assert_eq!(report.actions, 1);
            } else {
                assert_eq!(result.unwrap().actions, 1);
            }
            let saved: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert!(saved["pending"].is_null());
            assert_eq!(saved["preview"]["eager_intentions"], 1);
            assert_eq!(saved["prefixes"].as_object().unwrap().len(), 1);
        }
    }
    #[tokio::test]
    async fn eager_paused_pages_resume_only_after_validation_and_keep_unread_cursor() {
        let root = Temp::new();
        let path = root.0.join("nav.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let c = client(format!("http://{}/", listener.local_addr().unwrap()));
        let server = tokio::spawn(async move {
            eager_open(&listener).await;
            respond(
                &listener,
                "POST /v1/bots/fixture/observations/query ",
                eager_page(vec![route([0.5, 1., 1.5])], false, Some(8), 500),
                200,
            )
            .await;
            listener
        });
        let first = navigate(
            &c,
            goal(),
            &path,
            Options {
                timeout_ms: 100,
                ..eager_options()
            },
        )
        .await
        .unwrap();
        let listener = server.await.unwrap();
        assert_eq!(first.actions, 0);
        let server = tokio::spawn(async move {
            eager_open(&listener).await;
            let (stream, q) = request(&listener, "POST /v1/bots/fixture/observations/query ").await;
            assert_eq!(q["input"]["offset"], 0);
            assert_eq!(q["input"]["limit"], 1);
            reply(stream, eager_page(vec![], false, Some(1), 25), 200).await;
            let (stream, q) = request(&listener, "POST /v1/bots/fixture/observations/query ").await;
            assert_eq!(q["input"]["offset"], 8);
            reply(
                stream,
                eager_page(vec![route([1.5, 1., 0.5])], false, Some(9), 5000),
                200,
            )
            .await;
            let (stream, value) = tokio::time::timeout(
                Duration::from_secs(2),
                request(&listener, "POST /v1/bots/fixture/actions "),
            )
            .await
            .unwrap();
            let action: ActionRequest = serde_json::from_value(value).unwrap();
            reply(stream,serde_json::json!({"action_id":action.action_id,"outcome":"running","adapter_version":"fixture","error":null,"evidence":{}}),200).await;
            eager_finish(&listener, &action.action_id, [1.5, 1., 0.5]).await;
        });
        let report = navigate(&c, goal(), &path, eager_options()).await.unwrap();
        server.await.unwrap();
        assert_eq!(report.actions, 1);
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["expansion"]["next_offset"], 9);
        assert_eq!(saved["expansion"]["routes"].as_array().unwrap().len(), 2);
        assert_eq!(saved["nodes"][key([0.5, 1., 0.5])]["expanded"], false);
    }
    #[tokio::test]
    async fn eager_saved_prefix_is_usable_only_after_matching_fresh_validation() {
        let root = Temp::new();
        let path = root.0.join("nav.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let c = client(format!("http://{}/", listener.local_addr().unwrap()));
        let server = tokio::spawn(async move {
            eager_open(&listener).await;
            respond(
                &listener,
                "POST /v1/bots/fixture/observations/query ",
                eager_page(vec![route([1.5, 1., 0.5])], false, Some(8), 500),
                200,
            )
            .await;
            listener
        });
        let first = navigate(
            &c,
            goal(),
            &path,
            Options {
                timeout_ms: 100,
                ..Options::default()
            },
        )
        .await
        .unwrap();
        let listener = server.await.unwrap();
        assert_eq!(first.actions, 0);
        let server = tokio::spawn(async move {
            eager_open(&listener).await;
            let (stream, q) = request(&listener, "POST /v1/bots/fixture/observations/query ").await;
            assert_eq!(q["input"]["offset"], 0);
            assert_eq!(q["input"]["limit"], 1);
            reply(stream, eager_page(vec![], false, Some(1), 5000), 200).await;
            let (stream, value) = tokio::time::timeout(
                Duration::from_secs(2),
                request(&listener, "POST /v1/bots/fixture/actions "),
            )
            .await
            .unwrap();
            let action: ActionRequest = serde_json::from_value(value).unwrap();
            reply(stream,serde_json::json!({"action_id":action.action_id,"outcome":"running","adapter_version":"fixture","error":null,"evidence":{}}),200).await;
            eager_finish(&listener, &action.action_id, [1.5, 1., 0.5]).await;
        });
        let report = navigate(&c, goal(), &path, eager_options()).await.unwrap();
        server.await.unwrap();
        assert_eq!(report.actions, 1);
        assert_eq!(report.preview.eager_intentions, 1);
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["expansion"]["next_offset"], 8);
    }
    #[tokio::test]
    async fn eager_dead_end_backtracks_to_an_arrived_partial_frontier_then_reads_its_detour() {
        let root = Temp::new();
        let path = root.0.join("nav.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let c = client(format!("http://{}/", listener.local_addr().unwrap()));
        let a = [0.5, 1., 0.5];
        let b = [1.5, 1., 0.5];
        let end = [2.5, 1., 0.5];
        let detour = [1.5, 1., 1.5];
        let server = tokio::spawn(async move {
            eager_open(&listener).await;
            let page_at = |p: [f64; 3], routes: Vec<Route>, complete, next| {
                let mut page = eager_page(routes, complete, next, 25);
                page["origin"]["position"] = serde_json::json!(p);
                page["scene"] = serde_json::json!(format!("scene-{p:?}"));
                page
            };
            for (step, origin, target) in [(0, a, b), (1, b, end), (2, end, b), (3, b, detour)] {
                let (stream, q) =
                    request(&listener, "POST /v1/bots/fixture/observations/query ").await;
                assert_eq!(q["input"]["offset"], 0);
                if step == 3 {
                    // C was genuinely reached, but B's old prefix was never a
                    // completed graph expansion. Skip C only as an eager edge.
                    reply(stream, page_at(b, vec![route(end)], false, Some(1)), 200).await;
                    let (stream, q) =
                        request(&listener, "POST /v1/bots/fixture/observations/query ").await;
                    assert_eq!(q["input"]["offset"], 1);
                    reply(stream, page_at(b, vec![route(detour)], true, None), 200).await;
                } else {
                    reply(
                        stream,
                        page_at(
                            origin,
                            vec![route(target)],
                            step == 2,
                            if step == 2 { None } else { Some(1) },
                        ),
                        200,
                    )
                    .await;
                }
                let (stream, value) = tokio::time::timeout(
                    Duration::from_secs(2),
                    request(&listener, "POST /v1/bots/fixture/actions "),
                )
                .await
                .unwrap();
                assert_eq!(
                    value["action"]["target"],
                    serde_json::json!({"x":target[0],"y":target[1],"z":target[2]}),
                    "A→B→C must return to partial B before taking its later detour"
                );
                let action: ActionRequest = serde_json::from_value(value).unwrap();
                reply(stream,serde_json::json!({"action_id":action.action_id,"outcome":"running","adapter_version":"fixture","error":null,"evidence":{}}),200).await;
                eager_finish(&listener, &action.action_id, target).await;
            }
            assert!(
                tokio::time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err()
            );
        });
        let report = tokio::time::timeout(
            Duration::from_secs(5),
            navigate(
                &c,
                goal(),
                &path,
                Options {
                    max_steps: 4,
                    ..eager_options()
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
        server.await.unwrap();
        assert_eq!(report.actions, 4);
        assert_eq!(report.preview.eager_intentions, 2);
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["nodes"][key(b)]["expanded"], true);
        assert_eq!(saved["history"][2]["target"], serde_json::json!(b));
        assert_eq!(saved["history"][3]["target"], serde_json::json!(detour));
    }
    #[tokio::test]
    async fn eager_malformed_validation_page_cannot_dispatch_cached_routes_or_drop_ids() {
        for (complete, next) in [
            (false, None),
            (false, Some(0)),
            (false, Some(1025)),
            (true, Some(1)),
        ] {
            let root = Temp::new();
            let path = root.0.join("nav.json");
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let c = client(format!("http://{}/", listener.local_addr().unwrap()));
            let server = tokio::spawn(async move {
                eager_open(&listener).await;
                respond(
                    &listener,
                    "POST /v1/bots/fixture/observations/query ",
                    eager_page(vec![route([1.5, 1., 0.5])], false, Some(8), 200),
                    200,
                )
                .await;
                listener
            });
            let first = navigate(
                &c,
                goal(),
                &path,
                Options {
                    timeout_ms: 50,
                    ..Options::default()
                },
            )
            .await
            .unwrap();
            let listener = server.await.unwrap();
            assert_eq!(first.actions, 0);
            {
                let mut j =
                    Journal::open(&path, goal(), "instance", "actor", [0.5, 1., 0.5]).unwrap();
                j.state.history.push(serde_json::from_value(serde_json::json!({
                    "request":{"action_id":"retained-prior-id","action":{"kind":"start_move","target":{"x":0.5,"y":1.,"z":1.5},"jump":false,"max_ticks":60}},
                    "origin":[0.5,1.,0.5],"scene":"prior","target":[0.5,1.,1.5],"outcome":"cancelled"})).unwrap());
                j.save().unwrap();
            }
            let server = tokio::spawn(async move {
                eager_open(&listener).await;
                let (stream, q) =
                    request(&listener, "POST /v1/bots/fixture/observations/query ").await;
                assert_eq!(q["input"]["limit"], 1);
                reply(stream, eager_page(vec![], complete, next, 25), 200).await;
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), listener.accept())
                        .await
                        .is_err(),
                    "Malformed fresh validation must stop before any mutation or continuation read"
                );
            });
            let error = navigate(
                &c,
                goal(),
                &path,
                Options {
                    max_steps: 2,
                    ..eager_options()
                },
            )
            .await
            .unwrap_err();
            assert_eq!(
                error.code,
                if complete {
                    ErrorCode::UnsupportedVersion
                } else {
                    ErrorCode::NotReady
                }
            );
            server.await.unwrap();
            let saved: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(
                saved["history"][0]["request"]["action_id"],
                "retained-prior-id"
            );
            assert_eq!(saved["expansion"]["next_offset"], 8);
            assert!(saved["pending"].is_null());
            assert_eq!(saved["sequence"], 0);
        }
    }
    #[tokio::test]
    async fn eager_mode_keeps_full_detour_search_block_policy_and_node_budget() {
        for case in ["detour", "blocked", "node_budget"] {
            let root = Temp::new();
            let path = root.0.join("nav.json");
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let c = client(format!("http://{}/", listener.local_addr().unwrap()));
            let server = tokio::spawn(async move {
                eager_open(&listener).await;
                let target = if case == "detour" {
                    [-0.5, 1., 0.5]
                } else {
                    [1.5, 1., 0.5]
                };
                let mut r = route(target);
                if case == "blocked" {
                    r.touched_blocks = vec!["fixture:forbidden".into()];
                }
                respond(
                    &listener,
                    "POST /v1/bots/fixture/observations/query ",
                    eager_page(vec![r], false, Some(8), 25),
                    200,
                )
                .await;
                if case != "node_budget" {
                    let (stream, q) =
                        request(&listener, "POST /v1/bots/fixture/observations/query ").await;
                    assert_eq!(
                        q["input"]["offset"], 8,
                        "No improving prefix: complete the bounded graph"
                    );
                    reply(stream, eager_page(vec![], true, None, 25), 200).await;
                }
                if case == "detour" {
                    let (stream, value) =
                        request(&listener, "POST /v1/bots/fixture/actions ").await;
                    let action: ActionRequest = serde_json::from_value(value).unwrap();
                    assert!(
                        matches!(action.action,Action::StartMove{target,..} if target.x == -0.5)
                    );
                    reply(stream,serde_json::json!({"action_id":action.action_id,"outcome":"running","adapter_version":"fixture","error":null,"evidence":{}}),200).await;
                    eager_finish(&listener, &action.action_id, target).await;
                }
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), listener.accept())
                        .await
                        .is_err(),
                    "Budget/policy denial must not dispatch; a detour must stop at the same action budget"
                );
            });
            let report = navigate(
                &c,
                goal(),
                &path,
                Options {
                    max_nodes: if case == "node_budget" { 1 } else { 1024 },
                    blocked_blocks: if case == "blocked" {
                        vec!["fixture:forbidden".into()]
                    } else {
                        vec![]
                    },
                    ..eager_options()
                },
            )
            .await
            .unwrap();
            server.await.unwrap();
            assert_eq!(report.actions, usize::from(case == "detour"));
            assert_eq!(
                report.status,
                if case == "blocked" {
                    "no_known_route"
                } else {
                    "budget_exhausted"
                }
            );
            assert_eq!(report.preview.eager_intentions, 0);
        }
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn eager_local_preemption_keeps_prefix_without_claiming_or_replaying_dispatch() {
        use std::os::unix::fs::PermissionsExt;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        struct Socket(PathBuf);
        impl Drop for Socket {
            fn drop(&mut self) {
                let _ = fs::remove_file(&self.0);
            }
        }
        let root = Temp::new();
        let path = root.0.join("nav.json");
        // Unix socket paths must fit Darwin's short sun_path independently of
        // the test runner's potentially long temporary-directory prefix.
        let socket = Socket(Path::new("/tmp").join(format!(
            "mc-eager-{}-{}.sock",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )));
        let local = tokio::net::UnixListener::bind(&socket.0).unwrap();
        fs::set_permissions(&socket.0, fs::Permissions::from_mode(0o600)).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut c = client(format!("http://{}/", listener.local_addr().unwrap()));
        c.local_arbiter = Some(socket.0.clone());
        let server = tokio::spawn(async move {
            eager_open(&listener).await;
            respond(
                &listener,
                "POST /v1/bots/fixture/observations/query ",
                eager_page(vec![route([1.5, 1., 0.5])], false, Some(8), 5000),
                200,
            )
            .await;
            listener
        });
        let arbiter = tokio::spawn(async move {
            let (socket, _) = local.accept().await.unwrap();
            let mut socket = tokio::io::BufReader::new(socket);
            let mut line = String::new();
            socket.read_line(&mut line).await.unwrap();
            let request: LocalActionRequest = serde_json::from_str(&line).unwrap();
            assert_eq!(request.version, LOCAL_CONTROL_VERSION);
            let response =
                LocalActionResponse::Error(error(ErrorCode::LocalPreempted, "No gateway dispatch"));
            let mut bytes = serde_json::to_vec(&response).unwrap();
            bytes.push(b'\n');
            socket.get_mut().write_all(&bytes).await.unwrap();
        });
        let report = tokio::time::timeout(
            Duration::from_secs(2),
            navigate(&c, goal(), &path, eager_options()),
        )
        .await
        .unwrap()
        .unwrap();
        arbiter.await.unwrap();
        let listener = server.await.unwrap();
        assert_eq!(report.status, "preempted");
        assert!(report.pending_action.is_none());
        assert_eq!(report.preview.eager_intentions, 1);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "Local refusal must produce zero HTTP gameplay POSTs"
        );
        let state: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(state["history"][0]["outcome"], "rejected");
        assert_eq!(
            state["history"][0]["arrival_verification"]["gateway_dispatch_attempted"],
            false
        );
        assert_eq!(state["prefixes"].as_object().unwrap().len(), 1);
        assert_eq!(state["expansion"]["next_offset"], 8);
    }
    #[test]
    fn verified_arrival_drift_expands_the_reached_node_only_in_the_same_life_and_clock() {
        let root = Temp::new();
        let mut j = Journal::open(
            &root.0.join("nav.json"),
            goal(),
            "instance",
            "actor",
            [1.7, 1., 0.5],
        )
        .unwrap();
        j.state.history.push(Attempt {
            request: ActionRequest {
                action_id: "arrived".into(),
                action: Action::StartMove {
                    target: Position {
                        x: 1.5,
                        y: 1.,
                        z: 0.5,
                    },
                    jump: false,
                    max_ticks: 60,
                },
            },
            origin: [0.5, 1., 0.5],
            source_node: Some(key([0.5, 1., 0.5])),
            scene: "fixture".into(),
            target: [1.5, 1., 0.5],
            outcome: Some(Outcome::Observed),
            arrival_verification: Some(serde_json::json!({"target_reached":true,
                "native_after_position":[1.535,1.,0.5],"entity_id":1,
                "dimension":"fixture:world","epoch":"clock-a"})),
        });
        let mut observed: Observation =
            serde_json::from_value(observation([1.7, 1., 0.5])).unwrap();
        observed.player["entity_id"] = serde_json::json!(1);
        observed.world["sample_clock"] = serde_json::json!({"epoch":"clock-a"});
        let canonical = current_node(&j.state, &observed, [1.7, 1., 0.5]);
        assert_eq!(canonical, key([1.5, 1., 0.5]));
        assert_ne!(
            canonical,
            key([1.7, 1., 0.5]),
            "The old rounded origin left the arrived node unexplored"
        );
        j.state.nodes.insert(
            canonical.clone(),
            Node {
                position: [1.7, 1., 0.5],
                expanded: true,
                edges: vec![Edge {
                    route: route([2.5, 1., 0.5]),
                    to: key([2.5, 1., 0.5]),
                }],
            },
        );
        j.state.nodes.insert(
            key([2.5, 1., 0.5]),
            Node {
                position: [2.5, 1., 0.5],
                expanded: false,
                edges: Vec::new(),
            },
        );
        assert_eq!(
            choose(&j.state, &canonical).unwrap().target,
            [2.5, 1., 0.5],
            "Expand from the arrived waypoint to another frontier instead of correcting the drift"
        );
        for case in [
            "life",
            "clock",
            "dimension",
            "distant",
            "unverified",
            "legacy",
        ] {
            let mut o = observed.clone();
            let mut p = [1.7, 1., 0.5];
            match case {
                "life" => o.player["entity_id"] = serde_json::json!(2),
                "clock" => o.world["sample_clock"]["epoch"] = serde_json::json!("clock-b"),
                "dimension" => o.player["dimension"] = serde_json::json!("other:world"),
                "distant" => p[0] = 1.9,
                "unverified" => {
                    j.state.history[0].arrival_verification.as_mut().unwrap()["target_reached"] =
                        serde_json::json!(false)
                }
                _ => j.state.history[0].arrival_verification = None,
            }
            assert_eq!(
                current_node(&j.state, &o, p),
                key(p),
                "{case} cannot reuse arrival identity"
            );
        }
    }
    #[test]
    fn a_released_swim_rise_is_not_selected_again_after_vertical_settling() {
        let root = Temp::new();
        let position = [0.5, 62., 0.5];
        let target = [0.5, 62.4888889, 0.5];
        let mut j = Journal::open(
            &root.0.join("nav.json"),
            goal(),
            "instance",
            "actor",
            position,
        )
        .unwrap();
        j.state.history.push(Attempt {
            request: ActionRequest {
                action_id: "swim-rise".into(),
                action: Action::StartMove {
                    target: Position {
                        x: target[0],
                        y: target[1],
                        z: target[2],
                    },
                    jump: false,
                    max_ticks: 100,
                },
            },
            origin: position,
            source_node: Some(key(position)),
            scene: "wet-bank".into(),
            target,
            outcome: Some(Outcome::Observed),
            arrival_verification: Some(serde_json::json!({"target_reached":true,
                "native_after_position":[0.5,62.52,0.5],"entity_id":1,
                "dimension":"fixture:world","epoch":"clock-a"})),
        });
        let mut o: Observation = serde_json::from_value(observation(position)).unwrap();
        o.player["entity_id"] = serde_json::json!(1);
        o.player["in_water"] = serde_json::json!(true);
        o.player["control"] = serde_json::json!({"action_id":"swim-rise",
            "outcome":"observed","input_released":true});
        o.world["sample_clock"] = serde_json::json!({"epoch":"clock-a"});
        assert_eq!(current_node(&j.state, &o, position), key(target));
        let next = [1.5, 63., 0.5];
        j.state.nodes.insert(
            key(target),
            Node {
                position,
                expanded: true,
                edges: vec![Edge {
                    route: route(next),
                    to: key(next),
                }],
            },
        );
        j.state.nodes.insert(
            key(next),
            Node {
                position: next,
                expanded: false,
                edges: vec![],
            },
        );
        assert_eq!(
            choose(&j.state, &current_node(&j.state, &o, position))
                .unwrap()
                .target,
            next
        );
        for case in [
            "dry",
            "missing_water",
            "different_control",
            "running",
            "unreleased",
            "different_life",
            "clock",
            "unverified",
            "cancelled",
            "far_vertical",
            "far_horizontal",
        ] {
            let mut changed = o.clone();
            let mut p = position;
            let mut state: State =
                serde_json::from_value(serde_json::to_value(&j.state).unwrap()).unwrap();
            match case {
                "dry" => changed.player["in_water"] = serde_json::json!(false),
                "missing_water" => changed
                    .player
                    .as_object_mut()
                    .unwrap()
                    .remove("in_water")
                    .map(|_| ())
                    .unwrap(),
                "different_control" => {
                    changed.player["control"]["action_id"] = serde_json::json!("other")
                }
                "running" => changed.player["control"]["outcome"] = serde_json::json!("running"),
                "unreleased" => {
                    changed.player["control"]["input_released"] = serde_json::json!(false)
                }
                "different_life" => changed.player["entity_id"] = serde_json::json!(2),
                "clock" => changed.world["sample_clock"]["epoch"] = serde_json::json!("clock-b"),
                "unverified" => {
                    state.history[0].arrival_verification.as_mut().unwrap()["target_reached"] =
                        serde_json::json!(false)
                }
                "cancelled" => state.history[0].outcome = Some(Outcome::Cancelled),
                "far_vertical" => p[1] = 61.8,
                "far_horizontal" => p[0] = 0.7,
                _ => unreachable!(),
            }
            assert_eq!(current_node(&state, &changed, p), key(p), "{case}");
        }
    }
    async fn respond(
        listener: &tokio::net::TcpListener,
        expected: &str,
        value: serde_json::Value,
        status: u16,
    ) {
        let (stream, _) = request(listener, expected).await;
        reply(stream, value, status).await;
    }
    async fn request(
        listener: &tokio::net::TcpListener,
        expected: &str,
    ) -> (
        tokio::io::BufReader<tokio::net::TcpStream>,
        serde_json::Value,
    ) {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt};
        let (stream, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut stream = tokio::io::BufReader::new(stream);
        let mut line = String::new();
        stream.read_line(&mut line).await.unwrap();
        assert!(line.starts_with(expected), "Unexpected request: {line}");
        let public = line.starts_with("GET /v1/server ");
        let mut length = 0;
        loop {
            line.clear();
            stream.read_line(&mut line).await.unwrap();
            if line == "\r\n" {
                break;
            }
            if public {
                assert!(!line.to_ascii_lowercase().starts_with("authorization:"));
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                length = value.trim().parse::<usize>().unwrap();
            }
        }
        assert!(length <= 8192);
        let mut bytes = vec![0; length];
        stream.read_exact(&mut bytes).await.unwrap();
        let value = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (stream, value)
    }
    async fn reply(
        mut stream: tokio::io::BufReader<tokio::net::TcpStream>,
        value: serde_json::Value,
        status: u16,
    ) {
        use tokio::io::AsyncWriteExt;
        let body = value.to_string();
        let response = format!(
            "HTTP/1.1 {status} fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .get_mut()
            .write_all(response.as_bytes())
            .await
            .unwrap();
    }
    fn info(instance: &str) -> serde_json::Value {
        serde_json::json!({"api_version":API_VERSION,"instance_id":instance,"minecraft_version":"fixture","platform":"fixture","adapter_version":"fixture","ready":true,"capabilities":[CAPABILITY],"limits":{}})
    }
    fn observation(p: [f64; 3]) -> serde_json::Value {
        serde_json::json!({"revision":"fixture","player":{"uuid":"actor","dimension":"fixture:world","position":p},"world":{},"menu":null})
    }
    fn client(endpoint: String) -> Client {
        Client::with_profile(&Profile {
            endpoint,
            instance_id: "instance".into(),
            credential: Credential {
                bot_id: "fixture".into(),
                player_uuid: "actor".into(),
                key: "f".repeat(64),
            },
            session: Some(Session {
                session_id: "fixture-session".into(),
                adapter_version: "fixture".into(),
            }),
        })
        .unwrap()
        .without_local_arbiter()
    }
    fn busy_preview() -> serde_json::Value {
        serde_json::json!({"format":"minecraft-movement-options-v1","origin":null,
            "scene":"","options":[],"complete":false,"next_offset":null,
            "retry_after_ms":25,"busy":true,"prediction_not_effect_proof":true})
    }
    #[tokio::test]
    async fn busy_previews_retry_without_misreporting_version_or_dispatching_a_mutation() {
        for case in [
            "ready",
            "deadline",
            "malformed",
            "version",
            "empty_ready_scene",
        ] {
            let root = Temp::new();
            let path = root.0.join("navigation.json");
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let c = client(format!("http://{}/", listener.local_addr().unwrap()));
            let server = tokio::spawn(async move {
                respond(&listener, "GET /v1/server ", info("instance"), 200).await;
                for _ in 0..2 {
                    respond(
                        &listener,
                        "GET /v1/bots/fixture/observations ",
                        observation([0.5, 1., 0.5]),
                        200,
                    )
                    .await;
                }
                let mut pending = busy_preview();
                match case {
                    "deadline" => pending["retry_after_ms"] = serde_json::json!(5000),
                    "malformed" => pending["complete"] = serde_json::json!(true),
                    "version" => pending["format"] = serde_json::json!("future-v2"),
                    "empty_ready_scene" => {
                        pending["origin"] = serde_json::json!({
                        "position":[0.5,1.,0.5],"dimension":"fixture:world"})
                    }
                    _ => (),
                }
                respond(
                    &listener,
                    "POST /v1/bots/fixture/observations/query ",
                    pending,
                    200,
                )
                .await;
                if case == "ready" {
                    let (stream, query) =
                        request(&listener, "POST /v1/bots/fixture/observations/query ").await;
                    assert_eq!(query["input"]["offset"], 0);
                    reply(
                        stream,
                        serde_json::json!({"format":"minecraft-movement-options-v1",
                        "origin":{"position":[0.5,1.,0.5],"dimension":"fixture:world"},
                        "scene":"ready-scene","options":[],"complete":true,
                        "next_offset":null,"retry_after_ms":25}),
                        200,
                    )
                    .await;
                }
                assert!(
                    tokio::time::timeout(Duration::from_millis(300), listener.accept())
                        .await
                        .is_err(),
                    "Readiness must not invent a gameplay intention"
                );
            });
            let result = navigate(
                &c,
                goal(),
                &path,
                Options {
                    timeout_ms: 200,
                    ..Options::default()
                },
            )
            .await;
            match case {
                "ready" => assert_eq!(result.unwrap().status, "no_known_route"),
                "deadline" => assert_eq!(result.unwrap().status, "budget_exhausted"),
                _ => assert_eq!(result.unwrap_err().code, ErrorCode::UnsupportedVersion),
            }
            let saved: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert!(saved["pending"].is_null());
            assert_eq!(
                saved["preview_waits"],
                if matches!(case, "ready" | "deadline") {
                    1
                } else {
                    0
                }
            );
            server.await.unwrap();
        }
    }
    #[tokio::test]
    async fn a_navigation_deadline_preserves_completed_preview_pages() {
        // Transport fixture only. Page zero completes, then its advertised
        // retry interval outlasts this invocation's remaining wall budget.
        let root = Temp::new();
        let path = root.0.join("navigation.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let c = client(format!("http://{}/", listener.local_addr().unwrap()));
        let server = tokio::spawn(async move {
            respond(&listener, "GET /v1/server ", info("instance"), 200).await;
            for _ in 0..2 {
                respond(
                    &listener,
                    "GET /v1/bots/fixture/observations ",
                    observation([0.5, 1., 0.5]),
                    200,
                )
                .await;
            }
            respond(
                &listener,
                "POST /v1/bots/fixture/observations/query ",
                serde_json::json!({"format":"minecraft-movement-options-v1",
                    "origin":{"position":[0.5,1.,0.5],"dimension":"fixture:world"},
                    "scene":"unchanged-scene","options":[route([1.5,1.,0.5])],
                    "complete":false,"next_offset":8,"retry_after_ms":1000}),
                200,
            )
            .await;
            listener
        });
        let report = navigate(
            &c,
            goal(),
            &path,
            Options {
                timeout_ms: 200,
                ..Options::default()
            },
        )
        .await
        .unwrap();
        let listener = server.await.unwrap();
        assert_eq!(report.status, "budget_exhausted");
        assert_eq!(report.actions, 0);
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            saved["expansion"]["next_offset"], 8,
            "A completed read page must survive cancellation instead of restarting at zero forever"
        );
        assert_eq!(saved["expansion"]["routes"].as_array().unwrap().len(), 1);
        assert!(
            saved["pending"].is_null(),
            "No gameplay mutation has been dispatched"
        );
        let server = tokio::spawn(async move {
            respond(&listener, "GET /v1/server ", info("instance"), 200).await;
            for _ in 0..2 {
                respond(
                    &listener,
                    "GET /v1/bots/fixture/observations ",
                    observation([0.5, 1., 0.5]),
                    200,
                )
                .await;
            }
            let (stream, query) =
                request(&listener, "POST /v1/bots/fixture/observations/query ").await;
            assert_eq!(query["input"]["offset"], 0);
            assert_eq!(
                query["input"]["limit"], 1,
                "Resume first validates a small current scene page"
            );
            reply(stream,serde_json::json!({"format":"minecraft-movement-options-v1",
                "origin":{"position":[0.5,1.,0.5],"dimension":"fixture:world"},
                "scene":"unchanged-scene","options":[],"complete":false,"next_offset":1,"retry_after_ms":25}),200).await;
            let (stream, query) =
                request(&listener, "POST /v1/bots/fixture/observations/query ").await;
            assert_eq!(
                query["input"]["offset"], 8,
                "Completed read work must not restart at zero"
            );
            reply(stream,serde_json::json!({"format":"minecraft-movement-options-v1",
                "origin":{"position":[0.5,1.,0.5],"dimension":"fixture:world"},
                "scene":"unchanged-scene","options":[],"complete":true,"next_offset":null,"retry_after_ms":25}),200).await;
            let (stream, value) = request(&listener, "POST /v1/bots/fixture/actions ").await;
            let action: ActionRequest = serde_json::from_value(value).unwrap();
            assert!(matches!(action.action,Action::StartMove {target,..} if target.x==1.5));
            reply(
                stream,
                serde_json::json!({"action_id":action.action_id,"outcome":"running",
                "adapter_version":"fixture","error":null,"evidence":{}}),
                200,
            )
            .await;
            respond(&listener,&format!("GET /v1/bots/fixture/actions/{} ",action.action_id),
                serde_json::json!({"action_id":action.action_id,"outcome":"observed","adapter_version":"fixture",
                    "error":null,"evidence":{"native_effect_verified":true,"after":{"player":{
                        "uuid":"actor","dimension":"fixture:world","position":[1.5,1.,0.5]}}}}),200).await;
            respond(
                &listener,
                "GET /v1/bots/fixture/observations ",
                observation([1.5, 1., 0.5]),
                200,
            )
            .await;
            assert!(
                tokio::time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err(),
                "The resumed invocation must not replay or submit a second intention"
            );
        });
        let report = navigate(
            &c,
            goal(),
            &path,
            Options {
                timeout_ms: 2000,
                max_steps: 1,
                ..Options::default()
            },
        )
        .await
        .unwrap();
        server.await.unwrap();
        assert_eq!(report.status, "budget_exhausted");
        assert_eq!(report.actions, 1);
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(saved["pending"].is_null());
        assert!(saved["expansion"].is_null());
    }
    #[tokio::test]
    async fn resumed_previews_reject_changed_scenes_lives_clocks_and_block_policy() {
        for (case, eager_progress) in ["scene", "life", "clock", "velocity", "policy"]
            .into_iter()
            .flat_map(|case| [false, true].into_iter().map(move |eager| (case, eager)))
        {
            let root = Temp::new();
            let path = root.0.join("navigation.json");
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let c = client(format!("http://{}/", listener.local_addr().unwrap()));
            let server = tokio::spawn(async move {
                respond(&listener, "GET /v1/server ", info("instance"), 200).await;
                for _ in 0..2 {
                    respond(
                        &listener,
                        "GET /v1/bots/fixture/observations ",
                        observation([0.5, 1., 0.5]),
                        200,
                    )
                    .await;
                }
                let mut r = route([1.5, 1., 0.5]);
                r.touched_blocks = vec!["minecraft:stone".into()];
                respond(&listener,"POST /v1/bots/fixture/observations/query ",
                    serde_json::json!({"format":"minecraft-movement-options-v1",
                        "origin":{"position":[0.5,1.,0.5],"dimension":"fixture:world"},"scene":"scene-a",
                        "options":[r],"complete":false,"next_offset":8,"retry_after_ms":1000}),200).await;
                listener
            });
            let first = navigate(
                &c,
                goal(),
                &path,
                Options {
                    timeout_ms: 200,
                    ..Options::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(first.actions, 0);
            let listener = server.await.unwrap();
            let server = tokio::spawn(async move {
                respond(&listener, "GET /v1/server ", info("instance"), 200).await;
                let mut current = observation([0.5, 1., 0.5]);
                match case {
                    "life" => current["player"]["entity_id"] = serde_json::json!(2),
                    "clock" => {
                        current["world"]["sample_clock"] =
                            serde_json::json!({"epoch":"replacement"})
                    }
                    "velocity" => current["player"]["velocity"] = serde_json::json!([0.1, 0., 0.]),
                    _ => (),
                }
                for _ in 0..2 {
                    respond(
                        &listener,
                        "GET /v1/bots/fixture/observations ",
                        current.clone(),
                        200,
                    )
                    .await;
                }
                let (stream, query) =
                    request(&listener, "POST /v1/bots/fixture/observations/query ").await;
                assert_eq!(query["input"]["offset"], 0);
                let validation = matches!(case, "scene" | "policy");
                assert_eq!(
                    query["input"]["limit"].as_u64(),
                    validation.then_some(1),
                    "{case}"
                );
                reply(
                    stream,
                    serde_json::json!({"format":"minecraft-movement-options-v1",
                    "origin":{"position":[0.5,1.,0.5],"dimension":"fixture:world"},
                    "scene":if case=="scene"{"scene-b"}else{"scene-a"},"options":[],
                    "complete":case!="policy","next_offset":if case=="policy"{Some(1)}else{None},
                    "retry_after_ms":25}),
                    200,
                )
                .await;
                if case == "policy" {
                    let (stream, query) =
                        request(&listener, "POST /v1/bots/fixture/observations/query ").await;
                    assert_eq!(query["input"]["offset"], 8);
                    reply(stream,serde_json::json!({"format":"minecraft-movement-options-v1",
                        "origin":{"position":[0.5,1.,0.5],"dimension":"fixture:world"},"scene":"scene-a",
                        "options":[],"complete":true,"next_offset":null,"retry_after_ms":25}),200).await;
                }
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), listener.accept())
                        .await
                        .is_err(),
                    "Stale or newly forbidden routes must not dispatch a mutation: {case}"
                );
            });
            let report = navigate(
                &c,
                goal(),
                &path,
                Options {
                    timeout_ms: 2000,
                    eager_progress,
                    blocked_blocks: if case == "policy" {
                        vec!["minecraft:stone".into()]
                    } else {
                        Vec::new()
                    },
                    ..Options::default()
                },
            )
            .await
            .unwrap();
            server.await.unwrap();
            assert_eq!(report.status, "no_known_route", "{case}");
            assert_eq!(report.actions, 0);
            let saved: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert!(saved["expansion"].is_null());
            assert!(saved["pending"].is_null());
        }
    }
    #[test]
    fn only_explicit_local_pre_dispatch_rejection_retires_pending_without_native_claims() {
        let temp = Temp::new();
        let path = temp.0.join("nav.json");
        let mut j = Journal::open(&path, goal(), "instance", "actor", [0.5, 1., 0.5]).unwrap();
        j.state.pending = Some(Attempt {
            request: ActionRequest {
                action_id: "local-denial".into(),
                action: Action::StartMove {
                    target: Position {
                        x: 1.5,
                        y: 1.,
                        z: 0.5,
                    },
                    jump: false,
                    max_ticks: 60,
                },
            },
            origin: [0.5, 1., 0.5],
            scene: "fixture".into(),
            target: [1.5, 1., 0.5],
            outcome: None,
            source_node: None,
            arrival_verification: None,
        });
        j.save().unwrap();
        let mut client = client("http://127.0.0.1:1".into());
        let denied = error(ErrorCode::LocalPreempted, "No gateway dispatch");
        assert!(
            retire_local_preemption(&client, &mut j, &denied)
                .unwrap()
                .is_none()
        );
        assert!(
            j.state.pending.is_some(),
            "a remote error cannot establish non-dispatch"
        );
        client.local_arbiter = Some(temp.0.join("body.sock"));
        for code in [
            ErrorCode::Conflict,
            ErrorCode::Internal,
            ErrorCode::UncertainOutcome,
        ] {
            assert!(
                retire_local_preemption(&client, &mut j, &error(code, "ambiguous"))
                    .unwrap()
                    .is_none()
            );
            assert!(j.state.pending.is_some());
        }
        let report = retire_local_preemption(&client, &mut j, &denied)
            .unwrap()
            .unwrap();
        assert_eq!(report.status, "preempted");
        assert!(report.pending_action.is_none());
        drop(j);
        let j = Journal::open(&path, goal(), "instance", "actor", [0.5, 1., 0.5]).unwrap();
        assert_eq!(j.state.history.len(), 1);
        assert_eq!(j.state.history[0].request.action_id, "local-denial");
        let evidence = j.state.history[0].arrival_verification.as_ref().unwrap();
        assert_eq!(evidence["gateway_dispatch_attempted"], false);
        assert_eq!(evidence["native_effect_verified"], false);
        assert!(
            j.state.blocked.is_empty(),
            "arbitration did not evaluate physical feasibility"
        );
    }
    #[tokio::test]
    async fn terminal_target_miss_is_retained_without_replay_while_unbound_receipts_stay_fenced() {
        for case in ["miss", "foreign", "unverified"] {
            let temp = Temp::new();
            let path = temp.0.join("nav.json");
            {
                let mut j =
                    Journal::open(&path, goal(), "instance", "actor", [0.5, 1., 0.5]).unwrap();
                j.state.pending = Some(Attempt {
                    request: ActionRequest {
                        action_id: "ended".into(),
                        action: Action::StartMove {
                            target: Position {
                                x: 1.5,
                                y: 1.,
                                z: 0.5,
                            },
                            jump: false,
                            max_ticks: 60,
                        },
                    },
                    origin: [0.5, 1., 0.5],
                    scene: "scene".into(),
                    target: [1.5, 1., 0.5],
                    outcome: None,
                    source_node: Some("canonical-source".into()),
                    arrival_verification: None,
                });
                j.save().unwrap();
            }
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let c = client(format!("http://{}/", listener.local_addr().unwrap()));
            let server = tokio::spawn(async move {
                respond(&listener, "GET /v1/server ", info("instance"), 200).await;
                respond(
                    &listener,
                    "GET /v1/bots/fixture/observations ",
                    observation([0.5, 1., 0.5]),
                    200,
                )
                .await;
                respond(&listener,"GET /v1/bots/fixture/actions/ended ",serde_json::json!({"action_id":"ended","outcome":"observed","adapter_version":"fixture","error":null,"evidence":{"native_effect_verified":case!="unverified","after":{"player":{"uuid":if case=="foreign"{"other"}else{"actor"},"dimension":"fixture:world","position":[1.7,1.,0.5]}}}}),200).await;
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), listener.accept())
                        .await
                        .is_err(),
                    "No reconciliation or mutation is silently dispatched"
                );
            });
            let result = navigate(&c, goal(), &path, Options::default()).await;
            server.await.unwrap();
            let j = Journal::open(&path, goal(), "instance", "actor", [0.5, 1., 0.5]).unwrap();
            match case {
                "miss" => {
                    let report = result.unwrap();
                    assert_eq!(report.status, "target_not_reached");
                    assert!(report.pending_action.is_none());
                    assert!(j.state.pending.is_none());
                    assert_eq!(j.state.history[0].outcome, Some(Outcome::Observed));
                    assert_eq!(
                        j.state.history[0].arrival_verification.as_ref().unwrap()["target_reached"],
                        false
                    );
                    assert_eq!(j.state.history[0].request.action_id, "ended");
                    assert!(
                        j.state.blocked.contains(&blocked_key(
                            "canonical-source",
                            &key([1.5, 1., 0.5]),
                            "scene"
                        )),
                        "A failed edge must remain fenced under the persisted source node after reopen"
                    );
                }
                "foreign" => {
                    assert_eq!(result.unwrap_err().code, ErrorCode::Conflict);
                    assert!(j.state.pending.is_some());
                }
                _ => {
                    assert_eq!(result.unwrap().status, "needs_reconciliation");
                    assert!(j.state.pending.is_some());
                }
            }
        }
    }
    #[tokio::test]
    async fn missing_interrupted_intention_is_inspected_but_never_submitted_again() {
        let temp = Temp::new();
        let path = temp.0.join("nav.json");
        {
            let mut j = Journal::open(&path, goal(), "instance", "actor", [0.5, 1., 0.5]).unwrap();
            j.state.pending = Some(Attempt {
                request: ActionRequest {
                    action_id: "interrupted".into(),
                    action: Action::StartMove {
                        target: Position {
                            x: 1.5,
                            y: 1.,
                            z: 0.5,
                        },
                        jump: false,
                        max_ticks: 60,
                    },
                },
                origin: [0.5, 1., 0.5],
                scene: "scene".into(),
                target: [1.5, 1., 0.5],
                outcome: None,
                source_node: None,
                arrival_verification: None,
            });
            j.save().unwrap();
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let c = client(format!("http://{}/", listener.local_addr().unwrap()));
        let server = tokio::spawn(async move {
            respond(&listener, "GET /v1/server ", info("instance"), 200).await;
            respond(
                &listener,
                "GET /v1/bots/fixture/observations ",
                observation([0.5, 1., 0.5]),
                200,
            )
            .await;
            respond(
                &listener,
                "GET /v1/bots/fixture/actions/interrupted ",
                serde_json::to_value(ApiError::new(ErrorCode::NotFound, "Unknown action")).unwrap(),
                404,
            )
            .await;
            assert!(
                tokio::time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err(),
                "No replay or other gameplay mutation may follow a missing ID"
            );
        });
        let report = navigate(&c, goal(), &path, Options::default())
            .await
            .unwrap();
        assert_eq!(report.status, "needs_reconciliation");
        assert_eq!(report.pending_action.as_deref(), Some("interrupted"));
        server.await.unwrap();
    }
    #[tokio::test]
    async fn a_replacement_instance_never_receives_the_existing_key() {
        let temp = Temp::new();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let c = client(format!("http://{}/", listener.local_addr().unwrap()));
        let server = tokio::spawn(async move {
            respond(&listener, "GET /v1/server ", info("replacement"), 200).await;
            assert!(
                tokio::time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err()
            );
        });
        assert_eq!(
            navigate(&c, goal(), &temp.0.join("nav.json"), Options::default())
                .await
                .unwrap_err()
                .code,
            ErrorCode::Conflict
        );
        server.await.unwrap();
    }
    #[test]
    fn reopening_removes_only_legacy_cancelled_only_edges_without_changing_intentions() {
        let temp = Temp::new();
        let path = temp.0.join("nav.json");
        let attempt = |id: &str, x: f64, outcome| Attempt {
            request: ActionRequest {
                action_id: id.into(),
                action: Action::StartMove {
                    target: Position { x, y: 1., z: 0.5 },
                    jump: false,
                    max_ticks: 60,
                },
            },
            origin: [0.5, 1., 0.5],
            source_node: Some("canonical-source".into()),
            scene: "scene".into(),
            target: [x, 1., 0.5],
            outcome,
            arrival_verification: None,
        };
        let cancelled = attempt("cancel-only", 1.5, Some(Outcome::Cancelled));
        let cancelled_after_failure = attempt("cancel-mixed", 2.5, Some(Outcome::Cancelled));
        let failed = attempt("failed", 2.5, Some(Outcome::Rejected));
        let pending = attempt("still-pending", 3.5, None);
        {
            let mut j = Journal::open(&path, goal(), "instance", "actor", [0.5, 1., 0.5]).unwrap();
            j.state.blocked.insert(attempt_edge_key(&cancelled));
            j.state.blocked.insert(attempt_edge_key(&failed));
            j.state.blocked.insert("unattributed-old-edge".into());
            j.state.history = vec![cancelled.clone(), cancelled_after_failure, failed.clone()];
            j.state.pending = Some(pending.clone());
            j.save().unwrap();
        }
        let j = Journal::open(&path, goal(), "instance", "actor", [0.5, 1., 0.5]).unwrap();
        assert!(!j.state.blocked.contains(&attempt_edge_key(&cancelled)));
        assert!(j.state.blocked.contains(&attempt_edge_key(&failed)));
        assert!(j.state.blocked.contains("unattributed-old-edge"));
        assert_eq!(j.state.history.len(), 3);
        assert_eq!(j.state.history[0].request.action_id, "cancel-only");
        assert_eq!(
            serde_json::to_value(&j.state.pending.as_ref().unwrap().request).unwrap(),
            serde_json::to_value(pending.request).unwrap()
        );
    }
    #[tokio::test]
    async fn a_defensively_cancelled_intention_is_resolved_before_reporting_the_fresh_pose() {
        for (fresh_pose, expected) in [([5.5, 1., 0.5], "reached"), ([1.0, 1., 0.5], "preempted")] {
            let temp = Temp::new();
            let path = temp.0.join("nav.json");
            {
                let mut j =
                    Journal::open(&path, goal(), "instance", "actor", [0.5, 1., 0.5]).unwrap();
                j.state.pending = Some(Attempt {
                    request: ActionRequest {
                        action_id: "cancelled".into(),
                        action: Action::StartMove {
                            target: Position {
                                x: 1.5,
                                y: 1.,
                                z: 0.5,
                            },
                            jump: false,
                            max_ticks: 60,
                        },
                    },
                    origin: [0.5, 1., 0.5],
                    scene: "scene".into(),
                    target: [1.5, 1., 0.5],
                    outcome: None,
                    source_node: None,
                    arrival_verification: None,
                });
                j.save().unwrap();
            }
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let c = client(format!("http://{}/", listener.local_addr().unwrap()));
            let server = tokio::spawn(async move {
                respond(&listener, "GET /v1/server ", info("instance"), 200).await;
                respond(
                    &listener,
                    "GET /v1/bots/fixture/observations ",
                    observation(fresh_pose),
                    200,
                )
                .await;
                respond(&listener,"GET /v1/bots/fixture/actions/cancelled ",serde_json::json!({"action_id":"cancelled","outcome":"cancelled","adapter_version":"fixture","evidence":{"input_released":true},"error":null}),200).await;
                respond(
                    &listener,
                    "GET /v1/bots/fixture/observations ",
                    observation(fresh_pose),
                    200,
                )
                .await;
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), listener.accept())
                        .await
                        .is_err()
                );
            });
            let report = navigate(&c, goal(), &path, Options::default())
                .await
                .unwrap();
            assert_eq!(report.status, expected);
            assert_eq!(report.position, fresh_pose);
            assert_eq!(report.actions, 1);
            assert!(report.pending_action.is_none());
            server.await.unwrap();
            let j = Journal::open(&path, goal(), "instance", "actor", [5.5, 1., 0.5]).unwrap();
            assert_eq!(j.state.history[0].outcome, Some(Outcome::Cancelled));
            assert!(
                j.state.blocked.is_empty(),
                "Cancellation must not retire a feasible route"
            );
        }
    }
    #[test]
    fn block_reach_uses_caller_parameters_and_nearest_face() {
        let g = Goal::BlockReach {
            dimension: "fixture:world".into(),
            block: [0, 6, 0],
            distance: 4.4,
            eye_height: 1.62,
        };
        assert_eq!(g.gap([0.5, 0., 0.5]), 0.);
        assert!(g.gap([4., 0., 0.5]) > 0.);
        assert!(
            Goal::Position {
                dimension: "fixture:world".into(),
                target: [f64::NAN, 0., 0.],
                distance: 1.
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn interrupted_intention_survives_reopen_and_concurrent_owner_is_refused() {
        let temp = Temp::new();
        let path = temp.0.join("nav.json");
        let saved;
        {
            let mut j = Journal::open(&path, goal(), "instance", "actor", [0.5, 1., 0.5]).unwrap();
            assert!(Journal::open(&path, goal(), "instance", "actor", [0.5, 1., 0.5]).is_err());
            saved = ActionRequest {
                action_id: format!("{}-1", j.state.prefix),
                action: Action::StartMove {
                    target: Position {
                        x: 1.5,
                        y: 1.,
                        z: 0.5,
                    },
                    jump: false,
                    max_ticks: 60,
                },
            };
            j.state.pending = Some(Attempt {
                request: saved.clone(),
                origin: [0.5, 1., 0.5],
                scene: "known".into(),
                target: [1.5, 1., 0.5],
                outcome: None,
                source_node: None,
                arrival_verification: None,
            });
            j.save().unwrap();
        }
        let j = Journal::open(&path, goal(), "instance", "actor", [0.5, 1., 0.5]).unwrap();
        assert_eq!(
            serde_json::to_value(&j.state.pending.as_ref().unwrap().request).unwrap(),
            serde_json::to_value(saved).unwrap()
        );
        drop(j);
        assert!(
            Journal::open(&path, goal(), "different-instance", "actor", [0.5, 1., 0.5]).is_err()
        );
        assert!(
            Journal::open(
                &path,
                goal(),
                "instance",
                "another-character",
                [0.5, 1., 0.5]
            )
            .is_err()
        );
    }
    #[test]
    fn graph_can_detour_but_never_invents_reverse_edges_after_a_drop() {
        let temp = Temp::new();
        let mut j = Journal::open(
            &temp.0.join("nav.json"),
            goal(),
            "instance",
            "actor",
            [0.5, 1., 0.5],
        )
        .unwrap();
        let a = [0.5, 1., 0.5];
        let b = [0.5, 1., 1.5];
        let c = [2.5, 1., 1.5];
        j.state.nodes.insert(
            key(a),
            Node {
                position: a,
                expanded: true,
                edges: vec![Edge {
                    route: route(b),
                    to: key(b),
                }],
            },
        );
        j.state.nodes.insert(
            key(b),
            Node {
                position: b,
                expanded: true,
                edges: vec![Edge {
                    route: route(c),
                    to: key(c),
                }],
            },
        );
        j.state.nodes.insert(
            key(c),
            Node {
                position: c,
                expanded: false,
                edges: Vec::new(),
            },
        );
        assert_eq!(choose(&j.state, &key(a)).unwrap().target, b);
        assert!(choose(&j.state, &key(c)).is_none());
    }
}
