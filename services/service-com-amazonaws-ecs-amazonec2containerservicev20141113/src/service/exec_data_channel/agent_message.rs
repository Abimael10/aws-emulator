// SPDX-License-Identifier: CC0-1.0

//! The MESSAGE a session's data channel carries, and how one is read and written.
//!
//! An execute-command session is carried over the Systems Manager data channel the
//! `Session$streamUrl` names ("a URL to the managed agent on the container that the SSM
//! Session Manager client uses to send commands and receive output from the container"), and
//! what travels on that channel after the opening handshake is a sequence of binary
//! AGENT MESSAGES: a fixed header followed by one payload, the framing a Session Manager
//! client sends its keystrokes in and reads the container's output back from.
//!
//! The header is, in order: the header's own LENGTH as a four-byte number
//! ([`HEADER_LENGTH`]), the message TYPE in [`MESSAGE_TYPE_FIELD_BYTES`] bytes padded with
//! spaces, the schema VERSION, the instant it was CREATED as milliseconds since the epoch,
//! the SEQUENCE number, the FLAGS, the message's own 16-byte ID, the SHA-256 DIGEST of the
//! payload, and the payload's TYPE; the payload's LENGTH and then the payload itself follow
//! the header. Every number is big-endian, which is how a client reads them.
//!
//! What this module is NOT is a protocol of this emulator's own: the field order, the widths,
//! the message types and the payload types are the channel's, so a client that speaks to the
//! real service speaks to this one unchanged. Nothing here reads or writes a socket — the
//! connection is [`super`]'s — and nothing here decides anything about a session.

/// The bytes of header every message carries before its payload length: the four-byte
/// header length, the 32-byte type, the version, the created date, the sequence number, the
/// flags, the 16-byte id, the 32-byte digest and the payload type.
pub const HEADER_LENGTH: u32 = 116;

/// The width of the message-type field, which is padded with spaces to fill it.
pub const MESSAGE_TYPE_FIELD_BYTES: usize = 32;

/// The schema version a message is written with.
pub const SCHEMA_VERSION: u32 = 1;

/// The message a CLIENT sends its keystrokes, its terminal size and its handshake answer in.
pub const INPUT_STREAM_DATA: &str = "input_stream_data";

/// The message the agent sends the container's output, its handshake request and its
/// handshake completion in.
pub const OUTPUT_STREAM_DATA: &str = "output_stream_data";

/// The message either side sends to acknowledge one it received.
pub const ACKNOWLEDGE: &str = "acknowledge";

/// The message that says the channel is over, which is what ends a session whose shell has
/// exited.
pub const CHANNEL_CLOSED: &str = "channel_closed";

/// The payload of a message carrying STREAM DATA: a client's keystrokes, and the output the
/// container wrote.
pub const PAYLOAD_TYPE_OUTPUT: u32 = 1;

/// The payload of a message carrying a diagnostic of the session itself.
pub const PAYLOAD_TYPE_ERROR: u32 = 2;

/// The payload of a message carrying the client's TERMINAL SIZE, as `{"cols":…,"rows":…}`.
pub const PAYLOAD_TYPE_SIZE: u32 = 3;

/// The payload of the agent's handshake REQUEST, which states the session type it is about
/// to carry.
pub const PAYLOAD_TYPE_HANDSHAKE_REQUEST: u32 = 5;

/// The payload of the client's handshake RESPONSE to that request.
pub const PAYLOAD_TYPE_HANDSHAKE_RESPONSE: u32 = 6;

/// The payload of the agent's handshake COMPLETION, after which the session's own stream
/// starts.
pub const PAYLOAD_TYPE_HANDSHAKE_COMPLETE: u32 = 7;

/// The flag the FIRST message of a stream carries, so a client knows where that stream's
/// sequence starts.
pub const FLAG_FIRST: u64 = 1;

/// The flag the LAST message of a stream carries.
pub const FLAG_LAST: u64 = 2;

/// One agent message, as the channel carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentMessage {
    /// Which message this is ([`INPUT_STREAM_DATA`] and the types beside it).
    pub message_type: String,
    /// The schema version it was written with.
    pub schema_version: u32,
    /// The instant it was created, as milliseconds since the epoch.
    pub created_date: u64,
    /// Where it stands in its sender's stream.
    pub sequence_number: i64,
    /// The flags it carries ([`FLAG_FIRST`], [`FLAG_LAST`]).
    pub flags: u64,
    /// Its own id, which an acknowledgement names it by.
    pub message_id: [u8; 16],
    /// What KIND of payload it carries ([`PAYLOAD_TYPE_OUTPUT`] and the types beside it).
    pub payload_type: u32,
    /// The payload itself.
    pub payload: Vec<u8>,
}

