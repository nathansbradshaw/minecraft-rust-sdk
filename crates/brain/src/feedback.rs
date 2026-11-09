//! Bounded client diagnostics, never authority for game effects.
use minecraft_sdk::{ActionRequest, LocalActionResponse, Receipt};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};

#[derive(Default)]
pub struct Trace {
    sequence: u64,
    records: VecDeque<Value>,
}
pub fn receipt_summary(r: &Receipt) -> Value {
    json!({"action_id":r.action_id,"outcome":r.outcome,"native_effect_verified":r.evidence["native_effect_verified"],"error":r.error})
}
pub fn compiler_diagnostics(stdout: &[u8]) -> Value {
    let mut errors = Vec::new();
    let mut total = 0usize;
    for line in stdout.split(|b| *b == b'\n') {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if value["reason"] != "compiler-message"
            || !matches!(value["message"]["level"].as_str(), Some("error" | "ice"))
        {
            continue;
        }
        total += 1;
        if errors.len() < 4 {
            let rendered = value["message"]["rendered"].as_str().unwrap_or_default();
            let text: String = rendered.chars().take(1800).collect();
            let message: String = value["message"]["message"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .take(1200)
                .collect();
            errors.push(json!({"level":value["message"]["level"],"code":value["message"]["code"]["code"],"message":message,"rendered":text}));
        }
    }
    json!({"errors":errors,"total_errors":total,"omitted_errors":total.saturating_sub(4),"scope":"client compiler output, complete bytes retained in output_files"})
}
fn prefix(text: &str, limit: usize) -> String {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// Keep primary compiler errors ahead of routine notes and large stdout/history.
pub fn compiler_view(experiment: &Value) -> Value {
    let diagnostics = &experiment["result"]["compiler_diagnostics"];
    let Some(records) = diagnostics["errors"].as_array() else {
        return Value::Null;
    };
    let actual: Vec<_> = records
        .iter()
        .filter(|r| matches!(r["level"].as_str(), Some("error" | "ice")))
        .collect();
    let files = &experiment["result"]["output_files"];
    for limit in [900, 400, 160, 64] {
        let rows: Vec<_> = actual
            .iter()
            .take(2)
            .map(|r| {
                json!({
                    "code":r["code"].as_str().map(|s|prefix(s,32)),
                    "message":prefix(r["message"].as_str().unwrap_or_default(),limit.min(200)),
                    "rendered":prefix(r["rendered"].as_str().unwrap_or_default(),limit),
                    "rendered_complete":r["rendered"].as_str().is_some_and(|s|s.len()<=limit)
                })
            })
            .collect();
        let locations:serde_json::Map<_,_>=["stdout","stderr"].into_iter().filter_map(|name| {
            files[name]["path"].as_str().map(|path|(name.to_owned(),json!({"path":prefix(path,limit.min(256)),"path_complete":path.len()<=limit.min(256),"sha256":files[name]["sha256"]})))
        }).collect();
        let value = json!({"format":"compiler-feedback-v1","goal_revision":experiment["goal_revision"],
            "source_sha256":experiment["source_sha256"],"tool":experiment["tool"],
            "success":experiment["result"]["success"],"exit_code":experiment["result"]["exit_code"],
            "errors":rows,"retained_actual_errors":actual.len().min(2),
            "omitted_actual_errors":actual.len().saturating_sub(2),
            "upstream_omitted_records":diagnostics["omitted_errors"],"output_files":locations,
            "scope":"Historical client compiler diagnostics for the recorded source, not current compile/gameplay success. After an edit compile the changed version; do not repeatedly reconstruct unchanged errors."});
        if value.to_string().len() <= 4096 {
            return value;
        }
    }
    json!({"format":"compiler-feedback-v1","oversized_metadata":true,"errors":actual.iter().take(2).map(|r|json!({"message":prefix(r["message"].as_str().unwrap_or_default(),80)})).collect::<Vec<_>>(),"scope":"Bounded partial compiler view; original journal/output bytes remain available"})
}

impl crate::Journal {
    pub(crate) fn compiler_feedback(&self, revision: &str) -> crate::Result<Value> {
        let value = if let Some(saved) = self.get("compiler_feedback")? {
            serde_json::from_str(&saved)?
        } else {
            // Upgrade existing failed runs without code execution or inference.
            let experiment: Value = self
                .get("last_experiment")?
                .map(|s| serde_json::from_str(&s))
                .transpose()?
                .unwrap_or(Value::Null);
            compiler_view(&experiment)
        };
        Ok(if value["goal_revision"] == revision {
            value
        } else {
            Value::Null
        })
    }
}

impl Trace {
    pub fn cursor(&self) -> u64 {
        self.sequence
    }
    pub fn record(&mut self, request: &ActionRequest, response: &LocalActionResponse) {
        self.sequence = self.sequence.saturating_add(1);
        let inspection = match response {
            LocalActionResponse::Receipt(r) => crate::containers::inspection(request, r),
            _ => None,
        };
        let response = match response {
            LocalActionResponse::Receipt(r) => receipt_summary(r),
            LocalActionResponse::Error(e) => json!({"action_id":request.action_id,"error":e}),
        };
        self.records.push_back(json!({"sequence":self.sequence,"request":request,"response":response,"container_inspection":inspection}));
        while self.records.len() > 64 {
            self.records.pop_front();
        }
    }
    pub fn since(&self, cursor: u64) -> Value {
        let rows: Vec<_> = self
            .records
            .iter()
            .filter(|r| r["sequence"].as_u64().is_some_and(|s| s > cursor))
            .collect();
        let omitted = rows.len().saturating_sub(16);
        json!({"scope":"local action arbiter only; clients can bypass it; not authoritative or necessarily complete","from_sequence":cursor,"through_sequence":self.sequence,"omitted_records":self.sequence.saturating_sub(cursor).saturating_sub(rows.len() as u64)+omitted as u64,"requests":rows.into_iter().skip(omitted).collect::<Vec<_>>()})
    }
}
pub fn inventory_delta(previous: &Value, current: &Value) -> Value {
    fn counts(rows: &Value) -> BTreeMap<String, i64> {
        let mut result = BTreeMap::new();
        for row in rows.as_array().into_iter().flatten() {
            if let (Some(item), Some(count)) = (row["item"].as_str(), row["count"].as_i64())
                && count > 0
            {
                *result.entry(item.to_owned()).or_default() += count;
            }
        }
        result
    }
    let old = counts(previous);
    let new = counts(current);
    let mut ids: Vec<_> = old.keys().chain(new.keys()).cloned().collect();
    ids.sort();
    ids.dedup();
    json!(
        ids.into_iter()
            .filter_map(|item| {
                let a = *old.get(&item).unwrap_or(&0);
                let b = *new.get(&item).unwrap_or(&0);
                (a != b)
                    .then(|| json!({"item":item,"previous_count":a,"current_count":b,"change":b-a}))
            })
            .collect::<Vec<_>>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use minecraft_sdk::{Action, ApiError, ErrorCode};
    #[test]
    fn compiler_errors_survive_large_program_output() {
        let diagnostic = json!({"reason":"compiler-message","message":{"level":"error","code":{"code":"E0308"},"message":"type mismatch","rendered":"expected a block position"}});
        let mut bytes = vec![b'x'; 10000];
        bytes.push(b'\n');
        bytes.extend(diagnostic.to_string().as_bytes());
        bytes.push(b'\n');
        bytes.extend(vec![b'y'; 10000]);
        let result = compiler_diagnostics(&bytes);
        assert_eq!(result["total_errors"], 1);
        assert_eq!(result["errors"][0]["code"], "E0308");
        assert_eq!(result["errors"][0]["message"], "type mismatch");
    }
    #[test]
    fn primary_errors_survive_journal_and_model_compaction() {
        let dir = tempfile::tempdir().unwrap();
        let journal = crate::Journal::open(&dir.path().join("brain.sqlite")).unwrap();
        journal.set("goal_revision", "goal-one").unwrap();
        let records = json!([
            {"level":"error","code":"E0603","message":"module is private","rendered":format!("error[E0603]: module is private\n{}", "λ".repeat(6000))},
            {"level":"error","code":"E0308","message":"mismatched types","rendered":format!("error[E0308]: expected i16, found u64\n{}", "x".repeat(6000))},
            {"level":"failure-note","message":"abort due to 2 previous errors"},
            {"level":"failure-note","message":"could not compile"}
        ]);
        let experiment = json!({"tool":"run","goal_revision":"goal-one","source_sha256":"failed-source","result":{"success":false,"exit_code":101,"stdout":"x".repeat(16000),"compiler_diagnostics":{"errors":records,"omitted_errors":0},"output_files":{"stdout":{"path":"output/full.jsonl","sha256":"output-hash"}}}});
        // Legacy journals retained the raw diagnostics but generic context
        // compaction previously kept only the final two failure notes.
        journal
            .set("last_experiment", &experiment.to_string())
            .unwrap();
        let mut context = json!({"goal":"craft a shovel","sdk_reference":include_str!("sdk.md"),"observation":{"player":{"position":[1,68,2]}},"server":"s".repeat(10000),"recent":["old".repeat(10000)]});
        let report = crate::replay_context(&journal, &mut context).unwrap();
        assert_eq!(report["fit"], true);
        let feedback = &context["compiler_feedback"];
        assert_eq!(feedback["errors"][0]["code"], "E0603");
        assert_eq!(feedback["errors"][1]["code"], "E0308");
        assert_eq!(feedback["retained_actual_errors"], 2);
        assert_eq!(feedback["source_sha256"], "failed-source");
        assert_eq!(
            feedback["output_files"]["stdout"]["path"],
            "output/full.jsonl"
        );
        assert_eq!(feedback["errors"][0]["rendered_complete"], false);
        assert!(feedback.to_string().len() <= 4096);
        assert!(context.to_string().len() <= 32768);
        assert!(journal.compiler_feedback("another-goal").unwrap().is_null());

        let choice = crate::model::Choice {
            tool: "run".into(),
            input: "candidate".into(),
            path: String::new(),
            summary: "compile".into(),
            notes: String::new(),
            experiment: true,
        };
        journal
            .experiment(&choice, &experiment["result"], "failed-source", "goal-one")
            .unwrap();
        assert_eq!(
            journal.compiler_feedback("goal-one").unwrap()["errors"][1]["code"],
            "E0308"
        );
        // A later successful compiler result supersedes historical errors.
        journal.experiment(&choice, &json!({"success":true,"exit_code":0,"compiler_diagnostics":{"errors":[],"omitted_errors":0}}), "fixed-source", "goal-one").unwrap();
        let success = journal.compiler_feedback("goal-one").unwrap();
        assert_eq!(success["source_sha256"], "fixed-source");
        assert_eq!(success["errors"], json!([]));
    }
    #[test]
    fn compiler_notes_are_not_errors() {
        let lines = ["error", "failure-note", "warning", "error"].map(|level|json!({"reason":"compiler-message","message":{"level":level,"message":"diagnostic"}}).to_string()).join("\n");
        assert_eq!(compiler_diagnostics(lines.as_bytes())["total_errors"], 2);
    }
    #[test]
    fn trace_is_scoped_bounded_and_preserves_failed_request_identity() {
        let mut trace = Trace::default();
        for n in 0..80 {
            trace.record(
                &ActionRequest {
                    action_id: format!("attempt-{n}"),
                    action: Action::Look { yaw: 1., pitch: 2. },
                },
                &LocalActionResponse::Error(ApiError::new(
                    ErrorCode::PermissionDenied,
                    "fixture denial",
                )),
            );
        }
        let rows = trace.since(0);
        assert_eq!(rows["omitted_records"], 64);
        assert_eq!(rows["requests"].as_array().unwrap().len(), 16);
        assert_eq!(rows["requests"][15]["request"]["action_id"], "attempt-79");
        assert_eq!(
            rows["requests"][15]["response"]["error"]["code"],
            "permission_denied"
        );
        assert!(
            trace.since(trace.cursor())["requests"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn inventory_transfer_is_not_mistaken_for_acquisition() {
        let old = json!([{"slot":0,"item":"custom:item","count":2}]);
        let moved = json!([{"slot":8,"item":"custom:item","count":2}]);
        assert_eq!(inventory_delta(&old, &moved), json!([]));
        assert_eq!(
            inventory_delta(&old, &json!([{"slot":8,"item":"custom:item","count":3}])),
            json!([{"item":"custom:item","previous_count":2,"current_count":3,"change":1}])
        );
    }
}
