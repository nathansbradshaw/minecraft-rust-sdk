//! New trusted observations can change a diagnosis without claiming execution.
use crate::{Journal, Result, digest, model::Choice};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

pub(crate) fn search_fact(choice: &Choice, value: &Value) -> Option<Value> {
    if choice.tool != "query" || value["capability"] != "minecraft:block_search" {
        return None;
    }
    let query: Value = serde_json::from_str(&choice.input).ok()?;
    if query["capability"] != "minecraft:block_search" {
        return None;
    }
    let input = &query["input"];
    let selector = match (input["block"].as_str(), input["tag"].as_str()) {
        (Some(block), None) => json!({"block":block}),
        (None, Some(tag)) => json!({"tag":tag}),
        _ => return None,
    };
    let result = &value["result"];
    let origin = result["origin"].as_array()?;
    let radius = result["radius"].as_u64()?;
    let rows = result["matches"].as_array()?;
    let complete = result["complete"].as_bool()?;
    let from = input.get("offset").map_or(Some(0), Value::as_u64)?;
    let through = result["next_offset"].as_u64();
    if origin.len() != 3
        || origin.iter().any(|n| n.as_i64().is_none())
        || !(1..=64).contains(&radius)
        || rows.len() > 32
        || result["checked_blocks"].as_u64().unwrap_or(0) == 0
        || (!complete && through.is_none_or(|end| end <= from))
    {
        return None;
    }
    // Do not hash clocks, retry hints, input nonces or unknown provider fields.
    let matches: Vec<_> = rows.iter().map(|row| json!({"position":row["position"],"block":row["block"],"item":row["item"],"visible":row["visible"],"may_interact":row["may_interact"]})).collect();
    Some(
        json!({"format":"block-search-discovery-v1","selector":selector,"origin":origin,"radius":radius,"from":from,"through":through,"complete":complete,"matches":matches}),
    )
}
impl Journal {
    pub(crate) fn search_discovery(
        &self,
        choice: &Choice,
        value: &Value,
        expected_revision: &str,
        instance: &str,
        dimension: &str,
    ) -> Result<bool> {
        let Some(fact) = search_fact(choice, value) else {
            return Ok(false);
        };
        if instance.is_empty() || dimension.is_empty() {
            return Ok(false);
        }
        let signature = digest(
            json!({"instance":instance,"dimension":dimension,"fact":fact})
                .to_string()
                .as_bytes(),
        );
        let tx = rusqlite::Transaction::new_unchecked(
            &self.db,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let current: Option<String> = tx
            .query_row(
                "SELECT value FROM meta WHERE key='goal_revision'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if current.as_deref().unwrap_or_default() != expected_revision {
            return Ok(false);
        }
        let key = format!("search-discovery:{expected_revision}:{signature}");
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO meta(key,value) VALUES(?1,'true')",
            [&key],
        )? == 1;
        if inserted {
            let previous: Option<String> = tx
                .query_row(
                    "SELECT value FROM meta WHERE key='discovery_revision'",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            let revision = digest(
                json!({"previous":previous,"new":signature})
                    .to_string()
                    .as_bytes(),
            );
            tx.execute("INSERT INTO meta(key,value) VALUES('discovery_revision',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![revision])?;
        }
        tx.commit()?;
        Ok(inserted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn search(radius: u32) -> (Choice, Value) {
        (Choice{tool:"query".into(),input:json!({"capability":"minecraft:block_search","input":{"tag":"minecraft:logs","radius":radius}}).to_string(),path:String::new(),summary:String::new(),notes:String::new(),experiment:false},json!({"capability":"minecraft:block_search","result":{"origin":[32,103,-15],"radius":radius,"matches":[],"checked_blocks":57777,"complete":true,"next_offset":null}}))
    }
    #[test]
    fn new_negative_search_coverage_reopens_diagnosis_but_repeated_reads_do_not() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("brain.sqlite");
        let j = Journal::open(&path).unwrap();
        let revision = j
            .select_goal("collect material", "fixture", json!({}), None)
            .unwrap()
            .to_string();
        j.set("sol_daily_reset", "1793750400").unwrap();
        j.set("model_admissions", "240").unwrap();
        let old = j
            .diagnosis_subject("same source", &revision, "same world")
            .unwrap();
        j.investigation_budget(&old, 1, &revision).unwrap();
        j.investigation_spent(&old, false).unwrap();
        let (choice, mut value) = search(24);
        assert!(
            j.search_discovery(&choice, &value, &revision, "instance", "dimension")
                .unwrap()
        );
        let changed = j
            .diagnosis_subject("same source", &revision, "same world")
            .unwrap();
        assert_ne!(old, changed);
        assert_eq!(
            j.investigation_budget(&changed, 1, &revision).unwrap(),
            Some((crate::progress::Investigation::Start, 1))
        );
        j.investigation_spent(&changed, false).unwrap();
        drop(j);
        let j = Journal::open(&path).unwrap();
        value["result"]["revision"] = json!("new clock");
        value["input_sha256"] = json!("new nonce");
        assert!(
            !j.search_discovery(&choice, &value, &revision, "instance", "dimension")
                .unwrap()
        );
        assert_eq!(
            j.diagnosis_subject("same source", &revision, "same world")
                .unwrap(),
            changed
        );
        assert_eq!(
            j.investigation_budget(&changed, 8, &revision).unwrap(),
            Some((crate::progress::Investigation::Exhausted, 0))
        );
        let (wider, result) = search(32);
        assert!(
            j.search_discovery(&wider, &result, &revision, "instance", "dimension")
                .unwrap()
        );
        let expanded = j
            .diagnosis_subject("same source", &revision, "same world")
            .unwrap();
        assert_ne!(expanded, changed);
        assert!(
            !j.search_discovery(&choice, &value, &revision, "instance", "dimension")
                .unwrap()
        );
        assert_eq!(
            j.diagnosis_subject("same source", &revision, "same world")
                .unwrap(),
            expanded
        );
        let mut nonce_choice = choice.clone();
        let mut input: Value = serde_json::from_str(&nonce_choice.input).unwrap();
        input["input"]["nonce"] = json!("arbitrary new text");
        nonce_choice.input = input.to_string();
        assert!(
            !j.search_discovery(&nonce_choice, &value, &revision, "instance", "dimension")
                .unwrap()
        );
        assert_eq!(
            j.get("sol_daily_reset").unwrap().as_deref(),
            Some("1793750400")
        );
        assert_eq!(j.get("model_admissions").unwrap().as_deref(), Some("240"));
        let mut matched = value.clone();
        matched["result"]["matches"] = json!([{"position":[33,104,-15],"block":"minecraft:oak_log","item":"minecraft:oak_log","visible":true,"may_interact":true}]);
        assert!(
            j.search_discovery(&choice, &matched, &revision, "instance", "dimension")
                .unwrap()
        );
    }
    #[test]
    fn pacing_errors_and_superseded_goals_are_not_new_discoveries() {
        let dir = tempfile::tempdir().unwrap();
        let j = Journal::open(&dir.path().join("brain.sqlite")).unwrap();
        let revision = j
            .select_goal("collect material", "fixture", json!({}), None)
            .unwrap()
            .to_string();
        let (choice, mut value) = search(24);
        value["result"]["checked_blocks"] = json!(0);
        value["result"]["complete"] = json!(false);
        value["result"]["next_offset"] = json!(0);
        assert!(
            !j.search_discovery(&choice, &value, &revision, "instance", "dimension")
                .unwrap()
        );
        assert!(
            !j.search_discovery(
                &choice,
                &json!({"error":"unavailable"}),
                &revision,
                "instance",
                "dimension"
            )
            .unwrap()
        );
        j.select_goal("find food", "fixture", json!({}), None)
            .unwrap();
        let (choice, value) = search(32);
        assert!(
            !j.search_discovery(&choice, &value, &revision, "instance", "dimension")
                .unwrap()
        );
        assert!(j.get("discovery_revision").unwrap().is_none());
    }
}
