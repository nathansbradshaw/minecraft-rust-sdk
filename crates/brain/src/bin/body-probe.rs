//! Declared native execution-independence diagnostic, not a competence benchmark.
use minecraft_sdk_brain::{Brain, Config, Result, private_read, tools};
use serde_json::json;
use std::{
    path::Path,
    time::{Duration, Instant},
};
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() == 2 && args[0] == "inspect-observation" {
        let bytes = private_read(Path::new(&args[1]), 1024 * 1024)?;
        let observed: minecraft_sdk::Observation = serde_json::from_slice(&bytes)?;
        let geometry = minecraft_sdk_brain::body::geometry::evade(&observed)
            .map(|route| json!({"target":route.target,"jump":route.jump,"reason":route.reason}));
        println!(
            "{}",
            json!({"event":"body_observation_diagnostic","scope":"offline copied observation; no connection or gameplay mutation","observation_sha256":minecraft_sdk_brain::digest(&bytes),"revision":observed.revision,"position":observed.player["position"],"health":observed.player["health"],"threatened":minecraft_sdk_brain::body::geometry::threatened(&observed),"incoming_projectile":minecraft_sdk_brain::body::geometry::incoming(&observed),"escape":geometry})
        );
        return Ok(());
    }
    if args.len() != 3 || !matches!(args[1].as_str(), "blocked" | "compile") {
        return Err("body-probe PRIVATE_CONFIG.json blocked|compile SECONDS | body-probe inspect-observation PRIVATE_OBSERVATION.json".into());
    }
    let seconds: u64 = args[2].parse()?;
    if !(1..=600).contains(&seconds) {
        return Err("Diagnostic duration must be 1–600 seconds".into());
    }
    let mut cfg: Config = serde_json::from_slice(&private_read(Path::new(&args[0]), 16384)?)?;
    cfg.body.enabled = true;
    let mut brain = Brain::open(cfg).await?;
    let started = Instant::now();
    let initial_scene = brain.client.observe().await?;
    brain.journal.event("body_probe_scene", json!({"phase":"after_body_startup",
        "scope":"Full SDK observation for this diagnostic; body may already be acting. Not an earlier frozen-trial scene.",
        "observation":initial_scene}))?;
    brain.journal.event("body_probe_started",json!({"mode":args[1],"seconds":seconds,"models_called":0,"interpretation":"Blocked mode injects a stalled reasoning future, not a provider request. Compile mode runs the real Rust compiler."}))?;
    let run=async {
        if args[1]=="compile" {
            let compiler_started=Instant::now();
            let result=tools::terminal(&mut brain,"cargo build --offline --message-format=json").await?;
            brain.journal.event("body_probe_compiler",json!({"result":result,"elapsed_ms":compiler_started.elapsed().as_millis(),"success_is_not_survival_proof":true}))?;
            // A failed compiler is also a required resilience case. Preserve its
            // error and keep the body running for the declared trial duration.
        } else {
            // Same caller runtime can remain blocked while the independent body
            // and local gameplay arbiter keep receiving real SDK observations.
            tokio::time::sleep(Duration::from_secs(seconds)).await;
        }
        if started.elapsed()<Duration::from_secs(seconds) {tokio::time::sleep(Duration::from_secs(seconds)-started.elapsed()).await;}
        let observed=brain.client.observe().await?;
        brain.journal.event("body_probe_finished",json!({"elapsed_ms":started.elapsed().as_millis(),"health":observed.player["health"],"food":observed.player["food"],"position":observed.player["position"],"revision":observed.revision,"body":brain.body.as_ref().map(|b|b.shared.view()),"models_called":0,"ordinary_respawn_is_not_survival_success":true}))?;
        brain.journal.event("body_probe_scene",json!({"phase":"before_body_stop",
            "scope":"Full SDK observation; final session disconnect and saved player state follow separately",
            "observation":observed}))?;
        Ok::<_,Box<dyn std::error::Error+Send+Sync>>(())
    }.await;
    if let Some(body) = brain.body.take() {
        body.stop().await?;
    }
    let disconnected = brain.client.disconnect().await;
    brain.journal.event("body_probe_exit",json!({"disconnected":disconnected.is_ok(),"error":run.as_ref().err().map(ToString::to_string)}))?;
    run?;
    disconnected?;
    Ok(())
}
