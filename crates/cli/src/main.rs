use minecraft_sdk::*;
use serde_json::{Value, json};
use std::{
    io::{BufRead, Write},
    path::Path,
};

fn save(
    path: &Path,
    profile: &Profile,
    new: bool,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::OpenOptionsExt;
    let target = if new {
        path.to_path_buf()
    } else {
        path.with_extension("key.new")
    };
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&target)?;
    file.write_all(&serde_json::to_vec(profile)?)?;
    file.sync_all()?;
    if !new {
        std::fs::rename(&target, path)?;
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}
fn load(path: &Path) -> std::result::Result<Profile, Box<dyn std::error::Error>> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.mode() & 0o077 != 0 || meta.uid() != unsafe { libc::geteuid() } {
        return Err("Profile must be a private regular file owned by you (chmod 600)".into());
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(16385)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 16384 {
        return Err("Profile too large".into());
    }
    Ok(serde_json::from_slice(&bytes)?)
}
#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("import-profile") && args.len() == 5 {
        Client::new(&args[1])?;
        let info: ServerInfo = serde_json::from_slice(&std::fs::read(&args[2])?)?;
        let response: Value = serde_json::from_slice(&std::fs::read(&args[3])?)?;
        let credential: Credential =
            serde_json::from_value(response.get("ok").cloned().unwrap_or(response))?;
        let profile = Profile {
            endpoint: args[1].clone(),
            instance_id: info.instance_id,
            credential,
            session: None,
        };
        save(Path::new(&args[4]), &profile, true)?;
        println!("Private profile saved for {}", profile.credential.bot_id);
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("discover") && args.len() == 2 {
        println!(
            "{}",
            serde_json::to_string_pretty(&Client::new(&args[1])?.discover().await?)?
        );
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("register") && args.len() == 4 {
        let path = Path::new(&args[3]);
        // Reserve the output file before registration so an existing key cannot be overwritten.
        use std::os::unix::fs::OpenOptionsExt;
        let reserved = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        drop(reserved);
        let client = Client::new(&args[1])?;
        let info = client.discover().await?;
        let credential = client.register(&args[2]).await?;
        let profile = Profile {
            endpoint: args[1].clone(),
            instance_id: info.instance_id,
            credential,
            session: None,
        };
        save(path, &profile, false)?;
        println!(
            "Registered {}. Private key saved to {}",
            profile.credential.bot_id,
            path.display()
        );
        return Ok(());
    }
    if args.len() < 2 {
        return Err("Usage: mc-sdk discover URL | register URL NAME PROFILE | connect PROFILE REQUEST_ID | disconnect PROFILE | observe PROFILE | act PROFILE ACTION.json | status PROFILE ID | reconcile PROFILE ID | rotate-key PROFILE | mcp PROFILE".into());
    }
    let path = Path::new(&args[1]);
    let mut profile = load(path)?;
    let mut client = Client::with_profile(&profile)?;
    client.verify_instance(&profile.instance_id).await?;
    let value = match args[0].as_str() {
        "connect" => {
            let session = client
                .connect(args.get(2).ok_or("Connection request ID required")?)
                .await?;
            profile.session = Some(session.clone());
            client.set_session(Some(session));
            save(path, &profile, false)?;
            json!({"connected":true})
        }
        "disconnect" => {
            client.disconnect().await?;
            profile.session = None;
            save(path, &profile, false)?;
            json!({"disconnected":true})
        }
        "observe" => serde_json::to_value(client.observe().await?)?,
        "act" => {
            let request: ActionRequest = serde_json::from_slice(&std::fs::read(
                args.get(2).ok_or("Action JSON file required")?,
            )?)?;
            serde_json::to_value(client.act(&request).await?)?
        }
        "status" => serde_json::to_value(
            client
                .action_status(args.get(2).ok_or("Action ID required")?)
                .await?,
        )?,
        "reconcile" => serde_json::to_value(
            client
                .reconcile(args.get(2).ok_or("Action ID required")?)
                .await?,
        )?,
        "rotate-key" => {
            profile.credential = client.rotate_key().await?;
            save(path, &profile, false)?;
            json!({"rotated":true})
        }
        "mcp" => {
            mcp(&client).await?;
            return Ok(());
        }
        _ => return Err("Unknown command".into()),
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
async fn mcp(client: &Client) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut output = std::io::stdout().lock();
    loop {
        let mut line = Vec::new();
        loop {
            let buffer = input.fill_buf()?;
            if buffer.is_empty() {
                if line.is_empty() {
                    return Ok(());
                }
                return Err("Incomplete JSON-RPC message".into());
            }
            let n = buffer
                .iter()
                .position(|b| *b == b'\n')
                .map(|n| n + 1)
                .unwrap_or(buffer.len());
            if line.len() + n > 16384 {
                return Err("MCP request too large".into());
            }
            let end = buffer[n - 1] == b'\n';
            line.extend_from_slice(&buffer[..n]);
            input.consume(n);
            if end {
                break;
            }
        }
        let request: Value = match serde_json::from_slice(&line) {
            Ok(v) => v,
            Err(_) => {
                writeln!(
                    output,
                    "{}",
                    json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Invalid JSON"}})
                )?;
                output.flush()?;
                continue;
            }
        };
        let Some(id) = request.get("id") else {
            continue;
        };
        let result = match request["method"].as_str().unwrap_or("") {
            "initialize" => Ok(
                json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"minecraft-sdk","version":env!("CARGO_PKG_VERSION")}}),
            ),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools":[
                {"name":"minecraft_discover","description":"Discover this configured gateway and its installed capabilities","inputSchema":{"type":"object","properties":{},"additionalProperties":false}},
                {"name":"minecraft_observe","description":"Observe your connected Survival character","inputSchema":{"type":"object","properties":{},"additionalProperties":false}},
                {"name":"minecraft_act","description":"Submit a typed gameplay action with a unique stable action_id. Never retry an uncertain mutation under another ID. Supported kinds are movement, mining, placement, item use, entity interactions, inventory, crafting, chat, commands and respawn; capability discovery determines availability.","inputSchema":{"type":"object","required":["action_id","action"],"properties":{"action_id":{"type":"string"},"action":{"type":"object"}},"additionalProperties":false}},
                {"name":"minecraft_action_status","description":"Read the durable receipt for an action_id","inputSchema":{"type":"object","required":["action_id"],"properties":{"action_id":{"type":"string"}},"additionalProperties":false}},
                {"name":"minecraft_reconcile","description":"Explicitly reconcile an uncertain action without replay","inputSchema":{"type":"object","required":["action_id"],"properties":{"action_id":{"type":"string"}},"additionalProperties":false}}
            ]})),
            "tools/call" => {
                let args = &request["params"]["arguments"];
                let result: Result<Value> = match request["params"]["name"].as_str().unwrap_or("") {
                    "minecraft_discover" => client.discover().await.and_then(as_json),
                    "minecraft_observe" => client.observe().await.and_then(as_json),
                    "minecraft_act" => {
                        match serde_json::from_value::<ActionRequest>(args.clone()) {
                            Ok(r) => client.act(&r).await.and_then(as_json),
                            Err(_) => Err(ApiError::new(
                                ErrorCode::InvalidRequest,
                                "Invalid typed action request",
                            )),
                        }
                    }
                    "minecraft_action_status" => client
                        .action_status(args["action_id"].as_str().unwrap_or(""))
                        .await
                        .and_then(as_json),
                    "minecraft_reconcile" => client
                        .reconcile(args["action_id"].as_str().unwrap_or(""))
                        .await
                        .and_then(as_json),
                    _ => Err(ApiError::new(
                        ErrorCode::UnsupportedCapability,
                        "Unknown gameplay tool",
                    )),
                };
                Ok(match result {
                    Ok(v) => json!({"content":[{"type":"text","text":v.to_string()}]}),
                    Err(e) => {
                        json!({"isError":true,"content":[{"type":"text","text":serde_json::to_string(&e)?}]})
                    }
                })
            }
            _ => Err(json!({"code":-32601,"message":"Method not found"})),
        };
        let response = match result {
            Ok(v) => json!({"jsonrpc":"2.0","id":id,"result":v}),
            Err(e) => json!({"jsonrpc":"2.0","id":id,"error":e}),
        };
        writeln!(output, "{response}")?;
        output.flush()?;
    }
}
fn as_json(value: impl serde::Serialize) -> Result<Value> {
    serde_json::to_value(value)
        .map_err(|_| ApiError::new(ErrorCode::Internal, "Cannot encode response"))
}
