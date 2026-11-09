use minecraft_sdk::{Observation, Position};
use serde_json::Value;
use std::collections::HashMap;
mod boundary;
mod cover;
type Cells<'a> = HashMap<[i32; 3], &'a Value>;

pub fn point(v: &Value) -> Option<[f64; 3]> {
    Some([v[0].as_f64()?, v[1].as_f64()?, v[2].as_f64()?])
}
pub(crate) const KNOWN_HAZARDS: &[&str] = &[
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
];
fn hazardous(b: &Value) -> bool {
    b["dangerous"] == true || KNOWN_HAZARDS.iter().any(|id| b["block"] == *id)
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
fn occluded(from: [f64; 3], to: [f64; 3], obstacles: &[[f64; 6]]) -> bool {
    (1..40).any(|n| {
        let t = f64::from(n) / 40.;
        let ray: [f64; 3] = std::array::from_fn(|i| from[i] + (to[i] - from[i]) * t);
        obstacles
            .iter()
            .any(|b| (0..3).all(|i| ray[i] > b[i] && ray[i] < b[i + 3]))
    })
}
fn covered(p: [f64; 3], threat: [f64; 3], obstacles: &[[f64; 6]]) -> bool {
    occluded(
        [p[0], p[1] + 1.6, p[2]],
        [threat[0], threat[1] + 1.6, threat[2]],
        obstacles,
    )
}
#[derive(Clone, Debug)]
pub struct Escape {
    pub target: Position,
    pub jump: bool,
    pub reason: String,
    /// Remaining currently observed route toward predicted ballistic cover.
    pub cover_path: Vec<Position>,
}
/// A defensive pass-through may carry passive momentum beyond its waypoint.
/// Require a fully observed, supported, level continuation; unknown is unsafe.
pub fn can_pass_waypoint(o: &Observation, route: &Escape) -> bool {
    if !route.cover_path.is_empty()
        || route.jump
        || o.player["on_ground"] != true
        || o.player["in_water"] == true
        || o.player["in_lava"] == true
    {
        return false;
    }
    let Some(start) = point(&o.player["position"]) else {
        return false;
    };
    let end = [route.target.x, route.target.y, route.target.z];
    let dx = end[0] - start[0];
    let dz = end[2] - start[2];
    let length = dx.hypot(dz);
    if length < 0.2 || (end[1] - start[1]).abs() > 0.01 {
        return false;
    }
    let Some(rows) = o.world["blocks"].as_array() else {
        return false;
    };
    let cells: Cells<'_> = rows
        .iter()
        .filter_map(|b| point(&b["position"]).map(|p| ([p[0] as i32, p[1] as i32, p[2] as i32], b)))
        .collect();
    let obstacles = boxes(o);
    (0..=5).all(|n| {
        let extra = f64::from(n) / 5.;
        let p = [
            end[0] + dx / length * extra,
            end[1],
            end[2] + dz / length * extra,
        ];
        clear(&cells, p, &obstacles) && supported(&cells, p, &obstacles)
    })
}
/// A bounded prediction returned by the installed movement adapter. It still
/// needs fresh action admission and observed completion, like every movement.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct Admitted {
    pub target: [f64; 3],
    pub jump: bool,
}
/// Client behavior only: ranged actors must not be declared safe at melee range.
pub fn threat_radius(entity: &Value) -> f64 {
    if super::aerial::diver(entity) {
        32.
    } else if entity["ranged_attack"] == true {
        24.
    } else if entity["creeper"]["powered"] == true {
        14.
    } else {
        10.
    }
}
/// An observed fuse, not a countdown or a guarantee of blast exposure.
pub(super) fn primed_blast(entity: &Value) -> Option<f64> {
    let creeper = &entity["creeper"];
    (creeper["ignited"] == true || creeper["swell_direction"].as_i64().is_some_and(|n| n > 0))
        .then_some(if creeper["powered"] == true { 12. } else { 6. })
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
                && perceived(e)
                && point(&e["position"]).is_some_and(|p| {
                    (0..3).map(|i| (p[i] - start[i]).powi(2)).sum::<f64>()
                        < threat_radius(e).powi(2)
                })
        })
}
fn perceived(entity: &Value) -> bool {
    entity["visible"] == true || entity["client_memory"]["kind"] == "last_visible_ranged_threat"
}
/// Submersion needs protection independently of inference. A natively confirmed
/// breathing surface swimmer may keep its finite input; unknown facts retain
/// the conservative escape policy. No health/shelter prerequisite gates thought.
pub fn water_danger(o: &Observation) -> bool {
    if o.player["in_water"] != true {
        return false;
    }
    let breathing = o.player["eyes_in_water"] == false
        && o.player["air"]
            .as_i64()
            .zip(o.player["max_air"].as_i64())
            .is_some_and(|(air, max)| max > 0 && air >= max);
    !breathing
}

