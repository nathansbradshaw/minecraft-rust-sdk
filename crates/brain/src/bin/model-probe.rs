//! Allocated-model integration probe. No SDK connection or tool execution.
use minecraft_sdk_brain::{Result, model::Model};
use serde_json::json;
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 4 || !["routine", "develop"].contains(&args[2].as_str()) || args[3].len() > 128
    {
        return Err("model-probe HTTPS_ENDPOINT ALLOCATED_KEY routine|develop LABEL".into());
    }
    let model = Model::new(&args[0], std::path::Path::new(&args[1]))?;
    let start = minecraft_sdk_brain::now();
    let result=model.choose(&json!({"goal":"Inference integration probe only: choose observe with empty input and path. No returned tool will execute; no game state or gameplay task exists.","probe_label":args[3]}),args[2]=="develop").await;
    match result {
        Ok((choice, usage)) => println!(
            "{}",
            json!({"label":args[3],"started_at_utc":start,"finished_at_utc":minecraft_sdk_brain::now(),"choice":choice,"usage":usage,"tools_executed":false,"gameplay_actions":0})
        ),
        Err(error) => {
            println!(
                "{}",
                json!({"label":args[3],"started_at_utc":start,"finished_at_utc":minecraft_sdk_brain::now(),"error":error.to_string(),"retry_at_utc":minecraft_sdk_brain::model::retry_at(error.as_ref()),"tools_executed":false,"gameplay_actions":0})
            );
            std::process::exit(2);
        }
    }
    Ok(())
}
