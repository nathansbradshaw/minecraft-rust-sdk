//! Operator-host-only Minecraft RCON. No arbitrary-command API or automatic retry.
//! The caller binds UUIDs to its frozen case and reads the password from a private
//! host file outside every bot mount. This module never logs authentication data.
use std::{
    fmt,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    time::{Duration, Instant},
};

const MAX_PACKET: usize = 65_536;
const MAX_REPLY: usize = 262_144;
// Vanilla Minecraft splits long command replies at 4096 characters. A bounded
// player-list sentinel must fit one such packet; otherwise do not leave an
// unread sentinel tail that could be mistaken for the next command's reply.
const MAX_SENTINEL_REPLY: usize = 1024;
const MAX_PACKETS: usize = 128;
const AUTH_ID: i32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EntityUuid(String);

impl EntityUuid {
    pub(super) fn parse(value: &str) -> Result<Self, Error> {
        let bytes = value.as_bytes();
        if bytes.len() != 36
            || bytes.iter().enumerate().any(|(i, b)| {
                if [8, 13, 18, 23].contains(&i) {
                    *b != b'-'
                } else {
                    !b.is_ascii_hexdigit()
                }
            })
        {
            return Err(Error::new("rcon_invalid_uuid"));
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }

    fn nbt(&self) -> String {
        let hex = self.0.replace('-', "");
        let words: Vec<String> = (0..4)
            .map(|i| {
                // parse() has already validated every byte and fixed separator.
                (u32::from_str_radix(&hex[i * 8..i * 8 + 8], 16).unwrap() as i32).to_string()
            })
            .collect();
        format!("[I;{}]", words.join(","))
    }
}

/// Fixed operator operations only. UUID ownership and fresh SDK/session evidence
/// are checked by the workflow; the console guard checks the actor again at use.
#[derive(Debug, Clone)]
pub(super) enum Command {
    ListPlayers,
    ReadPlayerHealth {
        actor: EntityUuid,
    },
    StageFreshPlayer {
        actor: EntityUuid,
        position: [f64; 3],
    },
    GuardedSummonSkeleton {
        actor: EntityUuid,
        npc: EntityUuid,
        actor_origin: [f64; 3],
        tolerance: f64,
        npc_position: [f64; 3],
    },
    ReadNpcPosition {
        npc: EntityUuid,
    },
    RemoveOwnedSkeleton {
        npc: EntityUuid,
    },
    QueryChunk {
        x: i32,
        z: i32,
    },
    /// Callers must record that a newly added load belongs to this run; removal
    /// must never include pre-existing or unowned forced chunks.
    SetChunk {
        x: i32,
        z: i32,
        loaded: bool,
    },
}

fn position(p: [f64; 3]) -> Result<String, Error> {
    if p.iter().any(|v| !v.is_finite())
        || p[0].abs() > 29_999_984.
        || p[2].abs() > 29_999_984.
        || !(-64.0..=319.0).contains(&p[1])
    {
        return Err(Error::new("rcon_invalid_overworld_position"));
    }
    // Fixed decimal syntax avoids command-parser exponent ambiguities. The
    // operator manifest/native admission tolerance must cover this sub-microblock
    // rounding; console text alone is never a native position receipt.
    Ok(format!("{:.6} {:.6} {:.6}", p[0], p[1], p[2]))
}

fn chunk(x: i32, z: i32) -> Result<(i32, i32), Error> {
    if x.unsigned_abs() > 1_874_998 || z.unsigned_abs() > 1_874_998 {
        return Err(Error::new("rcon_invalid_chunk"));
    }
    Ok((
        x.checked_mul(16).ok_or(Error::new("rcon_invalid_chunk"))?,
        z.checked_mul(16).ok_or(Error::new("rcon_invalid_chunk"))?,
    ))
}

impl Command {
    pub(super) fn render(&self) -> Result<String, Error> {
        let command = match self {
            Self::ListPlayers => "list".to_owned(),
            Self::ReadPlayerHealth { actor } => {
                format!("data get entity {} Health", actor.as_str())
            }
            Self::StageFreshPlayer { actor, position: p } => format!(
                "execute as {} if entity @s[type=minecraft:player,gamemode=survival,nbt={{Health:20.0f,foodLevel:20,Inventory:[]}}] run tp @s {} 0 0",
                actor.as_str(),
                position(*p)?
            ),
            Self::GuardedSummonSkeleton {
                actor,
                npc,
                actor_origin,
                tolerance,
                npc_position,
            } => {
                if actor == npc || !tolerance.is_finite() || !(0.001..=1.0).contains(tolerance) {
                    return Err(Error::new("rcon_invalid_spawn_guard"));
                }
                format!(
                    "execute in minecraft:overworld as {} positioned {} if entity @s[type=minecraft:player,gamemode=survival,distance=..{:.6},nbt={{Health:20.0f,foodLevel:20,Inventory:[]}}] run summon minecraft:skeleton {} {{UUID:{},PersistenceRequired:1b,HandItems:[{{id:\"minecraft:bow\",count:1}},{{}}],ArmorItems:[{{}},{{}},{{}},{{id:\"minecraft:iron_helmet\",count:1}}],HandDropChances:[0.0f,0.0f],ArmorDropChances:[0.0f,0.0f,0.0f,0.0f]}}",
                    actor.as_str(),
                    position(*actor_origin)?,
                    tolerance,
                    position(*npc_position)?,
                    npc.nbt()
                )
            }
            Self::ReadNpcPosition { npc } => format!(
                "execute as {} if entity @s[type=minecraft:skeleton] run data get entity @s Pos",
                npc.as_str()
            ),
            Self::RemoveOwnedSkeleton { npc } => format!(
                "execute as {} if entity @s[type=minecraft:skeleton] run kill @s",
                npc.as_str()
            ),
            Self::QueryChunk { x, z } => {
                let (x, z) = chunk(*x, *z)?;
                format!("execute in minecraft:overworld run forceload query {x} {z}")
            }
            Self::SetChunk { x, z, loaded } => {
                let (x, z) = chunk(*x, *z)?;
                let operation = if *loaded { "add" } else { "remove" };
                format!("execute in minecraft:overworld run forceload {operation} {x} {z}")
            }
        };
        if command.len() > 2048 || command.as_bytes().contains(&0) {
            return Err(Error::new("rcon_command_bounds"));
        }
        Ok(command)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Error {
    pub(super) code: &'static str,
    pub(super) request_id: Option<i32>,
    pub(super) may_have_executed: bool,
}
impl Error {
    const fn new(code: &'static str) -> Self {
        Self {
            code,
            request_id: None,
            may_have_executed: false,
        }
    }
    fn uncertain(mut self, id: i32) -> Self {
        self.request_id = Some(id);
        self.may_have_executed = true;
        self
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code)
    }
}
impl std::error::Error for Error {}

#[derive(Debug)]
pub(super) struct Reply {
    pub(super) request_id: i32,
    pub(super) text: String,
}

struct Packet {
    id: i32,
    kind: i32,
    payload: Vec<u8>,
}

fn remaining(deadline: Instant) -> Result<Duration, Error> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or(Error::new("rcon_deadline"))
}

fn write_bounded(stream: &mut TcpStream, bytes: &[u8], deadline: Instant) -> Result<(), Error> {
    let mut offset = 0;
    while offset < bytes.len() {
        stream
            .set_write_timeout(Some(remaining(deadline)?))
            .map_err(|_| Error::new("rcon_write_timeout_setup"))?;
        match stream.write(&bytes[offset..]) {
            Ok(0) => return Err(Error::new("rcon_closed")),
            Ok(n) => offset += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                return Err(Error::new("rcon_deadline"));
            }
            Err(_) => return Err(Error::new("rcon_write_failed")),
        }
    }
    remaining(deadline)?;
    Ok(())
}

