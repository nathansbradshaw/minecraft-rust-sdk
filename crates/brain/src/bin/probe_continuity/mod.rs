//! Probe evidence only. Sampling does not prevent damage, death or respawn.
use minecraft_sdk::Observation;
use minecraft_sdk_brain::{Brain, Journal, Result, body::Shared, ensure, private_read};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const PERIOD: Duration = Duration::from_millis(150);

#[derive(Clone, Debug, PartialEq, Serialize)]
struct Binding {
    uuid: String,
    life_id: String,
    clock_epoch: String,
}
#[derive(Clone, Debug, Serialize)]
struct Facts {
    binding: Binding,
    native_nanos: String,
    health: f64,
    revision: String,
}
impl Facts {
    fn read(o: &Observation) -> Result<Self> {
        let text = |value: &Value| value.as_str().filter(|s| !s.is_empty()).map(str::to_owned);
        let nanos = text(&o.world["sample_clock"]["nanos"])
            .filter(|s| s.parse::<u64>().is_ok())
            .ok_or("Native sample clock missing")?;
        Ok(Self {
            binding: Binding {
                uuid: text(&o.player["uuid"]).ok_or("Native player UUID missing")?,
                life_id: text(&o.player["life_id"]).ok_or("Native player life identity missing")?,
                clock_epoch: text(&o.world["sample_clock"]["epoch"])
                    .ok_or("Native clock epoch missing")?,
            },
            native_nanos: nanos,
            health: o.player["health"]
                .as_f64()
                .filter(|h| h.is_finite())
                .ok_or("Native health missing")?,
            revision: o.revision.clone(),
        })
    }
}

