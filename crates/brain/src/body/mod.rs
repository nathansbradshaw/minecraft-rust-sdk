//! Replaceable container-local safety behavior, independent of model/terminal work.
mod aerial;
mod cover_handoff;
pub mod decision;
pub mod geometry;
mod hearing;
mod motion;
mod perception;
mod program;
mod recovery;
mod tactics;
#[cfg(test)]
mod tests;
use crate::{Journal, Result, ensure};
use minecraft_sdk::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
fn yes() -> bool {
    true
}
fn period() -> u64 {
    150
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "period")]
    pub period_ms: u64,
    /// Opt-in diagnostic cap per body startup. Zero avoids production logging.
    #[serde(default)]
    pub decision_samples: u16,
    /// Unproven ranged corner maneuver. Native trials have recorded deaths.
    #[serde(default)]
    pub experimental_ranged_boundary_turns: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            period_ms: period(),
            decision_samples: 0,
            experimental_ranged_boundary_turns: false,
        }
    }
}
pub struct Shared {
    pub mutation: Arc<tokio::sync::Mutex<()>>,
    pub model_lanes: crate::model::Lanes,
    pub feedback: Mutex<crate::feedback::Trace>,
    // Bumped under mutation ownership before submitting an SDK program's
    // action. A food choice cannot act on a hand snapshot older than that.
    program_generation: AtomicU64,
    program_pending: Mutex<std::collections::BTreeSet<String>>,
    scheduling_revision: AtomicU64,
    emergency: AtomicBool,
    running: AtomicBool,
    latest: Mutex<Option<(Instant, Arc<Observation>)>>,
    reason: Mutex<String>,
    pub failed: tokio::sync::Notify,
}
#[derive(Debug)]
struct StaleObservation;
impl std::fmt::Display for StaleObservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Body observation is stale")
    }
}
impl std::error::Error for StaleObservation {}
#[derive(Clone, Copy)]
struct ObservationAdmission {
    generation: u64,
    mutation_free: bool,
}
#[derive(Clone, PartialEq, Eq)]
struct NativeBasis {
    actor: String,
    life: String,
    dimension: String,
    epoch: String,
    nanos: u64,
}
impl NativeBasis {
    fn read(o: &Observation) -> Option<Self> {
        fn text(value: &Value) -> Option<String> {
            value.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
        }
        geometry::point(&o.player["position"])?;
        o.player["entity_id"].as_i64()?;
        Some(Self {
            actor: text(&o.player["uuid"])?,
            life: text(&o.player["life_id"])?,
            dimension: text(&o.player["dimension"])?,
            epoch: text(&o.world["sample_clock"]["epoch"])?,
            nanos: o.world["sample_clock"]["nanos"].as_str()?.parse().ok()?,
        })
    }
    fn permits_cached(&self, other: &Self) -> bool {
        self.actor == other.actor
            && self.life == other.life
            && self.dimension == other.dimension
            && self.epoch == other.epoch
            && other.nanos >= self.nanos
    }
}
// Only a typed native query can discharge a foreground-mutation fence. An
// ordinary Observe response may still contain a pre-mutation cached snapshot.
struct NativeAdmission {
    generation: u64,
    required: bool,
    floor: Option<NativeBasis>,
    actor: Option<String>,
    attempted: Option<Instant>,
}
impl NativeAdmission {
    fn begin(&mut self, admission: ObservationAdmission) {
        self.required |= !admission.mutation_free || admission.generation != self.generation;
    }
    fn accepts_cached(&self, o: &Observation) -> bool {
        self.floor.as_ref().is_none_or(|floor| {
            NativeBasis::read(o).is_some_and(|other| floor.permits_cached(&other))
        })
    }
    fn accept_fresh(&mut self, o: &Observation, admission: ObservationAdmission) -> bool {
        let Some(basis) = NativeBasis::read(o) else {
            return false;
        };
        if self
            .actor
            .as_ref()
            .is_some_and(|actor| actor != &basis.actor)
            || self.floor.as_ref().is_some_and(|floor| {
                floor.life == basis.life && floor.epoch == basis.epoch && basis.nanos < floor.nanos
            })
        {
            return false;
        }
        self.actor = Some(basis.actor.clone());
        self.floor = Some(basis);
        self.generation = admission.generation;
        self.required = false;
        true
    }
}
struct InteractionRead {
    id: String,
    task: tokio::task::JoinHandle<Option<Receipt>>,
}
impl Drop for InteractionRead {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Shared {
    fn observation_admission(&self) -> ObservationAdmission {
        ObservationAdmission {
            generation: self.program_generation.load(Ordering::Acquire),
            mutation_free: self.mutation.try_lock().is_ok(),
        }
    }
    fn try_observed_mutation(
        &self,
        basis: ObservationAdmission,
        observed_at: Instant,
    ) -> Option<tokio::sync::MutexGuard<'_, ()>> {
        let guard = self.mutation.try_lock().ok()?;
        // A read begun behind an in-flight dispatch cannot become admissible
        // merely because that dispatch completed while its response arrived.
        // Try a new observation cycle instead; never retire/replay its ID here.
        (basis.mutation_free
            && basis.generation == self.program_generation.load(Ordering::Acquire)
            && observed_at.elapsed() < Duration::from_millis(250))
        .then_some(guard)
    }
    fn set_urgent(&self, urgent: bool) -> bool {
        let previous = self.emergency.swap(urgent, Ordering::AcqRel);
        if previous != urgent {
            self.scheduling_revision.fetch_add(1, Ordering::Release);
        }
        previous
    }
    pub fn urgent(&self) -> bool {
        self.emergency.load(Ordering::Acquire)
    }
    pub fn view(&self) -> Value {
        json!({"emergency":self.urgent(),"scheduling_revision":self.scheduling_revision.load(Ordering::Acquire),"running":self.running.load(Ordering::Acquire),"reason":self.reason.lock().ok().map(|s|s.clone()),"observation_age_ms":self.latest.lock().ok().and_then(|v|v.as_ref().map(|(at,_)|at.elapsed().as_millis()))})
    }
    pub fn observe(&self) -> Result<Observation> {
        ensure(
            self.running.load(Ordering::Acquire),
            "Background Survival controller stopped",
        )?;
        let latest = self
            .latest
            .lock()
            .map_err(|_| "Body observation unavailable")?;
        let (at, o) = latest.as_ref().ok_or("Body observation has not arrived")?;
        if at.elapsed() >= Duration::from_secs(3) {
            return Err(Box::new(StaleObservation));
        }
        Ok((**o).clone())
    }
    /// Readers can await the body's existing transport retry. This never makes
    /// stale data admissible for gameplay and does not consume the supervisor's
    /// failure notification. The body retains its own loss/disconnect policy.
    pub async fn wait_fresh(&self) -> Result<Observation> {
        self.wait_fresh_for(Duration::from_millis(2500)).await
    }
    async fn wait_fresh_for(&self, budget: Duration) -> Result<Observation> {
        let deadline = Instant::now() + budget;
        loop {
            match self.observe() {
                Ok(observation) => return Ok(observation),
                Err(error) if error.is::<StaleObservation>() && Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(error) => return Err(error),
            }
        }
    }
}
pub struct Handle {
    pub shared: Arc<Shared>,
    pub socket: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}