fn read_bounded(stream: &mut TcpStream, bytes: &mut [u8], deadline: Instant) -> Result<(), Error> {
    let mut offset = 0;
    while offset < bytes.len() {
        stream
            .set_read_timeout(Some(remaining(deadline)?))
            .map_err(|_| Error::new("rcon_read_timeout_setup"))?;
        match stream.read(&mut bytes[offset..]) {
            Ok(0) => return Err(Error::new("rcon_closed")),
            Ok(n) => offset += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                return Err(Error::new("rcon_deadline"));
            }
            Err(_) => return Err(Error::new("rcon_read_failed")),
        }
    }
    remaining(deadline)?;
    Ok(())
}

fn send(
    stream: &mut TcpStream,
    id: i32,
    kind: i32,
    text: &str,
    deadline: Instant,
) -> Result<(), Error> {
    let length = text
        .len()
        .checked_add(10)
        .filter(|n| *n <= MAX_PACKET)
        .ok_or(Error::new("rcon_packet_bounds"))?;
    if text.as_bytes().contains(&0) {
        return Err(Error::new("rcon_embedded_nul"));
    }
    let mut bytes = Vec::with_capacity(length + 4);
    bytes.extend_from_slice(&(length as i32).to_le_bytes());
    bytes.extend_from_slice(&id.to_le_bytes());
    bytes.extend_from_slice(&kind.to_le_bytes());
    bytes.extend_from_slice(text.as_bytes());
    bytes.extend_from_slice(&[0, 0]);
    let result = write_bounded(stream, &bytes, deadline);
    // Authentication packets contain the private password. Do not retain a copy.
    if kind == 3 {
        bytes.fill(0);
    }
    result
}

