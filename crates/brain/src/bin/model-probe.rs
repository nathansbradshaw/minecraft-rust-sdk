//! Allocated-model integration probe. No SDK connection or tool execution.
use minecraft_sdk_brain::{Result, model::Model};
use serde_json::json;
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() == 3 && matches!(args[0].as_str(), "watch" | "idle") {
        let url = reqwest::Url::parse(&args[1])?;
        let samples: usize = args[2].parse()?;
        if url.scheme() != "http"
            || url
                .host_str()
                .and_then(|s| s.parse::<std::net::IpAddr>().ok())
                .is_none_or(|ip| !ip.is_loopback())
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !(1..=240).contains(&samples)
        {
            return Err("watch requires an operator loopback health URL and1..240samples".into());
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(2))
            .build()?;
        let mut maximum = 0;
        for _ in 0..samples {
            let mut response = client.get(url.clone()).send().await?.error_for_status()?;
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                if bytes.len() + chunk.len() > 1024 {
                    return Err("Upstream health response exceeds budget".into());
                }
                bytes.extend(chunk);
            }
            let health: serde_json::Value = serde_json::from_slice(&bytes)?;
            let busy = health["busy"]
                .as_u64()
                .ok_or("Upstream active-job count missing")?;
            maximum = maximum.max(busy);
            println!(
                "{}",
                json!({"event":"upstream_sample","at_utc_ms":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis(),"busy":busy})
            );
            if busy > 2 {
                return Err("Observed upstream concurrency exceeded two".into());
            }
            if args[0] == "idle" && busy == 0 {
                println!(
                    "{}",
                    json!({"event":"upstream_idle","tools_executed":false,"model_calls":0})
                );
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        if args[0] == "idle" {
            return Err("No idle inference boundary within the bounded observation window".into());
        }
        println!(
            "{}",
            json!({"event":"upstream_watch_finished","samples":samples,"maximum_busy":maximum,"tools_executed":false})
        );
        return Ok(());
    }
    if args.len() == 5 && args[2] == "conversation" && args[3].len() <= 128 {
        let context: serde_json::Value = serde_json::from_slice(
            &minecraft_sdk_brain::private_read(std::path::Path::new(&args[4]), 32768)?,
        )?;
        let model = Model::new(&args[0], std::path::Path::new(&args[1]))?;
        let (reply, usage) = model.converse(&context).await?;
        println!(
            "{}",
            json!({"label":args[3],"reply":reply,"usage":usage,"tools_executed":false,"gameplay_actions":0,"evidence":"conversation-only replay; not controller or gameplay success"})
        );
        return Ok(());
    }
    if !(4..=5).contains(&args.len())
        || !["routine", "develop", "investigate"].contains(&args[2].as_str())
        || args[3].len() > 128
    {
        return Err("model-probe HTTPS_ENDPOINT ALLOCATED_KEY routine|develop|investigate LABEL [PRIVATE_CONTEXT.json] | model-probe HTTPS_ENDPOINT ALLOCATED_KEY conversation LABEL PRIVATE_CONTEXT.json".into());
    }
    let model = Model::new(&args[0], std::path::Path::new(&args[1]))?;
    let start = minecraft_sdk_brain::now();
    let context = if args.len() == 5 {
        serde_json::from_slice(&minecraft_sdk_brain::private_read(
            std::path::Path::new(&args[4]),
            32768,
        )?)?
    } else {
        json!({"goal":"Inference integration probe only: choose observe with empty input and path. No returned tool will execute; no game state or gameplay task exists.","probe_label":args[3]})
    };
    let result = model
        .choose_for(&context, args[2] == "develop", args[2] != "routine")
        .await;
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
