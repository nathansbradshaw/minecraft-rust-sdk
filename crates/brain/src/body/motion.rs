//! Short client predictions from native feet samples, not server impulse velocity.
use super::geometry::point;
use minecraft_sdk::Observation;
use serde_json::{Value, json};
#[derive(Default)]
pub(super) struct Motion {
    previous: Option<(String, i64, [f64; 3])>,
}
impl Motion {
    pub fn sample(&mut self, o: &Observation, owned: &str) -> Option<Value> {
        let p = point(&o.player["position"])?;
        let tick = o.world["game_time"].as_i64()?;
        let scope = format!(
            "{}:{}:{}:{}",
            o.player["uuid"].as_str()?,
            o.player["dimension"].as_str()?,
            o.player["entity_id"].as_i64()?,
            o.world["sample_clock"]["epoch"].as_str()?
        );
        let previous = self.previous.replace((scope.clone(), tick, p));
        let (old_scope, old_tick, old) = previous?;
        let dt = tick.checked_sub(old_tick)?;
        if scope != old_scope
            || !(1..=8).contains(&dt)
            || o.player["on_ground"] != true
            || o.player["in_water"] == true
            || o.player["in_lava"] == true
            || o.player["health"].as_f64()? <= 0.
        {
            return None;
        }
        let active = &o.player["control"];
        if owned.is_empty()
            || active["action_id"].as_str() != Some(owned)
            || active["outcome"] != "running"
            || active["cancel_requested"] == true
            || active["progress"]["releasing"] != false
            || active["progress"]["elapsed_ms"].as_u64()? >= 2300
        {
            return None;
        }
        let v: [f64; 3] = std::array::from_fn(|i| (p[i] - old[i]) / dt as f64);
        let speed = v[0].hypot(v[2]);
        let target = point(&active["target"])?;
        let length = (target[0] - p[0]).hypot(target[2] - p[2]);
        let alignment = (v[0] * (target[0] - p[0]) + v[2] * (target[2] - p[2])) / (speed * length);
        if !(0.05..=0.9).contains(&speed)
            || v[1].abs() > 0.02
            || length < 0.6
            || alignment < 0.95
            || (target[1] - p[1]).abs() > 0.05
        {
            return None;
        }
        Some(
            json!({"kind":"finite_input_extrapolation","velocity_per_tick":v,"target":target,
            "sample_ticks":[old_tick,tick],"sample_positions":[old,p],"horizon_ticks":12,
            "scope":"Client extrapolation while own grounded finite input continues,then conservatively stops at target; not native impulse velocity or guaranteed future motion"}),
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn scene(tick: i64, x: f64) -> Observation {
        Observation {
            revision: tick.to_string(),
            menu: Value::Null,
            world: json!({"game_time":tick,"sample_clock":{"epoch":"server"}}),
            player: json!({"uuid":"own","entity_id":1,"dimension":"minecraft:overworld","position":[x,1.,0.5],
                "health":20.,"on_ground":true,"velocity":[0.,-0.0784,0.],"control":{"action_id":"own-control","outcome":"running",
                    "target":[4.5,1.,0.5],"progress":{"releasing":false,"elapsed_ms":200}}}),
        }
    }
    #[test]
    fn predictions_use_native_displacement_and_refuse_foreign_releasing_or_changed_lives() {
        for mode in [
            "valid",
            "foreign",
            "releasing",
            "respawn",
            "clock",
            "airborne",
        ] {
            let mut tracker = Motion::default();
            assert!(tracker.sample(&scene(10, 0.5), "own-control").is_none());
            let mut next = scene(13, 1.1);
            match mode {
                "foreign" => next.player["control"]["action_id"] = json!("other"),
                "releasing" => next.player["control"]["progress"]["releasing"] = json!(true),
                "respawn" => next.player["entity_id"] = json!(2),
                "clock" => next.world["game_time"] = json!(9),
                "airborne" => next.player["on_ground"] = json!(false),
                _ => {}
            }
            let result = tracker.sample(&next, "own-control");
            if mode == "valid" {
                assert!(
                    (result.unwrap()["velocity_per_tick"][0].as_f64().unwrap() - 0.2).abs() < 1e-9
                );
            } else {
                assert!(result.is_none(), "{mode}");
            }
        }
    }
}
