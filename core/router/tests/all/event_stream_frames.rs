// SPDX-License-Identifier: CC0-1.0

//! The ONE reading of an `application/vnd.amazon.eventstream` body every suite of this
//! test binary shares.
//!
//! A service crate's own suite reads that framing with the maintained decoder that crate
//! declares (`aws_smithy_eventstream::frame::read_message_from`). This test binary's
//! manifest carries no decoder and is not this change's to extend, and three suites had
//! each read the framing for themselves, so the reading lives here once instead of once
//! per suite.
//!
//! Nothing here is this module's invention: a frame is a 12-byte prelude (its total
//! length, its header-block length and their checksum), then the header block, the
//! payload and a trailing checksum, every length a big-endian `u32`; and one header is a
//! length-prefixed name, a type byte and — for the string type every event header uses —
//! a big-endian `u16` length and the value.

use serde_json::Value;

/// The type byte a string-valued event-stream header carries.
const STRING_HEADER_TYPE: u8 = 7;

/// One frame of an event-stream body, reduced to what a suite asserts on.
#[derive(Debug)]
pub(crate) struct Frame {
    /// `:message-type` — `event` for a modelled event, `exception` for an in-band error.
    pub message_type: String,
    /// `:event-type` — which event of the stream this frame carries.
    pub event_type: String,
    /// `:content-type` — the media type of this frame's OWN payload.
    pub payload_content_type: String,
    /// The payload as the JSON document it carries.
    pub payload: Value,
}

/// Every frame of `body`, in the order the stream carries them.
pub(crate) fn frames(case: &str, body: &[u8]) -> Vec<Frame> {
    let mut frames = Vec::new();
    let mut rest = body;
    while !rest.is_empty() {
        let length = |at: usize| {
            rest.get(at..at + 4)
                .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
                .map(u32::from_be_bytes)
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or_else(|| panic!("case {case:?}: a frame carries its prelude"))
        };
        let total = length(0);
        let header_block = length(4);
        assert!(
            total >= header_block + 16 && total <= rest.len(),
            "case {case:?}: a frame of {total} byte(s) does not fit the {} left",
            rest.len()
        );
        let headers = rest.get(12..header_block + 12).unwrap_or_else(|| {
            panic!("case {case:?}: a frame carries {header_block} header byte(s)")
        });
        let payload = rest
            .get(header_block + 12..total - 4)
            .unwrap_or_else(|| panic!("case {case:?}: a frame of {total} byte(s) carries a payload"));
        frames.push(Frame {
            message_type: header_value(case, headers, b":message-type"),
            event_type: header_value(case, headers, b":event-type"),
            payload_content_type: header_value(case, headers, b":content-type"),
            payload: serde_json::from_slice(payload).unwrap_or_else(|error| {
                panic!(
                    "case {case:?}: an event payload must be JSON ({error}), got {:?}",
                    String::from_utf8_lossy(payload)
                )
            }),
        });
        rest = rest.get(total..).unwrap_or_default();
    }
    frames
}

/// The value of the string header `wanted` in a frame's header block, or the empty string
/// when the block carries no header of that name.
fn header_value(case: &str, mut headers: &[u8], wanted: &[u8]) -> String {
    while let Some((&name_length, after_name_length)) = headers.split_first() {
        let name_length = usize::from(name_length);
        let name = after_name_length
            .get(..name_length)
            .unwrap_or_else(|| panic!("case {case:?}: a header carries its name"));
        let (&kind, after_kind) = after_name_length
            .get(name_length..)
            .and_then(<[u8]>::split_first)
            .unwrap_or_else(|| panic!("case {case:?}: a header carries its type"));
        assert_eq!(
            kind,
            STRING_HEADER_TYPE,
            "case {case:?}: expected the string type for the header {:?}, got type {kind}",
            String::from_utf8_lossy(name)
        );
        let value_length = after_kind
            .get(..2)
            .and_then(|bytes| <[u8; 2]>::try_from(bytes).ok())
            .map(|bytes| usize::from(u16::from_be_bytes(bytes)))
            .unwrap_or_else(|| panic!("case {case:?}: a string header carries its length"));
        let value = after_kind
            .get(2..2 + value_length)
            .unwrap_or_else(|| panic!("case {case:?}: a header carries its value"));
        if name == wanted {
            return String::from_utf8_lossy(value).to_string();
        }
        headers = after_kind.get(2 + value_length..).unwrap_or_default();
    }
    String::new()
}
