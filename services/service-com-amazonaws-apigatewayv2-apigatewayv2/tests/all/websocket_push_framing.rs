// SPDX-License-Identifier: CC0-1.0

//! The FRAMES a WEBSOCKET api's socket writes a message to its client in, read off the wire
//! byte for byte over a real connection.
//!
//! The rule these tables are a slice of:
//!
//! > A message an API Gateway WebSocket api sends a client — the payload of a
//! > `PostToConnection` on the `@connections` endpoint, and the reply of a two-way route alike
//! > — travels in RFC 6455 data frames none of which carries more than the api's published
//! > frame quota: a payload at or under the quota is one final text frame, and a payload past
//! > it is FRAGMENTED over several (RFC 6455 §5.4 — the first frame carries the text opcode and
//! > is not final, every later frame continues it, and the last one ends the message), so the
//! > frames assemble back to exactly the bytes that were sent.
//!
//! The rows span what the rule generalises over: the payload's size against the quota (empty,
//! far under it, one byte under it, exactly at it, one byte past it, an exact multiple of it, a
//! size that is no multiple of it, and the modelled `Data` maximum), the bytes it carries
//! (single-byte text, multibyte characters whose own bytes a frame boundary falls inside, and
//! bytes that are not UTF-8 at all), and which half of the socket wrote it (an out-of-band
//! push, and a route's reply).
//!
//! A client library reassembles a fragmented message and then hides the frames that carried
//! it, so every frame here is written and read as raw bytes: that is the only way the quota a
//! frame was written under is observable at all.
//!
//! The neighbouring suites are not restated here: what the socket does with a client's own
//! frames is `websocket_message_refusals.rs` and `websocket_message_ceiling_framing.rs`, the
//! `@connections` answers themselves are `websocket_endpoint_connections.rs`, and the
//! handshake's own head is `websocket_stage_upgrade_counts.rs`.

use std::time::Duration;

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::websocket_endpoint_connections::{
    connections_call, emulator_with_connections, MAX_DATA_BYTES, STAGE,
};
use crate::websocket_handshake_data_plane::CLIENT_KEY;
use crate::websocket_message_refusals::FRAGMENT_BYTES as FRAME_QUOTA_BYTES;
use crate::websocket_transport_data_plane::{
    create_api, create_integration, create_integration_response, create_route, create_stage,
    dial_loopback, Api,
};

/// How long a case waits for the next bytes of a frame before failing: generous for a loaded
/// test machine, and bounded so a case that will never be answered fails instead of hanging
/// the suite.
const FRAME_DEADLINE: Duration = Duration::from_secs(30);

/// The opcodes RFC 6455 §11.8 registers for the frames a text message travels in: `0x1` for the
/// frame that starts it and `0x0` for every frame that continues it.
const TEXT_OPCODE: u8 = 0x1;
const CONTINUATION_OPCODE: u8 = 0x0;

/// The route key whose integration reply is read back as the REPLY half of the rule, and the
/// message that selects it under the api's `$request.body.action` selection expression.
const REPLY_ROUTE_KEY: &str = "quota";

/// The masking key the client frames below are masked with — "the client MUST mask all frames"
/// (RFC 6455 §5.3) — which is the one §5.7's own worked example uses.
const CLIENT_MASK: [u8; 4] = [0x37, 0xfa, 0x21, 0x3d];

/// One frame read off the socket: whether it ENDS the message, its opcode, and its payload.
#[derive(Debug)]
struct ReadFrame {
    fin: bool,
    opcode: u8,
    payload: Vec<u8>,
}

/// A client of the api's own socket that writes and reads RFC 6455 frames as BYTES.
struct RawClient {
    stream: TcpStream,
    /// Bytes read off the socket that no frame has consumed yet.
    read: Vec<u8>,
}

impl RawClient {
    /// Complete an opening handshake against `authority`'s `path` and answer the client that
    /// carries the established connection.
    async fn connect(case: &str, authority: &str, path: &str) -> Self {
        let mut stream = dial_loopback(case, authority).await.unwrap_or_else(|error| {
            panic!("case {case:?}: the announced socket {authority:?} must accept a connection: {error}")
        });
        let request = format!(
            "GET {path} HTTP/1.1\r\n\
             Host: {authority}\r\n\
             Connection: Upgrade\r\n\
             Upgrade: websocket\r\n\
             Sec-WebSocket-Key: {CLIENT_KEY}\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n"
        );
        stream
            .write_all(request.as_bytes())
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the handshake must be sent: {error}"));
        stream.flush().await.unwrap_or_else(|error| {
            panic!("case {case:?}: the handshake must be flushed: {error}")
        });
        let mut client = Self {
            stream,
            read: Vec::new(),
        };
        client.switched(case).await;
        client
    }