impl AgentMessage {
    /// A message of `message_type` carrying `payload` of `payload_type`, stamped `created_date`
    /// and standing at `sequence_number` in its sender's stream.
    #[must_use]
    pub fn new(
        message_type: &str,
        sequence_number: i64,
        payload_type: u32,
        payload: Vec<u8>,
        message_id: [u8; 16],
        created_date: u64,
        flags: u64,
    ) -> Self {
        Self {
            message_type: message_type.to_string(),
            schema_version: SCHEMA_VERSION,
            created_date,
            sequence_number,
            flags,
            message_id,
            payload_type,
            payload,
        }
    }

    /// This message as the bytes the channel carries: the header, the payload length and the
    /// payload, with the payload's SHA-256 in the digest field the protocol puts it in.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes: Vec<u8> = Vec::with_capacity(
            HEADER_LENGTH as usize + ::std::mem::size_of::<u32>() + self.payload.len(),
        );
        bytes.extend_from_slice(&HEADER_LENGTH.to_be_bytes());
        let mut field = [b' '; MESSAGE_TYPE_FIELD_BYTES];
        for (slot, byte) in field
            .iter_mut()
            .zip(self.message_type.bytes().take(MESSAGE_TYPE_FIELD_BYTES))
        {
            *slot = byte;
        }
        bytes.extend_from_slice(&field);
        bytes.extend_from_slice(&self.schema_version.to_be_bytes());
        bytes.extend_from_slice(&self.created_date.to_be_bytes());
        bytes.extend_from_slice(&self.sequence_number.to_be_bytes());
        bytes.extend_from_slice(&self.flags.to_be_bytes());
        bytes.extend_from_slice(&self.message_id);
        bytes.extend_from_slice(&payload_digest(&self.payload));
        bytes.extend_from_slice(&self.payload_type.to_be_bytes());
        let length = u32::try_from(self.payload.len()).unwrap_or(u32::MAX);
        bytes.extend_from_slice(&length.to_be_bytes());
        bytes.extend_from_slice(&self.payload);
        bytes
    }

    /// The message `bytes` carry, and `None` for bytes that are not one: a frame shorter than
    /// the header it declares, a header shorter than the fields a message has, or a payload
    /// shorter than the length it states.
    ///
    /// The declared header length is READ rather than assumed, so a sender that writes a
    /// longer header — a later schema version — is read by the fields this one knows and its
    /// payload is still found where it says it is.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let declared = read_u32(bytes, 0)?;
        if declared < HEADER_LENGTH {
            return None;
        }
        let header = usize::try_from(declared).ok()?;
        let message_type = ::std::str::from_utf8(bytes.get(4..4 + MESSAGE_TYPE_FIELD_BYTES)?)
            .ok()?
            .trim()
            .to_string();
        let schema_version = read_u32(bytes, 36)?;
        let created_date = read_u64(bytes, 40)?;
        let sequence_number = read_u64(bytes, 48)? as i64;
        let flags = read_u64(bytes, 56)?;
        let mut message_id = [0u8; 16];
        message_id.copy_from_slice(bytes.get(64..80)?);
        // Bytes 80..112 are the payload's digest, which is recomputed from the payload below
        // rather than trusted: a sender's digest says nothing this reader needs.
        let payload_type = read_u32(bytes, 112)?;
        let length = usize::try_from(read_u32(bytes, header)?).ok()?;
        let start = header.checked_add(4)?;
        let end = start.checked_add(length)?;
        let payload = bytes.get(start..end)?.to_vec();
        Some(Self {
            message_type,
            schema_version,
            created_date,
            sequence_number,
            flags,
            message_id,
            payload_type,
            payload,
        })
    }

    /// The payload read as the text it is, and `None` for a payload that is not text.
    #[must_use]
    pub fn payload_text(&self) -> Option<&str> {
        ::std::str::from_utf8(&self.payload).ok()
    }

    /// This message's id as a client spells it: the hyphenated 8-4-4-4-12 form, with the two
    /// halves in the order the channel writes them — the least significant eight bytes come
    /// first on the wire, so the id reads back from the second half followed by the first.
    #[must_use]
    pub fn message_id_text(&self) -> String {
        let (least, most) = self.message_id.split_at(8);
        let digits: String = most
            .iter()
            .chain(least.iter())
            .map(|byte| format!("{byte:02x}"))
            .collect();
        [
            &digits[0..8],
            &digits[8..12],
            &digits[12..16],
            &digits[16..20],
            &digits[20..32],
        ]
        .join("-")
    }
}

