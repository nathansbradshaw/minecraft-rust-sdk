use minecraft_sdk_evidence::{Result, sha256};
use serde_json::{Value, json};
use sha2::Digest;
use std::{
    ffi::OsString,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
mod capture;
mod cleanup;
mod config;
mod io;
mod process;
mod rcon;
use config::{Fixture, Pinned};
use io::*;
use process::{Runner, SystemRunner};

pub fn run(args: Vec<OsString>) -> Result<()> {
    if args.first().is_some_and(|a| a == "rcon-list") {
        ensure(
            args.len() == 3,
            "Usage: mc-sdk-bench rcon-list LOOPBACK_ADDR PRIVATE_PASSWORD_FILE",
        )?;
        let address: std::net::SocketAddr = args[1]
            .to_str()
            .ok_or("invalid loopback address")?
            .parse()?;
        ensure(
            address.ip().is_loopback(),
            "rcon-list only accepts loopback",
        )?;
        let path = canonical(Path::new(&args[2]))?;
        let bytes = read(&path, 4096)?;
        let password = std::str::from_utf8(&bytes)?.trim();
        ensure(
            password.len() >= 16 && !password.contains(['\n', '\r']),
            "one private operator password required",
        )?;
        let mut client = rcon::RconClient::connect(address, password, Duration::from_secs(5))?;
        let reply = client.execute(&rcon::Command::ListPlayers)?;
        let safe = Secrets(vec![password.to_owned()]).scrub(reply.text.as_bytes());
        println!(
            "{}",
            json!({"command":"rcon-list","request_id":reply.request_id,"reply":String::from_utf8(safe)?,"gameplay_commands_executed":false})
        );
        return Ok(());
    }
    if args.is_empty() || (args.len() == 1 && args[0] == "--help") {
        println!(
            "mc-sdk-bench rcon-list LOOPBACK_ADDR PRIVATE_PASSWORD_FILE\nmc-sdk-bench preflight|stage|launch FIXTURE.json SHA256 CASE_DIR\nmc-sdk-bench nativecapture FIXTURE.json SHA256 NEW_CAPTURE_DIR GATEWAY.sqlite TRACE.log SPEC_TEMPLATE.json [SAVED_JSON ...]\nSee docs/operator-bench.md. Stage/launch make real connections only when the operator runs them; no command awards a PASS."
        );
        return Ok(());
    }
    ensure(
        args.len() >= 4,
        "Usage: mc-sdk-bench preflight|stage|launch FIXTURE.json SHA256 CASE_DIR | nativecapture FIXTURE.json SHA256 NEW_CAPTURE_DIR GATEWAY.sqlite TRACE.log SPEC_TEMPLATE.json [SAVED_JSON ...]",
    )?;
    let command = args[0].to_str().ok_or("command is not UTF-8")?;
    let fixture_path = Path::new(&args[1]);
    let hash = args[2].to_str().ok_or("hash is not UTF-8")?;
    let fixture = Fixture::load(fixture_path, hash)?;
    let case = Path::new(&args[3]);
    if !case.exists() {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(case)?;
    }
    directory(case)?;
    canonical(case)?;
    ensure(
        !case.starts_with(&fixture.workspace) && !case.starts_with(&fixture.secret_dir),
        "operator evidence must be outside bot workspace and secret mount",
    )?;
    ensure(
        free_bytes(case)? >= 500_000_000_000,
        "at least500GB free required",
    )?;
    let frozen = case.join("fixture.json");
    if frozen.exists() {
        ensure(
            sha256(&read(&frozen, 65536)?) == hash,
            "case already belongs to another frozen fixture",
        )?;
    } else {
        let mut file = create(&frozen)?;
        file.write_all(&read(fixture_path, 65536)?)?;
        file.sync_all()?;
    }
    let mut runner = SystemRunner::new();
    match command {
        "preflight" if args.len() == 4 => preflight(&mut runner, &fixture, case, "preflight")?,
        "stage" if args.len() == 4 => stage(&mut runner, &fixture, case, hash)?,
        "launch" if args.len() == 4 => {
            ensure(
                false,
                "launch disabled until owned remote finalization and archive integration are verified",
            )?;
            launch(&mut runner, &fixture, case, hash)?;
        }
        "nativecapture" if (7..=11).contains(&args.len()) => capture::native(
            &fixture,
            case,
            Path::new(&args[4]),
            Path::new(&args[5]),
            Path::new(&args[6]),
            &args[7..].iter().map(PathBuf::from).collect::<Vec<_>>(),
        )?,
        _ => return Err("unsupported command or argument count".into()),
    }
    println!(
        "{}",
        json!({"command":command,"case_id":fixture.case_id,"operator_evidence_written":true,"certifies_goal_completion":false})
    );
    Ok(())
}
fn preflight(runner: &mut dyn Runner, f: &Fixture, case: &Path, label: &str) -> Result<()> {
    f.validate()?;
    let format = "{\"running\":{{json .State.Running}},\"readonly_rootfs\":{{json .HostConfig.ReadonlyRootfs}},\"privileged\":{{json .HostConfig.Privileged}},\"user\":{{json .Config.User}},\"memory\":{{json .HostConfig.Memory}},\"nano_cpus\":{{json .HostConfig.NanoCpus}},\"pids\":{{json .HostConfig.PidsLimit}},\"cap_drop\":{{json .HostConfig.CapDrop}},\"cap_add\":{{json .HostConfig.CapAdd}},\"security\":{{json .HostConfig.SecurityOpt}},\"network\":{{json .HostConfig.NetworkMode}},\"pid_mode\":{{json .HostConfig.PidMode}},\"ipc_mode\":{{json .HostConfig.IpcMode}},\"devices\":{{json .HostConfig.Devices}},\"ca_env\":\"\",\"env_names\":[{{range $i,$e := .Config.Env}}{{if $i}},{{end}}{{json (index (split $e \"=\") 0)}}{{end}}],\"mounts\":{{json .Mounts}}}";
    let inspect = process::call(
        runner,
        f,
        &f.docker,
        vec![
            "inspect".into(),
            "--format".into(),
            format.into(),
            f.container.clone(),
        ],
        case,
        &format!("{label}-container"),
    )?;
    config::validate_mounts(f, &inspect)?;
    verify_container_artifacts(runner, f, case, label)?;
    save(
        &case.join(format!("{label}.json")),
        &json!({"case_id":f.case_id,"player_uuid":f.player_uuid,"npc_uuid":f.npc_uuid,"artifact_and_mount_checks":true,"free_bytes":free_bytes(case)?,"scoped_model_allocation":"operator-attested bot-scoped proxy; no provider account mounted","console_ready":f.operator_rcon.is_some(),"console_readiness_scope":"Configuration only, no connection or service restart during preflight","certifies_goal_completion":false}),
    )
}
fn verify_container_artifacts(
    runner: &mut dyn Runner,
    f: &Fixture,
    case: &Path,
    label: &str,
) -> Result<()> {
    for (index, artifact) in f.artifacts.iter().chain(f.ca_file.iter()).enumerate() {
        artifact.file.verify()?;
        let out = process::execute(
            runner,
            f,
            &f.docker,
            vec![
                "exec".into(),
                f.container.clone(),
                "sha256sum".into(),
                artifact.container_path.clone(),
            ],
            30,
            case,
            &format!("{label}-artifact-{index}"),
            &mut |_| Ok(()),
        )?;
        ensure(
            out.success
                && String::from_utf8_lossy(&out.stdout)
                    .split_whitespace()
                    .next()
                    == Some(&artifact.file.sha256),
            "container artifact differs",
        )?;
    }
    let mut paths = vec![f.container_profile()];
    if let Some(name) = &f.model_key_name {
        paths.push(format!("{}/{}", f.container_secret_dir, name));
    }
    for (index, path) in paths.into_iter().enumerate() {
        let out = process::execute(
            runner,
            f,
            &f.docker,
            vec![
                "exec".into(),
                f.container.clone(),
                "test".into(),
                "-r".into(),
                path,
            ],
            30,
            case,
            &format!("{label}-secret-readable-{index}"),
            &mut |_| Ok(()),
        )?;
        ensure(
            out.success,
            "scoped secret is not readable by container user; fix operator ownership mapping",
        )?;
    }
    Ok(())
}
fn sdk(
    runner: &mut dyn Runner,
    f: &Fixture,
    case: &Path,
    label: &str,
    operation: &str,
    tail: &[String],
) -> Result<Value> {
    let mut args = vec![operation.into(), f.profile().display().to_string()];
    args.extend_from_slice(tail);
    process::call(runner, f, &f.sdk, args, case, label)
}
fn rcon_client(f: &Fixture) -> Result<Option<rcon::RconClient>> {
    match &f.operator_rcon {
        None => Ok(None),
        Some(cfg) => {
            let bytes = read(&cfg.password_file, 4096)?;
            let password = std::str::from_utf8(&bytes)?.trim();
            ensure(
                password.len() >= 16 && !password.contains(['\n', '\r']),
                "one private operator password required",
            )?;
            Ok(Some(rcon::RconClient::connect(
                cfg.address,
                password,
                Duration::from_secs(5),
            )?))
        }
    }
}
fn fresh(runner: &mut dyn Runner, f: &Fixture, case: &Path, label: &str) -> Result<Value> {
    let query = case.join(format!("{label}-query.json"));
    save(
        &query,
        &json!({"capability":"minecraft:fresh_observation","input":{}}),
    )?;
    let value = sdk(
        runner,
        f,
        case,
        label,
        "query",
        &[query.display().to_string()],
    )?;
    ensure(
        value["format"] == "minecraft-fresh-observation-v1",
        "fresh snapshot capability/version required; no cached fallback",
    )?;
    Ok(value["observation"].clone())
}
trait Console {
    fn execute(&mut self, command: &rcon::Command)
    -> std::result::Result<rcon::Reply, rcon::Error>;
}
impl Console for rcon::RconClient {
    fn execute(
        &mut self,
        command: &rcon::Command,
    ) -> std::result::Result<rcon::Reply, rcon::Error> {
        rcon::RconClient::execute(self, command)
    }
}
fn rcon_execute(
    client: &mut dyn Console,
    command: rcon::Command,
    case: &Path,
    label: &str,
) -> Result<rcon::Reply> {
    match client.execute(&command) {
        Ok(reply) => {
            save(
                &case.join(format!("{label}.json")),
                &json!({"request_id":reply.request_id,"text":reply.text,"native_state_verified":false,"scope":"Command response only; SDK native state must independently verify its effect"}),
            )?;
            Ok(reply)
        }
        Err(error) => {
            save(
                &case.join(format!("{label}.json")),
                &json!({"error_code":error.code,"request_id":error.request_id,"may_have_executed":error.may_have_executed,"retry_performed":false}),
            )?;
            Err("operator command failed or uncertain; never retried".into())
        }
    }
}
fn stage(runner: &mut dyn Runner, f: &Fixture, case: &Path, hash: &str) -> Result<()> {
    preflight(runner, f, case, "stage-preflight")?;
    ensure(
        json(&f.profile(), 16384)?["session"].is_null(),
        "stage requires disconnected scoped profile",
    )?;
    // Durable intent precedes the one connection attempt. Re-running cannot allocate a new ID.
    save(
        &case.join("stage-intent.json"),
        &json!({"fixture_sha256":hash,"connection_request_id":f.connection_request_id,"player_uuid":f.player_uuid,"utc_ms":utc_ms()?}),
    )?;
    let result = (|| -> Result<()> {
        sdk(
            runner,
            f,
            case,
            "stage-connect",
            "connect",
            std::slice::from_ref(&f.connection_request_id),
        )?;
        let observed = fresh(runner, f, case, "stage-before")?;
        validate_player(f, &observed, false)?;
        let life = observed["player"]["life_id"]
            .as_str()
            .ok_or("life identity absent")?;
        let mut client = rcon_client(f)?;
        if let Some(client) = &mut client {
            rcon_execute(
                client,
                rcon::Command::StageFreshPlayer {
                    actor: rcon::EntityUuid::parse(&f.player_uuid)?,
                    position: f.expected_player_position,
                },
                case,
                "stage-operator",
            )?;
            let staged = fresh(runner, f, case, "stage-after")?;
            validate_player(f, &staged, true)?;
            ensure(staged["player"]["life_id"] == life, "staging changed life")?;
        }
        save(
            &case.join("stage.json"),
            &json!({"fixture_sha256":hash,"player_uuid":f.player_uuid,"life_id":life,"connected":true,"staged_native_verified":client.is_some(),"operator_handoff_pending":client.is_none(),"npc_summoned":false,"next":"Launch validates real READY before guarded summon; no heal, respawn or rollback."}),
        )?;
        if client.is_none() {
            println!(
                "{}",
                json!({"status":"pending_operator_stage_handoff","case_id":f.case_id,"player_uuid":f.player_uuid,"required_position":f.expected_player_position,"console_commands_executed":false})
            );
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = sdk(runner, f, case, "stage-error-disconnect", "disconnect", &[]);
    }
    result
}
pub fn validate_player(f: &Fixture, observation: &Value, position: bool) -> Result<()> {
    let player = &observation["player"];
    ensure(
        player["dimension"] == "minecraft:overworld",
        "overworld fixture required",
    )?;
    ensure(
        player["uuid"] == f.player_uuid && player["life_id"].as_str().is_some_and(uuid),
        "READY player/life identity mismatch",
    )?;
    ensure(
        player["health"].as_f64() == Some(f.expected_health)
            && player["food"].as_u64() == Some(f.expected_food),
        "fresh healthy actor required; no healing/reuse",
    )?;
    let inventory = player["inventory"].as_array().ok_or("inventory absent")?;
    ensure(
        inventory.iter().all(empty_stack),
        "fresh empty inventory required",
    )?;
    let cursor = observation["menu"]
        .get("carried")
        .or_else(|| observation["menu"].get("cursor"))
        .ok_or("explicit menu cursor required")?;
    ensure(empty_stack(cursor), "empty cursor required")?;
    ensure(
        observation["menu"]["menu_id"] == 0
            && observation["menu"]["slots"]
                .as_array()
                .is_some_and(|rows| rows.iter().all(empty_stack)),
        "fresh empty own menu required",
    )?;
    if position {
        let point = player["position"]
            .as_array()
            .ok_or("native position absent")?;
        ensure(point.len() == 3, "position shape differs")?;
        let mut distance = 0.;
        for (value, wanted) in point.iter().zip(f.expected_player_position) {
            let actual = value
                .as_f64()
                .filter(|n| n.is_finite())
                .ok_or("invalid native position")?;
            distance += (actual - wanted).powi(2);
        }
        ensure(
            distance.sqrt() <= f.position_tolerance,
            "actor outside declared starting pose",
        )?;
    }
    Ok(())
}
fn empty_stack(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|_| value["count"].as_u64() == Some(0) && value["item"] == "minecraft:air")
}
fn launch(runner: &mut dyn Runner, f: &Fixture, case: &Path, hash: &str) -> Result<()> {
    preflight(runner, f, case, "launch-preflight")?;
    let stage = json(&case.join("stage.json"), 16384)?;
    ensure(
        stage["fixture_sha256"] == hash
            && stage["player_uuid"] == f.player_uuid
            && stage["connected"] == true,
        "matching staged connection required",
    )?;
    let expected_life = stage["life_id"].as_str().ok_or("staged life absent")?;
    ensure(
        json(&f.profile(), 16384)?["session"].is_object(),
        "staged controller session required",
    )?;
    let mut console = rcon_client(f)?;
    let mut ready = false;
    let mut admitted = false;
    let mut summon_attempted = false;
    let mut last_npc = f.expected_npc_position;
    let mut npc_native_seen = false;
    save(
        &case.join("launch-intent.json"),
        &json!({"fixture_sha256":hash,"player_uuid":f.player_uuid,"life_id":expected_life,"duration_secs":120,"utc_ms":utc_ms()?}),
    )?;
    let mut args = vec!["exec".into()];
    if let Some(ca) = &f.ca_file {
        args.extend([
            "--env".into(),
            format!("MC_SDK_CA_CERT={}", ca.container_path),
        ]);
    }
    args.extend([
        f.container.clone(),
        "timeout".into(),
        "--signal=TERM".into(),
        "--kill-after=5s".into(),
        "245s".into(),
        f.handler.clone(),
    ]);
    args.extend(f.handler_args.clone());
    let result = process::execute(
        runner,
        f,
        &f.docker,
        args,
        255,
        case,
        "handler",
        &mut |value| {
            if let Some(entities) = value["data"]["observation"]["world"]["entities"].as_array() {
                if let Some(enemy) = entities
                    .iter()
                    .find(|e| e["uuid"] == f.npc_uuid && e["type"] == "minecraft:skeleton")
                {
                    if let Some(p) = enemy["position"].as_array().filter(|p| p.len() == 3) {
                        let coords: Option<Vec<f64>> = p
                            .iter()
                            .map(|v| v.as_f64().filter(|n| n.is_finite()))
                            .collect();
                        if let Some(p) = coords {
                            last_npc = [p[0], p[1], p[2]];
                            npc_native_seen = true;
                        }
                    }
                }
            }
            if value["event"] == "body_probe_ready" {
                ensure(!ready, "duplicate READY")?;
                ensure(
                    value["data"]["entity_uuid"] == f.npc_uuid,
                    "READY NPC identity differs",
                )?;
                validate_player(f, &value["data"]["observation"], true)?;
                ensure(
                    value["data"]["observation"]["player"]["life_id"] == expected_life,
                    "READY life differs from stage",
                )?;
                save(&case.join("ready-native.json"), value)?;
                ready = true;
                if let Some(client) = &mut console {
                    summon_attempted = true;
                    save(
                        &case.join("summon-intent.json"),
                        &json!({"npc_uuid":f.npc_uuid,"player_uuid":f.player_uuid,"life_id":expected_life,"automatic_retry":false}),
                    )?;
                    rcon_execute(
                        client,
                        rcon::Command::GuardedSummonSkeleton {
                            actor: rcon::EntityUuid::parse(&f.player_uuid)?,
                            npc: rcon::EntityUuid::parse(&f.npc_uuid)?,
                            actor_origin: f.expected_player_position,
                            tolerance: f.position_tolerance,
                            npc_position: f.expected_npc_position,
                        },
                        case,
                        "summon-operator",
                    )?;
                } else {
                    save(
                        &case.join("summon-handoff-pending.json"),
                        &json!({"player_uuid":f.player_uuid,"npc_uuid":f.npc_uuid,"npc_position":f.expected_npc_position,"console_commands_executed":false,"status":"operator must perform exact declared staging; handler admission remains authoritative"}),
                    )?;
                    println!(
                        "{}",
                        json!({"status":"pending_operator_spawn_handoff","case_id":f.case_id,"ready_native_verified":true,"console_commands_executed":false})
                    );
                    std::io::stdout().flush()?;
                }
            }
            if value["event"] == "body_probe_stimulus_admitted" {
                ensure(
                    ready && !admitted && value["data"]["entity_uuid"] == f.npc_uuid,
                    "unexpected admission",
                )?;
                validate_player(f, &value["data"]["observation"], true)?;
                ensure(
                    value["data"]["observation"]["player"]["life_id"] == expected_life,
                    "admission life differs",
                )?;
                save(&case.join("admitted-native.json"), value)?;
                admitted = true;
            }
            Ok(())
        },
    );
    // Always attempt ordinary disconnection after a launched handler, including fatal output/timeout.
    let disconnected = sdk(runner, f, case, "launch-disconnect", "disconnect", &[]);
    let post_hash = verify_container_artifacts(runner, f, case, "post-launch");
    save(
        &case.join("last-known-npc.json"),
        &json!({"npc_uuid":f.npc_uuid,"position":last_npc,"native_seen":npc_native_seen,"fallback_scope":"Declared spawn position if no native NPC snapshot was recorded; does not prove absence outside bounded ring"}),
    )?;
    let mut cleanup_complete = !summon_attempted && !admitted;
    if summon_attempted && let Ok(Some(mut client)) = rcon_client(f) {
        cleanup_complete = cleanup::run(&mut client, f, case, last_npc).is_ok();
    }
    let handler_ok = result.as_ref().is_ok_and(|o| o.success);
    let closed = disconnected
        .as_ref()
        .is_ok_and(|v| v["disconnected"] == true);
    save(
        &case.join("launch-result.json"),
        &json!({"ready_native_verified":ready,"admitted_native_verified":admitted,"handler_exit_success":handler_ok,"sdk_disconnected":closed,"artifact_hashes_unchanged":post_hash.is_ok(),"owned_npc_cleanup_attempted":summon_attempted,"cleanup_complete":cleanup_complete,"certifies_goal_completion":false,"original_errors_retained":true}),
    )?;
    ensure(
        handler_ok && closed && ready && admitted && post_hash.is_ok() && cleanup_complete,
        "launch failed/incomplete; retained outputs are not a PASS",
    )
}
#[cfg(test)]
mod tests;
