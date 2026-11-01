use crate::{Error, Packet, Result, invalid};
use minecraft_sdk_wire::{FrameCodec, Limits, decode_varint_prefix, encode_varint};
use std::io::{Read, Write};

/// Java signed VarInt, two's complement and never ZigZag. Require minimal wire
/// form; negative values always occupy five bytes, exactly as Java's writer.
pub fn encode_signed_varint(value: i32) -> Vec<u8> {
    encode_varint(i64::from(value)).expect("every Java i32 has a wire encoding")
}
pub fn decode_signed_varint(input: &[u8]) -> Result<(i32, usize)> {
    let (bits, count) = decode_varint_prefix(input)?;
    let value = bits as i32;
    if encode_signed_varint(value).as_slice() != &input[..count] {
        return Err(invalid("Noncanonical Java VarInt"));
    }
    Ok((value, count))
}

/// Exactly one connection, preserving packet IDs and payload bytes while the
/// existing bridge-wire implementation owns bounded framing and compression.
/// Compression switches only after sending/receiving SetCompression itself.
pub struct Wire<S> {
    connection: S,
    codec: FrameCodec,
    sent_packets: u64,
    received_packets: u64,
    failed_partial_frame: bool,
}
impl<S> Wire<S> {
    pub fn new(connection: S) -> Self {
        Self {
            connection,
            codec: FrameCodec::new(None),
            sent_packets: 0,
            received_packets: 0,
            failed_partial_frame: false,
        }
    }
    pub fn connection(&self) -> &S {
        &self.connection
    }
    pub fn connection_mut(&mut self) -> &mut S {
        &mut self.connection
    }
    pub fn into_inner(self) -> S {
        self.connection
    }
    pub fn compression(&self) -> Option<u32> {
        self.codec.compression
    }
    pub fn set_compression(&mut self, threshold: Option<i32>) -> Result<()> {
        if threshold.is_some_and(|n| n < 0) {
            return Err(invalid("Negative compression threshold"));
        }
        self.codec.compression = threshold.map(|n| n as u32);
        Ok(())
    }
    pub fn set_limits(&mut self, limits: Limits) -> Result<()> {
        if limits.frame_bytes == 0
            || limits.decompressed_bytes == 0
            || limits.frame_bytes > minecraft_sdk_wire::WIRE_BUDGET
            || limits.decompressed_bytes > minecraft_sdk_wire::WIRE_BUDGET
        {
            return Err(invalid("Invalid Java session frame limits"));
        }
        self.codec.limits = limits;
        Ok(())
    }
    pub fn packet_counts(&self) -> (u64, u64) {
        (self.sent_packets, self.received_packets)
    }
}
impl<S: Read + Write> Wire<S> {
    pub fn send(&mut self, id: u32, payload: &[u8]) -> Result<()> {
        if self.failed_partial_frame {
            return Err(invalid(
                "Java connection closed after partial frame failure",
            ));
        }
        if id > i32::MAX as u32 {
            return Err(invalid("Negative Java packet ID"));
        }
        let frame = self.codec.encode(id, payload)?;
        self.connection.write_all(&frame)?;
        self.sent_packets = self.sent_packets.saturating_add(1);
        Ok(())
    }
    pub fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        self.send(packet.id, &packet.payload)
    }
    pub fn receive(&mut self) -> Result<Packet> {
        self.receive_using(|connection, bytes| {
            connection.read_exact(bytes)?;
            Ok(())
        })
    }
    fn receive_using(
        &mut self,
        mut exact: impl FnMut(&mut S, &mut [u8]) -> Result<()>,
    ) -> Result<Packet> {
        if self.failed_partial_frame {
            return Err(invalid(
                "Java connection closed after partial frame failure",
            ));
        }
        let mut header = Vec::with_capacity(5);
        for _ in 0..5 {
            let mut byte = [0];
            exact(&mut self.connection, &mut byte)?;
            header.push(byte[0]);
            if byte[0] & 128 == 0 {
                break;
            }
        }
        let (length, used) = decode_signed_varint(&header)?;
        if length < 0 || length as usize > self.codec.limits.frame_bytes {
            return Err(minecraft_sdk_wire::Error::FrameLimit.into());
        }
        if used != header.len() {
            return Err(invalid("Invalid Java frame prefix"));
        }
        let mut frame = header;
        frame.resize(frame.len() + length as usize, 0);
        exact(&mut self.connection, &mut frame[used..])?;
        // bridge-wire deliberately accepts redundant Java VarInt groups for
        // legacy diagnostic compatibility. The session boundary is stricter:
        // both framing VarInts and the packet ID use the Java writer's form.
        let decoded_length = if self.codec.compression.is_some() {
            let (declared, prefix) = decode_signed_varint(&frame[used..])?;
            if declared < 0 {
                return Err(invalid("Negative Java decompressed size"));
            }
            if declared == 0 {
                decode_signed_varint(&frame[used + prefix..])?;
                None
            } else {
                Some(declared as usize)
            }
        } else {
            decode_signed_varint(&frame[used..])?;
            None
        };
        let packet = self.codec.decode(&frame)?;
        if packet.id > i32::MAX as u32 {
            return Err(invalid("Negative Java packet ID"));
        }
        if decoded_length.is_some_and(|declared| {
            declared != encode_signed_varint(packet.id as i32).len() + packet.payload.len()
        }) {
            return Err(invalid("Noncanonical compressed Java packet ID"));
        }
        self.received_packets = self.received_packets.saturating_add(1);
        Ok(packet)
    }
}

