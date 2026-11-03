//! Historical client recollection of inspected menus, never current authority.
use crate::{Journal, Observation, Result, digest, now};
use minecraft_sdk::{Action, ActionRequest, Outcome, Receipt};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Native observations supply every slot or fail. Unknown or malformed fields
/// must not turn an incomplete view into a claim that a container is empty.
pub fn menu_summary(menu: &Value) -> Option<Value> {
    let slots = menu["slots"].as_array()?;
    if slots.is_empty() || slots.len() > 512 {
        return None;
    }
    let mut seen = BTreeSet::new();
    let mut items = BTreeMap::<String, u64>::new();
    let mut container_slots = 0;
    let mut occupied = 0;
    for row in slots {
        let slot = usize::try_from(row["slot"].as_u64()?).ok()?;
        if slot >= slots.len() || !seen.insert(slot) {
            return None;
        }
        let player = row["player_inventory"].as_bool()?;
        let item = row["item"].as_str()?;
        let count = row["count"].as_u64()?;
        if !minecraft_sdk::registry_id(item) || (item == "minecraft:air") != (count == 0) {
            return None;
        }
        if !player {
            container_slots += 1;
            if count > 0 {
                occupied += 1;
                let value = items.entry(item.to_owned()).or_default();
                *value = value.checked_add(count)?;
            }
        }
    }
    let distinct = items.len();
    let items: BTreeMap<_, _> = items.into_iter().take(16).collect();
    Some(
        json!({"scope":"complete received SDK menu snapshot; non-player slots only","total_slots":slots.len(),"non_player_slots":container_slots,"occupied_non_player_slots":occupied,"empty":occupied==0,"items":items,"omitted_item_types":distinct.saturating_sub(16)}),
    )
}

/// Extract stable origin only from a verified menu-opening action. Numeric
/// entity and menu IDs are transient and deliberately excluded from memory.
pub fn inspection(request: &ActionRequest, receipt: &Receipt) -> Option<Value> {
    if receipt.action_id != request.action_id
        || receipt.outcome != Outcome::Observed
        || receipt.evidence["native_effect_verified"] != true
    {
        return None;
    }
    let before = &receipt.evidence["before"];
    let after = &receipt.evidence["after"];
    let actor = before["player"]["uuid"].as_str()?;
    let dimension = before["player"]["dimension"].as_str()?;
    before["menu"]["menu_id"].as_i64()?;
    if after["player"]["uuid"] != actor
        || after["player"]["dimension"] != dimension
        || after["menu"]["menu_id"].as_i64()? == 0
        || before["menu"]["menu_id"] == after["menu"]["menu_id"]
    {
        return None;
    }
    let origin = match &request.action {
        Action::Interact { entity, .. } => {
            let target = before["world"]["entities"]
                .as_array()?
                .iter()
                .find(|row| row["entity_id"] == *entity)?;
            let uuid = target["uuid"].as_str()?;
            if uuid.len() != 36
                || !uuid.bytes().enumerate().all(|(i, c)| {
                    if [8, 13, 18, 23].contains(&i) {
                        c == b'-'
                    } else {
                        c.is_ascii_hexdigit()
                    }
                })
            {
                return None;
            }
            json!({"entity_uuid":uuid,"type":target["type"]})
        }
        Action::UseBlock { block, .. } => json!({"block_position":[block.x,block.y,block.z]}),
        _ => return None,
    };
    let summary = menu_summary(&after["menu"])?;
    Some(
        json!({"format":"historical-container-v1","actor":actor,"dimension":dimension,"origin":origin,"summary":summary,"menu_type":after["menu"]["type"],"game_time":after["world"]["game_time"].as_i64()?,"revision":after["revision"],"action_id":receipt.action_id,"receipt_sha256":digest(serde_json::to_vec(receipt).ok()?.as_slice()),"native_authority":false,"binding":"Historical SDK result as seen by this client. Contents can change; reobserve when relevant new evidence warrants it. Do not repeat an empty inspection solely because it fell out of recent context."}),
    )
}

impl Journal {
    /// Reuse existing local SDK receipts without reconnecting, replaying an
    /// action or trusting a model-authored note. Bounded, resumable migration.
    pub fn import_container_history(&self, instance: &str, actor: &str) -> Result<usize> {
        let cursor_key = format!(
            "container_import_{}",
            digest(json!([instance, actor]).to_string().as_bytes())
        );
        let cursor = self
            .get(&cursor_key)?
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or_default();
        let mut stmt = self.db.prepare("SELECT id,data FROM events WHERE id>?1 AND kind IN ('action_receipt','action_status') ORDER BY id LIMIT 256")?;
        let mut rows = stmt.query([cursor])?;
        let mut count = 0;
        let mut through = cursor;
        while let Some(row) = rows.next()? {
            through = row.get(0)?;
            let text: String = row.get(1)?;
            let Ok(receipt) = serde_json::from_str::<Receipt>(&text) else {
                continue;
            };
            if self.remember_container_receipt(instance, actor, &receipt)? {
                count += 1;
            }
        }
        self.set(&cursor_key, &through.to_string())?;
        Ok(count)
    }

