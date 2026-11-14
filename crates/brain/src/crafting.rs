//! Optional container-local composition of the SDK's journaled single batch.
//! No recipe policy, new player authority, or inference-host execution.
use crate::{Brain, Result, digest, ensure, private_read, private_write};
use minecraft_sdk::{Client, Profile};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    recipe: String,
    operation_id: String,
}
fn input(bytes: &str) -> Result<Input> {
    ensure(bytes.len() <= 2048, "Bounded crafting input required")?;
    let input: Input = serde_json::from_str(bytes)?;
    ensure(
        minecraft_sdk::registry_id(&input.recipe),
        "Runtime recipe ID required",
    )?;
    ensure(
        minecraft_sdk::valid_id(&input.operation_id) && input.operation_id.len() <= 96,
        "Opaque crafting operation_id up to96bytes required; reuse it to resume, change it only for a distinct batch",
    )?;
    Ok(input)
}
fn key(instance: &str, actor: &Value, operation_id: &str) -> String {
    // Exclude recipe and life: reusing an operation with a different recipe or
    // after respawn must hit the SAME SDK journal and fail its binding checks.
    digest(
        json!(["craft-once-v1", instance, actor, operation_id])
            .to_string()
            .as_bytes(),
    )
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u8,
    profile: PathBuf,
    recipe: String,
    prefix: String,
    journal: PathBuf,
    result: PathBuf,
    wait_ms: u64,
}
pub async fn worker(path: &Path) -> Result<()> {
    ensure(
        std::env::var_os("MC_SDK_ARBITER").is_some(),
        "Craft worker requires the container body arbiter; no direct-gateway fallback",
    )?;
    let request: Request = serde_json::from_slice(&private_read(path, 16384)?)?;
    ensure(
        request.version == 1 && (1..=15000).contains(&request.wait_ms),
        "Unsupported crafting worker version or wait budget",
    )?;
    let profile: Profile = serde_json::from_slice(&private_read(&request.profile, 16384)?)?;
    // Inherit the same per-action arbiter as generated programs. Never hold the
    // body's mutation lock across the batch, connect, or replace its session.
    let client = Client::with_profile(&profile)?;
    let response = match minecraft_sdk::menu::craft_once(
        &client,
        &request.recipe,
        &request.prefix,
        &request.journal,
        Duration::from_millis(request.wait_ms),
    )
    .await
    {
        Ok(report) => json!({"report":report}),
        Err(error) => json!({"error":error}),
    };
    private_write(&request.result, &serde_json::to_vec(&response)?)?;
    println!("{response}");
    Ok(())
}
fn quoted(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}
fn transport_ok(result: &Value) -> bool {
    result["exit_code"] == 0
        && result["timed_out"] == false
        && result["error"].is_null()
        && result["crafting_result_error"].is_null()
        && result["output_files"]["error"].is_null()
        && result["local_action_feedback"]["error"].is_null()
}
pub(crate) fn scheduling_preempted(result: &Value) -> bool {
    transport_ok(result)
        && result["crafting_batch"]["format"] == "craft-once-tool-v1"
        && result["crafting_batch"]["worker_completed"] == true
        && result["crafting"]["error"].is_null()
        && result["crafting"]["report"]["status"] == "local_preempted"
        && result["crafting"]["report"]["receipt_outcome"].is_null()
        && result["crafting"]["report"]["error"]["code"] == "local_preempted"
        && result["local_action_feedback"]["requests"]
            .as_array()
            .is_some_and(|rows| {
                rows.iter().all(|row| {
                    let response = &row["response"];
                    (response["error"]["code"] == "local_preempted"
                        && response["outcome"].is_null())
                        || (response["error"].is_null() && response["outcome"] == "observed")
                })
            })
}
fn classify(result: &mut Value) {
    let worker_completed = result["success"] == true && transport_ok(result);
    let status = result["crafting"]["report"]["status"]
        .as_str()
        .unwrap_or("error")
        .to_owned();
    let completed =
        worker_completed && result["crafting"]["error"].is_null() && status == "completed";
    let historical =
        worker_completed && result["crafting"]["error"].is_null() && status == "already_completed";
    result["success"] = json!(completed || historical);
    result["crafting_batch"] = json!({"format":"craft-once-tool-v1","worker_completed":worker_completed,
        "new_batch_completed":completed,"historical_completion":historical,
        "resume_tool":"craft_once","resume_same_operation_id":true,
        "scope":"Existing SDK client composition, not a server transaction or benchmark certification. Completed verifies one retained output batch. AlreadyCompleted is journal history, not a new craft or proof the items are still retained. Pending/LocalPreempted resume the SAME operation after changed ready facts. NeedsInspection/Stopped require original-ID inspection; never clear the journal or replace IDs to retry."});
    result["crafting_batch"]["paused_for_control"] = json!(scheduling_preempted(result));
}
pub async fn execute(brain: &mut Brain, bytes: &str) -> Result<Value> {
    ensure(
        brain.body.is_some(),
        "Craft tool requires the responsive body",
    )?;
    let input = input(bytes)?;
    ensure(
        brain
            .journal
            .get("pending_action")?
            .is_none_or(|id| id.is_empty()),
        "Inspect/reconcile the existing brain action before starting a crafting helper",
    )?;
    let key = key(
        &brain.server.instance_id,
        &brain.observed.player["uuid"],
        &input.operation_id,
    );
    let sequence = brain.journal.event(
        "craft_tool_intent",
        json!({"recipe":input.recipe,"operation_id":input.operation_id,"journal_key":key,
            "built_in_helper":true,"generated_skill":false}),
    )?;
    let stem = format!("craft-{}-{sequence}", brain.run_id);
    let request_file = brain.cfg.state.join(format!("{stem}.request.json"));
    let result_file = brain.cfg.state.join(format!("{stem}.result.json"));
    let wait_ms = 15000.min(
        brain
            .cfg
            .tool_timeout_secs
            .saturating_mul(1000)
            .saturating_sub(500),
    );
    ensure(wait_ms > 0, "Crafting needs a nonzero bounded tool budget")?;
    let request = Request {
        version: 1,
        profile: brain.cfg.profile.clone(),
        recipe: input.recipe,
        prefix: format!("craft-{}", &key[..48]),
        journal: brain.cfg.state.join(format!("craft-{key}.json")),
        result: result_file.clone(),
        wait_ms,
    };
    private_write(&request_file, &serde_json::to_vec(&request)?)?;
    let executable = PathBuf::from(format!("/proc/{}/exe", std::process::id()));
    let command = format!(
        "{} craft-worker {}",
        quoted(&executable),
        quoted(&request_file)
    );
    let mut result = crate::tools::terminal(brain, &command).await?;
    match private_read(&result_file, 16384)
        .and_then(|bytes| Ok(serde_json::from_slice::<Value>(&bytes)?))
    {
        Ok(response) => {
            result["crafting"] = response;
            classify(&mut result);
        }
        Err(error) => {
            result["success"] = json!(false);
            result["crafting_result_error"] = json!(error.to_string());
            result["next_action"] = json!(
                "Worker result unavailable: preserve and inspect this SAME operation journal and original IDs before any new batch; a process timeout cannot erase a submitted mutation."
            );
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use minecraft_sdk::menu::CraftStatus;
    #[test]
    fn craft_tool_rejects_path_authority_and_binds_one_operation_across_recipe_and_life() {
        for bad in [
            r#"{"recipe":"fixture:result","operation_id":"../other"}"#,
            r#"{"recipe":"../recipe","operation_id":"one"}"#,
            r#"{"recipe":"fixture:result","operation_id":"one","journal":"/private/other"}"#,
            r#"{"recipe":"fixture:result","operation_id":"one","profile":"other"}"#,
        ] {
            assert!(input(bad).is_err(), "{bad}");
        }
        let a = input(r#"{"recipe":"fixture:one","operation_id":"batch_1"}"#).unwrap();
        let b = input(r#"{"recipe":"other:two","operation_id":"batch_1"}"#).unwrap();
        let own = json!("own");
        assert_eq!(
            key("instance", &own, &a.operation_id),
            key("instance", &own, &b.operation_id)
        );
        assert_ne!(
            key("instance", &own, &a.operation_id),
            key("instance", &json!("other"), &a.operation_id)
        );
        assert_ne!(
            key("instance", &own, &a.operation_id),
            key("other", &own, &a.operation_id)
        );
        assert_ne!(
            key("instance", &own, &a.operation_id),
            key("instance", &own, "batch_2")
        );
    }
    fn result(status: CraftStatus) -> Value {
        json!({"success":true,"exit_code":0,"timed_out":false,"local_action_feedback":{"requests":[]},
            "crafting":{"report":{"status":status,"receipt_outcome":null,"error":null}}})
    }
    #[test]
    fn craft_tool_never_promotes_pending_inspection_or_historical_completion_to_new_batch() {
        for status in [
            CraftStatus::Pending,
            CraftStatus::NeedsInspection,
            CraftStatus::Stopped,
            CraftStatus::AlreadyCompleted,
        ] {
            let mut value = result(status);
            classify(&mut value);
            assert_eq!(value["crafting_batch"]["new_batch_completed"], false);
            assert!(!scheduling_preempted(&value));
            value["inventory_changed"] = json!(true);
            assert!(!crate::progress::executed(
                "craft_once",
                &value,
                "before",
                "after"
            ));
        }
        let mut value = result(CraftStatus::Completed);
        classify(&mut value);
        assert_eq!(value["crafting_batch"]["new_batch_completed"], true);
        value["timed_out"] = json!(true);
        classify(&mut value);
        assert_eq!(value["crafting_batch"]["new_batch_completed"], false);
    }
    #[test]
    fn craft_tool_pause_cannot_hide_an_uncertain_or_failed_native_request() {
        let mut value = result(CraftStatus::LocalPreempted);
        value["crafting"]["report"]["error"] = json!({"code":"local_preempted"});
        classify(&mut value);
        assert!(scheduling_preempted(&value));
        assert!(!crate::tool_failed(&value));
        assert!(!crate::progress::executed(
            "craft_once",
            &value,
            "before",
            "after"
        ));
        for outcome in [
            "running",
            "uncertain",
            "reconciled",
            "rejected",
            "expired",
            "failed",
        ] {
            let mut bad = value.clone();
            bad["local_action_feedback"]["requests"] = json!([{"response":{"outcome":outcome}}]);
            assert!(!scheduling_preempted(&bad), "{outcome}");
            assert!(crate::tool_failed(&bad), "{outcome}");
        }
        value["crafting"]["report"]["receipt_outcome"] = json!("uncertain");
        assert!(!scheduling_preempted(&value));
    }
}
