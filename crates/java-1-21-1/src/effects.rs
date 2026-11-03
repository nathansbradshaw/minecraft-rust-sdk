//! Bounded observation of one dispatched interaction. Never retransmits input.
use super::*;

pub(super) fn managed(action: &Action) -> bool {
    matches!(
        action,
        Action::UseItem { .. }
            | Action::StartUseItem { .. }
            | Action::Attack { .. }
            | Action::Interact { .. }
            | Action::Dig { .. }
            | Action::Place { .. }
            | Action::UseBlock { .. }
            | Action::ClickSlot { .. }
            | Action::Craft { .. }
            | Action::DropSelected { .. }
    )
}
fn held(action: &Action) -> bool {
    matches!(action, Action::UseItem { .. } | Action::StartUseItem { .. })
}
fn held_stack<'a>(o: &'a Observation, hand: &Hand) -> Option<&'a Value> {
    let slot = match hand {
        Hand::Main => o.player["selected"].as_u64()?,
        Hand::Off => 40,
    };
    o.player["inventory"]
        .as_array()?
        .iter()
        .find(|row| row["slot"].as_u64() == Some(slot))
}
pub(super) fn validate_item_use(action: &Action, before: &Observation) -> Result<()> {
    if let Action::UseItem { hand } | Action::StartUseItem { hand, .. } = action {
        if before.player["using_item"] == true {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "Inspect or release the existing item use first",
            ));
        }
        let stack = held_stack(before, hand).ok_or_else(|| {
            ApiError::new(ErrorCode::NotReady, "Held stack observation unavailable")
        })?;
        if stack["count"].as_u64().unwrap_or(0) == 0 || stack["item"] == "minecraft:air" {
            return Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "There is no item in the requested hand",
            ));
        }
        if stack["food"]["can_eat"] == false {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "The held food cannot currently be eaten",
            ));
        }
    }
    Ok(())
}
fn new_event<'a>(
    action: &Action,
    before: &Observation,
    after: &'a Observation,
) -> Option<&'a Value> {
    let old = &before.player["native_action_effects"];
    let new = &after.player["native_action_effects"];
    if old["format"] != "native-action-effects-v1"
        || old["epoch"].as_str()? != new["epoch"].as_str()?
    {
        return None;
    }
    let baseline = old["last_sequence"].as_u64()?;
    new["events"].as_array()?.iter().find(|event| {
        if event["sequence"].as_u64().is_none_or(|n| n <= baseline)
            || event["actor_uuid"] != before.player["uuid"]
            || event["dimension"] != before.player["dimension"]
        {
            return false;
        }
        match action {
            Action::Attack { entity } => {
                let target = before.world["entities"]
                    .as_array()
                    .and_then(|rows| rows.iter().find(|e| e["entity_id"] == *entity));
                event["kind"] == "attack_damage"
                    && event["damage"]
                        .as_f64()
                        .is_some_and(|n| n.is_finite() && n > 0.)
                    && target.is_some_and(|t| {
                        t["uuid"].is_string()
                            && event["target_uuid"] == t["uuid"]
                            && event["target_entity_id"] == *entity
                    })
            }
            Action::UseItem { hand } | Action::StartUseItem { hand, .. } => {
                event["kind"] == "item_use_finished"
                    && held_stack(before, hand).is_some_and(|stack| {
                        stack["item"].is_string() && event["item"] == stack["item"]
                    })
                    && event["hand"]
                        == match hand {
                            Hand::Main => "MAIN_HAND",
                            Hand::Off => "OFF_HAND",
                        }
            }
            _ => false,
        }
    })
}
fn same_lane(before: &Observation, after: &Observation) -> bool {
    let clock = |o: &Observation| {
        o.world["sample_clock"]["nanos"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
    };
    let later = clock(before).zip(clock(after)).is_some_and(|(b, a)| {
        let d = a.wrapping_sub(b);
        d > 0 && d < u64::MAX / 2
    });
    later
        && before.player["uuid"].is_string()
        && before.player["uuid"] == after.player["uuid"]
        && before.player["dimension"].is_string()
        && before.player["dimension"] == after.player["dimension"]
        && before.world["sample_clock"]["epoch"].is_string()
        && before.world["sample_clock"]["epoch"] == after.world["sample_clock"]["epoch"]
}
fn proof(action: &Action, before: &Observation, after: &Observation) -> Option<Value> {
    if !same_lane(before, after) {
        return None;
    }
    match action {
        Action::Attack { .. } => new_event(action,before,after).cloned(),
        Action::UseItem { hand } | Action::StartUseItem { hand, .. } => {
            if after.player["using_item"] != false { return None; }
            if let Some(event)=new_event(action,before,after) { return Some(event.clone()); }
            let b=held_stack(before,hand)?;let a=held_stack(after,hand)?;
            let decreased=b["count"].as_u64()?.checked_sub(a["count"].as_u64()?).is_some_and(|n|n>0);
            if b["count"].as_u64()? > 0 && (b["item"]!=a["item"] || decreased) {
                Some(json!({"kind":"held_stack_changed","before":b,"after":a,"scope":"Observed item-use result; food or health gains require their own observation"}))
            } else {None}
        }
        Action::Interact { .. } => (before.menu["menu_id"].as_i64().is_some()
            && after.menu["menu_id"].as_i64().is_some_and(|id|id!=0)
            && before.menu["menu_id"]!=after.menu["menu_id"])
            .then(||json!({"kind":"menu_opened","menu_id":after.menu["menu_id"],"type":after.menu["type"]})),
        _ => verified_effect(action,before,after).then(||json!({"kind":"native_state_effect"})),
    }
}

pub(super) struct Job {
    id: String,
    held: bool,
    cancel_id: Option<String>,
    cancel: Arc<AtomicBool>,
    reported: AtomicBool,
    state: Arc<Mutex<Receipt>>,
}
impl Job {
    pub fn running(&self) -> bool {
        self.state
            .lock()
            .map_or(true, |r| r.outcome == Outcome::Running)
    }
    pub fn needs_status(&self) -> bool {
        self.running() || !self.reported.load(Ordering::Acquire)
    }
    pub fn acknowledge_reconciliation(&self, id: &str) {
        if (id == self.id || self.cancel_id.as_deref() == Some(id))
            && self
                .state
                .lock()
                .is_ok_and(|r| r.outcome == Outcome::Uncertain)
        {
            // Only called after two settled native reconciliation samples.
            // Preserve the original uncertain effect; allow later intentions.
            self.reported.store(true, Ordering::Release);
        }
    }
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    pub fn observation(&self) -> Value {
        self.state.lock().map(|r|json!({"action_id":self.id,"outcome":r.outcome,"progress":r.evidence["progress"],"input_released":r.evidence["input_released"],"held_item_use":self.held})).unwrap_or_else(|_|json!({"action_id":self.id,"outcome":"uncertain"}))
    }
    pub fn status(&self, id: &str) -> Result<Receipt> {
        let mut r = self
            .state
            .lock()
            .map_err(|_| ApiError::new(ErrorCode::Internal, "Interaction state unavailable"))?
            .clone();
        if id == self.id {
            if r.outcome.terminal() {
                self.reported.store(true, Ordering::Release);
            }
            return Ok(r);
        }
        if self.cancel_id.as_deref() != Some(id) {
            return Err(ApiError::new(
                ErrorCode::NotFound,
                "Interaction action ID unavailable",
            ));
        }
        r.action_id = id.into();
        let released = r.evidence["input_released"] == true;
        r.evidence = json!({"interaction_action_id":self.id,"interaction_outcome":r.outcome,"interaction":r.evidence,"input_released":released,"native_effect_verified":released});
        if r.outcome.terminal() {
            self.reported.store(true, Ordering::Release);
            if released {
                r.outcome = Outcome::Observed;
                r.error = None;
            }
        }
        Ok(r)
    }
    pub fn cancel(&mut self, id: &str, target: &str) -> Result<Receipt> {
        if target != self.id || !self.held {
            return Err(ApiError::new(
                ErrorCode::NotFound,
                "No matching held item use; discrete interactions cannot be undone",
            ));
        }
        if self.cancel_id.as_deref().is_some_and(|old| old != id) {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "Inspect the existing item-use cancellation",
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
        sent: bool,
        max_ticks: u16,
        epoch: u64,
    ) -> Self {
        let state = Arc::new(Mutex::new(Receipt {
            action_id: request.action_id.clone(),
            outcome: Outcome::Running,
            adapter_version: VERSION.into(),
            evidence: json!({"before":compact(&request.action,&before),"dispatch_completed":sent,"native_effect_verified":false,"input_released":!held(&request.action),"progress":{"samples":0},"replayed":false}),
            error: None,
        }));
        let cancel = Arc::new(AtomicBool::new(false));
        let ctx = Context {
            native,
            before,
            request: request.clone(),
            state: state.clone(),
            cancel: cancel.clone(),
            alive: player.alive.clone(),
            corrections: player.corrections.clone(),
            epoch,
            wire: player.wire.clone(),
            tables: player.tables.clone(),
            sent,
            max_ticks,
        };
        let worker = thread::Builder::new()
            .name("sdk-interaction".into())
            .spawn(move || ctx.run());
        if worker.is_err()
            && let Ok(mut r) = state.lock()
        {
            r.outcome = Outcome::Uncertain;
            r.error = Some(ApiError::new(
                ErrorCode::UncertainOutcome,
                "Interaction observer unavailable after dispatch; inspect/reconcile without replay",
            ));
            player.alive.store(false, Ordering::Release);
            if let Ok(w) = player.wire.lock() {
                let _ = w.connection().shutdown(std::net::Shutdown::Both);
            }
        }
        Self {
            id: request.action_id.clone(),
            held: held(&request.action),
            cancel_id: None,
            cancel,
            state,
            reported: AtomicBool::new(false),
        }
    }
}
struct Context {
    native: Native,
    before: Observation,
    request: ActionRequest,
    state: Arc<Mutex<Receipt>>,
    cancel: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    corrections: Arc<AtomicU64>,
    epoch: u64,
    wire: Arc<Mutex<Wire<TcpStream>>>,
    tables: Tables,
    sent: bool,
    max_ticks: u16,
}
impl Context {
    fn release(&self) -> AnyResult<()> {
        // Vanilla releaseUsingItem sends RELEASE_USE_ITEM with zero position,
        // DOWN and the packet constructor's default sequence zero.
        let mut body = vi(5);
        body.extend(block(&BlockPos { x: 0, y: 0, z: 0 }));
        body.push(0);
        body.extend(vi(0));
        let mut wire = self
            .wire
            .lock()
            .map_err(|_| "Item-use writer unavailable")?;
        self.tables.send(&mut wire, "play", "block_dig", &body)
    }
    fn finish(
        &self,
        outcome: Outcome,
        after: Option<&Observation>,
        effect: Option<Value>,
        released: bool,
        reason: &str,
    ) {
        if let Ok(mut r) = self.state.lock() {
            r.outcome = outcome.clone();
            r.evidence["after"] = after
                .map(|o| compact(&self.request.action, o))
                .unwrap_or(Value::Null);
            r.evidence["effect"] = effect.clone().unwrap_or(Value::Null);
            r.evidence["native_effect_verified"] = json!(effect.is_some());
            r.evidence["input_released"] = json!(released);
            r.evidence["reason"] = json!(reason);
            r.error = if outcome == Outcome::Uncertain {
                Some(ApiError::new(
                    ErrorCode::UncertainOutcome,
                    "Interaction interrupted; inspect/reconcile without replay",
                ))
            } else {
                None
            };
        }
    }
    fn run(self) {
        let started = Instant::now();
        let mut samples = 0u32;
        let mut last_native = started;
        let mut release_sent: Option<Instant> = None;
        let mut cancelled = false;
        let mut latest = None;
        let mut use_start = None;
        let is_held = held(&self.request.action);
        let limit = match self.request.action {
            Action::StartUseItem { max_ticks, .. } => max_ticks,
            _ => self.max_ticks,
        };
        let hard_limit = Duration::from_millis(u64::from(limit) * 100 + 2000);
        if !self.sent {
            self.finish(
                Outcome::Uncertain,
                None,
                None,
                false,
                "partial_or_failed_dispatch",
            );
            return;
        }
        loop {
            if !self.alive.load(Ordering::Acquire)
                || self.corrections.load(Ordering::Acquire) != self.epoch
            {
                if is_held {
                    let _ = self.release();
                }
                self.finish(
                    Outcome::Uncertain,
                    latest.as_ref(),
                    None,
                    !is_held,
                    "connection_or_position_interrupted",
                );
                return;
            }
            let cancel = self.cancel.load(Ordering::Acquire);
            let elapsed = started.elapsed();
            let tick_expired = latest
                .as_ref()
                .and_then(|o: &Observation| o.world["game_time"].as_u64())
                .zip(use_start)
                .is_some_and(|(now, first)| now.saturating_sub(first) >= u64::from(limit));
            if is_held
                && release_sent.is_none()
                && (cancel || tick_expired || elapsed >= hard_limit)
            {
                cancelled = cancel;
                if self.release().is_err() {
                    self.finish(
                        Outcome::Uncertain,
                        latest.as_ref(),
                        None,
                        false,
                        "item_release_dispatch_failed",
                    );
                    return;
                }
                release_sent = Some(Instant::now());
            }
            let after = self.native.observe_bounded(
                self.before.player["uuid"].as_str().unwrap_or(""),
                Duration::from_millis(400),
            );
            if let Ok(after) = after {
                if !same_lane(&self.before, &after) {
                    if is_held && release_sent.is_none() {
                        let _ = self.release();
                    }
                    self.finish(
                        Outcome::Uncertain,
                        Some(&after),
                        None,
                        false,
                        "native_actor_dimension_or_clock_changed",
                    );
                    return;
                }
                samples += 1;
                last_native = Instant::now();
                if is_held && after.player["using_item"] == true && use_start.is_none() {
                    use_start = after.world["game_time"].as_u64();
                }
                let released = !is_held || after.player["using_item"] == false;
                if let Some(effect) = proof(&self.request.action, &self.before, &after) {
                    self.finish(
                        Outcome::Observed,
                        Some(&after),
                        Some(effect),
                        released,
                        "native_effect_observed",
                    );
                    return;
                }
                if release_sent.is_some() && released {
                    self.finish(
                        if cancelled {
                            Outcome::Cancelled
                        } else {
                            Outcome::Expired
                        },
                        Some(&after),
                        None,
                        true,
                        "held_input_released_without_verified_completion",
                    );
                    return;
                }
                if after.player["health"].as_f64().is_some_and(|h| h <= 0.) {
                    if is_held && !released && release_sent.is_none() {
                        let _ = self.release();
                    }
                    self.finish(
                        Outcome::Failed,
                        Some(&after),
                        None,
                        released,
                        "character_died",
                    );
                    return;
                }
                if let Ok(mut r) = self.state.lock() {
                    r.evidence["progress"] = json!({"samples":samples,"revision":after.revision,"game_time":after.world["game_time"],"using_item":after.player["using_item"],"remaining_use_ticks":after.player["remaining_use_ticks"],"elapsed_ms":elapsed.as_millis()});
                }
                latest = Some(after);
            }
            if last_native.elapsed() >= Duration::from_secs(2)
                || release_sent.is_some_and(|at| at.elapsed() >= Duration::from_secs(2))
            {
                if is_held && release_sent.is_none() {
                    let _ = self.release();
                }
                self.finish(
                    Outcome::Uncertain,
                    latest.as_ref(),
                    None,
                    false,
                    "native_observation_or_release_interrupted",
                );
                return;
            }
            if !is_held && elapsed >= Duration::from_millis(u64::from(limit) * 50) {
                self.finish(Outcome::Expired,latest.as_ref(),None,true,"effect_not_observed_within_budget; no assertion that a later effect is impossible");
                return;
            }
            thread::sleep(Duration::from_millis(75));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn observation() -> Observation {
        Observation {
            revision: "1".into(),
            player: json!({"uuid":"own","dimension":"minecraft:overworld","health":20.,"selected":0,"using_item":false,"inventory":[{"slot":0,"item":"minecraft:apple","count":2}],"native_action_effects":{"format":"native-action-effects-v1","epoch":"events","last_sequence":1,"events":[]}}),
            world: json!({"game_time":1,"sample_clock":{"epoch":"clock","nanos":"1"},"entities":[{"entity_id":7,"uuid":"target","health":20.}]}),
            menu: json!({"menu_id":0}),
        }
    }
    fn damage() -> Value {
        json!({"sequence":2,"actor_uuid":"own","dimension":"minecraft:overworld","kind":"attack_damage","target_uuid":"target","target_entity_id":7,"damage":2.})
    }
    #[test]
    fn damage_requires_a_new_attributed_effect_not_another_attackers_health_change() {
        let b = observation();
        let mut a = b.clone();
        a.world["sample_clock"]["nanos"] = json!("2");
        a.world["entities"][0]["health"] = json!(18.);
        let action = Action::Attack { entity: 7 };
        assert!(proof(&action, &b, &a).is_none());
        a.player["native_action_effects"]["events"] = json!([damage()]);
        assert!(proof(&action, &b, &a).is_some());
        for (field, value) in [
            ("actor_uuid", json!("other")),
            ("target_uuid", json!("other")),
            ("target_entity_id", json!(8)),
            ("sequence", json!(1)),
            ("damage", json!(0)),
            ("dimension", json!("other:world")),
        ] {
            let mut wrong = a.clone();
            wrong.player["native_action_effects"]["events"][0][field] = value;
            assert!(
                proof(&action, &b, &wrong).is_none(),
                "accepted wrong {field}"
            );
        }
        a.player["native_action_effects"]["epoch"] = json!("new-connection");
        assert!(proof(&action, &b, &a).is_none());
    }

    struct Lab {
        observed: Arc<Mutex<Observation>>,
        server_alive: Arc<AtomicBool>,
        server: Option<thread::JoinHandle<()>>,
        native: Native,
        wire: Arc<Mutex<Wire<TcpStream>>>,
        receiver: Option<TcpStream>,
    }
    impl Lab {
        fn new() -> Self {
            let observed = Arc::new(Mutex::new(observation()));
            let server_alive = Arc::new(AtomicBool::new(true));
            let http = TcpListener::bind("127.0.0.1:0").unwrap();
            http.set_nonblocking(true).unwrap();
            let endpoint = format!("http://{}", http.local_addr().unwrap());
            let alive = server_alive.clone();
            let data = observed.clone();
            let server = thread::spawn(move || {
                let mut serial = 2u64;
                while alive.load(Ordering::Acquire) {
                    let Ok((mut stream, _)) = http.accept() else {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    };
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    let mut header = Vec::new();
                    while !header.ends_with(b"\r\n\r\n") {
                        let mut b = [0];
                        stream.read_exact(&mut b).unwrap();
                        header.push(b[0]);
                        assert!(header.len() < 8192);
                    }
                    let header = String::from_utf8(header).unwrap();
                    assert!(header.starts_with("POST /v1/observe "));
                    let size = header
                        .lines()
                        .find_map(|l| {
                            l.split_once(':')
                                .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                                .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    assert!(size < 1024);
                    let mut body = vec![0; size];
                    stream.read_exact(&mut body).unwrap();
                    assert_eq!(
                        serde_json::from_slice::<Value>(&body).unwrap()["player_uuid"],
                        "own"
                    );
                    let mut o = data.lock().unwrap().clone();
                    o.revision = serial.to_string();
                    o.world["sample_clock"]["nanos"] = json!(serial.to_string());
                    o.world["game_time"] = json!(serial);
                    serial += 1;
                    let body = serde_json::to_vec(&o).unwrap();
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
            Self {
                observed,
                server_alive,
                server: Some(server),
                native: Native {
                    endpoint,
                    token: "test-only-native".into(),
                    http: reqwest::blocking::Client::builder()
                        .no_proxy()
                        .build()
                        .unwrap(),
                },
                wire: Arc::new(Mutex::new(Wire::new(sender))),
                receiver: Some(receiver),
            }
        }
        fn start(
            &self,
            action: Action,
            max_ticks: u16,
        ) -> (Job, thread::JoinHandle<()>, Arc<AtomicBool>) {
            let request = ActionRequest {
                action_id: "interaction-one".into(),
                action,
            };
            let state = Arc::new(Mutex::new(Receipt {
                action_id: request.action_id.clone(),
                outcome: Outcome::Running,
                adapter_version: "fixture".into(),
                evidence: json!({"native_effect_verified":false}),
                error: None,
            }));
            let cancel = Arc::new(AtomicBool::new(false));
            let alive = Arc::new(AtomicBool::new(true));
            let ctx = Context {
                native: self.native.clone(),
                before: self.observed.lock().unwrap().clone(),
                request: request.clone(),
                state: state.clone(),
                cancel: cancel.clone(),
                alive: alive.clone(),
                corrections: Arc::new(AtomicU64::new(0)),
                epoch: 0,
                wire: self.wire.clone(),
                tables: Tables::new(),
                sent: true,
                max_ticks,
            };
            let job = Job {
                id: request.action_id,
                held: held(&request.action),
                cancel_id: None,
                cancel,
                state,
                reported: AtomicBool::new(false),
            };
            (job, thread::spawn(move || ctx.run()), alive)
        }
    }
    impl Drop for Lab {
        fn drop(&mut self) {
            self.server_alive.store(false, Ordering::Release);
            if let Some(server) = self.server.take() {
                let result = server.join();
                if !thread::panicking() {
                    assert!(result.is_ok(), "native fixture server failed");
                }
            }
            let _ = self
                .wire
                .lock()
                .unwrap()
                .connection()
                .shutdown(std::net::Shutdown::Both);
        }
    }
    #[test]
    fn a_delayed_menu_effect_stays_running_then_preserves_the_terminal_receipt_until_inspected() {
        let lab = Lab::new();
        let (job, worker, _) = lab.start(
            Action::Interact {
                entity: 7,
                hand: Hand::Main,
            },
            100,
        );
        thread::sleep(Duration::from_millis(300));
        assert!(job.running());
        assert!(job.needs_status());
        lab.observed.lock().unwrap().menu = json!({"menu_id":2,"type":"example:chest"});
        worker.join().unwrap();
        assert!(!job.running());
        assert!(job.needs_status());
        let r = job.status("interaction-one").unwrap();
        assert_eq!(r.outcome, Outcome::Observed);
        assert_eq!(r.evidence["native_effect_verified"], true);
        assert!(!job.needs_status());
    }
    #[test]
    fn ordinary_absence_expires_but_losing_the_connection_is_uncertain() {
        let lab = Lab::new();
        let (job, worker, _) = lab.start(
            Action::Interact {
                entity: 7,
                hand: Hand::Main,
            },
            4,
        );
        worker.join().unwrap();
        let r = job.status("interaction-one").unwrap();
        assert_eq!(r.outcome, Outcome::Expired);
        assert_eq!(r.evidence["native_effect_verified"], false);
        let (job, worker, alive) = lab.start(
            Action::Interact {
                entity: 7,
                hand: Hand::Main,
            },
            100,
        );
        thread::sleep(Duration::from_millis(150));
        alive.store(false, Ordering::Release);
        worker.join().unwrap();
        assert_eq!(
            job.status("interaction-one").unwrap().outcome,
            Outcome::Uncertain
        );
        assert!(job.needs_status());
        job.acknowledge_reconciliation("foreign");
        assert!(job.needs_status());
        job.acknowledge_reconciliation("interaction-one");
        assert!(!job.needs_status());
        assert_eq!(
            job.status("interaction-one").unwrap().outcome,
            Outcome::Uncertain,
            "Reconciliation must not manufacture historical success"
        );
    }
    #[test]
    fn held_use_is_progress_until_native_completion_and_release() {
        let lab = Lab::new();
        lab.observed.lock().unwrap().player["using_item"] = json!(true);
        let (job, worker, _) = lab.start(
            Action::StartUseItem {
                hand: Hand::Main,
                max_ticks: 100,
            },
            100,
        );
        thread::sleep(Duration::from_millis(300));
        assert!(job.running());
        {
            let mut o = lab.observed.lock().unwrap();
            o.player["using_item"] = json!(false);
            o.player["inventory"][0]["count"] = json!(1);
            o.player["native_action_effects"]["events"] = json!([{"sequence":2,"actor_uuid":"own","dimension":"minecraft:overworld","kind":"item_use_finished","item":"minecraft:apple","hand":"MAIN_HAND"}]);
        }
        worker.join().unwrap();
        let r = job.status("interaction-one").unwrap();
        assert_eq!(r.outcome, Outcome::Observed);
        assert_eq!(r.evidence["effect"]["kind"], "item_use_finished");
        assert_eq!(r.evidence["input_released"], true);
    }
    #[test]
    fn cancellation_sends_a_real_release_and_does_not_certify_consumption() {
        let mut lab = Lab::new();
        lab.observed.lock().unwrap().player["using_item"] = json!(true);
        let stream = lab.receiver.take().unwrap();
        let observed = lab.observed.clone();
        let reader = thread::spawn(move || {
            let mut wire = Wire::new(stream);
            let packet = wire.receive().unwrap();
            assert_eq!(
                packet.id,
                Tables::new().id("play", "toServer", "block_dig").unwrap()
            );
            assert_eq!(packet.payload, vec![5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            observed.lock().unwrap().player["using_item"] = json!(false);
        });
        let (mut job, worker, _) = lab.start(
            Action::StartUseItem {
                hand: Hand::Main,
                max_ticks: 100,
            },
            100,
        );
        thread::sleep(Duration::from_millis(200));
        assert!(job.cancel("cancel-one", "foreign").is_err());
        job.cancel("cancel-one", "interaction-one").unwrap();
        worker.join().unwrap();
        reader.join().unwrap();
        let original = job.status("interaction-one").unwrap();
        assert_eq!(original.outcome, Outcome::Cancelled);
        assert_eq!(original.evidence["native_effect_verified"], false);
        assert_eq!(original.evidence["input_released"], true);
        let cancel = job.status("cancel-one").unwrap();
        assert_eq!(cancel.outcome, Outcome::Observed);
        assert_eq!(cancel.evidence["native_effect_verified"], true);
    }
}