impl Handle {
    pub fn start(
        profile: Profile,
        cfg: Config,
        state: &Path,
        initial: Observation,
    ) -> Result<Self> {
        Self::start_with_waypoint_range(profile, cfg, state, initial, 3.5)
    }
    /// Start with the server-advertised local waypoint range. Remote admission
    /// remains authoritative; this only bounds client-side candidate selection.
    pub fn start_with_waypoint_range(
        profile: Profile,
        cfg: Config,
        state: &Path,
        initial: Observation,
        finite_waypoint_axis: f64,
    ) -> Result<Self> {
        Self::start_with_controls(profile, cfg, state, initial, finite_waypoint_axis, false)
    }
    /// Use capabilities already discovered by the owning controller; no extra
    /// network request delays the first defensive reaction.
    pub fn start_for_server(
        profile: Profile,
        cfg: Config,
        state: &Path,
        initial: Observation,
        server: &ServerInfo,
    ) -> Result<Self> {
        ensure(
            profile.instance_id == server.instance_id,
            "Body server instance mismatch",
        )?;
        for capability in [
            "minecraft:finite_locomotion",
            "minecraft:survival_facts",
            "minecraft:ranged_observation",
            "minecraft:pending_actions",
            "minecraft:session_material_preservation",
            auditory::CAPABILITY,
            menu::FRESH_OBSERVATION_CAPABILITY,
        ] {
            ensure(
                server.capabilities.iter().any(|c| c == capability),
                &format!("Default Survival brain requires {capability}"),
            )?;
        }
        auditory::frame(&initial)?;
        let range = server.limits["finite_move_waypoint_blocks"]
            .as_f64()
            .filter(|v| (0.9..=4.2).contains(v))
            .unwrap_or(3.5);
        let through = server
            .capabilities
            .iter()
            .any(|c| c == "minecraft:waypoint_passthrough");
        Self::start_with_controls(profile, cfg, state, initial, range, through)
    }
    fn start_with_controls(
        profile: Profile,
        cfg: Config,
        state: &Path,
        initial: Observation,
        finite_waypoint_axis: f64,
        through: bool,
    ) -> Result<Self> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
        ensure((50..=1000).contains(&cfg.period_ms), "Invalid body cadence")?;
        ensure(
            cfg.decision_samples <= 128,
            "At most128 diagnostic body decisions permitted",
        )?;
        ensure(
            (0.9..=4.2).contains(&finite_waypoint_axis),
            "Invalid body waypoint range",
        )?;
        let socket = state.join("actions.sock");
        if socket.exists() {
            let m = fs::symlink_metadata(&socket)?;
            ensure(
                m.file_type().is_socket()
                    && m.mode() & 0o077 == 0
                    && m.uid() == unsafe { libc::geteuid() },
                "Unsafe old action socket",
            )?;
            match std::os::unix::net::UnixStream::connect(&socket) {
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                    fs::remove_file(&socket)?
                }
                _ => return Err("An action arbiter already owns this socket".into()),
            }
        }
        let listener = std::os::unix::net::UnixListener::bind(&socket)?;
        listener.set_nonblocking(true)?;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
        let shared = Arc::new(Shared {
            feedback: Mutex::new(crate::feedback::Trace::default()),
            program_generation: AtomicU64::new(0),
            program_pending: Mutex::new(Default::default()),
            scheduling_revision: AtomicU64::new(0),
            mutation: Arc::new(tokio::sync::Mutex::new(())),
            model_lanes: crate::model::Lanes::default(),
            emergency: AtomicBool::new(false),
            running: AtomicBool::new(true),
            latest: Mutex::new(Some((Instant::now(), Arc::new(initial)))),
            reason: Mutex::new("observing".into()),
            failed: tokio::sync::Notify::new(),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let active = shared.clone();
        let stopped = stop.clone();
        let journal = state.join("body.sqlite");
        let sock = socket.clone();
        let thread = std::thread::Builder::new()
            .name("survival-body".into())
            .spawn(move || {
                struct End(Arc<Shared>, PathBuf);
                impl Drop for End {
                    fn drop(&mut self) {
                        self.0.running.store(false, Ordering::Release);
                        self.0.failed.notify_one();
                        let _ = fs::remove_file(&self.1);
                    }
                }
                let _end = End(active.clone(), sock);
                let run = || -> Result<()> {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    rt.block_on(async {
                        let client =
                            Arc::new(Client::with_profile(&profile)?.without_local_arbiter());
                        let journal = Journal::open(&journal)?;
                        let listener = tokio::net::UnixListener::from_std(listener)?;
                        tokio::select! {
                            r=controller(client.clone(),cfg,active.clone(),stopped,&journal,finite_waypoint_axis,through)=>r,
                            r=serve(listener,client,active.clone())=>r,
                        }
                    })
                };
                if let Err(e) = run() {
                    if let Ok(mut s) = active.reason.lock() {
                        *s = format!("Body failed: {e}");
                    }
                    eprintln!("{}", json!({"event":"body_failed","error":e.to_string()}));
                }
            })?;
        Ok(Self {
            shared,
            socket,
            stop,
            thread: Some(thread),
        })
    }
    pub async fn stop(mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            tokio::task::spawn_blocking(move || thread.join().map_err(|_| "Body thread panicked"))
                .await??;
        }
        Ok(())
    }
}
fn passive(action: &Action) -> bool {
    matches!(action, Action::Chat { .. } | Action::Look { .. })
}
pub fn supported(action: &Action) -> bool {
    !matches!(
        action,
        Action::Move { .. } | Action::MotionFrame { .. } | Action::Jump
    )
}
async fn serve(
    listener: tokio::net::UnixListener,
    client: Arc<Client>,
    shared: Arc<Shared>,
) -> Result<()> {
    let budget = Arc::new(tokio::sync::Semaphore::new(16));
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            accepted=listener.accept()=>{
                let (socket,_)=accepted?;
                if socket.peer_cred()?.uid()!=unsafe{libc::geteuid()} {continue;}
                let Ok(permit)=budget.clone().try_acquire_owned() else {continue;};
                let client=client.clone();let shared=shared.clone();
                tasks.spawn(async move {let _permit=permit;let _=proxy(socket,client,shared).await;});
            },
            _=tasks.join_next(),if !tasks.is_empty()=>{},
        }
    }
}
async fn proxy(
    socket: tokio::net::UnixStream,
    client: Arc<Client>,
    shared: Arc<Shared>,
) -> Result<()> {
    let (input, mut output) = socket.into_split();
    let mut input = tokio::io::BufReader::new(input);
    let mut line = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let b = input.read_u8().await?;
            line.push(b);
            if line.len() > 16384 {
                return Err(std::io::Error::other("Local action too large"));
            }
            if b == b'\n' {
                break;
            }
        }
        Ok::<_, std::io::Error>(())
    })
    .await??;
    let request: LocalActionRequest = serde_json::from_slice(&line)?;
    let mut error = None;
    if request.version != LOCAL_CONTROL_VERSION {
        error = Some(ApiError::new(
            ErrorCode::UnsupportedVersion,
            "Local control version mismatch",
        ));
    }
    if !supported(&request.request.action) {
        error = Some(ApiError::new(
            ErrorCode::UnsupportedCapability,
            "Use start_move finite controls inside a protected coding workspace",
        ));
    }
    let response = if let Some(e) = error {
        LocalActionResponse::Error(e)
    } else {
        request.request.action.validate()?;
        let acquire = async {
            loop {
                let guard = shared.mutation.clone().lock_owned().await;
                if !shared.urgent() || passive(&request.request.action) {
                    return guard;
                }
                drop(guard);
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        };
        // A queued caller that disappears must never be dispatched afterwards.
        let admission = tokio::select! {biased; _=input.read_u8()=>return Ok(()),g=tokio::time::timeout(Duration::from_secs(3), acquire)=>g};
        if let Ok(guard) = admission {
            // Readiness registration can lag an already-closed peer when the lock
            // becomes ready in the same poll. Check the kernel before dispatch.
            use std::os::fd::AsRawFd;
            let mut byte = 0u8;
            let peek = unsafe {
                libc::recv(
                    input.get_ref().as_ref().as_raw_fd(),
                    (&mut byte as *mut u8).cast(),
                    1,
                    libc::MSG_PEEK | libc::MSG_DONTWAIT,
                )
            };
            if !input.buffer().is_empty() || peek >= 0 {
                return Ok(());
            }
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::WouldBlock {
                return Err("Local action caller disconnected".into());
            }
            shared.program_dispatch(&request.request.action_id)?;
            let mut result = client.act(&request.request).await;
            if let Ok(receipt) = &result {
                shared.resolve_program_receipt(&request.request.action_id, receipt)?;
            }
            drop(guard);
            if let Ok(receipt) = &result
                && receipt.outcome == Outcome::Running
                && !matches!(
                    request.request.action,
                    Action::StartMove { .. }
                        | Action::StartMoveThrough { .. }
                        | Action::CancelControl { .. }
                        | Action::CancelUseItem { .. }
                )
            {
                // Keep body preemption and conversation live while the game's
                // interaction develops. Dispatch only once; waiting reads the
                // same durable ID and preserves cancellation/uncertainty.
                result = client
                    .wait_for_action(&request.request.action_id, Duration::from_secs(10))
                    .await;
            }
            if let Ok(receipt) = &result {
                shared.resolve_program_receipt(&request.request.action_id, receipt)?;
            }
            match result {
                Ok(r) => LocalActionResponse::Receipt(r),
                Err(e) => LocalActionResponse::Error(e),
            }
        } else {
            LocalActionResponse::Error(ApiError::new(
                ErrorCode::LocalPreempted,
                "Body admission deadline exceeded before gateway dispatch; no mutation was submitted. Inspect current observation and body reason before another intention.",
            ))
        }
    };
    if let Ok(mut trace) = shared.feedback.lock() {
        trace.record(&request.request, &response);
    }
    let mut bytes = serde_json::to_vec(&response)?;
    ensure(bytes.len() <= 1_048_576, "Local response too large")?;
    bytes.push(b'\n');
    output.write_all(&bytes).await?;
    Ok(())
}

