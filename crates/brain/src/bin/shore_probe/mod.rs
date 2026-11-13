//! Deterministic water/foreground coordination probe. Never calls inference.
use minecraft_sdk::{Observation, navigation::Goal};
use minecraft_sdk_brain::{Brain, Result, ensure, private_read, tools};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Workload {
    Blocked,
    Compile,
    Navigate,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    workload: Workload,
    goal: Option<Goal>,
    #[serde(default = "stall_seconds")]
    stationary_seconds: u64,
    #[serde(default = "stall_radius")]
    stationary_radius: f64,
    #[serde(default = "dry_samples")]
    dry_samples: u32,
}
fn stall_seconds() -> u64 {
    30
}
fn stall_radius() -> f64 {
    1.5
}
fn dry_samples() -> u32 {
    3
}
impl Spec {
    pub fn read(path: &Path) -> Result<Self> {
        let value: Self = serde_json::from_slice(&private_read(path, 10000)?)?;
        ensure(
            (5..=600).contains(&value.stationary_seconds),
            "Stationary window must be5..600seconds",
        )?;
        ensure(
            value.stationary_radius.is_finite() && (0.5..=16.).contains(&value.stationary_radius),
            "Stationary radius must be0.5..16blocks",
        )?;
        ensure(
            (2..=40).contains(&value.dry_samples),
            "Require2..40distinct dry samples",
        )?;
        ensure(
            matches!(value.workload, Workload::Navigate) == value.goal.is_some(),
            "Only navigate workload requires a typed goal",
        )?;
        Ok(value)
    }
}

struct Monitor {
    lane: Value,
    first_tick: i64,
    last_tick: i64,
    anchor: [f64; 3],
    anchor_tick: i64,
    water_seen: bool,
    dry_count: u32,
    samples: u32,
    minimum_health: f64,
    minimum_air: i64,
    release_seen: bool,
}
fn position(o: &Observation) -> Result<[f64; 3]> {
    let mut p = [0.; 3];
    for (i, n) in p.iter_mut().enumerate() {
        *n = o.player["position"][i]
            .as_f64()
            .filter(|n| n.is_finite())
            .ok_or("Native position missing")?;
    }
    Ok(p)
}
fn lane(o: &Observation) -> Result<Value> {
    let actor = o.player["uuid"].as_str().ok_or("Native actor missing")?;
    let entity = o.player["entity_id"]
        .as_i64()
        .ok_or("Native entity missing")?;
    let dimension = o.player["dimension"]
        .as_str()
        .ok_or("Native dimension missing")?;
    let epoch = o.world["sample_clock"]["epoch"]
        .as_str()
        .ok_or("Native clock epoch missing")?;
    Ok(json!([actor, entity, dimension, epoch]))
}
fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f64>().sqrt()
}
impl Monitor {
    fn new(o: &Observation) -> Result<Self> {
        let tick = o.world["game_time"]
            .as_i64()
            .ok_or("Native game time missing")?;
        ensure(tick >= 0, "Native game time must be nonnegative")?;
        Ok(Self {
            lane: lane(o)?,
            first_tick: tick,
            last_tick: tick - 1,
            anchor: position(o)?,
            anchor_tick: tick,
            water_seen: false,
            dry_count: 0,
            samples: 0,
            minimum_health: f64::INFINITY,
            minimum_air: i64::MAX,
            release_seen: false,
        })
    }
    fn sample(&mut self, o: &Observation, spec: &Spec) -> Result<Option<Value>> {
        ensure(
            lane(o)? == self.lane,
            "Actor/entity/dimension/clock changed; ordinary respawn is not probe success",
        )?;
        let tick = o.world["game_time"]
            .as_i64()
            .ok_or("Native game time missing")?;
        ensure(tick >= self.last_tick, "Native clock moved backward")?;
        if tick == self.last_tick {
            return Ok(None);
        }
        self.last_tick = tick;
        self.samples += 1;
        let p = position(o)?;
        let health = o.player["health"]
            .as_f64()
            .filter(|n| n.is_finite())
            .ok_or("Native health missing")?;
        let air = o.player["air"].as_i64().ok_or("Native air missing")?;
        self.minimum_health = self.minimum_health.min(health);
        self.minimum_air = self.minimum_air.min(air);
        self.water_seen |= o.player["in_water"] == true || o.player["eyes_in_water"] == true;
        let dry = o.player["in_water"] == false
            && o.player["eyes_in_water"] == false
            && o.player["on_ground"] == true;
        self.dry_count = if self.water_seen && dry {
            self.dry_count + 1
        } else {
            0
        };
        if distance(p, self.anchor) >= spec.stationary_radius {
            self.anchor = p;
            self.anchor_tick = tick;
        }
        self.release_seen |= o.player["control"]["input_released"] == true;
        let failure = if health <= 0. {
            Some("native_death")
        } else if self.water_seen
            && !dry
            && tick.saturating_sub(self.anchor_tick) >= i64::try_from(spec.stationary_seconds * 20)?
        {
            Some("stationary_wet_loop")
        } else {
            None
        };
        Ok(Some(
            json!({"event":"shore_probe_sample","revision":o.revision,"game_time":tick,
            "elapsed_native_ticks":tick.saturating_sub(self.first_tick),"position":p,"health":health,"air":air,
            "in_water":o.player["in_water"],"eyes_in_water":o.player["eyes_in_water"],"on_ground":o.player["on_ground"],
            "dry_samples":self.dry_count,"water_seen":self.water_seen,
            "stationary_native_ticks":tick.saturating_sub(self.anchor_tick),"failure":failure,
            "control":{"action_id":o.player["control"]["action_id"],"outcome":o.player["control"]["outcome"],"input_released":o.player["control"]["input_released"]}}),
        ))
    }
}

