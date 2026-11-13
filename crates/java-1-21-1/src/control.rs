//! Finite input ownership over a continuously simulated player connection.
use super::*;
type Frame = ([f64; 3], bool);
pub(super) fn preview(before: &Observation, target: [f64; 3], jump: bool) -> AnyResult<Vec<Frame>> {
    // The same held-input model drives admission, queries and execution.
    // Future collisions, damage and explicit cancellation remain game feedback.
    motion::preview(before, target, jump)
}
pub(super) struct FirstEffectContext {
    pub clock: Value,
    pub uuid: String,
    pub dimension: Value,
    pub initial: [f64; 3],
    pub frames: Vec<Frame>,
    pub corrections: Arc<AtomicU64>,
    pub epoch: u64,
}
pub(super) fn first_effect_sample(
    ctx: &FirstEffectContext,
    after: &Observation,
    sent: u32,
) -> Option<Value> {
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
        json!({"verified":true,"pending":false,"sample_clock":after.world["sample_clock"],"revision":after.revision,"position":p,"matched_frame":matched+1,"native_admission_to_sample_ns":elapsed.to_string(),"scope":"First observed pose matching a submitted frame; target completion is separate"}),
    )
}
pub struct Job {
    pub id: String,
    cancel_id: Option<String>,
    cancel: Arc<AtomicBool>,
    state: Arc<Mutex<Receipt>>,
    reconciled: AtomicBool,
}
impl Job {
    pub fn observation(&self) -> Value {
        self.state.lock().map(|r| {
            let mut observation=json!({"action_id":self.id,"outcome":r.outcome,"target":r.evidence["target"],"progress":r.evidence["progress"],"cancel_requested":self.cancel.load(Ordering::Acquire),"input_released":r.evidence["input_released"]});
            if self.reconciled.load(Ordering::Acquire)
                && r.outcome==Outcome::Uncertain && r.evidence["input_released"]==true {
                // This is present scheduling readiness, not a rewrite of the
                // original action's uncertain historical game effect.
                observation["outcome"]=json!("idle");
                observation["historical_outcome"]=json!(r.outcome);
                observation["reconciliation"]=json!({"settled":true,"historical_effect_verified":false,"replayed":false});
            }
            observation
        }).unwrap_or_else(|_|json!({"action_id":self.id,"outcome":"uncertain","error":"Control state unavailable"}))
    }
    pub fn acknowledge_reconciliation(&self, id: &str) {
        if (id == self.id || self.cancel_id.as_deref() == Some(id))
            && self.state.lock().is_ok_and(|r| {
                r.outcome == Outcome::Uncertain && r.evidence["input_released"] == true
            })
        {
            // Called only after settled native reconciliation. Retain status
            // and its evidence exactly; expose that this stopped lane is free.
            self.reconciled.store(true, Ordering::Release);
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
            // Cancellation certifies released input, independently of whether
            // the interrupted waypoint was reached or passed.
            let released = r.outcome.terminal() && r.evidence["input_released"] == true;
            r.evidence = json!({"control_action_id":self.id,"control_outcome":r.outcome,"control":r.evidence,"native_effect_verified":released});
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
        _native: Native,
        before: Observation,
        request: &ActionRequest,
        epoch: u64,
    ) -> Result<Self> {
        let (Action::StartMove {
            target, max_ticks, ..
        }
        | Action::StartMoveThrough { target, max_ticks }) = &request.action
        else {
            return Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "Finite movement required",
            ));
        };
        if player.correcting.load(Ordering::Acquire)
            || player.corrections.load(Ordering::Acquire) != epoch
        {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "Server corrected this character; observe fresh state before moving",
            ));
        }
        let state = Arc::new(Mutex::new(Receipt {
            action_id: request.action_id.clone(),
            outcome: Outcome::Running,
            adapter_version: VERSION.into(),
            evidence: json!({"before":compact(&request.action,&before),"native_effect_verified":false,"target":[target.x,target.y,target.z],"progress":{"frames_sent":0},"max_ticks":max_ticks,"first_effect":{"verified":false,"pending":true}}),
            error: None,
        }));
        let cancel = Arc::new(AtomicBool::new(false));
        player
            .physics
            .start_control(before, request, epoch, state.clone(), cancel.clone())?;
        Ok(Self {
            id: request.action_id.clone(),
            cancel_id: None,
            cancel,
            state,
            reconciled: AtomicBool::new(false),
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn interrupted_job(outcome: Outcome, input_released: bool) -> Job {
        Job {
            id: "move-original".into(),
            cancel_id: Some("cancel-original".into()),
            cancel: Arc::new(AtomicBool::new(false)),
            reconciled: AtomicBool::new(false),
            state: Arc::new(Mutex::new(Receipt {
                action_id: "move-original".into(),
                outcome,
                adapter_version: VERSION.into(),
                evidence: json!({"input_released":input_released,"native_effect_verified":false,
                    "target":[2.5,64.,1.5],"progress":{"frames_sent":3}}),
                error: Some(ApiError::new(
                    ErrorCode::UncertainOutcome,
                    "Inspect without replay",
                )),
            })),
        }
    }
    #[test]
    fn explicit_reconciliation_frees_a_stopped_lane_without_rewriting_historical_effects() {
        for id in ["move-original", "cancel-original"] {
            let job = interrupted_job(Outcome::Uncertain, true);
            let status_before = serde_json::to_value(job.status("move-original").unwrap()).unwrap();
            let cancel_before =
                serde_json::to_value(job.status("cancel-original").unwrap()).unwrap();
            assert_eq!(job.observation()["outcome"], "uncertain");
            job.acknowledge_reconciliation("foreign");
            assert_eq!(job.observation()["outcome"], "uncertain");
            assert!(
                !job.cancel.load(Ordering::Acquire),
                "reconciliation cannot submit cancellation input"
            );
            job.acknowledge_reconciliation(id);
            job.acknowledge_reconciliation(id);
            let observed = job.observation();
            assert_eq!(observed["action_id"], "move-original");
            assert_eq!(
                observed["outcome"], "idle",
                "existing navigation readiness accepts a settled idle lane"
            );
            assert_eq!(observed["historical_outcome"], "uncertain");
            assert_eq!(observed["input_released"], true);
            assert_eq!(
                observed["reconciliation"]["historical_effect_verified"],
                false
            );
            assert_eq!(observed["reconciliation"]["replayed"], false);
            assert_eq!(
                serde_json::to_value(job.status("move-original").unwrap()).unwrap(),
                status_before
            );
            assert_eq!(
                serde_json::to_value(job.status("cancel-original").unwrap()).unwrap(),
                cancel_before
            );
            assert!(!job.running());
            assert!(!job.cancel.load(Ordering::Acquire));
        }
    }
    #[test]
    fn reconciliation_cannot_free_running_unreleased_or_unrelated_controls() {
        for outcome in [
            Outcome::Prepared,
            Outcome::Running,
            Outcome::Observed,
            Outcome::Cancelled,
            Outcome::Expired,
            Outcome::Failed,
        ] {
            let job = interrupted_job(outcome, true);
            let before = job.observation();
            job.acknowledge_reconciliation("move-original");
            assert_eq!(job.observation(), before);
            assert!(!job.reconciled.load(Ordering::Acquire));
        }
        let job = interrupted_job(Outcome::Uncertain, false);
        job.acknowledge_reconciliation("move-original");
        assert_eq!(job.observation()["outcome"], "uncertain");
        assert!(!job.reconciled.load(Ordering::Acquire));
        let job = interrupted_job(Outcome::Running, true);
        job.acknowledge_reconciliation("move-original");
        job.state.lock().unwrap().outcome = Outcome::Uncertain;
        assert_eq!(
            job.observation()["outcome"],
            "uncertain",
            "a premature acknowledgment is never queued for later"
        );
    }
    fn sample() -> (FirstEffectContext, Observation) {
        (
            FirstEffectContext {
                clock: json!({"epoch":"clock","nanos":"100"}),
                uuid: "own".into(),
                dimension: json!("minecraft:overworld"),
                initial: [0.; 3],
                frames: vec![([0.2, 0., 0.], true), ([0.4, 0., 0.], true)],
                corrections: Arc::new(AtomicU64::new(7)),
                epoch: 7,
            },
            Observation {
                revision: "2".into(),
                player: json!({"uuid":"own","dimension":"minecraft:overworld","position":[0.2,0.,0.]}),
                world: json!({"sample_clock":{"epoch":"clock","nanos":"120"}}),
                menu: Value::Null,
            },
        )
    }
    #[test]
    fn first_effect_requires_submitted_native_displacement_and_stable_binding() {
        let (ctx, mut after) = sample();
        assert_eq!(
            first_effect_sample(&ctx, &after, 1).unwrap()["native_admission_to_sample_ns"],
            "20"
        );
        after.player["position"] = json!([0.4, 0., 0.]);
        assert!(first_effect_sample(&ctx, &after, 1).is_none());
        after.player["position"] = json!([0., 0., 0.]);
        assert!(first_effect_sample(&ctx, &after, 2).is_none());
        for (field, value) in [("uuid", "other"), ("dimension", "other:dimension")] {
            let (_, mut after) = sample();
            after.player[field] = json!(value);
            assert!(first_effect_sample(&ctx, &after, 2).is_none());
        }
        let (_, mut after) = sample();
        after.world["sample_clock"]["epoch"] = json!("new");
        assert!(first_effect_sample(&ctx, &after, 2).is_none());
        let (_, after) = sample();
        ctx.corrections.fetch_add(1, Ordering::AcqRel);
        assert!(first_effect_sample(&ctx, &after, 2).is_none());
    }
    #[test]
    fn native_nanosecond_wrap_and_precision_are_preserved() {
        let (mut ctx, mut after) = sample();
        ctx.clock["nanos"] = json!((u64::MAX - 10).to_string());
        after.world["sample_clock"]["nanos"] = json!("5");
        assert_eq!(
            first_effect_sample(&ctx, &after, 1).unwrap()["native_admission_to_sample_ns"],
            "16"
        );
        after.world["sample_clock"]["nanos"] = json!(5);
        assert!(first_effect_sample(&ctx, &after, 1).is_none());
    }
    #[test]
    fn cancellation_ids_cannot_take_over_another_control_or_certify_an_unverified_release() {
        let mut job = Job {
            id: "own".into(),
            cancel_id: None,
            cancel: Arc::new(AtomicBool::new(false)),
            reconciled: AtomicBool::new(false),
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
            job.cancel("again", "own").unwrap_err().code,
            ErrorCode::Conflict
        );
        assert!(job.status("foreign").is_err());
        {
            let mut r = job.state.lock().unwrap();
            r.outcome = Outcome::Cancelled;
        }
        assert_eq!(job.status("cancel").unwrap().outcome, Outcome::Failed);
        {
            let mut r = job.state.lock().unwrap();
            r.evidence["native_effect_verified"] = json!(true);
        }
        assert_eq!(job.status("cancel").unwrap().outcome, Outcome::Failed);
        {
            let mut r = job.state.lock().unwrap();
            r.evidence["native_effect_verified"] = json!(false);
            r.evidence["input_released"] = json!(true);
        }
        assert_eq!(job.status("cancel").unwrap().outcome, Outcome::Observed);
        assert_eq!(
            job.status("own").unwrap().evidence["native_effect_verified"],
            false
        );
    }
}