struct Tracker {
    initial: Facts,
    last: Facts,
    responses: u64,
    distinct_native_samples: u64,
    repeated_native_samples: u64,
    nonpositive_health_samples: u64,
    read_errors: u64,
    min_health: f64,
    last_response: Duration,
    last_advanced: Duration,
    max_response_gap: Duration,
    max_advanced_gap: Duration,
    max_native_gap_nanos: u64,
    failures: BTreeSet<&'static str>,
    incomplete: BTreeSet<&'static str>,
    gateway_final_scene: Option<Facts>,
    gateway_final_scene_error: Option<String>,
}
#[derive(Debug, Serialize)]
pub(super) struct Summary {
    initial: Facts,
    final_sample: Facts,
    sampled_original_life_min_health: f64,
    observed_actor_nonpositive_health_samples: u64,
    responses: u64,
    distinct_native_samples: u64,
    repeated_native_samples: u64,
    read_errors: u64,
    elapsed_ms: u128,
    max_response_gap_ms: u128,
    max_distinct_sample_gap_ms: u128,
    uncovered_tail_ms: u128,
    max_native_sample_gap_nanos: String,
    gap_acceptance_limit_ms: Option<u64>,
    gap_acceptance_evaluated: bool,
    failure_reasons: BTreeSet<&'static str>,
    incomplete_reasons: BTreeSet<&'static str>,
    ordered_final_sample_observed: bool,
    gateway_final_scene: Option<Facts>,
    gateway_final_scene_error: Option<String>,
    external_death_statistics: &'static str,
    scope: &'static str,
}
impl Summary {
    pub(super) fn no_observed_lifecycle_failure(&self) -> bool {
        self.failure_reasons.is_empty()
    }
}
impl Tracker {
    fn new(o: &Observation) -> Result<Self> {
        let initial = Facts::read(o)?;
        let dead = u64::from(initial.health <= 0.);
        let mut failures = BTreeSet::new();
        if dead != 0 {
            failures.insert("observed_actor_nonpositive_health");
        }
        Ok(Self {
            last: initial.clone(),
            min_health: initial.health,
            initial,
            responses: 1,
            distinct_native_samples: 1,
            repeated_native_samples: 0,
            nonpositive_health_samples: dead,
            read_errors: 0,
            last_response: Duration::ZERO,
            last_advanced: Duration::ZERO,
            max_response_gap: Duration::ZERO,
            max_advanced_gap: Duration::ZERO,
            max_native_gap_nanos: 0,
            failures,
            incomplete: BTreeSet::new(),
            gateway_final_scene: None,
            gateway_final_scene_error: None,
        })
    }
    fn health_facts(&mut self, facts: &Facts) {
        if facts.binding.uuid == self.initial.binding.uuid && facts.health <= 0. {
            self.nonpositive_health_samples += 1;
            self.failures.insert("observed_actor_nonpositive_health");
        }
        if facts.binding == self.initial.binding {
            self.min_health = self.min_health.min(facts.health);
        }
    }
    fn observe(&mut self, o: &Observation, elapsed: Duration) -> Value {
        self.responses += 1;
        self.max_response_gap = self
            .max_response_gap
            .max(elapsed.saturating_sub(self.last_response));
        self.last_response = elapsed;
        let facts = match Facts::read(o) {
            Ok(facts) => facts,
            Err(error) => {
                self.incomplete.insert("invalid_native_sample");
                return json!({"elapsed_ms":elapsed.as_millis(),"error":error.to_string(),"sample_accepted":false});
            }
        };
        if facts.binding.uuid != self.initial.binding.uuid {
            self.failures.insert("player_uuid_changed");
        }
        if facts.binding.life_id != self.initial.binding.life_id {
            self.failures.insert("native_life_changed");
        }
        if facts.binding.clock_epoch != self.initial.binding.clock_epoch {
            self.failures.insert("native_clock_epoch_changed");
        }
        let original_scope = facts.binding == self.initial.binding;
        self.health_facts(&facts);
        let same_clock = facts.binding == self.last.binding;
        let current = facts.native_nanos.parse::<u64>().expect("validated clock");
        let previous = self
            .last
            .native_nanos
            .parse::<u64>()
            .expect("validated clock");
        let delta = current.wrapping_sub(previous);
        let advanced = same_clock && delta != 0 && delta <= i64::MAX as u64;
        if same_clock && delta > i64::MAX as u64 {
            self.incomplete.insert("ordered_sample_clock_out_of_order");
        }
        if same_clock && delta == 0 {
            self.repeated_native_samples += 1;
            if facts.health != self.last.health {
                self.incomplete
                    .insert("changed_health_with_repeated_native_clock");
            }
        }
        if advanced {
            self.distinct_native_samples += 1;
            self.max_native_gap_nanos = self.max_native_gap_nanos.max(delta);
            self.max_advanced_gap = self
                .max_advanced_gap
                .max(elapsed.saturating_sub(self.last_advanced));
            self.last_advanced = elapsed;
        }
        self.last = facts.clone();
        json!({"elapsed_ms":elapsed.as_millis(),"facts":facts,"original_scope":original_scope,
            "native_sample_advanced":advanced,"sample_accepted":true})
    }
    fn read_error(&mut self, error: &str, elapsed: Duration) -> Value {
        self.read_errors += 1;
        self.incomplete.insert("observation_read_failed");
        json!({"elapsed_ms":elapsed.as_millis(),"error":error,"native_sample_observed":false})
    }
    fn gateway_final_scene(&mut self, o: &Observation, elapsed: Duration) -> Value {
        match Facts::read(o) {
            Ok(facts) => {
                self.health_facts(&facts);
                if facts.binding != self.initial.binding {
                    self.failures.insert("gateway_final_binding_conflict");
                }
                // This response can predate a later Shared snapshot. Retain its
                // native clock, but never compare it as the next ordered sample.
                self.gateway_final_scene = Some(facts.clone());
                json!({"elapsed_ms":elapsed.as_millis(),"facts":facts,"ordered_sample":false})
            }
            Err(error) => {
                self.incomplete.insert("invalid_gateway_final_scene");
                self.gateway_final_scene_error = Some(error.to_string());
                json!({"elapsed_ms":elapsed.as_millis(),"error":error.to_string(),"ordered_sample":false})
            }
        }
    }
    fn finish(mut self, elapsed: Duration, final_scene_observed: bool) -> Summary {
        let tail = elapsed.saturating_sub(self.last_advanced);
        self.max_response_gap = self
            .max_response_gap
            .max(elapsed.saturating_sub(self.last_response));
        self.max_advanced_gap = self.max_advanced_gap.max(tail);
        if self.distinct_native_samples < 2 {
            self.incomplete.insert("no_advancing_native_samples");
        }
        if !final_scene_observed {
            self.incomplete.insert("final_scene_not_observed");
        }
        Summary {
            initial: self.initial,
            final_sample: self.last,
            sampled_original_life_min_health: self.min_health,
            observed_actor_nonpositive_health_samples: self.nonpositive_health_samples,
            responses: self.responses,
            distinct_native_samples: self.distinct_native_samples,
            repeated_native_samples: self.repeated_native_samples,
            read_errors: self.read_errors,
            elapsed_ms: elapsed.as_millis(),
            max_response_gap_ms: self.max_response_gap.as_millis(),
            max_distinct_sample_gap_ms: self.max_advanced_gap.as_millis(),
            uncovered_tail_ms: tail.as_millis(),
            max_native_sample_gap_nanos: self.max_native_gap_nanos.to_string(),
            gap_acceptance_limit_ms: None,
            gap_acceptance_evaluated: false,
            failure_reasons: self.failures,
            incomplete_reasons: self.incomplete,
            ordered_final_sample_observed: final_scene_observed,
            gateway_final_scene: self.gateway_final_scene,
            gateway_final_scene_error: self.gateway_final_scene_error,
            external_death_statistics: "not read; independently compare saved before/after death statistics",
            scope: "Sampled native facts only; health minimum covers returned original-binding observations and may miss changes between samples. Gaps have no implicit acceptance limit. Nonpositive-health samples can repeat and are not death counts. No guaranteed abort, damage prevention, no-death claim or gameplay competence is established.",
        }
    }
}

