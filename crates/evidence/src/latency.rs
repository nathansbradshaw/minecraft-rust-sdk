//! Join pinned controller observations with operator-captured native effects.
use crate::{Result, ensure, sha256};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn nanos(clock: &Value) -> Option<(&str, u64)> {
    Some((
        clock["epoch"].as_str()?,
        clock["nanos"].as_str()?.parse().ok()?,
    ))
}
fn bound(sample: &Value, effect: &Value) -> Option<u64> {
    if effect["verified"] != true {
        return None;
    }
    let (epoch, start) = nanos(sample)?;
    let (after_epoch, end) = nanos(&effect["sample_clock"])?;
    let elapsed = end.wrapping_sub(start);
    (epoch == after_epoch && elapsed <= 60_000_000_000).then_some(elapsed)
}
fn stats(mut values: Vec<u64>, divisor: f64) -> Value {
    values.sort_unstable();
    if values.is_empty() {
        return json!({"count":0,"p95_ms":null,"max_ms":null});
    }
    json!({"count":values.len(),"p95_ms":values[(values.len()*95).div_ceil(100)-1] as f64/divisor,"max_ms":*values.last().unwrap() as f64/divisor})
}
pub fn summarize(trace: &[u8], report: &Value) -> Result<Value> {
    ensure(
        report["format"] == "minecraft-sdk-native-run-evidence-v1",
        "Native capture format required",
    )?;
    ensure(
        report["spec"]["trace_sha256"] == sha256(trace),
        "Native capture is bound to a different trace",
    )?;
    let mut receipts = BTreeMap::new();
    for row in report["native_actions"]
        .as_array()
        .ok_or("Native actions missing")?
    {
        let id = row["receipt"]["action_id"]
            .as_str()
            .ok_or("Native action ID missing")?;
        ensure(
            receipts.insert(id, row).is_none(),
            "Duplicate native action ID",
        )?;
    }
    let mut ids = BTreeSet::new();
    let mut physical = Vec::new();
    let mut acknowledgements = Vec::new();
    let mut missing_acknowledgements = Vec::new();
    let mut missing = Vec::new();
    let mut samples = Vec::new();
    let mut identity_checked = false;
    for line in std::str::from_utf8(trace)?
        .lines()
        .filter(|s| !s.trim().is_empty())
    {
        let event: Value = serde_json::from_str(line)?;
        if event["event"] == "brain_started" {
            ensure(
                event["data"]["uuid"] == report["spec"]["player_uuid"]
                    && event["data"]["instance"] == report["spec"]["instance_id"],
                "Controller/native actor or instance differs",
            )?;
            identity_checked = true;
        }
        if event["event"] != "body_reaction" {
            continue;
        }
        let data = &event["data"];
        let id = data["action_id"]
            .as_str()
            .ok_or("Reaction action ID missing")?;
        ensure(ids.insert(id.to_owned()), "Duplicate reaction action ID")?;
        if let Some(ms) = data["reaction_ms"].as_u64() {
            acknowledgements.push(ms);
        } else {
            missing_acknowledgements.push(id.to_owned());
        }
        let row = receipts.get(id);
        let elapsed = row.and_then(|row| {
            if row["action"]["kind"] != "start_move"
                || row["receipt"]["evidence"]["before"]["player"]["uuid"]
                    != report["spec"]["player_uuid"]
            {
                return None;
            }
            bound(
                &data["sample_clock"],
                &row["receipt"]["evidence"]["first_effect"],
            )
        });
        match elapsed {
            Some(ns) => {
                physical.push(ns);
                samples.push(json!({"action_id":id,"upper_bound_ns":ns.to_string(),"upper_bound_ms":ns as f64/1_000_000.}));
            }
            None => missing.push(id.to_owned()),
        }
    }
    ensure(
        identity_checked,
        "Controller actor/instance binding missing",
    )?;
    ensure(!ids.is_empty(), "No movement reactions in trace")?;
    Ok(
        json!({"format":"minecraft-sdk-reaction-latency-v1","attempted_reactions":ids.len(),
        "physical_upper_bound":stats(physical,1_000_000.),"acknowledgement":stats(acknowledgements,1.),
        "missing_or_unverified_effects":missing,"all_attempts_have_verified_timing":missing.is_empty(),"samples":samples,
        "missing_acknowledgements":missing_acknowledgements,
        "trace_sha256":sha256(trace),"native_report_canonical_sha256":sha256(&serde_json::to_vec_pretty(report)?),
        "method":"Same-JVM monotonic observation sample through first verified submitted-frame pose. Sampling precedes local observation receipt, so this conservatively bounds physical reaction. HTTP acknowledgement is reported separately. Missing effects must not be discarded when evaluating a limit.",
        "authority":"Native effects are operator-captured. Controller sample clocks and source/build binding remain operator-attested through the pinned trace; this is not a cryptographic attestation of client observation delivery, survival or competence."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clocks_require_verified_same_epoch_and_exact_integer_strings() {
        let sample = json!({"epoch":"one","nanos":(u64::MAX-10).to_string()});
        let mut effect = json!({"verified":true,"sample_clock":{"epoch":"one","nanos":"5"}});
        assert_eq!(bound(&sample, &effect), Some(16));
        effect["sample_clock"]["epoch"] = json!("restarted");
        assert_eq!(bound(&sample, &effect), None);
        effect["sample_clock"]["epoch"] = json!("one");
        effect["verified"] = json!(false);
        assert_eq!(bound(&sample, &effect), None);
    }
    #[test]
    fn nearest_rank_preserves_a_single_slow_sample_in_a_small_trial() {
        let mut values = vec![100; 12];
        values.push(526);
        assert_eq!(stats(values, 1.)["p95_ms"], 526.);
        assert!(stats(Vec::new(), 1.)["p95_ms"].is_null());
    }
    #[test]
    fn missing_effects_stay_in_the_denominator_and_wrong_trace_binding_fails() {
        let trace = concat!(r#"{"event":"brain_started","data":{"uuid":"own","instance":"instance"}}"#, "\n",
            r#"{"event":"body_reaction","data":{"action_id":"move","reaction_ms":789,"sample_clock":{"epoch":"clock","nanos":"100"}}}"#).as_bytes();
        let mut report = json!({"format":"minecraft-sdk-native-run-evidence-v1",
            "spec":{"trace_sha256":sha256(trace),"player_uuid":"own","instance_id":"instance"},
            "native_actions":[{"action":{"kind":"start_move"},"receipt":{"action_id":"move","evidence":{"before":{"player":{"uuid":"own"}}}}}]});
        let result = summarize(trace, &report).unwrap();
        assert_eq!(result["attempted_reactions"], 1);
        assert_eq!(result["physical_upper_bound"]["count"], 0);
        assert_eq!(result["acknowledgement"]["max_ms"], 789.);
        assert_eq!(result["missing_or_unverified_effects"], json!(["move"]));
        assert_eq!(result["all_attempts_have_verified_timing"], false);
        report["spec"]["player_uuid"] = json!("other");
        assert!(summarize(trace, &report).is_err());
        report["spec"]["player_uuid"] = json!("own");
        report["spec"]["trace_sha256"] = json!("wrong");
        assert!(summarize(trace, &report).is_err());
    }
}
