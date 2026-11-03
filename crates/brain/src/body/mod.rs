//! Replaceable container-local safety behavior, independent of model/terminal work.
pub mod geometry;
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
        atomic::{AtomicBool, Ordering},
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
}
impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            period_ms: period(),
        }
    }
}
pub struct Shared {
    pub mutation: Arc<tokio::sync::Mutex<()>>,
    pub model_lanes: crate::model::Lanes,
    pub feedback: Mutex<crate::feedback::Trace>,
    emergency: AtomicBool,
    running: AtomicBool,
    latest: Mutex<Option<(Instant, Arc<Observation>)>>,
    reason: Mutex<String>,
    pub failed: tokio::sync::Notify,
}
impl Shared {
    pub fn urgent(&self) -> bool {
        self.emergency.load(Ordering::Acquire)
    }
    pub fn view(&self) -> Value {
        json!({"emergency":self.urgent(),"running":self.running.load(Ordering::Acquire),"reason":self.reason.lock().ok().map(|s|s.clone()),"observation_age_ms":self.latest.lock().ok().and_then(|v|v.as_ref().map(|(at,_)|at.elapsed().as_millis()))})
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
        ensure(
            at.elapsed() < Duration::from_secs(3),
            "Body observation is stale",
        )?;
        Ok((**o).clone())
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
        use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
        ensure((50..=1000).contains(&cfg.period_ms), "Invalid body cadence")?;
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
                            r=controller(client.clone(),cfg,active.clone(),stopped,&journal)=>r,
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
            let result = client.act(&request.request).await;
            drop(guard);
            match result {
                Ok(r) => LocalActionResponse::Receipt(r),
                Err(e) => LocalActionResponse::Error(e),
            }
        } else {
            LocalActionResponse::Error(ApiError::new(
                ErrorCode::Conflict,
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
async fn controller(
    client: Arc<Client>,
    cfg: Config,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    j: &Journal,
) -> Result<()> {
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
    let mut cycles = 0u64;
    let mut rejected_routes = Vec::new();
    let mut conditions = String::new();
    let mut previous_health = Value::Null;
    let mut body_sprint = false;
    let mut inspected_interaction = String::new();
    let mut cancelled_item_use = String::new();
    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        let started = Instant::now();
        let observed = match tokio::time::timeout(Duration::from_secs(2), client.observe()).await {
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
        let observed_at = Instant::now();
        let observation_ms = started.elapsed().as_millis();
        cycles += 1;
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
        let incoming = geometry::incoming(&observed);
        let burning = observed.player["fire_ticks"]
            .as_i64()
            .is_some_and(|n| n > 0);
        let escape = if active["outcome"] != "running" {
            geometry::evade_avoiding(&observed, &rejected_routes)
        } else {
            None
        };
        let dead = observed.player["health"].as_f64().is_some_and(|h| h <= 0.);
        if cycles.is_multiple_of(10) || observed.player["health"] != previous_health {
            j.event("body_native_sample",json!({"revision":observed.revision,"health":observed.player["health"],"food":observed.player["food"],"position":p,"control":active,"nearest_hostile":nearby.map(|(e,d)|json!({"entity_id":e["entity_id"],"type":e["type"],"position":e["position"],"distance":d})),"incoming_projectile":incoming}))?;
            previous_health = observed.player["health"].clone();
        }
        let danger = dead
            || geometry::threatened(&observed)
            || escape.is_some()
            || incoming
            || burning
            || observed.player["in_lava"] == true;
        let policy_ms = observed_at.elapsed().as_millis();
        if danger {
            safe_since = Instant::now();
            if !shared.emergency.swap(true, Ordering::AcqRel) {
                j.event("body_emergency",json!({"revision":observed.revision,"health":observed.player["health"],"position":p,"nearest_hostile_distance":nearby.map(|(_,d)|d)}))?;
            }
        }
        // Flush terminal interaction evidence to the gateway before a later
        // body interaction replaces the adapter's current job. Reads never
        // replay input and remain independent of slow model/tool execution.
        let interaction = &observed.player["interaction"];
        if let Some(id) = interaction["action_id"].as_str()
            && interaction["outcome"] != "running"
            && id != inspected_interaction
            && let Ok(Ok(receipt)) =
                tokio::time::timeout(Duration::from_millis(400), client.action_status(id)).await
            && (receipt.outcome.terminal() || receipt.outcome == Outcome::Uncertain)
        {
            inspected_interaction = id.into();
            j.event("body_interaction_status",json!({"action_id":id,"outcome":receipt.outcome,"native_effect_verified":receipt.evidence["native_effect_verified"],"input_released":receipt.evidence["input_released"],"evidence_sha256":crate::digest(receipt.evidence.to_string().as_bytes()),"replayed":false}))?;
        }
        if danger {
            let _guard = shared.mutation.lock().await;
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
            let toward_threat = nearby.is_some_and(|(enemy, d)| {
                d < 5.
                    && geometry::point(&active["target"])
                        .zip(geometry::point(&enemy["position"]))
                        .is_some_and(|(t, e)| {
                            (0..3).map(|i| (t[i] - e[i]).powi(2)).sum::<f64>() < d * d
                        })
            });
            if active["outcome"] == "running"
                && (active["action_id"].as_str() != Some(&owned_control)
                    || incoming
                    || toward_threat)
            {
                if active["cancel_requested"] != true
                    && let Some(id) = active["action_id"].as_str()
                {
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
                        json!({"action_id":id,"reaction_ms":observed_at.elapsed().as_millis(),"cycle_ms":started.elapsed().as_millis()}),
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
                        let key =
                            geometry::target_key([route.target.x, route.target.y, route.target.z]);
                        *shared.reason.lock().map_err(|_| "Body state unavailable")? =
                            route.reason.clone();
                        let dispatch_started = Instant::now();
                        let r = send(
                            &client,
                            j,
                            Action::StartMove {
                                target: route.target,
                                jump: route.jump,
                                max_ticks: 60,
                            },
                        )
                        .await?;
                        let dispatched_id = r.action_id.clone();
                        if r.outcome == Outcome::Running {
                            owned_control = dispatched_id.clone();
                        } else {
                            rejected_routes.push(key);
                            j.event("body_route_failure",json!({"target":key,"outcome":r.outcome,"error":r.error,"next_action":"try a different observed route; investigate if none works"}))?;
                        }
                        j.event("body_reaction",json!({"reason":route.reason,"reaction_ms":observed_at.elapsed().as_millis(),"cycle_ms":started.elapsed().as_millis(),"policy_ms":policy_ms,"dispatch_ms":dispatch_started.elapsed().as_millis(),"observation_ms":observation_ms,"gateway_dispatch_timing":r.evidence["gateway_dispatch_timing"],"adapter_dispatch_timing":r.evidence["adapter_dispatch_timing"],"outcome":r.outcome,"action_id":dispatched_id,"revision":observed.revision,"sample_clock":observed.world["sample_clock"]}))?;
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
                    } else if let Some((enemy, d)) = nearby
                        && d <= 3.
                        && last_attack.elapsed() > Duration::from_millis(650)
                    {
                        if let Some(id) = enemy["entity_id"].as_i64() {
                            send(
                                &client,
                                j,
                                Action::Attack {
                                    entity: i32::try_from(id)?,
                                },
                            )
                            .await?;
                            last_attack = Instant::now();
                        }
                    } else {
                        *shared.reason.lock().map_err(|_|"Body state unavailable")?="Danger has no supported safe route; investigate perception/body capability".into();
                    }
                }
            }
        } else if shared.urgent() && safe_since.elapsed() > Duration::from_secs(3) {
            if body_sprint {
                let _guard = shared.mutation.lock().await;
                send(&client, j, Action::Sprint { enabled: false }).await?;
                body_sprint = false;
            }
            shared.emergency.store(false, Ordering::Release);
            *shared.reason.lock().map_err(|_| "Body state unavailable")? =
                "safe; ordinary program may resume".into();
            j.event(
                "body_program_resumed",
                json!({"revision":observed.revision,"position":p}),
            )?;
        } else if !shared.urgent()
            && cycles.is_multiple_of(10)
            && let Some(item) = client
                .pending_actions()
                .await?
                .into_iter()
                .find(|item| item.receipt.outcome == Outcome::Uncertain)
        {
            let result = client.reconcile(&item.receipt.action_id).await;
            j.event("body_reconciliation",json!({"action_id":item.receipt.action_id,"outcome":result.as_ref().ok().map(|r|&r.outcome),"error":result.as_ref().err().map(ToString::to_string)}))?;
        }
        if cycles.is_multiple_of(100) {
            j.event("body_heartbeat",json!({"cycles":cycles,"revision":observed.revision,"observation_ms":observation_ms,"cycle_ms":started.elapsed().as_millis(),"emergency":shared.urgent()}))?;
        }
        let elapsed = started.elapsed();
        if elapsed < Duration::from_millis(cfg.period_ms) {
            tokio::time::sleep(Duration::from_millis(cfg.period_ms) - elapsed).await;
        }
    }
    shared.emergency.store(false, Ordering::Release);
    j.event("body_stopped", json!({"cycles":cycles}))?;
    Ok(())
}
