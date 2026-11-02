//! Read-only, bounded predictions using the same admission planner as controls.
use super::*;
use std::{
    collections::{BTreeSet, HashMap},
    hash::{Hash, Hasher},
};

pub(super) const CAPABILITY: &str = "minecraft:movement_options";
pub(super) const INTERVAL_MS: u64 = 250;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    #[serde(default)]
    offset: usize,
    target: Option<[f64; 3]>,
}
fn parse(input: &Value) -> AnyResult<Input> {
    let input: Input = serde_json::from_value(input.clone())?;
    if input.offset > 1024
        || input
            .target
            .is_some_and(|p| p.iter().any(|n| !n.is_finite()))
    {
        return Err("Movement option input exceeds bounds".into());
    }
    Ok(input)
}
pub(super) fn validate(input: &Value) -> AnyResult<()> {
    parse(input).map(|_| ())
}
pub(super) fn schema() -> Value {
    json!({"description":"Read-only locally reachable grounded waypoints using this adapter's collision/gravity admission. Predictions are not game effects. Follow pagination only while scene and origin match.","input_schema":{"type":"object","additionalProperties":false,"properties":{"offset":{"type":"integer","minimum":0,"maximum":1024},"target":{"type":"array","items":{"type":"number"},"minItems":3,"maxItems":3}}},"max_evaluations":64,"min_interval_ms":INTERVAL_MS})
}
fn body_cells(p: [f64; 3]) -> impl Iterator<Item = [i32; 3]> {
    let lo = [
        (p[0] - 0.3 + 1e-7).floor() as i32,
        (p[1] - 0.01).floor() as i32,
        (p[2] - 0.3 + 1e-7).floor() as i32,
    ];
    let hi = [
        (p[0] + 0.3 - 1e-7).floor() as i32,
        (p[1] + 1.8 - 1e-7).floor() as i32,
        (p[2] + 0.3 - 1e-7).floor() as i32,
    ];
    (lo[0]..=hi[0]).flat_map(move |x| {
        (lo[1]..=hi[1]).flat_map(move |y| (lo[2]..=hi[2]).map(move |z| [x, y, z]))
    })
}
pub(super) fn page(before: &Observation, input: &Value, max_ticks: u16) -> AnyResult<Value> {
    let input = parse(input)?;
    if before.player["on_ground"] != true {
        return Err("Movement options require a grounded observation; wait for the finite control to release".into());
    }
    let start = position(before)?;
    let boxes = collision_boxes(before)?;
    if boxes.len() > 2048 {
        return Err("Movement option collision budget exceeded".into());
    }
    let rows = before.world["blocks"].as_array().ok_or("Terrain missing")?;
    if rows.len() > 729 {
        return Err("Movement option terrain exceeds observation budget".into());
    }
    let mut cells = HashMap::new();
    let mut candidates = Vec::new();
    for row in rows {
        let p = &row["position"];
        let key = [
            p[0].as_i64().ok_or("Block x missing")? as i32,
            p[1].as_i64().ok_or("Block y missing")? as i32,
            p[2].as_i64().ok_or("Block z missing")? as i32,
        ];
        cells.insert(key, row["block"].as_str().ok_or("Registry ID missing")?);
        let shapes = row["collision"]
            .as_array()
            .ok_or("Collision shapes missing")?;
        if shapes.len() > 64 {
            return Err("Collision surface budget exceeded".into());
        }
        for shape in shapes {
            let target = [
                f64::from(key[0]) + 0.5,
                f64::from(key[1]) + shape[4].as_f64().ok_or("Surface height missing")?,
                f64::from(key[2]) + 0.5,
            ];
            if target.iter().any(|v| !v.is_finite()) {
                return Err("Invalid surface coordinate".into());
            }
            if (target[0] - start[0]).abs() <= 3.5
                && (target[2] - start[2]).abs() <= 3.5
                && target[1] - start[1] <= 1.25
                && start[1] - target[1] <= 3.0
                && (0..3).map(|i| (target[i] - start[i]).powi(2)).sum::<f64>() > 0.04
            {
                candidates.push(target);
            }
        }
    }
    if let Some(target) = input.target {
        if target.iter().any(|n| !n.is_finite()) {
            return Err("Invalid target coordinate".into());
        }
        if (target[0] - start[0]).abs() <= 3.5
            && (target[2] - start[2]).abs() <= 3.5
            && target[1] - start[1] <= 1.25
            && start[1] - target[1] <= 3.0
        {
            candidates.push(target);
        }
    }
    candidates.sort_by(|a, b| {
        let d = |p: &[f64; 3]| (0..3).map(|i| (p[i] - start[i]).powi(2)).sum::<f64>();
        d(a).total_cmp(&d(b))
            .then_with(|| a[0].total_cmp(&b[0]))
            .then_with(|| a[1].total_cmp(&b[1]))
            .then_with(|| a[2].total_cmp(&b[2]))
    });
    candidates.dedup();
    if candidates.len() > 1024 {
        return Err("Movement option surface budget exceeded".into());
    }
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    input.target.map(|p| p.map(f64::to_bits)).hash(&mut hash);
    before.player["dimension"].to_string().hash(&mut hash);
    before.player["movement_speed"].to_string().hash(&mut hash);
    before.player["sneaking"].to_string().hash(&mut hash);
    for bounds in boxes {
        for n in bounds {
            n.to_bits().hash(&mut hash);
        }
    }
    // Include known air and runtime IDs as well as collision shapes: unknown
    // columns and changed hazards cannot masquerade as the same paged scene.
    serde_json::to_string(rows)?.hash(&mut hash);
    let mut end = input.offset.min(candidates.len());
    let started = Instant::now();
    let mut options = Vec::new();
    for target in candidates.iter().skip(input.offset).take(64) {
        if end > input.offset && started.elapsed() >= Duration::from_millis(40) {
            break;
        }
        end += 1;
        for jump in [false, true] {
            let Ok(frames) = control::preview(before, *target, jump) else {
                continue;
            };
            if frames.len() + 4 > usize::from(max_ticks) {
                continue;
            }
            let mut touched = BTreeSet::new();
            let known = frames.iter().all(|(p, _)| {
                body_cells(*p).all(|key| match cells.get(&key) {
                    Some(id) => {
                        touched.insert(*id);
                        true
                    }
                    None => false,
                })
            });
            if !known {
                continue;
            }
            options.push(json!({"target":target,"jump":jump,"max_ticks":max_ticks,"duration_ticks":frames.len(),"touched_blocks":touched}));
            break;
        }
    }
    Ok(
        json!({"format":"minecraft-movement-options-v1","origin":{"position":start,"dimension":before.player["dimension"]},"scene":format!("{:016x}",hash.finish()),"options":options,"complete":end==candidates.len(),"next_offset":if end<candidates.len(){Some(end)}else{None},"retry_after_ms":INTERVAL_MS,"prediction_not_effect_proof":true}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn room(wall: bool) -> Observation {
        let mut blocks = Vec::new();
        for x in -4..=4 {
            for y in -3..=4 {
                for z in -4..=4 {
                    let solid = y == 0 || (wall && x == 1 && (1..=3).contains(&y));
                    blocks.push(json!({"position":[x,y,z],"block":if solid{"fixture:solid"}else{"fixture:air"},"collision":if solid{json!([[0,0,0,1,1,1]])}else{json!([])}}));
                }
            }
        }
        Observation {
            revision: "fixture".into(),
            player: json!({"uuid":"fixture","dimension":"fixture:world","position":[0.5,1.0,0.5],"on_ground":true,"movement_speed":0.1}),
            world: json!({"blocks":blocks}),
            menu: Value::Null,
        }
    }
    fn all(o: &Observation) -> Vec<Value> {
        let mut offset = 0;
        let mut rows = Vec::new();
        loop {
            let result = page(o, &json!({"offset":offset}), 100).unwrap();
            rows.extend(result["options"].as_array().unwrap().iter().cloned());
            if result["complete"] == true {
                break;
            }
            offset = result["next_offset"].as_u64().unwrap() as usize;
        }
        rows
    }
    #[test]
    fn predicts_flat_waypoints_without_clipping_a_wall_or_claiming_an_effect() {
        let flat = room(false);
        let routes = all(&flat);
        assert!(routes.iter().any(|r| r["target"] == json!([2.5, 1.0, 0.5])));
        assert!(page(&flat, &json!({}), 100).unwrap()["prediction_not_effect_proof"] == true);
        let walled = room(true);
        assert!(
            !all(&walled)
                .iter()
                .any(|r| r["target"][0].as_f64().unwrap() > 1.)
        );
    }
    #[test]
    fn unknown_air_is_not_a_reachable_route_and_queries_are_closed() {
        let mut o = room(false);
        o.world["blocks"]
            .as_array_mut()
            .unwrap()
            .retain(|b| b["position"] != json!([1, 2, 0]));
        assert!(
            !all(&o)
                .iter()
                .any(|r| r["target"] == json!([1.5, 1.0, 0.5]))
        );
        assert!(page(&o, &json!({"console":"op"}), 100).is_err());
        assert!(page(&o, &json!({"offset":1025}), 100).is_err());
    }
}
