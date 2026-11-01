//! Explicit operator-run acceptance strategy for a disposable sand-biome test world.
//! This is an example bot program, never mandatory SDK behavior.
use minecraft_sdk::*;
use serde_json::{Value, json};
use std::time::Duration;
type AnyResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;
struct Probe {
    client: Client,
    prefix: String,
    next: u32,
}
impl Probe {
    async fn act(&mut self, action: Action) -> AnyResult<Receipt> {
        self.next += 1;
        let request = ActionRequest {
            action_id: format!("{}-{}", self.prefix, self.next),
            action,
        };
        let receipt = self.client.act(&request).await?;
        println!("{} {:?}", receipt.action_id, receipt.outcome);
        if receipt.outcome != Outcome::Observed {
            return Err(format!(
                "Action {} is {:?}; inspect and explicitly reconcile before continuing",
                receipt.action_id, receipt.outcome
            )
            .into());
        }
        let duplicate = self.client.act(&request).await?;
        if duplicate.action_id != receipt.action_id || duplicate.outcome != receipt.outcome {
            return Err("Duplicate receipt changed".into());
        }
        Ok(receipt)
    }
    async fn click(&mut self, slot: i16, button: u8, mode: u8) -> AnyResult<()> {
        let o = self.client.observe().await?;
        self.act(Action::ClickSlot {
            menu_id: o.menu["menu_id"].as_i64().ok_or("Menu missing")? as i32,
            state_id: o.menu["state_id"].as_i64().ok_or("Menu state missing")? as i32,
            slot,
            button,
            mode,
        })
        .await?;
        Ok(())
    }
}
#[tokio::main]
async fn main() -> AnyResult<()> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("Usage: minimal_survival CONNECTED_PRIVATE_PROFILE")?;
    let profile: Profile = serde_json::from_slice(&std::fs::read(path)?)?;
    let client = Client::with_profile(&profile)?;
    client.verify_instance(&profile.instance_id).await?;
    let prefix = format!(
        "survival-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis()
    );
    let mut probe = Probe {
        client,
        prefix,
        next: 0,
    };
    let mut original = probe.client.observe().await?;
    if original.player["health"].as_f64().is_some_and(|h| h <= 0.) {
        probe.act(Action::Respawn).await?;
        original = probe.client.observe().await?;
    }
    probe
        .act(Action::Look {
            yaw: 30.,
            pitch: 10.,
        })
        .await?;
    probe
        .act(Action::Chat {
            message: "SDK minimal Survival acceptance trial".into(),
        })
        .await?;
    let position = &original.player["position"];
    let craft_only = std::env::args().nth(2).as_deref() == Some("--craft-only");
    let x = position[0].as_f64().ok_or("Position missing")?.floor() as i32;
    let y = position[1].as_f64().ok_or("Position missing")?.floor() as i32;
    let z = position[2].as_f64().ok_or("Position missing")?.floor() as i32;
    if !craft_only {
        let terrain = original.world["blocks"]
            .as_array()
            .ok_or("Terrain missing")?;
        let candidates: Vec<(i32, i32)> = (-2..=2)
            .flat_map(|dx| (-2..=2).map(move |dz| (dx, dz)))
            .filter(|(dx, dz)| {
                *dx * *dx + *dz * *dz > 0
                    && *dx * *dx + *dz * *dz <= 5
                    && terrain.iter().any(|b| {
                        b["position"] == json!([x + dx, y - 1, z + dz])
                            && b["block"] == "minecraft:sand"
                    })
                    && [y, y + 1].iter().all(|height| {
                        terrain.iter().any(|b| {
                            b["position"] == json!([x + dx, height, z + dz])
                                && b["block"] == "minecraft:air"
                        })
                    })
            })
            .take(4)
            .collect();
        if candidates.len() != 4 {
            return Err("This example needs four nearby exposed sand blocks".into());
        }
        for (dx, dz) in candidates {
            let block = BlockPos {
                x: x + dx,
                y: y - 1,
                z: z + dz,
            };
            let before = probe.client.observe().await?;
            let target = before.world["blocks"]
                .as_array()
                .ok_or("Terrain missing")?
                .iter()
                .find(|b| b["position"] == json!([block.x, block.y, block.z]))
                .ok_or("Mining target unobserved")?;
            if target["block"] != "minecraft:sand" {
                return Err("This acceptance example needs four adjacent sand blocks; it will not mine other blocks".into());
            }
            let progress = target["destroy_progress"]
                .as_f64()
                .ok_or("Mining progress unavailable")?;
            if progress <= 0. {
                return Err("Mining target protected".into());
            }
            probe
                .act(Action::Dig {
                    block: block.clone(),
                    face: 1,
                    stage: DigStage::Start,
                })
                .await?;
            tokio::time::sleep(Duration::from_millis(
                ((1. / progress).ceil() as u64) * 50 + 150,
            ))
            .await;
            probe
                .act(Action::Dig {
                    block: block.clone(),
                    face: 1,
                    stage: DigStage::Finish,
                })
                .await?;
            tokio::time::sleep(Duration::from_millis(600)).await;
            probe
                .act(Action::Move {
                    target: Position {
                        x: f64::from(block.x) + 0.5,
                        y: f64::from(block.y),
                        z: f64::from(block.z) + 0.5,
                    },
                    on_ground: false,
                    jump: false,
                })
                .await?;
            tokio::time::sleep(Duration::from_millis(600)).await;
            probe
                .act(Action::Move {
                    target: Position {
                        x: position[0].as_f64().unwrap(),
                        y: position[1].as_f64().unwrap(),
                        z: position[2].as_f64().unwrap(),
                    },
                    on_ground: false,
                    jump: false,
                })
                .await?;
        }
    }
    let observation = probe.client.observe().await?;
    let source = observation.menu["slots"]
        .as_array()
        .ok_or("Slots missing")?
        .iter()
        .find(|s| {
            s["player_inventory"] == true
                && s["item"] == "minecraft:sand"
                && s["count"].as_i64().is_some_and(|n| n >= 4)
        })
        .ok_or("Four sand drops were not collected; inspect native state")?;
    probe
        .click(source["slot"].as_i64().ok_or("Slot missing")? as i16, 0, 0)
        .await?;
    for slot in 1..=4 {
        probe.click(slot, 1, 0).await?;
    }
    let crafted = probe.client.observe().await?;
    if crafted.menu["slots"][0]["item"] != "minecraft:sandstone" {
        return Err("Crafting output not observed".into());
    }
    probe.click(0, 0, 1).await?;
    let inventory = probe.client.observe().await?;
    let hotbar = inventory.player["inventory"]
        .as_array()
        .ok_or("Inventory missing")?
        .iter()
        .find(|s| {
            s["item"] == "minecraft:sandstone" && s["slot"].as_u64().is_some_and(|slot| slot < 9)
        })
        .ok_or("Sandstone not in hotbar")?;
    probe
        .act(Action::SelectSlot {
            slot: hotbar["slot"].as_u64().unwrap() as u8,
        })
        .await?;
    let placement = inventory.world["blocks"]
        .as_array()
        .ok_or("Terrain missing")?
        .iter()
        .find_map(|b| {
            let p = b["position"].as_array()?;
            let bx = p[0].as_i64()? as i32;
            let by = p[1].as_i64()? as i32;
            let bz = p[2].as_i64()? as i32;
            if b["block"] != "minecraft:air" || by != y || (bx - x).abs() + (bz - z).abs() != 1 {
                return None;
            }
            inventory.world["blocks"]
                .as_array()?
                .iter()
                .find(|s| s["position"] == json!([bx, by - 1, bz]) && s["replaceable"] == false)
                .map(|_| BlockPos {
                    x: bx,
                    y: by - 1,
                    z: bz,
                })
        })
        .ok_or("No adjacent supported placement cell")?;
    probe
        .act(Action::Place {
            block: placement,
            face: 1,
            hand: Hand::Main,
            hit: [0.5, 1., 0.5],
        })
        .await?;
    let final_state = probe.client.observe().await?;
    let report: Value = json!({"trial":"minimal_survival","player_uuid":final_state.player["uuid"],"last_action":probe.next,"mining_in_this_run":!craft_only,"crafting":true,"inventory_transfer":true,"building":true,"chat":true,"duplicate_action_receipts":true});
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
