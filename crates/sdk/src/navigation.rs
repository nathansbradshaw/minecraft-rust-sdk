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
    fn validate(&self) -> Result<()> {
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
}
impl Default for Options {
    fn default() -> Self {
        Self {
            max_steps: 128,
            max_nodes: 1024,
            timeout_ms: 120000,
            blocked_blocks: Vec::new(),
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
    pub pending_action: Option<String>,
    pub journal: PathBuf,
    pub note: String,
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
    prefix: String,
    sequence: u64,
    #[serde(default)]
    preview_waits: u64,
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
    fn open(path: &Path, goal: Goal, instance: &str, actor: &str, p: [f64; 3]) -> Result<Self> {
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
        let state = if path.exists() {
            let m = fs::symlink_metadata(path).map_err(local)?;
            if !m.is_file() || m.file_type().is_symlink() || m.len() > 16 * 1024 * 1024 {
                return Err(local("Expected a bounded regular navigation state file"));
            }
            let s: State =
                serde_json::from_slice(&fs::read(path).map_err(local)?).map_err(local)?;
            if s.version != 1
                || s.instance != instance
                || s.actor != actor
                || s.goal != goal
                || !valid_id(&s.prefix)
            {
                return Err(error(
                    ErrorCode::Conflict,
                    "Navigation journal belongs to another goal, character, instance or version; preserve it and use a new path",
                ));
            }
            s
        } else {
            State {
                version: 1,
                instance: instance.into(),
                actor: actor.into(),
                goal,
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
                nodes: BTreeMap::new(),
                blocked: BTreeSet::new(),
                pending: None,
                history: Vec::new(),
                position: p,
                expansion: None,
            }
        };
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
            pending_action: self
                .state
                .pending
                .as_ref()
                .map(|p| p.request.action_id.clone()),
            journal: self.path.clone(),
            note: note.into(),
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
        && distance(position, last.target) <= 0.35
    {
        return key(last.target);
    }
    key(position)
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
    let observation = client.observe().await?;
    let pose = observed_pose(&observation)?;
    let actor = observation.player["uuid"]
        .as_str()
        .ok_or_else(|| error(ErrorCode::NotReady, "Player identity unavailable"))?;
    let mut journal = Journal::open(path, goal, expected, actor, pose.position)?;
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
            if receipt.outcome != Outcome::Observed || arrival.is_some_and(|(_, reached)| !reached)
            {
                j.state.blocked.insert(blocked_key(
                    &attempt
                        .source_node
                        .clone()
                        .unwrap_or_else(|| key(attempt.origin)),
                    &key(attempt.target),
                    &attempt.scene,
                ));
            }
            j.state.history.push(attempt);
            j.state.expansion = None;
            if let Some((actual, false)) = arrival {
                j.state.position = actual;
                j.save()?;
                return Ok(j.report("target_not_reached","The server ended this intention at a different verified pose; native outcome and target mismatch are retained. Reobserve and plan a new intention; this ID was not replayed."));
            }
            j.save()?;
        }
        let observation = client.observe().await?;
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
        if j.state.goal.gap(pose.position) <= 0.001 {
            j.save()?;
            return Ok(j.report("reached","Observed player pose satisfies the requested distance; gameplay effects need their own verification"));
        }
        if j.state.history.len() >= options.max_steps || j.state.nodes.len() > options.max_nodes {
            j.save()?;
            return Ok(j.report("budget_exhausted","Navigation exploration budget reached; inspect saved routes and outcomes before extending it"));
        }
        let mut offset = 0;
        let mut origin: Option<Origin> = None;
        let mut scene = String::new();
        let mut routes = Vec::new();
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
            let now = page.origin.expect("Validated ready movement preview");
            if now.dimension != pose.dimension || distance(now.position, pose.position) > 0.15 {
                origin = None;
                j.state.expansion = None;
                j.save()?;
                break;
            }
            if let Some(mut saved) = resume.take()
                && saved.scene == page.scene
                && saved.origin.position == now.position
            {
                offset = saved.next_offset;
                scene = saved.scene.clone();
                origin = Some(saved.origin.clone());
                routes = saved.routes.clone();
                saved.ready_after_unix_ms =
                    unix_ms()?.saturating_add(page.retry_after_ms.clamp(25, 5000));
                j.state.expansion = Some(saved);
                j.save()?;
                tokio::time::sleep(Duration::from_millis(page.retry_after_ms.clamp(25, 5000)))
                    .await;
                continue;
            }
            if let Some(old) = &origin {
                if distance(old.position, now.position) > 0.01 || scene != page.scene {
                    origin = None;
                    j.state.expansion = None;
                    j.save()?;
                    break;
                }
            } else {
                scene = page.scene.clone();
                origin = Some(now);
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
                j.state.expansion = None;
                j.save()?;
                break;
            }
            let Some(next) = page.next_offset.filter(|n| *n > offset && *n <= 1024) else {
                return Err(error(
                    ErrorCode::NotReady,
                    "Movement option pagination did not advance",
                ));
            };
            offset = next;
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
                expanded: true,
                edges,
            },
        );
        let Some(route) = choose(&j.state, &current) else {
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
        for case in ["scene", "life", "clock", "velocity", "policy"] {
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
    #[tokio::test]
    async fn a_defensively_cancelled_intention_is_resolved_before_reporting_the_fresh_pose() {
        let temp = Temp::new();
        let path = temp.0.join("nav.json");
        {
            let mut j = Journal::open(&path, goal(), "instance", "actor", [0.5, 1., 0.5]).unwrap();
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
                observation([5.5, 1., 0.5]),
                200,
            )
            .await;
            respond(&listener,"GET /v1/bots/fixture/actions/cancelled ",serde_json::json!({"action_id":"cancelled","outcome":"cancelled","adapter_version":"fixture","evidence":{"input_released":true},"error":null}),200).await;
            respond(
                &listener,
                "GET /v1/bots/fixture/observations ",
                observation([5.5, 1., 0.5]),
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
        assert_eq!(report.status, "reached");
        assert_eq!(report.actions, 1);
        assert!(report.pending_action.is_none());
        server.await.unwrap();
        let j = Journal::open(&path, goal(), "instance", "actor", [5.5, 1., 0.5]).unwrap();
        assert_eq!(j.state.history[0].outcome, Some(Outcome::Cancelled));
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
