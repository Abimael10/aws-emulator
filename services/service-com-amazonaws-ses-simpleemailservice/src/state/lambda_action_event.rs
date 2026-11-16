// SPDX-License-Identifier: CC0-1.0

//! The event document Amazon SES hands the Amazon Web Services Lambda function a receipt
//! rule's `LambdaAction` names — what the handler of such a function parses.
//!
//! `LambdaAction` says what the action does: "this action calls an Amazon Web Services Lambda
//! function", and `LambdaAction$InvocationType` says how — "a value of `Event` means that the
//! function is invoked asynchronously", "`RequestResponse` means that the execution of the
//! function immediately results in a response". What the function is called WITH is the
//! received message: one record per received message, under the event source Amazon SES's own
//! events carry, holding the two objects every Amazon SES receipt event is made of —
//!
//! * `mail`: the message as it was accepted — the id it was accepted under, the envelope sender
//!   it is relayed from, the recipient addresses its own header block names, its headers in the
//!   order they are written (and whether that list was truncated), and the handful of them a
//!   reader reaches by name;
//! * `receipt`: what happened to it on the way in — the recipients the matching rule applied
//!   to, the verdict of each check Amazon SES made on it
//!   ([`super::received_scan::Verdicts`]), how long the processing took, and the ACTION this
//!   invocation is, which for a `LambdaAction` names the function and the invocation type the
//!   action carries.
//!
//! Every value here is READ off the message, off the rule that matched it and off the checks
//! made on it; nothing is asserted. The document is built once per invocation and handed to
//! Lambda as the invocation's payload ([`super::inbound_delivery`]), so a handler reading it
//! back reads what this account really received.

use std::time::{Duration, SystemTime};

use ::serde_json::{json, Map, Value};

/// The `eventSource` of an event Amazon SES's email receiving produces, which is how a handler
/// reading a record tells what produced it.
pub const EVENT_SOURCE: &str = "aws:ses";

/// The `eventVersion` of that record: the version of the event STRUCTURE, which is what a
/// handler written against this schema reads.
pub const EVENT_VERSION: &str = "1.0";

/// The `type` the receipt's action object carries for a `LambdaAction` — the action that
/// "calls an Amazon Web Services Lambda function" — which is how a handler tells which action
/// of the rule invoked it.
pub const LAMBDA_ACTION_TYPE: &str = "Lambda";

/// The `InvocationType` whose "execution of the function immediately results in a response"
/// (`LambdaAction$InvocationType`), which is the one value of that member that is not the
/// asynchronous invocation ([`crate::state::DEFAULT_LAMBDA_ACTION_INVOCATION_TYPE`]).
pub const INVOCATION_TYPE_REQUEST_RESPONSE: &str = "RequestResponse";

/// How many bytes of the message's header block the `mail` object reports.
///
/// The documented bound of the `headersTruncated` member: the headers "were truncated in the
/// notification, which will happen if the headers are larger than 10 KB". A message whose block
/// is longer is reported up to this many bytes, with the truncation declared, rather than
/// having the whole of a caller's header block copied into an event payload.
const MAX_REPORTED_HEADER_BYTES: usize = 10 * 1024;

/// The header fields `commonHeaders` reports as ADDRESS LISTS, each under the member name an
/// Amazon SES event names it by: a field the message does not carry contributes no member.
const COMMON_ADDRESS_HEADERS: [(&str, &str); 5] = [
    ("From", "from"),
    ("To", "to"),
    ("Cc", "cc"),
    ("Bcc", "bcc"),
    ("Reply-To", "replyTo"),
];

/// The header fields `commonHeaders` reports as single VALUES, under the same naming.
const COMMON_VALUE_HEADERS: [(&str, &str); 5] = [
    ("Return-Path", "returnPath"),
    ("Sender", "sender"),
    ("Date", "date"),
    ("Message-ID", "messageId"),
    ("Subject", "subject"),
];

