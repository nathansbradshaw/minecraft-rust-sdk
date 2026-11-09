//! Bounded dry-ground approach to currently observed ballistic cover.
use super::*;

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
pub(super) fn approach(o: &Observation, blocked: &[[i32; 3]], axis: f64) -> Option<Escape> {
    if o.player["on_ground"] != true || o.player["in_water"] == true || o.player["in_lava"] == true
    {
        return None;
    }
    let start = point(&o.player["position"])?;
    let mut enemies = Vec::new();
    for enemy in o.world["entities"]
        .as_array()?
        .iter()
        .filter(|e| e["visible"] == true && e["hostile"] == true)
    {
        if enemy["ranged_attack"] != true {
            return None;
        }
        let p = point(&enemy["position"])?;
        if (p[0] - start[0]).hypot(p[2] - start[2]) < 5. {
            return None;
        }
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
    if screens.is_empty() || shielded(start, &enemies, &screens) {
        return None;
    }
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
                && enemies
                    .iter()
                    .all(|e| (p[0] - e[0]).hypot(p[2] - e[2]) >= 4.5)
                && clear(&cells, p, &obstacles)
                && supported(&cells, p, &obstacles)
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
    for _ in 0..nodes.len() {
        let current = (0..nodes.len())
            .filter(|&i| !visited[i] && distance[i] <= 8.)
            .min_by(|&a, &b| distance[a].total_cmp(&distance[b]))?;
        visited[current] = true;
        if current != 0 && shielded(nodes[current], &enemies, &screens) {
            goal = Some(current);
            break;
        }
        for next in 1..nodes.len() {
            if visited[next] {
                continue;
            }
            let length =
                (nodes[current][0] - nodes[next][0]).hypot(nodes[current][2] - nodes[next][2]);
            if length <= 1.6
                && distance[current] + length < distance[next]
                && segment(&cells, &obstacles, nodes[current], nodes[next])
            {
                distance[next] = distance[current] + length;
                previous[next] = Some(current);
            }
        }
    }
    let mut at = goal?;
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
            && segment(&cells, &obstacles, start, p)
    })?;
    Some(Escape {
        target: Position {
            x: target[0],
            y: target[1],
            z: target[2],
        },
        jump: false,
        reason: "approach observed ranged cover around a supported corner".into(),
        cover_path: path
            .into_iter()
            .map(|p| Position {
                x: p[0],
                y: p[1],
                z: p[2],
            })
            .collect(),
    })
}
