use super::*;
use minecraft_sdk::{ActionRequest, Receipt};

fn require(ok: bool, message: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(message.to_owned().into())
    }
}
async fn submit(brain: &Brain, request: &ActionRequest) -> Result<Receipt> {
    brain
        .journal
        .event("finite_control_intent", serde_json::to_value(request)?)?;
    let r = brain.client.act(request).await?;
    brain
        .journal
        .event("finite_control_receipt", serde_json::to_value(&r)?)?;
    Ok(r)
}
async fn start(brain: &Brain, label: &str, jump: bool, ticks: u16) -> Result<ActionRequest> {
    let observed = brain.client.observe().await?;
    let p = waypoint(&observed)?;
    let request = ActionRequest {
        action_id: format!("finite-{}-{label}", brain.run_id),
        action: Action::StartMove {
            target: Position {
                x: p[0],
                y: p[1],
                z: p[2],
            },
            jump,
            max_ticks: ticks,
        },
    };
    require(
        submit(brain, &request).await?.outcome == Outcome::Running,
        "Control must initially report running",
    )?;
    Ok(request)
}
async fn finish(brain: &Brain, id: &str) -> Result<Receipt> {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let r = brain.client.action_status(id).await?;
        if r.outcome != Outcome::Running {
            brain
                .journal
                .event("finite_control_final", serde_json::to_value(&r)?)?;
            return Ok(r);
        }
        require(
            Instant::now() < deadline,
            "Finite control exceeded diagnostic deadline",
        )?;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
async fn cancel(brain: &Brain, request: &ActionRequest, label: &str) -> Result<()> {
    let id = format!("finite-{}-{label}", brain.run_id);
    submit(
        brain,
        &ActionRequest {
            action_id: id.clone(),
            action: Action::CancelControl {
                action_id: request.action_id.clone(),
            },
        },
    )
    .await?;
    let original = finish(brain, &request.action_id).await?;
    require(
        original.outcome == Outcome::Cancelled,
        "Original control must report cancelled",
    )?;
    let result = finish(brain, &id).await?;
    require(
        result.outcome == Outcome::Observed && result.evidence["native_effect_verified"] == true,
        "Cancellation release must be natively verified",
    )?;
    let before = brain.client.observe().await?;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let after = brain.client.observe().await?;
    require(
        position(&before)?
            .iter()
            .zip(position(&after)?)
            .all(|(a, b)| (*a - b).abs() < 0.08),
        "Movement continued after cancellation",
    )?;
    brain.journal.event("finite_control_release_observed",json!({"action_id":id,"before":before.player["position"],"after":after.player["position"],"before_revision":before.revision,"after_revision":after.revision}))?;
    Ok(())
}
pub async fn run(brain: &mut Brain) -> Result<()> {
    let info = brain.client.discover().await?;
    require(
        info.capabilities
            .iter()
            .any(|c| c == "minecraft:finite_locomotion"),
        "Finite locomotion capability is not installed",
    )?;
    let mut timings = Vec::new();
    for trial in 0..5 {
        let req = start(brain, &format!("walk-{trial}"), false, 100).await?;
        let duplicate = brain.client.act(&req).await?;
        require(
            duplicate.action_id == req.action_id,
            "Duplicate ID was not retained",
        )?;
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut looked = false;
        loop {
            let status = brain.client.action_status(&req.action_id).await?;
            if status.outcome != Outcome::Running {
                brain
                    .journal
                    .event("finite_control_final", serde_json::to_value(&status)?)?;
                require(
                    status.outcome == Outcome::Observed
                        && status.evidence["native_effect_verified"] == true,
                    "Finite walking target was not observed",
                )?;
                break;
            }
            let sent = Instant::now();
            let observed = brain.client.observe().await?;
            let ms = sent.elapsed().as_millis();
            timings.push(ms);
            brain.journal.event("finite_control_observation",json!({"action_id":req.action_id,"trial":trial,"latency_ms":ms,"position":observed.player["position"],"revision":observed.revision}))?;
            if trial == 0 && !looked {
                let yaw = observed.player["yaw"].as_f64().ok_or("Yaw missing")? as f32 + 5.;
                let r = submit(
                    brain,
                    &ActionRequest {
                        action_id: format!("finite-{}-look", brain.run_id),
                        action: Action::Look { yaw, pitch: 0. },
                    },
                )
                .await?;
                require(
                    r.outcome == Outcome::Observed,
                    "Look during locomotion was not observed",
                )?;
                looked = true;
            }
            require(Instant::now() < deadline, "Walking control did not finish")?;
        }
        if trial == 0 {
            require(looked, "No look action ran concurrently with locomotion")?;
        }
    }
    let req = start(brain, "cancel-ground", false, 100).await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel(brain, &req, "cancel-ground-request").await?;
    let req = start(brain, "expiry", false, 2).await?;
    require(
        finish(brain, &req.action_id).await?.outcome == Outcome::Expired,
        "Short lease did not expire normally",
    )?;
    let req = start(brain, "jump", true, 100).await?;
    require(
        finish(brain, &req.action_id).await?.outcome == Outcome::Observed,
        "Finite jump was not observed",
    )?;
    let req = start(brain, "cancel-jump", true, 100).await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel(brain, &req, "cancel-jump-request").await?;
    let req = start(brain, "disconnect", false, 100).await?;
    brain.client.disconnect().await?;
    let mut profile: Profile = serde_json::from_slice(&private_read(&brain.cfg.profile, 16384)?)?;
    profile.session = None;
    let mut client = Client::with_profile(&profile)?;
    client.verify_instance(&profile.instance_id).await?;
    profile.session = Some(
        client
            .connect(&format!("finite-reconnect-{}", brain.run_id))
            .await?,
    );
    minecraft_sdk_brain::private_write(&brain.cfg.profile, &serde_json::to_vec(&profile)?)?;
    client.set_session(profile.session.clone());
    brain.client = client;
    let interrupted = brain.client.action_status(&req.action_id).await?;
    brain.journal.event(
        "finite_interruption_status",
        serde_json::to_value(&interrupted)?,
    )?;
    require(
        interrupted.outcome == Outcome::Uncertain,
        "Interrupted control was not fenced",
    )?;
    let reconciled = brain.client.reconcile(&req.action_id).await?;
    brain.journal.event(
        "finite_interruption_reconciled",
        serde_json::to_value(&reconciled)?,
    )?;
    require(
        reconciled.outcome == Outcome::Reconciled && reconciled.evidence["replayed"] == false,
        "Interrupted control did not reconcile without replay",
    )?;
    let duplicate = brain.client.act(&req).await?;
    require(
        duplicate.outcome == Outcome::Reconciled,
        "Interrupted action ID was dispatched again",
    )?;
    require(
        !timings.is_empty(),
        "No concurrent observations were recorded",
    )?;
    timings.sort_unstable();
    let p95 = timings[(timings.len() * 95).div_ceil(100) - 1];
    let max = *timings.last().unwrap();
    brain.journal.event("finite_control_probe_result",json!({"models_called":0,"concurrent_observations":timings.len(),"p95_ms":p95,"max_ms":max,"walking_trials":5,"cancelled_ground_and_jump":true,"expiry":true,"disconnect_reconciliation_without_replay":true,"interpretation":"Finite control conformance only; not defensive Survival or multiplayer permission proof"}))?;
    require(
        p95 <= 500 && max <= 1000,
        "Finite control observation latency exceeded the initial boundary",
    )?;
    Ok(())
}
