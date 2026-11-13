//! Operator fixture for an exclusively assigned, already-connected character.
//! Supplies no items, opens no menus and never restarts shared services.
use minecraft_sdk::{
    menu::{self, CraftStatus},
    *,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

type AnyResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Material {
    item: String,
    minimum: u64,
}
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Batch {
    recipe: String,
    result: String,
    count: u64,
}
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    prefix: String,
    materials: Vec<Material>,
    batches: Vec<Batch>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    plan: Plan,
    instance: String,
    actor: String,
    life: String,
    before: Observation,
}
fn private_directory(path: &Path) -> AnyResult<()> {
    match fs::create_dir(path) {
        Ok(()) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("Expected a private fixture directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err("Fixture directory must be mode 700".into());
        }
    }
    Ok(())
}
fn save(path: &Path, value: &impl Serialize) -> AnyResult<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options.open(path)?;
    f.write_all(&serde_json::to_vec_pretty(value)?)?;
    f.sync_all()?;
    Ok(())
}
fn same_life(o: &Observation, b: &Binding) -> AnyResult<()> {
    if o.player["uuid"] != b.actor
        || o.player["life_id"] != b.life
        || o.player["health"].as_f64().is_none_or(|h| h <= 0.)
    {
        return Err("Fixture character died or changed life; preserve journals and inspect".into());
    }
    Ok(())
}
fn journal_pending(path: &Path) -> AnyResult<Option<Value>> {
    match fs::read(path) {
        Ok(bytes) => {
            let state: Value = serde_json::from_slice(&bytes)?;
            Ok(state.get("pending").filter(|v| !v.is_null()).cloned())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
#[tokio::main]
async fn main() -> AnyResult<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 4 || !matches!(args[3].as_str(), "run" | "interrupt") {
        return Err(
            "Usage: craft_conformance CONNECTED_PROFILE PLAN.json PRIVATE_DIRECTORY run|interrupt"
                .into(),
        );
    }
    let profile: Profile = serde_json::from_slice(&fs::read(&args[0])?)?;
    if profile.session.is_none() {
        return Err("An already-connected exclusive fixture session is required".into());
    }
    let plan: Plan = serde_json::from_slice(&fs::read(&args[1])?)?;
    if !valid_id(&plan.prefix)
        || plan.prefix.len() > 72
        || plan.batches.len() > 32
        || plan
            .batches
            .iter()
            .map(|b| &b.recipe)
            .collect::<BTreeSet<_>>()
            .len()
            < 3
        || plan
            .materials
            .iter()
            .map(|m| &m.item)
            .collect::<BTreeSet<_>>()
            .len()
            < 2
        || plan.materials.len() > 32
        || plan
            .materials
            .iter()
            .any(|m| !registry_id(&m.item) || m.minimum == 0)
        || plan
            .batches
            .iter()
            .any(|b| !registry_id(&b.recipe) || !registry_id(&b.result) || b.count == 0)
    {
        return Err("Plan requires 3 distinct recipes, 2 distinct input materials, positive counts and a bounded unique prefix".into());
    }
    let directory = PathBuf::from(&args[2]);
    private_directory(&directory)?;
    let report_path = directory.join(if args[3] == "interrupt" {
        "interrupted.json"
    } else {
        "completed.json"
    });
    if fs::symlink_metadata(&report_path).is_ok() {
        return Err("Existing fixture report; inspect it before any further action".into());
    }
    let client = Client::with_profile(&profile)?;
    client.verify_instance(&profile.instance_id).await?;
    let current = client.observe().await?;
    let binding_path = directory.join("binding.json");
    let binding: Binding = if binding_path.exists() {
        let b: Binding = serde_json::from_slice(&fs::read(&binding_path)?)?;
        if b.plan != plan
            || b.instance != profile.instance_id
            || b.actor != profile.credential.player_uuid
        {
            return Err(
                "Fixture plan, instance or actor changed; preserve original journals".into(),
            );
        }
        b
    } else {
        for material in &plan.materials {
            if menu::item_count(&current, &material.item)? < material.minimum {
                return Err("Declared operator input material minimum missing".into());
            }
        }
        let life = current.player["life_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("Native life identity required")?
            .to_owned();
        let b = Binding {
            plan,
            instance: profile.instance_id.clone(),
            actor: profile.credential.player_uuid.clone(),
            life,
            before: current.clone(),
        };
        same_life(&current, &b)?;
        save(&binding_path, &b)?;
        b
    };
    same_life(&current, &binding)?;
    let mut reports = Vec::new();
    for (index, batch) in binding.plan.batches.iter().enumerate() {
        let prefix = format!("{}-{index}", binding.plan.prefix);
        let path = directory.join(format!("batch-{index}.json"));
        let mut future = Box::pin(menu::craft_once(
            &client,
            &batch.recipe,
            &prefix,
            &path,
            Duration::from_secs(15),
        ));
        let result = if args[3] == "interrupt" {
            loop {
                tokio::select! {
                    result = &mut future => break result?,
                    _ = tokio::time::sleep(Duration::from_millis(1)) => {
                        if let Some(request)=journal_pending(&path)? {
                            let id=request["action_id"].as_str().ok_or("Pending journal action identity missing")?;
                            let Ok(receipt)=client.action_status(id).await else { continue; };
                            if receipt.action_id!=id { return Err("Pending receipt identity changed".into()); }
                            drop(future);
                            let after=client.observe().await?;
                            save(&report_path,&json!({"kind":"operator_fixture_client_future_interrupted","batch":index,"original_request":request,"original_status":receipt,"prior_reports":reports,"after":after,"note":"Original request is recorded by the gateway and may already be terminal. Resume same plan/directory; never replay or invent replacement IDs. This observation does not establish interruption of native execution."}))?;
                            println!("Interrupted the client future after durable local and gateway request persistence; resume this exact plan and directory.");
                            return Ok(());
                        }
                    }
                }
            }
        } else {
            future.await?
        };
        let completed = matches!(
            result.status,
            CraftStatus::Completed | CraftStatus::AlreadyCompleted
        );
        let expected = result.item.as_deref() == Some(batch.result.as_str())
            && result.output_count == Some(batch.count);
        reports.push(result);
        if !completed || !expected {
            let after = client.observe().await?;
            save(
                &directory.join(format!("stopped-{index}.json")),
                &json!({"kind":"operator_fixture_stopped","reports":reports,"after":after}),
            )?;
            return Err(
                "Fixture stopped; inspect original IDs and journals before continuing".into(),
            );
        }
    }
    if args[3] == "interrupt" {
        return Err("All batches settled before interruption was observed; no interruption claim, preserve completed journals".into());
    }
    let after = client.observe().await?;
    same_life(&after, &binding)?;
    save(
        &report_path,
        &json!({"kind":"operator_fixture_recipe_conformance","reports":reports,"after":after,"operator_supplied_inputs":true,"autonomous_acquisition":false,"gameplay_competence_established":false}),
    )?;
    println!(
        "Completed fixture recipe batches; keep the private report and capture authoritative action rows separately."
    );
    Ok(())
}