    pub(crate) fn remember_container_receipt(
        &self,
        instance: &str,
        actor: &str,
        receipt: &Receipt,
    ) -> Result<bool> {
        // Direct brain action IDs include the exact durable intention row ID.
        if !receipt.action_id.starts_with("brain-") {
            return Ok(false);
        }
        let Some(sequence) = receipt
            .action_id
            .rsplit('-')
            .next()
            .and_then(|s| s.parse::<i64>().ok())
        else {
            return Ok(false);
        };
        let started: Option<String> = self.db.query_row("SELECT data FROM events WHERE kind='brain_started' AND id<?1 ORDER BY id DESC LIMIT 1",[sequence],|row|row.get(0)).optional()?;
        let Some(started) = started else {
            return Ok(false);
        };
        let started: Value = serde_json::from_str(&started)?;
        if started["instance"] != instance || started["uuid"] != actor {
            return Ok(false);
        }
        let intent: Option<String> = self
            .db
            .query_row(
                "SELECT data FROM events WHERE id=?1 AND kind='action_intent'",
                [sequence],
                |row| row.get(0),
            )
            .optional()?;
        let Some(intent) = intent else {
            return Ok(false);
        };
        let intent: Value = serde_json::from_str(&intent)?;
        let Ok(action) = serde_json::from_value(intent["action"].clone()) else {
            return Ok(false);
        };
        let request = ActionRequest {
            action_id: receipt.action_id.clone(),
            action,
        };
        let Some(snapshot) = inspection(&request, receipt) else {
            return Ok(false);
        };
        if snapshot["actor"] != actor {
            return Ok(false);
        }
        self.remember_container(instance, actor, &snapshot)?;
        Ok(true)
    }

