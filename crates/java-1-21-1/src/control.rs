//! Finite client-side locomotion. Ordinary player packets remain authoritative.
use super::*;
use std::collections::VecDeque;

type Frame = ([f64; 3], bool);
struct FirstEffectContext {
    clock: Value,
    uuid: String,
    dimension: Value,
    initial: [f64; 3],
    frames: Vec<Frame>,
    corrections: Arc<AtomicU64>,
    epoch: u64,
}
fn first_effect_sample(ctx: &FirstEffectContext, after: &Observation, sent: u32) -> Option<Value> {
    if ctx.corrections.load(Ordering::Acquire) != ctx.epoch
        || after.player["uuid"] != ctx.uuid
        || after.player["dimension"] != ctx.dimension
        || ctx.clock["epoch"].as_str()? != after.world["sample_clock"]["epoch"].as_str()?
    {
        return None;
    }
    let start: u64 = ctx.clock["nanos"].as_str()?.parse().ok()?;
    let end: u64 = after.world["sample_clock"]["nanos"]
        .as_str()?
        .parse()
        .ok()?;
    let elapsed = end.wrapping_sub(start);
    if elapsed == 0 || elapsed > 2_000_000_000 {
        return None;
    }
    let p = position(after).ok()?;
    if (0..3).all(|i| (p[i] - ctx.initial[i]).abs() < 0.08) {
        return None;
    }
    let matched = ctx
        .frames
        .iter()
        .take(sent as usize)
        .position(|(expected, _)| (0..3).all(|i| (p[i] - expected[i]).abs() < 0.08))?;
    Some(
        json!({"verified":true,"pending":false,"sample_clock":after.world["sample_clock"],
        "revision":after.revision,"position":p,"matched_frame":matched+1,
        "native_admission_to_sample_ns":elapsed.to_string(),
        "scope":"First observed pose matching a submitted frame; target completion is separate"}),
    )
}
fn observe_first_effect(native: Native, ctx: FirstEffectContext, state: Arc<Mutex<Receipt>>) {
    let mut effect = json!({"verified":false,"pending":false,"reason":"No submitted displacement verified within bounded native samples"});
    for _ in 0..3 {
        if ctx.corrections.load(Ordering::Acquire) != ctx.epoch {
            effect["reason"] = json!("Control interrupted by server correction");
            break;
        }
        if let Ok(after) = native.observe_bounded(&ctx.uuid, Duration::from_millis(200)) {
            let sent = state
                .lock()
                .ok()
                .and_then(|r| r.evidence["progress"]["frames_sent"].as_u64())
                .unwrap_or(0);
            if let Some(verified) = first_effect_sample(&ctx, &after, sent as u32) {
                effect = verified;
                break;
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
    if let Ok(mut receipt) = state.lock() {
        receipt.evidence["first_effect"] = effect;
    }
}
struct Plan {
    frames: VecDeque<Frame>,
    boxes: Vec<[f64; 6]>,
    current: [f64; 3],
    velocity: f64,
    ground: bool,
    floor: f64,
    outcome: Option<Outcome>,
}
impl Plan {
    fn new(before: &Observation, target: [f64; 3], jump: bool) -> AnyResult<Self> {
        let frames = motion_plan(before, target, jump)?;
        let boxes = collision_boxes(before)?;
        let start = position(before)?;
        let mut previous = start;
        // Every admitted frame needs an observed gravity-only release path.
        // Unknown terrain cannot silently become air or a floating stop.
        for &(p, ground) in &frames {
            Self::landing(
                p,
                ground,
                (p[1] - previous[1] - 0.08) * 0.98,
                start[1] - 3.,
                &boxes,
            )?;
            previous = p;
        }
        Ok(Self {
            frames: frames.into(),
            boxes,
            current: start,
            velocity: -0.08,
            ground: before.player["on_ground"] == true,
            floor: start[1] - 3.,
            outcome: None,
        })
    }
    fn landing(
        mut p: [f64; 3],
        mut ground: bool,
        mut velocity: f64,
        floor: f64,
        boxes: &[[f64; 6]],
    ) -> AnyResult<VecDeque<Frame>> {
        let mut frames = VecDeque::new();
        for _ in 0..60 {
            if ground {
                return Ok(frames);
            }
            let (next, contact) =
                minecraft_sdk_java_1_21_1::physics::step(p, [0.; 3], velocity, boxes);
            if next[1] < floor {
                return Err("Control has no observed gravity-safe cancellation path".into());
            }
            let clipped = (next[1] - p[1] - velocity).abs() > 1e-7;
            velocity = if contact {
                -0.08
            } else {
                ((if clipped { 0. } else { velocity }) - 0.08) * 0.98
            };
            p = next;
            ground = contact;
            frames.push_back((p, ground));
        }
        Err("Control release did not land within observed terrain".into())
    }
    fn tick(&mut self, cancel: bool, expired: bool) -> AnyResult<Option<Frame>> {
        if self.outcome.is_none() && (cancel || expired) {
            self.outcome = Some(if cancel {
                Outcome::Cancelled
            } else {
                Outcome::Expired
            });
            self.frames = Self::landing(
                self.current,
                self.ground,
                self.velocity,
                self.floor,
                &self.boxes,
            )?;
        }
        let next = self.frames.pop_front();
        if let Some((p, ground)) = next {
            let delta = p[1] - self.current[1];
            self.velocity = if ground { -0.08 } else { (delta - 0.08) * 0.98 };
            self.current = p;
            self.ground = ground;
        } else if self.outcome.is_none() {
            self.outcome = Some(Outcome::Observed);
        }
        Ok(next)
    }
}
pub struct Job {
    pub id: String,
    cancel_id: Option<String>,
    cancel: Arc<AtomicBool>,
    state: Arc<Mutex<Receipt>>,
}
impl Job {
    pub fn observation(&self) -> Value {
        match self.state.lock() {
            Ok(r) => {
                json!({"action_id":self.id,"outcome":r.outcome,"target":r.evidence["target"],"progress":r.evidence["progress"],"cancel_requested":self.cancel.load(Ordering::Acquire),"input_released":r.evidence["input_released"]})
            }
            Err(_) => {
                json!({"action_id":self.id,"outcome":"uncertain","error":"Control state unavailable"})
            }
        }
    }
    pub fn running(&self) -> bool {
        self.state
            .lock()
            .map_or(true, |r| r.outcome == Outcome::Running)
    }
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    pub fn status(&self, id: &str) -> Result<Receipt> {
        let mut r = self
            .state
            .lock()
            .map_err(|_| ApiError::new(ErrorCode::Internal, "Control state unavailable"))?
            .clone();
        if id == self.id {
            return Ok(r);
        }
        if self.cancel_id.as_deref() == Some(id) {
            r.evidence = json!({"control_action_id":self.id,"control_outcome":r.outcome,"control":r.evidence,
                "native_effect_verified":r.outcome.terminal() && r.evidence["native_effect_verified"]==true});
            r.action_id = id.into();
            if r.outcome.terminal() {
                if r.evidence["native_effect_verified"] == true {
                    r.outcome = Outcome::Observed;
                    r.error = None;
                } else {
                    r.outcome = Outcome::Failed;
                    r.error = Some(ApiError::new(
                        ErrorCode::Conflict,
                        "Control ended but native release effect was not verified",
                    ));
                }
            }
            return Ok(r);
        }
        Err(ApiError::new(
            ErrorCode::NotFound,
            "Control ID is not active in this character lane",
        ))
    }
    pub fn cancel(&mut self, id: &str, target: &str) -> Result<Receipt> {
        if target != self.id {
            return Err(ApiError::new(
                ErrorCode::NotFound,
                "Unknown control action ID",
            ));
        }
        if self.cancel_id.as_deref().is_some_and(|old| old != id) {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "Inspect the existing cancellation before another request",
            ));
        }
        self.cancel_id = Some(id.into());
        self.stop();
        self.status(id)
    }
    pub fn start(
        player: &Player,
        native: Native,
        before: Observation,
        request: &ActionRequest,
        correction_epoch: u64,
    ) -> Result<Self> {
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
        let (target, jump, max_ticks) = ([target.x, target.y, target.z], *jump, *max_ticks);
        if player.correcting.load(Ordering::Acquire)
            || player.corrections.load(Ordering::Acquire) != correction_epoch
        {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "Server corrected this character; observe fresh state before moving",
            ));
        }
        let mut plan = Plan::new(&before, target, jump)
            .map_err(|e| ApiError::new(ErrorCode::UnsupportedCapability, e.to_string()))?;
        let cancel = Arc::new(AtomicBool::new(false));
        let initial = Receipt {
            action_id: request.action_id.clone(),
            outcome: Outcome::Running,
            adapter_version: VERSION.into(),
            evidence: json!({"before":compact(&request.action,&before),"native_effect_verified":false,"target":target,"progress":{"frames_sent":0},"max_ticks":max_ticks,
                "first_effect":{"verified":false,"pending":false,"reason":"No submitted displacement sampled"}}),
            error: None,
        };
        let state = Arc::new(Mutex::new(initial));
        let job = Self {
            id: request.action_id.clone(),
            cancel_id: None,
            cancel: cancel.clone(),
            state: state.clone(),
        };
        let writer = player.wire.clone();
        let alive = player.alive.clone();
        let corrections = player.corrections.clone();
        let correcting = player.correcting.clone();
        let tables = player.tables.clone();
        let uuid = before.player["uuid"]
            .as_str()
            .ok_or_else(|| ApiError::new(ErrorCode::NotReady, "Player UUID unavailable"))?
            .to_owned();
        let action = request.action.clone();
        let expected_frames: Vec<_> = plan.frames.iter().copied().collect();
        let initial_position = plan.current;
        thread::spawn(move || {
            let started = Instant::now();
            let expiry = Duration::from_millis(u64::from(max_ticks) * 50);
            let mut sent = 0u32;
            let mut first_check = None;
            let mut run = || -> AnyResult<(Outcome, Observation, bool)> {
                loop {
                    if !alive.load(Ordering::Acquire) {
                        return Err("Player connection interrupted".into());
                    }
                    let Some((p, ground)) =
                        plan.tick(cancel.load(Ordering::Acquire), started.elapsed() >= expiry)?
                    else {
                        break;
                    };
                    let mut bytes = Vec::with_capacity(25);
                    for v in p {
                        bytes.extend(v.to_be_bytes());
                    }
                    bytes.push(u8::from(ground));
                    {
                        let mut writer = writer.lock().map_err(|_| "Player writer unavailable")?;
                        // The correction handler changes this epoch under the
                        // same wire lock before acknowledging a relocation.
                        // Never send a stale trajectory into the new location.
                        if correcting.load(Ordering::Acquire)
                            || corrections.load(Ordering::Acquire) != correction_epoch
                        {
                            return Err(
                                "Server position correction interrupted this control".into()
                            );
                        }
                        tables.send(&mut writer, "play", "position", &bytes)?;
                    }
                    sent += 1;
                    if let Ok(mut r) = state.lock() {
                        r.evidence["progress"] = json!({"frames_sent":sent,"commanded_position":p,"elapsed_ms":started.elapsed().as_millis(),"releasing":plan.outcome.is_some()});
                    }
                    if sent == 1 && before.world["sample_clock"].is_object() {
                        let ctx = FirstEffectContext {
                            clock: before.world["sample_clock"].clone(),
                            uuid: uuid.clone(),
                            dimension: before.player["dimension"].clone(),
                            initial: initial_position,
                            frames: expected_frames.clone(),
                            corrections: corrections.clone(),
                            epoch: correction_epoch,
                        };
                        let observer = native.clone();
                        let progress = state.clone();
                        if let Ok(mut r) = state.lock() {
                            r.evidence["first_effect"]["pending"] = json!(true);
                        }
                        // Verification never delays submission of the remaining
                        // movement frames. Join only after input is released.
                        first_check = Some(thread::spawn(move || {
                            observe_first_effect(observer, ctx, progress)
                        }));
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                let after = native.observe(&uuid)?;
                let verified = position(&after)
                    .is_ok_and(|p| (0..3).all(|i| (p[i] - plan.current[i]).abs() < 0.08));
                let mut outcome = plan.outcome.clone().ok_or("Control termination missing")?;
                if outcome == Outcome::Observed && !verified {
                    outcome = Outcome::Failed;
                }
                Ok((outcome, after, verified))
            };
            let result = run();
            if let Some(check) = first_check
                && check.join().is_err()
                && let Ok(mut r) = state.lock()
            {
                r.evidence["first_effect"] = json!({"verified":false,"pending":false,"reason":"Native first-effect observer failed"});
            }
            if let Ok(mut r) = state.lock() {
                let first_effect = r.evidence["first_effect"].clone();
                match result {
                    Ok((outcome, after, verified)) => {
                        r.error = if outcome == Outcome::Failed {
                            Some(ApiError::new(
                                ErrorCode::Conflict,
                                "Control ended without reaching its observed target",
                            ))
                        } else {
                            None
                        };
                        r.outcome = outcome;
                        r.evidence = json!({"before":compact(&action,&before),"after":compact(&action,&after),"native_effect_verified":verified,"input_released":true,"frames_sent":sent,"elapsed_ms":started.elapsed().as_millis(),"last_commanded_position":plan.current,"first_effect":first_effect});
                    }
                    Err(e) => {
                        r.outcome = Outcome::Uncertain;
                        r.error = Some(ApiError::new(
                            ErrorCode::UncertainOutcome,
                            format!(
                                "Control verification interrupted: {e}; reconcile without replay"
                            ),
                        ));
                        r.evidence["input_released"] = json!(true);
                    }
                }
            }
        });
        Ok(job)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn first_sample_fixture() -> (FirstEffectContext, Observation) {
        let ctx = FirstEffectContext {
            clock: json!({"epoch":"clock","nanos":"100"}),
            uuid: "own".into(),
            dimension: json!("minecraft:overworld"),
            initial: [0.; 3],
            frames: vec![([0.2, 0., 0.], true), ([0.4, 0., 0.], true)],
            corrections: Arc::new(AtomicU64::new(7)),
            epoch: 7,
        };
        let after = Observation {
            revision: "2".into(),
            player: json!({"uuid":"own","dimension":"minecraft:overworld","position":[0.2,0.,0.]}),
            world: json!({"sample_clock":{"epoch":"clock","nanos":"120"}}),
            menu: Value::Null,
        };
        (ctx, after)
    }
    #[test]
    fn first_effect_requires_an_observed_submitted_displacement() {
        let (ctx, mut after) = first_sample_fixture();
        let sample = first_effect_sample(&ctx, &after, 1).unwrap();
        assert_eq!(sample["matched_frame"], 1);
        assert_eq!(sample["native_admission_to_sample_ns"], "20");
        after.player["position"] = json!([0.4, 0., 0.]);
        assert!(
            first_effect_sample(&ctx, &after, 1).is_none(),
            "unsent frame is not evidence"
        );
        after.player["position"] = json!([0., 0., 0.]);
        assert!(
            first_effect_sample(&ctx, &after, 2).is_none(),
            "unchanged pose is not movement"
        );
    }
    #[test]
    fn first_effect_rejects_changed_actor_dimension_clock_or_correction() {
        let (ctx, after) = first_sample_fixture();
        for (field, value) in [("uuid", "other"), ("dimension", "minecraft:the_nether")] {
            let mut changed = after.clone();
            changed.player[field] = json!(value);
            assert!(first_effect_sample(&ctx, &changed, 1).is_none());
        }
        let mut changed = after.clone();
        changed.world["sample_clock"]["epoch"] = json!("restarted");
        assert!(first_effect_sample(&ctx, &changed, 1).is_none());
        ctx.corrections.fetch_add(1, Ordering::AcqRel);
        assert!(first_effect_sample(&ctx, &after, 1).is_none());
    }
    #[test]
    fn native_clock_preserves_unsigned_nanoseconds_and_wraps_safely() {
        let (mut ctx, mut after) = first_sample_fixture();
        ctx.clock["nanos"] = json!((u64::MAX - 10).to_string());
        after.world["sample_clock"]["nanos"] = json!("5");
        assert_eq!(
            first_effect_sample(&ctx, &after, 1).unwrap()["native_admission_to_sample_ns"],
            "16"
        );
        after.world["sample_clock"]["nanos"] = json!(5);
        assert!(
            first_effect_sample(&ctx, &after, 1).is_none(),
            "numeric JSON loses clock precision"
        );
    }
    #[test]
    fn control_and_cancellation_ids_cannot_select_another_action() {
        let mut job = Job {
            id: "own".into(),
            cancel_id: None,
            cancel: Arc::new(AtomicBool::new(false)),
            state: Arc::new(Mutex::new(Receipt {
                action_id: "own".into(),
                outcome: Outcome::Running,
                adapter_version: VERSION.into(),
                evidence: json!({"native_effect_verified":false}),
                error: None,
            })),
        };
        assert_eq!(
            job.cancel("cancel", "other").unwrap_err().code,
            ErrorCode::NotFound
        );
        assert_eq!(
            job.cancel("cancel", "own").unwrap().outcome,
            Outcome::Running
        );
        assert!(job.cancel.load(Ordering::Acquire));
        assert_eq!(
            job.cancel("competing-cancel", "own").unwrap_err().code,
            ErrorCode::Conflict
        );
        assert_eq!(job.status("foreign").unwrap_err().code, ErrorCode::NotFound);
        let mut state = job.state.lock().unwrap();
        state.outcome = Outcome::Cancelled;
        state.evidence = json!({"native_effect_verified":true,"input_released":true});
        drop(state);
        assert_eq!(job.status("own").unwrap().outcome, Outcome::Cancelled);
        let cancelled = job.status("cancel").unwrap();
        assert_eq!(cancelled.outcome, Outcome::Observed);
        assert_eq!(cancelled.evidence["native_effect_verified"], true);
    }
    fn observation() -> Observation {
        Observation {
            revision: "fixture".into(),
            player: json!({"position":[0.5,1.,0.5],"on_ground":true}),
            menu: Value::Null,
            world: json!({"blocks":[{"position":[-4,0,-4],"collision":[[0.,0.,0.,8.,1.,8.]]}]}),
        }
    }
    #[test]
    fn cancellation_releases_horizontal_motion_and_lands_under_gravity() {
        let mut plan = Plan::new(&observation(), [2.5, 1., 0.5], true).unwrap();
        for _ in 0..3 {
            plan.tick(false, false).unwrap();
        }
        let x = plan.current[0];
        assert!(plan.current[1] > 1.);
        let mut ticks = 0;
        while let Some((p, _)) = plan.tick(true, false).unwrap() {
            assert_eq!(p[0], x);
            ticks += 1;
            assert!(ticks < 60);
        }
        assert_eq!(plan.outcome, Some(Outcome::Cancelled));
        assert!(plan.ground);
        assert!((plan.current[1] - 1.).abs() < 1e-8);
    }
    #[test]
    fn expiry_is_terminal_without_becoming_uncertain_or_reaching_the_target() {
        let mut plan = Plan::new(&observation(), [2.5, 1., 0.5], false).unwrap();
        plan.tick(false, false).unwrap();
        assert!(plan.tick(false, true).unwrap().is_none());
        assert_eq!(plan.outcome, Some(Outcome::Expired));
        assert!(plan.current[0] < 2.);
    }
    #[test]
    fn unobserved_release_terrain_is_not_admitted() {
        let mut o = observation();
        o.world["blocks"][0]["collision"] = json!([[0., 0., 0., 4.8, 1., 8.]]);
        assert!(Plan::new(&o, [2.5, 1., 0.5], true).is_err());
    }
}
