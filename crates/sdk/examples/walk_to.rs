//! Optional example navigation strategy, using only runtime geometry and typed SDK actions.
use minecraft_sdk::*;
use std::collections::{HashSet, VecDeque};
type AnyResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f64>().sqrt()
}
fn key(p: [f64; 3]) -> [i32; 3] {
    [
        (p[0] * 16.).round() as i32,
        (p[1] * 16.).round() as i32,
        (p[2] * 16.).round() as i32,
    ]
}
#[tokio::main]
async fn main() -> AnyResult<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 4 {
        return Err("Usage: walk_to CONNECTED_PROFILE X Y Z".into());
    }
    let profile: Profile = serde_json::from_slice(&std::fs::read(&args[0])?)?;
    let client = Client::with_profile(&profile)?;
    client.verify_instance(&profile.instance_id).await?;
    let goal = [args[1].parse()?, args[2].parse()?, args[3].parse()?];
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis();
    let mut blocked = HashSet::new();
    for step in 0..120 {
        let observation = client.observe().await?;
        if observation.player["health"]
            .as_f64()
            .is_none_or(|h| h <= 0.)
        {
            return Err("Character died; explicit respawn required".into());
        }
        let p = &observation.player["position"];
        let start = [
            p[0].as_f64().ok_or("Position missing")?,
            p[1].as_f64().ok_or("Position missing")?,
            p[2].as_f64().ok_or("Position missing")?,
        ];
        if distance(start, goal) < 1.1 {
            println!("Reached {} {} {}", start[0], start[1], start[2]);
            return Ok(());
        }
        let mut boxes = Vec::new();
        for block in observation.world["blocks"]
            .as_array()
            .ok_or("Terrain missing")?
        {
            let b = &block["position"];
            let origin = [
                b[0].as_f64().ok_or("Block missing")?,
                b[1].as_f64().ok_or("Block missing")?,
                b[2].as_f64().ok_or("Block missing")?,
            ];
            for shape in block["collision"].as_array().ok_or("Geometry missing")? {
                let mut bounds = [0.; 6];
                for i in 0..6 {
                    bounds[i] = origin[i % 3] + shape[i].as_f64().ok_or("Geometry missing")?;
                }
                boxes.push(bounds);
            }
        }
        let mut nodes = vec![start];
        for block in observation.world["blocks"].as_array().unwrap() {
            let b = &block["position"];
            let x = b[0].as_f64().unwrap() + 0.5;
            let z = b[2].as_f64().unwrap() + 0.5;
            for shape in block["collision"].as_array().unwrap() {
                let y = b[1].as_f64().unwrap() + shape[4].as_f64().unwrap();
                let p = [x, y, z];
                if (y - start[1]).abs() > 3. || nodes.iter().any(|n| distance(*n, p) < 0.01) {
                    continue;
                }
                if boxes.iter().any(|a| {
                    p[0] + 0.3 > a[0] + 1e-6
                        && p[0] - 0.3 < a[3] - 1e-6
                        && p[1] + 1.8 > a[1] + 1e-6
                        && p[1] < a[4] - 1e-6
                        && p[2] + 0.3 > a[2] + 1e-6
                        && p[2] - 0.3 < a[5] - 1e-6
                }) {
                    continue;
                }
                nodes.push(p);
            }
        }
        let mut parent = vec![None; nodes.len()];
        parent[0] = Some(0);
        let mut queue = VecDeque::from([0]);
        let mut best = 0;
        while let Some(a) = queue.pop_front() {
            if distance(nodes[a], goal) < distance(nodes[best], goal) {
                best = a;
            }
            for b in 1..nodes.len() {
                if parent[b].is_some() || blocked.contains(&(key(nodes[a]), key(nodes[b]))) {
                    continue;
                }
                let dx = (nodes[a][0] - nodes[b][0]).abs();
                let dz = (nodes[a][2] - nodes[b][2]).abs();
                let adjacent = (dx + dz <= 1.1)
                    && (dx < 0.1 || dz < 0.1)
                    && nodes[b][1] - nodes[a][1] <= 1.01
                    && nodes[a][1] - nodes[b][1] <= 3.;
                if adjacent {
                    parent[b] = Some(a);
                    queue.push_back(b);
                }
            }
        }
        if best == 0 {
            return Err("No observed supported route approaches the goal; use a smaller waypoint or interact with the obstruction".into());
        }
        let mut next = best;
        while parent[next] != Some(0) {
            next = parent[next].ok_or("Route missing")?;
        }
        let target = nodes[next];
        let receipt = client
            .act(&ActionRequest {
                action_id: format!("walk-{nonce}-{step}"),
                action: Action::Move {
                    target: Position {
                        x: target[0],
                        y: target[1],
                        z: target[2],
                    },
                    on_ground: false,
                    jump: false,
                },
            })
            .await?;
        println!("{step} {:?} {:?}", receipt.outcome, target);
        match receipt.outcome {
            Outcome::Observed => {}
            Outcome::Rejected => {
                blocked.insert((key(start), key(target)));
            }
            _ => {
                return Err(format!(
                    "Action {} requires explicit reconciliation",
                    receipt.action_id
                )
                .into());
            }
        }
    }
    Err("Example navigation step budget exceeded".into())
}
