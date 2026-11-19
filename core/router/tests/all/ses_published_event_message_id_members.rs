// SPDX-License-Identifier: CC0-1.0

//! The id a published email event reports the message under, over the whole mounted service
//! set — SESv2 publishes, SNS fans out, SQS holds what arrived.
//!
//! The RULE under test: a published email-event document identifies the message it is about by
//! the id the message was accepted under — the `MessageId` the send answered — both as
//! `mail.messageId` and as `mail.commonHeaders.messageId`, for every send and whatever form
//! its content took, because that id belongs to the acceptance rather than to the document the
//! caller submitted: a message carries one whether or not its own header block wrote a
//! `Message-ID` line, so a consumer correlating an event back to its send through either member
//! reads the same id. The rest of `commonHeaders` stays what the message carried: the addresses
//! it was accepted for, and the `Subject` and `Date` fields its header block wrote — a message
//! that wrote no `Date` has no `date` member at all.
//!
//! The rows span what the rule generalises over: the form the send's content takes (a simple
//! message whose header block SES assembles, a raw MIME document the caller wrote, a templated
//! message rendered from the request's own template), whether the message wrote a `Message-ID`
//! of its own, how it wrote it (in the MIME header block, or through the `Headers` the content
//! declares) and in which case it spelled the field name, and whether it wrote a `Date`. The
//! recorded case is the raw row that writes no `Message-ID`.
//!
//! The sibling suite [`crate::ses_send_events_reach_a_subscribed_queue`] pins WHICH events a
//! send produces and WHERE they go, and [`crate::ses_send_event_reports_what_answered`] what
//! each event reports of the reaction that produced it; this one pins the `mail` object's own
//! identification of the message, and reuses that wiring.

use serde_json::{json, Value};

use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{
    drain, drain_until, given_topic_with_subscriber, Grant,
};
use crate::ses_send_events_reach_a_subscribed_queue::{
    event_types, given_configuration_set, given_verified_sender, router, send_email, REGION,
    SENDER, SET,
};

/// The recipient every send below is addressed to: the mailbox simulator's accepting mailbox,
/// so each send produces an acceptance and a delivery and nothing else.
const RECIPIENT: &str = "success@simulator.amazonses.com";

/// The event types the one event destination of every row matches.
const MATCHING: &[&str] = &["SEND", "DELIVERY"];

/// The events one row's send publishes to that destination, in the order they happen.
const PUBLISHED: &[&str] = &["Send", "Delivery"];

/// The body every message under test carries.
const BODY: &str = "a body under test";

/// The subject each form of content carries, one per form so a row's document cannot be read
/// as another's.
const SIMPLE_SUBJECT: &str = "a simple message under test";
const RAW_SUBJECT: &str = "a raw message under test";
const TEMPLATE_SUBJECT: &str = "a templated message under test";

/// The `Date` field the one row that writes one writes, as RFC 5322 writes a date.
const WRITTEN_DATE: &str = "Mon, 2 Jan 2023 15:04:05 +0000";

/// The header block of a raw MIME document that writes NO `Message-ID`: the recorded shape of
/// a client's own document — an ordered block with a custom field among the standard ones.
const RAW_WITHOUT_MESSAGE_ID: &[(&str, &str)] = &[
    ("From", SENDER),
    ("To", RECIPIENT),
    ("Subject", RAW_SUBJECT),
    ("X-Custom-Header", "a value the caller wrote"),
    ("MIME-Version", "1.0"),
    ("Content-Type", "text/plain; charset=UTF-8"),
];

/// The same block, with a `Message-ID` field of the caller's own among it.
const RAW_WITH_MESSAGE_ID: &[(&str, &str)] = &[
    ("From", SENDER),
    ("To", RECIPIENT),
    ("Subject", RAW_SUBJECT),
    ("Message-ID", "<the-callers-own@senders.example.com>"),
    ("MIME-Version", "1.0"),
    ("Content-Type", "text/plain; charset=UTF-8"),
];

