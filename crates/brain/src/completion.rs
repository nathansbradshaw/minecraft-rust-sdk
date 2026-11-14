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
struct Scope {
    active: bool,
    superseded: bool,
    task: Option<String>,
    bound: Option<String>,
}
fn scope(journal: &Journal, cfg: &Config, goal: &str) -> Result<Scope> {
    // One SQLite snapshot: conversation changes task/goal together transactionally.
    let (current_goal,task,bound,configured):(Option<String>,Option<String>,Option<String>,Option<String>)=journal.db.query_row(
        "SELECT (SELECT value FROM meta WHERE key='goal'),(SELECT value FROM meta WHERE key='task_revision'),(SELECT value FROM meta WHERE key='completion_task_revision'),(SELECT value FROM meta WHERE key='completion_inventory_contract')",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    let superseded = current_goal.as_deref().is_some_and(|g| g != goal);
    if !cfg.completion_inventory.is_empty() {
        ensure(
            configured.as_deref() == Some(&contract(cfg)),
            "Completion contract has not been bound",
        )?;
        ensure(
            task.is_some() && bound.is_some(),
            "Completion task lineage unavailable",
        )?;
    }
    let active = !cfg.completion_inventory.is_empty()
        && bound
            .as_ref()
            .is_some_and(|b| !b.is_empty() && Some(b) == task.as_ref());
    Ok(Scope {
        active,
        superseded,
        task,
        bound,
    })
}
pub(crate) fn view(journal: &Journal, cfg: &Config, goal: &str) -> Result<Value> {
    let scope = scope(journal, cfg, goal)?;
    let active = scope.active && !scope.superseded;
    Ok(
        json!({"configured_goal":cfg.goal,"applies_to_current_goal":active,
        "task_revision":scope.task,"bound_task_revision":scope.bound,"superseded":scope.superseded,
        "inventory_minimum":if active {json!(cfg.completion_inventory)}else {json!({})},
        "contract":contract(cfg),"evaluation_phase":"final_success_claim_only",
        "intermediate_steps":"These are terminal postconditions, not prerequisites for each action. Temporary placement/use of a reusable resource, recipe inputs in a menu, or a cursor-held result can be legitimate incomplete progress. Preserve their dependencies; they still cannot satisfy the final retained-inventory check.",
        "scope":"Current retained player inventory only for the success claim; menu output, placed resources and carried stacks do not count. Plans are model hypotheses, never completion evidence. Optional client checks do not certify external benchmarks."}),
    )
}
pub(crate) fn contract(cfg: &Config) -> String {
    crate::digest(
        json!([cfg.goal, cfg.completion_inventory])
            .to_string()
            .as_bytes(),
    )
}
pub(crate) fn verify(
    journal: &Journal,
    cfg: &Config,
    goal: &str,
    observation: &Observation,
) -> Result<Value> {
    let mut checked = Vec::new();
    let scope = scope(journal, cfg, goal)?;
    ensure(
        !scope.superseded,
        "Gameplay goal changed before completion inspection",
    )?;
    if scope.active {
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
    let contract = contract(cfg);
    if journal.get("completion_inventory_contract")?.as_deref() == Some(&contract)
        && journal.get("completion_task_revision")?.is_some()
    {
        return Ok(());
    }
    // Initialize legacy lineage once from recorded exact facts. Never infer
    // that an old paraphrase was a refinement or reactivate by later wording.
    let tx = rusqlite::Transaction::new_unchecked(
        &journal.db,
        rusqlite::TransactionBehavior::Immediate,
    )?;
    let get = |key: &str| -> rusqlite::Result<Option<String>> {
        use rusqlite::OptionalExtension;
        tx.query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
            .optional()
    };
    let current_goal = get("goal")?.unwrap_or_else(|| goal.to_owned());
    let task = get("task_revision")?
        .or(get("goal_revision")?)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "legacy-unrevisioned".into());
    let origin = get("task_origin_goal")?.unwrap_or(current_goal);
    let prior_bound = get("completion_task_revision")?;
    let changed_contract = get("completion_inventory_contract")?.as_deref() != Some(&contract);
    let bound = if origin == cfg.goal
        && (changed_contract || prior_bound.as_deref().is_none_or(|old| old == task))
    {
        task.as_str()
    } else {
        ""
    };
    for (key, value) in [
        ("task_revision", task.as_str()),
        ("task_origin_goal", origin.as_str()),
        ("completion_task_revision", bound),
        ("completion_inventory_contract", contract.as_str()),
    ] {
        tx.execute("INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",rusqlite::params![key,value])?;
    }
    tx.commit()?;
    if !bound.is_empty()
        && journal.goal_completed(goal)?
        && let Err(error) = verify(journal, cfg, goal, observed)
    {
        journal.select_goal_checked(goal, "completion_contract_reopened", json!({"previous_completion_retained_in_history":true,"reason":error.to_string(),"contract":contract}), journal.get("player_request")?.as_deref(),None,true)?;
        journal.request_development()?;
    }
    Ok(())
}

#[cfg(test)]
mod task_scope_tests;

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
        let revision = brain
            .journal
            .select_goal("fixture", "fixture", json!({}), None)
            .unwrap()
            .to_string();
        assert!(brain.journal.complete_goal("fixture", &revision).unwrap());
        bind(&brain.journal, &brain.cfg, "fixture", &brain.observed).unwrap();
        assert!(verify(&brain.journal, &brain.cfg, "fixture", &brain.observed).is_err());
        assert!(!brain.journal.goal_completed("fixture").unwrap());
        brain.observed.player["inventory"]
            .as_array_mut()
            .unwrap()
            .push(json!({"item":"fixture:workbench","count":1}));
        assert!(verify(&brain.journal, &brain.cfg, "fixture", &brain.observed).is_ok());
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
