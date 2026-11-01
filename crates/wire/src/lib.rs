//! Java1.21.1/767 diagnostic wire primitives. No Bedrock codec, socket, auth,
//! session, registry negotiation or game-state implementation.
use flate2::{Compression, Decompress, FlushDecompress, Status};
use std::io::{self, Write};

pub const JAVA_PROTOCOL: u32 = 767;
pub const WIRE_BUDGET: usize = 8 * 1024 * 1024;
pub const DIAGNOSTIC_STRING_BYTES: usize = 32767;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Truncated,
    VarIntOverflow,
    VarIntRange,
    StringLimit,
    InvalidUtf8,
    FrameLimit,
    DecompressedLimit,
    Compression,
    CompressionSize,
    CompressedBelowThreshold,
    Trailing,
    EmptyPacket,
    InvalidLimits,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
/// Legacy Python accepts [-2^31,2^32), encoding the low32bits without ZigZag.
pub fn encode_varint(value: i64) -> Result<Vec<u8>, Error> {
    if !(-(1i64 << 31)..(1i64 << 32)).contains(&value) {
        return Err(Error::VarIntRange);
    }
    let mut buffer = unsigned_varint::encode::u32_buffer();
    Ok(unsigned_varint::encode::u32(value as u32, &mut buffer).to_vec())
}
/// Unsigned identity matches legacy read_varint. Negative Java integers retain
/// their raw bit pattern; reinterpret as i32 only at a semantic field boundary.
pub fn decode_varint_prefix(input: &[u8]) -> Result<(u32, usize), Error> {
    let mut raw = [0u8; 5];
    let mut consumed = 0;
    for (index, slot) in raw.iter_mut().enumerate() {
        let byte = *input.get(index).ok_or(Error::Truncated)?;
        if index == 4 && byte > 15 {
            return Err(Error::VarIntOverflow);
        }
        *slot = byte;
        consumed = index + 1;
        if byte & 128 == 0 {
            break;
        }
    }
    if raw[consumed - 1] & 128 != 0 {
        return Err(Error::VarIntOverflow);
    }
    // The ecosystem decoder requires minimal form. Original Minecraft and this
    // bridge accept redundant zero groups, so normalize this bounded5byte copy
    // before delegation, preserving the original consumed prefix length.
    let mut minimal = consumed;
    while minimal > 1 && raw[minimal - 1] == 0 {
        minimal -= 1;
        raw[minimal - 1] &= 127;
    }
    let (value, rest) =
        unsigned_varint::decode::u32(&raw[..minimal]).map_err(|_| Error::VarIntOverflow)?;
    debug_assert!(rest.is_empty());
    Ok((value, consumed))
}
pub fn encode_string(value: &str) -> Result<Vec<u8>, Error> {
    if value.len() > WIRE_BUDGET {
        return Err(Error::StringLimit);
    }
    let mut output = encode_varint(value.len() as i64)?;
    output.extend_from_slice(value.as_bytes());
    Ok(output)
}
pub fn decode_string_prefix(input: &[u8]) -> Result<(&str, usize), Error> {
    decode_string_prefix_with_limit(input, DIAGNOSTIC_STRING_BYTES)
}
pub fn decode_string_prefix_with_limit(
    input: &[u8],
    max_bytes: usize,
) -> Result<(&str, usize), Error> {
    let (size, prefix) = decode_varint_prefix(input)?;
    let size = size as usize;
    if size > max_bytes {
        return Err(Error::StringLimit);
    }
    let end = prefix.checked_add(size).ok_or(Error::StringLimit)?;
    let text = std::str::from_utf8(input.get(prefix..end).ok_or(Error::Truncated)?)
        .map_err(|_| Error::InvalidUtf8)?;
    Ok((text, end))
}
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub frame_bytes: usize,
    pub decompressed_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            frame_bytes: WIRE_BUDGET,
            decompressed_bytes: WIRE_BUDGET,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub id: u32,
    pub payload: Vec<u8>,
}
struct BoundedOutput {
    bytes: Vec<u8>,
    maximum: usize,
}
impl Write for BoundedOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.maximum.saturating_sub(self.bytes.len()) {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "compressed frame cap",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn compression_error(error: io::Error) -> Error {
    if error.kind() == io::ErrorKind::WriteZero {
        Error::FrameLimit
    } else {
        Error::Compression
    }
}
/// Per-direction negotiated state. Send SetCompression with Disabled codec,
/// then switch only that direction's subsequent frames to the new threshold.
#[derive(Debug, Clone, Copy)]
pub struct FrameCodec {
    pub compression: Option<u32>,
    pub limits: Limits,
    pub validate_compressed_minimum: bool,
}
impl FrameCodec {
    pub fn new(compression: Option<u32>) -> Self {
        Self {
            compression,
            limits: Limits::default(),
            validate_compressed_minimum: true,
        }
    }
    fn validate_limits(&self) -> Result<(), Error> {
        if self.limits.frame_bytes == 0
            || self.limits.decompressed_bytes == 0
            || self.limits.frame_bytes > WIRE_BUDGET
            || self.limits.decompressed_bytes > WIRE_BUDGET
        {
            Err(Error::InvalidLimits)
        } else {
            Ok(())
        }
    }
    pub fn encode(&self, id: u32, payload: &[u8]) -> Result<Vec<u8>, Error> {
        self.validate_limits()?;
        let mut packet = encode_varint(id as i64)?;
        let size = packet
            .len()
            .checked_add(payload.len())
            .ok_or(Error::DecompressedLimit)?;
        if size > self.limits.decompressed_bytes {
            return Err(Error::DecompressedLimit);
        }
        packet.extend_from_slice(payload);
        let body = match self.compression {
            None => packet,
            Some(threshold) if size >= threshold as usize => {
                let mut body = encode_varint(size as i64)?;
                let maximum = self
                    .limits
                    .frame_bytes
                    .checked_sub(body.len())
                    .ok_or(Error::FrameLimit)?;
                let mut compressor = flate2::write::ZlibEncoder::new(
                    BoundedOutput {
                        bytes: Vec::new(),
                        maximum,
                    },
                    Compression::default(),
                );
                compressor.write_all(&packet).map_err(compression_error)?;
                let compressed = compressor.finish().map_err(compression_error)?;
                body.extend(compressed.bytes);
                body
            }
            Some(_) => {
                let mut body = vec![0];
                body.extend(packet);
                body
            }
        };
        if body.len() > self.limits.frame_bytes || body.len() > u32::MAX as usize {
            return Err(Error::FrameLimit);
        }
        let mut frame = encode_varint(body.len() as i64)?;
        frame.extend(body);
        Ok(frame)
    }
    pub fn decode_prefix(&self, input: &[u8]) -> Result<(Packet, usize), Error> {
        self.validate_limits()?;
        let (size, prefix) = decode_varint_prefix(input)?;
        let size = size as usize;
        if size > self.limits.frame_bytes {
            return Err(Error::FrameLimit);
        }
        let end = prefix.checked_add(size).ok_or(Error::FrameLimit)?;
        let body = input.get(prefix..end).ok_or(Error::Truncated)?;
        let packet = match self.compression {
            None => {
                if body.len() > self.limits.decompressed_bytes {
                    return Err(Error::DecompressedLimit);
                }
                body.to_vec()
            }
            Some(threshold) => {
                let (declared, header) = decode_varint_prefix(body)?;
                let declared = declared as usize;
                if declared > self.limits.decompressed_bytes {
                    return Err(Error::DecompressedLimit);
                }
                let encoded = &body[header..];
                if declared == 0 {
                    if encoded.len() > self.limits.decompressed_bytes {
                        return Err(Error::DecompressedLimit);
                    }
                    encoded.to_vec()
                } else {
                    if self.validate_compressed_minimum && declared < threshold as usize {
                        return Err(Error::CompressedBelowThreshold);
                    }
                    // One bounded sentinel byte detects dishonest output length/zipbombs.
                    let mut output =
                        vec![0; declared.checked_add(1).ok_or(Error::DecompressedLimit)?];
                    let mut decoder = Decompress::new(true);
                    let status = decoder
                        .decompress(encoded, &mut output, FlushDecompress::Finish)
                        .map_err(|_| Error::Compression)?;
                    if decoder.total_out() != declared as u64 {
                        return Err(Error::CompressionSize);
                    }
                    if status != Status::StreamEnd {
                        return Err(Error::Compression);
                    }
                    if decoder.total_in() != encoded.len() as u64 {
                        return Err(Error::Trailing);
                    }
                    output.truncate(declared);
                    output
                }
            }
        };
        if packet.is_empty() {
            return Err(Error::EmptyPacket);
        }
        let (id, id_bytes) = decode_varint_prefix(&packet)?;
        Ok((
            Packet {
                id,
                payload: packet[id_bytes..].to_vec(),
            },
            end,
        ))
    }
    pub fn decode(&self, input: &[u8]) -> Result<Packet, Error> {
        let (packet, consumed) = self.decode_prefix(input)?;
        if consumed != input.len() {
            return Err(Error::Trailing);
        }
        Ok(packet)
    }
}
