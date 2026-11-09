//! Explicit fault-injection worker for an operator-owned evaluation character.
//! Seven fixture choices are not inference or autonomous gameplay successes.
use minecraft_sdk_brain::{Result, model, private_read, private_write};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u32,
    request_id: String,
    context: Value,
    endpoint: String,
    key_file: PathBuf,
    use_sol: bool,
    development: bool,
}
fn choice(tool: &str, input: Value, summary: &str) -> model::Choice {
    model::Choice {
        tool: tool.into(),
        input: if input.is_null() {
            String::new()
        } else {
            input.to_string()
        },
        path: String::new(),
        summary: summary.into(),
        notes: "Declared diagnostic fixture; no model inference for this choice.".into(),
        experiment: false,
    }
}
fn fixture(r: &Request, file: &Path) -> Result<Option<model::Choice>> {
    let turn = r.context["turn"]
        .as_u64()
        .ok_or("Fixture requires a bounded controller turn")?;
    if turn == 0 {
        if file.exists() {
            return Err(
                "Use a fresh diagnostic workspace; fixture is not a production worker".into(),
            );
        }
        let p = &r.context["observation"]["player"];
        let inventory = p["inventory"]
            .as_array()
            .ok_or("Fixture requires inventory observations")?;
        let empty = (0..9)
            .find(|slot| {
                !inventory
                    .iter()
                    .any(|s| s["slot"] == *slot && s["count"].as_u64().unwrap_or(0) > 0)
            })
            .ok_or("Fixture needs an empty hotbar slot; never discards earned items")?;
        let rows = r.context["observation"]["world"]["blocks"]
            .as_array()
            .ok_or("Fixture requires observed terrain")?;
        let position = p["position"]
            .as_array()
            .ok_or("Fixture requires observed position")?;
        let block = rows
            .iter()
            .find(|b| {
                // Empty-hand dirt/grass clicks cannot strip logs, open containers or
                // place supplies. No coordinates, server address or task solution.
                if !matches!(b["block"].as_str(),Some("minecraft:dirt"|"minecraft:grass_block")) || b["may_interact"] != true {
                    return false;
                }
                let Some(q) = b["position"].as_array() else {
                    return false;
                };
                q.len() == 3
                    && position.len() == 3
                    && (0..3)
                        .map(|i| {
                            let d = q[i].as_f64().unwrap_or(f64::INFINITY) + 0.5
                                - position[i].as_f64().unwrap_or(f64::NEG_INFINITY)
                                - if i == 1 { 1.62 } else { 0. };
                            d * d
                        })
                        .sum::<f64>()
                        < 16.
            })
            .ok_or(
                "No nearby observed dirt/grass; fixture setup is ineligible, do not force a world repair",
            )?;
        let q = &block["position"];
        let action = json!({"kind":"place","block":{"x":q[0],"y":q[1],"z":q[2]},"face":1,"hand":"main","hit":[0.5,1.,0.5]});
        private_write(
            file,
            &serde_json::to_vec(
                &json!({"actor":p["uuid"],"goal":r.context["goal"],"slot":empty,"action":action}),
            )?,
        )?;
        return Ok(Some(choice(
            "sdk",
            json!({"kind":"select_slot","slot":empty}),
            "Fixture: select an empty hand without changing inventory",
        )));
    }
    if turn <= 6 {
        let saved: Value = serde_json::from_slice(&private_read(file, 4096)?)?;
        let p = &r.context["observation"]["player"];
        if saved["actor"] != p["uuid"] || saved["goal"] != r.context["goal"] {
            return Err("Fixture actor or task changed; no automatic restart".into());
        }
        if turn == 1 || turn == 6 {
            if p["selected"] != saved["slot"]
                || p["inventory"].as_array().is_none_or(|rows| {
                    rows.iter()
                        .any(|s| s["slot"] == saved["slot"] && s["count"].as_u64().unwrap_or(0) > 0)
                })
            {
                return Err(
                    "Fixture hand is not observed empty; do not send the failure probe".into(),
                );
            }
            if turn == 6
                && r.context["exhausted_boundary"]["exhausted_subject"]
                    .as_str()
                    .is_none()
            {
                return Err("No exhausted-boundary handoff observed; diagnostic did not exercise acceptance".into());
            }
            return Ok(Some(choice(
                "sdk",
                saved["action"].clone(),
                if turn == 1 {
                    "Fixture: one empty-hand placement must fail without world mutation"
                } else {
                    "Fixture: verify the exhausted identical intention is fenced before dispatch"
                },
            )));
        }
        return Ok(Some(choice(
            "observe",
            Value::Null,
            "Fixture: unchanged observation spends the existing bounded investigation",
        )));
    }
    Ok(None)
}
#[tokio::main]
async fn main() -> Result<()> {
    let mut bytes = Vec::new();
    std::io::stdin().take(65537).read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        return Err("Worker request too large".into());
    }
    let r: Request = serde_json::from_slice(&bytes)?;
    if r.version != 1 {
        return Err("Unsupported worker protocol".into());
    }
    let result = match fixture(&r, Path::new("./.handoff-fixture.json")) {
        Ok(Some(choice)) => Ok((
            choice,
            json!({"diagnostic_fixture":true,"inference_performed":false}),
        )),
        Ok(None) => {
            model::Model::new(&r.endpoint, &r.key_file)?
                .choose_for(&r.context, r.use_sol, r.development)
                .await
        }
        Err(error) => Err(error),
    };
    let response = match result {
        Ok(answer) => json!({"version":1,"request_id":r.request_id,"answer":answer,"error":null}),
        Err(error) => {
            json!({"version":1,"request_id":r.request_id,"answer":null,"error":{"message":error.to_string(),"retry_at":model::retry_at(error.as_ref()),"daily_reset":model::daily_reset(error.as_ref())}})
        }
    };
    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixture_is_bounded_requires_empty_hand_and_then_delegates_without_a_task_solution() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("fixture.json");
        let mut r = Request {
            version: 1,
            request_id: "fixture".into(),
            endpoint: "unused".into(),
            key_file: dir.path().join("unused.key"),
            use_sol: false,
            development: false,
            context: json!({"turn":0,"goal":"fixture","observation":{"player":{"uuid":"own","selected":0,"position":[0.5,2.,0.5],"inventory":[{"slot":0,"count":1,"item":"minecraft:crafting_table"}]},"world":{"blocks":[{"position":[0,0,0],"block":"minecraft:grass_block","may_interact":true}]}}}),
        };
        let setup = fixture(&r, &file).unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&setup.input).unwrap(),
            json!({"kind":"select_slot","slot":1})
        );
        assert!(fixture(&r, &file).is_err(), "no automatic fixture restart");
        r.context["observation"]["player"]["selected"] = json!(1);
        r.context["turn"] = json!(1);
        let failed = fixture(&r, &file).unwrap().unwrap();
        for turn in 2..=5 {
            r.context["turn"] = json!(turn);
            assert_eq!(fixture(&r, &file).unwrap().unwrap().tool, "observe");
        }
        r.context["turn"] = json!(6);
        assert!(fixture(&r, &file).is_err());
        r.context["exhausted_boundary"] = json!({"exhausted_subject":"fixture"});
        assert_eq!(fixture(&r, &file).unwrap().unwrap().input, failed.input);
        r.context["observation"]["player"]["inventory"] =
            json!([{"slot":1,"count":1,"item":"minecraft:dirt"}]);
        assert!(
            fixture(&r, &file).is_err(),
            "never accidentally place an earned item"
        );
        r.context["turn"] = json!(7);
        assert!(fixture(&r, &file).unwrap().is_none());
    }
}
