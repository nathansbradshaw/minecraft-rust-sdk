//! Client task bookkeeping, never server authority or benchmark certification.
use crate::{Config, Journal, Observation, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Claim {
    pub status: Status,
    pub summary: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Status {
    Succeeded,
}
pub(crate) fn claim(input: &str) -> Result<Claim> {
    let claim: Claim = serde_json::from_str(input).map_err(|_| {
        "complete requires JSON {\"status\":\"succeeded\",\"summary\":\"verified result\"}; use report for incomplete progress"
    })?;
    ensure(
        !claim.summary.trim().is_empty() && claim.summary.len() <= 2048,
        "Bounded completion summary required",
    )?;
    Ok(claim)
}
pub(crate) fn validate_config(cfg: &Config) -> Result<()> {
    ensure(
        cfg.completion_inventory.len() <= 8,
        "At most eight configured completion inventory requirements",
    )?;
    for (item, minimum) in &cfg.completion_inventory {
        ensure(
            minecraft_sdk::registry_id(item) && (1..=1_000_000).contains(minimum),
            "Completion requires runtime item IDs and positive bounded counts",
        )?;
    }
    Ok(())
}
pub(crate) fn view(cfg: &Config, goal: &str) -> Value {
    let active = goal == cfg.goal && !cfg.completion_inventory.is_empty();
    json!({"configured_goal":cfg.goal,"applies_to_current_goal":active,"inventory_minimum":if active {json!(cfg.completion_inventory)}else {json!({})},"scope":"Current retained player inventory only; menu output and carried stacks do not count. Optional client checks do not certify external benchmarks."})
}
pub(crate) fn verify(cfg: &Config, goal: &str, observation: &Observation) -> Result<Value> {
    let mut checked = Vec::new();
    if goal == cfg.goal {
        for (item, minimum) in &cfg.completion_inventory {
            let actual = minecraft_sdk::menu::item_count(observation, item)?;
            checked.push(json!({"item":item,"minimum":minimum,"observed":actual}));
        }
    }
    ensure(
        checked
            .iter()
            .all(|r| r["observed"].as_u64() >= r["minimum"].as_u64()),
        &format!(
            "Goal remains incomplete: retained inventory requirements not met: {}. Use report for progress, then continue work.",
            json!(checked)
        ),
    )?;
    Ok(
        json!({"revision":observation.revision,"checked_inventory":checked,"external_benchmark_certified":false}),
    )
}
/// A newly supplied contract cannot inherit an incompatible old completion.
/// Unchanged requirements do not reopen historical success after later losses.
pub(crate) fn bind(
    journal: &Journal,
    cfg: &Config,
    goal: &str,
    observed: &Observation,
) -> Result<()> {
    let contract = crate::digest(
        json!([cfg.goal, cfg.completion_inventory])
            .to_string()
            .as_bytes(),
    );
    if journal.get("completion_inventory_contract")?.as_deref() == Some(&contract) {
        return Ok(());
    }
    if goal == cfg.goal
        && journal.goal_completed(goal)?
        && let Err(error) = verify(cfg, goal, observed)
    {
        journal.select_goal(goal, "completion_contract_reopened", json!({"previous_completion_retained_in_history":true,"reason":error.to_string(),"contract":contract}), journal.get("player_request")?.as_deref())?;
        journal.request_development()?;
    }
    journal.set("completion_inventory_contract", &contract)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completion_needs_explicit_success_and_retained_inventory() {
        assert!(claim(r#"{"kind":"complete","text":"Requirement is not met"}"#).is_err());
        assert!(claim(r#"{"status":"in_progress","summary":"Still working"}"#).is_err());
        let accepted = claim(r#"{"status":"succeeded","summary":"Both retained"}"#).unwrap();
        assert!(matches!(accepted.status, Status::Succeeded));
        let (_dir, mut brain) = crate::admission_tests::fixture("http://127.0.0.1:1/");
        brain
            .cfg
            .completion_inventory
            .insert("fixture:tool".into(), 1);
        brain
            .cfg
            .completion_inventory
            .insert("fixture:workbench".into(), 1);
        validate_config(&brain.cfg).unwrap();
        brain.observed.player["inventory"] = json!([{"item":"fixture:tool","count":1}]);
        brain.observed.menu = json!({"slots":[{"item":"fixture:workbench","count":1}],"carried":{"item":"fixture:workbench","count":1}});
        assert!(verify(&brain.cfg, "fixture", &brain.observed).is_err());
        assert!(verify(&brain.cfg, "another player task", &brain.observed).is_ok());
        let revision = brain
            .journal
            .select_goal("fixture", "fixture", json!({}), None)
            .unwrap()
            .to_string();
        assert!(brain.journal.complete_goal("fixture", &revision).unwrap());
        bind(&brain.journal, &brain.cfg, "fixture", &brain.observed).unwrap();
        assert!(!brain.journal.goal_completed("fixture").unwrap());
        brain.observed.player["inventory"]
            .as_array_mut()
            .unwrap()
            .push(json!({"item":"fixture:workbench","count":1}));
        assert!(verify(&brain.cfg, "fixture", &brain.observed).is_ok());
        let current = brain.journal.get("goal_revision").unwrap().unwrap();
        assert!(brain.journal.complete_goal("fixture", &current).unwrap());
        brain.observed.player["inventory"] = json!([]);
        bind(&brain.journal, &brain.cfg, "fixture", &brain.observed).unwrap();
        assert!(
            brain.journal.goal_completed("fixture").unwrap(),
            "Unchanged requirements preserve historical success"
        );
    }
}