async fn send(client: &Client, j: &Journal, action: Action) -> Result<Receipt> {
    let sequence = j.event("body_action_intent", json!({"action":action}))?;
    let request = ActionRequest {
        action_id: format!(
            "body-{}-{sequence}",
            j.get("epoch")?.ok_or("Body epoch missing")?
        ),
        action,
    };
    j.set("pending_action", &request.action_id)?;
    let r = client.act(&request).await?;
    j.event("body_action_receipt",json!({"action_id":r.action_id,"outcome":r.outcome,"error":r.error,"native_effect_verified":r.evidence["native_effect_verified"],"evidence_sha256":crate::digest(r.evidence.to_string().as_bytes())}))?;
    if r.outcome.terminal() || r.outcome == Outcome::Running {
        j.set("pending_action", "")?;
    }
    Ok(r)
}
fn may_resume_after_danger(
    observed: &Observation,
    safe_for: Duration,
    owned_control: &str,
    owned_water_control: bool,
    owned_water_shore: bool,
) -> bool {
    let active = &observed.player["control"];
    // Explicit reconciliation publishes present idle readiness separately from
    // the action's unchanged uncertain historical receipt. It is not a native
    // terminal outcome or evidence that the original movement succeeded.
    let reconciled_idle = active["action_id"].as_str() == Some(owned_control)
        && active["outcome"] == "idle"
        && active["historical_outcome"] == "uncertain"
        && active["reconciliation"]["settled"] == true
        && active["reconciliation"]["historical_effect_verified"] == false
        && active["reconciliation"]["replayed"] == false;
    let released = active["input_released"] == true
        && (reconciled_idle
            || serde_json::from_value::<Outcome>(active["outcome"].clone())
                .is_ok_and(|outcome| outcome.terminal()));
    // Wall time cannot retire native input: a lagging server may still be
    // finishing the bank leg after three seconds. Only its own terminal,
    // released observation completes that handoff. A replacement control or
    // new life must not inherit an old control's ownership latch.
    let awaiting_shore_release =
        owned_water_shore && active["action_id"].as_str() == Some(owned_control) && !released;
    !awaiting_shore_release
        && (safe_for > Duration::from_secs(3)
            || (owned_water_control && geometry::breathing_recovered(observed) && released))
}
/// Invalidate an old escape that now approaches a visually observed fuse.
/// A priming sound may have happened before this character could hear it.
/// This only requests release; it neither invents hidden threats nor proves
/// that a replacement route can outrun the blast.
fn toward_visible_blast(observed: &Observation) -> bool {
    let Some(start) = geometry::point(&observed.player["position"]) else {
        return false;
    };
    let Some(target) = geometry::point(&observed.player["control"]["target"]) else {
        return false;
    };
    observed.world["entities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entity| entity["visible"] == true)
        .any(|entity| {
            let Some((origin, radius)) =
                geometry::point(&entity["position"]).zip(geometry::primed_blast(entity))
            else {
                return false;
            };
            let distance_squared = |point: [f64; 3]| {
                (0..3)
                    .map(|axis| (point[axis] - origin[axis]).powi(2))
                    .sum::<f64>()
            };
            let end_distance = distance_squared(target);
            end_distance <= radius * radius && end_distance < distance_squared(start)
        })
}
async fn controller(
    client: Arc<Client>,
    cfg: Config,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    j: &Journal,
    finite_waypoint_axis: f64,
    through_supported: bool,
) -> Result<()> {
    let mut decision_samples = cfg.decision_samples;
    let mut entropy = [0u8; 16];
    std::io::Read::read_exact(&mut fs::File::open("/dev/urandom")?, &mut entropy)?;
    let epoch = crate::digest(&entropy);
    j.set("epoch", &epoch[..24])?;
    j.event(
        "body_started",
        json!({"period_ms":cfg.period_ms,"execution":"independent Rust thread","model_calls":0}),
    )?;
    let mut last_error = String::new();
    let mut last_attack = Instant::now() - Duration::from_secs(2);
    let mut safe_since = Instant::now();
    let mut owned_control = String::new();
    let mut owned_water_control = false;
    let mut owned_water_shore = false;
    let mut allowed_surface_control = String::new();
    let mut breathing_release_requested = String::new();
    let mut owned_projectiles = Vec::new();
    let mut cycles = 0u64;
    let mut rejected_routes = Vec::new();
    let mut conditions = String::new();
    let mut previous_health = Value::Null;
    let mut body_sprint = false;
    let mut inspected_interaction = String::new();
    let mut interaction_read: Option<InteractionRead> = None;
    let mut interaction_attempt = Instant::now() - Duration::from_secs(1);
    let mut native_admission = NativeAdmission {
        generation: shared.program_generation.load(Ordering::Acquire),
        required: false,
        floor: None,
        actor: shared.latest.lock().ok().and_then(|cache| {
            cache
                .as_ref()
                .and_then(|(_, o)| o.player["uuid"].as_str().map(str::to_owned))
        }),
        attempted: None,
    };
    let mut cancelled_item_use = String::new();
    let mut progress = tactics::Progress::default();
    let mut projectiles = tactics::Projectiles::default();
    let mut retreat_trail = tactics::RetreatTrail::default();
    let mut threat_memory = perception::RangedMemory::default();
    let mut hearing_memory: hearing::Memory = j
        .get("hearing_memory")?
        .map(|value| serde_json::from_str(&value))
        .transpose()?
        .unwrap_or_default();
    // The public startup path requires this capability and validates the first
    // frame. Low-level mechanics fixtures can still construct a visual-only body.
    let hearing_enabled = shared
        .latest
        .lock()
        .map_err(|_| "Body cache unavailable")?
        .as_ref()
        .is_some_and(|(_, o)| o.world.get("auditory").is_some());
    let mut own_motion = motion::Motion::default();
    let mut motion_predictions = 0u64;
    let mut motion_trace_samples = 0u16;
    let mut last_motion_assessment = (Vec::new(), Vec::new());
    let mut retreat_progress = recovery::Progress::default();
    let mut native_escape = recovery::Discovery::default();
    let mut native_required = recovery::Required::default();
    let mut cover_handoff = cover_handoff::Handoff::default();
    let mut last_meal = None;
    let mut owned_meal: Option<(String, tactics::Meal)> = None;
    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        let started = Instant::now();
        let mut admission = shared.observation_admission();
        native_admission.begin(admission);
        let observed_generation = admission.generation;
        let mut observed =
            match tokio::time::timeout(Duration::from_secs(2), client.observe()).await {
                Ok(Ok(o)) => o,
                result => {
                    let error = match result {
                        Ok(Err(e)) => e.to_string(),
                        _ => "Observation deadline exceeded".into(),
                    };
                    if error != last_error {
                        j.event("body_observation_error", json!({"error":error}))?;
                        last_error = error;
                    }
                    if shared
                        .latest
                        .lock()
                        .ok()
                        .and_then(|v| v.as_ref().map(|(at, _)| at.elapsed()))
                        .is_some_and(|age| age > Duration::from_secs(3))
                    {
                        return Err("Body lost fresh SDK observations".into());
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
            };
        last_error.clear();
        let mut observed_at = Instant::now();
        if native_admission.actor.is_none() {
            native_admission.actor = observed.player["uuid"].as_str().map(str::to_owned);
        }
        if !native_admission.accepts_cached(&observed) {
            native_admission.required = true;
        }
        if native_admission.required
            && admission.mutation_free
            && native_admission
                .attempted
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(1))
        {
            native_admission.attempted = Some(Instant::now());
            let fresh =
                tokio::time::timeout(Duration::from_millis(650), menu::observe_fresh(&client))
                    .await;
            let current = shared.observation_admission();
            if let Ok(Ok(fresh)) = fresh
                && current.mutation_free
                && current.generation == admission.generation
                && native_admission.accept_fresh(&fresh, admission)
            {
                observed = fresh;
                observed_at = Instant::now();
            } else {
                j.event("body_native_admission_deferred", json!({"reason":"Fresh native basis unavailable after foreground mutation; cached state cannot authorize control","pending_ids_retained":true}))?;
            }
        }
        // Keep cached reads available during contention, but never act on a
        // cached pre-mutation frame or publish a frame behind a native floor.
        admission.mutation_free &= !native_admission.required;
        if !native_admission.accepts_cached(&observed) {
            if shared
                .latest
                .lock()
                .ok()
                .and_then(|cache| cache.as_ref().map(|(at, _)| at.elapsed()))
                .is_some_and(|age| age >= Duration::from_secs(3))
            {
                return Err("Body lost nonregressive native observations".into());
            }
            next_cycle(started, cfg.period_ms).await;
            continue;
        }
        let observation_ms = started.elapsed().as_millis();
        cycles += 1;
        if hearing_enabled {
            let report = hearing_memory.update(&mut observed)?;
            let memory = serde_json::to_string(&hearing_memory)?;
            if j.get("hearing_memory")?.as_deref() != Some(memory.as_str()) {
                j.set("hearing_memory", &memory)?;
            }
            if report["fresh"]
                .as_array()
                .is_some_and(|sounds| !sounds.is_empty())
            {
                j.event("body_heard", report)?;
            }
        }
        *shared.latest.lock().map_err(|_| "Body cache unavailable")? =
            Some((Instant::now(), Arc::new(observed.clone())));
        let p = geometry::point(&observed.player["position"]).ok_or("Body position missing")?;
        let nearby = observed.world["entities"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|e| e["visible"] == true && e["hostile"] == true)
            .filter_map(|e| {
                geometry::point(&e["position"])
                    .map(|q| (e, (0..3).map(|i| (p[i] - q[i]).powi(2)).sum::<f64>().sqrt()))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1));
        let changed=crate::digest(json!({"position":geometry::target_key(p),"blocks":observed.world["blocks"].as_array().map(|rows|rows.iter().map(|b|(&b["position"],&b["block"])).collect::<Vec<_>>())}).to_string().as_bytes());
        if changed != conditions {
            rejected_routes.clear();
            conditions = changed;
        }
        let active = &observed.player["control"];
        let mut policy_observed = threat_memory.policy(&observed);
        let trace_motion = decision_samples > 0
            && motion_trace_samples < 8
            && active["action_id"].as_str() == Some(owned_control.as_str())
            && active["outcome"] == "running";
        let previous_motion = trace_motion.then(|| own_motion.previous_sample());
        let prediction = own_motion.sample(&observed, &owned_control);
        if active["outcome"] != "running"
            && let Some(ground) = own_motion.ground_motion()
        {
            policy_observed.to_mut().player["client_ground_motion"] = ground.clone();
        }
        if trace_motion {
            motion_trace_samples += 1;
            j.event("body_motion_sample",json!({"owned_control":owned_control,"previous":previous_motion,
                "player":{"uuid":observed.player["uuid"],"dimension":observed.player["dimension"],"entity_id":observed.player["entity_id"],
                    "position":p,"on_ground":observed.player["on_ground"],"health":observed.player["health"],
                    "in_water":observed.player["in_water"],"in_lava":observed.player["in_lava"],"control":active},
                "game_time":observed.world["game_time"],"sample_clock":observed.world["sample_clock"],"prediction":prediction,
                "scope":"Bounded actual eligibility inputs for client motion estimate; not a gameplay success claim"}))?;
        }
        if let Some(prediction) = prediction {
            motion_predictions += 1;
            policy_observed.to_mut().player["client_motion"] = prediction;
        }
        let stuck = progress.stuck(&observed, &owned_control);
        let stationary_projectiles = projectiles.stationary(&observed);
        let incoming_projectiles =
            geometry::incoming_with_stationary(&policy_observed, &stationary_projectiles);
        let stationary_incoming =
            geometry::incoming_with_stationary(&observed, &stationary_projectiles);
        let cleared: Vec<_> = stationary_incoming
            .iter()
            .filter(|id| !incoming_projectiles.contains(id))
            .cloned()
            .collect();
        let leading: Vec<_> = incoming_projectiles
            .iter()
            .filter(|id| !stationary_incoming.contains(id))
            .cloned()
            .collect();
        let assessment = (cleared, leading);
        if assessment != last_motion_assessment {
            if !assessment.0.is_empty() || !assessment.1.is_empty() {
                j.event("body_motion_projectile_assessment",json!({"action_id":active["action_id"],"cleared":assessment.0,"leading":assessment.1,
                    "prediction":policy_observed.player["client_motion"],"sample_clock":observed.world["sample_clock"],
                    "scope":"Client prediction preserves existing input; native health/movement determine the actual result"}))?;
            }
            last_motion_assessment = assessment;
        }
        let incoming = !incoming_projectiles.is_empty();
        let new_projectile = incoming_projectiles
            .iter()
            .any(|id| !owned_projectiles.contains(id));
        // Leaving and then re-entering the collision corridor is a new threat,
        // including a previously stationary projectile that resumes moving.
        owned_projectiles.retain(|id| incoming_projectiles.contains(id));
        let burning = observed.player["fire_ticks"]
            .as_i64()
            .is_some_and(|n| n > 0);
        let threatened = geometry::threatened(&policy_observed);
        let heard_blasts = hearing::blasts(&policy_observed);
        let heard_danger = !heard_blasts.is_empty();
        let trail = retreat_trail.update(&observed, threatened || incoming);
        let dead = observed.player["health"].as_f64().is_some_and(|h| h <= 0.);
        let surface_swim = geometry::bounded_surface_swim(&observed, &owned_control);
        let water_emergency = geometry::water_danger(&observed) && !surface_swim;
        if surface_swim && active["action_id"].as_str() != Some(&allowed_surface_control) {
            allowed_surface_control = active["action_id"].as_str().unwrap_or_default().into();
            j.event("body_surface_swim_continues",json!({"action_id":allowed_surface_control,
                "air":observed.player["air"],"target":active["target"],
                "scope":"Bounded admitted foreground input toward known breathing space; no new input or safety guarantee"}))?;
        }
        let stalled = retreat_progress.stalled(&observed, threatened);
        let urgent = dead
            || heard_danger
            || water_emergency
            || incoming
            || burning
            || nearby.is_some_and(|(_, distance)| distance < 4.5)
            || observed.world["entities"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|e| e["visible"] == true && geometry::primed_blast(e).is_some());
        let require_native = native_required.for_threat(&observed, threatened, water_emergency);
        let discover = (require_native
            || (stalled && !urgent && observed.player["on_ground"] == true))
            && threatened
            && active["outcome"] != "running";
        let admitted = native_escape
            .tick(
                client.clone(),
                &observed,
                discover,
                require_native,
                finite_waypoint_axis,
                j,
            )
            .await?;
        // Finish the old finite input before discovery. During the short
        // read-only query keep observing and defending; any new urgent danger
        // immediately restores the fast route without waiting for the query.
        let awaiting_native =
            (require_native || (discover && native_escape.waiting())) && admitted.is_empty();
        let continuation = cover_handoff.propose(
            &policy_observed,
            &owned_control,
            urgent || require_native || discover,
            &rejected_routes,
            finite_waypoint_axis,
        );
        if let Some((original, reason)) = cover_handoff.take_invalidation() {
            j.event("body_cover_handoff_invalidated", json!({"original_action_id":original,"reason":reason,"revision":observed.revision,"replayed":false}))?;
        }
        let continuing_cover = continuation.is_some();
        let escape = if active["outcome"] != "running" && !awaiting_native {
            continuation.or_else(|| {
                let defensive = geometry::evade_policy(
                    &policy_observed,
                    &rejected_routes,
                    &stationary_projectiles,
                    &trail,
                    finite_waypoint_axis,
                    &admitted,
                    cfg.experimental_ranged_boundary_turns,
                );
                defensive.or_else(|| {
                    owned_water_control
                        .then(|| geometry::shore_after_breath(&policy_observed, &rejected_routes))
                        .flatten()
                })
            })
        } else {
            None
        };
        if dead {
            last_meal = None;
        }
        if cycles.is_multiple_of(10) || observed.player["health"] != previous_health {
            j.event("body_native_sample",json!({"revision":observed.revision,"health":observed.player["health"],"food":observed.player["food"],"air":observed.player["air"],"max_air":observed.player["max_air"],"in_water":observed.player["in_water"],"eyes_in_water":observed.player["eyes_in_water"],"water_emergency":water_emergency,"in_lava":observed.player["in_lava"],"position":p,"control":active,"nearest_hostile":nearby.map(|(e,d)|json!({"entity_id":e["entity_id"],"type":e["type"],"position":e["position"],"distance":d})),"incoming_projectile":incoming,"incoming_projectile_ids":incoming_projectiles,"stationary_projectile_ids":stationary_projectiles,"stationary_scope":"client inference from advancing native positions, not a server flight flag or future guarantee"}))?;
            previous_health = observed.player["health"].clone();
        }
        let danger = dead
            || heard_danger
            || water_emergency
            || threatened
            || escape.is_some()
            || incoming
            || burning
            || observed.player["in_lava"] == true;
        let policy_ms = observed_at.elapsed().as_millis();
        if danger {
            safe_since = Instant::now();
            if !shared.set_urgent(true) {
                j.event("body_emergency",json!({"revision":observed.revision,"health":observed.player["health"],"position":p,"nearest_hostile_distance":nearby.map(|(_,d)|d)}))?;
            }
        }
        // Flush terminal interaction evidence to the gateway before a later
        // body interaction replaces the adapter's current job. Reads never
        // replay input and remain independent of slow model/tool execution.
        let interaction = &observed.player["interaction"];
        if interaction_read.is_none()
            && interaction_attempt.elapsed() >= Duration::from_secs(1)
            && let Some(id) = interaction["action_id"].as_str()
            && interaction["outcome"] != "running"
            && id != inspected_interaction
        {
            let id = id.to_owned();
            let (read_client, read_id) = (client.clone(), id.clone());
            interaction_attempt = Instant::now();
            interaction_read = Some(InteractionRead {
                id,
                task: tokio::spawn(async move {
                    tokio::time::timeout(
                        Duration::from_millis(400),
                        read_client.action_status(&read_id),
                    )
                    .await
                    .ok()?
                    .ok()
                }),
            });
        }
        let completed = if interaction_read
            .as_ref()
            .is_some_and(|read| read.task.is_finished())
        {
            let mut read = interaction_read.take().expect("finished status read");
            let receipt = (&mut read.task).await.ok().flatten();
            receipt.map(|receipt| (read.id.clone(), receipt))
        } else {
            None
        };
        if let Some((id, receipt)) = completed
            && receipt.action_id == id
            && (receipt.outcome.terminal() || receipt.outcome == Outcome::Uncertain)
        {
            inspected_interaction = id.clone();
            shared.resolve_program_receipt(&id, &receipt)?;
            j.event("body_interaction_status",json!({"action_id":id,"outcome":receipt.outcome,"native_effect_verified":receipt.evidence["native_effect_verified"],"input_released":receipt.evidence["input_released"],"evidence_sha256":crate::digest(receipt.evidence.to_string().as_bytes()),"replayed":false}))?;
            if owned_meal.as_ref().is_some_and(|(action, _)| action == &id)
                && let Some((_, meal)) = owned_meal.take()
            {
                j.event("body_meal_result", json!({
                    "action_id":id,"item":meal.item,"slot":meal.slot,
                    "outcome":receipt.outcome,"input_released":receipt.evidence["input_released"],
                    "native_effect":receipt.evidence["effect"]["kind"],
                    "food_before":receipt.evidence["before"]["player"]["food"],
                    "food_after":receipt.evidence["after"]["player"]["food"],
                    "saturation_after":receipt.evidence["after"]["player"]["saturation"],
                    "replayed":false
                }))?;
                if receipt.outcome == Outcome::Cancelled
                    && receipt.evidence["input_released"] == true
                {
                    // A later safe opening may start a new intention. The
                    // cancelled durable ID is never replayed.
                    last_meal = None;
                }
            }
        }
        if danger {
            let Some(_guard) = shared.try_observed_mutation(admission, observed_at) else {
                next_cycle(started, cfg.period_ms).await;
                continue;
            };
            let mut cancelling_control = false;
            if interaction["held_item_use"] == true
                && interaction["outcome"] == "running"
                && let Some(id) = interaction["action_id"].as_str()
                && id != cancelled_item_use
            {
                let receipt = send(
                    &client,
                    j,
                    Action::CancelUseItem {
                        action_id: id.into(),
                    },
                )
                .await?;
                cancelled_item_use = id.into();
                j.event("body_preempted_item_use",json!({"action_id":id,"cancel_action_id":receipt.action_id,"outcome":receipt.outcome,"reason":"release held input before urgent survival work","replayed":false}))?;
            }
            let toward_threat = geometry::point(&active["target"])
                .is_some_and(|target| geometry::toward_close_threat(&observed, target));
            let toward_heard_blast = geometry::point(&active["target"]).is_some_and(|target| {
                heard_blasts.iter().any(|(origin, _)| {
                    (0..3).map(|i| (target[i] - origin[i]).powi(2)).sum::<f64>()
                        < (0..3).map(|i| (p[i] - origin[i]).powi(2)).sum::<f64>()
                })
            });
            let toward_visible_blast = toward_visible_blast(&observed);
            if active["outcome"] == "running"
                && (active["action_id"].as_str() != Some(&owned_control)
                    || new_projectile
                    || toward_threat
                    || toward_heard_blast
                    || toward_visible_blast
                    || (water_emergency && !owned_water_control)
                    || stuck)
            {
                if active["cancel_requested"] != true
                    && let Some(id) = active["action_id"].as_str()
                {
                    cancelling_control = true;
                    if stuck {
                        if let Some(target) = geometry::point(&active["target"]) {
                            rejected_routes.push(geometry::target_key(target));
                        }
                        j.event("body_stuck_route",json!({"action_id":id,"position":p,"game_time":observed.world["game_time"],"reason":"owned escape has less than 0.2 blocks displacement over 14 advancing native ticks; cancel and try another observed route"}))?;
                    }
                    send(
                        &client,
                        j,
                        Action::CancelControl {
                            action_id: id.into(),
                        },
                    )
                    .await?;
                    j.event(
                        "body_preempted_program",
                        json!({"action_id":id,"reaction_ms":observed_at.elapsed().as_millis(),"cycle_ms":started.elapsed().as_millis(),"new_projectile":new_projectile,"incoming_projectile_ids":incoming_projectiles,"previous_dodge_projectile_ids":owned_projectiles,"toward_threat":toward_threat,"toward_heard_blast":toward_heard_blast,"toward_visible_blast":toward_visible_blast,"stuck":stuck}),
                    )?;
                }
            } else {
                // Historical uncertain inventory/world changes remain fenced by
                // the gateway. Independent evasion must not wait for their
                // reconciliation or replay those mutations.
                if active["outcome"] != "running" {
                    if dead {
                        send(&client, j, Action::Respawn).await?;
                        j.event(
                            "body_normal_recovery",
                            json!({"death_recorded":true,"world_reset":false}),
                        )?;
                    } else if let Some(route) = escape {
                        if continuing_cover {
                            let Some(original) = cover_handoff.consume() else {
                                next_cycle(started, cfg.period_ms).await;
                                continue;
                            };
                            j.event("body_cover_handoff_consumed",json!({"original_action_id":original,"revision":observed.revision,"target":route.target,"finite_max_ticks":60,"authority":"previously selected route, fresh terrain and ordinary native admission; no hidden cover safety","replayed":false}))?;
                        }
                        let key =
                            geometry::target_key([route.target.x, route.target.y, route.target.z]);
                        *shared.reason.lock().map_err(|_| "Body state unavailable")? =
                            route.reason.clone();
                        let dispatch_started = Instant::now();
                        let ranged_only =
                            observed.world["entities"]
                                .as_array()
                                .is_some_and(|entities| {
                                    let mut ranged = false;
                                    for e in entities
                                        .iter()
                                        .filter(|e| e["visible"] == true && e["hostile"] == true)
                                    {
                                        if e["ranged_attack"] != true {
                                            return false;
                                        }
                                        ranged = true;
                                    }
                                    ranged
                                });
                        let through = through_supported
                            && !continuing_cover
                            && ranged_only
                            && !burning
                            && geometry::can_pass_waypoint(&observed, &route);
                        let action = if through {
                            Action::StartMoveThrough {
                                target: route.target.clone(),
                                max_ticks: 60,
                            }
                        } else {
                            Action::StartMove {
                                target: route.target.clone(),
                                jump: route.jump,
                                max_ticks: 60,
                            }
                        };
                        let decision = (decision_samples > 0).then(|| {
                            decision::Snapshot::capture(
                                &policy_observed,
                                &rejected_routes,
                                &stationary_projectiles,
                                &trail,
                                finite_waypoint_axis,
                                &admitted,
                                &route,
                            )
                            .with_boundary_turns(cfg.experimental_ranged_boundary_turns)
                        });
                        let mut r = send(&client, j, action).await?;
                        // A definite pre-dispatch capability refusal can use
                        // the precise alternative. Never replay uncertain input.
                        if through
                            && r.outcome == Outcome::Rejected
                            && r.error
                                .as_ref()
                                .is_some_and(|e| e.code == ErrorCode::UnsupportedCapability)
                        {
                            r = send(
                                &client,
                                j,
                                Action::StartMove {
                                    target: route.target.clone(),
                                    jump: route.jump,
                                    max_ticks: 60,
                                },
                            )
                            .await?;
                        }
                        let dispatched_id = r.action_id.clone();
                        if let Some(decision) = decision {
                            decision_samples -= 1;
                            let bytes = serde_json::to_vec(&decision)?;
                            if bytes.len() <= decision::MAX_BYTES {
                                j.event("body_decision",json!({"action_id":dispatched_id,"snapshot":decision,
                                    "snapshot_sha256":crate::digest(&bytes),"scope":"Client policy inputs only; native receipts remain authoritative"}))?;
                            } else {
                                j.event("body_decision_omitted",json!({"action_id":dispatched_id,"bytes":bytes.len(),"limit":decision::MAX_BYTES,"reason":"Diagnostic input exceeds bounded record; no complete replay claim"}))?;
                            }
                        }
                        if r.outcome == Outcome::Running {
                            cover_handoff.record(&policy_observed, &route, &dispatched_id);
                            owned_control = dispatched_id.clone();
                            owned_water_shore =
                                route.reason == "leave water for observed dry support";
                            owned_water_control = water_emergency
                                || (observed.player["in_water"] == true && owned_water_shore);
                            owned_projectiles = incoming_projectiles.clone();
                        } else {
                            native_required.movement_rejected(&observed, &r);
                            rejected_routes.push(key);
                            j.event("body_route_failure",json!({"target":key,"outcome":r.outcome,"error":r.error,"next_action":"try a different observed route; investigate if none works"}))?;
                        }
                        j.event("body_reaction",json!({"reason":route.reason,"planned_cover_path":route.cover_path,"reaction_ms":observed_at.elapsed().as_millis(),"cycle_ms":started.elapsed().as_millis(),"policy_ms":policy_ms,"dispatch_ms":dispatch_started.elapsed().as_millis(),"observation_ms":observation_ms,"gateway_dispatch_timing":r.evidence["gateway_dispatch_timing"],"adapter_dispatch_timing":r.evidence["adapter_dispatch_timing"],"outcome":r.outcome,"action_id":dispatched_id,"revision":observed.revision,"sample_clock":observed.world["sample_clock"],"recent_retreat_points":trail.len()}))?;
                        // Start displacement before waiting on the optional
                        // sprint modifier's effect verification. This first
                        // trajectory uses its observed walking speed; the next
                        // one can use the newly observed sprint attribute.
                        if r.outcome == Outcome::Running
                            && observed.player["sprinting"] != true
                            && observed.player["food"].as_u64().is_some_and(|n| n > 6)
                        {
                            let sprint = send(&client, j, Action::Sprint { enabled: true }).await?;
                            body_sprint = sprint.outcome == Outcome::Observed;
                        }
                    } else {
                        *shared.reason.lock().map_err(|_|"Body state unavailable")?="Danger has no supported safe route; investigate perception/body capability".into();
                    }
                }
            }
            // Native movement and interaction have independent finite lanes.
            // Do not stop escaping just to defend against an adjacent hostile,
            // or wait for the route's expiry before attempting knockback.
            if !dead
                && !cancelling_control
                && (interaction["action_id"].is_null()
                    || interaction["action_id"] == inspected_interaction)
                && last_attack.elapsed() >= Duration::from_millis(650)
                && let Some(entity) = tactics::defensive_target(&observed)
            {
                let receipt = send(&client, j, Action::Attack { entity }).await?;
                last_attack = Instant::now();
                j.event("body_defensive_attack",json!({"entity":entity,"action_id":receipt.action_id,"outcome":receipt.outcome,"native_effect_verified":receipt.evidence["native_effect_verified"],"observed_control":active,"reason":"cooldown-ready defense without pausing escape; server validates sight/reach and damage"}))?;
            }
        } else if shared.urgent()
            && owned_water_control
            && !owned_water_shore
            && geometry::breathing_recovered(&observed)
            && active["action_id"].as_str() == Some(&owned_control)
            && active["outcome"] == "running"
        {
            // Release breathing-only input once, then wait for native release.
            // A three-second hold here lets the character sink before handoff.
            if active["cancel_requested"] != true && breathing_release_requested != owned_control {
                let Some(_guard) = shared.try_observed_mutation(admission, observed_at) else {
                    next_cycle(started, cfg.period_ms).await;
                    continue;
                };
                breathing_release_requested = owned_control.clone();
                let r = send(
                    &client,
                    j,
                    Action::CancelControl {
                        action_id: owned_control.clone(),
                    },
                )
                .await?;
                j.event(
                    "body_breathing_input_released",
                    json!({"action_id":owned_control,
                    "cancel_action_id":r.action_id,"outcome":r.outcome,"replayed":false}),
                )?;
            }
        } else if shared.urgent()
            && may_resume_after_danger(
                &observed,
                safe_since.elapsed(),
                &owned_control,
                owned_water_control,
                owned_water_shore,
            )
        {
            if body_sprint {
                let Some(_guard) = shared.try_observed_mutation(admission, observed_at) else {
                    next_cycle(started, cfg.period_ms).await;
                    continue;
                };
                send(&client, j, Action::Sprint { enabled: false }).await?;
                body_sprint = false;
            }
            shared.set_urgent(false);
            native_required.episode_ended();
            owned_water_control = false;
            owned_water_shore = false;
            *shared.reason.lock().map_err(|_| "Body state unavailable")? =
                "safe; ordinary program may resume".into();
            j.event(
                "body_program_resumed",
                json!({"revision":observed.revision,"position":p}),
            )?;
        } else if !shared.urgent()
            && safe_since.elapsed() >= Duration::from_secs(1)
            && let Some(meal) = tactics::meal(&observed)
            && last_meal.as_ref() != Some(&meal)
            && (interaction["action_id"].is_null()
                || interaction["action_id"] == inspected_interaction)
        {
            let Some(_guard) = shared.try_observed_mutation(admission, observed_at) else {
                next_cycle(started, cfg.period_ms).await;
                continue;
            };
            if observed_generation.is_multiple_of(2)
                && shared.program_generation.load(Ordering::Acquire) == observed_generation
                && observed_at.elapsed() < Duration::from_millis(250)
            {
                let action = meal.action();
                let using = matches!(action, Action::StartUseItem { .. });
                last_meal = Some(meal.clone());
                j.event("body_meal_attempt",json!({"item":meal.item,"slot":meal.slot,"count":meal.stock,"hunger":meal.hunger,"revision":observed.revision,"finite_use":using,"reason":"observed safe opening and confirmed edible stock"}))?;
                match tokio::time::timeout(Duration::from_millis(500), send(&client, j, action))
                    .await
                {
                    Ok(Ok(r)) if using && r.outcome == Outcome::Running => {
                        owned_meal = Some((r.action_id, meal));
                    }
                    Ok(Ok(_)) => {}
                    result => {
                        let error = match result {
                            Ok(Err(e)) => e.to_string(),
                            _ => "Meal dispatch deadline exceeded; inspect pending ID, no replay"
                                .into(),
                        };
                        j.event(
                            "body_meal_deferred",
                            json!({"error":error,"unchanged_attempt_retried":false}),
                        )?;
                    }
                }
            }
        } else if !shared.urgent()
            && cycles.is_multiple_of(10)
            && let Some(item) = client
                .pending_actions()
                .await?
                .into_iter()
                .find(|item| item.receipt.outcome == Outcome::Uncertain)
        {
            let result = client.reconcile(&item.receipt.action_id).await;
            if let Ok(receipt) = &result {
                shared.resolve_program_receipt(&item.receipt.action_id, receipt)?;
            }
            j.event("body_reconciliation",json!({"action_id":item.receipt.action_id,"outcome":result.as_ref().ok().map(|r|&r.outcome),"error":result.as_ref().err().map(ToString::to_string)}))?;
        }
        // SDK program status reads bypass the local proxy. Inspect one exact
        // tracked ID so completed finite controls cannot leave food fenced.
        if !shared.urgent()
            && cycles.is_multiple_of(10)
            && let Some(id) = shared.pending_program()?
            && let Ok(Ok(receipt)) =
                tokio::time::timeout(Duration::from_millis(400), client.action_status(&id)).await
        {
            shared.resolve_program_receipt(&id, &receipt)?;
        }
        if cycles.is_multiple_of(100) {
            j.event("body_heartbeat",json!({"cycles":cycles,"motion_predictions":motion_predictions,"revision":observed.revision,"observation_ms":observation_ms,"cycle_ms":started.elapsed().as_millis(),"emergency":shared.urgent()}))?;
        }
        next_cycle(started, cfg.period_ms).await;
    }
    shared.set_urgent(false);
    j.event("body_stopped", json!({"cycles":cycles}))?;
    Ok(())
}
async fn next_cycle(started: Instant, period_ms: u64) {
    let period = Duration::from_millis(period_ms);
    if let Some(remaining) = period.checked_sub(started.elapsed()) {
        tokio::time::sleep(remaining).await;
    }
}