async fn workload(brain: &mut Brain, spec: &Spec) -> Result<Value> {
    match spec.workload {
        Workload::Blocked => std::future::pending().await,
        Workload::Compile => {
            let result =
                tools::terminal(brain, "cargo build --offline --message-format=json").await?;
            Ok(
                json!({"kind":"compile","success":result["success"],"exit_code":result["exit_code"],"elapsed_ms":result["elapsed_ms"],"output_files":result["output_files"],"compiler_diagnostics":result["compiler_diagnostics"]}),
            )
        }
        Workload::Navigate => {
            let goal = serde_json::to_string(spec.goal.as_ref().ok_or("Navigation goal missing")?)?;
            loop {
                let result = minecraft_sdk_brain::navigation::execute(brain, &goal).await?;
                let status = result["navigation"]["report"]["status"]
                    .as_str()
                    .unwrap_or("error");
                println!(
                    "{}",
                    json!({"event":"shore_probe_navigation","status":status,"report":result["navigation"]["report"],"position_before":result["position_before"],"position_after":result["position_after"],"models_called":0})
                );
                if status != "budget_exhausted" {
                    return Ok(json!({"kind":"navigate","status":status}));
                }
            }
        }
    }
}

pub async fn run(brain: &mut Brain, spec: &Spec, seconds: u64) -> Result<()> {
    let shared = brain
        .body
        .as_ref()
        .ok_or("Shore probe requires independent body")?
        .shared
        .clone();
    // Brain::open captured this before starting the independent body. A fast
    // successful escape must not be mistaken for a probe that merely started dry.
    let mut monitor = Monitor::new(&brain.observed)?;
    println!(
        "{}",
        json!({"event":"shore_probe_started","parameters":spec,"seconds":seconds,"models_called":0})
    );
    if let Some(sample) = monitor.sample(&brain.observed, spec)? {
        println!("{sample}");
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    let mut cadence = tokio::time::interval(Duration::from_millis(250));
    cadence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut work_result = None;
    let mut work = Box::pin(workload(brain, spec));
    let reason = loop {
        tokio::select! {
            biased;
            _=tokio::signal::ctrl_c()=>break "interrupted",
            _=tokio::time::sleep_until(deadline)=>break "deadline",
            _=cadence.tick()=> {
                if let Some(sample)=monitor.sample(&shared.observe()?,spec)? {
                    let failed=sample["failure"].as_str().map(str::to_owned);
                    println!("{sample}");
                    if failed.is_some() {break "stationary_or_dead";}
                    let work_ready=match spec.workload {
                        Workload::Blocked=>true,
                        Workload::Compile=>work_result.as_ref().is_some_and(|r:&Value|r["success"]==true),
                        Workload::Navigate=>work_result.as_ref().is_some_and(|r:&Value|r["status"]=="reached"),
                    };
                    if monitor.dry_count>=spec.dry_samples && work_ready {break "dry_ground_observed";}
                }
            }
            result=&mut work,if work_result.is_none()=> {
                let result=result.unwrap_or_else(|error|json!({"kind":"tool_error","error":error.to_string()}));
                println!("{}",json!({"event":"shore_probe_workload","result":result}));
                let failed=matches!(spec.workload,Workload::Compile) && result["success"]!=true
                    || matches!(spec.workload,Workload::Navigate) && result["status"]!="reached";
                work_result=Some(result);
                if failed {break "workload_failed";}
            }
        }
    };
    drop(work);
    println!(
        "{}",
        json!({"event":"shore_probe_summary","reason":reason,"samples":monitor.samples,
        "minimum_health":monitor.minimum_health,"minimum_air":monitor.minimum_air,"water_seen":monitor.water_seen,
        "dry_samples":monitor.dry_count,"input_release_observed":monitor.release_seen,"workload":work_result,
        "models_called":0,"scope":"Declared deterministic native diagnostic, not autonomous task success. Dry observations and input-release sample are not authoritative receipts. Parent cleanup stops the body and disconnects; independently verify native action/release and saved-state evidence."})
    );
    ensure(
        reason == "dry_ground_observed",
        "Shore probe did not establish dry-ground recovery; inspect preserved samples, workload result and trusted native journal",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn spec() -> Spec {
        serde_json::from_value(json!({"workload":"blocked","stationary_seconds":5,"stationary_radius":1.5,"dry_samples":3})).unwrap()
    }
    fn observation(tick: i64, p: [f64; 3], wet: bool, ground: bool) -> Observation {
        Observation {
            revision: tick.to_string(),
            player: json!({"uuid":"actor","entity_id":1,"dimension":"fixture:world","position":p,"health":20.,"air":280,"in_water":wet,"eyes_in_water":wet,"on_ground":ground}),
            world: json!({"game_time":tick,"sample_clock":{"epoch":"epoch"}}),
            menu: json!({}),
        }
    }
    #[test]
    fn repeated_native_frame_and_vertical_bobbing_do_not_hide_stationary_failure() {
        let spec = spec();
        let first = observation(0, [0., 1., 0.], true, false);
        let mut m = Monitor::new(&first).unwrap();
        m.sample(&first, &spec).unwrap();
        assert!(m.sample(&first, &spec).unwrap().is_none());
        for tick in 1..100 {
            m.sample(
                &observation(
                    tick,
                    [0., 1. + if tick % 2 == 0 { 0. } else { 1. }, 0.],
                    true,
                    false,
                ),
                &spec,
            )
            .unwrap();
        }
        let sample = m
            .sample(&observation(100, [0., 1., 0.], true, false), &spec)
            .unwrap()
            .unwrap();
        assert_eq!(sample["failure"], "stationary_wet_loop");
    }
    #[test]
    fn real_displacement_resets_stationary_clock_and_dry_requires_advancing_samples() {
        let spec = spec();
        let first = observation(0, [0., 1., 0.], true, false);
        let mut m = Monitor::new(&first).unwrap();
        m.sample(&first, &spec).unwrap();
        let moving = m
            .sample(&observation(90, [2., 1., 0.], true, false), &spec)
            .unwrap()
            .unwrap();
        assert_eq!(moving["stationary_native_ticks"], 0);
        for tick in [100, 101, 102] {
            m.sample(&observation(tick, [2., 1., 0.], false, true), &spec)
                .unwrap();
        }
        assert_eq!(m.dry_count, 3);
        m.sample(&observation(103, [2., 1., 0.], true, false), &spec)
            .unwrap();
        assert_eq!(m.dry_count, 0);
    }
    #[test]
    fn dry_start_death_and_actor_change_cannot_become_water_recovery() {
        let spec = spec();
        let mut o = observation(0, [0., 1., 0.], false, true);
        let mut m = Monitor::new(&o).unwrap();
        for tick in 0..5 {
            o.world["game_time"] = json!(tick);
            m.sample(&o, &spec).unwrap();
        }
        assert_eq!(m.dry_count, 0);
        o.world["game_time"] = json!(5);
        o.player["health"] = json!(0);
        assert_eq!(
            m.sample(&o, &spec).unwrap().unwrap()["failure"],
            "native_death"
        );
        o.player["entity_id"] = json!(2);
        assert!(m.sample(&o, &spec).is_err());
    }
}