struct Recorded {
    tracker: Tracker,
    journal: Journal,
    started: Instant,
}
pub(super) struct Monitor {
    recorded: Arc<Mutex<Recorded>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    worker: Option<tokio::task::JoinHandle<Result<()>>>,
    shared: Option<Arc<Shared>>,
}
fn record(recorded: &Mutex<Recorded>, o: &Observation, source: &str) -> Result<()> {
    let mut state = recorded
        .lock()
        .map_err(|_| "Probe continuity monitor unavailable")?;
    let elapsed = state.started.elapsed();
    let sample = state.tracker.observe(o, elapsed);
    state.journal.event(
        "body_probe_continuity_sample",
        json!({"source":source,"sample":sample}),
    )?;
    Ok(())
}
impl Monitor {
    pub(super) fn start(brain: &Brain) -> Result<Self> {
        let profile: minecraft_sdk::Profile =
            serde_json::from_slice(&private_read(&brain.cfg.profile, 16384)?)?;
        ensure(
            brain.observed.player["uuid"] == profile.credential.player_uuid,
            "Probe baseline character mismatch",
        )?;
        let tracker = Tracker::new(&brain.observed)?;
        let path = brain
            .cfg
            .state
            .join(format!("body-probe-continuity-{}.sqlite", brain.run_id));
        let journal = Journal::open(&path)?;
        journal.event("body_probe_continuity_started", json!({"baseline":tracker.initial,"gap_acceptance_limit_ms":null,
            "source":"Initial connection or stimulus admission snapshot; baseline predates sampler startup", "external_death_statistics":"not_read"}))?;
        let recorded = Arc::new(Mutex::new(Recorded {
            tracker,
            journal,
            started: Instant::now(),
        }));
        let (stop, worker) = if let Some(body) = &brain.body {
            let shared: Arc<Shared> = body.shared.clone();
            let state = recorded.clone();
            let (stop, mut stopping) = tokio::sync::oneshot::channel();
            let worker = tokio::spawn(async move {
                let mut cadence = tokio::time::interval(PERIOD);
                cadence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tokio::select! {
                        biased;
                        _ = &mut stopping => return Ok(()),
                        _ = cadence.tick() => {}
                    }
                    match shared.observe() {
                        Ok(o) => record(&state, &o, "body_shared_snapshot")?,
                        Err(error) => {
                            let mut state = state
                                .lock()
                                .map_err(|_| "Probe continuity monitor unavailable")?;
                            let elapsed = state.started.elapsed();
                            let sample = state.tracker.read_error(&error.to_string(), elapsed);
                            state
                                .journal
                                .event("body_probe_continuity_read_error", sample)?;
                        }
                    }
                }
            });
            (Some(stop), Some(worker))
        } else {
            (None, None)
        };
        Ok(Self {
            recorded,
            stop,
            worker,
            shared: brain.body.as_ref().map(|body| body.shared.clone()),
        })
    }
    pub(super) fn sample(&self, o: &Observation) -> Result<()> {
        record(&self.recorded, o, "exposed_gateway_snapshot")
    }
    pub(super) async fn finish(mut self, final_scene: Option<Observation>) -> Result<Summary> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            worker.await??;
        }
        let ordered_final = if let Some(shared) = &self.shared {
            match shared.observe() {
                Ok(o) => {
                    record(&self.recorded, &o, "final_body_shared_snapshot")?;
                    true
                }
                Err(error) => {
                    let mut state = self
                        .recorded
                        .lock()
                        .map_err(|_| "Probe continuity monitor unavailable")?;
                    let elapsed = state.started.elapsed();
                    let sample = state.tracker.read_error(&error.to_string(), elapsed);
                    state
                        .journal
                        .event("body_probe_continuity_read_error", sample)?;
                    false
                }
            }
        } else if let Some(o) = &final_scene {
            record(&self.recorded, o, "final_exposed_gateway_snapshot")?;
            true
        } else {
            false
        };
        if self.shared.is_some()
            && let Some(o) = &final_scene
        {
            let mut state = self
                .recorded
                .lock()
                .map_err(|_| "Probe continuity monitor unavailable")?;
            let elapsed = state.started.elapsed();
            let sample = state.tracker.gateway_final_scene(o, elapsed);
            state
                .journal
                .event("body_probe_gateway_final_scene", sample)?;
        }
        let recorded = Arc::try_unwrap(self.recorded)
            .map_err(|_| "Probe continuity worker still owns state")?;
        let state = recorded
            .into_inner()
            .map_err(|_| "Probe continuity monitor unavailable")?;
        let summary = state.tracker.finish(state.started.elapsed(), ordered_final);
        state.journal.event(
            "body_probe_continuity_summary",
            serde_json::to_value(&summary)?,
        )?;
        Ok(summary)
    }
}

#[cfg(test)]
mod tests;
