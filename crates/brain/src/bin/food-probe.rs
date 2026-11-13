//! Operator-supplied food diagnostic; never autonomous acquisition evidence.
use minecraft_sdk::{Observation, Profile};
use minecraft_sdk_brain::{Brain, Config, Result, body, ensure, private_read, tools};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::Path,
    time::{Duration, Instant},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Spec {
    player_uuid: String,
    player_position: [f64; 3],
    source_slot: u8,
}

fn inventory(o: &Observation) -> Result<&Vec<Value>> {
    o.player["inventory"]
        .as_array()
        .ok_or_else(|| "Inventory missing".into())
}
fn slot(o: &Observation, n: u8) -> Result<&Value> {
    let mut rows = inventory(o)?.iter().filter(|row| row["slot"] == n);
    let row = rows.next().ok_or("Inventory slot missing")?;
    ensure(rows.next().is_none(), "Duplicate inventory slot")?;
    Ok(row)
}
fn bread(o: &Observation) -> Result<u64> {
    inventory(o)?
        .iter()
        .filter(|row| row["item"] == "minecraft:bread")
        .try_fold(0u64, |sum, row| {
            sum.checked_add(row["count"].as_u64().ok_or("Item count missing")?)
                .ok_or_else(|| "Item count overflow".into())
        })
}
fn admit(spec: &Spec, o: &Observation) -> Result<()> {
    ensure(
        (9..=35).contains(&spec.source_slot) && spec.player_position.iter().all(|v| v.is_finite()),
        "Invalid food fixture bounds",
    )?;
    let position = o.player["position"]
        .as_array()
        .filter(|p| p.len() == 3)
        .ok_or("Player position missing")?;
    let distance = position
        .iter()
        .zip(spec.player_position)
        .try_fold(0., |sum, (v, expected)| {
            v.as_f64()
                .filter(|v| v.is_finite())
                .map(|v| sum + (v - expected).powi(2))
        })
        .ok_or("Player position invalid")?;
    ensure(
        o.player["uuid"] == spec.player_uuid && distance <= 0.25f64.powi(2),
        "Wrong character or fixture position",
    )?;
    ensure(
        o.player["health"] == 20.
            && o.player["max_health"] == 20.
            && o.player["food"] == 12
            && o.player["selected"] == 0
            && o.player["on_ground"] == true
            && o.player["sprinting"] == false
            && o.player["using_item"] == false
            && o.player["in_water"] == false
            && o.player["in_lava"] == false
            && o.player["fire_ticks"].as_i64().is_some_and(|n| n <= 0)
            && o.player["control"]["outcome"] != "running"
            && o.player["interaction"]["outcome"] != "running",
        "Food fixture player state changed",
    )?;
    ensure(
        o.menu["menu_id"] == 0
            && o.menu["type"] == "minecraft:inventory"
            && o.menu["carried"]["item"] == "minecraft:air"
            && o.menu["carried"]["count"] == 0,
        "Food fixture menu/cursor changed",
    )?;
    ensure(
        inventory(o)?.len() == 41,
        "Complete player inventory required",
    )?;
    for n in 0..=40 {
        let row = slot(o, n)?;
        let (item, count) = if n == 0 {
            ("minecraft:wooden_pickaxe", 1)
        } else if n == spec.source_slot {
            ("minecraft:bread", 2)
        } else {
            ("minecraft:air", 0)
        };
        ensure(
            row["item"] == item && row["count"] == count,
            "Food fixture inventory changed",
        )?;
    }
    ensure(
        o.world["entities"].as_array().is_some_and(|rows| {
            !rows
                .iter()
                .any(|e| e["hostile"] == true || e["projectile"] == true)
        }),
        "Food fixture has observed pressure",
    )?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure(
        args.len() == 4 && matches!(args[1].as_str(), "blocked" | "compile"),
        "food-probe PRIVATE_CONFIG.json blocked|compile SECONDS PRIVATE_SPEC.json",
    )?;
    let seconds: u64 = args[2].parse()?;
    ensure(
        (5..=120).contains(&seconds),
        "Food probe duration must be 5–120 seconds",
    )?;
    let spec: Spec = serde_json::from_slice(&private_read(Path::new(&args[3]), 16384)?)?;
    let mut cfg: Config = serde_json::from_slice(&private_read(Path::new(&args[0]), 16384)?)?;
    cfg.body.enabled = false;
    cfg.conversation = false;
    cfg.await_chat_goal = false;
    let mut brain = Brain::open(cfg).await?;
    let run = async {
        let before = brain.client.observe().await?;
        if let Err(error) = admit(&spec, &before) {
            brain.journal.event("food_probe_rejected", json!({"error":error.to_string(),"observation":before,"body_started":false}))?;
            return Err(error);
        }
        brain.journal.event("food_probe_admitted", json!({"observation":before,"models_called":0,
            "scope":"Operator-supplied bread and tool; native snapshot before starting body. No acquisition or gameplay competence claim."}))?;
        let profile: Profile = serde_json::from_slice(&private_read(&brain.cfg.profile, 16384)?)?;
        brain.cfg.body.enabled = true;
        brain.body = Some(body::Handle::start_for_server(profile, brain.cfg.body.clone(), &brain.cfg.state, before.clone(), &brain.server)?);
        let started = Instant::now();
        if args[1] == "compile" {
            brain.journal.event("food_probe_compile_started", json!({"elapsed_ms":started.elapsed().as_millis()}))?;
            let result = tools::terminal(&mut brain, "cargo build --offline --message-format=json").await?;
            brain.journal.event("food_probe_compile_finished", json!({"elapsed_ms":started.elapsed().as_millis(),"result":result,
                "scope":"Compiler success alone does not prove eating overlapped compilation; compare native action timestamps."}))?;
        }
        if let Some(remaining) = Duration::from_secs(seconds).checked_sub(started.elapsed()) {
            tokio::time::sleep(remaining).await;
        }
        let after = brain.client.observe().await?;
        brain.journal.event("food_probe_finished", json!({"mode":args[1],"elapsed_ms":started.elapsed().as_millis(),"observation":after,"models_called":0}))?;
        ensure(after.player["uuid"] == before.player["uuid"]
            && after.world["sample_clock"]["epoch"] == before.world["sample_clock"]["epoch"]
            && after.player["health"] == 20. && after.player["food"] == 17,
            "Food recovery/life continuity not observed")?;
        ensure(slot(&after, 0)? == slot(&before, 0)? && bread(&after)? == 1
            && after.menu["carried"]["item"] == "minecraft:air" && after.menu["carried"]["count"] == 0,
            "Tool/cursor preservation or exact meal consumption not observed")?;
        brain.journal.event("food_probe_state_verified", json!({"food_before":12,"food_after":17,"bread_consumed":1,
            "tool_preserved":true,"models_called":0,"scope":"Bounded supplied-food state check. Bind native receipts and saved state before acceptance; no autonomous acquisition claim."}))?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    }.await;
    let stopped = if let Some(body) = brain.body.take() {
        body.stop().await
    } else {
        Ok(())
    };
    let disconnected = brain.client.disconnect().await;
    brain.journal.event(
        "food_probe_exit",
        json!({"error":run.as_ref().err().map(ToString::to_string),
        "body_stopped":stopped.is_ok(),"disconnected":disconnected.is_ok()}),
    )?;
    run?;
    stopped?;
    disconnected?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Spec, Observation) {
        let spec = Spec {
            player_uuid: "fixture-actor".into(),
            player_position: [0.5, 1., 0.5],
            source_slot: 9,
        };
        let inventory: Vec<_> = (0..=40).map(|n| json!({"slot":n,"item":if n==0{"minecraft:wooden_pickaxe"}else if n==9{"minecraft:bread"}else{"minecraft:air"},"count":if n==0{1}else if n==9{2}else{0}})).collect();
        let o = Observation {
            revision: "1".into(),
            player: json!({"uuid":"fixture-actor","position":[0.5,1.,0.5],
            "health":20.,"max_health":20.,"food":12,"selected":0,"on_ground":true,"sprinting":false,"using_item":false,
            "in_water":false,"in_lava":false,"fire_ticks":0,"inventory":inventory}),
            world: json!({"entities":[]}),
            menu: json!({"menu_id":0,"type":"minecraft:inventory","carried":{"item":"minecraft:air","count":0}}),
        };
        (spec, o)
    }
    #[test]
    fn admission_precedes_body_and_rejects_wrong_actor_food_or_hotbar() {
        let (spec, o) = fixture();
        admit(&spec, &o).unwrap();
        for (path, value) in [
            ("/player/uuid", json!("other")),
            ("/player/food", json!(17)),
            ("/player/inventory/9/count", json!(1)),
            ("/player/inventory/1/item", json!("minecraft:bread")),
            ("/menu/carried/count", json!(1)),
            ("/player/control/outcome", json!("running")),
        ] {
            let mut changed = serde_json::to_value(&o).unwrap();
            if path == "/player/control/outcome" {
                changed["player"]["control"] = json!({"outcome":"running"});
            } else {
                *changed.pointer_mut(path).unwrap() = value;
            }
            assert!(
                admit(&spec, &serde_json::from_value(changed).unwrap()).is_err(),
                "{path}"
            );
        }
    }
}
