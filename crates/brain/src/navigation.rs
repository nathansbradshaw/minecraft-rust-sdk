//! Container-local composition of the existing SDK navigator. No new authority.
use crate::{Brain, Journal, Result, digest, ensure, private_read, private_write};
use minecraft_sdk::{
    Client, Profile,
    navigation::{Goal, Options, RetainedInventory},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u32,
    profile: PathBuf,
    goal: Goal,
    journal: PathBuf,
    result: PathBuf,
    options: Options,
}
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum ToolGoal {
    Acquisition(AcquisitionGoal),
    Position(Goal),
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AcquisitionGoal {
    goal: Goal,
    until_inventory: RetainedInventory,
}
impl ToolGoal {
    pub(crate) fn validate(&self) -> Result<()> {
        match self {
            Self::Position(goal) => goal.validate()?,
            Self::Acquisition(request) => {
                request.goal.validate()?;
                request.until_inventory.validate()?;
            }
        }
        Ok(())
    }
}
fn parse_goal(input: &str) -> Result<(Goal, Option<RetainedInventory>)> {
    let request = serde_json::from_str::<ToolGoal>(input)?;
    request.validate()?;
    let result = match request {
        ToolGoal::Position(goal) => (goal, None),
        ToolGoal::Acquisition(goal) => (goal.goal, Some(goal.until_inventory)),
    };
    Ok(result)
}
pub async fn worker(path: &Path) -> Result<()> {
    ensure(
        std::env::var_os("MC_SDK_ARBITER").is_some(),
        "Navigation worker requires the container body arbiter",
    )?;
    let request: Request = serde_json::from_slice(&private_read(path, 16384)?)?;
    ensure(
        request.version == 1,
        "Unsupported navigation worker version",
    )?;
    let profile: Profile = serde_json::from_slice(&private_read(&request.profile, 16384)?)?;
    let client = Client::with_profile(&profile)?;
    // The already-connected profile and local arbiter are inherited. Never
    // connect/disconnect or bypass the body's mutation/preemption boundary.
    let result = match minecraft_sdk::navigation::navigate(
        &client,
        request.goal,
        &request.journal,
        request.options,
    )
    .await
    {
        Ok(report) => json!({"report":report}),
        Err(error) => json!({"error":error}),
    };
    private_write(&request.result, &serde_json::to_vec(&result)?)?;
    println!("{result}");
    Ok(())
}
fn quoted(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}
fn navigation_blocked_blocks() -> Vec<String> {
    // Native fluid-aware predictions can traverse a surface or leave a wet
    // boundary. Urgent drowning recovery remains owned by the reactive body.
    crate::body::geometry::KNOWN_HAZARDS
        .iter()
        .filter(|id| **id != "minecraft:water")
        .map(|id| (*id).to_owned())
        .collect()
}
fn journal_key(instance: &str, actor: &Value, goal: &Goal) -> String {
    digest(
        json!([instance, actor, goal, navigation_blocked_blocks()])
            .to_string()
            .as_bytes(),
    )
}
fn acquisition_key(
    instance: &str,
    actor: &Value,
    goal: &Goal,
    condition: Option<&RetainedInventory>,
) -> String {
    let original = journal_key(instance, actor, goal);
    match condition {
        None => original,
        Some(condition) => digest(
            json!([original, "until-inventory-v1", condition])
                .to_string()
                .as_bytes(),
        ),
    }
}
fn classify_slice(result: &mut Value, goal: &Goal, condition: Option<&RetainedInventory>) {
    let transport_ok =
        result["success"] == true && result["exit_code"] == 0 && result["timed_out"] == false;
    let status = result["navigation"]["report"]["status"]
        .as_str()
        .unwrap_or("error")
        .to_owned();
    let resumable =
        transport_ok && status == "budget_exhausted" && result["navigation"]["error"].is_null();
    let preempted = transport_ok && preemption_report(result);
    let evidence = &result["navigation"]["report"]["inventory_completion"];
    let inventory_satisfied = status == "inventory_satisfied"
        && condition.is_some_and(|condition| {
            evidence["condition"]["item"] == condition.item
                && evidence["condition"]["minimum"] == condition.minimum
                && evidence["observed_count"]
                    .as_u64()
                    .is_some_and(|n| n >= condition.minimum)
                && evidence["condition_satisfied"] == true
                && evidence["owned_input_settled"] == true
                && ["observation_revision", "actor", "life"]
                    .iter()
                    .all(|key| evidence[*key].as_str().is_some_and(|s| !s.is_empty()))
                && result["navigation"]["report"]
                    .get("pending_action")
                    .is_some_and(Value::is_null)
        });
    result["success"] = json!(
        transport_ok && ((condition.is_none() && status == "reached") || inventory_satisfied)
    );
    result["navigation_slice"] = json!({"format":"navigation-slice-v1",
        "completed":result["success"],"can_resume":resumable||preempted,"goal":goal,
        "until_inventory":condition,"completion_kind":if inventory_satisfied{Some("retained_inventory")}else if condition.is_none()&&status=="reached"{Some("position")}else{None},
        "paused_for_control":preempted,
        "resume_condition":if preempted{"Wait for changed, ready body/control facts; writing code and conversation remain available"}else{"Reobserve and continue the bounded slice"},
        "resume_tool":"navigate","scope":"Local bounded helper outcome, not proof of task completion. A normal slice deadline can resume the same goal/journal; pending IDs are inspected rather than replayed. Investigate unchanged slices without native movement or new read work; do not poll indefinitely."});
}
fn preemption_report(result: &Value) -> bool {
    result["exit_code"] == 0
        && result["timed_out"] == false
        && result["navigation"]["report"]["status"] == "preempted"
        && result["navigation"]["report"]
            .get("pending_action")
            .is_some_and(Value::is_null)
        && result["navigation"]["error"].is_null()
        && result["error"].is_null()
        && result["output_files"]["error"].is_null()
        && result["local_action_feedback"]["error"].is_null()
        && result["local_action_feedback"]["requests"]
            .as_array()
            .into_iter()
            .flatten()
            .all(|row| {
                let error = &row["response"]["error"];
                (error.is_null() || error["code"] == "local_preempted")
                    && row["status_refresh_error"].is_null()
                    && !matches!(
                        row["response"]["outcome"].as_str(),
                        Some("rejected" | "uncertain" | "failed" | "expired")
                    )
            })
}
pub(crate) fn scheduling_preempted(result: &Value) -> bool {
    result["navigation_slice"]["format"] == "navigation-slice-v1"
        && result["navigation_slice"]["paused_for_control"] == true
        && preemption_report(result)
}
fn control_basis(observation: &minecraft_sdk::Observation, body: &Value) -> Value {
    let p = &observation.player;
    json!({"actor":p["uuid"],"entity":p["entity_id"],"dimension":p["dimension"],
        "epoch":observation.world["sample_clock"]["epoch"],"body_running":body["running"],
        "scheduling_revision":body["scheduling_revision"],
        "emergency":body["emergency"],"control":{"action_id":p["control"]["action_id"],
        "outcome":p["control"]["outcome"],"input_released":p["control"]["input_released"]}})
}
fn control_ready(observation: &minecraft_sdk::Observation, body: &Value) -> bool {
    let control = &observation.player["control"];
    body["running"] == true
        && body["emergency"] == false
        && (control.is_null()
            || matches!(
                control["outcome"].as_str(),
                Some("observed" | "cancelled" | "failed" | "rejected" | "expired" | "idle")
            ))
        && control["input_released"] != false
}
pub(crate) fn resume_context(
    journal: &Journal,
    observation: &minecraft_sdk::Observation,
    body: &Value,
) -> Result<Value> {
    let Some(saved) = journal.get("navigation_pause")? else {
        return Ok(Value::Null);
    };
    let saved: Value = serde_json::from_str(&saved)?;
    let revision = journal.get("goal_revision")?.filter(|r| !r.is_empty());
    if revision.is_none() || saved["goal_revision"].as_str() != revision.as_deref() {
        return Ok(Value::Null);
    }
    let changed = saved["control_basis"] != control_basis(observation, body);
    let ready = changed && control_ready(observation, body);
    Ok(
        json!({"paused":!ready,"ready":ready,"control_facts_changed":changed,
        "scope":"Expected navigation scheduling pause; no success, failed-program diagnosis or new allowance. Resume navigation only after changed, ready body/control facts. Other code, observations and chat remain available."}),
    )
}
fn guard_paused_choice(choice: &mut crate::model::Choice, resume: &Value) -> bool {
    if choice.tool != "navigate" || resume["paused"] != true {
        return false;
    }
    // This is an explicit local scheduling report, not a model success claim
    // or a failed gameplay experiment. Normal inspection/diagnosis accounting
    // still bounds a nonconforming worker that repeatedly selects this tool.
    choice.tool = "report".into();
    choice.input = "Local scheduler withheld an unavailable navigation attempt; no SDK mutation was submitted. Navigation requires changed, ready body/control facts. Choose another observation, code, terminal, memory or goal operation while protection continues.".into();
    choice.path.clear();
    choice.summary = "Local scheduling report: navigation remains paused".into();
    choice.experiment = false;
    true
}
pub(crate) fn defer_if_paused(
    brain: &mut Brain,
    choice: &mut crate::model::Choice,
) -> Result<bool> {
    if choice.tool != "navigate" || brain.body.is_none() {
        return Ok(false);
    }
    let body = brain
        .body
        .as_ref()
        .map(|h| h.shared.view())
        .unwrap_or(Value::Null);
    let resume = resume_context(&brain.journal, &brain.observed, &body)?;
    if resume["paused"] != true {
        return Ok(false);
    }
    let revision = brain.journal.get("goal_revision")?;
    brain.journal.event("navigation_resume_deferred",json!({"goal_revision":revision,
        "requested_tool":"navigate","scheduled_tool":"report",
        "execution":"No SDK mutation. Main reasoning continues with navigation excluded until changed ready control facts; coding and chat remain available."}))?;
    Ok(guard_paused_choice(choice, &resume))
}
pub(crate) fn resumable_slice(result: &Value) -> bool {
    result["navigation_slice"]["format"] == "navigation-slice-v1"
        && result["navigation_slice"]["can_resume"] == true
        && result["navigation_slice"]["completed"] == false
        && result["navigation"]["report"]["status"] == "budget_exhausted"
        && result["navigation"]["error"].is_null()
        && result["exit_code"] == 0
        && result["timed_out"] == false
}
pub async fn execute(brain: &mut Brain, input: &str) -> Result<Value> {
    ensure(
        brain.body.is_some(),
        "Navigation tool requires the responsive body",
    )?;
    ensure(
        input.len() <= 10000,
        "Bounded typed navigation goal required",
    )?;
    let (goal, until_inventory) = parse_goal(input)?;
    let pause_revision = brain.journal.get("goal_revision")?;
    let body = brain
        .body
        .as_ref()
        .map(|h| h.shared.view())
        .unwrap_or(Value::Null);
    let before_control = control_basis(&brain.observed, &body);
    let key = acquisition_key(
        &brain.server.instance_id,
        &brain.observed.player["uuid"],
        &goal,
        until_inventory.as_ref(),
    );
    let sequence = brain.journal.event(
        "navigation_tool_intent",
        json!({"goal":goal,"until_inventory":until_inventory,"journal_key":key,"built_in_helper":true,"generated_skill":false}),
    )?;
    let stem = format!("navigation-{}-{sequence}", brain.run_id);
    let request_file = brain.cfg.state.join(format!("{stem}.request.json"));
    let result_file = brain.cfg.state.join(format!("{stem}.result.json"));
    let request = Request {
        version: 1,
        profile: brain.cfg.profile.clone(),
        goal,
        journal: brain.cfg.state.join(format!("navigation-{key}.json")),
        result: result_file.clone(),
        options: Options {
            timeout_ms: 30000.min(brain.cfg.tool_timeout_secs.saturating_mul(1000)),
            blocked_blocks: navigation_blocked_blocks(),
            eager_progress: true,
            until_inventory,
            ..Options::default()
        },
    };
    private_write(&request_file, &serde_json::to_vec(&request)?)?;
    // A proc executable link retains this running version even if an operator
    // atomically replaces its pathname. This worker runs only in Linux Docker.
    let executable = PathBuf::from(format!("/proc/{}/exe", std::process::id()));
    let command = format!(
        "{} navigation-worker {}",
        quoted(&executable),
        quoted(&request_file)
    );
    let before = brain.observed.player["position"].clone();
    let mut result = crate::tools::terminal(brain, &command).await?;
    let response = private_read(&result_file, 16384)
        .and_then(|bytes| Ok(serde_json::from_slice::<Value>(&bytes)?));
    match response {
        Ok(response) => {
            result["navigation"] = response;
            classify_slice(
                &mut result,
                &request.goal,
                request.options.until_inventory.as_ref(),
            );
            if result["navigation"]["report"]["status"] == "no_known_route" {
                result["recommended_observation"] = json!({"tool":"observe","input":{"view":"terrain"},
                    "reason":"Inspect local support, foot space and headroom. Another distant target does not open an exhausted local graph. Diagnose the observed constraint before choosing a different method; this is not permission or a request to break blocks."});
            }
        }
        Err(error) => {
            result["success"] = json!(false);
            result["navigation_result_error"] = json!(error.to_string());
        }
    }
    result["position_before"] = before;
    result["position_after"] = brain.observed.player["position"].clone();
    let body_after = brain
        .body
        .as_ref()
        .map(|h| h.shared.view())
        .unwrap_or(Value::Null);
    result["navigation_scheduling"] = json!({"before":before_control,
        "after":control_basis(&brain.observed,&body_after)});
    if scheduling_preempted(&result) {
        brain.journal.set(
            "navigation_pause",
            &json!({"goal_revision":pause_revision,
            "control_basis":before_control})
            .to_string(),
        )?;
    }
    result["helper_scope"] = json!(
        "Built-in bounded SDK navigation; report and local feedback are not external authority or proof of resource collection. Persistent journal retains pending action IDs."
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn acquisition_resume_input_and_key_preserve_bare_goal_semantics() {
        let bare = json!({"kind":"position","dimension":"fixture:world","target":[10.5,1.0,2.5],"distance":0.5});
        let (goal, none) = parse_goal(&bare.to_string()).unwrap();
        assert!(none.is_none());
        assert_eq!(
            acquisition_key("instance", &json!("own"), &goal, None),
            journal_key("instance", &json!("own"), &goal)
        );
        let wrapped =
            json!({"goal":bare,"until_inventory":{"item":"fixture:resource","minimum":1}});
        let (same, condition) = parse_goal(&wrapped.to_string()).unwrap();
        assert_eq!(same, goal);
        let key = acquisition_key("instance", &json!("own"), &goal, condition.as_ref());
        assert_ne!(key, journal_key("instance", &json!("own"), &goal));
        for pointer in ["/until_inventory/minimum", "/until_inventory/item"] {
            let mut changed = wrapped.clone();
            *changed.pointer_mut(pointer).unwrap() = if pointer.ends_with("minimum") {
                json!(2)
            } else {
                json!("fixture:other")
            };
            let (_, condition) = parse_goal(&changed.to_string()).unwrap();
            assert_ne!(
                key,
                acquisition_key("instance", &json!("own"), &goal, condition.as_ref())
            );
        }
        let mut bad = wrapped.clone();
        bad["until_inventory"]["minimum"] = json!(0);
        assert!(parse_goal(&bad.to_string()).is_err());
        bad = wrapped;
        bad["actor"] = json!("forged");
        assert!(parse_goal(&bad.to_string()).is_err());
    }
    #[test]
    fn acquisition_resume_completion_is_distinct_from_position_and_movement_effects() {
        let goal = parse_goal(
            r#"{"kind":"position","dimension":"fixture:world","target":[1,1,1],"distance":0.5}"#,
        )
        .unwrap()
        .0;
        let condition = RetainedInventory {
            item: "fixture:resource".into(),
            minimum: 1,
        };
        let successful = json!({"success":true,"exit_code":0,"timed_out":false,"navigation":{"report":{
            "status":"inventory_satisfied","remaining_distance":1.29,"pending_action":null,
            "inventory_completion":{"condition":{"item":"fixture:resource","minimum":1},"observed_count":1,
                "condition_satisfied":true,"owned_input_settled":true,"observation_revision":"native-sample","actor":"own","life":"life1"}}}});
        let mut result = successful.clone();
        classify_slice(&mut result, &goal, Some(&condition));
        assert_eq!(result["success"], true);
        assert_eq!(
            result["navigation_slice"]["completion_kind"],
            "retained_inventory"
        );
        assert_eq!(result["navigation"]["report"]["remaining_distance"], 1.29);
        assert!(
            !crate::progress::executed("navigate", &result, "same-source", "same-source"),
            "condition report cannot invent native movement or gain"
        );
        for (pointer, value) in [
            (
                "/navigation/report/inventory_completion/owned_input_settled",
                json!(false),
            ),
            (
                "/navigation/report/inventory_completion/observed_count",
                json!(0),
            ),
            (
                "/navigation/report/inventory_completion/condition/item",
                json!("fixture:other"),
            ),
            ("/navigation/report/inventory_completion/life", json!("")),
            (
                "/navigation/report/pending_action",
                json!("original-still-pending"),
            ),
        ] {
            let mut bad = successful.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            classify_slice(&mut bad, &goal, Some(&condition));
            assert_eq!(bad["success"], false, "{pointer}");
        }
        result = successful.clone();
        classify_slice(&mut result, &goal, None);
        assert_eq!(
            result["success"], false,
            "bare caller did not ask for an inventory stop"
        );
        result = successful;
        result["navigation"]["report"]["status"] = json!("reached");
        classify_slice(&mut result, &goal, Some(&condition));
        assert_eq!(
            result["success"], false,
            "arrival cannot satisfy an opt-in resource condition"
        );
    }
    #[test]
    fn unavailable_navigation_becomes_a_nonexecuting_report_without_blocking_other_work() {
        let requested = crate::model::Choice {
            tool: "navigate".into(),
            input:
                r#"{"kind":"position","dimension":"fixture:world","target":[1,2,3],"distance":1}"#
                    .into(),
            path: String::new(),
            summary: "try navigation again".into(),
            notes: String::new(),
            experiment: false,
        };
        let paused = json!({"paused":true,"ready":false});
        let mut withheld = requested.clone();
        assert!(guard_paused_choice(&mut withheld, &paused));
        assert_eq!(withheld.tool, "report");
        assert!(!withheld.experiment);
        assert!(!crate::progress::executed(
            &withheld.tool,
            &json!({"recorded":"progress report"}),
            "same",
            "same"
        ));
        assert_eq!(
            crate::progress::next_inspection_count(0, true, false),
            1,
            "A nonconforming worker does not get unlimited unaccounted navigation polls"
        );
        for tool in [
            "write",
            "edit",
            "run",
            "terminal",
            "observe",
            "query",
            "sdk",
            "status",
            "reconcile",
            "goal",
        ] {
            let mut alternative = requested.clone();
            alternative.tool = tool.into();
            let before = serde_json::to_value(&alternative).unwrap();
            assert!(!guard_paused_choice(&mut alternative, &paused));
            assert_eq!(serde_json::to_value(alternative).unwrap(), before, "{tool}");
        }
        let mut ready = requested.clone();
        assert!(!guard_paused_choice(
            &mut ready,
            &json!({"paused":false,"ready":true})
        ));
        assert_eq!(ready.tool, "navigate");
    }
    fn preempted_result() -> Value {
        let goal: Goal = serde_json::from_value(json!({"kind":"position",
            "dimension":"fixture:world","target":[1.,64.,2.],"distance":1.}))
        .unwrap();
        let mut result = json!({"success":true,"exit_code":0,"timed_out":false,
            "navigation":{"report":{"status":"preempted","pending_action":null}},
            "local_action_feedback":{"requests":[{"response":{"outcome":"cancelled"}}]}});
        classify_slice(&mut result, &goal, None);
        result
    }
    #[test]
    fn control_preemption_preserves_prior_program_feedback_and_is_not_failure_or_progress() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(&dir.path().join("brain.sqlite")).unwrap();
        let prior = json!({"source_sha256":"failed-source","result":{"exit_code":101}}).to_string();
        journal.set("last_experiment", &prior).unwrap();
        journal
            .set("experiment_signature", "prior-signature")
            .unwrap();
        journal.set("inspection_streak", "7").unwrap();
        journal.set("needs_investigation", "true").unwrap();
        let result = preempted_result();
        assert!(scheduling_preempted(&result));
        assert_eq!(result["success"], false);
        assert_eq!(result["navigation_slice"]["completed"], false);
        assert_eq!(result["navigation_slice"]["can_resume"], true);
        assert!(!crate::tool_failed(&result));
        assert!(!crate::progress::executed(
            "navigate", &result, "before", "after"
        ));
        let choice = crate::model::Choice {
            tool: "navigate".into(),
            input: "{}".into(),
            path: String::new(),
            summary: "control handoff".into(),
            notes: String::new(),
            experiment: false,
        };
        assert!(
            !journal
                .experiment(&choice, &result, "unchanged", "goal")
                .unwrap()
        );
        journal.activity(&choice, &result).unwrap();
        assert_eq!(
            journal.get("last_experiment").unwrap().as_deref(),
            Some(prior.as_str())
        );
        assert_eq!(
            journal.get("experiment_signature").unwrap().as_deref(),
            Some("prior-signature")
        );
        assert_eq!(
            journal.get("inspection_streak").unwrap().as_deref(),
            Some("7")
        );
        assert_eq!(
            journal.get("needs_investigation").unwrap().as_deref(),
            Some("true")
        );
        assert!(journal.get("activity").unwrap().is_none());
    }
    #[test]
    fn scheduling_preemption_does_not_hide_real_or_mixed_failures() {
        let mut local = preempted_result();
        local["local_action_feedback"]["requests"] = json!([
            {"response":{"error":{"code":"local_preempted","message":"not dispatched"}}}]);
        assert!(!crate::tool_failed(&local));
        for outcome in ["rejected", "uncertain", "failed", "expired"] {
            let mut mixed = local.clone();
            mixed["local_action_feedback"]["requests"]
                .as_array_mut()
                .unwrap()
                .push(json!({"response":{"outcome":outcome}}));
            assert!(!scheduling_preempted(&mixed), "{outcome}");
            assert!(crate::tool_failed(&mixed), "{outcome}");
        }
        for field in [
            "error",
            "navigation_error",
            "capture_error",
            "pending",
            "timeout",
            "exit",
        ] {
            let mut bad = local.clone();
            match field {
                "error" => bad["error"] = json!("real failure"),
                "navigation_error" => bad["navigation"]["error"] = json!({"code":"conflict"}),
                "capture_error" => bad["output_files"]["error"] = json!("output unavailable"),
                "pending" => bad["navigation"]["report"]["pending_action"] = json!("unresolved"),
                "timeout" => bad["timed_out"] = json!(true),
                _ => bad["exit_code"] = json!(101),
            }
            assert!(!scheduling_preempted(&bad), "{field}");
            assert!(crate::tool_failed(&bad), "{field}");
        }
    }
    #[test]
    fn navigation_resume_needs_changed_ready_control_facts_not_bobbing_or_clocks() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(&dir.path().join("brain.sqlite")).unwrap();
        journal.set("goal_revision", "goal").unwrap();
        let mut observation = minecraft_sdk::Observation {
            revision: "one".into(),
            player: json!({"uuid":"own","entity_id":7,"dimension":"fixture:world",
                "position":[1.,62.,2.],"air":250,"control":{"action_id":"breathing",
                "outcome":"running","input_released":false,"progress":{"elapsed_ms":100}}}),
            world: json!({"game_time":1,"sample_clock":{"epoch":"life","native_tick":1}}),
            menu: Value::Null,
        };
        let mut body = json!({"running":true,"emergency":true});
        journal
            .set(
                "navigation_pause",
                &json!({"goal_revision":"goal",
            "control_basis":control_basis(&observation,&body)})
                .to_string(),
            )
            .unwrap();
        observation.player["position"] = json!([1., 62.5, 2.]);
        observation.player["air"] = json!(300);
        observation.world["game_time"] = json!(300);
        observation.player["control"]["progress"]["elapsed_ms"] = json!(3000);
        assert_eq!(
            resume_context(&journal, &observation, &body).unwrap()["control_facts_changed"],
            false
        );
        body["emergency"] = json!(false);
        assert_eq!(
            resume_context(&journal, &observation, &body).unwrap()["ready"],
            false
        );
        observation.player["control"]["outcome"] = json!("cancelled");
        assert_eq!(
            resume_context(&journal, &observation, &body).unwrap()["ready"],
            false
        );
        observation.player["control"]["input_released"] = json!(true);
        assert_eq!(
            resume_context(&journal, &observation, &body).unwrap()["ready"],
            true
        );
        body["running"] = json!(false);
        assert_eq!(
            resume_context(&journal, &observation, &body).unwrap()["ready"],
            false
        );
        journal.set("goal_revision", "another-task").unwrap();
        assert!(
            resume_context(&journal, &observation, &body)
                .unwrap()
                .is_null()
        );
    }
    #[test]
    fn urgency_transition_revision_resumes_even_when_native_control_fields_look_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(&dir.path().join("brain.sqlite")).unwrap();
        journal.set("goal_revision", "goal").unwrap();
        let observation = minecraft_sdk::Observation {
            revision: "one".into(),
            player: json!({"uuid":"own","entity_id":7,"dimension":"fixture:world","control":null}),
            world: json!({"sample_clock":{"epoch":"life"}}),
            menu: Value::Null,
        };
        let mut body = json!({"running":true,"emergency":false,"scheduling_revision":7});
        journal
            .set(
                "navigation_pause",
                &json!({"goal_revision":"goal",
            "control_basis":control_basis(&observation,&body)})
                .to_string(),
            )
            .unwrap();
        assert_eq!(
            resume_context(&journal, &observation, &body).unwrap()["ready"],
            false
        );
        body["scheduling_revision"] = json!(9);
        assert_eq!(
            resume_context(&journal, &observation, &body).unwrap()["ready"],
            true
        );
        body["emergency"] = json!(true);
        assert_eq!(
            resume_context(&journal, &observation, &body).unwrap()["ready"],
            false
        );
    }
    #[test]
    fn wet_boundary_predictions_survive_without_weakening_defense_or_reusing_old_policy() {
        let policy = navigation_blocked_blocks();
        let wet_route = ["minecraft:grass_block", "minecraft:water"];
        assert!(!wet_route.iter().any(|id| policy.iter().any(|b| b == id)));
        assert!(policy.iter().any(|id| id == "minecraft:lava"));
        assert!(crate::body::geometry::KNOWN_HAZARDS.contains(&"minecraft:water"));
        let goal: Goal = serde_json::from_value(json!({"kind":"position",
            "dimension":"fixture:world","target":[1.,64.,2.],"distance":1.}))
        .unwrap();
        let legacy = digest(
            json!([
                "instance",
                json!("own"),
                goal,
                crate::body::geometry::KNOWN_HAZARDS
            ])
            .to_string()
            .as_bytes(),
        );
        assert_ne!(
            journal_key("instance", &json!("own"), &goal),
            legacy,
            "A new routing policy must not reuse the exhausted water-blocked graph"
        );
    }
    #[test]
    fn a_finite_slice_is_resumable_without_becoming_success_or_execution_credit() {
        let goal: Goal =
            serde_json::from_value(json!({"kind":"position","dimension":"fixture:world",
            "target":[10.,64.,0.],"distance":0.5}))
            .unwrap();
        let mut result = json!({"success":true,"exit_code":0,"timed_out":false,
            "navigation":{"report":{"status":"budget_exhausted","pending_action":"existing-id"}},
            "local_action_feedback":{"requests":[]}});
        classify_slice(&mut result, &goal, None);
        assert!(resumable_slice(&result));
        assert_eq!(result["success"], false);
        assert!(!crate::tool_failed(&result));
        assert!(
            !crate::progress::executed("navigate", &result, "same", "same"),
            "A resumable label alone cannot reset the unchanged-inspection guard"
        );
        assert_eq!(
            result["navigation"]["report"]["pending_action"],
            "existing-id"
        );
        let restored: Goal =
            serde_json::from_value(result["navigation_slice"]["goal"].clone()).unwrap();
        assert_eq!(restored, goal);
        for status in [
            "no_known_route",
            "target_not_reached",
            "needs_reconciliation",
        ] {
            let mut failure = json!({"success":true,"exit_code":0,"timed_out":false,
                "navigation":{"report":{"status":status}}});
            classify_slice(&mut failure, &goal, None);
            assert!(!resumable_slice(&failure));
            assert!(crate::tool_failed(&failure));
        }
        for case in [
            "timeout",
            "process",
            "native_rejection",
            "uncertain",
            "error",
        ] {
            let mut failure = result.clone();
            match case {
                "timeout" => failure["timed_out"] = json!(true),
                "process" => failure["exit_code"] = json!(1),
                "error" => failure["navigation"]["error"] = json!({"code":"unsupported_version"}),
                _ => {
                    failure["local_action_feedback"]["requests"] = json!([{"response":{
                    "outcome":if case=="uncertain"{"uncertain"}else{"rejected"}}}])
                }
            }
            assert!(
                crate::tool_failed(&failure),
                "{case} must retain its failure boundary"
            );
        }
    }
    #[test]
    fn route_journals_are_actor_server_and_goal_bound_not_input_format_bound() {
        let input =
            json!({"kind":"position","dimension":"mod:forest","target":[1.,64.,2.],"distance":0.5});
        let goal: Goal = serde_json::from_value(input.clone()).unwrap();
        let key = journal_key("instance", &json!("own"), &goal);
        let reordered: Goal = serde_json::from_str(
            r#"{"distance":0.5,"target":[1,64,2],"dimension":"mod:forest","kind":"position"}"#,
        )
        .unwrap();
        assert_eq!(key, journal_key("instance", &json!("own"), &reordered));
        assert_ne!(key, journal_key("other-instance", &json!("own"), &goal));
        assert_ne!(key, journal_key("instance", &json!("other"), &goal));
        for field in ["actor", "profile", "url", "journal", "command"] {
            let mut bad = input.clone();
            bad[field] = json!("override");
            assert!(serde_json::from_value::<Goal>(bad).is_err());
        }
    }
}
