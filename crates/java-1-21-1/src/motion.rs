//! One physical writer per admitted connection, independent of action/model work.
use super::*;
use minecraft_sdk_java_1_21_1::physics::{Body, Medium, Parameters};
use std::{
    collections::VecDeque,
    sync::{Condvar, atomic::AtomicI32},
};

// A finite waypoint can reach the edge of the local view when swept body
// geometry is actually known. The preview and per-frame checks still reject
// unknown space; this scalar is only the outer request bound.
pub(super) const FINITE_WAYPOINT_AXIS: f64 = 4.05;

#[derive(Clone, Copy, Debug)]
pub(super) enum Impulse {
    Set([f64; 3]),
    Add([f64; 3]),
}
pub(super) type Inbox = Arc<Mutex<VecDeque<Impulse>>>;
pub(super) fn push_impulse(inbox: &Inbox, event: Impulse) -> AnyResult<()> {
    let mut q = inbox.lock().map_err(|_| "Impulse state unavailable")?;
    let next = match (q.pop_back(), event) {
        (_, Impulse::Set(v)) => Impulse::Set(v),
        (Some(Impulse::Set(mut v)), Impulse::Add(a)) => {
            for i in 0..3 {
                v[i] += a[i];
            }
            Impulse::Set(v)
        }
        (Some(Impulse::Add(mut v)), Impulse::Add(a)) => {
            for i in 0..3 {
                v[i] += a[i];
            }
            Impulse::Add(v)
        }
        (None, v) => v,
    };
    let vector = match next {
        Impulse::Set(v) | Impulse::Add(v) => v,
    };
    if vector.iter().any(|n| !n.is_finite()) {
        return Err("Native momentum overflow".into());
    }
    q.clear();
    q.push_back(next);
    Ok(())
}
pub(super) fn entity_impulse(bytes: &[u8], own: i32) -> AnyResult<Option<Impulse>> {
    let (id, n) = decode_varint_prefix(bytes)?;
    if id != own as u32 {
        return Ok(None);
    }
    if bytes.len() != n + 6 {
        return Err("Malformed native entity momentum".into());
    }
    let mut v = [0.; 3];
    for i in 0..3 {
        v[i] = f64::from(i16::from_be_bytes(
            bytes[n + i * 2..n + i * 2 + 2].try_into()?,
        )) / 8000.;
    }
    Ok(Some(Impulse::Set(v)))
}
pub(super) fn explosion_impulse(bytes: &[u8]) -> AnyResult<Impulse> {
    let (count, n) = decode_varint_prefix(bytes.get(28..).ok_or("Truncated explosion")?)?;
    let offset = 28usize
        .checked_add(n)
        .and_then(|v| v.checked_add(usize::try_from(count).ok()?.checked_mul(3)?))
        .ok_or("Explosion record count overflow")?;
    let data = bytes
        .get(offset..offset.checked_add(12).ok_or("Explosion size overflow")?)
        .ok_or("Truncated explosion momentum")?;
    let mut v = [0.; 3];
    for i in 0..3 {
        v[i] = f64::from(f32::from_be_bytes(data[i * 4..i * 4 + 4].try_into()?));
        if !v[i].is_finite() {
            return Err("Invalid explosion momentum".into());
        }
    }
    Ok(Impulse::Add(v))
}
struct Scene {
    boxes: Vec<[f64; 6]>,
    cells: std::collections::HashSet<[i32; 3]>,
    surfaces: Vec<([f64; 6], f64, f64)>,
}
impl Scene {
    fn new(o: &Observation) -> AnyResult<Self> {
        let boxes = collision_boxes(o)?;
        let mut cells = std::collections::HashSet::new();
        let mut surfaces = Vec::new();
        for block in o.world["blocks"]
            .as_array()
            .ok_or("Observed terrain missing")?
        {
            let p = &block["position"];
            let origin = [
                p[0].as_i64().ok_or("Block x missing")? as i32,
                p[1].as_i64().ok_or("Block y missing")? as i32,
                p[2].as_i64().ok_or("Block z missing")? as i32,
            ];
            cells.insert(origin);
            let friction = block["friction"]
                .as_f64()
                .filter(|x| x.is_finite() && *x >= 0. && *x <= 2.)
                .ok_or("Native block friction missing")?;
            let speed = block["speed_factor"]
                .as_f64()
                .filter(|x| x.is_finite() && *x >= 0. && *x <= 8.)
                .ok_or("Native block speed factor missing")?;
            for shape in block["collision"]
                .as_array()
                .ok_or("Native collision missing")?
            {
                let mut b = [0.; 6];
                for i in 0..6 {
                    b[i] = shape[i].as_f64().ok_or("Collision bound missing")?
                        + f64::from(origin[i % 3]);
                }
                surfaces.push((b, friction, speed));
            }
        }
        Ok(Self {
            boxes,
            cells,
            surfaces,
        })
    }
    fn covers(&self, from: [f64; 3], to: [f64; 3], p: Parameters) -> bool {
        let min = [
            (from[0].min(to[0]) - p.width / 2.).floor() as i32,
            (from[1].min(to[1]) - 0.01).floor() as i32,
            (from[2].min(to[2]) - p.width / 2.).floor() as i32,
        ];
        let max = [
            (from[0].max(to[0]) + p.width / 2.).floor() as i32,
            (from[1].max(to[1]) + p.height + p.step_height).floor() as i32,
            (from[2].max(to[2]) + p.width / 2.).floor() as i32,
        ];
        let volume = (0..3)
            .map(|i| i64::from(max[i]) - i64::from(min[i]) + 1)
            .product::<i64>();
        if !(1..=4096).contains(&volume) {
            return false;
        }
        (min[0]..=max[0]).all(|x| {
            (min[1]..=max[1]).all(|y| (min[2]..=max[2]).all(|z| self.cells.contains(&[x, y, z])))
        })
    }
    fn surface(&self, body: &Body, width: f64) -> (f64, f64) {
        self.surfaces
            .iter()
            .find(|(b, _, _)| {
                (b[4] - body.position[1]).abs() < 0.002
                    && body.position[0] > b[0] - width / 2.
                    && body.position[0] < b[3] + width / 2.
                    && body.position[2] > b[2] - width / 2.
                    && body.position[2] < b[5] + width / 2.
            })
            .map(|(_, f, s)| (*f, *s))
            .unwrap_or((0.6, 1.))
    }
}
fn number(o: &Value, key: &str, min: f64, max: f64) -> AnyResult<f64> {
    o[key]
        .as_f64()
        .filter(|n| n.is_finite() && *n >= min && *n <= max)
        .ok_or_else(|| format!("Invalid native physics fact: {key}").into())
}
fn parameters(o: &Observation) -> AnyResult<Parameters> {
    let p = &o.player["physics"];
    if p["format"] != "java767-physics-v1" {
        return Err("Matching native physics facts required".into());
    }
    if p["flying"] == true || p["fall_flying"] == true {
        return Err("Flight physics capability is not installed".into());
    }
    Ok(Parameters {
        width: number(p, "width", 0.1, 4.)?,
        height: number(p, "height", 0.1, 4.)?,
        gravity: number(p, "gravity", 0., 1.)?,
        jump_velocity: number(p, "jump_velocity", 0., 3.)?,
        step_height: number(p, "step_height", 0., 2.)?,
        water_efficiency: number(p, "water_efficiency", 0., 1.)?,
        no_gravity: p["no_gravity"] == true,
        slow_falling: p["slow_falling"] == true,
        dolphins_grace: p["dolphins_grace"] == true,
        levitation: p["levitation"]
            .as_i64()
            .filter(|n| *n >= 0)
            .and_then(|n| u8::try_from(n).ok()),
        sprinting: o.player["sprinting"] == true,
        medium: if o.player["in_water"] == true {
            Medium::Water
        } else if o.player["in_lava"] == true {
            Medium::Lava
        } else {
            Medium::Air
        },
        ..Parameters::default()
    })
}
fn observed_body(o: &Observation) -> AnyResult<Body> {
    let mut velocity = [0.; 3];
    for (i, v) in velocity.iter_mut().enumerate() {
        *v = o.player["velocity"][i]
            .as_f64()
            .filter(|v| v.is_finite())
            .unwrap_or(0.);
    }
    Ok(Body {
        position: position(o)?,
        velocity,
        on_ground: o.player["on_ground"] == true,
    })
}
fn arrived(body: &Body, target: [f64; 3], needs_jump: bool, medium: Medium) -> bool {
    (!needs_jump || medium != Medium::Air)
        && distance(body.position, target) < 0.08
        && body.velocity[0].hypot(body.velocity[2]) < 0.025
        && (body.on_ground || medium != Medium::Air)
}
fn steering(
    body: &Body,
    target: [f64; 3],
    needs_jump: &mut bool,
    o: &Observation,
    p: Parameters,
) -> AnyResult<([f64; 3], bool)> {
    let delta = [
        target[0] - body.position[0],
        0.,
        target[2] - body.position[2],
    ];
    let len = delta[0].hypot(delta[2]);
    let speed = number(&o.player, "movement_speed", 0., 4.)?;
    let slow = if o.player["sneaking"] == true {
        0.3
    } else if o.player["using_item"] == true {
        0.2
    } else {
        1.
    };
    let accel = if p.medium == Medium::Air {
        if body.on_ground {
            speed * 0.21600002 / p.friction.max(0.01).powi(3)
        } else if p.sprinting {
            0.026
        } else {
            0.02
        }
    } else {
        0.02
    };
    let desired = (speed * 2.1585 * slow).min(len * 0.5);
    let mut change = [
        if len > 1e-9 {
            delta[0] / len * desired - body.velocity[0]
        } else {
            -body.velocity[0]
        },
        0.,
        if len > 1e-9 {
            delta[2] / len * desired - body.velocity[2]
        } else {
            -body.velocity[2]
        },
    ];
    let amount = change[0].hypot(change[2]);
    let factor = if amount > accel * slow {
        accel * slow / amount
    } else {
        1.
    };
    for v in &mut change {
        *v *= factor;
    }
    let mut jump = *needs_jump && (p.medium != Medium::Air || body.on_ground);
    if jump && p.medium == Medium::Air {
        *needs_jump = false;
    }
    if p.medium != Medium::Air && target[1] > body.position[1] + 0.1 {
        jump = true;
    }
    Ok((change, jump))
}
/// Admission/query prediction uses exactly the held-input equations executed by
/// the physical writer, including initial momentum and grounded/air controls.
pub(super) fn preview(
    o: &Observation,
    target: [f64; 3],
    jump: bool,
) -> AnyResult<Vec<([f64; 3], bool)>> {
    let scene = Scene::new(o)?;
    let params = parameters(o)?;
    let mut body = observed_body(o)?;
    let mut needs_jump = jump;
    let mut frames = Vec::new();
    for _ in 0..100 {
        if arrived(&body, target, needs_jump, params.medium) {
            return Ok(frames);
        }
        let (friction, speed_factor) = scene.surface(&body, params.width);
        let p = Parameters {
            friction,
            speed_factor,
            ..params
        };
        let (acceleration, jump) = steering(&body, target, &mut needs_jump, o, p)?;
        let mut next = body;
        next.tick(acceleration, jump, p, &scene.boxes);
        if !scene.covers(body.position, next.position, p) {
            return Err("Input prediction leaves observed collision geometry".into());
        }
        body = next;
        frames.push((body.position, body.on_ground));
    }
    Err("No bounded held-input trajectory reaches the requested waypoint".into())
}
struct Snapshot {
    at: Instant,
    requested_at: Instant,
    epoch: u64,
    o: Observation,
    scene: Scene,
}
struct Cache {
    latest: Mutex<Option<Arc<Snapshot>>>,
    wake: Condvar,
}
struct Drive {
    action: ActionRequest,
    before: Observation,
    target: [f64; 3],
    jump: bool,
    started: Instant,
    input_started: Option<Instant>,
    expiry: Duration,
    state: Arc<Mutex<Receipt>>,
    cancel: Arc<AtomicBool>,
    released: Option<(Outcome, Instant)>,
    release_frame_at: Option<Instant>,
    frames: Vec<([f64; 3], bool)>,
    release_at: usize,
    epoch: u64,
    entity: i64,
    momentum_updates: u64,
    momentum_samples: Vec<Value>,
}
impl Drive {
    fn momentum_evidence(&self) -> Value {
        json!({"applied_updates":self.momentum_updates,"samples":self.momentum_samples,"omitted_samples":self.momentum_updates.saturating_sub(self.momentum_samples.len() as u64),"scope":"Coalesced server velocity updates applied by this physical writer; not damage attribution or target-completion evidence"})
    }
}
struct Engine {
    body: Body,
    epoch: u64,
    drive: Option<Drive>,
    frames: u64,
    reason: String,
}
#[derive(Clone)]
pub(super) struct Actor {
    cache: Arc<Cache>,
    engine: Arc<Mutex<Engine>>,
    alive: Arc<AtomicBool>,
}
pub(super) struct Wires {
    pub writer: Arc<Mutex<Wire<TcpStream>>>,
    pub tables: Tables,
    pub alive: Arc<AtomicBool>,
    pub corrections: Arc<AtomicU64>,
    pub correcting: Arc<AtomicBool>,
    pub ground: Arc<AtomicBool>,
    pub impulses: Inbox,
    pub entity: Arc<AtomicI32>,
}
impl Actor {
    pub fn start(native: Native, uuid: String, initial: Observation, w: Wires) -> AnyResult<Self> {
        if initial.player["uuid"] != uuid {
            return Err("Native actor binding mismatch".into());
        }
        parameters(&initial)?;
        let epoch = w.corrections.load(Ordering::Acquire);
        let scene = Scene::new(&initial)?;
        w.entity.store(
            initial.player["entity_id"]
                .as_i64()
                .and_then(|v| i32::try_from(v).ok())
                .ok_or("Native entity ID missing")?,
            Ordering::Release,
        );
        let body = observed_body(&initial)?;
        let cache = Arc::new(Cache {
            latest: Mutex::new(Some(Arc::new(Snapshot {
                at: Instant::now(),
                requested_at: Instant::now(),
                epoch,
                o: initial,
                scene,
            }))),
            wake: Condvar::new(),
        });
        let engine = Arc::new(Mutex::new(Engine {
            body,
            epoch,
            drive: None,
            frames: 0,
            reason: "native physics running".into(),
        }));
        let actor = Self {
            cache: cache.clone(),
            engine: engine.clone(),
            alive: w.alive.clone(),
        };
        let refresh_cache = cache.clone();
        let refresh_alive = w.alive.clone();
        let refresh_epoch = w.corrections.clone();
        let refresh_correcting = w.correcting.clone();
        thread::spawn(move || {
            while refresh_alive.load(Ordering::Acquire) {
                let epoch = refresh_epoch.load(Ordering::Acquire);
                let requested_at = Instant::now();
                if !refresh_correcting.load(Ordering::Acquire)
                    && let Ok(o) = native.observe_bounded(&uuid, Duration::from_millis(400))
                    && o.player["uuid"] == uuid
                    && epoch == refresh_epoch.load(Ordering::Acquire)
                    && !refresh_correcting.load(Ordering::Acquire)
                    && let Ok(scene) = Scene::new(&o)
                    && let Ok(mut latest) = refresh_cache.latest.lock()
                {
                    *latest = Some(Arc::new(Snapshot {
                        at: Instant::now(),
                        requested_at,
                        epoch,
                        o,
                        scene,
                    }));
                }
                if let Ok(latest) = refresh_cache.latest.lock() {
                    drop(
                        refresh_cache
                            .wake
                            .wait_timeout(latest, Duration::from_millis(150)),
                    );
                }
            }
        });
        thread::spawn(move || {
            let run = || -> AnyResult<()> {
                let mut unknown_since = None;
                while w.alive.load(Ordering::Acquire) {
                    let at = Instant::now();
                    let snapshot = cache
                        .latest
                        .lock()
                        .map_err(|_| "Physics cache unavailable")?
                        .clone()
                        .ok_or("Physics cache absent")?;
                    if snapshot.at.elapsed() > Duration::from_secs(2) {
                        return Err("Physics lost fresh native observations".into());
                    }
                    if w.correcting.load(Ordering::Acquire)
                        || snapshot.epoch != w.corrections.load(Ordering::Acquire)
                    {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    let p = parameters(&snapshot.o)?;
                    w.entity.store(
                        snapshot.o.player["entity_id"]
                            .as_i64()
                            .and_then(|n| i32::try_from(n).ok())
                            .ok_or("Native entity ID missing")?,
                        Ordering::Release,
                    );
                    let mut e = engine.lock().map_err(|_| "Physical state unavailable")?;
                    if e.epoch != snapshot.epoch {
                        if let Some(d) = e.drive.take() {
                            finish_interrupted(d, "Server relocation interrupted this control");
                        }
                        e.body = Body {
                            position: position(&snapshot.o)?,
                            velocity: [0.; 3],
                            on_ground: false,
                        };
                        e.epoch = snapshot.epoch;
                    }
                    if snapshot.o.player["health"]
                        .as_f64()
                        .is_some_and(|h| h <= 0.)
                    {
                        if let Some(d) = e.drive.take() {
                            finish_interrupted(d, "Character died during this control");
                        }
                        drop(e);
                        thread::sleep(Duration::from_millis(50));
                        continue;
                    }
                    let impulses: Vec<_> = w
                        .impulses
                        .lock()
                        .map_err(|_| "Impulse state unavailable")?
                        .drain(..)
                        .collect();
                    if !impulses.is_empty() {
                        for impulse in &impulses {
                            match *impulse {
                                Impulse::Set(v) => e.body.velocity = v,
                                Impulse::Add(v) => {
                                    for (old, n) in e.body.velocity.iter_mut().zip(v) {
                                        *old += n;
                                    }
                                }
                            }
                        }
                        if let Some(d) = &mut e.drive {
                            // Native momentum changes velocity, not the held
                            // input. A normal player can keep steering through
                            // knockback. The same finite ID, expiry and explicit
                            // cancellation/relocation fences remain in force.
                            d.momentum_updates =
                                d.momentum_updates.saturating_add(impulses.len() as u64);
                            for impulse in &impulses {
                                if d.momentum_samples.len() < 8 {
                                    let (operation, velocity) = match impulse {
                                        Impulse::Set(v) => ("set", v),
                                        Impulse::Add(v) => ("add", v),
                                    };
                                    d.momentum_samples.push(json!({"frame":d.frames.len(),"operation":operation,"velocity":velocity,"input_held":d.released.is_none()}));
                                }
                            }
                            if let Ok(mut receipt) = d.state.lock() {
                                receipt.evidence["server_momentum"] = d.momentum_evidence();
                            }
                        }
                    }
                    if let Some(mut d) = e.drive.take() {
                        if let Ok(mut receipt) = d.state.lock()
                            && receipt.evidence["first_effect"]["verified"] != true
                        {
                            let ctx = control::FirstEffectContext {
                                clock: d.before.world["sample_clock"].clone(),
                                uuid: d.before.player["uuid"].as_str().unwrap_or("").to_owned(),
                                dimension: d.before.player["dimension"].clone(),
                                initial: position_of(&d.before),
                                frames: d.frames.clone(),
                                corrections: w.corrections.clone(),
                                epoch: d.epoch,
                            };
                            if let Some(effect) = control::first_effect_sample(
                                &ctx,
                                &snapshot.o,
                                d.frames.len() as u32,
                            ) {
                                receipt.evidence["first_effect"] = effect;
                            }
                        }
                        if d.epoch != e.epoch
                            || snapshot.o.player["dimension"] != d.before.player["dimension"]
                            || snapshot.o.player["entity_id"].as_i64() != Some(d.entity)
                        {
                            finish_interrupted(d, "Character binding changed during this control");
                        } else {
                            if d.cancel.load(Ordering::Acquire) {
                                release(&mut d, Outcome::Cancelled);
                            }
                            if d.input_started.is_some_and(|at| at.elapsed() >= d.expiry) {
                                release(&mut d, Outcome::Expired);
                            }
                            if d.released.is_none() && arrived(&e.body, d.target, d.jump, p.medium)
                            {
                                release(&mut d, Outcome::Observed);
                            }
                            if let Some((outcome, released_at)) = &d.released {
                                let observed = position(&snapshot.o)?;
                                let newer = newer_sample(&d.before, &snapshot.o);
                                if newer
                                    && d.release_frame_at
                                        .is_some_and(|sent| snapshot.requested_at > sent)
                                    && d.frames
                                        .iter()
                                        .skip(d.release_at)
                                        .any(|(pos, _)| distance(*pos, observed) < 0.08)
                                {
                                    let verified = *outcome != Outcome::Observed
                                        || movement_target_reached(observed, d.target);
                                    let final_outcome = if verified {
                                        outcome.clone()
                                    } else {
                                        Outcome::Failed
                                    };
                                    if let Ok(mut receipt) = d.state.lock() {
                                        receipt.outcome = final_outcome;
                                        receipt.error=(!verified).then(||ApiError::new(ErrorCode::Conflict,"Released control did not reach its observed target"));
                                        let first = receipt.evidence["first_effect"].clone();
                                        receipt.evidence = json!({"before":compact(&d.action.action,&d.before),"after":compact(&d.action.action,&snapshot.o),"target":d.target,"target_tolerance_blocks":MOVEMENT_TARGET_TOLERANCE,"native_effect_verified":verified,"input_released":true,"frames_sent":d.frames.len(),"elapsed_ms":d.started.elapsed().as_millis(),"first_effect":first,"server_momentum":d.momentum_evidence(),"release_scope":"Native pose matches a submitted released-input frame; passive gravity and momentum continue"});
                                    }
                                } else if released_at.elapsed() > Duration::from_secs(2) {
                                    finish_interrupted(
                                        d,
                                        "Released-input effect could not be verified from fresh native state",
                                    );
                                } else {
                                    e.drive = Some(d);
                                }
                            } else {
                                e.drive = Some(d);
                            }
                        }
                    }
                    let (friction, speed_factor) = snapshot.scene.surface(&e.body, p.width);
                    let p = Parameters {
                        friction,
                        speed_factor,
                        ..p
                    };
                    let mut acceleration = [0.; 3];
                    let mut jump = false;
                    let body = e.body;
                    if let Some(d) = &mut e.drive
                        && d.released.is_none()
                    {
                        (acceleration, jump) =
                            steering(&body, d.target, &mut d.jump, &snapshot.o, p)?;
                    }
                    let mut next = e.body;
                    next.tick(acceleration, jump, p, &snapshot.scene.boxes);
                    if !snapshot.scene.covers(e.body.position, next.position, p) {
                        e.reason = "Waiting for observed collision geometry".into();
                        cache.wake.notify_one();
                        let since = unknown_since.get_or_insert_with(Instant::now);
                        if since.elapsed() > Duration::from_secs(2) {
                            return Err("Physics cannot enter unobserved terrain".into());
                        }
                        drop(e);
                        thread::sleep(Duration::from_millis(25));
                        continue;
                    }
                    unknown_since = None;
                    let mut bytes = Vec::with_capacity(25);
                    for coordinate in next.position {
                        bytes.extend(coordinate.to_be_bytes());
                    }
                    bytes.push(u8::from(next.on_ground));
                    {
                        let mut writer =
                            w.writer.lock().map_err(|_| "Physical writer unavailable")?;
                        if w.correcting.load(Ordering::Acquire)
                            || e.epoch != w.corrections.load(Ordering::Acquire)
                        {
                            drop(writer);
                            drop(e);
                            continue;
                        }
                        w.tables.send(&mut writer, "play", "position", &bytes)?;
                        w.ground.store(next.on_ground, Ordering::Release);
                    }
                    e.body = next;
                    e.frames += 1;
                    e.reason = "continuous native-observed physics".into();
                    if let Some(d) = &mut e.drive {
                        if d.released.is_none() && d.input_started.is_none() {
                            d.input_started = Some(Instant::now());
                        }
                        d.frames.push((next.position, next.on_ground));
                        if d.released.is_some() && d.release_frame_at.is_none() {
                            d.release_frame_at = Some(Instant::now());
                        }
                        if let Ok(mut r) = d.state.lock() {
                            r.evidence["progress"] = json!({"frames_sent":d.frames.len(),"commanded_position":next.position,"elapsed_ms":d.started.elapsed().as_millis(),"releasing":d.released.is_some()});
                        }
                    }
                    drop(e);
                    if at.elapsed() < Duration::from_millis(50) {
                        thread::sleep(Duration::from_millis(50) - at.elapsed());
                    }
                }
                Ok(())
            };
            let result = run();
            if let Ok(mut e) = engine.lock() {
                e.reason = result
                    .err()
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "Connection closed".into());
                if let Some(d) = e.drive.take() {
                    finish_interrupted(d, &e.reason);
                }
            }
            w.alive.store(false, Ordering::Release);
            cache.wake.notify_all();
            if let Ok(writer) = w.writer.lock() {
                let _ = writer.connection().shutdown(std::net::Shutdown::Both);
            }
        });
        Ok(actor)
    }
    pub fn observe(&self) -> AnyResult<Observation> {
        let snapshot = self
            .cache
            .latest
            .lock()
            .map_err(|_| "Physics cache unavailable")?
            .clone()
            .ok_or("Physics cache absent")?;
        if snapshot.at.elapsed() > Duration::from_secs(1) || !self.alive.load(Ordering::Acquire) {
            return Err("Native physics observation is stale".into());
        }
        let mut o = snapshot.o.clone();
        let e = self
            .engine
            .lock()
            .map_err(|_| "Physical state unavailable")?;
        o.player["physical_controller"] = json!({"frames_sent":e.frames,"reason":e.reason,"observation_age_ms":snapshot.at.elapsed().as_millis(),"source":"client controller metadata, not native effect authority"});
        Ok(o)
    }
    pub fn start_control(
        &self,
        before: Observation,
        request: &ActionRequest,
        epoch: u64,
        state: Arc<Mutex<Receipt>>,
        cancel: Arc<AtomicBool>,
    ) -> Result<()> {
        if before.player["health"].as_f64().is_some_and(|n| n <= 0.) {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "A dead character must respawn before moving",
            ));
        }
        let Action::StartMove {
            target,
            jump,
            max_ticks,
        } = &request.action
        else {
            return Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "Finite movement required",
            ));
        };
        let mut e = self
            .engine
            .lock()
            .map_err(|_| ApiError::new(ErrorCode::Internal, "Physical state unavailable"))?;
        if e.drive.is_some() {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "Existing control must be inspected/cancelled",
            ));
        }
        if e.epoch != epoch {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "Server relocated this character; observe fresh state",
            ));
        }
        let target = [target.x, target.y, target.z];
        if (target[0] - e.body.position[0]).abs() > FINITE_WAYPOINT_AXIS
            || (target[2] - e.body.position[2]).abs() > FINITE_WAYPOINT_AXIS
            || (target[1] - e.body.position[1]).abs() > 4.
        {
            return Err(ApiError::new(
                ErrorCode::UnsupportedCapability,
                "Target exceeds the currently observed local waypoint range",
            ));
        }
        let mut predicted = before.clone();
        predicted.player["position"] = json!(e.body.position);
        predicted.player["velocity"] = json!(e.body.velocity);
        predicted.player["on_ground"] = json!(e.body.on_ground);
        preview(&predicted, target, *jump).map_err(|error| {
            ApiError::new(
                ErrorCode::UnsupportedCapability,
                format!("No supported held-input trajectory: {error}"),
            )
        })?;
        e.drive = Some(Drive {
            action: request.clone(),
            target,
            jump: *jump,
            started: Instant::now(),
            input_started: None,
            expiry: Duration::from_millis(u64::from(*max_ticks) * 50),
            state,
            cancel,
            released: None,
            release_frame_at: None,
            frames: Vec::new(),
            release_at: 0,
            momentum_updates: 0,
            momentum_samples: Vec::new(),
            epoch,
            entity: before.player["entity_id"].as_i64().ok_or_else(|| {
                ApiError::new(ErrorCode::NotReady, "Native entity binding missing")
            })?,
            before,
        });
        Ok(())
    }
}
fn position_of(o: &Observation) -> [f64; 3] {
    position(o).unwrap_or([f64::NAN; 3])
}
fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f64>().sqrt()
}
fn newer_sample(before: &Observation, after: &Observation) -> bool {
    let elapsed = before.world["sample_clock"]["nanos"]
        .as_str()
        .and_then(|n| n.parse::<u64>().ok())
        .zip(
            after.world["sample_clock"]["nanos"]
                .as_str()
                .and_then(|n| n.parse::<u64>().ok()),
        )
        .map(|(a, b)| b.wrapping_sub(a));
    before.player["uuid"] == after.player["uuid"]
        && before.player["dimension"] == after.player["dimension"]
        && before.world["sample_clock"]["epoch"] == after.world["sample_clock"]["epoch"]
        && before.revision != after.revision
        && elapsed.is_some_and(|n| n > 0 && n <= 120_000_000_000)
}
fn release(d: &mut Drive, outcome: Outcome) {
    if d.released.is_none() {
        d.released = Some((outcome, Instant::now()));
        d.release_at = d.frames.len();
    }
}
fn finish_interrupted(d: Drive, reason: &str) {
    if let Ok(mut r) = d.state.lock() {
        r.outcome = Outcome::Uncertain;
        r.error = Some(ApiError::new(
            ErrorCode::UncertainOutcome,
            format!("Physical control interrupted: {reason}; reconcile without replay"),
        ));
        r.evidence["input_released"] = json!(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input_prediction_fixture() -> Observation {
        let mut blocks = Vec::new();
        for x in -4..=4 {
            for y in -3..=4 {
                for z in -4..=4 {
                    blocks.push(json!({"position":[x,y,z],"friction":0.6,"speed_factor":1.,"collision":if y==0 || y == -3 {json!([[0.,0.,0.,1.,1.,1.]])}else{json!([])}}));
                }
            }
        }
        Observation {
            revision: "fixture".into(),
            menu: Value::Null,
            player: json!({"uuid":"fixture","dimension":"minecraft:overworld","position":[0.5,1.,0.5],"velocity":[0.,0.,0.],"health":20.,"movement_speed":0.1,"on_ground":true,"physics":{"format":"java767-physics-v1","width":0.6,"height":1.8,"gravity":0.08,"jump_velocity":0.42,"step_height":0.6,"water_efficiency":0.,"levitation":-1}}),
            world: json!({"blocks":blocks}),
        }
    }
    #[test]
    fn longer_local_input_still_requires_observed_swept_geometry() {
        let o = input_prediction_fixture();
        let target = [4.45, 1.0, 4.45];
        let frames = preview(&o, target, false).expect("known diagonal floor");
        assert!(frames.len() < 100);
        assert!(distance(frames.last().unwrap().0, target) < 0.08);
        let mut unknown = o.clone();
        unknown.world["blocks"]
            .as_array_mut()
            .unwrap()
            .retain(|b| b["position"][0].as_i64().unwrap() < 4);
        assert!(preview(&unknown, target, false).is_err());
        let mut wall = o;
        for b in wall.world["blocks"].as_array_mut().unwrap() {
            if b["position"][0] == 2 && b["position"][1] == 1 {
                b["collision"] = json!([[0., 0., 0., 1., 1., 1.]]);
            }
        }
        assert!(preview(&wall, target, false).is_err());
    }
    fn diagonal_gap_fixture() -> Observation {
        let mut o = input_prediction_fixture();
        for row in o.world["blocks"].as_array_mut().unwrap() {
            if row["position"][0] == -1
                && row["position"][1] == 0
                && row["position"][2]
                    .as_i64()
                    .is_some_and(|z| (-1..=2).contains(&z))
            {
                row["collision"] = json!([]);
            }
        }
        o.player["entity_id"] = json!(7);
        o
    }
    #[test]
    fn submerged_held_input_can_surface_without_a_sticky_ground_jump() {
        let mut o = input_prediction_fixture();
        o.player["in_water"] = json!(true);
        o.player["on_ground"] = json!(false);
        for row in o.world["blocks"].as_array_mut().unwrap() {
            row["block"] = json!(if row["position"][1] == 1 {
                "minecraft:water"
            } else {
                "minecraft:air"
            });
        }
        let frames = control::preview(&o, [0.5, 2., 0.5], false).unwrap();
        assert!(frames.len() < 100);
        assert!((frames.last().unwrap().0[1] - 2.).abs() < 0.08);
        o.player["velocity"] = json!([0.03, -0.02, 0.01]);
        assert!(control::preview(&o, [0.5, 2., 0.5], false).is_ok());
    }
    #[test]
    fn input_preview_rejects_the_native_diagonal_gap_that_constant_speed_admitted() {
        let o = diagonal_gap_fixture();
        assert!(
            control::preview(&o, [-1.5, 1., 1.5], true).is_err(),
            "A reachable constant-speed arc is not evidence that held-input acceleration can reach its landing"
        );
    }
    #[test]
    fn input_preview_can_steer_from_observed_airborne_momentum() {
        let mut o = input_prediction_fixture();
        o.player["position"] = json!([0.5, 1.3, 0.5]);
        o.player["velocity"] = json!([0.12, 0.3, 0.]);
        o.player["on_ground"] = json!(false);
        let frames = control::preview(&o, [2.5, 1., 1.5], false).unwrap();
        assert!(
            frames[0].0[1] > 1.3,
            "Native upward momentum is not an immediate forced descent"
        );
        let (last, ground) = frames.last().unwrap();
        assert!(*ground && distance(*last, [2.5, 1., 1.5]) < 0.08);
    }
    use std::net::TcpListener;

    fn wait_until(test: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(4);
        while !test() {
            assert!(
                Instant::now() < deadline,
                "physical worker did not reach the expected state"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Exercise the actual sampler, writer and input lease against framed TCP
    /// and a native-shaped HTTP observer; pure physics tests cannot prove this.
    #[test]
    fn actor_has_one_input_owner_releases_in_air_and_invalidates_on_correction() {
        let alive = Arc::new(AtomicBool::new(true));
        let mut blocks = Vec::new();
        for x in -4..=5 {
            for y in -2..=4 {
                for z in -4..=4 {
                    blocks.push(json!({"position":[x,y,z],"friction":0.6,"speed_factor":1.,"collision":if y == 0 {json!([[0.,0.,0.,1.,1.,1.]])}else{json!([])}}));
                }
            }
        }
        let observed = Arc::new(Mutex::new(Observation {
            revision: "0".into(),
            menu: Value::Null,
            player: json!({"uuid":"fixture","entity_id":7,"dimension":"minecraft:overworld","position":[0.5,1.,0.5],"velocity":[0.,0.,0.],"health":20.,"movement_speed":0.1,"on_ground":true,"physics":{"format":"java767-physics-v1","width":0.6,"height":1.8,"gravity":0.08,"jump_velocity":0.42,"step_height":0.6,"water_efficiency":0.,"levitation":-1}}),
            world: json!({"blocks":blocks,"sample_clock":{"epoch":"fixture","nanos":"1"}}),
        }));
        let http = TcpListener::bind("127.0.0.1:0").unwrap();
        http.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", http.local_addr().unwrap());
        let http_alive = alive.clone();
        let http_observed = observed.clone();
        let sampler = thread::spawn(move || {
            let mut serial = 1u64;
            while http_alive.load(Ordering::Acquire) {
                let Ok((mut stream, _)) = http.accept() else {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut headers = Vec::new();
                loop {
                    let mut byte = [0u8];
                    stream.read_exact(&mut byte).unwrap();
                    headers.push(byte[0]);
                    if headers.ends_with(b"\r\n\r\n") {
                        break;
                    }
                    assert!(headers.len() < 8192);
                }
                let headers = String::from_utf8(headers).unwrap();
                assert!(headers.starts_with("POST /v1/observe "));
                let size = headers
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                            .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                assert!(size < 1024);
                let mut body = vec![0u8; size];
                stream.read_exact(&mut body).unwrap();
                assert_eq!(
                    serde_json::from_slice::<Value>(&body).unwrap()["player_uuid"],
                    "fixture"
                );
                let body = {
                    let mut o = http_observed.lock().unwrap();
                    serial += 1;
                    o.revision = serial.to_string();
                    o.world["sample_clock"]["nanos"] = json!((serial * 150_000_000).to_string());
                    serde_json::to_vec(&*o).unwrap()
                };
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        let socket = TcpListener::bind("127.0.0.1:0").unwrap();
        let sender = TcpStream::connect(socket.local_addr().unwrap()).unwrap();
        let (receiver, _) = socket.accept().unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        let packets = Arc::new(AtomicU64::new(0));
        let received = packets.clone();
        let wire_observed = observed.clone();
        let reader = thread::spawn(move || {
            let mut wire = Wire::new(receiver);
            while let Ok(packet) = wire.receive() {
                assert_eq!(
                    packet.id,
                    Tables::new().id("play", "toServer", "position").unwrap()
                );
                assert_eq!(packet.payload.len(), 25);
                let mut p = [0.; 3];
                for (i, v) in p.iter_mut().enumerate() {
                    *v = f64::from_be_bytes(packet.payload[i * 8..i * 8 + 8].try_into().unwrap());
                }
                let mut o = wire_observed.lock().unwrap();
                o.player["position"] = json!(p);
                o.player["on_ground"] = json!(packet.payload[24] != 0);
                received.fetch_add(1, Ordering::Release);
            }
        });
        let corrections = Arc::new(AtomicU64::new(0));
        let correcting = Arc::new(AtomicBool::new(false));
        let native = Native {
            endpoint,
            token: "fixture-token".into(),
            http: reqwest::blocking::Client::builder()
                .no_proxy()
                .build()
                .unwrap(),
        };
        let writer = Arc::new(Mutex::new(Wire::new(sender)));
        let impulses = Arc::new(Mutex::new(VecDeque::new()));
        let actor = Actor::start(
            native,
            "fixture".into(),
            observed.lock().unwrap().clone(),
            Wires {
                writer: writer.clone(),
                tables: Tables::new(),
                alive: alive.clone(),
                corrections: corrections.clone(),
                correcting: correcting.clone(),
                ground: Arc::new(AtomicBool::new(true)),
                impulses: impulses.clone(),
                entity: Arc::new(AtomicI32::new(7)),
            },
        )
        .unwrap();
        wait_until(|| packets.load(Ordering::Acquire) >= 3);
        let unreachable = ActionRequest {
            action_id: "fixture-reject-before-input".into(),
            action: Action::StartMove {
                target: Position {
                    x: -1.5,
                    y: 1.,
                    z: 1.5,
                },
                jump: true,
                max_ticks: 100,
            },
        };
        let rejected_state = Arc::new(Mutex::new(Receipt {
            action_id: unreachable.action_id.clone(),
            outcome: Outcome::Running,
            adapter_version: VERSION.into(),
            evidence: json!({}),
            error: None,
        }));
        assert_eq!(
            actor
                .start_control(
                    diagonal_gap_fixture(),
                    &unreachable,
                    0,
                    rejected_state.clone(),
                    Arc::new(AtomicBool::new(false)),
                )
                .unwrap_err()
                .code,
            ErrorCode::UnsupportedCapability,
        );
        assert!(
            actor.engine.lock().unwrap().drive.is_none(),
            "Rejected admission cannot install input"
        );
        assert_eq!(rejected_state.lock().unwrap().evidence, json!({}));
        let request = ActionRequest {
            action_id: "fixture-expire".into(),
            action: Action::StartMove {
                target: Position {
                    x: 0.5,
                    y: 1.,
                    z: 0.5,
                },
                jump: true,
                max_ticks: 1,
            },
        };
        let state = Arc::new(Mutex::new(Receipt {
            action_id: request.action_id.clone(),
            outcome: Outcome::Running,
            adapter_version: VERSION.into(),
            evidence: json!({}),
            error: None,
        }));
        let before = actor.observe().unwrap();
        actor
            .start_control(
                before.clone(),
                &request,
                0,
                state.clone(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        assert_eq!(
            actor
                .start_control(
                    before,
                    &request,
                    0,
                    state.clone(),
                    Arc::new(AtomicBool::new(false))
                )
                .unwrap_err()
                .code,
            ErrorCode::Conflict
        );
        wait_until(|| state.lock().unwrap().outcome.terminal());
        assert_eq!(state.lock().unwrap().outcome, Outcome::Expired);
        assert!(
            state.lock().unwrap().evidence["after"]["player"]["position"][1]
                .as_f64()
                .unwrap()
                > 1.1,
            "release was verified while airborne"
        );
        wait_until(|| {
            let e = actor.engine.lock().unwrap();
            e.body.on_ground && e.body.position[1] == 1.
        });
        assert!(
            packets.load(Ordering::Acquire) >= 10,
            "the same physical writer continues after input release"
        );
        for (id, target, jump) in [
            ("fixture-arrive-walk", [2.5, 1., 1.5], false),
            ("fixture-arrive-jump", [0.5, 1., 0.5], true),
            ("fixture-cancel-momentum", [2.5, 1., 1.5], false),
        ] {
            let request = ActionRequest {
                action_id: id.into(),
                action: Action::StartMove {
                    target: Position {
                        x: target[0],
                        y: target[1],
                        z: target[2],
                    },
                    jump,
                    max_ticks: 100,
                },
            };
            let state = Arc::new(Mutex::new(Receipt {
                action_id: id.into(),
                outcome: Outcome::Running,
                adapter_version: VERSION.into(),
                evidence: json!({}),
                error: None,
            }));
            let cancel = Arc::new(AtomicBool::new(false));
            actor
                .start_control(
                    actor.observe().unwrap(),
                    &request,
                    0,
                    state.clone(),
                    cancel.clone(),
                )
                .unwrap();
            if matches!(id, "fixture-arrive-walk" | "fixture-cancel-momentum") {
                wait_until(|| {
                    state.lock().unwrap().evidence["progress"]["frames_sent"]
                        .as_u64()
                        .is_some_and(|n| n >= 2)
                });
                push_impulse(&impulses, Impulse::Set([0.12, 0.30, 0.])).unwrap();
                if id == "fixture-cancel-momentum" {
                    wait_until(|| {
                        state.lock().unwrap().evidence["server_momentum"]["applied_updates"] == 1
                    });
                    cancel.store(true, Ordering::Release);
                }
            }
            wait_until(|| state.lock().unwrap().outcome.terminal());
            let receipt = state.lock().unwrap();
            assert_eq!(
                receipt.outcome,
                if id == "fixture-cancel-momentum" {
                    Outcome::Cancelled
                } else {
                    Outcome::Observed
                },
                "{id}: {}",
                receipt.evidence
            );
            let actual = std::array::from_fn(|i| {
                receipt.evidence["after"]["player"]["position"][i]
                    .as_f64()
                    .unwrap()
            });
            if receipt.outcome == Outcome::Observed {
                assert!(
                    movement_target_reached(actual, target),
                    "Released native arrival must satisfy the public navigator's contract"
                );
            } else {
                assert!(
                    actual[1] > 1.1,
                    "Explicit cancellation must not erase native airborne momentum"
                );
            }
            assert_eq!(receipt.evidence["target"], json!(target));
            assert_eq!(receipt.evidence["input_released"], true);
            if matches!(id, "fixture-arrive-walk" | "fixture-cancel-momentum") {
                assert_eq!(receipt.evidence["server_momentum"]["applied_updates"], 1);
                assert_eq!(
                    receipt.evidence["server_momentum"]["samples"][0]["velocity"],
                    json!([0.12, 0.30, 0.])
                );
                assert_eq!(
                    receipt.evidence["server_momentum"]["samples"][0]["input_held"],
                    true
                );
            }
        }
        let request = ActionRequest {
            action_id: "fixture-correct".into(),
            action: Action::StartMove {
                target: Position {
                    x: 3.,
                    y: 1.,
                    z: 0.5,
                },
                jump: true,
                max_ticks: 60,
            },
        };
        let state = Arc::new(Mutex::new(Receipt {
            action_id: request.action_id.clone(),
            outcome: Outcome::Running,
            adapter_version: VERSION.into(),
            evidence: json!({}),
            error: None,
        }));
        actor
            .start_control(
                actor.observe().unwrap(),
                &request,
                0,
                state.clone(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        correcting.store(true, Ordering::Release);
        corrections.fetch_add(1, Ordering::AcqRel);
        thread::sleep(Duration::from_millis(100));
        let count = packets.load(Ordering::Acquire);
        thread::sleep(Duration::from_millis(120));
        assert_eq!(
            packets.load(Ordering::Acquire),
            count,
            "no position writes during native correction"
        );
        observed.lock().unwrap().player["position"] = json!([1.5, 1., 0.5]);
        correcting.store(false, Ordering::Release);
        wait_until(|| state.lock().unwrap().outcome == Outcome::Uncertain);
        assert_eq!(state.lock().unwrap().outcome, Outcome::Uncertain);
        assert!(
            actor.engine.lock().unwrap().drive.is_none(),
            "interrupted input must not replay"
        );
        observed.lock().unwrap().world["blocks"] = json!([]);
        wait_until(|| !alive.load(Ordering::Acquire));
        assert!(
            actor
                .engine
                .lock()
                .unwrap()
                .reason
                .contains("unobserved terrain")
        );
        // The writer closes its socket on loss of native geometry. No orphan
        // sampler or packet-reader may survive the fixture's explicit shutdown.
        reader.join().unwrap();
        sampler.join().unwrap();
        assert!(actor.observe().is_err());
    }
    #[test]
    fn native_impulses_are_actor_scoped_bounded_and_keep_packet_order() {
        let mut bytes = vi(7);
        for v in [8000i16, -4000, 2000] {
            bytes.extend(v.to_be_bytes());
        }
        assert!(entity_impulse(&bytes, 8).unwrap().is_none());
        assert!(matches!(
            entity_impulse(&bytes, 7).unwrap(),
            Some(Impulse::Set([1., -0.5, 0.25]))
        ));
        assert!(entity_impulse(&bytes[..3], 7).is_err());
        let inbox = Arc::new(Mutex::new(VecDeque::new()));
        push_impulse(&inbox, Impulse::Set([1.; 3])).unwrap();
        for _ in 0..10000 {
            push_impulse(&inbox, Impulse::Add([0.001; 3])).unwrap();
        }
        assert_eq!(inbox.lock().unwrap().len(), 1);
        push_impulse(&inbox, Impulse::Set([2.; 3])).unwrap();
        push_impulse(&inbox, Impulse::Add([0.25; 3])).unwrap();
        assert!(matches!(
            inbox.lock().unwrap()[0],
            Impulse::Set([2.25, 2.25, 2.25])
        ));
    }
    #[test]
    fn explosion_offsets_and_nonfinite_momentum_are_validated() {
        let mut bytes = vec![0u8; 28];
        bytes.extend(vi(2));
        bytes.extend([0u8; 6]);
        for n in [0.25f32, 0.5, -0.75] {
            bytes.extend(n.to_be_bytes());
        }
        assert!(matches!(
            explosion_impulse(&bytes).unwrap(),
            Impulse::Add([0.25, 0.5, -0.75])
        ));
        assert!(explosion_impulse(&bytes[..bytes.len() - 1]).is_err());
        let offset = bytes.len() - 4;
        bytes[offset..].copy_from_slice(&f32::NAN.to_be_bytes());
        assert!(explosion_impulse(&bytes).is_err());
    }
    #[test]
    fn effect_freshness_rejects_foreign_or_older_samples_and_preserves_clock_wrap() {
        let before = Observation {
            revision: "one".into(),
            player: json!({"uuid":"own","dimension":"mod:world"}),
            world: json!({"sample_clock":{"epoch":"clock","nanos":(u64::MAX-10).to_string()}}),
            menu: Value::Null,
        };
        let mut after = before.clone();
        after.revision = "two".into();
        after.world["sample_clock"]["nanos"] = json!("5");
        assert!(newer_sample(&before, &after));
        after.player["uuid"] = json!("other");
        assert!(!newer_sample(&before, &after));
        after.player["uuid"] = json!("own");
        after.world["sample_clock"]["epoch"] = json!("restarted");
        assert!(!newer_sample(&before, &after));
    }
}
