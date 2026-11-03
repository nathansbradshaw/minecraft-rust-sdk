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
            || !matches!(
                value["message"]["level"].as_str(),
                Some("error" | "failure-note" | "ice")
            )
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
