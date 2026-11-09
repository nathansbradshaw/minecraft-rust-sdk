//! Bounded historical query recollection, never proof that resources are absent.
use crate::{Journal, Observation, Result, digest, discovery::search_fact, model::Choice, now};
use rusqlite::params;
use serde_json::{Value, json};

const CACHE_LIMIT: usize = 256;
const CONTEXT_LIMIT: usize = 4096;

fn terrain_context(observed: &Observation) -> String {
    let blocks:Vec<_>=observed.world["blocks"].as_array().into_iter().flatten()
        .map(|b|json!({"position":b["position"],"block":b["block"],"state":b["state"],"collision":b["collision"]})).collect();
    digest(
        json!({"blocks":blocks,"unknown_columns":observed.world["unknown_columns"]})
            .to_string()
            .as_bytes(),
    )
}

/// Remove complete old records or optional details, never truncate registry IDs
/// or turn a positive result into an apparent empty search.
pub(crate) fn compact_view(view: &mut Value, budget: usize) {
    while view.to_string().len() > budget {
        let Some(records) = view["records"].as_array_mut() else {
            break;
        };
        if records.len() > 1 {
            records.remove(0);
            view["omitted_recent_records"] =
                json!(view["omitted_recent_records"].as_u64().unwrap_or(0) + 1);
        } else if let Some(record) = records.first_mut() {
            if let Some(matches) = record["fact"]["matches"].as_array_mut()
                && !matches.is_empty()
            {
                matches.pop();
                record["fact"]["omitted_matches"] =
                    json!(record["fact"]["omitted_matches"].as_u64().unwrap_or(0) + 1);
            } else if record["context"]["omitted_for_context_budget"] != true {
                record["context"] = json!({"omitted_for_context_budget":true,"retained_in":"client search cache and tool history"});
            } else {
                records.clear();
                view["omitted_recent_records"] =
                    json!(view["omitted_recent_records"].as_u64().unwrap_or(0) + 1);
                view["elided_for_context_budget"] = json!(true);
            }
        } else {
            break;
        }
    }
}