fn receive(stream: &mut TcpStream, deadline: Instant) -> Result<Packet, Error> {
    let mut header = [0; 4];
    read_bounded(stream, &mut header, deadline)?;
    let length = i32::from_le_bytes(header);
    if !(10..=MAX_PACKET as i32).contains(&length) {
        return Err(Error::new("rcon_packet_bounds"));
    }
    let mut body = vec![0; length as usize];
    read_bounded(stream, &mut body, deadline)?;
    if body[body.len() - 2..] != [0, 0] {
        return Err(Error::new("rcon_invalid_terminator"));
    }
    Ok(Packet {
        id: i32::from_le_bytes(body[0..4].try_into().unwrap()),
        kind: i32::from_le_bytes(body[4..8].try_into().unwrap()),
        payload: body[8..body.len() - 2].to_vec(),
    })
}

pub(super) struct RconClient {
    stream: TcpStream,
    timeout: Duration,
    next_id: i32,
    usable: bool,
}

impl RconClient {
    pub(super) fn connect(
        address: SocketAddr,
        password: &str,
        timeout: Duration,
    ) -> Result<Self, Error> {
        if !address.ip().is_loopback() {
            return Err(Error::new("rcon_requires_loopback"));
        }
        if timeout.is_zero() || timeout > Duration::from_secs(30) {
            return Err(Error::new("rcon_invalid_timeout"));
        }
        if password.is_empty()
            || password.len() > 1024
            || password.bytes().any(|b| matches!(b, 0 | b'\r' | b'\n'))
        {
            return Err(Error::new("rcon_invalid_password"));
        }
        let deadline = Instant::now() + timeout;
        let mut stream = TcpStream::connect_timeout(&address, remaining(deadline)?)
            .map_err(|_| Error::new("rcon_connect_failed"))?;
        stream
            .set_nodelay(true)
            .map_err(|_| Error::new("rcon_socket_setup_failed"))?;
        send(&mut stream, AUTH_ID, 3, password, deadline)?;
        // Some servers send an empty response-value before auth-response.
        for _ in 0..4 {
            let reply = receive(&mut stream, deadline)?;
            if reply.id == -1 {
                return Err(Error::new("rcon_auth_failed"));
            }
            if reply.id != AUTH_ID {
                return Err(Error::new("rcon_auth_id_mismatch"));
            }
            if reply.kind == 2 && reply.payload.is_empty() {
                return Ok(Self {
                    stream,
                    timeout,
                    next_id: 2,
                    usable: true,
                });
            }
            if reply.kind != 0 || !reply.payload.is_empty() {
                return Err(Error::new("rcon_invalid_auth_response"));
            }
        }
        Err(Error::new("rcon_auth_packet_limit"))
    }

