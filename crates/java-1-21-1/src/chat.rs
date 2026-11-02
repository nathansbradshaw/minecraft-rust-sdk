//! Java 767 last-seen acknowledgements and bounded inbox; no reply strategy or speech policy.
use minecraft_sdk_wire::{decode_string_prefix, decode_varint_prefix};
use serde_json::{Value, json};
use std::collections::VecDeque;
#[derive(Default)]
pub struct ChatState {
    seen: [bool; 20],
    offset: usize,
    pub pending: i32,
    sequence: u64,
    pub messages: VecDeque<Value>,
}
impl ChatState {
    /// Java 767's signed-command envelope also carries unsigned arguments for
    /// an admitted SDK connection. Never fabricate player signatures. The
    /// server's scoped decoder and ordinary command permissions remain final.
    pub fn command_payload(
        &self,
        command: &str,
        timestamp_ms: i64,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let mut bytes = minecraft_sdk_wire::encode_string(command)?;
        bytes.extend(timestamp_ms.to_be_bytes());
        bytes.extend(0i64.to_be_bytes());
        bytes.extend(minecraft_sdk_java_1_21_1::encode_signed_varint(0));
        bytes.extend(minecraft_sdk_java_1_21_1::encode_signed_varint(
            self.pending,
        ));
        bytes.extend(self.bits());
        Ok(bytes)
    }
    pub fn receive(&mut self, payload: &[u8]) -> Result<Option<i32>, Box<dyn std::error::Error>> {
        if payload.len() < 18 {
            return Err("Truncated player chat packet".into());
        }
        let (_, n) = decode_varint_prefix(&payload[16..])?;
        let mut at = 16 + n;
        let signed = match payload.get(at) {
            Some(0) => false,
            Some(1) => true,
            _ => return Err("Invalid chat signature flag".into()),
        };
        at += 1;
        if signed {
            at += 256;
        }
        let (text, n) = decode_string_prefix(payload.get(at..).ok_or("Truncated chat signature")?)?;
        if text.len() > 1024 {
            return Err("Chat text exceeds protocol budget".into());
        }
        at += n;
        let timestamp = i64::from_be_bytes(
            payload
                .get(at..at + 8)
                .ok_or("Chat timestamp missing")?
                .try_into()?,
        );
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("Chat sequence exhausted")?;
        let uuid = payload[..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        self.messages.push_back(json!({"sequence":self.sequence,"sender_uuid":uuid,"message":text,"timestamp":timestamp,"signature_verified":false}));
        while self.messages.len() > 16 {
            self.messages.pop_front();
        }
        if signed {
            self.seen[self.offset] = true;
            self.offset = (self.offset + 1) % 20;
            self.pending += 1;
        }
        if self.pending > 64 {
            let count = self.pending;
            self.pending = 0;
            Ok(Some(count))
        } else {
            Ok(None)
        }
    }
    pub fn bits(&self) -> [u8; 3] {
        let mut bits = 0u32;
        for i in 0..20 {
            if self.seen[(self.offset + i) % 20] {
                bits |= 1 << i;
            }
        }
        [bits as u8, (bits >> 8) as u8, (bits >> 16) as u8]
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_envelope_contains_time_empty_signatures_and_last_seen_update() {
        let state = ChatState {
            pending: 3,
            ..ChatState::default()
        };
        let bytes = state.command_payload("tell target hello", 123456).unwrap();
        let (command, prefix) = decode_string_prefix(&bytes).unwrap();
        assert_eq!(command, "tell target hello");
        assert_eq!(
            i64::from_be_bytes(bytes[prefix..prefix + 8].try_into().unwrap()),
            123456
        );
        assert_eq!(
            i64::from_be_bytes(bytes[prefix + 8..prefix + 16].try_into().unwrap()),
            0
        );
        let (signature_count, count_len) = decode_varint_prefix(&bytes[prefix + 16..]).unwrap();
        assert_eq!(signature_count, 0);
        let (offset, offset_len) = decode_varint_prefix(&bytes[prefix + 16 + count_len..]).unwrap();
        assert_eq!(offset, 3);
        assert_eq!(&bytes[prefix + 16 + count_len + offset_len..], &[0, 0, 0]);
    }
    #[test]
    fn signed_chat_acknowledgements_are_bounded_and_retained_across_sends() {
        let mut state = ChatState::default();
        let mut packet = vec![0; 16];
        packet.push(0);
        packet.push(1);
        packet.extend([0; 256]);
        packet.extend([1, b'x']);
        packet.extend(1i64.to_be_bytes());
        for i in 0..65 {
            assert_eq!(
                state.receive(&packet).unwrap(),
                if i == 64 { Some(65) } else { None }
            );
        }
        assert_eq!(state.messages.len(), 16);
        assert_eq!(state.bits(), [255, 255, 15]);
        assert_eq!(state.pending, 0);
    }
}