/// Local policy, not permission or a path-completion claim. Prefer an observed
/// dry landing; when air is low, first raise the eyes into observed air. Water
/// is allowed along this route, but unknown cells, other hazards and collisions
/// are not. The adapter separately admits the real held-input trajectory.
fn water_escape(o: &Observation, blocked: &[[i32; 3]]) -> Option<Escape> {
    if !water_danger(o) {
        return None;
    }
    let start = point(&o.player["position"])?;
    let cells: Cells<'_> = o.world["blocks"]
        .as_array()?
        .iter()
        .filter_map(|b| point(&b["position"]).map(|p| (p.map(|v| v as i32), b)))
        .collect();
    let obstacles = boxes(o);
    let passable = |p: [f64; 3]| {
        for x in [(p[0] - 0.3).floor() as i32, (p[0] + 0.3).floor() as i32] {
            for z in [(p[2] - 0.3).floor() as i32, (p[2] + 0.3).floor() as i32] {
                for y in p[1].floor() as i32..=(p[1] + 1.8).floor() as i32 {
                    if block(&cells, [x, y, z])
                        .is_none_or(|b| b["block"] != "minecraft:water" && hazardous(b))
                    {
                        return false;
                    }
                }
            }
        }
        !obstacles.iter().any(|b| intersects(p, b))
    };
    let mut candidates = Vec::new();
    let mut shores = Vec::new();
    for b in &obstacles {
        let p = [(b[0] + b[3]) / 2., b[4], (b[2] + b[5]) / 2.];
        candidates.push(p);
        if supported(&cells, p, &obstacles) && clear(&cells, p, &obstacles) {
            shores.push(p);
        }
    }
    for rise in [0.5, 1., 1.5, 2.] {
        candidates.push([start[0], start[1] + rise, start[2]]);
        for n in 0..16 {
            let angle = f64::from(n) * std::f64::consts::PI / 8.;
            for length in [0.8, 1.6, 2.4] {
                candidates.push([
                    start[0] + angle.cos() * length,
                    start[1] + rise,
                    start[2] + angle.sin() * length,
                ]);
            }
        }
    }
    let low_air = o.player["air"].as_i64().is_some_and(|n| n < 100);
    let shore_distance = |p: [f64; 3]| {
        shores
            .iter()
            .map(|q| (p[0] - q[0]).hypot(p[2] - q[2]))
            .fold(8., f64::min)
    };
    let start_shore_distance = shore_distance(start);
    let mut best: Option<(f64, Escape)> = None;
    for p in candidates {
        let horizontal = (p[0] - start[0]).hypot(p[2] - start[2]);
        let rise = p[1] - start[1];
        if horizontal > 3.
            || !(-0.05..=2.01).contains(&rise)
            || (horizontal < 0.1 && rise < 0.1)
            || blocked.contains(&target_key(p))
            || ![0., 0.6].into_iter().any(|arc| {
                (0..=24).all(|i| {
                    let t = f64::from(i) / 24.;
                    let mut q =
                        std::array::from_fn(|axis| start[axis] + (p[axis] - start[axis]) * t);
                    q[1] += arc * (std::f64::consts::PI * t).sin();
                    passable(q)
                })
            })
        {
            continue;
        }
        let eye = [
            p[0].floor() as i32,
            (p[1] + 1.62).floor() as i32,
            p[2].floor() as i32,
        ];
        let breathable = block(&cells, eye).is_some_and(|b| !hazardous(b));
        let dry = supported(&cells, p, &obstacles) && clear(&cells, p, &obstacles);
        if !breathable && !dry && rise < 0.1 {
            continue;
        }
        let score = if breathable {
            if low_air { 100. } else { 20. }
        } else {
            0.
        } + if dry { 50. } else { 0. }
            + rise * 2.
            - horizontal
            + (start_shore_distance - shore_distance(p)) * 4.
            - if breathable { rise * 3. } else { 0. };
        if best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((
                score,
                Escape {
                    target: Position {
                        x: p[0],
                        y: p[1],
                        z: p[2],
                    },
                    // Upward water steering holds swim input; a ground-jump request
                    // must not persist into the next grounded phase.
                    jump: false,
                    cover_path: Vec::new(),
                    reason: if dry {
                        "leave water for observed dry support"
                    } else if breathable {
                        "swim toward observed breathing space"
                    } else {
                        "swim upward through observed water"
                    }
                    .into(),
                },
            ));
        }
    }
    best.map(|(_, route)| route)
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
    !incoming_projectiles(o).is_empty()
}
pub(super) fn incoming_projectiles(o: &Observation) -> Vec<String> {
    incoming_with_stationary(o, &[])
}
pub(super) fn projectile_key(e: &Value) -> Option<String> {
    e["uuid"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|id| format!("uuid:{id}"))
        .or_else(|| e["entity_id"].as_i64().map(|id| format!("entity:{id}")))
}
pub(super) fn incoming_with_stationary(o: &Observation, stationary: &[String]) -> Vec<String> {
    let Some(start) = point(&o.player["position"]) else {
        return Vec::new();
    };
    let mut ids: Vec<_> = o.world["entities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| {
            if e["visible"] != true || e["projectile"] != true {
                return false;
            }
            if projectile_key(e).is_some_and(|id| stationary.contains(&id)) {
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
            ongoing_input_arrow_danger(o, e).unwrap_or((0.0..=12.).contains(&t) && miss < 1.5)
        })
        .map(|e| projectile_key(e).unwrap_or_else(|| "unidentified-projectile".into()))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}