    /// Read the head the socket answered the handshake with, assert it SWITCHED the protocol,
    /// and leave every byte after it for the frames that follow.
    async fn switched(&mut self, case: &str) {
        const HEAD_TERMINATOR: &[u8] = b"\r\n\r\n";
        /// Far above the few hundred bytes of a server's handshake, so a socket that writes
        /// something else fails the case instead of being read forever.
        const MAX_HEAD_BYTES: usize = 16 * 1024;
        let end = loop {
            if let Some(at) = self
                .read
                .windows(HEAD_TERMINATOR.len())
                .position(|window| window == HEAD_TERMINATOR)
            {
                break at;
            }
            assert!(
                self.read.len() < MAX_HEAD_BYTES,
                "case {case:?}: the socket wrote {} bytes without ending a response head",
                self.read.len()
            );
            self.more(case).await;
        };
        let head = String::from_utf8_lossy(&self.read[..end]).to_string();
        let line = head.lines().next().unwrap_or_default().to_string();
        let status = line
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse::<u16>().ok());
        assert_eq!(
            status,
            Some(101),
            "case {case:?}: the opening handshake must be switched, got {line:?}"
        );
        self.read.drain(..end + HEAD_TERMINATOR.len());
    }

    /// Write one text MESSAGE as a client writes one: a single masked final text frame.
    async fn write_text(&mut self, case: &str, message: &str) {
        let payload = message.as_bytes();
        let mut frame = vec![0x80 | TEXT_OPCODE];
        let length = payload.len();
        if length < 126 {
            frame.push(0x80 | u8::try_from(length).expect("a length under 126 is one byte"));
        } else if let Ok(length) = u16::try_from(length) {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&length.to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(length as u64).to_be_bytes());
        }
        frame.extend_from_slice(&CLIENT_MASK);
        frame.extend(
            payload
                .iter()
                .enumerate()
                .map(|(at, byte)| byte ^ CLIENT_MASK[at % CLIENT_MASK.len()]),
        );
        self.stream
            .write_all(&frame)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the message must be sent: {error}"));
        self.stream
            .flush()
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the message must be flushed: {error}"));
    }

    /// Read ONE message as the frames that carried it: frames up to and including the one that
    /// ends it (RFC 6455 §5.4).
    async fn message(&mut self, case: &str) -> Vec<ReadFrame> {
        let mut frames = Vec::new();
        loop {
            let frame = self.frame(case).await;
            let ends = frame.fin;
            frames.push(frame);
            if ends {
                return frames;
            }
        }
    }

    /// Read one frame, header and payload, off the socket.
    async fn frame(&mut self, case: &str) -> ReadFrame {
        self.filled(case, 2).await;
        let fin = self.read[0] & 0x80 != 0;
        let opcode = self.read[0] & 0x0f;
        assert_eq!(
            self.read[1] & 0x80,
            0,
            "case {case:?}: a frame a server writes is never masked (RFC 6455 §5.1)"
        );
        let stated = usize::from(self.read[1] & 0x7f);
        let (length, header) = match stated {
            126 => {
                self.filled(case, 4).await;
                let length = u16::from_be_bytes([self.read[2], self.read[3]]);
                (usize::from(length), 4)
            }
            127 => {
                self.filled(case, 10).await;
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&self.read[2..10]);
                let length = u64::from_be_bytes(bytes);
                (
                    usize::try_from(length).unwrap_or_else(|_| {
                        panic!("case {case:?}: a frame of {length} bytes is not readable here")
                    }),
                    10,
                )
            }
            _ => (stated, 2),
        };
        self.filled(case, header + length).await;
        let payload = self.read[header..header + length].to_vec();
        self.read.drain(..header + length);
        ReadFrame {
            fin,
            opcode,
            payload,
        }
    }

    /// Read until at least `needed` bytes are buffered.
    async fn filled(&mut self, case: &str, needed: usize) {
        while self.read.len() < needed {
            self.more(case).await;
        }
    }

    /// Read one chunk of whatever the socket has, failing the case when it closes or answers
    /// nothing within [`FRAME_DEADLINE`].
    async fn more(&mut self, case: &str) {
        let mut chunk = [0u8; 8192];
        let read = tokio::time::timeout(FRAME_DEADLINE, self.stream.read(&mut chunk))
            .await
            .unwrap_or_else(|_elapsed| {
                panic!("case {case:?}: no bytes arrived within {FRAME_DEADLINE:?}")
            })
            .unwrap_or_else(|error| panic!("case {case:?}: the socket must be readable: {error}"));
        assert!(
            read > 0,
            "case {case:?}: the socket closed with {} bytes buffered",
            self.read.len()
        );
        self.read.extend_from_slice(&chunk[..read]);
    }
}