/// The message one invocation is made for, as the receive path holds it.
#[derive(Debug, Clone, Copy)]
pub struct ReceivedMessage<'a> {
    /// The id the message was accepted under, which is the `mail` object's `messageId`.
    pub message_id: &'a str,
    /// The message's RFC 5322 bytes as the steps taken so far leave them, which is the header
    /// block the event reports.
    pub raw: &'a [u8],
    /// The recipients of this message the invoking rule applied to, which is the receipt's
    /// `recipients`.
    pub recipients: &'a [String],
    /// The envelope sender Amazon SES relays the message under, which is the `mail` object's
    /// `source`.
    pub envelope_from: &'a str,
    /// The instant the message was accepted, which is what both objects are stamped with.
    pub received_at: SystemTime,
    /// How long the message has been in the receive path by the time this invocation is made.
    pub processing_time: Duration,
    /// The verdicts of the checks made on the message.
    pub verdicts: super::received_scan::Verdicts,
}

/// The `LambdaAction` this invocation is, as the receipt's action object reports it.
#[derive(Debug, Clone, Copy)]
pub struct InvokedAction<'a> {
    /// `LambdaAction$FunctionArn`.
    pub function_arn: &'a str,
    /// `LambdaAction$InvocationType`, as the stored rule carries it.
    pub invocation_type: &'a str,
    /// `LambdaAction$TopicArn`, when the action names a topic to notify.
    pub topic_arn: Option<&'a str>,
}

/// The event document one invocation of `action` for `message` carries.
#[must_use]
pub fn event(message: &ReceivedMessage<'_>, action: &InvokedAction<'_>) -> Value {
    json!({
        "Records": [ record(message, action) ],
    })
}

/// The ONE record that event carries: Amazon SES invokes the function once per received
/// message, so a handler reading the envelope finds exactly one.
fn record(message: &ReceivedMessage<'_>, action: &InvokedAction<'_>) -> Value {
    json!({
        "eventVersion": EVENT_VERSION,
        "ses": {
            "mail": mail(message),
            "receipt": receipt(message, action),
        },
        "eventSource": EVENT_SOURCE,
    })
}

/// The `mail` object: the message as this account accepted it.
fn mail(message: &ReceivedMessage<'_>) -> Value {
    let reported = reported_headers(message.raw);
    json!({
        "timestamp": stamp(message.received_at),
        "source": message.envelope_from,
        "messageId": message.message_id,
        "destination": destination(&reported.headers),
        "headersTruncated": reported.truncated,
        "headers": reported
            .headers
            .iter()
            .map(|(name, value)| json!({"name": name, "value": value}))
            .collect::<Vec<Value>>(),
        "commonHeaders": common_headers(&reported.headers),
    })
}

/// The `receipt` object: what happened to the message on the way in.
fn receipt(message: &ReceivedMessage<'_>, action: &InvokedAction<'_>) -> Value {
    let verdicts = message.verdicts;
    json!({
        "timestamp": stamp(message.received_at),
        "processingTimeMillis": u64::try_from(message.processing_time.as_millis())
            .unwrap_or(u64::MAX),
        "recipients": message.recipients,
        "spamVerdict": { "status": verdicts.spam },
        "virusVerdict": { "status": verdicts.virus },
        "spfVerdict": { "status": verdicts.spf },
        "dkimVerdict": { "status": verdicts.dkim },
        "dmarcVerdict": { "status": verdicts.dmarc },
        "action": taken(action),
    })
}

/// The receipt's `action` object for a `LambdaAction`: what the action IS, and the members of
/// the action the rule carries — the function it calls, the invocation type it calls it with,
/// and the topic it notifies when the action names one.
fn taken(action: &InvokedAction<'_>) -> Value {
    let mut taken = Map::new();
    taken.insert("type".to_string(), json!(LAMBDA_ACTION_TYPE));
    if let Some(topic_arn) = action.topic_arn {
        taken.insert("topicArn".to_string(), json!(topic_arn));
    }
    taken.insert("functionArn".to_string(), json!(action.function_arn));
    taken.insert("invocationType".to_string(), json!(action.invocation_type));
    Value::Object(taken)
}

/// The header block the `mail` object reports, and whether it is all of it.
struct ReportedHeaders {
    /// The fields reported, in the order the message writes them.
    headers: Vec<(String, String)>,
    /// Whether fields past [`MAX_REPORTED_HEADER_BYTES`] were left out.
    truncated: bool,
}