fn ongoing_input_arrow_danger(o: &Observation, arrow: &Value) -> Option<bool> {
    if !matches!(
        arrow["type"].as_str(),
        Some("minecraft:arrow" | "minecraft:spectral_arrow")
    ) {
        return None;
    }
    let motion = &o.player["client_motion"];
    if motion["kind"] != "finite_input_extrapolation" {
        return None;
    }
    let start = point(&o.player["position"])?;
    let target = point(&motion["target"])?;
    let velocity = point(&motion["velocity_per_tick"])?;
    let p = point(&arrow["position"])?;
    let v = point(&arrow["velocity"])?;
    let speed = velocity[0].hypot(velocity[2]);
    let length = (target[0] - start[0]).hypot(target[2] - start[2]);
    if !(0.05..=0.9).contains(&speed) || length < 0.6 {
        return None;
    }
    let duration = length / speed;
    let body_v = [
        (target[0] - start[0]) / duration,
        0.,
        (target[2] - start[2]) / duration,
    ];
    let closest = |d: [f64; 3], v: [f64; 3], lo: f64, hi: f64, horizontal: bool| {
        let axes = |i: usize| !horizontal || i != 1;
        let vv = (0..3)
            .filter(|&i| axes(i))
            .map(|i| v[i] * v[i])
            .sum::<f64>();
        let t = if vv > 1e-9 {
            -(0..3)
                .filter(|&i| axes(i))
                .map(|i| d[i] * v[i])
                .sum::<f64>()
                / vv
        } else {
            lo
        }
        .clamp(lo, hi);
        (0..3)
            .filter(|&i| axes(i))
            .map(|i| (d[i] + v[i] * t).powi(2))
            .sum::<f64>()
            .sqrt()
    };
    let relative = std::array::from_fn(|i| v[i] - body_v[i]);
    let d = std::array::from_fn(|i| p[i] - start[i]);
    let mut clearance = closest(d, relative, 0., duration.min(12.), true);
    let centre = std::array::from_fn(|i| d[i] - if i == 1 { 0.9 } else { 0. });
    let mut collision = closest(centre, relative, 0., duration.min(12.), false);
    if duration < 12. {
        clearance = clearance.min(closest(
            std::array::from_fn(|i| p[i] - target[i]),
            v,
            duration,
            12.,
            true,
        ));
        collision = collision.min(closest(
            std::array::from_fn(|i| p[i] - target[i] - if i == 1 { 0.9 } else { 0. }),
            v,
            duration,
            12.,
            false,
        ));
    }
    // Larger than the ordinary danger corridor to allow prediction error.
    // Unknown projectiles and missing/short/releasing input retain old defense.
    if clearance >= 2. {
        Some(false)
    } else if collision < 1.5 {
        Some(true)
    } else {
        None
    }
}
pub fn target_key(p: [f64; 3]) -> [i32; 3] {
    p.map(|v| (v * 10.).round() as i32)
}
pub fn evade(o: &Observation) -> Option<Escape> {
    evade_avoiding(o, &[])
}
pub fn evade_avoiding(o: &Observation, blocked: &[[i32; 3]]) -> Option<Escape> {
    evade_with_stationary(o, blocked, &[], &[])
}
pub(super) fn evade_with_stationary(
    o: &Observation,
    blocked: &[[i32; 3]],
    stationary: &[String],
    recent_retreat: &[[f64; 3]],
) -> Option<Escape> {
    evade_with_limits(o, blocked, stationary, recent_retreat, 3.5)
}
pub(super) fn evade_with_limits(
    o: &Observation,
    blocked: &[[i32; 3]],
    stationary: &[String],
    recent_retreat: &[[f64; 3]],
    waypoint_axis: f64,
) -> Option<Escape> {
    evade_with_native(o, blocked, stationary, recent_retreat, waypoint_axis, &[])
}
pub(super) fn evade_with_native(
    o: &Observation,
    blocked: &[[i32; 3]],
    stationary: &[String],
    recent_retreat: &[[f64; 3]],
    waypoint_axis: f64,
    admitted: &[Admitted],
) -> Option<Escape> {
    evade_policy(
        o,
        blocked,
        stationary,
        recent_retreat,
        waypoint_axis,
        admitted,
        false,
    )
}
pub(super) fn evade_policy(
    o: &Observation,
    blocked: &[[i32; 3]],
    stationary: &[String],
    recent_retreat: &[[f64; 3]],
    waypoint_axis: f64,
    admitted: &[Admitted],
    boundary_enabled: bool,
) -> Option<Escape> {
    if water_danger(o) {
        return water_escape(o, blocked);
    }
    let start = point(&o.player["position"])?;
    let mut threats = Vec::new();
    let mut projectile_danger = false;
    let mut incoming_directions = Vec::new();
    let mut blasts = Vec::new();
    for e in o.world["entities"].as_array().into_iter().flatten() {
        if !perceived(e) {
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
            threats.push((p, e["ranged_attack"] == true, super::aerial::diver(e)));
            if let Some(radius) = primed_blast(e)
                && d <= radius + 2.
                && blasts.len() < 4
            {
                blasts.push((p, radius));
            }
        }
        if e["projectile"] == true
            && !projectile_key(e).is_some_and(|id| stationary.contains(&id))
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
                if ongoing_input_arrow_danger(o, e)
                    .unwrap_or((0.0..=12.).contains(&t) && miss < 1.5)
                {
                    projectile_danger = true;
                    threats.push((p, true, false));
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
    let blast_escape = !blasts.is_empty();
    if !burning
        && !projectile_danger
        && !blast_escape
        && let Some(plan) = cover::plan(o, blocked, waypoint_axis)
    {
        return match plan {
            cover::Plan::Move(route) => Some(route),
            cover::Plan::Hold => None,
        };
    }
    // Begin approaching observed refuge before a dive reaches melee range.
    // Only safe, supported floor centres participate; no unseen shelter is invented.
    let seek_refuge = threats.iter().any(|(_, _, aerial)| *aerial)
        // A visible flyer already within contact range can enter from a side.
        // Do not let a roof reward pin the body in place during that intrusion.
        && !threats.iter().any(|(enemy, _, aerial)| *aerial &&
            (0..3).map(|i|(start[i]-enemy[i]).powi(2)).sum::<f64>() < 4.5_f64.powi(2))
        && !blast_escape
        && !projectile_danger
        && !burning;
    let refuges: Vec<_> = if seek_refuge {
        obstacles
            .iter()
            .map(|b| [(b[0] + b[3]) / 2., b[4], (b[2] + b[5]) / 2.])
            .filter(|p| {
                (p[0] - start[0]).hypot(p[2] - start[2]) <= 6.
                    && (p[1] - start[1]).abs() <= 1.
                    && super::aerial::roofed(*p, &obstacles)
                    && supported(&cells, *p, &obstacles)
                    && clear(&cells, *p, &obstacles)
            })
            // Compute coverage margins only once per admitted floor centre,
            // rather than repeating polygon unions for every route candidate.
            .map(|p| (p, super::aerial::margin(p, &obstacles)))
            .collect()
    } else {
        Vec::new()
    };
    let boundary_turn = if boundary_enabled
        && !burning
        && !projectile_danger
        && !blast_escape
        && threats
            .iter()
            .all(|(_, ranged, aerial)| *ranged && !*aerial)
        && o.world["entities"].as_array().is_some_and(|entities| {
            entities
                .iter()
                .any(|e| e["visible"] == true && e["hostile"] == true && e["ranged_attack"] == true)
        }) {
        boundary::Turn::find(
            o,
            recent_retreat,
            &obstacles,
            threats.iter().map(|(p, _, _)| *p).collect(),
        )
    } else {
        None
    };
    let score = |p: [f64; 3]| -> f64 {
        let mut distance = threats
            .iter()
            .map(|(e, _, _)| ((p[0] - e[0]).powi(2) + (p[2] - e[2]).powi(2)).sqrt())
            .fold(30., f64::min);
        // At a known boundary, permit a still-separated sideways turn instead
        // of requiring every step to increase distance and reversing the lane.
        // Open escape, close contact and urgent hazards keep the existing score.
        if boundary_turn.is_some() {
            distance = distance.min(5.5);
        }
        let cover = threats
            .iter()
            .filter(|(e, _, _)| covered(p, *e, &obstacles))
            .map(|(_, ranged, _)| if *ranged { 24. } else { 4. })
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
        // A head-on retreat stays on an archer's firing line. Start moving
        // across that line before a close shot is already in flight, while
        // distance and cover still reward outward progress and protected routes.
        let ranged_lateral = boundary_turn
            .as_ref()
            .map(|turn| turn.lateral(start, p))
            .unwrap_or_else(|| {
                threats
                    .iter()
                    .filter(|(_, ranged, _)| *ranged)
                    .map(|(enemy, _, _)| {
                        let dx = start[0] - enemy[0];
                        let dz = start[2] - enemy[2];
                        let length = dx.hypot(dz);
                        if length > 0.001 {
                            ((p[0] - start[0]) * -dz + (p[2] - start[2]) * dx).abs() / length
                        } else {
                            0.
                        }
                    })
                    .fold(0., f64::max)
            });
        let blast_safety = blasts
            .iter()
            .map(|(center, radius)| {
                let distance = (0..3)
                    .map(|i| (p[i] - center[i]).powi(2))
                    .sum::<f64>()
                    .sqrt();
                // Occlusion is a preference, not a promise that a block will
                // survive the explosion. Check more than an eye-height ray.
                let shadow = [0.15, 0.85, 1.65].iter().all(|height| {
                    occluded(
                        [p[0], p[1] + height, p[2]],
                        [center[0], center[1] + 0.7, center[2]],
                        &obstacles,
                    )
                });
                (distance / radius).min(1.25) + if shadow { 0.65 } else { 0.0 }
            })
            .reduce(f64::min)
            .unwrap_or(0.0);
        // Never buy aerial cover by moving next to another observed melee threat.
        let refuge_safe = !threats.iter().any(|(enemy, _, aerial)| {
            !aerial && (0..3).map(|i| (p[i] - enemy[i]).powi(2)).sum::<f64>() < 4.5_f64.powi(2)
        });
        let refuge = if seek_refuge && refuge_safe {
            let approach = refuges
                .iter()
                .map(|(q, _)| (6. - (p[0] - q[0]).hypot(p[2] - q[2])).max(0.) * 4.)
                .fold(0., f64::max);
            // Translated square footprints retain at least this much of the
            // sampled margin. A different floor height cannot inherit it.
            let interior = refuges
                .iter()
                .filter(|(q, _)| (p[1] - q[1]).abs() < 0.001)
                .map(|(q, margin)| (margin - (p[0] - q[0]).abs().max((p[2] - q[2]).abs())).max(0.))
                .fold(0., f64::max);
            approach
                + if super::aerial::roofed(p, &obstacles) {
                    60. + interior * 24.
                } else {
                    0.
                }
        } else {
            0.
        };
        let turn_cost = if !burning && !blast_escape && threats.iter().all(|(_, ranged, _)| *ranged)
        {
            point(&o.player["client_ground_motion"]["velocity_per_tick"])
                .map(|v| {
                    let speed = v[0].hypot(v[2]);
                    let dx = p[0] - start[0];
                    let dz = p[2] - start[2];
                    let length = dx.hypot(dz);
                    if speed > 0.05 && length > 0.2 {
                        let alignment = ((v[0] * dx + v[2] * dz) / (speed * length)).clamp(-1., 1.);
                        4. * (speed / 0.28).min(1.) * (1. - alignment)
                    } else {
                        0.
                    }
                })
                .unwrap_or(0.)
        } else {
            0.
        };
        distance * 2. + cover + sidestep * 8. + ranged_lateral * 2. + blast_safety * 48.0 + refuge
            - turn_cost
    };
    let current = score(start) - if burning { 10. } else { 0. };
    let mut best = None;
    let mut best_score = f64::NEG_INFINITY;
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
        if blast_escape {
            // Use one longer admitted input lease to avoid stopping in the
            // blast radius between the ordinary short retreat segments.
            let longest = ((waypoint_axis - 0.10) / a.cos().abs().max(a.sin().abs())).min(5.65);
            for scale in [1., 0.82] {
                candidates.push([
                    start[0] + a.cos() * longest * scale,
                    start[1],
                    start[2] + a.sin() * longest * scale,
                ]);
            }
        }
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
    for (p, native_jump) in candidates
        .into_iter()
        .map(|p| (p, None))
        .chain(admitted.iter().map(|r| (r.target, Some(r.jump))))
    {
        let dy = p[1] - start[1];
        let length = ((p[0] - start[0]).powi(2) + (p[2] - start[2]).powi(2)).sqrt();
        for jump in [false, true] {
            if native_jump.is_some_and(|j| j != jump) {
                continue;
            }
            if (native_jump.is_none() && dy > 0.1 && !jump)
                || (jump && o.player["on_ground"] == false)
            {
                continue;
            }
            if blocked.contains(&target_key(p)) {
                continue;
            }
            if native_jump.is_none()
                && (!supported(&cells, p, &obstacles) || !clear(&cells, p, &obstacles))
            {
                continue;
            }
            // The quick geometric arc is deliberately conservative. A fresh
            // native prediction can establish a path it misses (for example
            // a step jump whose head clips a low ceiling). Do not run that
            // same approximate arc over the admitted prediction again.
            let route = if native_jump.is_some() {
                true
            } else if dy < 0. && !jump {
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
            if boundary_turn
                .as_ref()
                .is_some_and(|turn| !turn.allows(start, p))
            {
                continue;
            }
            let safety = score(p) - length * 0.1 - if jump { 0.2 } else { 0. };
            if safety <= current + 0.4 {
                continue;
            }
            // Recent positions are a preference, not an obstruction. Keep an
            // actually viable way back when every new route is worse than
            // staying, and let strong cover/projectile scores win.
            let revisit = recent_retreat
                .iter()
                .filter(|q| (q[1] - p[1]).abs() < 0.75)
                .map(|q| (2.0 - (p[0] - q[0]).hypot(p[2] - q[2])).max(0.0))
                .fold(0.0, f64::max);
            let value = safety - if blast_escape { 0.0 } else { revisit * 4.0 };
            if value > best_score {
                best_score = value;
                best = Some(Escape {
                    target: Position {
                        x: p[0],
                        y: p[1],
                        z: p[2],
                    },
                    jump,
                    cover_path: Vec::new(),
                    reason: if native_jump.is_some() {
                        "native-predicted retreat after stalled local avoidance"
                    } else if blast_escape {
                        "leave the primed blast radius on one observed route"
                    } else if projectile_danger {
                        "evade incoming projectile"
                    } else if burning {
                        "leave burning terrain"
                    } else if seek_refuge && !refuges.is_empty() {
                        "approach observed low-roof refuge from flying attacker"
                    } else if boundary_turn.is_some() {
                        "turn along observed boundary without reversing the firing lane"
                    } else {
                        "increase threat distance and seek cover"
                    }
                    .into(),
                });
            }
        }
    }
    if best.is_none() && boundary_turn.is_some() {
        // A circulation preference must not fence the only admitted way back.
        // One fallback pass retains history and the original hazard scoring.
        return evade_policy(
            o,
            blocked,
            stationary,
            recent_retreat,
            waypoint_axis,
            admitted,
            false,
        );
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
    fn ranged_corner() -> (Observation, Vec<[f64; 3]>) {
        let mut o = world();
        o.player = json!({"position":[0.4971,1.,0.4886],"on_ground":true});
        o.world["entities"] = json!([{"visible":true,"hostile":true,"ranged_attack":true,"position":[8.292,1.,-4.095]}]);
        for b in o.world["blocks"].as_array_mut().unwrap() {
            let p = point(&b["position"]).unwrap();
            if (p[0] == -1. || p[2] == 1.) && (1. ..=3.).contains(&p[1]) {
                b["block"] = json!("minecraft:stone");
                b["collision"] = json!([[0., 0., 0., 1., 1., 1.]]);
            }
        }
        (o, vec![[0.3524, 1., -0.6502]])
    }
    #[test]
    fn a_known_corner_turn_preserves_circulation_in_rotated_worlds() {
        let (mut o, mut trail) = ranged_corner();
        let mut desired = [1., 0.];
        for _ in 0..4 {
            let start = point(&o.player["position"]).unwrap();
            let route = evade_policy(&o, &[], &[], &trail, 3.5, &[], true).unwrap();
            assert!(!route.jump);
            assert!(route.reason.contains("boundary"));
            let snapshot =
                super::super::decision::Snapshot::capture(&o, &[], &[], &trail, 3.5, &[], &route)
                    .with_boundary_turns(true);
            let bytes = serde_json::to_vec(&snapshot).unwrap();
            let restored: super::super::decision::Snapshot =
                serde_json::from_slice(&bytes).unwrap();
            assert_eq!(restored.replay()["matches_recorded_choice"], true);
            assert_eq!(
                restored.replay()["experimental_ranged_boundary_turns"],
                true
            );
            let delta = [route.target.x - start[0], route.target.z - start[2]];
            assert!(delta[0] * desired[0] + delta[1] * desired[1] > 2.);
            let legacy = evade_policy(&o, &[], &[], &trail, 3.5, &[], false).unwrap();
            let default = evade_with_stationary(&o, &[], &[], &trail).unwrap();
            assert_eq!(
                [default.target.x, default.target.z],
                [legacy.target.x, legacy.target.z]
            );
            let old_delta = [legacy.target.x - start[0], legacy.target.z - start[2]];
            assert!(old_delta[0] * desired[0] + old_delta[1] * desired[1] < 1.);
            // Rotate complete voxel cells, entities and reached history, not
            // just the waypoint. No arena coordinates enter the policy.
            for b in o.world["blocks"].as_array_mut().unwrap() {
                let p = point(&b["position"]).unwrap();
                b["position"] = json!([-p[2] - 1., p[1], p[0]]);
            }
            o.player["position"] = json!([-start[2], start[1], start[0]]);
            for e in o.world["entities"].as_array_mut().unwrap() {
                let p = point(&e["position"]).unwrap();
                e["position"] = json!([-p[2], p[1], p[0]]);
            }
            for p in &mut trail {
                *p = [-p[2], p[1], p[0]];
            }
            desired = [-desired[1], desired[0]];
        }
    }
    #[test]
    fn boundary_preference_does_not_invent_support_or_disable_the_only_way_back() {
        let (mut o, trail) = ranged_corner();
        o.world["blocks"]
            .as_array_mut()
            .unwrap()
            .retain(|b| b["position"][0].as_f64().unwrap() < 1.);
        let route = evade_policy(&o, &[], &[], &trail, 3.5, &[], true).unwrap();
        assert!(route.target.x < 1.);
        assert!(!route.reason.contains("boundary"));
        assert!(
            evade_policy(
                &Observation {
                    world: json!({"blocks":[],"entities":o.world["entities"]}),
                    ..o.clone()
                },
                &[],
                &[],
                &trail,
                3.5,
                &[],
                true
            )
            .is_none()
        );
    }
    #[test]
    fn urgent_projectiles_and_mixed_melee_keep_existing_escape_policy() {
        let (base, trail) = ranged_corner();
        for mode in ["projectile", "melee", "burning"] {
            let mut o = base.clone();
            match mode {
                "projectile"=>o.world["entities"].as_array_mut().unwrap().push(json!({"visible":true,"projectile":true,"position":[2.,2.,0.5],"velocity":[-1.,0.,0.]})),
                "melee"=>o.world["entities"].as_array_mut().unwrap().push(json!({"visible":true,"hostile":true,"ranged_attack":false,"position":[3.,1.,-1.]})),
                _=>o.player["fire_ticks"]=json!(20),
            }
            let actual = evade_policy(&o, &[], &[], &trail, 3.5, &[], true);
            let legacy = evade_policy(&o, &[], &[], &trail, 3.5, &[], false);
            assert_eq!(
                actual
                    .as_ref()
                    .map(|r| ([r.target.x, r.target.y, r.target.z], r.jump)),
                legacy
                    .as_ref()
                    .map(|r| ([r.target.x, r.target.y, r.target.z], r.jump)),
                "{mode}"
            );
        }
    }
    #[test]
    fn a_known_cover_corner_is_reached_instead_of_repeated_exposed_retreat() {
        // Source-only policy counterexample, with geometry translated from the
        // declared arena. Not a reconstruction of unrecorded decision inputs.
        let mut o = world();
        o.player = json!({"position":[0.1189,1.,-0.7871],"on_ground":true});
        o.world["entities"] = json!([{"visible":true,"hostile":true,
            "ranged_attack":true,"position":[10.6578,1.,0.297]}]);
        for b in o.world["blocks"].as_array_mut().unwrap() {
            let p = point(&b["position"]).unwrap();
            if p[0] == -3. && (1. ..=3.).contains(&p[1]) && (-2. ..=2.).contains(&p[2]) {
                b["block"] = json!("minecraft:stone_bricks");
                b["collision"] = json!([[0., 0., 0., 1., 1., 1.]]);
            }
        }
        let enemy = point(&o.world["entities"][0]["position"]).unwrap();
        let obstacles = boxes(&o);
        let mut steps = Vec::new();
        for _ in 0..3 {
            let p = point(&o.player["position"]).unwrap();
            if covered(p, enemy, &obstacles) {
                break;
            }
            let Some(route) = evade(&o) else {
                break;
            };
            let p = [route.target.x, route.target.y, route.target.z];
            steps.push(p);
            o.player["position"] = json!(p);
        }
        assert!(
            covered(point(&o.player["position"]).unwrap(), enemy, &obstacles),
            "Known reachable cover was not reached within three finite waypoints: {steps:?}"
        );
    }
    #[test]
    fn bounded_decision_capture_replays_real_policy_fields_and_history() {
        let mut o = world();
        let blocked = [[1, 1, 1]];
        let stationary = ["uuid:embedded".to_string()];
        let trail = [[0.5, 1., 0.5], [-0.5, 1., -0.5]];
        let admitted = [Admitted {
            target: [-1.5, 1., 0.5],
            jump: false,
        }];
        for mode in ["ranged", "melee", "water", "unknown"] {
            if mode == "water" {
                o = pool();
            }
            if mode == "unknown" {
                o = world();
                o.world["blocks"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|b| b["position"][0] != -2);
            }
            if mode == "ranged" {
                o.world["entities"][0]["ranged_attack"] = json!(true);
            }
            if mode == "melee" {
                o.world["entities"][0]["ranged_attack"] = json!(false);
            }
            let route =
                evade_with_native(&o, &blocked, &stationary, &trail, 3.5, &admitted).unwrap();
            let snapshot = super::super::decision::Snapshot::capture(
                &o,
                &blocked,
                &stationary,
                &trail,
                3.5,
                &admitted,
                &route,
            );
            let bytes = serde_json::to_vec(&snapshot).unwrap();
            assert!(bytes.len() <= super::super::decision::MAX_BYTES);
            let restored: super::super::decision::Snapshot =
                serde_json::from_slice(&bytes).unwrap();
            assert_eq!(restored.replay()["matches_recorded_choice"], true, "{mode}");
        }
    }
    #[test]
    fn cover_approach_turns_around_known_obstacle_without_blind_or_mixed_threat_routes() {
        let mut o = world();
        o.player = json!({"position":[0.06898744264365,1.,-0.289612793940735],"on_ground":true});
        o.world["entities"] = json!([{"visible":true,"hostile":true,"ranged_attack":true,
            "position":[11.2047145447027,1.,0.12051863731968064]}]);
        for b in o.world["blocks"].as_array_mut().unwrap() {
            let p = point(&b["position"]).unwrap();
            if p[0] == -2. && (1. ..=3.).contains(&p[1]) && (-2. ..=2.).contains(&p[2]) {
                b["collision"] = json!([[0., 0., 0., 1., 1., 1.]]);
                b["block"] = json!("minecraft:stone_bricks");
            }
        }
        let route = evade(&o).expect("Known corner route");
        assert!(
            route.target.z < -2.3,
            "Approach nearer end of the wall: {route:?}"
        );
        let goal = route
            .cover_path
            .last()
            .expect("Retained planned cover goal");
        assert!(covered(
            [goal.x, goal.y, goal.z],
            point(&o.world["entities"][0]["position"]).unwrap(),
            &boxes(&o)
        ));
        assert!(
            !can_pass_waypoint(&o, &route),
            "Corner approach requires a precise stop"
        );
        let mut unknown = o.clone();
        unknown.world["blocks"]
            .as_array_mut()
            .unwrap()
            .retain(|b| b["position"][0].as_i64().unwrap() >= -2);
        assert!(
            cover::plan(&unknown, &[], 3.5).is_none(),
            "Unknown space behind wall is not a cover route"
        );
        o.world["entities"]
            .as_array_mut()
            .unwrap()
            .push(json!({"visible":true,"hostile":true,
            "ranged_attack":false,"position":[2.5,1.,0.5]}));
        assert!(
            cover::plan(&o, &[], 3.5).is_none(),
            "Mixed melee threat retains urgent evasion"
        );
    }
    #[test]
    fn guarded_retreat_stays_beside_the_wall_and_settles_without_repeated_actions() {
        let mut o = world();
        o.player = json!({"position":[-0.5,1.,-1.5],"on_ground":true});
        o.world["entities"] = json!([{"visible":false,"hostile":true,"ranged_attack":true,
            "position":[11.5,1.,-2.8],"client_memory":{"kind":"last_visible_ranged_threat"}}]);
        for b in o.world["blocks"].as_array_mut().unwrap() {
            let p = point(&b["position"]).unwrap();
            if p[0] == 0. && (1. ..=3.).contains(&p[1]) && (-2. ..=2.).contains(&p[2]) {
                b["block"] = json!("minecraft:stone_bricks");
                b["collision"] = json!([[0., 0., 0., 1., 1., 1.]]);
            }
        }
        let first = evade(&o).expect("Guard toward the protected wall interior");
        assert!(
            (first.target.x + 0.5).abs() < 0.01 && first.target.z.abs() <= 0.5,
            "{first:?}"
        );
        for direction in [-1., 1.] {
            assert!(
                covered(
                    [
                        first.target.x,
                        first.target.y,
                        first.target.z + direction * 1.5
                    ],
                    [11.5, 1., -2.8],
                    &boxes(&o),
                ),
                "A guard needs covered room on both sides, rather than just the farthest endpoint"
            );
        }
        assert!(!first.cover_path.is_empty());
        assert!(!can_pass_waypoint(&o, &first));
        o.player["position"] = json!([first.target.x, first.target.y, first.target.z]);
        for _ in 0..3 {
            let Some(route) = evade(&o) else {
                break;
            };
            assert!(
                (route.target.x + 0.5).abs() < 0.01,
                "Do not drift away from cover: {route:?}"
            );
            assert!(covered(
                [route.target.x, route.target.y, route.target.z],
                [11.5, 1., -2.8],
                &boxes(&o)
            ));
            o.player["position"] = json!([route.target.x, route.target.y, route.target.z]);
        }
        assert!(
            evade(&o).is_none(),
            "A good guard position does not submit zero-distance controls repeatedly"
        );
    }
    #[test]
    fn a_close_archer_across_a_solid_wall_does_not_invalidate_guarded_ground() {
        let mut o = world();
        o.player = json!({"position":[-0.5,1.,0.5],"on_ground":true});
        o.world["entities"] = json!([{"visible":false,"hostile":true,"ranged_attack":true,
            "position":[1.5,1.,0.5],"client_memory":{"kind":"last_visible_ranged_threat"}}]);
        for block in o.world["blocks"].as_array_mut().unwrap() {
            let p = point(&block["position"]).unwrap();
            if p[0] == 0. && (1. ..=3.).contains(&p[1]) && (-3. ..=3.).contains(&p[2]) {
                block["block"] = json!("minecraft:stone");
                block["collision"] = json!([[0., 0., 0., 1., 1., 1.]]);
            }
        }
        let plan = cover::plan(&o, &[], 3.5)
            .expect("A solid screen can preserve guarded ground despite short Euclidean distance");
        if let cover::Plan::Move(route) = plan {
            assert!(
                (route.target.x + 0.5).abs() < 0.01 && !route.jump,
                "Stay on the protected side: {route:?}"
            );
            for point in route.cover_path {
                assert!(
                    covered([point.x, point.y, point.z], [1.5, 1., 0.5], &boxes(&o)),
                    "Every selected point remains occluded"
                );
            }
        }
        let mut open = o.clone();
        for block in open.world["blocks"].as_array_mut().unwrap() {
            if block["position"][0] == 0 && block["position"][1].as_i64().unwrap() > 0 {
                block["block"] = json!("minecraft:air");
                block["collision"] = json!([]);
            }
        }
        assert!(
            cover::plan(&open, &[], 3.5).is_none(),
            "A nearby unshielded shooter still requires ordinary escape"
        );
        o.world["entities"]
            .as_array_mut()
            .unwrap()
            .push(json!({"visible":true,"hostile":true,
            "ranged_attack":false,"position":[-2.5,1.,0.5]}));
        assert!(
            cover::plan(&o, &[], 3.5).is_none(),
            "A close melee enemy cannot inherit ranged cover protection"
        );
    }
    #[test]
    fn continuing_owned_strafe_is_not_interrupted_for_a_shot_it_already_clears() {
        let mut o = world();
        o.player["position"] = json!([0.5, 1., 0.5]);
        o.world["entities"] = json!([{"uuid":"shot","visible":true,"projectile":true,"type":"minecraft:arrow",
            "position":[12.5,1.9,0.5],"velocity":[-1.5,0.,0.]}]);
        assert!(incoming(&o));
        o.player["client_motion"] = json!({"kind":"finite_input_extrapolation","velocity_per_tick":[0.,0.,0.3],"target":[0.5,1.,3.5]});
        assert!(
            !incoming(&o),
            "Continue a strafe with at least2m predicted clearance"
        );
        o.player["client_motion"]["target"] = json!([0.5, 1., 1.2]);
        assert!(
            incoming(&o),
            "Stopping early must retain danger after the input ends"
        );
        o.player["client_motion"]["target"] = json!([0.5, 1., 3.5]);
        o.world["entities"][0]["velocity"] = json!([-1.5, 0., 0.3]);
        assert!(
            incoming(&o),
            "A shot leading the moving body still needs defense"
        );
        o.world["entities"][0]["velocity"] = json!([-1.5, 0., 0.]);
        o.world["entities"][0]["type"] = json!("some_mod:unknown_projectile");
        assert!(
            incoming(&o),
            "Unknown projectile dynamics keep conservative handling"
        );
    }
    #[test]
    fn ranged_escape_keeps_observed_momentum_when_both_sides_are_safe() {
        let mut o = world();
        for b in o.world["blocks"].as_array_mut().unwrap() {
            b["position"][2] = json!(b["position"][2].as_i64().unwrap() - 2);
        }
        let start = [0.4335514763727, 1., -1.6993455708818217];
        let v = [-0.075306516797016, -0., -0.225919550391046];
        o.player = json!({"position":start,"on_ground":true,
            "client_ground_motion":{"velocity_per_tick":v}});
        o.world["entities"] =
            json!([{"position":[9.,1.,0.5],"visible":true,"hostile":true,"ranged_attack":true}]);
        let route = evade(&o).unwrap();
        let dx = route.target.x - start[0];
        let dz = route.target.z - start[2];
        let alignment = (dx * v[0] + dz * v[2]) / (dx.hypot(dz) * v[0].hypot(v[2]));
        assert!(
            alignment > 0.7,
            "Avoid a sharp reversal with equally safe forward support: {route:?}"
        );
        assert!(
            (route.target.x - 9.).hypot(route.target.z - 0.5)
                > (start[0] - 9.).hypot(start[2] - 0.5)
        );
    }
    #[test]
    fn passage_needs_known_safe_ground_beyond_an_otherwise_valid_waypoint() {
        let mut o = world();
        o.player["on_ground"] = json!(true);
        let route = Escape {
            target: Position {
                x: 2.5,
                y: 1.,
                z: 0.5,
            },
            jump: false,
            reason: "fixture".into(),
            cover_path: Vec::new(),
        };
        assert!(can_pass_waypoint(&o, &route));
        for kind in ["wall", "hole", "fire", "unknown"] {
            let mut bad = o.clone();
            if kind == "unknown" {
                bad.world["blocks"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|b| b["position"][0] != 3);
            } else {
                for b in bad.world["blocks"].as_array_mut().unwrap() {
                    if b["position"] == json!([3, 1, 0]) && kind == "wall" {
                        b["collision"] = json!([[0., 0., 0., 1., 1., 1.]]);
                    }
                    if b["position"] == json!([3, 0, 0]) && kind == "hole" {
                        b["collision"] = json!([]);
                        b["block"] = json!("minecraft:air");
                    }
                    if b["position"] == json!([3, 1, 0]) && kind == "fire" {
                        b["block"] = json!("minecraft:fire");
                    }
                }
            }
            assert!(
                !can_pass_waypoint(&bad, &route),
                "unsafe {kind} continuation admitted"
            );
        }
        o.player["on_ground"] = json!(false);
        assert!(!can_pass_waypoint(&o, &route));
    }
    #[test]
    fn visible_phantom_is_detected_before_the_melee_radius() {
        let mut o = world();
        o.world["entities"] = json!([{"entity_id":9,"type":"minecraft:phantom","visible":true,"hostile":true,"position":[-12.,20.,0.5]}]);
        assert!(threatened(&o));
        o.world["entities"][0]["visible"] = json!(false);
        assert!(!threatened(&o));
        o.world["entities"][0]["visible"] = json!(true);
        o.world["entities"][0]["type"] = json!("minecraft:zombie");
        assert!(!threatened(&o), "do not broaden every melee threat");
    }
    #[test]
    fn aerial_escape_prefers_reachable_observed_roof_to_open_distance() {
        let mut o = world();
        o.player["on_ground"] = json!(true);
        o.world["entities"] = json!([{"entity_id":9,"type":"minecraft:phantom","visible":true,"hostile":true,"position":[-12.,20.,0.5]}]);
        assert!(evade(&o).unwrap().target.x > 0.5);
        for b in o.world["blocks"].as_array_mut().unwrap() {
            if b["position"] == json!([-2, 3, 0]) {
                b["block"] = json!("minecraft:stone");
                b["collision"] = json!([[0., 0., 0., 1., 1., 1.]]);
            }
        }
        let route = evade(&o).unwrap();
        assert!(
            route.target.x < 0.5,
            "approach the observed roof: {route:?}"
        );
        assert!(route.reason.contains("refuge"));
        o.world["entities"].as_array_mut().unwrap().push(json!({"entity_id":10,"type":"minecraft:zombie","visible":true,"hostile":true,"position":[-2.5,1.,0.5]}));
        assert!(
            evade(&o).unwrap().target.x > 0.5,
            "roof reward must not lead into an observed melee threat"
        );
    }
    #[test]
    fn sheltered_body_prefers_interior_over_distance_chasing_at_the_edge() {
        let mut o = world();
        o.player = json!({"position":[-0.5,1.,-0.5],"on_ground":true});
        o.world["entities"] = json!([{"entity_id":9,"type":"minecraft:phantom","visible":true,"hostile":true,"position":[12.,1.5,0.5]}]);
        for b in o.world["blocks"].as_array_mut().unwrap() {
            let p = point(&b["position"]).unwrap();
            if p[1] == 3. && (-1.0..=1.0).contains(&p[0]) && (-1.0..=1.0).contains(&p[2]) {
                b["block"] = json!("minecraft:stone");
                b["collision"] = json!([[0., 0., 0., 1., 1., 1.]]);
            }
        }
        let route = evade(&o).unwrap();
        assert!(
            (route.target.x - 0.5).abs() < 0.01 && (route.target.z - 0.5).abs() < 0.01,
            "prefer roof interior: {route:?}"
        );
        o.player["position"] = json!([0.5, 1., 0.5]);
        assert!(
            evade(&o).is_none(),
            "do not chase the circling flyer back to a roof edge"
        );
        o.world["entities"][0]["position"] = json!([2., 1.5, 0.5]);
        let urgent = evade(&o).expect("a close visible flyer can enter under the roof");
        let distance = (urgent.target.x - 2.).hypot(urgent.target.z - 0.5);
        assert!(
            distance > 2.,
            "roof preference must not suppress contact-range escape: {urgent:?}"
        );
    }
    fn pool() -> Observation {
        let mut o = world();
        o.player = json!({"position":[0.5,1.,0.5],"on_ground":false,"in_water":true,"air":40,"max_air":300});
        o.world["entities"] = json!([]);
        for b in o.world["blocks"].as_array_mut().unwrap() {
            if (1..=2).contains(&b["position"][1].as_i64().unwrap()) {
                b["block"] = json!("minecraft:water");
            }
        }
        o
    }
    #[test]
    fn only_confirmed_breathing_with_full_air_allows_surface_swimming() {
        let mut o = pool();
        o.player["air"] = json!(300);
        assert!(water_danger(&o), "missing eye facts retain protection");
        o.player["eyes_in_water"] = json!(false);
        assert!(!water_danger(&o));
        o.player["eyes_in_water"] = json!(true);
        assert!(water_danger(&o), "protect immediately, before air loss");
        o.player["eyes_in_water"] = json!(false);
        o.player["air"] = json!(280);
        assert!(water_danger(&o), "recover air before yielding protection");
    }
    #[test]
    fn water_without_hostiles_still_requires_a_breathing_route() {
        let o = pool();
        assert!(!threatened(&o));
        assert!(water_danger(&o));
        let route = evade(&o).expect("Submerged idle players must surface independently");
        assert!(!route.jump);
        assert!(route.target.y > 1.);
        assert!(route.target.y + 1.62 >= 3.);
        assert_eq!(route.target.x, 0.5);
        assert_eq!(route.target.z, 0.5);
    }
    #[test]
    fn water_escape_does_not_invent_air_or_swim_through_a_ceiling() {
        let mut o = pool();
        o.world["blocks"]
            .as_array_mut()
            .unwrap()
            .retain(|b| b["position"][1] != 3);
        assert!(
            evade(&o).is_none(),
            "Unknown overhead cells are not breathable evidence"
        );
        let mut o = pool();
        for b in o.world["blocks"].as_array_mut().unwrap() {
            if b["position"][1] == 3 {
                b["block"] = json!("minecraft:stone");
                b["collision"] = json!([[0., 0., 0., 1., 1., 1.]]);
            }
        }
        assert!(evade(&o).is_none());
    }
    #[test]
    fn a_reachable_dry_bank_beats_remaining_in_water() {
        let mut o = pool();
        o.player["position"] = json!([0.5, 2., 0.5]);
        for b in o.world["blocks"].as_array_mut().unwrap() {
            let p = &b["position"];
            if p[0].as_i64().unwrap() >= 2 && p[1] == 1 {
                b["block"] = json!("minecraft:stone");
                b["collision"] = json!([[0., 0., 0., 1., 1., 1.]]);
            } else if p[0].as_i64().unwrap() >= 2 && p[1] == 2 {
                b["block"] = json!("minecraft:air");
            }
        }
        let route = evade(&o).unwrap();
        assert!(route.target.x >= 2.);
        assert_eq!(route.target.y, 2.);
        assert!(route.reason.contains("dry support"));
        o.player["in_water"] = json!(false);
        assert!(evade(&o).is_none());
    }
    #[test]
    fn a_bank_beyond_one_control_pulls_the_surfacing_waypoint_toward_shore() {
        let mut o = pool();
        for b in o.world["blocks"].as_array_mut().unwrap() {
            if b["position"][0] == 4 && (1..=2).contains(&b["position"][1].as_i64().unwrap()) {
                b["block"] = json!("minecraft:stone");
                b["collision"] = json!([[0., 0., 0., 1., 1., 1.]]);
            }
        }
        let route = evade(&o).unwrap();
        assert!(
            route.target.x > 1.,
            "Surfacing must also progress toward the observed shore"
        );
        assert!(route.target.y + 1.62 >= 3.);
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
    fn ranged_retreat_moves_laterally_before_an_arrow_is_already_incoming() {
        let mut o = world();
        o.world["entities"][0]["position"] = json!([8.5, 1., 0.5]);
        o.world["entities"][0]["ranged_attack"] = json!(true);
        assert!(!incoming(&o));
        let route = evade(&o).unwrap();
        let retreat = 0.5 - route.target.x;
        let lateral = (route.target.z - 0.5).abs();
        assert!(retreat > 0.7, "Retreat still needs outward progress");
        assert!(
            lateral >= retreat * 0.5,
            "Do not wait for an already imminent arrow before moving across its firing line: {route:?}"
        );
    }
    #[test]
    fn primed_charged_creeper_prefers_a_long_observed_route() {
        let mut o = world();
        o.player["on_ground"] = json!(true);
        o.world["entities"][0]["position"] = json!([-2.5, 1., 0.5]);
        o.world["entities"][0]["creeper"] = json!({
            "ignited":true,"swell_direction":1,"powered":true
        });
        let route = evade_with_limits(&o, &[], &[], &[], 4.05).expect("observed retreat");
        assert!(!route.jump);
        assert!(route.reason.contains("primed blast"));
        assert!((route.target.x + 2.5).hypot(route.target.z - 0.5) > 7.9);
        let legacy = evade_with_limits(&o, &[], &[], &[], 3.5).expect("compatible retreat");
        assert!((legacy.target.x - 0.5).abs() <= 3.5 && (legacy.target.z - 0.5).abs() <= 3.5);
        o.world["blocks"] = json!([]);
        assert!(evade_with_limits(&o, &[], &[], &[], 4.05).is_none());
    }
    #[test]
    fn ranged_retreat_prefers_an_alternative_to_a_recently_traversed_landing() {
        let mut o = world();
        o.world["entities"][0]["position"] = json!([-8.5, 1., 0.5]);
        o.world["entities"][0]["ranged_attack"] = json!(true);
        let first = evade(&o).expect("clear retreat");
        let seen = [first.target.x, first.target.y, first.target.z];
        let alternative = evade_with_stationary(&o, &[], &[], &[seen]).expect("alternate retreat");
        assert!((alternative.target.z - first.target.z).abs() > 2.0);
        assert!(
            alternative.target.x > 1.0,
            "continue increasing enemy distance"
        );

        // A recently traversed corridor is still usable if it is the only
        // observed escape; history must never turn the floor into an obstacle.
        let mut corridor = world();
        corridor.world["blocks"]
            .as_array_mut()
            .unwrap()
            .retain(|b| b["position"][2] == 0);
        let seen: Vec<_> = (-4..=0).map(|x| [f64::from(x) + 0.5, 1., 0.5]).collect();
        let fallback =
            evade_with_stationary(&corridor, &[], &[], &seen).expect("only safe corridor");
        assert!(fallback.target.x < 0.0);
        assert!((fallback.target.z - 0.5).abs() < 1e-6);
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
