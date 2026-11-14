//! Declared native execution-independence diagnostic, not a competence benchmark.
use minecraft_sdk_brain::{Brain, Config, Result, ensure, private_read, tools};
use serde_json::json;
use std::{
    path::Path,
    time::{Duration, Instant},
};
mod probe_admission;
mod probe_continuity;
mod probe_workload;
mod shore_probe;
use probe_workload::Mode;
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() == 2 && args[0] == "navigation-worker" {
        return minecraft_sdk_brain::navigation::worker(Path::new(&args[1])).await;
    }
    if args.len() == 2 && args[0] == "craft-worker" {
        return minecraft_sdk_brain::crafting::worker(Path::new(&args[1])).await;
    }
    if args.len() == 2 && args[0] == "inspect-observation" {
        let bytes = private_read(Path::new(&args[1]), 1024 * 1024)?;
        let observed: minecraft_sdk::Observation = serde_json::from_slice(&bytes)?;
        let policy_started = Instant::now();
        let geometry = minecraft_sdk_brain::body::geometry::evade(&observed)
            .map(|route| json!({"target":route.target,"jump":route.jump,"reason":route.reason}));
        let threatened = minecraft_sdk_brain::body::geometry::threatened(&observed);
        let incoming = minecraft_sdk_brain::body::geometry::incoming(&observed);
        let policy_ms = policy_started.elapsed().as_secs_f64() * 1000.;
        println!(
            "{}",
            json!({"event":"body_observation_diagnostic","scope":"offline copied observation; no connection or gameplay mutation","observation_sha256":minecraft_sdk_brain::digest(&bytes),"revision":observed.revision,"position":observed.player["position"],"health":observed.player["health"],"threatened":threatened,"incoming_projectile":incoming,"escape":geometry,"policy_ms":policy_ms})
        );
        return Ok(());
    }
    if args.len() == 2 && args[0] == "replay-decision" {
        let bytes = private_read(
            Path::new(&args[1]),
            minecraft_sdk_brain::body::decision::MAX_BYTES as u64,
        )?;
        let snapshot: minecraft_sdk_brain::body::decision::Snapshot =
            serde_json::from_slice(&bytes)?;
        println!("{}", snapshot.replay());
        return Ok(());
    }
    let mode = args.get(1).and_then(|value| Mode::parse(value))
        .filter(|mode| mode.accepts_arity(args.len()))
        .ok_or("body-probe PRIVATE_CONFIG.json blocked|compile|exposed|task|task-exposed SECONDS [PRIVATE_STIMULUS.json] | body-probe PRIVATE_CONFIG.json task-kit 1200 PRIVATE_STIMULUS.json | body-probe PRIVATE_CONFIG.json navigate SECONDS GOAL.json | body-probe PRIVATE_CONFIG.json shore SECONDS PRIVATE_SPEC.json | body-probe inspect-observation PRIVATE_OBSERVATION.json")?;
    let seconds: u64 = args[2].parse()?;
    if !mode.accepts_seconds(seconds) {
        return Err(
            "Diagnostic duration must be1–600 seconds; task-kit requires exactly1200".into(),
        );
    }
    let mut cfg: Config = serde_json::from_slice(&private_read(Path::new(&args[0]), 16384)?)?;
    if mode == Mode::TaskKit {
        ensure(
            serde_json::to_value(&cfg.completion_inventory)?
                == json!({"minecraft:crafting_table":1,"minecraft:wooden_pickaxe":1})
                && !cfg.goal.trim().is_empty(),
            "task-kit requires an explicit retained table/pickaxe goal contract",
        )?;
    }
    let shore = (args[1] == "shore")
        .then(|| shore_probe::Spec::read(Path::new(&args[3])))
        .transpose()?;
    let admission = if args.len() == 4 && !matches!(args[1].as_str(), "navigate" | "shore") {
        Some(probe_admission::Spec::read(Path::new(&args[3]))?)
    } else {
        None
    };
    // Exposed is a positive pressure control: the same ordinary character and
    // admission path, deliberately without the independent protective body.
    let enable_body = mode.body_enabled();
    cfg.body.enabled = enable_body && admission.is_none();
    cfg.conversation = false;
    cfg.await_chat_goal = false;
    let mut brain = Brain::open(cfg).await?;
    let mut continuity = None;
    let mut final_scene = None;
    let mut task_outcome = None;
    let run=async {
        let mut admitted_at = Instant::now();
        if let Some(spec)=&admission {
            brain.observed=probe_admission::wait(&mut brain,spec).await?;
            admitted_at = Instant::now();
            brain.cfg.body.enabled=enable_body;
            if enable_body {
                let profile: minecraft_sdk::Profile=serde_json::from_slice(&private_read(&brain.cfg.profile,16384)?)?;
                brain.body=Some(minecraft_sdk_brain::body::Handle::start_for_server(profile,brain.cfg.body.clone(),&brain.cfg.state,brain.observed.clone(),&brain.server)?);
            }
        }
        continuity=Some(probe_continuity::Monitor::start(&brain, mode == Mode::TaskExposed).await?);
        let started = if mode == Mode::TaskKit {admitted_at}else{Instant::now()};
        let initial_scene = brain.client.observe().await?;
        brain.journal.event("body_probe_scene", json!({"phase":"after_body_startup",
            "scope":"Full SDK observation; body may already be acting. Optional stimulus admission is the earlier start-state witness.",
            "observation":initial_scene}))?;
        brain.journal.event("body_probe_started",json!({"utc_ms":probe_continuity::utc_ms()?,"mode":args[1],"seconds":seconds,"models_called":mode.models_called(),"body_enabled":brain.body.is_some(),"stimulus_admission":admission.is_some(),"interpretation":"Blocked mode injects a stalled reasoning future, not a provider request. Compile mode runs the configured compiler command, not proof of useful coding. Task modes run the existing Brain::run with the operator goal; exact model counts, useful output and real compiler overlap require original journals/provider evidence. Exposed modes deliberately omit the body, never an agent success."}))?;
        if let Some(spec)=&shore {
            shore_probe::run(&mut brain,spec,seconds).await?;
        } else if mode.is_task() {
            task_outcome = Some(probe_workload::run(&mut brain,
                Duration::from_secs(seconds).saturating_sub(started.elapsed())).await?);
        } else if args[1]=="compile" {
            let compiler_started=Instant::now();
            let result=tools::terminal(&mut brain,"cargo build --offline --message-format=json").await?;
            brain.journal.event("body_probe_compiler",json!({"result":result,"elapsed_ms":compiler_started.elapsed().as_millis(),"success_is_not_survival_proof":true}))?;
            // A failed compiler is also a required resilience case. Preserve its
            // error and keep the body running for the declared trial duration.
        } else if args[1]=="navigate" {
            let goal=private_read(Path::new(&args[3]),10000)?;
            let goal=std::str::from_utf8(&goal)?;
            // Prime the real adapter's read interval, retaining its exact
            // response. The SDK must handle the next transient page itself.
            for _ in 0..2 {
                let page=brain.client.read_observation(&minecraft_sdk::ObservationQuery {
                    capability:"minecraft:movement_options".into(),
                    input:json!({"limit":1,"offset":0}),
                }).await?;
                brain.journal.event("body_probe_movement_preview",json!({"response":page,
                    "scope":"Unmodified adapter response; read-only interval priming, not a generated plan"}))?;
            }
            let result=minecraft_sdk_brain::navigation::execute(&mut brain,goal).await?;
            brain.journal.event("body_probe_navigation",json!({"result":result,
                "scope":"Operator-declared traversal diagnostic, not autonomous competence or hostile survival"}))?;
        } else if args[1]=="exposed" {
            let deadline=tokio::time::Instant::from_std(started+Duration::from_secs(seconds));
            let mut cadence=tokio::time::interval(Duration::from_millis(500));
            cadence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    _=tokio::time::sleep_until(deadline)=>break,
                    _=cadence.tick()=>{}
                }
                let Ok(observed)=tokio::time::timeout_at(deadline,brain.client.observe()).await else { break; };
                let observed=observed?;
                if let Some(monitor)=&continuity { monitor.sample(&observed)?; }
                brain.journal.event("exposed_native_sample",json!({"elapsed_ms":started.elapsed().as_millis(),
                    "revision":observed.revision,"sample_clock":observed.world["sample_clock"],
                    "health":observed.player["health"],"food":observed.player["food"],
                    "position":observed.player["position"],"body_enabled":false,"models_called":0}))?;
            }
        } else {
            // Same caller runtime can remain blocked while the independent body
            // and local gameplay arbiter keep receiving real SDK observations.
            tokio::time::sleep(Duration::from_secs(seconds)).await;
        }
        if shore.is_none() && mode != Mode::TaskKit && started.elapsed()<Duration::from_secs(seconds) {tokio::time::sleep(Duration::from_secs(seconds)-started.elapsed()).await;}
        let observed=brain.client.observe().await?;
        if mode == Mode::TaskKit {
            let table = minecraft_sdk::menu::item_count(&observed,"minecraft:crafting_table")?;
            let pick = minecraft_sdk::menu::item_count(&observed,"minecraft:wooden_pickaxe")?;
            brain.journal.event("body_probe_kit_final",json!({"retained_crafting_table":table,"retained_wooden_pickaxe":pick,
                "observation":observed,"scope":"Final native observation before body stop; external saved-state, original-life and real-attack evidence still required. No benchmark verdict."}))?;
            ensure(table >= 1 && pick >= 1,"task-kit ended without retained table and wooden pickaxe")?;
        }
        final_scene=Some(observed.clone());
        brain.journal.event("body_probe_finished",json!({"utc_ms":probe_continuity::utc_ms()?,"elapsed_ms":started.elapsed().as_millis(),"health":observed.player["health"],"food":observed.player["food"],"position":observed.player["position"],"revision":observed.revision,"body":brain.body.as_ref().map(|b|b.shared.view()),"models_called":mode.models_called(),"task_workload":task_outcome,"ordinary_respawn_is_not_survival_success":true}))?;
        brain.journal.event("body_probe_scene",json!({"phase":"before_body_stop",
            "scope":"Full SDK observation; final session disconnect and saved player state follow separately",
            "observation":observed}))?;
        Ok::<_,Box<dyn std::error::Error+Send+Sync>>(())
    }.await;
    let continuity = match continuity {
        Some(monitor) => Some(monitor.finish(final_scene, &brain.client).await),
        None => None,
    };
    let body_stopped = if let Some(body) = brain.body.take() {
        body.stop().await
    } else {
        Ok(())
    };
    let disconnected = brain.client.disconnect().await;
    brain.journal.event("body_probe_exit",json!({"disconnected":disconnected.is_ok(),"error":run.as_ref().err().map(ToString::to_string),"body_stop_error":body_stopped.as_ref().err().map(ToString::to_string),"disconnect_error":disconnected.as_ref().err().map(ToString::to_string),"sampled_continuity":continuity.as_ref().and_then(|r|r.as_ref().ok()),"continuity_error":continuity.as_ref().and_then(|r|r.as_ref().err()).map(ToString::to_string),"cleanup_interval":"outside sampled interval; independently verify saved death statistics"}))?;
    run?;
    body_stopped?;
    disconnected?;
    let summary = continuity.ok_or("Probe continuity evidence unavailable")??;
    ensure(
        summary.no_observed_lifecycle_failure(),
        "Probe observed a death or identity-continuity failure; inspect continuity journal, sample coverage and independent death statistics",
    )?;
    if let Some(outcome) = task_outcome {
        ensure(
            outcome.workload_completed && !outcome.timed_out,
            "Operator task failed or timed out; inspect body_probe_task_finished and original model/tool journals",
        )?;
    }
    Ok(())
}