    pub(crate) fn remember_container(
        &self,
        instance: &str,
        actor: &str,
        value: &Value,
    ) -> Result<()> {
        if value["format"] != "historical-container-v1"
            || value["actor"] != actor
            || value.to_string().len() > 6000
        {
            return Ok(());
        }
        let Some(action_id) = value["action_id"].as_str() else {
            return Ok(());
        };
        let Some(time) = value["game_time"].as_i64() else {
            return Ok(());
        };
        let Some(dimension) = value["dimension"].as_str() else {
            return Ok(());
        };
        let key = digest(value["origin"].to_string().as_bytes());
        self.db.execute("INSERT OR IGNORE INTO container_inspections(instance,actor,dimension,origin_key,action_id,game_time,at,snapshot) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![instance,actor,dimension,key,action_id,time,now(),value.to_string()])?;
        Ok(())
    }

    pub fn recalled_containers(&self, instance: &str, o: &Observation) -> Result<Value> {
        let actor = o.player["uuid"].as_str().unwrap_or_default();
        let dimension = o.player["dimension"].as_str().unwrap_or_default();
        let mut stmt = self.db.prepare("SELECT origin_key,snapshot FROM container_inspections WHERE instance=?1 AND actor=?2 AND dimension=?3 ORDER BY game_time DESC,rowid DESC LIMIT 64")?;
        let mut rows = stmt.query(params![instance, actor, dimension])?;
        let mut seen = BTreeSet::new();
        let mut result = Vec::new();
        let mut bytes = 0;
        while let Some(row) = rows.next()? {
            if !seen.insert(row.get::<_, String>(0)?) {
                continue;
            }
            let text = row.get::<_, String>(1)?;
            if bytes + text.len() > 5000 {
                continue;
            }
            let mut value: Value = serde_json::from_str(&text)?;
            value["currently_observed_origin"] = json!(if let Some(uuid) =
                value["origin"]["entity_uuid"].as_str()
            {
                o.world["entities"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|row| row["uuid"] == uuid)
            } else {
                o.world["blocks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|row| row["position"] == value["origin"]["block_position"])
            });
            bytes += text.len();
            result.push(value);
            if result.len() == 8 {
                break;
            }
        }
        Ok(json!(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const UUID: &str = "12345678-1234-4abc-8def-1234567890ab";
    fn menu(count: u64) -> Value {
        json!({"menu_id":4,"state_id":1,"type":"mod:container","slots":[{"slot":0,"player_inventory":false,"item":if count==0{"minecraft:air"}else{"minecraft:oak_log"},"count":count},{"slot":1,"player_inventory":true,"item":"minecraft:diamond","count":64}]})
    }
    fn pair(id: &str, time: i64, count: u64) -> (ActionRequest, Receipt) {
        let request = ActionRequest {
            action_id: id.into(),
            action: Action::Interact {
                entity: 455,
                hand: minecraft_sdk::Hand::Main,
            },
        };
        let receipt = Receipt {
            action_id: id.into(),
            outcome: Outcome::Observed,
            adapter_version: "fixture".into(),
            error: None,
            evidence: json!({"native_effect_verified":true,"before":{"menu":{"menu_id":0},"player":{"uuid":"own","dimension":"mod:forest"},"world":{"entities":[{"entity_id":455,"uuid":UUID,"type":"mod:container_entity"}]}},"after":{"revision":time.to_string(),"player":{"uuid":"own","dimension":"mod:forest"},"world":{"game_time":time},"menu":menu(count)}}),
        };
        (request, receipt)
    }
    fn observed(entity: i32) -> Observation {
        Observation {
            revision: "current".into(),
            player: json!({"uuid":"own","dimension":"mod:forest"}),
            menu: Value::Null,
            world: json!({"entities":[{"entity_id":entity,"uuid":UUID}]}),
        }
    }
    #[test]
    fn full_summary_separates_player_items_and_never_calls_unknown_slots_empty() {
        let full = menu_summary(&menu(0)).unwrap();
        assert_eq!(full["empty"], true);
        assert_eq!(full["non_player_slots"], 1);
        assert_eq!(
            menu_summary(&menu(3)).unwrap()["items"]["minecraft:oak_log"],
            3
        );
        for bad in [
            json!({}),
            json!({"slots":[]}),
            json!({"slots":[{"slot":0,"item":"minecraft:air","count":0}]}),
            json!({"slots":[{"slot":1,"player_inventory":false,"item":"minecraft:air","count":0}]}),
        ] {
            assert!(menu_summary(&bad).is_none());
        }
        let mut bad = menu(0);
        bad["slots"][1]["slot"] = json!(0);
        assert!(menu_summary(&bad).is_none());
        let mut bad = menu(0);
        bad["slots"][0]["count"] = json!(2);
        assert!(menu_summary(&bad).is_none());
    }
    #[test]
    fn only_matching_native_verified_openings_produce_stable_historical_origins() {
        let (request, receipt) = pair("test", 5, 0);
        let snapshot = inspection(&request, &receipt).unwrap();
        assert_eq!(snapshot["origin"]["entity_uuid"], UUID);
        assert!(!snapshot.to_string().contains("entity_id"));
        assert!(!snapshot.to_string().contains("menu_id"));
        assert_eq!(snapshot["native_authority"], false);
        for kind in 0..5 {
            let mut bad = receipt.clone();
            match kind {
                0 => bad.outcome = Outcome::Uncertain,
                1 => bad.evidence["native_effect_verified"] = json!(false),
                2 => bad.evidence["after"]["player"]["uuid"] = json!("another"),
                3 => bad.evidence["after"]["menu"]["menu_id"] = json!(0),
                _ => bad.action_id = "other".into(),
            }
            assert!(inspection(&request, &bad).is_none());
        }
        let mut trace = crate::feedback::Trace::default();
        trace.record(
            &request,
            &minecraft_sdk::LocalActionResponse::Receipt(receipt),
        );
        assert_eq!(
            trace.since(0)["requests"][0]["container_inspection"]["summary"]["empty"],
            true
        );
    }
    #[test]
    fn restart_migration_scope_and_newer_contents_survive_id_changes_and_old_replay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("brain.sqlite");
        let j = Journal::open(&path).unwrap();
        j.event("brain_started", json!({"instance":"instance","uuid":"own"}))
            .unwrap();
        let (mut request, mut receipt) = pair("placeholder", 5, 0);
        let id = j
            .event("action_intent", json!({"action":request.action}))
            .unwrap();
        request.action_id = format!("brain-fixture-{id}");
        receipt.action_id = request.action_id.clone();
        j.event("action_receipt", json!(receipt)).unwrap();
        assert_eq!(
            j.import_container_history("other-instance", "own").unwrap(),
            0
        );
        assert_eq!(j.import_container_history("instance", "own").unwrap(), 1);
        assert_eq!(j.import_container_history("instance", "own").unwrap(), 0);
        drop(j);
        let j = Journal::open(&path).unwrap();
        let rows = j.recalled_containers("instance", &observed(999)).unwrap();
        assert_eq!(rows[0]["summary"]["empty"], true);
        assert_eq!(rows[0]["currently_observed_origin"], true);
        assert_eq!(
            j.recalled_containers("other-instance", &observed(999))
                .unwrap(),
            json!([])
        );
        let mut other = observed(999);
        other.player["uuid"] = json!("other");
        assert_eq!(
            j.recalled_containers("instance", &other).unwrap(),
            json!([])
        );
        let (request, receipt) = pair("changed", 10, 2);
        j.remember_container("instance", "own", &inspection(&request, &receipt).unwrap())
            .unwrap();
        let (request, receipt) = pair("late-old", 6, 0);
        j.remember_container("instance", "own", &inspection(&request, &receipt).unwrap())
            .unwrap();
        let rows = j.recalled_containers("instance", &observed(1000)).unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 1);
        assert_eq!(rows[0]["summary"]["items"]["minecraft:oak_log"], 2);
        j.remember("fact", "The container is full of diamonds")
            .unwrap();
        assert_eq!(
            j.recalled_containers("instance", &observed(1000)).unwrap(),
            rows
        );
    }
}
