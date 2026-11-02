use minecraft_sdk::{Observation, Position};
use serde_json::Value;
use std::collections::HashMap;
type Cells<'a> = HashMap<[i32; 3], &'a Value>;

pub fn point(v: &Value) -> Option<[f64; 3]> {
    Some([v[0].as_f64()?, v[1].as_f64()?, v[2].as_f64()?])
}
fn hazardous(b: &Value) -> bool {
    b["dangerous"] == true
        || [
            "minecraft:lava",
            "minecraft:water",
            "minecraft:fire",
            "minecraft:soul_fire",
            "minecraft:magma_block",
            "minecraft:campfire",
            "minecraft:soul_campfire",
            "minecraft:cactus",
            "minecraft:sweet_berry_bush",
            "minecraft:powder_snow",
        ]
        .iter()
        .any(|id| b["block"] == *id)
}
fn block<'a>(cells: &Cells<'a>, p: [i32; 3]) -> Option<&'a Value> {
    cells.get(&p).copied()
}
fn boxes(o: &Observation) -> Vec<[f64; 6]> {
    let mut out = Vec::new();
    for b in o.world["blocks"].as_array().into_iter().flatten() {
        let Some(p) = point(&b["position"]) else {
            continue;
        };
        for s in b["collision"].as_array().into_iter().flatten() {
            let mut bb = [0.; 6];
            let mut valid = true;
            for i in 0..6 {
                if let Some(v) = s[i].as_f64() {
                    bb[i] = v + p[i % 3];
                } else {
                    valid = false;
                }
            }
            if valid {
                out.push(bb);
            }
        }
    }
    out
}
fn intersects(p: [f64; 3], bb: &[f64; 6]) -> bool {
    let body = [
        p[0] - 0.3,
        p[1] + 0.0001,
        p[2] - 0.3,
        p[0] + 0.3,
        p[1] + 1.8,
        p[2] + 0.3,
    ];
    (0..3).all(|i| body[i + 3] > bb[i] + 1e-6 && body[i] < bb[i + 3] - 1e-6)
}
fn clear(cells: &Cells<'_>, p: [f64; 3], obstacles: &[[f64; 6]]) -> bool {
    for x in [(p[0] - 0.3).floor() as i32, (p[0] + 0.3).floor() as i32] {
        for z in [(p[2] - 0.3).floor() as i32, (p[2] + 0.3).floor() as i32] {
            for y in p[1].floor() as i32..=(p[1] + 1.8).floor() as i32 {
                if block(cells, [x, y, z]).is_none_or(hazardous) {
                    return false;
                }
            }
        }
    }
    !obstacles.iter().any(|b| intersects(p, b))
}
fn supported(cells: &Cells<'_>, p: [f64; 3], obstacles: &[[f64; 6]]) -> bool {
    [-0.25, 0.25].iter().all(|dx| {
        [-0.25, 0.25].iter().all(|dz| {
            let x = p[0] + dx;
            let z = p[2] + dz;
            let below = [
                x.floor() as i32,
                (p[1] - 0.01).floor() as i32,
                z.floor() as i32,
            ];
            block(cells, below).is_some_and(|b| !hazardous(b))
                && obstacles.iter().any(|b| {
                    (b[4] - p[1]).abs() < 0.001 && x >= b[0] && x <= b[3] && z >= b[2] && z <= b[5]
                })
        })
    })
}
fn covered(p: [f64; 3], threat: [f64; 3], obstacles: &[[f64; 6]]) -> bool {
    (1..40).any(|n| {
        let t = f64::from(n) / 40.;
        let ray = [
            p[0] + (threat[0] - p[0]) * t,
            p[1] + 1.6 + (threat[1] - p[1]) * t,
            p[2] + (threat[2] - p[2]) * t,
        ];
        obstacles
            .iter()
            .any(|b| (0..3).all(|i| ray[i] > b[i] && ray[i] < b[i + 3]))
    })
}
#[derive(Clone, Debug)]
pub struct Escape {
    pub target: Position,
    pub jump: bool,
    pub reason: String,
}
pub fn incoming(o: &Observation) -> bool {
    let Some(start) = point(&o.player["position"]) else {
        return false;
    };
    o.world["entities"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|e| {
            if e["visible"] != true || e["projectile"] != true {
                return false;
            }
            let (Some(p), Some(v)) = (point(&e["position"]), point(&e["velocity"])) else {
                return false;
            };
            let vv = v.iter().map(|x| x * x).sum::<f64>();
            if vv <= 0.0001 {
                return false;
            }
            let t = (0..3)
                .map(|i| (start[i] + if i == 1 { 0.9 } else { 0. } - p[i]) * v[i])
                .sum::<f64>()
                / vv;
            let miss = (0..3)
                .map(|i| (p[i] + v[i] * t - start[i] - if i == 1 { 0.9 } else { 0. }).powi(2))
                .sum::<f64>()
                .sqrt();
            (0.0..=12.).contains(&t) && miss < 1.5
        })
}
pub fn target_key(p: [f64; 3]) -> [i32; 3] {
    p.map(|v| (v * 10.).round() as i32)
}
pub fn evade(o: &Observation) -> Option<Escape> {
    evade_avoiding(o, &[])
}
pub fn evade_avoiding(o: &Observation, blocked: &[[i32; 3]]) -> Option<Escape> {
    let start = point(&o.player["position"])?;
    let mut threats = Vec::new();
    let mut projectile_danger = false;
    let mut incoming_directions = Vec::new();
    for e in o.world["entities"].as_array().into_iter().flatten() {
        if e["visible"] != true {
            continue;
        }
        let Some(p) = point(&e["position"]) else {
            continue;
        };
        let d = (0..3)
            .map(|i| (p[i] - start[i]).powi(2))
            .sum::<f64>()
            .sqrt();
        if e["hostile"] == true && d < 10. {
            threats.push(p);
        }
        if e["projectile"] == true
            && let Some(v) = point(&e["velocity"])
        {
            let vv = v.iter().map(|x| x * x).sum::<f64>();
            if vv > 0.0001 {
                let t = (0..3)
                    .map(|i| (start[i] + if i == 1 { 0.9 } else { 0. } - p[i]) * v[i])
                    .sum::<f64>()
                    / vv;
                let miss = (0..3)
                    .map(|i| (p[i] + v[i] * t - start[i] - if i == 1 { 0.9 } else { 0. }).powi(2))
                    .sum::<f64>()
                    .sqrt();
                if (0.0..=12.).contains(&t) && miss < 1.5 {
                    projectile_danger = true;
                    threats.push(p);
                    incoming_directions.push(v);
                }
            }
        }
    }
    let burning =
        o.player["fire_ticks"].as_i64().is_some_and(|n| n > 0) || o.player["in_lava"] == true;
    if threats.is_empty() && !burning {
        return None;
    }
    threats.truncate(16);
    let cells: Cells<'_> = o.world["blocks"]
        .as_array()?
        .iter()
        .filter_map(|b| point(&b["position"]).map(|p| ([p[0] as i32, p[1] as i32, p[2] as i32], b)))
        .collect();
    let obstacles = boxes(o);
    let score = |p: [f64; 3]| -> f64 {
        let distance = threats
            .iter()
            .map(|e| ((p[0] - e[0]).powi(2) + (p[2] - e[2]).powi(2)).sqrt())
            .fold(30., f64::min);
        let cover = threats
            .iter()
            .filter(|e| covered(p, **e, &obstacles))
            .count() as f64;
        let sidestep = incoming_directions
            .iter()
            .map(|v| {
                let len = (v[0] * v[0] + v[2] * v[2]).sqrt();
                if len > 0.001 {
                    ((p[0] - start[0]) * (-v[2]) + (p[2] - start[2]) * v[0]).abs() / len
                } else {
                    0.
                }
            })
            .fold(0., f64::max);
        distance * 2. + cover * 4. + sidestep * 8.
    };
    let current = score(start) - if burning { 10. } else { 0. };
    let mut best = None;
    let mut best_score = current + 0.4;
    for n in 0..16 {
        let a = f64::from(n) * std::f64::consts::PI / 8.;
        for length in [2.8, 1.8, 0.9] {
            for dy in [0., 1., -1.] {
                let p = [
                    start[0] + a.cos() * length,
                    start[1] + dy,
                    start[2] + a.sin() * length,
                ];
                if blocked.contains(&target_key(p)) {
                    continue;
                }
                if !supported(&cells, p, &obstacles) || !clear(&cells, p, &obstacles) {
                    continue;
                }
                let jump = dy > 0.1;
                let route = (0..=20).all(|i| {
                    let t = f64::from(i) / 20.;
                    let mut q = [
                        start[0] + (p[0] - start[0]) * t,
                        start[1] + dy * t,
                        start[2] + (p[2] - start[2]) * t,
                    ];
                    if jump {
                        q[1] += 1.25 * (std::f64::consts::PI * t).sin();
                    }
                    clear(&cells, q, &obstacles) && (jump || supported(&cells, q, &obstacles))
                });
                if !route {
                    continue;
                }
                let value = score(p) - length * 0.1;
                if value > best_score {
                    best_score = value;
                    best = Some(Escape {
                        target: Position {
                            x: p[0],
                            y: p[1],
                            z: p[2],
                        },
                        jump,
                        reason: if projectile_danger {
                            "evade incoming projectile"
                        } else if burning {
                            "leave burning terrain"
                        } else {
                            "increase threat distance and seek cover"
                        }
                        .into(),
                    });
                }
            }
        }
    }
    best
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn world() -> Observation {
        let mut blocks = Vec::new();
        for x in -4..=4 {
            for z in -4..=4 {
                for y in -1..=4 {
                    blocks.push(json!({"position":[x,y,z],"block":if y==0{"minecraft:stone"}else{"minecraft:air"},"collision":if y==0{json!([[0.,0.,0.,1.,1.,1.]])}else{json!([])}}));
                }
            }
        }
        Observation {
            revision: "fixture".into(),
            player: json!({"position":[0.5,1.,0.5]}),
            world: json!({"blocks":blocks,"entities":[{"visible":true,"hostile":true,"position":[2.5,1.,0.5]}]}),
            menu: Value::Null,
        }
    }
    #[test]
    fn escape_increases_distance_without_assuming_unknown_terrain_is_air() {
        let mut o = world();
        let escape = evade(&o).unwrap();
        assert!(escape.target.x < 0.);
        o.world["blocks"] = json!([]);
        assert!(evade(&o).is_none());
    }
    #[test]
    fn invisible_mobs_and_stationary_arrows_do_not_trigger_blind_flight() {
        let mut o = world();
        o.world["entities"][0]["visible"] = json!(false);
        o.world["entities"].as_array_mut().unwrap().push(
            json!({"visible":true,"projectile":true,"position":[1.,1.,0.5],"velocity":[0.,0.,0.]}),
        );
        assert!(evade(&o).is_none());
    }
    #[test]
    fn lava_on_the_only_retreat_route_is_not_treated_as_cover() {
        let mut o = world();
        for b in o.world["blocks"].as_array_mut().unwrap() {
            if b["position"][0].as_i64().unwrap() < 0 {
                b["block"] = json!("minecraft:lava");
            }
        }
        let e = evade(&o);
        assert!(e.is_none_or(|e| e.target.x >= 0.3));
    }
}
