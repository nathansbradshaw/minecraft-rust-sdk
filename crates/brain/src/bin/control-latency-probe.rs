//! Bounded native diagnostic, not an autonomous-play benchmark. No model calls.
use minecraft_sdk::{Action, Client, Observation, Outcome, Position, Profile};
use minecraft_sdk_brain::{Brain, Config, Result, private_read};
use serde_json::{Value, json};
use std::{
    path::Path,
    time::{Duration, Instant},
};
#[path = "control-latency-probe/finite.rs"]
mod finite;

fn position(o: &Observation) -> Result<[f64; 3]> {
    Ok([
        o.player["position"][0].as_f64().ok_or("x missing")?,
        o.player["position"][1].as_f64().ok_or("y missing")?,
        o.player["position"][2].as_f64().ok_or("z missing")?,
    ])
}
fn block(o: &Observation, p: [i32; 3]) -> Option<&Value> {
    o.world["blocks"]
        .as_array()?
        .iter()
        .find(|b| b["position"] == json!(p))
}
fn safe_column(o: &Observation, x: f64, y: i32, z: f64) -> bool {
    let bx = x.floor() as i32;
    let bz = z.floor() as i32;
    for height in [y, y + 1] {
        let Some(b) = block(o, [bx, height, bz]) else {
            return false;
        };
        if b["collision"].as_array().is_none_or(|a| !a.is_empty())
            || [
                "minecraft:water",
                "minecraft:lava",
                "minecraft:fire",
                "minecraft:sweet_berry_bush",
                "minecraft:powder_snow",
            ]
            .iter()
            .any(|id| b["block"] == *id)
        {
            return false;
        }
    }
    let Some(b) = block(o, [bx, y - 1, bz]) else {
        return false;
    };
    if [
        "minecraft:magma_block",
        "minecraft:campfire",
        "minecraft:soul_campfire",
        "minecraft:cactus",
    ]
    .iter()
    .any(|id| b["block"] == *id)
    {
        return false;
    }
    b["collision"].as_array().is_some_and(|boxes| {
        boxes
            .iter()
            .any(|b| b == &json!([0.0, 0.0, 0.0, 1.0, 1.0, 1.0]))
    })
}
fn waypoint(o: &Observation) -> Result<[f64; 3]> {
    let p = position(o)?;
    if o.player["on_ground"] != true || (p[1] - p[1].round()).abs() > 0.001 {
        return Err("This diagnostic requires observed flat full-block ground".into());
    }
    for distance in [2.5, 2.0, 1.5] {
        for (dx, dz) in [(1., 0.), (-1., 0.), (0., 1.), (0., -1.)] {
            let target = [p[0] + dx * distance, p[1], p[2] + dz * distance];
            let safe = (0..=20).all(|step| {
                let t = f64::from(step) / 20.;
                [-0.29, 0.29].iter().all(|x| {
                    [-0.29, 0.29].iter().all(|z| {
                        safe_column(
                            o,
                            p[0] + dx * distance * t + x,
                            p[1].round() as i32,
                            p[2] + dz * distance * t + z,
                        )
                    })
                })
            });
            if safe {
                return Ok(target);
            }
        }
    }
    Err("No sufficiently long, observed, collision-free flat route; no movement dispatched".into())
}
async fn probe(brain: &mut Brain) -> Result<()> {
    let profile: Profile = serde_json::from_slice(&private_read(&brain.cfg.profile, 16384)?)?;
    let observer = Client::with_profile(&profile)?;
    let before = observer.observe().await?;
    let target = waypoint(&before)?;
    let discovery = observer.discover().await?;
    let baseline = Instant::now();
    let _ = observer.observe().await?;
    let baseline_ms = baseline.elapsed().as_millis();
    brain.journal.event("control_latency_probe_started",json!({"target":target,"baseline_observe_ms":baseline_ms,"capabilities":discovery.capabilities,"models_called":0,"fixture":"ordinary existing character, no reset or supplied items"}))?;
    let started = Instant::now();
    let move_future = brain.act(Action::Move {
        target: Position {
            x: target[0],
            y: target[1],
            z: target[2],
        },
        on_ground: true,
        jump: false,
    });
    let observe_future = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let sent_at = started.elapsed().as_millis();
        let sent = Instant::now();
        let observation = observer.observe().await;
        (sent_at, sent.elapsed().as_millis(), observation)
    };
    let (movement, (sent_at, observe_ms, observed)) = tokio::join!(move_future, observe_future);
    let observation = observed?;
    let movement = movement?;
    brain.journal.event("control_latency_probe_result",json!({"baseline_observe_ms":baseline_ms,"concurrent_observe_sent_at_ms":sent_at,"concurrent_observe_ms":observe_ms,"whole_probe_ms":started.elapsed().as_millis(),"observed_position":observation.player["position"],"movement_outcome":movement["receipt"]["outcome"],"action_id":movement["receipt"]["action_id"],"native_effect_verified":movement["receipt"]["evidence"]["native_effect_verified"],"interpretation":"Single native diagnostic; not a latency percentile or Survival competence result"}))?;
    if movement["receipt"]["outcome"] == serde_json::to_value(Outcome::Uncertain)? {
        brain
            .settle(
                movement["receipt"]["action_id"]
                    .as_str()
                    .ok_or("Action ID missing")?,
            )
            .await?;
    }
    if movement["receipt"]["outcome"] != serde_json::to_value(Outcome::Observed)? {
        return Err(
            "Movement effect was not verified; diagnostic cannot establish responsiveness".into(),
        );
    }
    Ok(())
}
async fn watch(brain: &mut Brain, count: u32) -> Result<()> {
    let profile: Profile = serde_json::from_slice(&private_read(&brain.cfg.profile, 16384)?)?;
    let started = Instant::now();
    brain.journal.event(
        "lane_watch_started",
        json!({"uuid":profile.credential.player_uuid,"samples":count,"models_called":0}),
    )?;
    let mut timings = Vec::new();
    for sample in 0..count {
        let sent_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis();
        let sent = Instant::now();
        let observed = brain.client.observe().await;
        let ms = sent.elapsed().as_millis();
        match observed {
            Ok(o) => {
                if o.player["uuid"] != profile.credential.player_uuid {
                    return Err("Observation crossed character identities".into());
                }
                brain.journal.event("lane_watch_sample",json!({"sample":sample,"sent_at_unix_ms":sent_at,"latency_ms":ms,"uuid":o.player["uuid"],"revision":o.revision,"health":o.player["health"],"position":o.player["position"]}))?;
                timings.push(ms);
            }
            Err(e) => {
                brain.journal.event("lane_watch_interrupted",json!({"sample":sample,"sent_at_unix_ms":sent_at,"latency_ms":ms,"code":e.code,"message":e.message,"completed_samples":timings.len()}))?;
                return Err(e.into());
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    timings.sort_unstable();
    brain.journal.event("lane_watch_finished",json!({"samples":timings.len(),"p50_ms":timings[(timings.len()-1)/2],"p95_ms":timings[(timings.len()*95).div_ceil(100)-1],"max_ms":timings.last(),"elapsed_ms":started.elapsed().as_millis(),"interpretation":"Native transport diagnostic only; not autonomous Survival or threat-reaction evidence"}))?;
    Ok(())
}
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let path = args.first().ok_or(
        "Usage: control-latency-probe PRIVATE_CONFIG.json [watch SAMPLE_COUNT] (inside container)",
    )?;
    let finite = matches!(args.as_slice(),[_,command] if command=="finite");
    let samples = match args.as_slice() {
        [_] => None,
        [_, command] if command == "finite" => None,
        [_, command, count] if command == "watch" => {
            let count: u32 = count.parse()?;
            if !(1..=120).contains(&count) {
                return Err("Watch samples must be 1–120".into());
            }
            Some(count)
        }
        _ => return Err("Unexpected diagnostic arguments".into()),
    };
    let cfg: Config = serde_json::from_slice(&private_read(Path::new(&path), 16384)?)?;
    let mut brain = Brain::open(cfg).await?;
    let result = if finite {
        finite::run(&mut brain).await
    } else if let Some(count) = samples {
        watch(&mut brain, count).await
    } else {
        probe(&mut brain).await
    };
    let disconnected =
        tokio::time::timeout(Duration::from_secs(15), brain.client.disconnect()).await;
    brain.journal.event("control_latency_probe_exit",json!({"disconnected":matches!(disconnected,Ok(Ok(()))),"error":result.as_ref().err().map(ToString::to_string)}))?;
    result?;
    if !matches!(disconnected, Ok(Ok(()))) {
        return Err("Diagnostic stopped but SDK disconnect was not confirmed".into());
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_geometry_or_headroom_never_becomes_a_walking_fixture() {
        let mut o = Observation {
            revision: "fixture".into(),
            player: json!({"position":[0.5,1.,0.5],"on_ground":true}),
            world: json!({"blocks":[]}),
            menu: Value::Null,
        };
        assert!(waypoint(&o).is_err());
        let mut blocks = Vec::new();
        for x in -4..=4 {
            for z in -4..=4 {
                for y in 0..=2 {
                    blocks.push(json!({"position":[x,y,z],"block":if y==0{"minecraft:stone"}else{"minecraft:air"},"collision":if y==0{json!([[0.,0.,0.,1.,1.,1.]])}else{json!([])}}));
                }
            }
        }
        o.world["blocks"] = json!(blocks);
        assert_eq!(waypoint(&o).unwrap(), [3., 1., 0.5]);
        for b in o.world["blocks"].as_array_mut().unwrap() {
            if b["position"][1] == 2 {
                b["collision"] = json!([[0., 0., 0., 1., 1., 1.]]);
            }
        }
        assert!(waypoint(&o).is_err());
    }
}
