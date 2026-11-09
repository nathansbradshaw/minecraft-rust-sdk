//! Diagnosis scope for direct SDK failures, independent of older program runs.
//! These local cost hints never certify effects or authorize mutation replay.
use crate::{Journal, Result, digest};
use minecraft_sdk::{Action, Observation, Outcome, Receipt};
use serde_json::{Value, json};

fn intention(action: &Action) -> Result<Value> {
    let mut value = serde_json::to_value(action)?;
    if let Some(object) = value.as_object_mut() {
        // Receipt/request IDs are never included. A cancellation's action_id
        // identifies its target control and must remain distinct. Menu revisions
        // and aiming jitter alone do not establish a new failed boundary.
        object.remove("state_id");
        object.remove("hit");
        if let Some(target) = object.get_mut("target").and_then(Value::as_object_mut) {
            for axis in ["x", "y", "z"] {
                if let Some(n) = target.get(axis).and_then(Value::as_f64) {
                    target.insert(axis.into(), json!(n.floor()));
                }
            }
        }
    }
    Ok(value)
}

/// Retain the cells actually sampled for this action, even when general history
/// projection removes the large world arrays. Missing cells stay unknown.
fn placement_context(action: &Action, receipt: &Receipt) -> Value {
    let Action::Place { block, face, .. } = action else {
        return Value::Null;
    };
    let clicked = [i64::from(block.x), i64::from(block.y), i64::from(block.z)];
    let mut adjacent = clicked;
    match face {
        0 => adjacent[1] -= 1,
        1 => adjacent[1] += 1,
        2 => adjacent[2] -= 1,
        3 => adjacent[2] += 1,
        4 => adjacent[0] -= 1,
        5 => adjacent[0] += 1,
        _ => return Value::Null,
    }
    let sample = |phase: &str| {
        let observed = &receipt.evidence[phase];
        let cell = |position: [i64; 3]| {
            let row = observed["world"]["blocks"]
                .as_array()
                .and_then(|rows| rows.iter().find(|row| row["position"] == json!(position)));
            match row {
                Some(row) => json!({"position":position,"observed":true,
                    "block":row["block"].as_str().map(|s|s.chars().take(256).collect::<String>()),
                    "state":row["state"].as_str().map(|s|s.chars().take(512).collect::<String>()),
                    "replaceable":row["replaceable"].as_bool(),
                    "may_interact":row["may_interact"].as_bool()}),
                None => json!({"position":position,"observed":false}),
            }
        };
        json!({"game_time":observed["world"]["game_time"].as_i64(),
            "clicked_cell":cell(clicked),"adjacent_candidate":cell(adjacent)})
    };
    json!({"initial_sample":sample("before"),"final_sample":sample("after"),
        "meaning":"Historical receipt samples. Place.block is the clicked cell; face selects an adjacent candidate (0 down, 1 up, 2 north, 3 south, 4 west, 5 east). Check replaceability, held item, player collision and permissions using fresh facts. Some items replace or merge the clicked cell; the server decides. Expiry is not proof of an effect or permission to replay uncertainty."})
}

