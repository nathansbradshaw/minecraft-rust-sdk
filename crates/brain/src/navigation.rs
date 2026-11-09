//! Container-local composition of the existing SDK navigator. No new authority.
use crate::{Brain, Result, digest, ensure, private_read, private_write};
use minecraft_sdk::{
    Client, Profile,
    navigation::{Goal, Options},
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
fn journal_key(instance: &str, actor: &Value, goal: &Goal) -> String {
    digest(
        json!([instance, actor, goal, crate::body::geometry::KNOWN_HAZARDS])
            .to_string()
            .as_bytes(),
    )
}
fn classify_slice(result: &mut Value, goal: &Goal) {
    let transport_ok =
        result["success"] == true && result["exit_code"] == 0 && result["timed_out"] == false;
    let status = result["navigation"]["report"]["status"]
        .as_str()
        .unwrap_or("error")
        .to_owned();
    let resumable =
        transport_ok && status == "budget_exhausted" && result["navigation"]["error"].is_null();
    result["success"] = json!(transport_ok && status == "reached");
    result["navigation_slice"] = json!({"format":"navigation-slice-v1",
        "completed":result["success"],"can_resume":resumable,"goal":goal,
        "resume_tool":"navigate","scope":"Local bounded helper outcome, not proof of task completion. A normal slice deadline can resume the same goal/journal; pending IDs are inspected rather than replayed. Investigate unchanged slices without native movement or new read work; do not poll indefinitely."});
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
    let goal: Goal = serde_json::from_str(input)?;
    let key = journal_key(
        &brain.server.instance_id,
        &brain.observed.player["uuid"],
        &goal,
    );
    let sequence = brain.journal.event(
        "navigation_tool_intent",
        json!({"goal":goal,"journal_key":key,"built_in_helper":true,"generated_skill":false}),
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
            blocked_blocks: crate::body::geometry::KNOWN_HAZARDS
                .iter()
                .map(|id| (*id).to_owned())
                .collect(),
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
            classify_slice(&mut result, &request.goal);
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
    result["helper_scope"] = json!(
        "Built-in bounded SDK navigation; report and local feedback are not external authority or proof of resource collection. Persistent journal retains pending action IDs."
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_finite_slice_is_resumable_without_becoming_success_or_execution_credit() {
        let goal: Goal =
            serde_json::from_value(json!({"kind":"position","dimension":"fixture:world",
            "target":[10.,64.,0.],"distance":0.5}))
            .unwrap();
        let mut result = json!({"success":true,"exit_code":0,"timed_out":false,
            "navigation":{"report":{"status":"budget_exhausted","pending_action":"existing-id"}},
            "local_action_feedback":{"requests":[]}});
        classify_slice(&mut result, &goal);
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
            classify_slice(&mut failure, &goal);
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