/// The fields of `raw`'s own header block, in the order they are written, up to
/// [`MAX_REPORTED_HEADER_BYTES`] of them.
///
/// The reading is `mailparse`'s, the crate this service reads an RFC 5322 message with: the
/// name keeps the case it was written in, a folded field is unfolded into the one field it is,
/// and an encoded word is decoded the way a reader of the message decodes it. A message that
/// does not open with a header block at all reports none.
fn reported_headers(raw: &[u8]) -> ReportedHeaders {
    let Ok((headers, _body_at)) = ::mailparse::parse_headers(raw) else {
        return ReportedHeaders {
            headers: Vec::new(),
            truncated: false,
        };
    };
    let mut reported = Vec::new();
    let mut bytes = 0usize;
    let mut truncated = false;
    for header in &headers {
        let name = header.get_key();
        let value = header.get_value();
        bytes = bytes.saturating_add(name.len()).saturating_add(value.len());
        if bytes > MAX_REPORTED_HEADER_BYTES {
            truncated = true;
            break;
        }
        reported.push((name, value));
    }
    ReportedHeaders {
        headers: reported,
        truncated,
    }
}

/// The `mail` object's `destination`: "a complete list of all recipient addresses (including
/// `To:` and `Cc:` recipients) from the MIME headers of the incoming email message", in the
/// order those fields list them.
///
/// Each field is an RFC 5322 address list, so one field may name several mailboxes and may
/// group them behind a group name; every mailbox of it is a recipient the message names
/// ([`crate::state::header_addresses`]).
fn destination(headers: &[(String, String)]) -> Vec<String> {
    let mut addresses = Vec::new();
    for (name, value) in headers {
        if !["To", "Cc"]
            .iter()
            .any(|field| name.eq_ignore_ascii_case(field))
        {
            continue;
        }
        addresses.extend(super::header_addresses(value).unwrap_or_default());
    }
    addresses
}

/// The `commonHeaders` object: the fields of [`COMMON_ADDRESS_HEADERS`] and
/// [`COMMON_VALUE_HEADERS`] the message carries, each under the member name an event names it
/// by, in that order. A field the message does not carry contributes no member at all.
fn common_headers(headers: &[(String, String)]) -> Value {
    let mut common = Map::new();
    for (field, member) in COMMON_ADDRESS_HEADERS {
        let Some(value) = written(headers, field) else {
            continue;
        };
        let addresses = address_list(&value);
        if !addresses.is_empty() {
            common.insert(member.to_string(), json!(addresses));
        }
    }
    for (field, member) in COMMON_VALUE_HEADERS {
        if let Some(value) = written(headers, field) {
            common.insert(member.to_string(), json!(value));
        }
    }
    Value::Object(common)
}

/// The addresses one address field names, each AS AN ADDRESS — display name and all, which is
/// what `commonHeaders` reports for such a field.
///
/// Read with the RFC 5322 address reader this crate parses messages with (`mailparse`): a field
/// may name several mailboxes and may group them behind a group name, and each entry is rendered
/// back by that same reader. A value it cannot read as an address list at all is reported as the
/// one value the field carries, which is what the field says.
fn address_list(value: &str) -> Vec<String> {
    let Ok(parsed) = ::mailparse::addrparse(value) else {
        return vec![value.to_string()];
    };
    let mut addresses = Vec::new();
    for entry in parsed.iter() {
        match entry {
            ::mailparse::MailAddr::Single(single) => addresses.push(single.to_string()),
            ::mailparse::MailAddr::Group(group) => {
                addresses.extend(group.addrs.iter().map(ToString::to_string));
            }
        }
    }
    addresses
}

/// The value the message writes for the field `field`, compared without regard to case the way
/// a mail header name compares, and `None` when it carries none.
fn written(headers: &[(String, String)], field: &str) -> Option<String> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(field))
        .map(|(_, value)| value.clone())
}

/// `instant` as this event stamps one: RFC 3339, the format every timestamp of this emulator's
/// documents is written in.
fn stamp(instant: SystemTime) -> String {
    ::aws_smithy_types::DateTime::from(instant)
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_default()
}
