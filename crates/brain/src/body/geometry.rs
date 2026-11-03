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
/// Client behavior only: ranged actors must not be declared safe at melee range.
pub fn threat_radius(entity: &Value) -> f64 {
    if entity["ranged_attack"] == true {
        24.
    } else {
        10.
    }
}
pub fn threatened(o: &Observation) -> bool {
    let Some(start) = point(&o.player["position"]) else {
        return false;
    };
    o.world["entities"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|e| {
            e["hostile"] == true
                && e["visible"] == true
                && point(&e["position"]).is_some_and(|p| {
                    (0..3).map(|i| (p[i] - start[i]).powi(2)).sum::<f64>()
                        < threat_radius(e).powi(2)
                })
        })
}
// A descent is not a diagonal ramp through the ground. Validate the observed
// surfaces under each footprint sample and its vertical clearance; the finite
// server control still validates the actual collision/gravity trajectory.
fn descending_route(
    cells: &Cells<'_>,
    start: [f64; 3],
    target: [f64; 3],
    obstacles: &[[f64; 6]],
) -> bool {
    let mut previous = start[1];
    (0..=20).all(|i| {
        let t = f64::from(i) / 20.;
        let x = start[0] + (target[0] - start[0]) * t;
        let z = start[2] + (target[2] - start[2]) * t;
        let mut highest = target[1];
        for dx in [-0.299, 0.299] {
            for dz in [-0.299, 0.299] {
                let (px, pz) = (x + dx, z + dz);
                let floor = obstacles
                    .iter()
                    .filter(|b| {
                        b[4] >= target[1] - 0.001
                            && b[4] <= start[1] + 0.001
                            && px >= b[0]
                            && px <= b[3]
                            && pz >= b[2]
                            && pz <= b[5]
                            && block(
                                cells,
                                [
                                    px.floor() as i32,
                                    (b[4] - 0.01).floor() as i32,
                                    pz.floor() as i32,
                                ],
                            )
                            .is_some_and(|b| !hazardous(b))
                    })
                    .map(|b| b[4])
                    .max_by(f64::total_cmp);
                let Some(floor) = floor else {
                    return false;
                };
                highest = highest.max(floor);
            }
        }
        if highest > previous + 0.001 {
            return false;
        }
        let safe = if (highest - previous).abs() < 0.001 {
            clear(cells, [x, highest, z], obstacles)
        } else {
            (0..=4).all(|n| {
                clear(
                    cells,
                    [x, previous + (highest - previous) * f64::from(n) / 4., z],
                    obstacles,
                )
            })
        };
        previous = highest;
        safe
    })
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
        if e["hostile"] == true && d < threat_radius(e) {
            threats.push((p, e["ranged_attack"] == true));
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
                    threats.push((p, true));
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
            .map(|(e, _)| ((p[0] - e[0]).powi(2) + (p[2] - e[2]).powi(2)).sqrt())
            .fold(30., f64::min);
        let cover = threats
            .iter()
            .filter(|(e, _)| covered(p, *e, &obstacles))
            .map(|(_, ranged)| if *ranged { 24. } else { 4. })
            .sum::<f64>();
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
        distance * 2. + cover + sidestep * 8.
    };
    let current = score(start) - if burning { 10. } else { 0. };
    let mut best = None;
    let mut best_score = current + 0.4;
    // Narrow ledges rarely line up with a fixed angle and stride. Include
    // observed collision-top centres as landings, without inventing support.
    let mut candidates = Vec::new();
    for b in &obstacles {
        let p = [(b[0] + b[3]) / 2., b[4], (b[2] + b[5]) / 2.];
        let horizontal = ((p[0] - start[0]).powi(2) + (p[2] - start[2]).powi(2)).sqrt();
        if (0.35..=3.2).contains(&horizontal) && (-1.001..=1.001).contains(&(p[1] - start[1])) {
            candidates.push(p);
        }
    }
    for n in 0..16 {
        let a = f64::from(n) * std::f64::consts::PI / 8.;
        for length in [2.8, 1.8, 0.9] {
            for dy in [0., 1., -1.] {
                candidates.push([
                    start[0] + a.cos() * length,
                    start[1] + dy,
                    start[2] + a.sin() * length,
                ]);
            }
        }
    }
    for p in candidates {
        let dy = p[1] - start[1];
        let length = ((p[0] - start[0]).powi(2) + (p[2] - start[2]).powi(2)).sqrt();
        for jump in [false, true] {
            if (dy > 0.1 && !jump) || (jump && o.player["on_ground"] == false) {
                continue;
            }
            if blocked.contains(&target_key(p)) {
                continue;
            }
            if !supported(&cells, p, &obstacles) || !clear(&cells, p, &obstacles) {
                continue;
            }
            let route = if dy < 0. && !jump {
                descending_route(&cells, start, p, &obstacles)
            } else {
                (0..=20).all(|i| {
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
                })
            };
            if !route {
                continue;
            }
            let value = score(p) - length * 0.1 - if jump { 0.2 } else { 0. };
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
    fn a_narrow_lower_landing_across_a_gap_uses_an_observed_jump_target() {
        let mut o = world();
        o.player = json!({"position":[0.375629,2.,0.701009],"on_ground":true});
        o.world["entities"][0]["position"] = json!([3.7, 1., -2.4]);
        for b in o.world["blocks"].as_array_mut().unwrap() {
            let p = b["position"].clone();
            b["block"] = json!("minecraft:air");
            b["collision"] = json!([]);
            if p == json!([0, 0, 0]) || p == json!([0, 1, 0]) {
                b["block"] = json!("minecraft:stone");
                b["collision"] = json!([[0., 0., 0., 1., 1., 1.]]);
            } else if p == json!([-1, 0, 2]) {
                b["block"] = json!("mod:narrow_landing");
                b["collision"] = json!([[0.2, 0., 0.2, 0.8, 1., 0.8]]);
            }
        }
        let escape = evade(&o).expect("A lower landing requires jumping across the observed gap");
        assert!(escape.jump);
        assert_eq!(
            [escape.target.x, escape.target.y, escape.target.z],
            [-0.5, 1., 2.5]
        );
        let mut lava = o.clone();
        for b in lava.world["blocks"].as_array_mut().unwrap() {
            if b["position"] == json!([-1, 0, 2]) {
                b["block"] = json!("minecraft:lava");
            }
        }
        assert!(
            evade(&lava).is_none(),
            "A known hazard cannot become a landing"
        );
        for b in o.world["blocks"].as_array_mut().unwrap() {
            if b["position"][1] == 4 {
                b["block"] = json!("minecraft:stone");
                b["collision"] = json!([[0., 0., 0., 1., 1., 1.]]);
            }
        }
        assert!(
            evade(&o).is_none(),
            "The jump arc must respect an observed low ceiling"
        );
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
    #[test]
    fn ranged_threats_remain_active_beyond_the_melee_cutoff() {
        let mut o = world();
        o.world["entities"][0]["position"] = json!([18.5, 1., 0.5]);
        o.world["entities"][0]["ranged_attack"] = json!(true);
        assert!(threatened(&o));
        assert!(evade(&o).is_some_and(|e| e.target.x < 0.));
        o.world["entities"][0]["ranged_attack"] = json!(false);
        assert!(!threatened(&o));
        assert!(evade(&o).is_none());
    }
    #[test]
    fn a_one_block_descent_is_not_rejected_as_an_unsupported_diagonal_ramp() {
        let mut o = world();
        o.player["position"] = json!([0.5, 2., 0.5]);
        o.world["entities"][0]["position"] = json!([3.5, 2., 0.5]);
        for b in o.world["blocks"].as_array_mut().unwrap() {
            if b["position"][0].as_i64().unwrap() >= 0 && b["position"][1] == 1 {
                b["block"] = json!("minecraft:stone");
                b["collision"] = json!([[0., 0., 0., 1., 1., 1.]]);
            }
        }
        let e = evade(&o).expect("observed lower ground must be available for retreat");
        assert!(e.target.x < 0. && e.target.y == 1. && !e.jump);
        let cells: Cells<'_> = o.world["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| {
                let p = point(&b["position"]).unwrap();
                (p.map(|n| n as i32), b)
            })
            .collect();
        assert!(descending_route(
            &cells,
            [0.5, 2., 0.5],
            [-2.3, 1., 0.5],
            &boxes(&o)
        ));
        for b in o.world["blocks"].as_array_mut().unwrap() {
            if b["position"][0] == -1 && b["position"][1] == 0 {
                b["block"] = json!("minecraft:air");
                b["collision"] = json!([]);
            }
        }
        let cells: Cells<'_> = o.world["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| {
                let p = point(&b["position"]).unwrap();
                (p.map(|n| n as i32), b)
            })
            .collect();
        assert!(
            !descending_route(&cells, [0.5, 2., 0.5], [-2.3, 1., 0.5], &boxes(&o)),
            "a gap must not become a safe descent"
        );
    }
}