impl Wire<std::net::TcpStream> {
    /// Receive one complete frame before an absolute monotonic deadline. Each
    /// individual socket read uses the remaining deadline, so partial traffic
    /// cannot renew a thirty-second timeout indefinitely. The existing socket
    /// timeout is restored on usable sockets; the codec and counters use the
    /// same path as `receive`. A platform may clear socket options after the
    /// shutdown of an unusable partial frame.
    ///
    /// This API requires a blocking TcpStream. An error after reading any frame
    /// bytes shuts down both directions: a consumed partial frame is never
    /// retried as a fresh packet. An already elapsed deadline consumes no bytes.
    pub fn receive_until(&mut self, deadline: std::time::Instant) -> Result<Packet> {
        let original_timeout = self.connection.read_timeout()?;
        let previous_count = self.received_packets;
        let mut consumed = 0usize;
        let mut result = self.receive_using(|connection, mut bytes| {
            while !bytes.is_empty() {
                let remaining = deadline
                    .checked_duration_since(std::time::Instant::now())
                    .filter(|remaining| !remaining.is_zero())
                    .ok_or(Error::Deadline)?;
                connection.set_read_timeout(Some(remaining))?;
                match connection.read(bytes) {
                    Ok(0) => {
                        if consumed == 0 {
                            return Err(Error::Closed);
                        }
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "Java peer closed during frame",
                        )
                        .into());
                    }
                    Ok(count) => {
                        consumed += count;
                        bytes = &mut bytes[count..];
                        check_deadline(deadline)?;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        return Err(Error::Deadline);
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            Ok(())
        });
        if result.is_ok() && std::time::Instant::now() >= deadline {
            self.received_packets = previous_count;
            result = Err(Error::Deadline);
        }
        if result.is_err() && consumed != 0 {
            self.failed_partial_frame = true;
            let _ = self.connection.shutdown(std::net::Shutdown::Both);
        }
        // Some TCP implementations clear SO_RCVTIMEO during shutdown, so
        // restoration must occur after invalidating a partially consumed frame.
        let restored = self.connection.set_read_timeout(original_timeout);
        match result {
            Ok(packet) => {
                restored?;
                Ok(packet)
            }
            Err(error) => Err(error),
        }
    }
}

/// Connect to the local native backend without sending any Minecraft bytes.
/// The owner must invoke its control-plane admit hook with source_port and the
/// pinned generation before calling BackendLogin::begin.
pub fn connect_backend(port: u16, timeout: std::time::Duration) -> Result<ConnectedBackend> {
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let connection = std::net::TcpStream::connect_timeout(&address, timeout)?;
    connection.set_read_timeout(Some(timeout))?;
    connection.set_write_timeout(Some(timeout))?;
    connection.set_nodelay(true)?;
    let source_port = connection.local_addr()?.port();
    Ok(ConnectedBackend {
        wire: Wire::new(connection),
        source_port,
        destination_port: port,
    })
}
pub struct ConnectedBackend {
    pub wire: Wire<std::net::TcpStream>,
    pub source_port: u16,
    pub destination_port: u16,
}

/// Generic bounded loop around a caller-controlled deadline. A socket's own
/// timeout must be configured by the IO owner; this never creates a busy loop.
pub fn check_deadline(deadline: std::time::Instant) -> Result<()> {
    if std::time::Instant::now() >= deadline {
        Err(Error::Deadline)
    } else {
        Ok(())
    }
}