/// The SHA-256 of a payload, which is the digest field of the message carrying it.
fn payload_digest(payload: &[u8]) -> [u8; 32] {
    use ::sha2::Digest as _;
    let mut hasher = ::sha2::Sha256::new();
    hasher.update(payload);
    hasher.finalize().into()
}

/// The four-byte big-endian number at `offset`, and `None` when the bytes do not reach it.
fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let slice = bytes.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_be_bytes(slice.try_into().ok()?))
}

/// The eight-byte big-endian number at `offset`, and `None` when the bytes do not reach it.
fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    let slice = bytes.get(offset..offset.checked_add(8)?)?;
    Some(u64::from_be_bytes(slice.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A message written and read back is the message it was: the framing is one encoding,
    /// so every field a client reads is the field the agent wrote. The rows span the message
    /// types and payload types this channel carries, an empty payload and a long one.
    #[test]
    fn a_message_reads_back_as_the_message_it_was_written_as() {
        let rows: [(&str, u32, Vec<u8>, i64, u64); 5] = [
            (OUTPUT_STREAM_DATA, PAYLOAD_TYPE_OUTPUT, b"hello\r\n".to_vec(), 0, FLAG_FIRST),
            (INPUT_STREAM_DATA, PAYLOAD_TYPE_OUTPUT, b"ls -l\n".to_vec(), 7, 0),
            (
                OUTPUT_STREAM_DATA,
                PAYLOAD_TYPE_HANDSHAKE_REQUEST,
                br#"{"AgentVersion":"3"}"#.to_vec(),
                1,
                0,
            ),
            (ACKNOWLEDGE, PAYLOAD_TYPE_OUTPUT, Vec::new(), 2, 0),
            (
                OUTPUT_STREAM_DATA,
                PAYLOAD_TYPE_OUTPUT,
                vec![b'x'; 70_000],
                i64::from(u32::MAX) + 1,
                FLAG_LAST,
            ),
        ];
        for (message_type, payload_type, payload, sequence, flags) in rows {
            let case = format!("{message_type} carrying {} payload bytes", payload.len());
            let message = AgentMessage::new(
                message_type,
                sequence,
                payload_type,
                payload.clone(),
                [9u8; 16],
                1_700_000_000_000,
                flags,
            );
            let encoded = message.encode();
            assert_eq!(
                &encoded[116..120],
                &u32::try_from(payload.len()).unwrap_or(u32::MAX).to_be_bytes(),
                "case {case:?}: the payload length stands after the header"
            );
            let read = AgentMessage::decode(&encoded)
                .unwrap_or_else(|| panic!("case {case:?}: expected the message to read back"));
            assert_eq!(
                read, message,
                "case {case:?}: expected the message it was written as, got {read:?}"
            );
        }
    }

    /// Bytes that are not a message read as none: a frame that is too short for the header,
    /// one declaring a header shorter than a message has, and one whose payload is shorter
    /// than it states.
    #[test]
    fn bytes_that_are_not_a_message_read_as_none() {
        let whole = AgentMessage::new(
            OUTPUT_STREAM_DATA,
            0,
            PAYLOAD_TYPE_OUTPUT,
            b"hello".to_vec(),
            [0u8; 16],
            1,
            0,
        )
        .encode();
        let mut short_header = whole.clone();
        short_header[0..4].copy_from_slice(&1u32.to_be_bytes());
        let rows: [(&str, Vec<u8>); 4] = [
            ("no bytes at all", Vec::new()),
            ("a frame cut off inside its header", whole[..40].to_vec()),
            ("a header shorter than a message has", short_header),
            ("a payload shorter than it states", whole[..whole.len() - 2].to_vec()),
        ];
        for (case, bytes) in rows {
            assert!(
                AgentMessage::decode(&bytes).is_none(),
                "case {case:?}: expected no message to be read"
            );
        }
    }

    /// The id a client spells for a message is the hyphenated form over the two halves the
    /// channel writes in the order it writes them.
    #[test]
    fn a_message_id_is_spelled_as_the_client_spells_it() {
        let mut id = [0u8; 16];
        for (index, slot) in id.iter_mut().enumerate() {
            *slot = u8::try_from(index).unwrap_or_default();
        }
        let message =
            AgentMessage::new(ACKNOWLEDGE, 0, PAYLOAD_TYPE_OUTPUT, Vec::new(), id, 0, 0);
        assert_eq!(
            message.message_id_text(),
            "08090a0b-0c0d-0e0f-0001-020304050607",
            "expected the second half of the written id to lead the spelled one"
        );
    }
}
