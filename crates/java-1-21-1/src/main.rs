use minecraft_sdk_core::*;
mod chat;
mod control;
mod effects;
mod motion;
mod recipes;
mod routes;
use minecraft_sdk_java_1_21_1::{Wire, encode_signed_varint as vi};
use minecraft_sdk_wire::{decode_string_prefix, decode_varint_prefix, encode_string as text};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, Read, Write},
    net::{SocketAddr, TcpStream},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const VERSION: &str = env!("CARGO_PKG_VERSION");
type AnyResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    backend: SocketAddr,
    native_endpoint: String,
    native_token: PathBuf,
    #[serde(default = "default_control_ticks")]
    control_max_ticks: u16,
    /// Legacy conformance only. Public coding workspaces require finite controls.
    #[serde(default)]
    allow_blocking_locomotion: bool,
}
fn default_control_ticks() -> u16 {
    100
}
#[derive(Clone)]
struct Native {
    endpoint: String,
    token: String,
    http: reqwest::blocking::Client,
}
impl Native {
    fn new(config: &Config) -> AnyResult<Self> {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let url = reqwest::Url::parse(&config.native_endpoint)?;
        if url.scheme() != "http"
            || !url
                .host_str()
                .and_then(|s| s.parse::<std::net::IpAddr>().ok())
                .is_some_and(|ip| ip.is_loopback())
            || url.path() != "/"
            || url.query().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("Native endpoint must be a literal loopback HTTP origin".into());
        }
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&config.native_token)?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.mode() & 0o077 != 0 || meta.uid() != unsafe { libc::geteuid() } {
            return Err("Native token must be owned and private".into());
        }
        let mut token = String::new();
        std::io::Read::by_ref(&mut file)
            .take(129)
            .read_to_string(&mut token)?;
        let token = token.trim().to_owned();
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("Invalid native token file".into());
        }
        Ok(Self {
            endpoint: config.native_endpoint.trim_end_matches('/').into(),
            token,
            http: reqwest::blocking::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(8))
                .build()?,
        })
    }
    fn call(&self, op: &str, body: Value) -> AnyResult<Value> {
        self.call_with_timeout(op, body, Duration::from_secs(8))
    }
    fn call_with_timeout(&self, op: &str, body: Value, timeout: Duration) -> AnyResult<Value> {
        let response = self
            .http
            .post(format!("{}/v1/{op}", self.endpoint))
            .bearer_auth(&self.token)
            .json(&body)
            .timeout(timeout)
            .send()?;
        let status = response.status();
        let mut bytes = Vec::new();
        let limit = if op == "observe-extension" {
            8 * 1_048_576
        } else {
            1_048_576
        };
        response.take(limit + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit {
            return Err("Native response too large".into());
        }
        if !status.is_success() {
            return Err(format!("Native operation refused: HTTP {}", status.as_u16()).into());
        }
        Ok(serde_json::from_slice(&bytes)?)
    }
    fn observe(&self, uuid: &str) -> AnyResult<Observation> {
        self.observe_bounded(uuid, Duration::from_secs(8))
    }
    fn observe_bounded(&self, uuid: &str, timeout: Duration) -> AnyResult<Observation> {
        Ok(serde_json::from_value(self.call_with_timeout(
            "observe",
            json!({"player_uuid":uuid}),
            timeout,
        )?)?)
    }
}
#[derive(Clone)]
struct Tables(Value);
impl Tables {
    fn new() -> Self {
        Self(serde_json::from_str(include_str!("../packet-ids.json")).unwrap())
    }
    fn id(&self, phase: &str, direction: &str, name: &str) -> AnyResult<u32> {
        self.0[phase][direction]
            .as_object()
            .and_then(|m| m.iter().find(|(_, v)| v.as_str() == Some(name)))
            .and_then(|(k, _)| u32::from_str_radix(k.trim_start_matches("0x"), 16).ok())
            .ok_or_else(|| format!("Unknown {phase} packet {name}").into())
    }
    fn name(&self, phase: &str, id: u32) -> &str {
        self.0[phase]["toClient"][format!("0x{id:02x}")]
            .as_str()
            .unwrap_or("unknown")
    }
    fn send(
        &self,
        wire: &mut Wire<TcpStream>,
        phase: &str,
        name: &str,
        body: &[u8],
    ) -> AnyResult<()> {
        wire.send(self.id(phase, "toServer", name)?, body)?;
        Ok(())
    }
    fn custom(&self, wire: &mut Wire<TcpStream>, name: &str, body: &[u8]) -> AnyResult<()> {
        let mut payload = text(name)?;
        payload.extend(body);
        self.send(wire, "configuration", "custom_payload", &payload)
    }
}
struct Player {
    physics: motion::Actor,
    wire_ground: Arc<AtomicBool>,
    route_preview_at: Option<Instant>,
    corrections: Arc<AtomicU64>,
    correcting: Arc<AtomicBool>,
    control: Option<control::Job>,
    interaction: Option<effects::Job>,
    motion: Option<([f64; 3], f64)>,
    wire: Arc<Mutex<Wire<TcpStream>>>,
    alive: Arc<AtomicBool>,
    tables: Tables,
    sequence: i32,
    chat: Arc<Mutex<chat::ChatState>>,
}
impl Drop for Player {
    fn drop(&mut self) {
        if let Some(job) = &self.control {
            job.stop();
        }
        if let Some(job) = &self.interaction {
            job.stop();
        }
        self.alive.store(false, Ordering::Release);
        if let Ok(wire) = self.wire.lock() {
            let _ = wire.connection().shutdown(std::net::Shutdown::Both);
        }
    }
}
fn parse_uuid(s: &str) -> AnyResult<[u8; 16]> {
    let text = s.replace('-', "");
    if text.len() != 32 {
        return Err("Invalid player UUID".into());
    }
    let mut id = [0u8; 16];
    for (i, b) in id.iter_mut().enumerate() {
        *b = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16)?;
    }
    Ok(id)
}
fn connect(
    config: &Config,
    native: &Native,
    request: &AdapterRequest,
    discovery: &Value,
) -> Result<Player> {
    let run = || -> AnyResult<Player> {
        if !discovery["native_observation_features"]
            .as_array()
            .is_some_and(|c| c.iter().any(|v| v == "minecraft:physics_facts"))
        {
            return Err("Matching continuous-physics server integration required".into());
        }
        native.call(
            "register",
            json!({"player_uuid":request.player_uuid,"player_name":request.player_name}),
        )?;
        let stream = TcpStream::connect_timeout(&config.backend, Duration::from_secs(5))?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(Duration::from_secs(15)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let admitted=native.call("admit",json!({"player_uuid":request.player_uuid,"player_name":request.player_name,"source_port":stream.local_addr()?.port()}))?;
        if admitted["error"] == "server_full" {
            return Err("server_full".into());
        }
        if admitted["reserved"] != true {
            return Err("Admission refused".into());
        }
        let tables = Tables::new();
        let mut wire = Wire::new(stream);
        let mut handshake = vi(767);
        handshake.extend(text(&config.backend.ip().to_string())?);
        handshake.extend(config.backend.port().to_be_bytes());
        handshake.extend(vi(2));
        wire.send(0, &handshake)?;
        let mut login = text(&request.player_name)?;
        login.extend(parse_uuid(&request.player_uuid)?);
        wire.send(0, &login)?;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let packet = wire.receive_until(deadline)?;
            match tables.name("login", packet.id) {
                "compress" => {
                    let (n, _) = decode_varint_prefix(&packet.payload)?;
                    wire.set_compression(Some(i32::try_from(n)?))?;
                }
                "success" => {
                    if packet.payload.get(..16)
                        != Some(parse_uuid(&request.player_uuid)?.as_slice())
                    {
                        return Err("Login UUID mismatch".into());
                    }
                    let (name, _) = decode_string_prefix(&packet.payload[16..])?;
                    if name != request.player_name {
                        return Err("Login name mismatch".into());
                    }
                    tables.send(&mut wire, "login", "login_acknowledged", &[])?;
                    break;
                }
                "login_plugin_request" => {
                    let (id, _) = decode_varint_prefix(&packet.payload)?;
                    let mut body = vi(id as i32);
                    body.push(0);
                    tables.send(&mut wire, "login", "login_plugin_response", &body)?;
                }
                "disconnect" => return Err("Server rejected bot login".into()),
                _ => return Err("Unsupported Java login requirement".into()),
            }
        }
        register_channels(&tables, &mut wire, discovery)?;
        loop {
            let packet = wire.receive_until(deadline)?;
            match tables.name("configuration", packet.id) {
                "keep_alive" => {
                    tables.send(&mut wire, "configuration", "keep_alive", &packet.payload)?
                }
                "ping" => tables.send(&mut wire, "configuration", "pong", &packet.payload)?,
                "select_known_packs" => {
                    tables.send(&mut wire, "configuration", "select_known_packs", &[0])?
                }
                "finish_configuration" => {
                    tables.send(&mut wire, "configuration", "finish_configuration", &[])?;
                    break;
                }
                "custom_payload" => configuration_custom(&tables, &mut wire, &packet.payload)?,
                "disconnect" => return Err("NeoForge configuration refused".into()),
                "add_resource_pack" => return Err("Required resource packs are unsupported".into()),
                "cookie_request" | "transfer" => {
                    return Err("Unsupported configuration requirement".into());
                }
                _ => {}
            }
        }
        wire.connection().set_read_timeout(None)?;
        let stream = wire.connection().try_clone()?;
        let compression = wire.compression();
        let wire = Arc::new(Mutex::new(wire));
        let alive = Arc::new(AtomicBool::new(true));
        let corrections = Arc::new(AtomicU64::new(0));
        let correcting = Arc::new(AtomicBool::new(false));
        let wire_ground = Arc::new(AtomicBool::new(false));
        let reader_ground = wire_ground.clone();
        let impulses: motion::Inbox = Arc::new(Mutex::new(std::collections::VecDeque::new()));
        let reader_impulses = impulses.clone();
        let entity = Arc::new(std::sync::atomic::AtomicI32::new(-1));
        let reader_entity = entity.clone();
        let reader_corrections = corrections.clone();
        let reader_correcting = correcting.clone();
        let reader_writer = wire.clone();
        let reader_alive = alive.clone();
        let reader_tables = tables.clone();
        let reader_native = native.clone();
        let reader_uuid = request.player_uuid.clone();
        let chat = Arc::new(Mutex::new(chat::ChatState::default()));
        let reader_chat = chat.clone();
        thread::spawn(move || {
            let mut reader = Wire::new(stream);
            let _ = reader.set_compression(compression.map(|n| n as i32));
            while reader_alive.load(Ordering::Acquire) {
                let result = (|| -> AnyResult<()> {
                    let packet = reader.receive()?;
                    if reader_tables.name("play", packet.id) == "player_chat" {
                        let mut chat = reader_chat.lock().map_err(|_| "Chat state unavailable")?;
                        if let Some(count) = chat.receive(&packet.payload)? {
                            let mut writer = reader_writer
                                .lock()
                                .map_err(|_| "Player writer unavailable")?;
                            reader_tables.send(
                                &mut writer,
                                "play",
                                "message_acknowledgement",
                                &vi(count),
                            )?;
                        }
                        return Ok(());
                    }
                    let mut writer = reader_writer
                        .lock()
                        .map_err(|_| "Player writer unavailable")?;
                    match reader_tables.name("play", packet.id) {
                        "login" => {
                            reader_entity.store(
                                i32::from_be_bytes(
                                    packet
                                        .payload
                                        .get(..4)
                                        .ok_or("Truncated player login")?
                                        .try_into()?,
                                ),
                                Ordering::Release,
                            );
                        }
                        "keep_alive" => reader_tables.send(
                            &mut writer,
                            "play",
                            "keep_alive",
                            &packet.payload,
                        )?,
                        "ping" => {
                            reader_tables.send(&mut writer, "play", "pong", &packet.payload)?
                        }
                        "chunk_batch_finished" => reader_tables.send(
                            &mut writer,
                            "play",
                            "chunk_batch_received",
                            &8f32.to_be_bytes(),
                        )?,
                        "position" => {
                            reader_correcting.store(true, Ordering::Release);
                            reader_corrections.fetch_add(1, Ordering::AcqRel);
                            if packet.payload.len() < 34 {
                                return Err("Malformed player correction".into());
                            }
                            let (id, _) = decode_varint_prefix(&packet.payload[33..])?;
                            reader_tables.send(
                                &mut writer,
                                "play",
                                "teleport_confirm",
                                &vi(id as i32),
                            )?;
                            // A normal client reports its corrected position as
                            // well as the teleport ID. Without a movement
                            // report, server chunk tracking can stay at the old
                            // location indefinitely while this client is idle.
                            drop(writer);
                            let observed = reader_native.observe(&reader_uuid).ok();
                            let body = correction_ack(&packet.payload, observed.as_ref())?;
                            let mut writer = reader_writer
                                .lock()
                                .map_err(|_| "Player writer unavailable")?;
                            reader_tables.send(&mut writer, "play", "position", &body)?;
                            reader_ground.store(body.last() == Some(&1), Ordering::Release);
                            reader_correcting.store(false, Ordering::Release);
                        }
                        "entity_velocity" => {
                            if let Some(impulse) = motion::entity_impulse(
                                &packet.payload,
                                reader_entity.load(Ordering::Acquire),
                            )? {
                                motion::push_impulse(&reader_impulses, impulse)?;
                            }
                        }
                        "explosion" => {
                            motion::push_impulse(
                                &reader_impulses,
                                motion::explosion_impulse(&packet.payload)?,
                            )?;
                        }
                        "kick_disconnect" | "start_configuration" => {
                            return Err("Player disconnected or requires reconfiguration".into());
                        }
                        _ => {}
                    }
                    Ok(())
                })();
                if result.is_err() {
                    break;
                }
            }
            reader_alive.store(false, Ordering::Release);
            if let Ok(writer) = reader_writer.lock() {
                let _ = writer.connection().shutdown(std::net::Shutdown::Both);
            }
        });
        let until = Instant::now() + Duration::from_secs(5);
        while correcting.load(Ordering::Acquire) || native.observe(&request.player_uuid).is_err() {
            if !alive.load(Ordering::Acquire) {
                return Err("Player connection interrupted during admission".into());
            }
            if Instant::now() > until {
                return Err("Player did not enter Survival".into());
            }
            thread::sleep(Duration::from_millis(100));
        }
        let initial = native.observe(&request.player_uuid)?;
        let physics = match motion::Actor::start(
            native.clone(),
            request.player_uuid.clone(),
            initial,
            motion::Wires {
                writer: wire.clone(),
                tables: tables.clone(),
                alive: alive.clone(),
                corrections: corrections.clone(),
                correcting: correcting.clone(),
                ground: wire_ground.clone(),
                impulses,
                entity,
            },
        ) {
            Ok(actor) => actor,
            Err(error) => {
                alive.store(false, Ordering::Release);
                if let Ok(w) = wire.lock() {
                    let _ = w.connection().shutdown(std::net::Shutdown::Both);
                }
                return Err(error);
            }
        };
        Ok(Player {
            physics,
            wire_ground,
            route_preview_at: None,
            corrections,
            correcting,
            control: None,
            interaction: None,
            motion: None,
            wire,
            alive,
            tables,
            sequence: 0,
            chat,
        })
    };
    run().map_err(|e| {
        ApiError::new(
            if e.to_string() == "server_full" {
                ErrorCode::ServerFull
            } else {
                ErrorCode::NotReady
            },
            format!("Player connection failed: {e}"),
        )
    })
}
fn register_channels(
    tables: &Tables,
    wire: &mut Wire<TcpStream>,
    discovery: &Value,
) -> AnyResult<()> {
    let channels = discovery["channels"]
        .as_array()
        .ok_or("Native channel registry missing")?;
    let mut payload = vec![2];
    for (protocol, ordinal) in [("play", 1), ("configuration", 4)] {
        let group: Vec<_> = channels
            .iter()
            .filter(|c| c["protocol"] == protocol)
            .collect();
        payload.extend(vi(ordinal));
        payload.extend(vi(group.len() as i32));
        for c in group {
            payload.extend(text(c["id"].as_str().ok_or("Channel ID missing")?)?);
            payload.extend(text(
                c["version"].as_str().ok_or("Channel version missing")?,
            )?);
            let flow = c["flow"].as_str().ok_or("Channel flow missing")?;
            if flow == "BIDIRECTIONAL" {
                payload.push(0);
            } else {
                payload.push(1);
                payload.extend(vi(i32::from(flow == "CLIENTBOUND")));
            }
            payload.push(u8::from(c["optional"] == true));
        }
    }
    tables.custom(wire, "neoforge:register", &payload)?;
    tables.custom(wire, "minecraft:brand", &text("minecraft-rust-sdk")?)?;
    Ok(())
}
fn configuration_custom(
    tables: &Tables,
    wire: &mut Wire<TcpStream>,
    payload: &[u8],
) -> AnyResult<()> {
    let (channel, n) = decode_string_prefix(payload)?;
    let body = &payload[n..];
    match channel {
        "neoforge:feature_flags" => tables.custom(wire, "neoforge:feature_flags_ack", &[])?,
        "neoforge:extensible_enum_data" => {
            tables.custom(wire, "neoforge:extensible_enum_ack", &[])?
        }
        "neoforge:frozen_registry_sync_completed" => tables.custom(wire, channel, &[])?,
        "neoforge:known_registry_data_maps" => {
            let mut at = 0;
            let mut output = Vec::new();
            let count = read_vi(body, &mut at)?;
            if !(0..=65536).contains(&count) {
                return Err("Data map count invalid".into());
            }
            output.extend(vi(count));
            for _ in 0..count {
                let (registry, n) = decode_string_prefix(&body[at..])?;
                at += n;
                output.extend(text(registry)?);
                let maps = read_vi(body, &mut at)?;
                if !(0..=65536).contains(&maps) {
                    return Err("Data map count invalid".into());
                }
                output.extend(vi(maps));
                for _ in 0..maps {
                    let (id, n) = decode_string_prefix(&body[at..])?;
                    at += n;
                    output.extend(text(id)?);
                    at = at.checked_add(1).ok_or("Data map overflow")?;
                    if at > body.len() {
                        return Err("Data map truncated".into());
                    }
                }
            }
            if at != body.len() {
                return Err("Trailing data map bytes".into());
            }
            tables.custom(wire, "neoforge:known_registry_data_maps_reply", &output)?;
        }
        "c:version" | "c:register" => tables.custom(wire, channel, body)?,
        "neoforge:frozen_registry_sync_start" | "neoforge:frozen_registry" => {}
        _ => {}
    }
    Ok(())
}
fn read_vi(bytes: &[u8], at: &mut usize) -> AnyResult<i32> {
    let (v, n) = decode_varint_prefix(bytes.get(*at..).ok_or("Truncated varint")?)?;
    *at += n;
    Ok(v as i32)
}
fn hand(value: &Hand) -> i32 {
    match value {
        Hand::Main => 0,
        Hand::Off => 1,
    }
}
fn block(value: &BlockPos) -> [u8; 8] {
    (((value.x as i64 & 0x3ffffff) << 38)
        | ((value.z as i64 & 0x3ffffff) << 12)
        | (value.y as i64 & 0xfff))
        .to_be_bytes()
}
fn position(o: &Observation) -> AnyResult<[f64; 3]> {
    let p = &o.player["position"];
    Ok([
        p[0].as_f64().ok_or("Position missing")?,
        p[1].as_f64().ok_or("Position missing")?,
        p[2].as_f64().ok_or("Position missing")?,
    ])
}
fn correction_ack(payload: &[u8], observed: Option<&Observation>) -> AnyResult<Vec<u8>> {
    if payload.len() < 34 || payload[32] & !0x1f != 0 {
        return Err("Malformed player correction".into());
    }
    let target = if payload[32] & 7 != 0 {
        // Native state is already absolute. Adding relative wire offsets again
        // would move the character twice. Only this admitted UUID is queried.
        position(observed.ok_or("Relative correction needs native player state")?)?
    } else {
        let mut p = [0.; 3];
        for (i, value) in p.iter_mut().enumerate() {
            *value = f64::from_be_bytes(payload[i * 8..i * 8 + 8].try_into()?);
        }
        p
    };
    if target.iter().any(|v| !v.is_finite())
        || target[0].abs() > 30_000_000.
        || target[2].abs() > 30_000_000.
    {
        return Err("Invalid corrected position".into());
    }
    let ground = observed.is_some_and(|o| {
        position(o).is_ok_and(|p| {
            (0..3).all(|i| (p[i] - target[i]).abs() < 0.001)
                && collision_boxes(o).is_ok_and(|boxes| {
                    let (next, ground) =
                        minecraft_sdk_java_1_21_1::physics::step(p, [0.; 3], -0.08, &boxes);
                    ground && (next[1] - p[1]).abs() < 0.001
                })
        })
    });
    let mut body = Vec::with_capacity(25);
    for value in target {
        body.extend(value.to_be_bytes());
    }
    body.push(u8::from(ground));
    Ok(body)
}
impl Player {
    fn send(&self, name: &str, body: &[u8]) -> AnyResult<()> {
        if !self.alive.load(Ordering::Acquire) {
            return Err("Player connection closed".into());
        }
        let mut wire = self.wire.lock().map_err(|_| "Player writer unavailable")?;
        let mut body = body.to_vec();
        if name == "look"
            && let Some(last) = body.last_mut()
        {
            *last = u8::from(self.wire_ground.load(Ordering::Acquire));
        }
        let result = self.tables.send(&mut wire, "play", name, &body);
        if result.is_err() {
            self.alive.store(false, Ordering::Release);
            let _ = wire.connection().shutdown(std::net::Shutdown::Both);
        }
        result
    }
    fn action(&mut self, action: &Action, before: &Observation) -> AnyResult<()> {
        let entity = before.player["entity_id"]
            .as_i64()
            .ok_or("Entity ID missing")? as i32;
        let mut body = Vec::new();
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("Sequence exhausted; reconnect")?;
        let name = match action {
            Action::StartMove { .. }
            | Action::CancelControl { .. }
            | Action::CancelUseItem { .. } => {
                return Err("Finite controls use their own scheduler".into());
            }
            Action::MotionFrame { target } => {
                let (next, ground, velocity) = motion_frame(before, target, self.motion)?;
                for v in next {
                    body.extend(v.to_be_bytes());
                }
                body.push(u8::from(ground));
                self.send("position", &body)?;
                self.motion = Some((next, velocity));
                return Ok(());
            }
            Action::DropSelected { entire_stack } => {
                body.extend(vi(if *entire_stack { 3 } else { 4 }));
                body.extend(0i64.to_be_bytes());
                body.push(0);
                body.extend(vi(self.sequence));
                "block_dig"
            }
            Action::Move { target, jump, .. } => {
                for (p, ground) in motion_plan(before, [target.x, target.y, target.z], *jump)? {
                    let mut bytes = Vec::new();
                    for v in p {
                        bytes.extend(v.to_be_bytes());
                    }
                    bytes.push(u8::from(ground));
                    self.send("position", &bytes)?;
                    thread::sleep(Duration::from_millis(50));
                }
                return Ok(());
            }
            Action::Look { yaw, pitch } => {
                body.extend(yaw.to_be_bytes());
                body.extend(pitch.to_be_bytes());
                body.push(u8::from(before.player["on_ground"] == true));
                "look"
            }
            Action::Jump => {
                if before.player["on_ground"] != true {
                    return Err("Jump requires observed ground support".into());
                }
                for (p, ground) in motion_plan(before, position(before)?, true)? {
                    let mut payload = Vec::new();
                    for v in p {
                        payload.extend(v.to_be_bytes());
                    }
                    payload.push(u8::from(ground));
                    self.send("position", &payload)?;
                    thread::sleep(Duration::from_millis(50));
                }
                return Ok(());
            }
            Action::Sprint { enabled } | Action::Sneak { enabled } => {
                body.extend(vi(entity));
                body.extend(vi(match action {
                    Action::Sprint { .. } => {
                        if *enabled {
                            3
                        } else {
                            4
                        }
                    }
                    _ => {
                        if *enabled {
                            0
                        } else {
                            1
                        }
                    }
                }));
                body.extend(vi(0));
                "entity_action"
            }
            Action::Dig {
                block: pos,
                face,
                stage,
            } => {
                body.extend(vi(match stage {
                    DigStage::Start => 0,
                    DigStage::Cancel => 1,
                    DigStage::Finish => 2,
                }));
                body.extend(block(pos));
                body.push(*face);
                body.extend(vi(self.sequence));
                "block_dig"
            }
            Action::Place {
                block: pos,
                face,
                hand: h,
                hit,
            }
            | Action::UseBlock {
                block: pos,
                face,
                hand: h,
                hit,
            } => {
                body.extend(vi(hand(h)));
                body.extend(block(pos));
                body.extend(vi(i32::from(*face)));
                for v in hit {
                    body.extend(v.to_be_bytes());
                }
                body.push(0);
                body.extend(vi(self.sequence));
                "block_place"
            }
            Action::UseItem { hand: h } | Action::StartUseItem { hand: h, .. } => {
                body.extend(vi(hand(h)));
                body.extend(vi(self.sequence));
                for key in ["yaw", "pitch"] {
                    body.extend(
                        (before.player[key].as_f64().ok_or("Rotation missing")? as f32)
                            .to_be_bytes(),
                    );
                }
                "use_item"
            }
            Action::Attack { entity } | Action::Interact { entity, .. } => {
                body.extend(vi(*entity));
                body.extend(vi(if matches!(action, Action::Attack { .. }) {
                    1
                } else {
                    0
                }));
                if let Action::Interact { hand: h, .. } = action {
                    body.extend(vi(hand(h)));
                }
                body.push(u8::from(before.player["sneaking"] == true));
                "use_entity"
            }
            Action::SelectSlot { slot } => {
                body.extend(i16::from(*slot).to_be_bytes());
                "held_item_slot"
            }
            Action::ClickSlot {
                menu_id,
                state_id,
                slot,
                button,
                mode,
            } => {
                if before.menu["menu_id"] != *menu_id || before.menu["state_id"] != *state_id {
                    return Err("Stale menu state".into());
                }
                body.push(u8::try_from(*menu_id)?);
                body.extend(vi(*state_id));
                body.extend(slot.to_be_bytes());
                body.push(*button);
                body.extend(vi(i32::from(*mode)));
                body.push(0);
                body.push(0);
                "window_click"
            }
            Action::CloseMenu { menu_id } => {
                body.push(u8::try_from(*menu_id)?);
                "close_window"
            }
            Action::Craft {
                recipe,
                menu_id,
                make_all,
            } => {
                body.push(u8::try_from(*menu_id)?);
                body.extend(text(recipe)?);
                body.push(u8::from(*make_all));
                "craft_recipe_request"
            }
            Action::Chat { message } => {
                body.extend(text(message)?);
                body.extend(
                    (std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_millis() as i64)
                        .to_be_bytes(),
                );
                body.extend(0i64.to_be_bytes());
                body.push(0);
                let mut chat = self.chat.lock().map_err(|_| "Chat state unavailable")?;
                body.extend(vi(chat.pending));
                body.extend(chat.bits());
                self.send("chat_message", &body)?;
                chat.pending = 0;
                return Ok(());
            }
            Action::Command { command } => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_millis() as i64;
                let mut chat = self.chat.lock().map_err(|_| "Chat state unavailable")?;
                let body = chat.command_payload(command, now)?;
                self.send("chat_command_signed", &body)?;
                chat.pending = 0;
                return Ok(());
            }
            Action::Respawn => {
                body.extend(vi(0));
                "client_command"
            }
            Action::Extension { .. } => return Err("Capability not installed".into()),
        };
        self.send(name, &body)
    }
}
fn motion_plan(
    observation: &Observation,
    target: [f64; 3],
    jump: bool,
) -> AnyResult<Vec<([f64; 3], bool)>> {
    let boxes = collision_boxes(observation)?;
    let start = position(observation)?;
    if (target[0] - start[0]).abs() > 3.5
        || (target[2] - start[2]).abs() > 3.5
        || target[1] - start[1] > 1.25
        || start[1] - target[1] > 3.
    {
        return Err("Waypoint exceeds observed terrain".into());
    }
    let speed = observation.player["movement_speed"]
        .as_f64()
        .filter(|v| v.is_finite() && *v > 0.)
        .map(|v| {
            (v * 2.1585
                * if observation.player["sneaking"] == true {
                    0.3
                } else {
                    1.
                })
            .min(0.36)
        })
        .unwrap_or(0.12);
    trajectory(start, target, jump, &boxes, speed)
}
fn collision_boxes(observation: &Observation) -> AnyResult<Vec<[f64; 6]>> {
    let blocks = observation.world["blocks"]
        .as_array()
        .ok_or("Terrain unavailable")?;
    let mut boxes = Vec::new();
    for block in blocks {
        let b = &block["position"];
        let origin = [
            b[0].as_f64().ok_or("Block position missing")?,
            b[1].as_f64().ok_or("Block position missing")?,
            b[2].as_f64().ok_or("Block position missing")?,
        ];
        for shape in block["collision"]
            .as_array()
            .ok_or("Collision shape missing")?
        {
            let a: Vec<f64> = shape
                .as_array()
                .ok_or("Collision shape invalid")?
                .iter()
                .map(|x| x.as_f64().unwrap_or(f64::NAN))
                .collect();
            if a.len() != 6 || a.iter().any(|v| !v.is_finite()) {
                return Err("Collision shape invalid".into());
            }
            let mut bounds = [0.; 6];
            for i in 0..6 {
                bounds[i] = origin[i % 3] + a[i];
            }
            boxes.push(bounds);
        }
    }
    Ok(boxes)
}
fn motion_frame(
    observation: &Observation,
    target: &Position,
    previous: Option<([f64; 3], f64)>,
) -> AnyResult<([f64; 3], bool, f64)> {
    let start = position(observation)?;
    let delta = [
        target.x - start[0],
        target.y - start[1],
        target.z - start[2],
    ];
    if delta[0] * delta[0] + delta[2] * delta[2] > 0.13 || !(-1.4..=0.43).contains(&delta[1]) {
        return Err("Motion frame exceeds ordinary player bounds".into());
    }
    let boxes = collision_boxes(observation)?;
    let (support, contact) =
        minecraft_sdk_java_1_21_1::physics::step(start, [0.; 3], -0.08, &boxes);
    let supported = contact && (support[1] - start[1]).abs() < 1e-7;
    let velocity = if supported {
        if (delta[1] - 0.42).abs() < 0.001 {
            0.42
        } else if delta[1].abs() < 0.001 {
            0.
        } else {
            -0.0784
        }
    } else {
        previous
            .filter(|(p, _)| (0..3).all(|i| (p[i] - start[i]).abs() < 0.05))
            .map(|(_, velocity)| velocity)
            // Reconnects/corrections may discard the trajectory. Only ordinary
            // downward gravity is admitted without tracked airborne momentum.
            .unwrap_or(-0.0784)
    };
    let (next, ground) =
        minecraft_sdk_java_1_21_1::physics::step(start, [delta[0], 0., delta[2]], velocity, &boxes);
    let wanted = [target.x, target.y, target.z];
    if !(0..3).all(|i| (next[i] - wanted[i]).abs() < 0.03) {
        return Err("Motion frame violates collision or gravity".into());
    }
    let (end_support, end_contact) =
        minecraft_sdk_java_1_21_1::physics::step(next, [0.; 3], -0.0784, &boxes);
    let ground =
        ground || (velocity <= 0. && end_contact && (end_support[1] - next[1]).abs() < 1e-7);
    let clipped = (next[1] - start[1] - velocity).abs() > 1e-7;
    let following = if ground {
        -0.0784
    } else {
        ((if clipped { 0. } else { velocity }) - 0.08) * 0.98
    };
    Ok((next, ground, following))
}
fn trajectory(
    start: [f64; 3],
    target: [f64; 3],
    jump: bool,
    boxes: &[[f64; 6]],
    speed: f64,
) -> AnyResult<Vec<([f64; 3], bool)>> {
    let mut frames = Vec::new();
    let mut p = start;
    let mut velocity = if jump { 0.42 } else { -0.08 };
    let mut peak = start[1];
    for _ in 0..60 {
        let dx = target[0] - p[0];
        let dz = target[2] - p[2];
        let distance = (dx * dx + dz * dz).sqrt();
        let factor = if distance > speed {
            speed / distance
        } else {
            1.
        };
        let (next, ground) = minecraft_sdk_java_1_21_1::physics::step(
            p,
            [dx * factor, 0., dz * factor],
            velocity,
            boxes,
        );
        if next[1] < start[1] - 3. || next[1] > start[1] + 2.5 {
            return Err("Movement leaves observed vertical terrain".into());
        }
        frames.push((next, ground));
        peak = f64::max(peak, next[1]);
        let clipped = (next[1] - p[1] - velocity).abs() > 1e-7;
        p = next;
        if ground
            && (p[0] - target[0]).abs() < 0.08
            && (p[1] - target[1]).abs() < 0.08
            && (p[2] - target[2]).abs() < 0.08
            && (!jump || peak > start[1] + 0.2)
        {
            return Ok(frames);
        }
        velocity = if ground {
            -0.08
        } else {
            ((if clipped { 0. } else { velocity }) - 0.08) * 0.98
        };
    }
    Err("No collision-safe trajectory reaches the requested supported waypoint".into())
}
fn main() -> AnyResult<()> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("Usage: minecraft-sdk-java-1-21-1 CONFIG.json")?;
    let config: Config = serde_json::from_slice(&std::fs::read(path)?)?;
    if !(1..=600).contains(&config.control_max_ticks) {
        return Err("Control tick budget must be 1–600".into());
    }
    if config.allow_blocking_locomotion {
        return Err("Continuous physics requires finite controls; legacy blocking locomotion is unsupported".into());
    }
    if !config.backend.ip().is_loopback() {
        return Err("Backend must be local to the trusted integration".into());
    }
    let native = Native::new(&config)?;
    let discovery = native.call("discover", json!({}))?;
    if discovery["minecraft_version"] != "1.21.1"
        || discovery["platform"] != "neoforge"
        || discovery["online_mode"] != true
        || !discovery["native_observation_features"]
            .as_array()
            .is_some_and(|v| {
                ["minecraft:physics_facts", "minecraft:native_action_effects"]
                    .iter()
                    .all(|required| v.iter().any(|c| c == required))
            })
    {
        return Err("Unsupported native integration: alpha.10 physics and native action effects are required".into());
    }
    let mut players: HashMap<String, Player> = HashMap::new();
    let mut identities: HashMap<String, (String, String, String)> = HashMap::new();
    let mut channel: Option<(String, u64)> = None;
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut output = std::io::stdout().lock();
    loop {
        let mut line = Vec::new();
        let count = std::io::Read::by_ref(&mut input)
            .take(16385)
            .read_until(b'\n', &mut line)?;
        if count == 0 {
            break;
        }
        if line.len() > 16384 {
            return Err("Adapter request too large".into());
        }
        let raw: Value = serde_json::from_slice(&line)?;
        let frame: Option<AdapterFrameRequest> = if raw.get("request").is_some() {
            Some(serde_json::from_value(raw.clone())?)
        } else {
            None
        };
        let request: AdapterRequest = if let Some(frame) = &frame {
            frame.request.clone()
        } else {
            serde_json::from_value(raw)?
        };
        let framing_ok = if let Some(frame) = &frame {
            frame.protocol == ADAPTER_VERSION
                && frame.channel_id.len() == 64
                && frame.channel_id.bytes().all(|b| b.is_ascii_hexdigit())
                && frame.request_id > 0
                && channel
                    .as_ref()
                    .is_none_or(|(id, last)| id == &frame.channel_id && frame.request_id > *last)
        } else {
            channel.is_none() && matches!(request.operation, AdapterOperation::Discover)
        };
        if framing_ok && let Some(frame) = &frame {
            channel = Some((frame.channel_id.clone(), frame.request_id));
        }
        let response = if request.protocol != ADAPTER_VERSION || !framing_ok {
            AdapterResponse::Error(ApiError::new(
                ErrorCode::UnsupportedVersion,
                "Unsupported adapter process protocol",
            ))
        } else {
            let actor = (
                request.instance_id.clone(),
                request.player_uuid.clone(),
                request.player_name.clone(),
            );
            if !request.bot_id.is_empty()
                && identities
                    .get(&request.bot_id)
                    .is_some_and(|existing| existing != &actor)
            {
                AdapterResponse::Error(ApiError::new(ErrorCode::Conflict, "Actor binding changed"))
            } else {
                if !request.bot_id.is_empty() {
                    identities.insert(request.bot_id.clone(), actor);
                }
                dispatch(&config, &native, &discovery, &mut players, &request)
            }
        };
        if let Some(frame) = frame {
            serde_json::to_writer(
                &mut output,
                &AdapterFrameResponse {
                    protocol: ADAPTER_VERSION,
                    channel_id: frame.channel_id,
                    request_id: frame.request_id,
                    instance_id: request.instance_id,
                    bot_id: request.bot_id,
                    player_uuid: request.player_uuid,
                    response,
                },
            )?;
        } else {
            serde_json::to_writer(&mut output, &response)?;
        }
        output.write_all(b"\n")?;
        output.flush()?;
    }
    Ok(())
}
fn dispatch(
    config: &Config,
    native: &Native,
    discovery: &Value,
    players: &mut HashMap<String, Player>,
    request: &AdapterRequest,
) -> AdapterResponse {
    let mut run = || -> Result<AdapterResponse> {
        match &request.operation {
            AdapterOperation::Discover => Ok(AdapterResponse::Info(ServerInfo {
                api_version: API_VERSION,
                instance_id: request.instance_id.clone(),
                minecraft_version: "1.21.1".into(),
                platform: "neoforge".into(),
                adapter_version: VERSION.into(),
                ready: true,
                capabilities: [
                    "minecraft:movement",
                    "minecraft:movement_frames",
                    "minecraft:finite_locomotion",
                    "minecraft:continuous_physics",
                    "minecraft:mining",
                    "minecraft:placement",
                    "minecraft:block_use",
                    "minecraft:item_use",
                    "minecraft:finite_item_use",
                    "minecraft:entities",
                    "minecraft:inventory",
                    "minecraft:crafting",
                    "minecraft:chat",
                    "minecraft:commands",
                    "minecraft:respawn",
                    routes::CAPABILITY,
                ]
                .into_iter()
                .filter(|c| {
                    config.allow_blocking_locomotion || !matches!(*c, "minecraft:movement_frames")
                })
                .map(String::from)
                .chain(
                    discovery["native_observation_features"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(String::from),
                )
                .chain(
                    discovery["observation_capabilities"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .filter(|s| registry_id(s))
                        .map(String::from),
                )
                .collect(),
                limits: json!({"process_protocol":ADAPTER_VERSION,"max_players":discovery["max_players"],"move_waypoint_blocks":3.5,"move_up_blocks":4,"move_down_blocks":4,"observation_radius":4,"entity_observation_radius":discovery["entity_observation_radius"],"finite_control_max_ticks":config.control_max_ticks,"blocking_locomotion":config.allow_blocking_locomotion,"observation_queries":({
                    let mut schemas=discovery["observation_query_schemas"].as_object().cloned().unwrap_or_default();
                    schemas.insert(routes::CAPABILITY.into(),routes::schema());
                    Value::Object(schemas)
                })}),
            })),
            AdapterOperation::Connect => {
                if players.contains_key(&request.bot_id) {
                    return Err(ApiError::new(
                        ErrorCode::Conflict,
                        "Player already connected",
                    ));
                }
                let player = connect(config, native, request, discovery)?;
                players.insert(request.bot_id.clone(), player);
                Ok(AdapterResponse::Connected)
            }
            AdapterOperation::Disconnect => {
                players.remove(&request.bot_id);
                Ok(AdapterResponse::Disconnected)
            }
            AdapterOperation::Observe => {
                if players
                    .get(&request.bot_id)
                    .is_none_or(|p| !p.alive.load(Ordering::Acquire))
                {
                    players.remove(&request.bot_id);
                    return Err(ApiError::new(
                        ErrorCode::SessionRequired,
                        "Character connection ended; reconnect without replay",
                    ));
                }
                let mut observation = players
                    .get(&request.bot_id)
                    .ok_or_else(|| {
                        ApiError::new(ErrorCode::SessionRequired, "Character disconnected")
                    })?
                    .physics
                    .observe()
                    .map_err(|_| {
                        ApiError::new(ErrorCode::NotReady, "Native observation unavailable")
                    })?;
                if let Some(player) = players.get(&request.bot_id) {
                    if let Some(job) = &player.interaction {
                        job.freshen(&mut observation);
                    }
                    if let Some(job) = &player.control {
                        observation.player["control"] = job.observation();
                    }
                    if let Some(job) = &player.interaction {
                        observation.player["interaction"] = job.observation();
                    }
                    if observation.world.get("chat").is_none() {
                        observation.world["chat"] = json!(
                            player
                                .chat
                                .lock()
                                .map_err(|_| ApiError::new(
                                    ErrorCode::Internal,
                                    "Chat observation unavailable"
                                ))?
                                .messages
                        );
                    }
                }
                Ok(AdapterResponse::Observation(observation))
            }
            AdapterOperation::ReadObservation { request: query } => {
                query.validate()?;
                if !players.contains_key(&request.bot_id) {
                    return Err(ApiError::new(
                        ErrorCode::SessionRequired,
                        "Player is not connected",
                    ));
                }
                if query.capability == routes::CAPABILITY {
                    routes::validate(&query.input).map_err(|e| {
                        ApiError::new(
                            ErrorCode::InvalidRequest,
                            format!("Movement preview input refused: {e}"),
                        )
                    })?;
                    let player = players.get_mut(&request.bot_id).ok_or_else(|| {
                        ApiError::new(ErrorCode::SessionRequired, "Player disconnected")
                    })?;
                    let busy = player.correcting.load(Ordering::Acquire)
                        || player.control.as_ref().is_some_and(control::Job::running);
                    let remaining = player
                        .route_preview_at
                        .map(|at| {
                            routes::INTERVAL_MS.saturating_sub(at.elapsed().as_millis() as u64)
                        })
                        .unwrap_or(0);
                    if busy || remaining > 0 {
                        if remaining == 0 {
                            player.route_preview_at = Some(Instant::now());
                        }
                        return Ok(AdapterResponse::ObservationData(
                            json!({"format":"minecraft-movement-options-v1","origin":null,"scene":"","options":[],"complete":false,"next_offset":null,"retry_after_ms":remaining.max(if busy{routes::INTERVAL_MS}else{100}),"busy":busy,"prediction_not_effect_proof":true}),
                        ));
                    }
                    player.route_preview_at = Some(Instant::now());
                    let before = native.observe(&request.player_uuid).map_err(|_| {
                        ApiError::new(
                            ErrorCode::NotReady,
                            "Movement preview observation unavailable",
                        )
                    })?;
                    let result = routes::page(&before, &query.input, config.control_max_ticks)
                        .map_err(|e| {
                            ApiError::new(
                                ErrorCode::InvalidRequest,
                                format!("Movement preview refused: {e}"),
                            )
                        })?;
                    player.route_preview_at = Some(Instant::now());
                    return Ok(AdapterResponse::ObservationData(result));
                }
                if !discovery["observation_capabilities"]
                    .as_array()
                    .is_some_and(|caps| {
                        caps.iter()
                            .any(|cap| cap.as_str() == Some(&query.capability))
                    })
                {
                    return Err(ApiError::new(
                        ErrorCode::UnsupportedCapability,
                        "Observation provider is not installed",
                    ));
                }
                native.call("observe-extension", json!({"player_uuid":request.player_uuid,"capability":query.capability,"input":query.input}))
                    .map(AdapterResponse::ObservationData)
                    .map_err(|_| ApiError::new(ErrorCode::InvalidRequest, "Observation query refused"))
            }
            AdapterOperation::Action { request: action } => {
                action.action.validate()?;
                if !config.allow_blocking_locomotion
                    && matches!(
                        action.action,
                        Action::Move { .. } | Action::MotionFrame { .. } | Action::Jump
                    )
                {
                    return Err(ApiError::new(
                        ErrorCode::UnsupportedCapability,
                        "This deployment requires cancellable start_move controls; blocking locomotion is disabled",
                    ));
                }
                let player = players.get_mut(&request.bot_id).ok_or_else(|| {
                    ApiError::new(ErrorCode::SessionRequired, "Player disconnected")
                })?;
                if let Action::CancelControl { action_id } = &action.action {
                    return player
                        .control
                        .as_mut()
                        .ok_or_else(|| {
                            ApiError::new(ErrorCode::NotFound, "No character control to cancel")
                        })?
                        .cancel(&action.action_id, action_id)
                        .map(AdapterResponse::Receipt);
                }
                if let Action::CancelUseItem { action_id } = &action.action {
                    return player
                        .interaction
                        .as_mut()
                        .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "No item use to cancel"))?
                        .cancel(&action.action_id, action_id)
                        .map(AdapterResponse::Receipt);
                }
                if player
                    .interaction
                    .as_ref()
                    .is_some_and(|job| job.needs_status())
                    && !matches!(
                        action.action,
                        Action::Chat { .. }
                            | Action::Look { .. }
                            | Action::StartMove { .. }
                            | Action::Sprint { .. }
                            | Action::Sneak { .. }
                            | Action::Respawn
                    )
                {
                    return Err(ApiError::new(
                        ErrorCode::Conflict,
                        "Inspect the active interaction, or cancel held item use before another interaction",
                    ));
                }
                if let Action::StartUseItem { max_ticks, .. } = &action.action
                    && *max_ticks > config.control_max_ticks
                {
                    return Err(ApiError::new(
                        ErrorCode::InvalidRequest,
                        "Item use exceeds the deployment finite-control limit",
                    ));
                }
                if player.control.as_ref().is_some_and(|c| c.running())
                    && matches!(
                        action.action,
                        Action::StartMove { .. }
                            | Action::Move { .. }
                            | Action::MotionFrame { .. }
                            | Action::Jump
                            | Action::Respawn
                    )
                {
                    return Err(ApiError::new(
                        ErrorCode::Conflict,
                        "Inspect/cancel the active finite locomotion control first",
                    ));
                }
                let correction_epoch = player.corrections.load(Ordering::Acquire);
                let observation_started = Instant::now();
                let before = native.observe(&request.player_uuid).map_err(|_| {
                    ApiError::new(
                        ErrorCode::NotReady,
                        "Native observation unavailable before dispatch",
                    )
                })?;
                let observation_ms = observation_started.elapsed().as_millis();
                effects::validate_item_use(&action.action, &before)?;
                if let Action::Craft { recipe, .. } = &action.action {
                    if !discovery["observation_capabilities"]
                        .as_array()
                        .is_some_and(|caps| caps.iter().any(|cap| cap == recipes::CAPABILITY))
                    {
                        return Err(ApiError::new(
                            ErrorCode::UnsupportedCapability,
                            "Matching server integration with minecraft:recipes is required for recipe-book crafting",
                        ));
                    }
                    let found=native.call("observe-extension",json!({"player_uuid":request.player_uuid,"capability":recipes::CAPABILITY,"input":{"recipe":recipe,"limit":1}})).map_err(|_|ApiError::new(ErrorCode::NotReady,"Runtime recipe validation unavailable; no gameplay packet sent"))?;
                    recipes::validate(recipe, &found)?;
                }
                if before.player["health"].as_f64().is_some_and(|h| h <= 0.)
                    && !matches!(
                        action.action,
                        Action::Respawn | Action::Chat { .. } | Action::Command { .. }
                    )
                {
                    return Err(ApiError::new(
                        ErrorCode::Conflict,
                        "Character is dead; explicitly respawn before physical gameplay",
                    ));
                }
                if let Action::StartMove { max_ticks, .. } = &action.action {
                    if *max_ticks > config.control_max_ticks {
                        return Err(ApiError::new(
                            ErrorCode::InvalidRequest,
                            "Control duration exceeds this deployment's advertised limit",
                        ));
                    }
                    let planning_started = Instant::now();
                    let job = control::Job::start(
                        player,
                        native.clone(),
                        before,
                        action,
                        correction_epoch,
                    )?;
                    let mut receipt = job.status(&action.action_id)?;
                    receipt.evidence["adapter_dispatch_timing"] = json!({"native_observation_ms":observation_ms,"planning_ms":planning_started.elapsed().as_millis(),"excludes":"wire delivery and game-effect verification"});
                    player.motion = None;
                    player.control = Some(job);
                    return Ok(AdapterResponse::Receipt(receipt));
                }
                match &action.action {
                    Action::MotionFrame { target } => {
                        motion_frame(&before, target, player.motion).map_err(|_| {
                            ApiError::new(
                                ErrorCode::InvalidRequest,
                                "Motion frame violates collision, gravity or trajectory continuity",
                            )
                        })?;
                    }
                    Action::Move { target, jump, .. } => {
                        if *jump && before.player["on_ground"] != true {
                            return Err(ApiError::new(
                                ErrorCode::Conflict,
                                "Jumping movement requires observed ground support",
                            ));
                        }
                        motion_plan(&before, [target.x, target.y, target.z], *jump).map_err(
                            |_| {
                                ApiError::new(
                                    ErrorCode::InvalidRequest,
                                    "No supported collision-safe trajectory reaches this waypoint",
                                )
                            },
                        )?;
                    }
                    Action::Jump => {
                        if before.player["on_ground"] != true {
                            return Err(ApiError::new(
                                ErrorCode::Conflict,
                                "Jump requires observed ground support",
                            ));
                        }
                        motion_plan(
                            &before,
                            position(&before).map_err(|_| {
                                ApiError::new(ErrorCode::NotReady, "Player position unavailable")
                            })?,
                            true,
                        )
                        .map_err(|_| {
                            ApiError::new(ErrorCode::InvalidRequest, "No supported jump trajectory")
                        })?;
                    }
                    Action::Dig { block, .. }
                    | Action::Place { block, .. }
                    | Action::UseBlock { block, .. } => {
                        if !(-30_000_000..=30_000_000).contains(&block.x)
                            || !(-30_000_000..=30_000_000).contains(&block.z)
                            || !(-2048..=2047).contains(&block.y)
                        {
                            return Err(ApiError::new(
                                ErrorCode::InvalidRequest,
                                "Block coordinates exceed the Java 767 wire range",
                            ));
                        }
                    }
                    Action::Extension { .. } => {
                        return Err(ApiError::new(
                            ErrorCode::UnsupportedCapability,
                            "No operator-installed adapter implements this extension",
                        ));
                    }
                    Action::ClickSlot {
                        menu_id, state_id, ..
                    } if before.menu["menu_id"] != *menu_id
                        || before.menu["state_id"] != *state_id =>
                    {
                        return Err(ApiError::new(
                            ErrorCode::Conflict,
                            "Menu revision changed; observe again",
                        ));
                    }
                    _ => {}
                }
                // A send failure may be partial. Never return a definite rejection after entering dispatch.
                let sent = player.action(&action.action, &before);
                if effects::managed(&action.action) {
                    let job = effects::Job::start(
                        player,
                        native.clone(),
                        before,
                        action,
                        sent.is_ok(),
                        config.control_max_ticks,
                        correction_epoch,
                    );
                    let receipt = job.status(&action.action_id)?;
                    player.interaction = Some(job);
                    return Ok(AdapterResponse::Receipt(receipt));
                }
                thread::sleep(Duration::from_millis(150));
                let after = native.observe(&request.player_uuid).ok();
                let verified = sent.is_ok()
                    && after
                        .as_ref()
                        .is_some_and(|o| verified_effect(&action.action, &before, o));
                Ok(AdapterResponse::Receipt(Receipt {
                    action_id: action.action_id.clone(),
                    outcome: if verified {
                        Outcome::Observed
                    } else {
                        Outcome::Uncertain
                    },
                    adapter_version: VERSION.into(),
                    evidence: json!({"before":compact(&action.action,&before),"after":after.as_ref().map(|o|compact(&action.action,o)),"dispatch_completed":sent.is_ok(),"native_effect_verified":verified}),
                    error: if verified {
                        None
                    } else {
                        Some(ApiError::new(
                            ErrorCode::UncertainOutcome,
                            "Native effect not verified; explicitly reconcile without replay",
                        ))
                    },
                }))
            }
            AdapterOperation::ActionStatus { action_id } => {
                let player = players.get(&request.bot_id).ok_or_else(|| {
                    ApiError::new(
                        ErrorCode::UncertainOutcome,
                        "Character history unavailable; reconcile without replay",
                    )
                })?;
                if let Some(job) = &player.interaction
                    && let Ok(receipt) = job.status(action_id)
                {
                    return Ok(AdapterResponse::Receipt(receipt));
                }
                player
                    .control
                    .as_ref()
                    .ok_or_else(|| {
                        ApiError::new(
                            ErrorCode::UncertainOutcome,
                            "Control history unavailable; reconcile without replay",
                        )
                    })?
                    .status(action_id)
                    .map(AdapterResponse::Receipt)
            }
            AdapterOperation::Reconcile { action_id } => {
                if let Some(job) = players
                    .get(&request.bot_id)
                    .and_then(|p| p.interaction.as_ref())
                    && let Ok(receipt) = job.status(action_id)
                    && (receipt.outcome.terminal() || receipt.outcome == Outcome::Running)
                {
                    return Ok(AdapterResponse::Receipt(receipt));
                }
                if let Some(job) = players
                    .get(&request.bot_id)
                    .and_then(|p| p.control.as_ref())
                    && let Ok(receipt) = job.status(action_id)
                    && (receipt.outcome.terminal() || receipt.outcome == Outcome::Running)
                {
                    return Ok(AdapterResponse::Receipt(receipt));
                }
                let first = native.observe(&request.player_uuid).map_err(|_| {
                    ApiError::new(ErrorCode::NotReady, "Native reconciliation unavailable")
                })?;
                thread::sleep(Duration::from_millis(150));
                let second = native.observe(&request.player_uuid).map_err(|_| {
                    ApiError::new(ErrorCode::NotReady, "Native reconciliation unavailable")
                })?;
                if first.player["inventory"] != second.player["inventory"]
                    || first.menu != second.menu
                {
                    return Err(ApiError::new(
                        ErrorCode::UncertainOutcome,
                        "Player state has not settled",
                    ));
                }
                if first.player["uuid"] != request.player_uuid
                    || second.player["uuid"] != request.player_uuid
                    || !first.player["dimension"].is_string()
                    || first.player["dimension"] != second.player["dimension"]
                    || first.player["using_item"] != second.player["using_item"]
                    || second.player["using_item"] == true
                {
                    return Err(ApiError::new(
                        ErrorCode::UncertainOutcome,
                        "Character binding or held input has not settled",
                    ));
                }
                if let Some(job) = players
                    .get(&request.bot_id)
                    .and_then(|p| p.interaction.as_ref())
                {
                    job.acknowledge_reconciliation(action_id);
                }
                Ok(AdapterResponse::Receipt(Receipt {
                    action_id: action_id.clone(),
                    outcome: Outcome::Reconciled,
                    adapter_version: VERSION.into(),
                    evidence: json!({"current":second,"historical_effect_verified":false,"replayed":false}),
                    error: None,
                }))
            }
        }
    };
    match run() {
        Ok(response) => response,
        Err(error) => AdapterResponse::Error(error),
    }
}
fn verified_effect(action: &Action, before: &Observation, after: &Observation) -> bool {
    match action {
        Action::Move { target, .. } | Action::MotionFrame { target } => {
            position(after).is_ok_and(|p| {
                (p[0] - target.x).abs() < 0.08
                    && (p[1] - target.y).abs() < 0.08
                    && (p[2] - target.z).abs() < 0.08
            })
        }
        Action::Look { yaw, pitch } => {
            after.player["yaw"]
                .as_f64()
                .is_some_and(|y| (y - f64::from(*yaw)).abs() < 0.5)
                && after.player["pitch"]
                    .as_f64()
                    .is_some_and(|p| (p - f64::from(*pitch)).abs() < 0.5)
        }
        Action::SelectSlot { slot } => after.player["selected"] == *slot,
        Action::Sprint { enabled } => after.player["sprinting"] == *enabled,
        Action::Sneak { enabled } => after.player["sneaking"] == *enabled,
        Action::Respawn => {
            before.player["health"].as_f64().is_some_and(|h| h <= 0.)
                && after.player["health"].as_f64().is_some_and(|h| h > 0.)
        }
        Action::Dig { block, stage, .. } => {
            let p = json!([block.x, block.y, block.z]);
            let changed = block_state(before, block)
                .zip(block_state(after, block))
                .is_some_and(|(b, a)| b != a);
            match stage {
                DigStage::Start => {
                    changed
                        || (after.player["mining"]["active"] == true
                            && after.player["mining"]["block"] == p)
                }
                DigStage::Finish => changed,
                DigStage::Cancel => {
                    before.player["mining"]["active"] == true
                        && after.player["mining"]["active"] == false
                }
            }
        }
        Action::Place { block, face, .. } => {
            let mut adjacent = block.clone();
            match face {
                0 => adjacent.y -= 1,
                1 => adjacent.y += 1,
                2 => adjacent.z -= 1,
                3 => adjacent.z += 1,
                4 => adjacent.x -= 1,
                5 => adjacent.x += 1,
                _ => return false,
            };
            block_state(before, &adjacent)
                .zip(block_state(after, &adjacent))
                .is_some_and(|(b, a)| b != a)
        }
        Action::UseBlock { block, face, .. } => {
            let mut adjacent = block.clone();
            match face {
                0 => adjacent.y -= 1,
                1 => adjacent.y += 1,
                2 => adjacent.z -= 1,
                3 => adjacent.z += 1,
                4 => adjacent.x -= 1,
                5 => adjacent.x += 1,
                _ => return false,
            };
            block_state(before, block)
                .zip(block_state(after, block))
                .is_some_and(|(b, a)| b != a)
                || block_state(before, &adjacent)
                    .zip(block_state(after, &adjacent))
                    .is_some_and(|(b, a)| b != a)
                || (before.menu["menu_id"] != after.menu["menu_id"] && after.menu["menu_id"] != 0)
        }
        Action::DropSelected { entire_stack } => {
            let selected = &before.player["selected"];
            let stack = |o: &Observation| {
                o.player["inventory"]
                    .as_array()
                    .and_then(|rows| rows.iter().find(|s| &s["slot"] == selected))
                    .cloned()
            };
            stack(before).zip(stack(after)).is_some_and(|(b, a)| {
                b["count"]
                    .as_u64()
                    .zip(a["count"].as_u64())
                    .is_some_and(|(old, new)| {
                        old > 0 && new == if *entire_stack { 0 } else { old - 1 }
                    })
            })
        }
        Action::ClickSlot { slot, .. } => {
            let get = |o: &Observation| {
                o.menu["slots"]
                    .as_array()
                    .and_then(|s| s.iter().find(|s| s["slot"] == *slot))
                    .cloned()
            };
            before.menu["menu_id"] == after.menu["menu_id"]
                && (get(before) != get(after) || before.menu["carried"] != after.menu["carried"])
        }
        Action::Craft { menu_id, .. } => {
            after.menu["menu_id"] == *menu_id && before.menu["slots"] != after.menu["slots"]
        }
        Action::CloseMenu { menu_id } => *menu_id != 0 && after.menu["menu_id"] == 0,
        Action::Chat { message } => {
            let baseline = before.player["accepted_chat"]
                .as_array()
                .and_then(|a| a.last())
                .and_then(|e| e["sequence"].as_i64())
                .unwrap_or(0);
            after.player["accepted_chat"]
                .as_array()
                .is_some_and(|events| {
                    events.iter().any(|e| {
                        e["message"] == *message
                            && e["sequence"].as_i64().is_some_and(|n| n > baseline)
                    })
                })
        }
        _ => false,
    }
}
fn block_state<'a>(observation: &'a Observation, pos: &BlockPos) -> Option<&'a Value> {
    observation.world["blocks"]
        .as_array()?
        .iter()
        .find(|b| b["position"] == json!([pos.x, pos.y, pos.z]))
        .map(|b| &b["state"])
}
fn compact(action: &Action, o: &Observation) -> Value {
    let positions = match action {
        Action::Dig { block, .. } | Action::UseBlock { block, .. } => {
            vec![[block.x, block.y, block.z]]
        }
        Action::Place { block, face, .. } => {
            let mut p = [block.x, block.y, block.z];
            match face {
                0 => p[1] -= 1,
                1 => p[1] += 1,
                2 => p[2] -= 1,
                3 => p[2] += 1,
                4 => p[0] -= 1,
                5 => p[0] += 1,
                _ => {}
            }
            vec![[block.x, block.y, block.z], p]
        }
        _ => Vec::new(),
    };
    let blocks: Vec<_> = o.world["blocks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| positions.iter().any(|p| b["position"] == json!(p)))
        .cloned()
        .collect();
    let entities:Vec<_>=o.world["entities"].as_array().into_iter().flatten()
        .filter(|row|matches!(action,Action::Attack{entity}|Action::Interact{entity,..} if row["entity_id"]==*entity))
        .cloned().collect();
    json!({"revision":o.revision,"player":o.player,"menu":o.menu,"world":{"game_time":o.world["game_time"],"day_time":o.world["day_time"],"sample_clock":o.world["sample_clock"],"blocks":blocks,"entities":entities}})
}

#[cfg(test)]
mod movement_tests {
    use super::*;
    #[test]
    fn correction_reports_authorized_absolute_position_and_observed_support() {
        let mut packet = Vec::new();
        for value in [12.5_f64, 1., -3.5] {
            packet.extend(value.to_be_bytes());
        }
        packet.extend([0; 8]);
        packet.push(0);
        packet.extend(vi(7));
        let o = Observation {
            revision: "fixture".into(),
            player: json!({"position":[12.5,1.,-3.5]}),
            world: json!({"blocks":[{"position":[12,0,-4],"collision":[[0.,0.,0.,1.,1.,1.]]}]}),
            menu: Value::Null,
        };
        let ack = correction_ack(&packet, Some(&o)).unwrap();
        assert_eq!(&ack[..24], &packet[..24]);
        assert_eq!(ack[24], 1);
        assert_eq!(correction_ack(&packet, None).unwrap()[24], 0);
        packet[32] = 1;
        let mut corrected = o.clone();
        corrected.player["position"] = json!([100., 64., -20.]);
        let ack = correction_ack(&packet, Some(&corrected)).unwrap();
        assert_eq!(f64::from_be_bytes(ack[..8].try_into().unwrap()), 100.);
        assert!(correction_ack(&packet, None).is_err());
        packet[32] = 0;
        packet[..8].copy_from_slice(&f64::NAN.to_be_bytes());
        assert!(correction_ack(&packet, Some(&o)).is_err());
    }
    #[test]
    fn finite_only_discovery_still_allows_sprint_and_sneak() {
        let cfg = Config {
            backend: "127.0.0.1:9".parse().unwrap(),
            native_endpoint: "http://127.0.0.1:9".into(),
            native_token: "fixture".into(),
            control_max_ticks: 100,
            allow_blocking_locomotion: false,
        };
        let native = Native {
            endpoint: cfg.native_endpoint.clone(),
            token: "fixture".into(),
            http: reqwest::blocking::Client::new(),
        };
        let request = AdapterRequest {
            protocol: ADAPTER_VERSION,
            instance_id: "fixture".into(),
            bot_id: String::new(),
            player_uuid: String::new(),
            player_name: String::new(),
            operation: AdapterOperation::Discover,
        };
        let AdapterResponse::Info(info) = dispatch(
            &cfg,
            &native,
            &json!({"max_players":10}),
            &mut HashMap::new(),
            &request,
        ) else {
            panic!("Discovery failed");
        };
        for action in [
            Action::Sprint { enabled: true },
            Action::Sneak { enabled: true },
            Action::StartMove {
                target: Position {
                    x: 1.,
                    y: 0.,
                    z: 0.,
                },
                jump: false,
                max_ticks: 10,
            },
        ] {
            assert!(info.capabilities.iter().any(|c| c == action.capability()));
        }
        assert!(
            !info
                .capabilities
                .iter()
                .any(|c| c == "minecraft:movement_frames")
        );
        assert_eq!(info.limits["blocking_locomotion"], false);
    }
    #[test]
    fn leaving_support_allows_one_level_frame_then_requires_gravity() {
        let mut o = Observation {
            revision: "test".into(),
            player: json!({"position":[1.25,1.,0.5],"on_ground":true}),
            world: json!({"blocks":[{"position":[0,0,0],"collision":[[0.,0.,0.,1.,1.,1.]]}]}),
            menu: json!({}),
        };
        let (p, ground, v) = motion_frame(
            &o,
            &Position {
                x: 1.5,
                y: 1.,
                z: 0.5,
            },
            None,
        )
        .unwrap();
        assert!(!ground);
        assert!((v + 0.0784).abs() < 1e-9);
        o.player["position"] = json!(p);
        o.player["on_ground"] = json!(false);
        assert!(
            motion_frame(
                &o,
                &Position {
                    x: 1.5,
                    y: 1.,
                    z: 0.5
                },
                Some((p, v))
            )
            .is_err()
        );
        let (fallen, _, _) = motion_frame(
            &o,
            &Position {
                x: 1.5,
                y: 0.9216,
                z: 0.5,
            },
            Some((p, v)),
        )
        .unwrap();
        assert!((fallen[1] - 0.9216).abs() < 1e-9);
        assert!(
            motion_frame(
                &o,
                &Position {
                    x: 1.5,
                    y: 1.42,
                    z: 0.5
                },
                None
            )
            .is_err()
        );
    }
    #[test]
    fn rounding_never_sends_requested_position_inside_collision() {
        let o = Observation {
            revision: "test".into(),
            player: json!({"position":[0.5,1.02,0.5],"on_ground":false}),
            world: json!({"blocks":[{"position":[0,0,0],"collision":[[0.,0.,0.,1.,1.,1.]]}]}),
            menu: json!({}),
        };
        let (p, ground, _) = motion_frame(
            &o,
            &Position {
                x: 0.5,
                y: 1.01,
                z: 0.5,
            },
            None,
        )
        .unwrap();
        assert_eq!(p[1], 1.);
        assert!(ground);
    }
    #[test]
    fn frames_enforce_collision_gravity_and_continuity() {
        let mut observation = Observation {
            revision: "test".into(),
            player: json!({"position":[0.5,1.,0.5],"on_ground":true}),
            world: json!({"blocks":[{"position":[0,0,0],"collision":[[0.,0.,0.,1.,1.,1.]]}]}),
            menu: json!({}),
        };
        let jump = Position {
            x: 0.5,
            y: 1.42,
            z: 0.5,
        };
        let (p, ground, velocity) = motion_frame(&observation, &jump, None).unwrap();
        assert!(!ground);
        observation.player["position"] = json!(p);
        observation.player["on_ground"] = json!(false);
        observation.player["on_ground"] = json!(true);
        assert!(
            motion_frame(
                &observation,
                &Position {
                    x: 0.5,
                    y: 1.84,
                    z: 0.5
                },
                Some((p, velocity))
            )
            .is_err(),
            "a premature native ground flag cannot authorize an airborne jump"
        );
        observation.player["on_ground"] = json!(false);
        assert!(
            motion_frame(
                &observation,
                &Position {
                    x: 0.5,
                    y: 1.84,
                    z: 0.5
                },
                Some((p, velocity))
            )
            .is_err(),
            "repeated upward jump must fail"
        );
        assert!(
            motion_frame(
                &observation,
                &Position {
                    x: 0.5,
                    y: p[1] + velocity,
                    z: 0.5
                },
                None
            )
            .is_err(),
            "untracked air frame must fail"
        );
        assert!(
            motion_frame(
                &observation,
                &Position {
                    x: 0.5,
                    y: p[1] + velocity,
                    z: 0.5
                },
                Some((p, velocity))
            )
            .is_ok()
        );
        observation.player["position"] = json!([0.5, 1., 0.5]);
        observation.player["on_ground"] = json!(true);
        observation.world["blocks"]
            .as_array_mut()
            .unwrap()
            .push(json!({"position":[1,1,0],"collision":[[0.,0.,0.,1.,1.,1.]]}));
        assert!(
            motion_frame(
                &observation,
                &Position {
                    x: 0.85,
                    y: 1.,
                    z: 0.5
                },
                None
            )
            .is_err(),
            "wall clipping must fail"
        );
    }
    #[test]
    fn explicit_jump_crosses_a_gap_while_grounded_walk_refuses_it() {
        let observation = Observation {
            revision: "fixture".into(),
            player: json!({"position":[0.5,1.,0.5]}),
            world: json!({"blocks":[
                {"position":[0,0,0],"collision":[[0.,0.,0.,1.,1.,1.]]},
                {"position":[2,0,0],"collision":[[0.,0.,0.,1.,1.,1.]]}
            ]}),
            menu: Value::Null,
        };
        assert!(motion_plan(&observation, [2.5, 1., 0.5], false).is_err());
        let frames = motion_plan(&observation, [2.5, 1., 0.5], true).unwrap();
        assert!(frames.iter().any(|(p, _)| p[1] > 2.));
        assert!(frames.last().unwrap().1);
    }
    #[test]
    fn short_drop_lands_on_observed_support_and_deeper_drop_is_refused() {
        let observation = Observation {
            revision: "fixture".into(),
            player: json!({"position":[0.5,4.,0.5]}),
            world: json!({"blocks":[
                {"position":[0,3,0],"collision":[[0.,0.,0.,1.,1.,1.]]},
                {"position":[1,0,0],"collision":[[0.,0.,0.,1.,1.,1.]]}
            ]}),
            menu: Value::Null,
        };
        let frames = motion_plan(&observation, [1.5, 1., 0.5], false).unwrap();
        let (last, grounded) = frames.last().unwrap();
        assert!(*grounded);
        assert!((last[1] - 1.).abs() < 1e-8);
        assert!(frames.iter().all(|(p, _)| p[1] >= 1.));
        assert!(motion_plan(&observation, [1.5, 0., 0.5], false).is_err());
    }
}