/// The same block again, writing the field name in lower case — a mail header field name
/// compares without regard to case, so this is the same field.
const RAW_WITH_LOWERCASE_MESSAGE_ID: &[(&str, &str)] = &[
    ("From", SENDER),
    ("To", RECIPIENT),
    ("Subject", RAW_SUBJECT),
    ("message-id", "<lower-case-field-name@senders.example.com>"),
];

/// A block that writes a `Date` of its own beside no `Message-ID` at all.
const RAW_WITH_DATE: &[(&str, &str)] = &[
    ("From", SENDER),
    ("To", RECIPIENT),
    ("Subject", RAW_SUBJECT),
    ("Date", WRITTEN_DATE),
];

/// The `Headers` a simple content declares for the row that writes its `Message-ID` that way:
/// "the list of message headers that will be added to the email message"
/// (`smithy_query(query="shape",shape="Message",model_path="services/service-com-amazonaws-sesv2-simpleemailservice-v2/smithy-model/com-amazonaws-sesv2-simpleemailservice-v2.json")`).
const SUPPLIED_MESSAGE_ID: &[(&str, &str)] =
    &[("Message-ID", "<supplied-by-parameter@senders.example.com>")];

/// What the message's own header block is, and the form the send's `Content` takes.
enum Written {
    /// `Content.Simple`: SES assembles the header block from the request's members, plus the
    /// `Headers` the content declares.
    Simple(&'static [(&'static str, &'static str)]),
    /// `Content.Raw`: the message IS the MIME document, opening with these fields in this
    /// order.
    Raw(&'static [(&'static str, &'static str)]),
    /// `Content.Template`: the message is rendered from the template the request carries.
    Template,
}

/// One row: a send whose content takes one form, and what the documents published for it must
/// report about the message.
struct Row {
    case: &'static str,
    written: Written,
    /// The `commonHeaders.subject` each document must carry.
    subject: &'static str,
    /// The `commonHeaders.date` each document must carry, or `None` for a message that wrote
    /// no `Date` field — which must then carry no `date` member at all.
    date: Option<&'static str>,
    /// The `Message-ID` field the message itself wrote, as `(name, value)` — still reported
    /// among the message's own headers, under the name as written.
    wrote_message_id: Option<(&'static str, &'static str)>,
}

const ROWS: &[Row] = &[
    Row {
        case: "a simple content, whose header block SES assembles",
        written: Written::Simple(&[]),
        subject: SIMPLE_SUBJECT,
        date: None,
        wrote_message_id: None,
    },
    Row {
        case: "a raw MIME document that writes no Message-ID",
        written: Written::Raw(RAW_WITHOUT_MESSAGE_ID),
        subject: RAW_SUBJECT,
        date: None,
        wrote_message_id: None,
    },
    Row {
        case: "a raw MIME document that writes a Message-ID of its own",
        written: Written::Raw(RAW_WITH_MESSAGE_ID),
        subject: RAW_SUBJECT,
        date: None,
        wrote_message_id: Some(("Message-ID", "<the-callers-own@senders.example.com>")),
    },
    Row {
        case: "a raw MIME document that writes the field name in lower case",
        written: Written::Raw(RAW_WITH_LOWERCASE_MESSAGE_ID),
        subject: RAW_SUBJECT,
        date: None,
        wrote_message_id: Some(("message-id", "<lower-case-field-name@senders.example.com>")),
    },
    Row {
        case: "a raw MIME document that writes a Date of its own",
        written: Written::Raw(RAW_WITH_DATE),
        subject: RAW_SUBJECT,
        date: Some(WRITTEN_DATE),
        wrote_message_id: None,
    },
    Row {
        case: "a simple content whose declared headers supply a Message-ID",
        written: Written::Simple(SUPPLIED_MESSAGE_ID),
        subject: SIMPLE_SUBJECT,
        date: None,
        wrote_message_id: Some((
            "Message-ID",
            "<supplied-by-parameter@senders.example.com>",
        )),
    },
    Row {
        case: "a templated content, rendered from the template the request carries",
        written: Written::Template,
        subject: TEMPLATE_SUBJECT,
        date: None,
        wrote_message_id: None,
    },
];

impl Row {
    /// The `SendEmail` request this row sends.
    fn request(&self) -> Value {
        json!({
            "FromEmailAddress": SENDER,
            "ConfigurationSetName": SET,
            "Destination": {"ToAddresses": [RECIPIENT]},
            "Content": self.content(),
        })
    }

    /// The `Content` of that request, in the form this row is about.
    fn content(&self) -> Value {
        match &self.written {
            Written::Simple(headers) => json!({"Simple": {
                "Subject": {"Data": self.subject},
                "Body": {"Text": {"Data": BODY}},
                "Headers": declared(headers),
            }}),
            Written::Raw(fields) => {
                json!({"Raw": {"Data": ::aws_smithy_types::base64::encode(mime(fields))}})
            }
            Written::Template => json!({"Template": {
                "TemplateContent": {"Subject": self.subject, "Text": BODY},
                "TemplateData": "{}",
            }}),
        }
    }
}

/// `fields` as the `MessageHeader` list a content declares them in.
fn declared(fields: &[(&str, &str)]) -> Vec<Value> {
    fields
        .iter()
        .map(|(name, value)| json!({"Name": name, "Value": value}))
        .collect()
}

/// A MIME document whose header block is `fields`, in that order, followed by the one blank
/// line that separates the block from the body.
fn mime(fields: &[(&str, &str)]) -> String {
    let mut document = String::new();
    for (name, value) in fields {
        document.push_str(name);
        document.push_str(": ");
        document.push_str(value);
        document.push_str("\r\n");
    }
    document.push_str("\r\n");
    document.push_str(BODY);
    document
}

/// A string at `pointer` inside `document`, or `None` when it carries nothing there.
fn text(document: &Value, pointer: &str) -> Option<String> {
    document
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[tokio::test]
async fn every_published_event_identifies_the_message_by_the_id_it_was_accepted_under() {
    for row in ROWS {
        let case = row.case;
        let (router, engine) = router();
        let (topic, queue) = given_topic_with_subscriber(
            &router,
            case,
            REGION,
            "ses-published-message-id",
            Grant::Unscoped,
        )
        .await;
        given_verified_sender(&router, &engine, case).await;
        given_configuration_set(&router, case, &topic, true, MATCHING).await;

        let message_id = send_email(&router, case, row.request()).await;
        let mut events = drain_until(&router, case, REGION, &queue, PUBLISHED.len()).await;
        // One more read, so a row whose send published MORE than the events expected fails on
        // the comparison rather than passing on timing.
        events.extend(drain(&router, case, REGION, &queue).await);
        let mut arrived = event_types(&events);
        arrived.sort();
        let mut wanted: Vec<String> = PUBLISHED.iter().map(|at| at.to_string()).collect();
        wanted.sort();
        assert_eq!(
            arrived, wanted,
            "case {case:?}: expected exactly {wanted:?} on the queue, got {events:?}"
        );

        for event in &events {
            let of = text(event, "/eventType").unwrap_or_default();
            assert_eq!(
                text(event, "/mail/messageId").as_deref(),
                Some(message_id.as_str()),
                "case {case:?}: the {of} event must name the message the send answered, got \
                 {event}"
            );
            assert_eq!(
                text(event, "/mail/commonHeaders/messageId").as_deref(),
                Some(message_id.as_str()),
                "case {case:?}: the {of} event's commonHeaders must report the id the message \
                 was accepted under, got {event}"
            );
            assert_eq!(
                text(event, "/mail/commonHeaders/subject").as_deref(),
                Some(row.subject),
                "case {case:?}: the {of} event's commonHeaders must report the subject the \
                 message carried, got {event}"
            );
            assert_eq!(
                text(event, "/mail/commonHeaders/date").as_deref(),
                row.date,
                "case {case:?}: the {of} event's commonHeaders must report the date the \
                 message wrote, and no date member for a message that wrote none, got {event}"
            );
            let reported = event
                .pointer("/mail/headers")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if let Some((name, value)) = row.wrote_message_id {
                assert!(
                    reported.contains(&json!({"name": name, "value": value})),
                    "case {case:?}: the {of} event must report the Message-ID field the \
                     message wrote, {name}: {value}, among its headers, got {event}"
                );
            }
        }
    }
}
