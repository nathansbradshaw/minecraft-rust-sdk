//! Bounded dry-ground approach to currently observed ballistic cover.
use super::*;

fn avoids_contact(a: [f64; 3], b: [f64; 3], enemies: &[[f64; 3]]) -> bool {
    let dx = b[0] - a[0];
    let dz = b[2] - a[2];
    let length = dx * dx + dz * dz;
    enemies.iter().all(|e| {
        let t = if length > 0. {
            ((e[0] - a[0]) * dx + (e[2] - a[2]) * dz) / length
        } else {
            0.
        }
        .clamp(0., 1.);
        (a[0] + t * dx - e[0]).hypot(a[2] + t * dz - e[2]) >= 4.5
    })
}

fn segment(cells: &Cells<'_>, obstacles: &[[f64; 6]], a: [f64; 3], b: [f64; 3]) -> bool {
    let steps = ((a[0] - b[0]).hypot(a[2] - b[2]) * 8.).ceil().max(1.) as u32;
    (0..=steps).all(|n| {
        let t = f64::from(n) / f64::from(steps);
        let p = std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t);
        clear(cells, p, obstacles) && supported(cells, p, obstacles)
    })
}
fn shielded(p: [f64; 3], enemies: &[[f64; 3]], screens: &[[f64; 6]]) -> bool {
    enemies.iter().all(|enemy| {
        [-0.25, 0.25].iter().all(|dx| {
            [-0.25, 0.25].iter().all(|dz| {
                [0.3, 1.6].iter().all(|height| {
                    occluded(
                        [p[0] + dx, p[1] + height, p[2] + dz],
                        [enemy[0], enemy[1] + 1.6, enemy[2]],
                        screens,
                    )
                })
            })
        })
    })
}
/// All points and transitions remain supported by the current observation.
/// This predicts occlusion, not future enemy behavior or verified protection.
pub(super) enum Plan {
    Move(Escape),
    Hold,
}
pub(super) fn plan(o: &Observation, blocked: &[[i32; 3]], axis: f64) -> Option<Plan> {
    if o.player["on_ground"] != true || o.player["in_water"] == true || o.player["in_lava"] == true
    {
        return None;
    }
    let start = point(&o.player["position"])?;
    let mut enemies = Vec::new();
    for enemy in o.world["entities"]
        .as_array()?
        .iter()
        .filter(|e| perceived(e) && e["hostile"] == true)
    {
        if enemy["ranged_attack"] != true {
            return None;
        }
        let p = point(&enemy["position"])?;
        enemies.push(p);
        if enemies.len() > 8 {
            return None;
        }
    }
    if enemies.is_empty() {
        return None;
    }
    let cells: Cells<'_> = o.world["blocks"]
        .as_array()?
        .iter()
        .filter_map(|b| point(&b["position"]).map(|p| (p.map(|v| v as i32), b)))
        .collect();
    let obstacles = boxes(o);
    let screens: Vec<_> = obstacles
        .iter()
        .copied()
        .filter(|b| b[4] > start[1] + 0.3 && b[1] < start[1] + 1.8)
        .collect();
    if screens.is_empty() {
        return None;
    }
    let guarding = shielded(start, &enemies, &screens);
    // A nearby archer across a solid screen does not establish an open contact
    // route. Retain the guard only while every point stays fully occluded.
    if !guarding
        && enemies
            .iter()
            .any(|e| (e[0] - start[0]).hypot(e[2] - start[2]) < 5.)
    {
        return None;
    }
    let adjacent = |p: [f64; 3]| {
        screens
            .iter()
            .any(|b| (p[0] - p[0].clamp(b[0], b[3])).hypot(p[2] - p[2].clamp(b[2], b[5])) <= 0.85)
    };
    let shadow_segment = |a: [f64; 3], b: [f64; 3]| {
        let steps = ((a[0] - b[0]).hypot(a[2] - b[2]) * 4.).ceil().max(1.) as u32;
        (0..=steps).all(|n| {
            let t = f64::from(n) / f64::from(steps);
            shielded(
                std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t),
                &enemies,
                &screens,
            )
        })
    };
    let separation = |p: [f64; 3]| {
        enemies
            .iter()
            .map(|e| (p[0] - e[0]).hypot(p[2] - e[2]))
            .fold(24., f64::min)
    };
    // Distance from the archer alone can reward walking to the very end of a
    // screen. Prefer room to move in either direction along its nearby face.
    // Every sampled point must remain known, supported and occluded; this is a
    // current geometric margin, not a prediction of the unseen enemy's pose.
    let guard_margin = |p: [f64; 3]| {
        let nearest = screens.iter().min_by(|a, b| {
            let distance = |v: &&[f64; 6]| {
                (p[0] - p[0].clamp(v[0], v[3])).hypot(p[2] - p[2].clamp(v[2], v[5]))
            };
            distance(a).total_cmp(&distance(b))
        });
        let Some(screen) = nearest else {
            return 0.;
        };
        let x_gap = (p[0] - p[0].clamp(screen[0], screen[3])).abs();
        let z_gap = (p[2] - p[2].clamp(screen[2], screen[5])).abs();
        let axis = if x_gap > z_gap { 2 } else { 0 };
        [-1., 1.]
            .into_iter()
            .map(|sign| {
                let mut margin = 0.;
                for n in 1..=4 {
                    let mut next = p;
                    next[axis] += sign * f64::from(n) * 0.5;
                    if !clear(&cells, next, &obstacles)
                        || !supported(&cells, next, &obstacles)
                        || !shielded(next, &enemies, &screens)
                    {
                        break;
                    }
                    margin = f64::from(n) * 0.5;
                }
                margin
            })
            .fold(2., f64::min)
    };
    let guard_value = |p| guard_margin(p) * 4. + separation(p).min(8.);
    let mut nodes = vec![start];
    for x in -4..=4 {
        for z in -4..=4 {
            let p = [
                start[0].floor() + f64::from(x) + 0.5,
                start[1],
                start[2].floor() + f64::from(z) + 0.5,
            ];
            if !blocked.contains(&target_key(p))
                && (p[0] - start[0]).hypot(p[2] - start[2]) > 0.1
                && (guarding
                    || enemies
                        .iter()
                        .all(|e| (p[0] - e[0]).hypot(p[2] - e[2]) >= 4.5))
                && clear(&cells, p, &obstacles)
                && supported(&cells, p, &obstacles)
                && (!guarding || shielded(p, &enemies, &screens))
            {
                nodes.push(p);
            }
        }
    }
    let mut distance = vec![f64::INFINITY; nodes.len()];
    let mut previous = vec![None; nodes.len()];
    let mut visited = vec![false; nodes.len()];
    distance[0] = 0.;
    let mut goal = None;
    let mut best_guard = f64::NEG_INFINITY;
    for _ in 0..nodes.len() {
        let Some(current) = (0..nodes.len())
            .filter(|&i| !visited[i] && distance[i] <= 8.)
            .min_by(|&a, &b| distance[a].total_cmp(&distance[b]))
        else {
            break;
        };
        visited[current] = true;
        if shielded(nodes[current], &enemies, &screens) {
            if !guarding && current != 0 {
                goal = Some(current);
                break;
            }
            if guarding && adjacent(nodes[current]) {
                let value = guard_value(nodes[current]) - distance[current] * 0.15;
                if value > best_guard {
                    best_guard = value;
                    goal = Some(current);
                }
            }
        }
        for next in 1..nodes.len() {
            if visited[next] {
                continue;
            }
            let length =
                (nodes[current][0] - nodes[next][0]).hypot(nodes[current][2] - nodes[next][2]);
            if length <= 1.6
                && distance[current] + length < distance[next]
                && (guarding || avoids_contact(nodes[current], nodes[next], &enemies))
                && segment(&cells, &obstacles, nodes[current], nodes[next])
                && (!guarding || shadow_segment(nodes[current], nodes[next]))
            {
                distance[next] = distance[current] + length;
                previous[next] = Some(current);
            }
        }
    }
    if guarding && adjacent(start) && best_guard <= guard_value(start) + 0.15 {
        return Some(Plan::Hold);
    }
    let mut at = goal?;
    if at == 0 {
        return Some(Plan::Hold);
    }
    let mut path = vec![nodes[at]];
    while let Some(parent) = previous[at] {
        if parent == 0 {
            break;
        }
        path.push(nodes[parent]);
        at = parent;
    }
    path.reverse();
    let target = path.iter().rev().find(|&&p| {
        (p[0] - start[0]).abs() <= axis
            && (p[2] - start[2]).abs() <= axis
            && (p[0] - start[0]).hypot(p[2] - start[2]) >= 0.35
            && (guarding || avoids_contact(start, p, &enemies))
            && segment(&cells, &obstacles, start, p)
            && (!guarding || shadow_segment(start, p))
    })?;
    Some(Plan::Move(Escape {
        target: Position {
            x: target[0],
            y: target[1],
            z: target[2],
        },
        jump: false,
        reason: if guarding {
            "maintain guarded position beside observed ranged cover"
        } else {
            "approach observed ranged cover around a supported corner"
        }
        .into(),
        cover_path: path
            .into_iter()
            .map(|p| Position {
                x: p[0],
                y: p[1],
                z: p[2],
            })
            .collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn safe_endpoints_cannot_shortcut_through_an_archers_contact_range() {
        assert!(!avoids_contact(
            [-5., 1., 0.],
            [5., 1., 0.],
            &[[0., 1., 0.]]
        ));
        assert!(avoids_contact([-5., 1., 5.], [5., 1., 5.], &[[0., 1., 0.]]));
    }
}