impl Journal {
    pub(crate) fn remember_search(
        &self,
        choice: &Choice,
        result: &Value,
        goal_revision: &str,
        instance: &str,
        observed: &Observation,
    ) -> Result<()> {
        let Some(fact) = search_fact(choice, result) else {
            return Ok(());
        };
        let (Some(actor), Some(dimension)) = (
            observed.player["uuid"].as_str(),
            observed.player["dimension"].as_str(),
        ) else {
            return Ok(());
        };
        if [instance, actor, dimension]
            .iter()
            .any(|s| s.is_empty() || s.len() > 256)
            || fact.to_string().len() > 16384
            || fact["matches"].as_array().unwrap().iter().any(|row| {
                row["position"]
                    .as_array()
                    .is_none_or(|p| p.len() != 3 || p.iter().any(|n| n.as_i64().is_none()))
                    || !["block", "item"]
                        .iter()
                        .all(|name| row[*name].as_str().is_some_and(minecraft_sdk::registry_id))
            })
        {
            return Ok(());
        }
        // Coalesce reobservations of the same page. Its latest response replaces
        // this cache slot; the full original tool events remain in the journal.
        let key = digest(
            json!([
                instance,
                actor,
                dimension,
                fact["selector"],
                fact["origin"],
                fact["radius"],
                fact["from"]
            ])
            .to_string()
            .as_bytes(),
        );
        let context = json!({"goal_revision":goal_revision,"context_revision":observed.revision,
            "context_entity_id":observed.player["entity_id"],
            "context_epoch":observed.world["sample_clock"]["epoch"],
            "context_terrain_sha256":terrain_context(observed)});
        if context.to_string().len() > 3072 {
            return Ok(());
        }
        let tx = self.db.unchecked_transaction()?;
        tx.execute("INSERT INTO search_observations(key,instance,actor,dimension,seen_at,observations,fact,context) VALUES(?1,?2,?3,?4,?5,1,?6,?7) ON CONFLICT(key) DO UPDATE SET seen_at=excluded.seen_at,observations=search_observations.observations+1,fact=excluded.fact,context=excluded.context",
            params![key,instance,actor,dimension,now(),fact.to_string(),context.to_string()])?;
        tx.execute("DELETE FROM search_observations WHERE id NOT IN (SELECT id FROM search_observations ORDER BY seen_at DESC,id DESC LIMIT ?1)", [CACHE_LIMIT as i64])?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn recalled_searches(
        &self,
        instance: &str,
        observed: &Observation,
    ) -> Result<Value> {
        let actor = observed.player["uuid"].as_str().unwrap_or_default();
        let dimension = observed.player["dimension"].as_str().unwrap_or_default();
        let mut statement=self.db.prepare("SELECT seen_at,observations,fact,context FROM search_observations WHERE instance=?1 AND actor=?2 AND dimension=?3 ORDER BY seen_at DESC,id DESC LIMIT 12")?;
        let mut rows = statement.query(params![instance, actor, dimension])?;
        let mut records = Vec::new();
        let current_terrain = terrain_context(observed);
        let mut omitted = 0usize;
        while let Some(row) = rows.next()? {
            let at = row.get::<_, i64>(0)?;
            let mut fact: Value = serde_json::from_str(&row.get::<_, String>(2)?)?;
            let context: Value = serde_json::from_str(&row.get::<_, String>(3)?)?;
            if let Some(matches) = fact["matches"].as_array_mut() {
                let reported_matches = matches.len();
                let omitted_matches = matches.len().saturating_sub(4);
                matches.truncate(4);
                fact["reported_matches"] = json!(reported_matches);
                fact["omitted_matches"] = json!(omitted_matches);
            }
            let same_lane = context["context_entity_id"].as_i64().is_some()
                && context["context_entity_id"] == observed.player["entity_id"]
                && context["context_epoch"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
                && context["context_epoch"] == observed.world["sample_clock"]["epoch"];
            let record = json!({"last_seen":at,"age_seconds":now().saturating_sub(at),
                "observations":row.get::<_,i64>(1)?,"fact":fact,"context":context,
                "matches_current_context_lane":same_lane,
                "matches_current_context_terrain":context["context_terrain_sha256"]==current_terrain});
            if records.len() == 6
                || serde_json::to_vec(&records)?.len() + record.to_string().len()
                    > CONTEXT_LIMIT - 768
            {
                omitted += 1;
            } else {
                records.push(record);
            }
        }
        // Context compaction preserves array tails. Put the newest facts last.
        records.reverse();
        Ok(
            json!({"format":"spatial-search-memory-v1","records":records,
            "omitted_recent_records":omitted,"cache_limit":CACHE_LIMIT,"native_authority":false,
            "scope":"Historical SDK query pages received by this client. Search checks loaded ray-visible block centers only. Complete means the query page scan ended, not that occluded/unloaded resources are absent. Partial pages do not establish absence. Context observations and queries are not atomic; matching a life/epoch does not prove terrain unchanged. Repeated overlapping scans without useful new facts call for a different view, route or experiment. Full tool history remains in the journal."}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn observed() -> Observation {
        serde_json::from_value(json!({"revision":"context-one","player":{
            "uuid":"actor","entity_id":1,"dimension":"fixture:world","position":[0.5,64.,0.5]},
            "world":{"sample_clock":{"epoch":"clock-one"}},"menu":null}))
        .unwrap()
    }
    fn query(x: i32) -> (Choice, Value) {
        (
            Choice {
                tool: "query".into(),
                input: json!({"capability":"minecraft:block_search",
            "input":{"tag":"fixture:resource","radius":32,"offset":0}})
                .to_string(),
                path: String::new(),
                notes: String::new(),
                summary: String::new(),
                experiment: false,
            },
            json!({"capability":"minecraft:block_search","result":{"origin":[x,64,0],
            "radius":32,"matches":[],"checked_blocks":100,"complete":true,"next_offset":null}}),
        )
    }
    #[test]
    fn repeated_empty_pages_survive_restart_without_renewing_diagnosis_or_other_actor_access() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("brain.sqlite");
        let journal = Journal::open(&path).unwrap();
        journal.set("inspection_streak", "8").unwrap();
        journal.set("investigation_remaining", "0").unwrap();
        let o = observed();
        let (choice, result) = query(0);
        for _ in 0..3 {
            journal
                .remember_search(&choice, &result, "first-goal", "instance", &o)
                .unwrap();
        }
        assert_eq!(
            journal.get("inspection_streak").unwrap().as_deref(),
            Some("8")
        );
        assert_eq!(
            journal.get("investigation_remaining").unwrap().as_deref(),
            Some("0")
        );
        drop(journal);
        let journal = Journal::open(&path).unwrap();
        let recalled = journal.recalled_searches("instance", &o).unwrap();
        assert_eq!(recalled["records"].as_array().unwrap().len(), 1);
        assert_eq!(recalled["records"][0]["observations"], 3);
        assert_eq!(recalled["records"][0]["fact"]["reported_matches"], 0);
        assert_eq!(
            recalled["records"][0]["context"]["goal_revision"],
            "first-goal"
        );
        assert_eq!(recalled["native_authority"], false);
        assert!(
            recalled["scope"]
                .as_str()
                .unwrap()
                .contains("not that occluded/unloaded resources are absent")
        );
        for case in ["instance", "actor", "dimension", "life", "epoch", "terrain"] {
            let mut changed = o.clone();
            let instance = if case == "instance" {
                "other"
            } else {
                "instance"
            };
            match case {
                "actor" => changed.player["uuid"] = json!("other"),
                "dimension" => changed.player["dimension"] = json!("other:world"),
                "life" => changed.player["entity_id"] = json!(2),
                "epoch" => changed.world["sample_clock"]["epoch"] = json!("clock-two"),
                "terrain" => {
                    changed.world["blocks"] =
                        json!([{"position":[1,64,0],"block":"fixture:changed"}])
                }
                _ => (),
            }
            let result = journal.recalled_searches(instance, &changed).unwrap();
            if case == "terrain" {
                assert_eq!(
                    result["records"][0]["matches_current_context_terrain"],
                    false
                );
            } else if matches!(case, "life" | "epoch") {
                assert_eq!(result["records"][0]["matches_current_context_lane"], false);
            } else {
                assert!(result["records"].as_array().unwrap().is_empty());
            }
        }
    }
    #[test]
    fn partial_and_positive_pages_keep_counts_while_malformed_and_pacing_pages_do_not_enter_memory()
    {
        let root = tempfile::tempdir().unwrap();
        let journal = Journal::open(&root.path().join("brain.sqlite")).unwrap();
        let (choice, mut result) = query(0);
        result["result"]["complete"] = json!(false);
        result["result"]["next_offset"] = json!(100);
        result["result"]["matches"] = json!(
            (0..8)
                .map(|x| json!({"position":[x,64,0],
            "block":"fixture:block","item":"fixture:item","visible":true,"may_interact":true}))
                .collect::<Vec<_>>()
        );
        journal
            .remember_search(&choice, &result, "goal", "instance", &observed())
            .unwrap();
        let recalled = journal.recalled_searches("instance", &observed()).unwrap();
        let fact = &recalled["records"][0]["fact"];
        assert_eq!(fact["complete"], false);
        assert_eq!(fact["reported_matches"], 8);
        assert_eq!(fact["omitted_matches"], 4);
        assert_eq!(fact["matches"].as_array().unwrap().len(), 4);
        let mut compact = recalled.clone();
        compact_view(&mut compact, 1500);
        assert!(compact.to_string().len() <= 1500);
        if let Some(record) = compact["records"].as_array().unwrap().first() {
            assert_eq!(
                record["fact"]["reported_matches"], 8,
                "Context pressure cannot turn an abbreviated positive result into absence"
            );
        }
        for case in ["pacing", "malformed", "oversized"] {
            let mut bad = result.clone();
            match case {
                "pacing" => {
                    bad["result"]["checked_blocks"] = json!(0);
                }
                "malformed" => {
                    bad["result"]["matches"][0]["position"] = json!("not-a-position");
                }
                _ => {
                    bad["result"]["matches"][0]["block"] = json!("x".repeat(20000));
                }
            }
            journal
                .remember_search(&choice, &bad, "goal", "instance", &observed())
                .unwrap();
        }
        assert_eq!(
            journal.recalled_searches("instance", &observed()).unwrap()["records"][0]["observations"],
            1
        );
    }
    #[test]
    fn cache_and_context_are_bounded_and_newest_records_remain_at_the_array_tail() {
        let root = tempfile::tempdir().unwrap();
        let journal = Journal::open(&root.path().join("brain.sqlite")).unwrap();
        journal
            .event("fixture_history", json!({"preserve":true}))
            .unwrap();
        for x in 0..300 {
            let (choice, result) = query(x);
            journal
                .remember_search(&choice, &result, "goal", "instance", &observed())
                .unwrap();
        }
        let count: i64 = journal
            .db
            .query_row("SELECT count(*) FROM search_observations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 256);
        let result = journal.recalled_searches("instance", &observed()).unwrap();
        assert!(result.to_string().len() <= CONTEXT_LIMIT);
        assert!(result["records"].as_array().unwrap().len() <= 6);
        assert_eq!(
            result["records"].as_array().unwrap().last().unwrap()["fact"]["origin"][0],
            299
        );
        let mut frame = json!({"sdk_reference":"reference documentation".repeat(20000),
            "spatial_searches":result,"recent":[{"data":"old history".repeat(20000)}]});
        crate::fit_context(&mut frame).unwrap();
        assert!(frame.to_string().len() <= 32768);
        assert_eq!(
            frame["spatial_searches"]["records"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()["fact"]["origin"][0],
            299
        );
        let events: i64 = journal
            .db
            .query_row(
                "SELECT count(*) FROM events WHERE kind='fixture_history'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            events, 1,
            "Only disposable lookup cache may be pruned; original history stays intact"
        );
    }
}