/// A deployed WEBSOCKET api whose `$default` route renders the connection's own id — which is
/// how a client learns the id the `@connections` calls address — and whose [`REPLY_ROUTE_KEY`]
/// route renders a reply of exactly `reply_bytes` bytes.
async fn deployed(
    emulator: &MultiServiceRouter,
    case: &str,
    name: &str,
    reply_bytes: usize,
) -> Api {
    let api = create_api(emulator, case, name).await;
    for (route_key, rendered) in [
        ("$default", "$context.connectionId".to_string()),
        (REPLY_ROUTE_KEY, "r".repeat(reply_bytes)),
    ] {
        let integration = create_integration(
            emulator,
            case,
            &api.api_id,
            json!({
                "integrationType": "MOCK",
                "requestTemplates": { "$default": "{\"statusCode\": 200}" }
            }),
        )
        .await;
        create_integration_response(
            emulator,
            case,
            &api.api_id,
            &integration,
            json!({
                "integrationResponseKey": "$default",
                "responseTemplates": { "$default": rendered }
            }),
        )
        .await;
        create_route(emulator, case, &api.api_id, route_key, &integration, true).await;
    }
    create_stage(emulator, case, &api.api_id, STAGE, true).await;
    api
}

/// Open a connection on the api's socket as raw bytes and learn the `ConnectionId` the
/// handshake minted, which the `$default` route renders.
async fn opened(case: &str, api: &Api) -> (RawClient, String) {
    let mut client = RawClient::connect(case, &api.authority, &format!("/{STAGE}")).await;
    client.write_text(case, r#"{"action":"whoami"}"#).await;
    let frames = client.message(case).await;
    let rendered: Vec<u8> = frames
        .iter()
        .flat_map(|frame| frame.payload.clone())
        .collect();
    let connection_id = String::from_utf8_lossy(&rendered).to_string();
    assert!(
        !connection_id.is_empty(),
        "case {case:?}: the connection must have an id the $default route renders"
    );
    (client, connection_id)
}

/// The frame sizes the rule owes a message of `payload_bytes` bytes: as many frames as the
/// frame quota needs, each at the quota except the last, and ONE empty frame for an empty
/// payload — a message of no bytes, not an absence of frames.
fn owed_sizes(payload_bytes: usize) -> Vec<usize> {
    let count = payload_bytes.div_ceil(FRAME_QUOTA_BYTES).max(1);
    (0..count)
        .map(|index| {
            let start = index * FRAME_QUOTA_BYTES;
            (start + FRAME_QUOTA_BYTES).min(payload_bytes) - start
        })
        .collect()
}

/// Assert `frames` are the frames the rule owes for `expected`: the sizes [`owed_sizes`] names,
/// the text opcode on the first and the continuation opcode on every later one, `FIN` on the
/// last one alone, and a reassembled payload equal to `expected`.
fn assert_framing(case: &str, frames: &[ReadFrame], expected: &[u8]) {
    let sizes: Vec<usize> = frames.iter().map(|frame| frame.payload.len()).collect();
    let owed = owed_sizes(expected.len());
    assert_eq!(
        sizes, owed,
        "case {case:?}: a message of {} bytes must travel in frames of {owed:?} bytes, got \
         {sizes:?}",
        expected.len()
    );
    for (index, frame) in frames.iter().enumerate() {
        assert!(
            frame.payload.len() <= FRAME_QUOTA_BYTES,
            "case {case:?}: frame {index} carries {} bytes, past the frame quota of \
             {FRAME_QUOTA_BYTES}",
            frame.payload.len()
        );
        let owed_opcode = if index == 0 {
            TEXT_OPCODE
        } else {
            CONTINUATION_OPCODE
        };
        assert_eq!(
            frame.opcode, owed_opcode,
            "case {case:?}: frame {index} must carry opcode {owed_opcode:#x}, got {:#x}",
            frame.opcode
        );
        let last = index + 1 == frames.len();
        assert_eq!(
            frame.fin, last,
            "case {case:?}: frame {index} of {} must {}end the message, got fin={}",
            frames.len(),
            if last { "" } else { "not " },
            frame.fin
        );
    }
    let reassembled: Vec<u8> = frames
        .iter()
        .flat_map(|frame| frame.payload.clone())
        .collect();
    assert_eq!(
        reassembled.len(),
        expected.len(),
        "case {case:?}: the frames must assemble to {} bytes, got {}",
        expected.len(),
        reassembled.len()
    );
    assert!(
        reassembled == expected,
        "case {case:?}: the frames must assemble to the bytes that were sent"
    );
}

/// `repeats` copies of a unit carrying two- and three-byte characters, so a frame boundary
/// inside one falls inside a CHARACTER — which RFC 6455 §5.4 permits, since a fragment is a
/// byte range and only the assembled message is text.
fn multibyte_of(repeats: usize) -> Vec<u8> {
    "héllo✓".repeat(repeats).into_bytes()
}

/// A payload pushed through `PostToConnection` reaches the client in frames within the
/// published frame quota, fragmented over as many as its size needs.
#[tokio::test]
async fn a_pushed_payload_travels_in_frames_within_the_published_frame_quota() {
    /// One row: the payload the `@connections` call sends.
    struct Row {
        case: &'static str,
        payload: Vec<u8>,
    }

    let rows = vec![
        Row {
            case: "the empty payload",
            payload: Vec::new(),
        },
        Row {
            case: "a short payload, far under the quota",
            payload: b"{\"hello\":\"socket\"}".to_vec(),
        },
        Row {
            case: "one byte under the frame quota",
            payload: vec![b'x'; FRAME_QUOTA_BYTES - 1],
        },
        Row {
            case: "exactly the frame quota",
            payload: vec![b'x'; FRAME_QUOTA_BYTES],
        },
        Row {
            case: "one byte past the frame quota",
            payload: vec![b'x'; FRAME_QUOTA_BYTES + 1],
        },
        Row {
            case: "twice the frame quota, an exact multiple of it",
            payload: vec![b'x'; 2 * FRAME_QUOTA_BYTES],
        },
        Row {
            case: "a payload that is no multiple of the quota",
            payload: vec![b'x'; 3 * FRAME_QUOTA_BYTES + 1_234],
        },
        Row {
            case: "exactly the modelled Data maximum",
            payload: vec![b'x'; MAX_DATA_BYTES],
        },
        Row {
            case: "multibyte characters, so a frame boundary falls inside one",
            payload: multibyte_of(5_000),
        },
        Row {
            // A text frame carries UTF-8, so bytes that are not valid UTF-8 travel with each
            // invalid sequence replaced — and the replacement is what the frames must carry.
            case: "bytes that are not UTF-8",
            payload: vec![0xff; 40_000],
        },
    ];

    let case = "pushed payloads of every size against the frame quota";
    let emulator = emulator_with_connections();
    let api = deployed(&emulator, case, "push-framing-api", 8).await;
    let (mut client, connection_id) = opened(case, &api).await;

    for row in rows {
        let case = row.case;
        let answer =
            connections_call(case, &api.authority, "POST", &connection_id, &row.payload).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: PostToConnection must answer 200, got {} {:?}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        let expected = String::from_utf8_lossy(&row.payload).into_owned().into_bytes();
        let frames = client.message(case).await;
        assert_framing(case, &frames, &expected);
    }
}

/// A two-way route's REPLY is written under the same quota: the largest reply a mapping
/// template can render (a `TemplateMap` value is "a string with a length between [0-32768]")
/// is exactly the quota, and it travels as one final text frame.
#[tokio::test]
async fn a_routes_reply_travels_in_frames_within_the_published_frame_quota() {
    let case = "a reply of exactly the frame quota";
    let emulator = emulator_with_connections();
    let api = deployed(&emulator, case, "reply-framing-api", FRAME_QUOTA_BYTES).await;
    let (mut client, _connection_id) = opened(case, &api).await;

    client
        .write_text(case, &json!({ "action": REPLY_ROUTE_KEY }).to_string())
        .await;
    let frames = client.message(case).await;
    assert_framing(case, &frames, &vec![b'r'; FRAME_QUOTA_BYTES]);
}
