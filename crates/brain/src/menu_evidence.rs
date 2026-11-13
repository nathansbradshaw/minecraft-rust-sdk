//! Retained menu questions, never fresh observations or native effect authority.
use crate::{Journal, Observation, Result, digest};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
const KEY: &str = "menu_question_evidence_v1";
const MAX_BYTES: usize = 262144;
fn stack_facts(value: &Value) -> Value {
    let mut result = serde_json::Map::new();
    for field in ["slot", "item", "count", "components"] {
        if let Some(value) = value.get(field) {
            result.insert(field.into(), value.clone());
        }
    }
    Value::Object(result)
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Evidence {
    binding: String,
    goal_revision: String,
    first_revision: String,
    rows: BTreeMap<i64, Value>,
    filters: BTreeMap<String, usize>,
    pages: Vec<Value>,
    repeats: u32,
}
fn binding(o: &Observation, revision: &str) -> Option<String> {
    if revision.is_empty()
        || o.player["uuid"].as_str()?.is_empty()
        || o.player["life_id"].as_str()?.is_empty()
        || o.player["dimension"].as_str()?.is_empty()
        || o.menu["menu_id"].as_i64().is_none()
        || o.menu["state_id"].as_i64().is_none()
        || o.menu["slots"].as_array()?.len() > 512
        || !o.player["inventory"].is_array()
    {
        return None;
    }
    let slots = o.menu["slots"].as_array()?;
    let ids: std::collections::BTreeSet<_> =
        slots.iter().filter_map(|r| r["slot"].as_i64()).collect();
    if ids.len() != slots.len() || ids.iter().any(|id| !(0..512).contains(id)) {
        return None;
    }
    let mut slots: Vec<_> = slots
        .iter()
        .map(|row| {
            let mut facts = crate::views::menu_slot(row);
            if let Some(components) = row.get("components") {
                facts["components"] = components.clone();
            }
            facts
        })
        .collect();
    slots.sort_by_key(|row| row["slot"].as_i64());
    let mut inventory: Vec<_> = o.player["inventory"]
        .as_array()?
        .iter()
        .map(stack_facts)
        .collect();
    inventory.sort_by_key(|row| row["slot"].as_i64());
    Some(digest(
        json!({"goal":revision,"actor":o.player["uuid"],"life":o.player["life_id"],
        "dimension":o.player["dimension"],"menu_id":o.menu["menu_id"],"state_id":o.menu["state_id"],
        "slots":slots,"carried":stack_facts(&o.menu["carried"]),"inventory":inventory})
        .to_string()
        .as_bytes(),
    ))
}
fn load(j: &Journal) -> Result<Evidence> {
    let Some(saved) = j.get(KEY)? else {
        return Ok(Evidence::default());
    };
    crate::ensure(
        saved.len() <= MAX_BYTES,
        "Stored menu evidence exceeds bound",
    )?;
    let evidence: Evidence = serde_json::from_str(&saved)?;
    crate::ensure(
        evidence.rows.len() <= 512 && evidence.pages.len() <= 16 && evidence.filters.len() <= 32,
        "Stored menu evidence exceeds record bounds",
    )?;
    Ok(evidence)
}
/// Called only with a successfully validated focused view from this same sample.
/// Unchanged rows under another offset/limit or new notes are not a new question.
/// The ordinary SDK observation was checked; this never claims a fresh native query.
pub(crate) fn record(j: &Journal, o: &Observation, input: &str, mut value: Value) -> Result<Value> {
    if value["view"] != "menu" {
        return Ok(value);
    }
    let revision = j.get("goal_revision")?.unwrap_or_default();
    let Some(key) = binding(o, &revision) else {
        return Ok(value);
    };
    crate::ensure(
        value["revision"] == o.revision
            && value["player"]["uuid"] == o.player["uuid"]
            && value["menu"]["menu_id"] == o.menu["menu_id"]
            && value["menu"]["state_id"] == o.menu["state_id"],
        "Menu evidence sample binding mismatch",
    )?;
    let query: Value = serde_json::from_str(input)?;
    let offset = value["page"]["offset"]
        .as_u64()
        .ok_or("Missing menu page offset")?;
    let total = value["page"]["total_in_snapshot"]
        .as_u64()
        .filter(|n| *n <= 512)
        .ok_or("Missing bounded menu total")? as usize;
    let rows = value["menu"]["slots"]
        .as_array()
        .ok_or("Missing focused menu rows")?;
    let selector = json!({"view":"menu","offset":offset,"limit":query["limit"].as_u64().unwrap_or(16),"match_id":query["match_id"].as_str()});
    let mut evidence = load(j)?;
    if evidence.binding != key || evidence.goal_revision != revision {
        evidence = Evidence {
            binding: key,
            goal_revision: revision,
            first_revision: o.revision.clone(),
            ..Default::default()
        };
    }
    let filter = query["match_id"].as_str().unwrap_or("").to_owned();
    let filter_new = evidence.rows.len() < o.menu["slots"].as_array().unwrap().len()
        && evidence.filters.get(&filter) != Some(&total);
    let mut new_rows = 0;
    for row in rows {
        let slot = row["slot"]
            .as_i64()
            .filter(|n| (0..512).contains(n))
            .ok_or("Invalid menu evidence slot")?;
        if evidence.rows.get(&slot) != Some(row) {
            new_rows += 1;
        }
        evidence.rows.insert(slot, row.clone());
    }
    let repeated = new_rows == 0 && !filter_new;
    if evidence.filters.len() < 32 || evidence.filters.contains_key(&filter) {
        evidence.filters.insert(filter, total);
    }
    if repeated {
        evidence.repeats = evidence.repeats.saturating_add(1);
    }
    let fingerprint = digest(
        json!({"binding":evidence.binding,"selector":selector,"rows":rows,"total":total})
            .to_string()
            .as_bytes(),
    );
    if !evidence
        .pages
        .iter()
        .any(|p| p["fingerprint"] == fingerprint)
    {
        evidence.pages.push(
            json!({"selector":selector,"fingerprint":fingerprint,"sample_revision":o.revision,
            "returned":rows.len(),"complete":value["page"]["complete"]}),
        );
        if evidence.pages.len() > 16 {
            evidence.pages.remove(0);
        }
    }
    let saved = serde_json::to_string(&evidence)?;
    if saved.len() > MAX_BYTES {
        value["menu_question"] = json!({"available":false,"reason":"Bounded retained evidence unavailable; use a small SDK program for this menu"});
        return Ok(value);
    }
    j.set(KEY, &saved)?;
    let witness = json!({"format":"menu-question-v1","binding":evidence.binding,"selector":selector,
        "fingerprint":fingerprint,"new_rows":new_rows,"new_filter_fact":filter_new,"repeated":repeated,
        "checked_sample_revision":o.revision,"first_retained_revision":evidence.first_revision,
        "scope":"Read-only cached SDK sample checked. Retained facts are not a fresh native query, execution, item gain or effect proof."});
    if repeated {
        j.event("menu_question_repeated", witness.clone())?;
        return Ok(
            json!({"view":"retained_menu_evidence","status":"already_answered","menu_question":witness,
            "retained":context_from(&evidence,o,false),"next_action":"This selector adds no menu facts. Use retained slots; ask a different unresolved question, retrieve the existing SDK helper contract, write/run a bounded experiment, act using verified preconditions, or report the precise missing fact. No unsafe action is required."}),
        );
    }
    value["menu_question"] = witness;
    Ok(value)
}
pub(crate) fn repeated(value: &Value) -> bool {
    value["menu_question"]["format"] == "menu-question-v1"
        && value["menu_question"]["repeated"] == true
}
/// No new grants: early duplicates enter existing diagnosis; an alternative
/// window cannot spend eight more calls repeating its already-answered question.
pub(crate) fn inspection_count(
    j: &Journal,
    value: &Value,
    investigating: bool,
    alternative: bool,
    ordinary: u32,
    ceiling: u32,
) -> Result<u32> {
    if !repeated(value) {
        return Ok(ordinary);
    }
    if alternative {
        return Ok(ordinary.max(ceiling));
    }
    if !investigating {
        j.set("needs_investigation", "true")?;
    }
    Ok(ordinary)
}
fn context_from(e: &Evidence, o: &Observation, recovery: bool) -> Value {
    let mut slots = Vec::new();
    let mut empty = 0;
    for row in e.rows.values() {
        if row["player_inventory"] == true && row["count"] == 0 {
            if empty >= 4 {
                continue;
            }
            empty += 1;
        }
        if slots.len() >= 32 {
            break;
        }
        slots.push(row.clone());
    }
    let reference = crate::workspace::reference_index();
    let helper = reference["sections"].as_array().and_then(|s| {
        s.iter().find(|s| {
            s["title"]
                .as_str()
                .is_some_and(|s| s.contains("Generic menu builders"))
        })
    });
    let mut view = json!({"format":"retained-menu-questions-v1","binding":e.binding,"goal_revision":e.goal_revision,
        "current_cached_sample_revision":o.revision,"first_retained_revision":e.first_revision,
        "actor":o.player["uuid"],"life":o.player["life_id"],"menu_id":o.menu["menu_id"],"state_id":o.menu["state_id"],
        "carried":o.menu["carried"],"covered_slots":e.rows.len(),"total_slots":o.menu["slots"].as_array().map(Vec::len),
        "known_slots":slots,"answered_pages":e.pages.iter().rev().take(4).collect::<Vec<_>>(),"repeated_questions":e.repeats,
        "changed_question_required":recovery || e.repeats>0,"omitted_known_slots":0,
        "constraint":"Previously answered menu rows remain available here. Revision clocks, reworded notes, and offset/limit jitter do not create new facts. Repeating covered rows returns local no-new-facts feedback; choose a genuinely unresolved question or changed experiment. Other perception views and changed menu/inventory remain available. Never infer freshness or native success from this cache.",
        "client_composition":{"helper":"menu::craft_once","reference_path":"sdk://reference","section":helper,
            "source_sha256":reference["source_sha256"],"scope":"Optional finite client helper: prepare one runtime recipe, collect/store output and verify retained gain using native evidence. Requires its documented menu/cursor/life/journal preconditions. A Rust program can compose controls without one model turn per click; it must inspect actual outcomes. No task strategy or automatic invocation supplied."}});
    while serde_json::to_vec(&view).map_or(true, |b| b.len() > 4096) {
        if view["known_slots"].as_array_mut().unwrap().pop().is_some() {
            continue;
        }
        if view["answered_pages"]
            .as_array_mut()
            .unwrap()
            .pop()
            .is_none()
        {
            break;
        }
    }
    view["omitted_known_slots"] = json!(
        e.rows
            .len()
            .saturating_sub(view["known_slots"].as_array().unwrap().len())
    );
    if serde_json::to_vec(&view).map_or(true, |b| b.len() > 4096) {
        return json!({"available":false,"scope":"Retained menu metadata exceeds its bounded view; use an SDK program to inspect the actual menu. No freshness or gameplay result inferred."});
    }
    view
}
pub(crate) fn context(
    j: &Journal,
    o: &Observation,
    revision: &str,
    recovery: bool,
) -> Result<Value> {
    let Some(key) = binding(o, revision) else {
        return Ok(Value::Null);
    };
    let evidence = load(j)?;
    Ok(
        if evidence.binding == key && evidence.goal_revision == revision {
            context_from(&evidence, o, recovery)
        } else {
            Value::Null
        },
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Journal, Observation) {
        let d = tempfile::tempdir().unwrap();
        let j = Journal::open(&d.path().join("brain.sqlite")).unwrap();
        j.activate_configured_goal("test menu", false).unwrap();
        let rows:Vec<_>=(0..46).map(|slot|json!({"slot":slot,"inventory_slot":slot,"item":if slot==0{"minecraft:birch_button"}else if slot==1{"minecraft:birch_planks"}else{"minecraft:air"},
            "count":if slot<2{1}else{0},"player_inventory":slot>=10,"may_pickup":true,"may_place_carried":slot!=0})).collect();
        let o = Observation {
            revision: "sample1".into(),
            player: json!({"uuid":"actor","life_id":"life1","dimension":"minecraft:overworld","inventory":[]}),
            world: json!({"game_time":1}),
            menu: json!({"menu_id":1,"state_id":10,"slots":rows,"carried":{"item":"minecraft:birch_planks","count":3}}),
        };
        (d, j, o)
    }
    fn read(j: &Journal, o: &Observation, input: &str) -> Value {
        record(j, o, input, crate::views::focused(o, input).unwrap()).unwrap()
    }
    const A: &str = r#"{"view":"menu","offset":0,"limit":32}"#;
    const B: &str = r#"{"view":"menu","offset":32,"limit":14}"#;
    #[test]
    fn canonical_pages_retain_roles_across_paging_and_do_not_reward_clock_or_selector_jitter() {
        let (_d, j, mut o) = fixture();
        assert!(!repeated(&read(&j, &o, A)));
        assert!(!repeated(&read(&j, &o, B)));
        o.revision = "new-clock".into();
        o.world["game_time"] = json!(999);
        assert!(repeated(&read(&j, &o, A)));
        assert!(repeated(&read(&j, &o, r#"{"view":"menu","limit":16}"#)));
        let c = context(&j, &o, &j.get("goal_revision").unwrap().unwrap(), true).unwrap();
        assert_eq!(c["covered_slots"], 46);
        assert_eq!(c["state_id"], 10);
        assert_eq!(c["changed_question_required"], true);
        assert!(
            c["known_slots"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["slot"] == 1 && r["player_inventory"] == false)
        );
        assert!(
            c["known_slots"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["slot"] == 10 && r["player_inventory"] == true)
        );
        assert_eq!(c["client_composition"]["reference_path"], "sdk://reference");
        assert!(serde_json::to_vec(&c).unwrap().len() < 4300);
    }
    #[test]
    fn material_menu_actor_life_inventory_changes_and_other_perception_stay_available() {
        for (pointer, value) in [
            ("/player/uuid", json!("other")),
            ("/player/life_id", json!("new-life")),
            ("/menu/menu_id", json!(2)),
            ("/menu/state_id", json!(11)),
            ("/menu/carried/count", json!(2)),
            (
                "/player/inventory",
                json!([{"slot":1,"item":"minecraft:stick","count":4}]),
            ),
            ("/menu/slots/1/count", json!(2)),
        ] {
            let (_d, j, o) = fixture();
            read(&j, &o, A);
            assert!(repeated(&read(&j, &o, A)));
            let mut v = serde_json::to_value(o).unwrap();
            *v.pointer_mut(pointer).unwrap() = value;
            let changed: Observation = serde_json::from_value(v).unwrap();
            assert!(!repeated(&read(&j, &changed, A)), "{pointer}");
        }
        let (_d, j, o) = fixture();
        read(&j, &o, A);
        for view in ["inventory", "entities", "blocks"] {
            assert!(!repeated(&read(&j, &o, &json!({"view":view}).to_string())));
        }
    }
    #[test]
    fn restart_retains_answers_without_execution_credit_or_replacing_the_experiment() {
        let (d, j, o) = fixture();
        j.set("last_experiment", "retained failure").unwrap();
        read(&j, &o, A);
        drop(j);
        let j = Journal::open(&d.path().join("brain.sqlite")).unwrap();
        let value = read(&j, &o, A);
        assert!(repeated(&value));
        assert!(!crate::tool_failed(&value));
        assert!(!crate::progress::executed(
            "observe", &value, "same", "same"
        ));
        assert_eq!(
            j.get("last_experiment").unwrap().as_deref(),
            Some("retained failure")
        );
        assert_eq!(inspection_count(&j, &value, false, false, 3, 8).unwrap(), 3);
        assert_eq!(
            j.get("needs_investigation").unwrap().as_deref(),
            Some("true")
        );
        assert_eq!(
            inspection_count(&j, &value, false, true, 1, 8).unwrap(),
            8,
            "duplicate cannot consume eight more alternative reads"
        );
        j.activate_configured_goal("different question", false)
            .unwrap();
        assert!(!repeated(&read(&j, &o, A)));
    }
    #[test]
    fn unchanged_e_menu_sequence_uses_existing_diagnosis_then_one_alternative_not_eight() {
        let (_d, j, mut o) = fixture();
        read(&j, &o, A);
        read(&j, &o, B);
        let value = read(&j, &o, A);
        inspection_count(&j, &value, false, false, 3, 8).unwrap();
        assert_eq!(
            j.get("needs_investigation").unwrap().as_deref(),
            Some("true")
        );
        let revision = j.get("goal_revision").unwrap().unwrap();
        assert_eq!(
            j.investigation_budget("same-subject", 4, &revision)
                .unwrap(),
            Some((crate::progress::Investigation::Start, 4))
        );
        // Use the real durable finite budget, then its existing alternative
        // window. No provider call or new grant is created by menu evidence.
        for n in 0..4 {
            o.revision = format!("ambient-{n}");
            let v = read(&j, &o, if n % 2 == 0 { A } else { B });
            assert!(repeated(&v));
            assert_eq!(
                inspection_count(&j, &v, true, false, n + 4, 8).unwrap(),
                n + 4
            );
            j.investigation_spent("same-subject", false).unwrap();
        }
        assert_eq!(
            j.investigation_budget("same-subject", 4, &revision)
                .unwrap(),
            Some((crate::progress::Investigation::Exhausted, 0))
        );
        assert_eq!(
            j.alternative_window("same-subject", &revision, "test menu", &o, 8, 8)
                .unwrap(),
            Some(true)
        );
        let v = read(&j, &o, B);
        let inspected = inspection_count(&j, &v, false, true, 1, 8).unwrap();
        assert_eq!(inspected, 8);
        assert_eq!(
            j.alternative_window("same-subject", &revision, "test menu", &o, inspected, 8)
                .unwrap(),
            None
        );
        assert_eq!(
            j.get("investigation_remaining").unwrap().as_deref(),
            Some("0")
        );
        assert_eq!(
            j.get("development_turns_remaining").unwrap().as_deref(),
            Some("0")
        );
        assert_eq!(j.get("pending_action").unwrap(), None);
    }
    #[test]
    fn semantic_rows_ignore_annotation_and_order_jitter_but_not_components_or_permissions() {
        let (_d, j, mut o) = fixture();
        read(&j, &o, A);
        read(&j, &o, B);
        o.menu["slots"].as_array_mut().unwrap().reverse();
        o.menu["slots"][0]["snapshot_note"] = json!("new clock annotation");
        o.menu["carried"]["sample_time"] = json!(99);
        assert!(repeated(&read(&j, &o, A)));
        o.menu["slots"][0]["may_pickup"] = json!(false);
        assert!(!repeated(&read(&j, &o, A)));
        o.menu["carried"]["components"] = json!({"custom:variant":1});
        assert!(!repeated(&read(&j, &o, A)));
    }
    #[test]
    fn retained_roles_and_changed_question_survive_normal_context_fitting() {
        let (_d, j, o) = fixture();
        read(&j, &o, A);
        read(&j, &o, B);
        let revision = j.get("goal_revision").unwrap().unwrap();
        let menu = context(&j, &o, &revision, true).unwrap();
        assert!(serde_json::to_vec(&menu).unwrap().len() <= 4096);
        let mut ctx = json!({"goal":"a useful task","menu_inspection":menu,
            "sdk_reference":include_str!("sdk.md"),"recent":["old history".repeat(4000)],
            "last_experiment":{"result":{"exit_code":0,"output_files":{"stdout":{"path":"retained-output.txt","sha256":"original"}}}}});
        crate::fit_context(&mut ctx).unwrap();
        assert!(serde_json::to_vec(&ctx).unwrap().len() <= 32768);
        assert_eq!(ctx["menu_inspection"]["changed_question_required"], true);
        assert_eq!(ctx["menu_inspection"]["state_id"], 10);
        assert!(
            ctx["menu_inspection"]["known_slots"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["slot"] == 1 && s["player_inventory"] == false)
        );
        assert_eq!(
            ctx["last_experiment"]["result"]["output_files"]["stdout"]["sha256"],
            "original"
        );
    }
    #[test]
    fn foreign_focused_result_and_missing_life_do_not_create_retained_authority() {
        let (_d, j, mut o) = fixture();
        let mut v = crate::views::focused(&o, A).unwrap();
        v["player"]["uuid"] = json!("foreign");
        assert!(record(&j, &o, A, v).is_err());
        assert_eq!(j.get(KEY).unwrap(), None);
        o.player.as_object_mut().unwrap().remove("life_id");
        assert!(!repeated(&read(&j, &o, A)));
        assert_eq!(j.get(KEY).unwrap(), None);
    }
}