impl Journal {
    pub(crate) fn note_gameplay_failure(
        &self,
        action: &Action,
        receipt: &Receipt,
        before: &Observation,
    ) -> Result<bool> {
        if !matches!(
            receipt.outcome,
            Outcome::Failed | Outcome::Expired | Outcome::Rejected | Outcome::Uncertain
        ) && receipt.error.is_none()
        {
            return Ok(false);
        }
        let revision = self.get("goal_revision")?.unwrap_or_default();
        let identity = json!({"goal_revision":revision,"action":intention(action)?,
            "outcome":receipt.outcome,"error_code":receipt.error.as_ref().map(|e|&e.code)});
        let key = digest(identity.to_string().as_bytes());
        let changed = self.get("gameplay_failure_revision")?.as_deref() != Some(key.as_str());
        let action_value = serde_json::to_value(action)?;
        let action_bytes = serde_json::to_vec(&action_value)?;
        let request = if action_bytes.len() <= 2048 {
            action_value
        } else {
            json!({"kind":action_value["kind"],"sha256":digest(&action_bytes),"bytes":action_bytes.len(),"omitted":"Large intention retained in action_intent event; inspect it from the private journal"})
        };
        let error = receipt.error.as_ref().map(|e| json!({"code":e.code,
            "message":e.message.chars().take(512).collect::<String>(),"message_complete":e.message.chars().count()<=512}));
        let witness = json!({"goal_revision":revision,"action":request,
            "basis":crate::handoff::basis(before),
            "action_id":receipt.action_id,"outcome":receipt.outcome,
            "error":error,"selected_before":before.player["selected"].as_i64(),
            "placement_context":placement_context(action,receipt),
            "scope":"Historical direct SDK failure, not a current world fact or effect proof. Inspect/reconcile uncertain IDs; do not replay them under a new ID."});
        self.set("gameplay_failure_context", &witness.to_string())?;
        // Refresh the status/reconciliation reference even when this diagnosis
        // stays exhausted. A newer receipt is not a new reasoning allowance.
        if !changed {
            return Ok(false);
        }
        self.set("gameplay_failure_revision", &key)?;
        self.event("gameplay_failure_scoped",json!({"revision":key,"action_id":receipt.action_id,
            "outcome":receipt.outcome,"provider_allowances_changed":false,"mutation_replayed":false}))?;
        Ok(true)
    }
    pub(crate) fn gameplay_failure_context(&self, revision: &str) -> Result<Value> {
        let value: Value = self
            .get("gameplay_failure_context")?
            .map(|s| serde_json::from_str(&s))
            .transpose()?
            .unwrap_or(Value::Null);
        Ok(if value["goal_revision"] == revision {
            value
        } else {
            Value::Null
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progress;
    fn scene() -> Observation {
        Observation {
            revision: "fixture".into(),
            player: json!({"uuid":"fixture","dimension":"minecraft:overworld","position":[0.5,64.,0.5],"health":20.,"food":20,"selected":0,"inventory":[{"slot":7,"item":"fixture:table","count":1}]}),
            world: json!({"game_time":1,"blocks":[]}),
            menu: json!({}),
        }
    }
    fn action(x: i32) -> Action {
        serde_json::from_value(json!({"kind":"place","block":{"x":x,"y":63,"z":0},"face":1,"hand":"main","hit":[0.5,1.,0.5]})).unwrap()
    }
    fn expired(id: &str) -> Receipt {
        Receipt {
            action_id: id.into(),
            outcome: Outcome::Expired,
            adapter_version: "fixture".into(),
            evidence: json!({}),
            error: None,
        }
    }
    #[test]
    fn failed_placement_retains_exact_cells_through_history_projection() {
        let mut receipt = expired("blocked");
        receipt.evidence = json!({"before":{"world":{"game_time":99,"blocks":[
            {"position":[1,63,0],"block":"minecraft:dirt","state":"dirt","replaceable":false,"may_interact":true},
            {"position":[1,64,0],"block":"minecraft:oak_log","state":"log","replaceable":false,"may_interact":true},
            {"position":[2,64,0],"block":"unrelated","replaceable":true}
        ]}}});
        let context = placement_context(&action(1), &receipt);
        let mut projected = context.clone();
        crate::compact_history(&mut projected);
        assert_eq!(projected, context);
        assert_eq!(
            projected["initial_sample"]["adjacent_candidate"]["block"],
            "minecraft:oak_log"
        );
        assert_eq!(
            projected["initial_sample"]["clicked_cell"]["may_interact"],
            true
        );
        assert_eq!(
            projected["final_sample"]["adjacent_candidate"]["observed"],
            false
        );
        assert!(
            projected["final_sample"]["adjacent_candidate"]
                .get("replaceable")
                .is_none()
        );
        assert!(!projected.to_string().contains("unrelated"));
        // Every face uses its ordinary protocol direction, including negatives.
        for (face, expected) in [
            (0, [1, 62, 0]),
            (1, [1, 64, 0]),
            (2, [1, 63, -1]),
            (3, [1, 63, 1]),
            (4, [0, 63, 0]),
            (5, [2, 63, 0]),
        ] {
            let mut request = serde_json::to_value(action(1)).unwrap();
            request["face"] = json!(face);
            let request: Action = serde_json::from_value(request).unwrap();
            assert_eq!(
                placement_context(&request, &receipt)["initial_sample"]["adjacent_candidate"]["position"],
                json!(expected)
            );
        }
    }
    #[test]
    fn unrelated_failed_intention_reopens_once_and_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.sqlite");
        let j = Journal::open(&path).unwrap();
        let revision = j
            .select_goal("build", "fixture", json!({}), None)
            .unwrap()
            .to_string();
        let o = scene();
        let environment = progress::environment_signature(&o);
        j.note_gameplay_failure(&action(1), &expired("first"), &o)
            .unwrap();
        let first = j
            .diagnosis_subject("source", &revision, &environment)
            .unwrap();
        j.investigation_budget(&first, 1, &revision).unwrap();
        j.investigation_spent(&first, false).unwrap();
        assert!(
            !j.note_gameplay_failure(&action(1), &expired("new-id"), &o)
                .unwrap()
        );
        assert_eq!(
            j.diagnosis_subject("source", &revision, &environment)
                .unwrap(),
            first
        );
        j.note_gameplay_failure(&action(2), &expired("other-target"), &o)
            .unwrap();
        let second = j
            .diagnosis_subject("source", &revision, &environment)
            .unwrap();
        assert_ne!(
            second, first,
            "a separate failed intention is not the old program diagnosis"
        );
        assert_eq!(
            j.investigation_budget(&second, 1, &revision).unwrap(),
            Some((progress::Investigation::Start, 1))
        );
        j.investigation_spent(&second, false).unwrap();
        drop(j);
        let j = Journal::open(&path).unwrap();
        assert_eq!(
            j.diagnosis_subject("source", &revision, &environment)
                .unwrap(),
            second
        );
        assert_eq!(
            j.investigation_budget(&second, 1, &revision).unwrap(),
            Some((progress::Investigation::Exhausted, 0))
        );
        j.note_gameplay_failure(&action(1), &expired("again"), &o)
            .unwrap();
        assert_eq!(
            j.investigation_budget(&first, 1, &revision).unwrap(),
            Some((progress::Investigation::Exhausted, 0)),
            "alternating old failures does not refill budgets"
        );
    }
    #[test]
    fn held_item_repair_changes_facts_without_counting_clocks_as_progress() {
        let mut o = scene();
        let initial = progress::environment_signature(&o);
        o.world["game_time"] = json!(1000);
        o.revision = "another receipt".into();
        assert_eq!(progress::environment_signature(&o), initial);
        o.player["selected"] = json!(7);
        assert_ne!(
            progress::environment_signature(&o),
            initial,
            "repairing the held item changes a genuine placement precondition"
        );
    }
    #[test]
    fn verbose_failure_feedback_is_bounded_and_wording_does_not_renew_it() {
        let dir = tempfile::tempdir().unwrap();
        let j = Journal::open(&dir.path().join("journal.sqlite")).unwrap();
        let revision = j
            .select_goal("build", "fixture", json!({}), None)
            .unwrap()
            .to_string();
        let mut receipt = expired("failed");
        receipt.error = Some(minecraft_sdk::ApiError::new(
            minecraft_sdk::ErrorCode::NotReady,
            "診断".repeat(10000),
        ));
        j.note_gameplay_failure(&action(1), &receipt, &scene())
            .unwrap();
        assert!(
            j.gameplay_failure_context(&revision)
                .unwrap()
                .to_string()
                .len()
                < 8192
        );
        receipt.error.as_mut().unwrap().message = "different wording".into();
        assert!(
            !j.note_gameplay_failure(&action(1), &receipt, &scene())
                .unwrap()
        );
    }
    #[test]
    fn revision_ids_and_subcell_waypoint_jitter_do_not_refill_diagnosis() {
        let a: Action = serde_json::from_value(json!({"kind":"start_move","target":{"x":1.1,"y":64.,"z":0.2},"jump":false,"max_ticks":60})).unwrap();
        let b: Action = serde_json::from_value(json!({"kind":"start_move","target":{"x":1.2,"y":64.,"z":0.3},"jump":false,"max_ticks":60})).unwrap();
        assert_eq!(intention(&a).unwrap(), intention(&b).unwrap());
        let first = Action::CancelControl {
            action_id: "control-first".into(),
        };
        let second = Action::CancelControl {
            action_id: "control-second".into(),
        };
        assert_ne!(
            intention(&first).unwrap(),
            intention(&second).unwrap(),
            "target control IDs are semantic identities, not receipt jitter"
        );
        let dir = tempfile::tempdir().unwrap();
        let j = Journal::open(&dir.path().join("journal.sqlite")).unwrap();
        let revision = j
            .select_goal("build", "fixture", json!({}), None)
            .unwrap()
            .to_string();
        j.note_gameplay_failure(&a, &expired("first"), &scene())
            .unwrap();
        assert!(
            !j.note_gameplay_failure(&b, &expired("second"), &scene())
                .unwrap()
        );
        assert!(j.gameplay_failure_context(&revision).unwrap().is_object());
        assert_eq!(
            j.gameplay_failure_context(&revision).unwrap()["action_id"],
            "second"
        );
        assert!(
            j.gameplay_failure_context("another goal")
                .unwrap()
                .is_null()
        );
    }
}