    pub(super) fn execute(&mut self, command: &Command) -> Result<Reply, Error> {
        if !self.usable {
            return Err(Error::new("rcon_connection_unusable"));
        }
        let text = command.render()?;
        let id = self.next_id;
        let sentinel = id
            .checked_add(1)
            .ok_or(Error::new("rcon_request_id_exhausted"))?;
        self.next_id = id
            .checked_add(2)
            .ok_or(Error::new("rcon_request_id_exhausted"))?;
        self.usable = false;
        let deadline = Instant::now() + self.timeout;
        let result = self.exchange(id, sentinel, &text, deadline);
        match result {
            Ok(reply) => {
                self.usable = true;
                Ok(reply)
            }
            Err(error) => Err(error.uncertain(id)),
        }
    }

    fn exchange(
        &mut self,
        id: i32,
        sentinel: i32,
        text: &str,
        deadline: Instant,
    ) -> Result<Reply, Error> {
        send(&mut self.stream, id, 2, text, deadline)?;
        // Minecraft responds to commands in order. A distinct, typed read-only
        // list request marks the end of even an exactly-full/multi-packet reply.
        // This avoids treating one TCP read or a short fragment as completion.
        let sentinel_command = Command::ListPlayers.render()?;
        send(&mut self.stream, sentinel, 2, &sentinel_command, deadline)?;
        let mut bytes = Vec::new();
        let mut seen = false;
        for _ in 0..MAX_PACKETS {
            let packet = receive(&mut self.stream, deadline)?;
            if packet.kind != 0 {
                return Err(Error::new("rcon_response_type"));
            }
            if packet.id == sentinel {
                if !seen {
                    return Err(Error::new("rcon_missing_command_response"));
                }
                if packet.payload.len() > MAX_SENTINEL_REPLY {
                    return Err(Error::new("rcon_sentinel_bounds"));
                }
                let text =
                    String::from_utf8(bytes).map_err(|_| Error::new("rcon_response_not_utf8"))?;
                return Ok(Reply {
                    request_id: id,
                    text,
                });
            }
            if packet.id != id {
                return Err(Error::new("rcon_response_id"));
            }
            seen = true;
            if bytes.len().saturating_add(packet.payload.len()) > MAX_REPLY {
                return Err(Error::new("rcon_reply_bounds"));
            }
            bytes.extend_from_slice(&packet.payload);
        }
        Err(Error::new("rcon_response_packet_limit"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, thread};

    fn actor() -> EntityUuid {
        EntityUuid::parse("00112233-4455-6677-8899-aabbccddeeff").unwrap()
    }
    fn npc() -> EntityUuid {
        EntityUuid::parse("e48187a8-4ca7-4108-8f54-f2c041bda900").unwrap()
    }
    fn server(f: impl FnOnce(TcpStream) + Send + 'static) -> (SocketAddr, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let task = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        f(stream);
                        return;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "fixture accept deadline");
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => panic!("fixture accept failed"),
                }
            }
        });
        (address, task)
    }
    fn auth(stream: &mut TcpStream, with_empty: bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        let packet = receive(stream, deadline).unwrap();
        assert_eq!((packet.id, packet.kind), (AUTH_ID, 3));
        assert_eq!(packet.payload, b"fixture-password");
        if with_empty {
            send(stream, AUTH_ID, 0, "", deadline).unwrap();
        }
        send(stream, AUTH_ID, 2, "", deadline).unwrap();
    }

    #[test]
    fn typed_commands_reject_injection_invalid_geometry_and_player_kills() {
        assert!(EntityUuid::parse("00112233-4455-6677-8899-aabbccddeeff;kill").is_err());
        assert_eq!(
            actor().nbt(),
            "[I;1122867,1146447479,-2003195205,-857870593]"
        );
        let stage = Command::StageFreshPlayer {
            actor: actor(),
            position: [262.5, 63., -720.5],
        }
        .render()
        .unwrap();
        assert!(stage.contains("gamemode=survival"));
        assert!(stage.contains("Health:20.0f,foodLevel:20,Inventory:[]"));
        assert!(
            Command::StageFreshPlayer {
                actor: actor(),
                position: [f64::NAN, 63., 0.]
            }
            .render()
            .is_err()
        );
        assert!(Command::QueryChunk { x: i32::MAX, z: 0 }.render().is_err());
        let cleanup = Command::RemoveOwnedSkeleton { npc: npc() }
            .render()
            .unwrap();
        assert!(cleanup.contains("if entity @s[type=minecraft:skeleton] run kill @s"));
        assert!(!cleanup.contains("kill @a"));
        assert_eq!(
            Command::ReadPlayerHealth { actor: actor() }.render().unwrap(),
            "data get entity 00112233-4455-6677-8899-aabbccddeeff Health"
        );
    }

    #[test]
    fn summon_is_guarded_and_fixed_to_the_declared_ordinary_variant() {
        let command = Command::GuardedSummonSkeleton {
            actor: actor(),
            npc: npc(),
            actor_origin: [262.5, 63., -720.5],
            tolerance: 0.5,
            npc_position: [262.5, 64., -709.5],
        };
        let text = command.render().unwrap();
        assert!(text.contains("distance=..0.500000"));
        assert!(text.contains("Health:20.0f,foodLevel:20,Inventory:[]"));
        assert!(text.contains("PersistenceRequired:1b"));
        assert!(text.contains("HandDropChances:[0.0f,0.0f]"));
        assert!(!text.contains("NoAI"));
        assert!(
            Command::GuardedSummonSkeleton {
                actor: actor(),
                npc: actor(),
                actor_origin: [262.5, 63., -720.5],
                tolerance: 0.5,
                npc_position: [262.5, 64., -709.5],
            }
            .render()
            .is_err()
        );
    }

    #[test]
    fn public_addresses_and_invalid_auth_inputs_never_connect() {
        assert_eq!(
            RconClient::connect(
                "203.0.113.1:25575".parse().unwrap(),
                "fixture-password",
                Duration::from_secs(1)
            )
            .err()
            .unwrap()
            .code,
            "rcon_requires_loopback"
        );
        assert_eq!(
            RconClient::connect(
                "127.0.0.1:9".parse().unwrap(),
                "secret\n",
                Duration::from_secs(1)
            )
            .err()
            .unwrap()
            .code,
            "rcon_invalid_password"
        );
    }

    #[test]
    fn auth_optional_empty_and_multi_packet_replies_are_fully_drained() {
        let (address, task) = server(|mut stream| {
            auth(&mut stream, true);
            let deadline = Instant::now() + Duration::from_secs(2);
            for expected in [2, 4] {
                let command = receive(&mut stream, deadline).unwrap();
                let sentinel = receive(&mut stream, deadline).unwrap();
                assert_eq!((command.id, command.kind), (expected, 2));
                assert_eq!((sentinel.id, sentinel.kind), (expected + 1, 2));
                assert_eq!(sentinel.payload, b"list");
                send(&mut stream, expected, 0, "first", deadline).unwrap();
                send(&mut stream, expected, 0, "second", deadline).unwrap();
                send(&mut stream, expected + 1, 0, "ignored list reply", deadline).unwrap();
            }
        });
        let mut client =
            RconClient::connect(address, "fixture-password", Duration::from_secs(2)).unwrap();
        assert_eq!(
            client.execute(&Command::ListPlayers).unwrap().text,
            "firstsecond"
        );
        assert_eq!(client.execute(&Command::ListPlayers).unwrap().request_id, 4);
        task.join().unwrap();
    }

    #[test]
    fn auth_failure_is_not_a_mutation_and_does_not_disclose_password() {
        let (address, task) = server(|mut stream| {
            let deadline = Instant::now() + Duration::from_secs(2);
            let _ = receive(&mut stream, deadline).unwrap();
            send(&mut stream, -1, 2, "", deadline).unwrap();
        });
        let error = RconClient::connect(address, "fixture-password", Duration::from_secs(1))
            .err()
            .unwrap();
        assert_eq!(error.code, "rcon_auth_failed");
        assert!(!error.may_have_executed);
        assert!(!error.to_string().contains("fixture-password"));
        task.join().unwrap();
    }

    #[test]
    fn malformed_response_is_uncertain_and_poisons_the_connection() {
        let (address, task) = server(|mut stream| {
            auth(&mut stream, false);
            let deadline = Instant::now() + Duration::from_secs(2);
            let _ = receive(&mut stream, deadline).unwrap();
            let _ = receive(&mut stream, deadline).unwrap();
            write_bounded(
                &mut stream,
                &((MAX_PACKET + 1) as i32).to_le_bytes(),
                deadline,
            )
            .unwrap();
        });
        let mut client =
            RconClient::connect(address, "fixture-password", Duration::from_secs(1)).unwrap();
        let error = client.execute(&Command::ListPlayers).unwrap_err();
        assert_eq!(error.code, "rcon_packet_bounds");
        assert_eq!(error.request_id, Some(2));
        assert!(error.may_have_executed);
        assert_eq!(
            client.execute(&Command::ListPlayers).unwrap_err().code,
            "rcon_connection_unusable"
        );
        task.join().unwrap();
    }

    #[test]
    fn oversized_sentinel_never_certifies_a_complete_command_reply() {
        let (address, task) = server(|mut stream| {
            auth(&mut stream, false);
            let deadline = Instant::now() + Duration::from_secs(2);
            let _ = receive(&mut stream, deadline).unwrap();
            let _ = receive(&mut stream, deadline).unwrap();
            send(&mut stream, 2, 0, "command response", deadline).unwrap();
            send(
                &mut stream,
                3,
                0,
                &"x".repeat(MAX_SENTINEL_REPLY + 1),
                deadline,
            )
            .unwrap();
        });
        let mut client =
            RconClient::connect(address, "fixture-password", Duration::from_secs(1)).unwrap();
        let error = client.execute(&Command::ListPlayers).unwrap_err();
        assert_eq!(error.code, "rcon_sentinel_bounds");
        assert!(error.may_have_executed);
        assert_eq!(
            client.execute(&Command::ListPlayers).unwrap_err().code,
            "rcon_connection_unusable"
        );
        task.join().unwrap();
    }

    #[test]
    fn trickled_response_cannot_reset_the_absolute_command_deadline() {
        let (address, task) = server(|mut stream| {
            auth(&mut stream, false);
            let deadline = Instant::now() + Duration::from_secs(2);
            let _ = receive(&mut stream, deadline).unwrap();
            let _ = receive(&mut stream, deadline).unwrap();
            stream.write_all(&10_i32.to_le_bytes()).unwrap();
            for byte in [2_u8, 0, 0, 0, 0, 0, 0, 0, 0, 0] {
                if stream.write_all(&[byte]).is_err() {
                    return;
                }
                thread::sleep(Duration::from_millis(25));
            }
        });
        let mut client =
            RconClient::connect(address, "fixture-password", Duration::from_millis(100)).unwrap();
        let started = Instant::now();
        let error = client.execute(&Command::ListPlayers).unwrap_err();
        assert_eq!(error.code, "rcon_deadline");
        assert!(error.may_have_executed);
        assert!(started.elapsed() < Duration::from_secs(1));
        task.join().unwrap();
    }
}
