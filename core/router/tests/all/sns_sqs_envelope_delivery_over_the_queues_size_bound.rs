// SPDX-License-Identifier: CC0-1.0

//! What a subscribed Amazon SQS queue RECEIVES when an Amazon SNS topic fans a publish out to
//! it, asserted over the production service set (`aws_emulator_router::build_router` on one
//! engine, exactly as the binary assembles it) and read where a consumer reads it: on the
//! queue.
//!
//! Two rules are pinned here, and one suite pins them together because each is observed on the
//! same delivered document:
//!
//! * `MaximumMessageSize` is "the limit of how many bytes a message can contain before Amazon
//!   SQS rejects it" (`SetQueueAttributes$Attributes`) — a bound on what THAT service accepts
//!   from its own caller. A notification another service hands the queue on behalf of one of
//!   its own resources is not such a request, so the attribute bounds a client's `SendMessage`
//!   and bounds nothing about the fan-out: the whole notification is enqueued however small the
//!   attribute stands and whenever it was written. The notification is the published body
//!   WRAPPED in the envelope, so what the attribute would have bounded is the envelope rather
//!   than the body — a body inside the queue's limit whose envelope is past it is delivered too.
//! * Every notification delivered in the JSON envelope carries the members that seal it —
//!   `SignatureVersion` (the version the topic signs under, "1" unless the topic asks for
//!   another), `Signature` and `SigningCertURL`, which are what "lets you verify the
//!   authenticity of the message" (the Amazon SNS Developer Guide, "Verifying the signatures of
//!   Amazon SNS messages") — beside `Type`, `MessageId`, `TopicArn`, `Subject` when the publish
//!   carried one, `Message`, `Timestamp`, `UnsubscribeURL` and the published `MessageAttributes`.
//!
//! The rows differ along what the rules generalise over: the queue's configured
//! `MaximumMessageSize` (the documented floor, a value between the published bounds, the
//! documented ceiling), whether that attribute was written when the queue was made or lowered
//! afterwards, how the published body compares to it (well past it, past it only once wrapped
//! in the envelope, inside it), whether the body's bytes are all ASCII, and whether the publish
//! carried a subject and typed message attributes.
//!
//! The last test pins the same two rules over the notification a client never publishes itself:
//! the endpoint-lifecycle event of a mobile push platform application, whose rows differ along
//! which lifecycle the call performs and which `Event<lifecycle>` attribute names its topic.
//!
//! What this suite adds to each service's own cover is the CROSSING on the assembled emulator:
//! the delivery is made by the SNS mounted beside the SQS the production router mounts, and the
//! same payload is then offered to that queue's own API as a client, so one run shows both the
//! bound in force and the exemption from it.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_sqs_amazonsqs::scaffolded::{
    legacy_query_code_of_shape, INVALID_PARAMETER_VALUE,
};
use service_com_amazonaws_sqs_amazonsqs::state::{integer_bounds, MAXIMUM_MESSAGE_SIZE};

use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The service principal Amazon SNS delivers to a subscribed queue as, which is the identity a
/// queue policy has to grant for a notification to reach the queue at all.
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";

/// The `Type` every notification delivered in the JSON envelope declares.
const NOTIFICATION_TYPE: &str = "Notification";

/// The members the envelope carries for every notification, whatever the publish said: the
/// metadata and the three a subscriber verifies it with.
const ALWAYS_CARRIED: &[&str] = &[
    "Type",
    "MessageId",
    "TopicArn",
    "Message",
    "Timestamp",
    "SignatureVersion",
    "Signature",
    "SigningCertURL",
    "UnsubscribeURL",
];

/// The `SignatureVersion` a topic that asks for no other signs under: "By default
/// SignatureVersion is set to 1" (`SetTopicAttributesInput$AttributeName`).
const DEFAULT_SIGNATURE_VERSION: &str = "1";

/// The push notification service the platform applications below target, spelled as a client
/// spells it.
const PLATFORM: &str = "BAIDU";

/// The documented spelling of [`PLATFORM`], which is what the ARNs this emulator mints carry.
const CANONICAL_PLATFORM: &str = "Baidu";

/// The whole emulator over one engine, as the binary assembles it, so which service claims
/// each call — SNS's own in-process call into Amazon SQS included — is the production mount's
/// decision rather than a hand-picked set.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature itself is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// One Amazon SNS awsQuery call (API version 2010-03-31) that must answer 200, as its XML body.
async fn sns(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version=2010-03-31");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sns.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("sns"))
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SNS {action}, got {status} with {text}"
    );
    text
}

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`), with whatever status it
/// answered: the call every SQS request of this suite goes through, and the one a REFUSED call
/// is read from.
async fn sqs_call(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header("authorization", authorization("sqs"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Amazon SQS call that must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = sqs_call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// The error code a refused SQS call carries: the shape id the body names, mapped to the
/// service's own legacy query code through the serving crate's own mapping of the two.
fn error_code(body: &Value) -> String {
    let shape = body["__type"].as_str().unwrap_or_default();
    if let Some(code) = legacy_query_code_of_shape(shape) {
        return code.to_string();
    }
    shape
        .rsplit(['#', '/'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The sentence Amazon SQS refuses a message larger than the addressed queue will carry with,
/// written out here rather than taken from the serving crate's own composer, so a change of
/// WORDING is a failure of this suite and not something it follows. Its one variable part is
/// that queue's own limit, which is the figure the request is judged against
/// (`SetQueueAttributes$Attributes`).
fn too_long_sentence(maximum: i64) -> String {
    format!(
        "One or more parameters are invalid. Reason: Message must be shorter than {maximum} bytes."
    )
}

/// The queue policy AWS documents for a queue subscribed to an SNS topic: the service
/// principal, allowed `sqs:SendMessage` on that queue.
fn grant_to_sns(queue_arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowSNSDelivery",
            "Effect": "Allow",
            "Principal": {"Service": SNS_PRINCIPAL},
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
        }],
    })
    .to_string()
}

/// A queue called `name` an SNS topic may deliver to, created with `attributes` beside the
/// policy that grants the delivery: its url and the ARN a subscription names it by.
async fn given_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    attributes: &Value,
) -> (String, String) {
    let created = sqs(
        router,
        case,
        "CreateQueue",
        &json!({ "QueueName": name, "Attributes": attributes }),
    )
    .await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl, got {created}"))
        .to_string();
    let reported = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = reported["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueArn, got {reported}"))
        .to_string();
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": grant_to_sns(&arn) } }),
    )
    .await;
    (url, arn)
}

/// Write `maximum` as the `MaximumMessageSize` of the queue at `url`, which is how a client
/// lowers the bound after the queue was made.
async fn lower_the_bound(router: &MultiServiceRouter, case: &str, url: &str, maximum: i64) {
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({
            "QueueUrl": url,
            "Attributes": { MAXIMUM_MESSAGE_SIZE: maximum.to_string() },
        }),
    )
    .await;
}

/// A topic called `name` with the queue `queue_arn` subscribed to it in the DEFAULT delivery
/// form, so what the queue receives is the notification envelope: its ARN.
async fn given_topic_subscribed(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    queue_arn: &str,
) -> String {
    let topic_arn = element(&sns(router, case, "CreateTopic", &[("Name", name)]).await, "TopicArn");
    sns(
        router,
        case,
        "Subscribe",
        &[
            ("TopicArn", &topic_arn),
            ("Protocol", "sqs"),
            ("Endpoint", queue_arn),
            ("ReturnSubscriptionArn", "true"),
        ],
    )
    .await;
    topic_arn
}

/// How long a wait for a delivery that MUST arrive may take, and how often it looks again.
/// Generous: nothing here measures how fast the host runs, and a wait fails only by the delivery
/// never arriving.
const DELIVERY_BUDGET: Duration = Duration::from_secs(60);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// Every message body waiting on the queue at `url`, DELETED as it is read, so what a later
/// stage reads is what that stage's own call left.
async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut bodies = Vec::with_capacity(messages.len());
    for message in &messages {
        let handle = message["ReceiptHandle"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a ReceiptHandle, got {message}"));
        sqs(
            router,
            case,
            "DeleteMessage",
            &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
        )
        .await;
        bodies.push(message["Body"].as_str().unwrap_or_default().to_string());
    }
    bodies
}

/// The bodies on the queue at `url` once it holds at least one, looking again until it does or
/// [`DELIVERY_BUDGET`] has passed with it holding none.
///
/// A fan-out this emulator performs inline is on the queue by the time the publish answered, so
/// this normally returns on its first look; it waits for the CONDITION rather than assuming that,
/// so a delivery path that ever hands over off the request's own task makes a row slower rather
/// than flaky.
async fn delivered_bodies(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let deadline = Instant::now() + DELIVERY_BUDGET;
    loop {
        let held = drain(router, case, url).await;
        if !held.is_empty() {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a delivery on the subscribed queue, got none"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// A body of exactly `bytes` bytes carrying `marker`, padded with `filler` — one character,
/// which a row may give as a multi-byte one so the published body's bytes are not all ASCII.
fn body_of(bytes: usize, marker: &str, filler: char) -> String {
    let mut body = String::with_capacity(bytes);
    body.push_str(marker);
    while body.len() + filler.len_utf8() <= bytes {
        body.push(filler);
    }
    while body.len() < bytes {
        body.push('.');
    }
    body
}

/// The envelope `body` is, as a JSON object; panics quoting the body when it is not one.
fn envelope_of(case: &str, body: &str) -> serde_json::Map<String, Value> {
    let parsed: Value = serde_json::from_str(body)
        .unwrap_or_else(|error| panic!("case {case:?}: the body is not the envelope ({error}): {body}"));
    parsed
        .as_object()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the envelope must be a JSON object, got {body}"))
}

/// Assert that `envelope` carries every member a notification always carries, with the
/// signature version `version`, and that each is a non-empty string.
fn assert_sealed(case: &str, envelope: &serde_json::Map<String, Value>, version: &str) {
    for member in ALWAYS_CARRIED {
        let carried = envelope.get(*member).and_then(Value::as_str);
        assert!(
            carried.is_some_and(|value| !value.is_empty()),
            "case {case:?}: every notification carries a non-empty {member}, got \
             {carried:?} in {envelope:?}"
        );
    }
    assert_eq!(
        envelope.get("Type").and_then(Value::as_str),
        Some(NOTIFICATION_TYPE),
        "case {case:?}: expected Type {NOTIFICATION_TYPE}, got {envelope:?}"
    );
    assert_eq!(
        envelope.get("SignatureVersion").and_then(Value::as_str),
        Some(version),
        "case {case:?}: expected SignatureVersion {version:?}, got {envelope:?}"
    );
    // The two members the version is read beside: the signature a subscriber decodes, and the
    // address of the certificate it verifies that signature against. A notification whose
    // SignatureVersion says "1" but whose Signature is not a base64 document, or whose
    // SigningCertURL does not lead to a certificate, is not a verifiable notification.
    let signature = envelope
        .get("Signature")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        signature.len().is_multiple_of(BASE64_GROUP)
            && signature
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='),
        "case {case:?}: the Signature must be a base64 document, got {signature:?}"
    );
    let certificate_url = envelope
        .get("SigningCertURL")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let path = certificate_url
        .strip_prefix(HTTPS_SCHEME)
        .map(|rest| rest.split('?').next().unwrap_or(rest))
        .map(|rest| rest.split_once('/').map_or("", |(_, path)| path))
        .unwrap_or_default();
    assert!(
        path.starts_with(CERTIFICATE_FILE_PREFIX) && path.ends_with(CERTIFICATE_FILE_SUFFIX),
        "case {case:?}: the SigningCertURL must address the published certificate \
         ({HTTPS_SCHEME}<host>/{CERTIFICATE_FILE_PREFIX}<id>{CERTIFICATE_FILE_SUFFIX}), got \
         {certificate_url:?}"
    );
}

/// How many characters one base64 group is written in, which is what the length of a base64
/// document is a whole number of.
const BASE64_GROUP: usize = 4;

/// What a URL a subscriber is handed to fetch the signing certificate begins with.
const HTTPS_SCHEME: &str = "https://";

/// The name of the document a notification's `SigningCertURL` addresses, as every published
/// sample of the member spells it: `SimpleNotificationService-<id>.pem`.
const CERTIFICATE_FILE_PREFIX: &str = "SimpleNotificationService-";
const CERTIFICATE_FILE_SUFFIX: &str = ".pem";

/// One row of the delivery table.
struct Row {
    case: &'static str,
    /// The `MaximumMessageSize` the subscribed queue carries while the publish is delivered.
    maximum: i64,
    /// Whether that bound was written AFTER the queue was made (the queue is then created with
    /// the documented ceiling, which is what a queue nobody configured carries).
    lowered_afterwards: bool,
    /// How many bytes the published body weighs.
    body_bytes: usize,
    /// What the body is padded with, so a row can publish bytes that are not all ASCII.
    filler: char,
    /// The subject the publish carries, if any.
    subject: Option<&'static str>,
    /// The message attributes it carries, as `(name, data type, value)`.
    attributes: &'static [(&'static str, &'static str, &'static str)],
    /// Whether the delivered envelope is LARGER than the queue's bound, which is the relation
    /// that makes the row exercise the exemption rather than pass under the bound.
    envelope_past_the_bound: bool,
}

/// A notification is delivered whole into a subscribed queue whatever that queue's
/// `MaximumMessageSize` says, while the same payload offered to the queue's own API as a client
/// is bounded by it — and every delivered envelope carries the members that seal it.
#[tokio::test]
async fn a_subscribed_queue_takes_the_whole_envelope_and_a_client_send_stays_bounded() {
    // The bounds the attribute publishes, read from the one place the serving crate keeps them
    // rather than written out again here.
    let (floor, ceiling) = integer_bounds(MAXIMUM_MESSAGE_SIZE);
    let between = 262_144_i64;
    assert!(
        floor < between && between < ceiling,
        "case setup: the middling rows must sit inside the published bounds ({floor}..={ceiling})"
    );

    let rows: &[Row] = &[
        Row {
            case: "a body four times the queue's limit, which is the documented floor",
            maximum: floor,
            lowered_afterwards: false,
            body_bytes: 4096,
            filler: 'x',
            subject: None,
            attributes: &[],
            envelope_past_the_bound: true,
        },
        Row {
            case: "a non-ASCII body with a subject and typed attributes, on a limit lowered \
                   after the queue was made",
            maximum: floor,
            lowered_afterwards: true,
            body_bytes: 4096,
            filler: 'ü',
            subject: Some("Versandt: Grüße 中文 📦"),
            attributes: &[("tier", "String", "gold"), ("count", "Number", "7")],
            envelope_past_the_bound: true,
        },
        Row {
            case: "a body INSIDE the queue's limit whose envelope is past it",
            maximum: floor,
            lowered_afterwards: false,
            body_bytes: 1000,
            filler: 'x',
            subject: Some("wrapped"),
            attributes: &[],
            envelope_past_the_bound: true,
        },
        Row {
            case: "a body inside a limit between the published bounds",
            maximum: between,
            lowered_afterwards: false,
            body_bytes: 4096,
            filler: 'x',
            subject: None,
            attributes: &[("count", "Number", "11")],
            envelope_past_the_bound: false,
        },
        Row {
            case: "a body inside the documented ceiling",
            maximum: ceiling,
            lowered_afterwards: false,
            body_bytes: 4096,
            filler: 'x',
            subject: Some("at the ceiling"),
            attributes: &[],
            envelope_past_the_bound: false,
        },
    ];

    let router = router();
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let created_with = if row.lowered_afterwards {
            json!({ MAXIMUM_MESSAGE_SIZE: ceiling.to_string() })
        } else {
            json!({ MAXIMUM_MESSAGE_SIZE: row.maximum.to_string() })
        };
        let (url, queue_arn) =
            given_queue(&router, case, &format!("bounded-{index}"), &created_with).await;
        let topic_arn =
            given_topic_subscribed(&router, case, &format!("bounded-topic-{index}"), &queue_arn)
                .await;
        if row.lowered_afterwards {
            lower_the_bound(&router, case, &url, row.maximum).await;
        }

        let body = body_of(row.body_bytes, &format!("row-{index}-"), row.filler);
        assert_eq!(
            body.len(),
            row.body_bytes,
            "case {case:?}: the row's body must weigh {} bytes",
            row.body_bytes
        );
        let mut members: Vec<(String, String)> = vec![
            ("TopicArn".to_string(), topic_arn.clone()),
            ("Message".to_string(), body.clone()),
        ];
        if let Some(subject) = row.subject {
            members.push(("Subject".to_string(), subject.to_string()));
        }
        for (entry, (name, data_type, value)) in row.attributes.iter().enumerate() {
            let at = entry + 1;
            members.push((
                format!("MessageAttributes.entry.{at}.Name"),
                (*name).to_string(),
            ));
            members.push((
                format!("MessageAttributes.entry.{at}.Value.DataType"),
                (*data_type).to_string(),
            ));
            members.push((
                format!("MessageAttributes.entry.{at}.Value.StringValue"),
                (*value).to_string(),
            ));
        }
        let borrowed: Vec<(&str, &str)> = members
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let published = sns(&router, case, "Publish", &borrowed).await;
        let message_id = element(&published, "MessageId");

        let delivered = delivered_bodies(&router, case, &url).await;
        assert_eq!(
            delivered.len(),
            1,
            "case {case:?}: a notification of a {}-byte body must reach the queue whose \
             {MAXIMUM_MESSAGE_SIZE} is {}, got {delivered:?}",
            body.len(),
            row.maximum
        );
        let text = delivered[0].clone();
        let envelope = envelope_of(case, &text);

        // The relation the row is about: the envelope the queue took is past the bound its own
        // API would have applied, or under it for the control rows.
        let envelope_bytes = i64::try_from(text.len()).unwrap_or(i64::MAX);
        assert_eq!(
            envelope_bytes > row.maximum,
            row.envelope_past_the_bound,
            "case {case:?}: the {envelope_bytes}-byte envelope must {} the queue's \
             {}-byte bound for this row to assert what it says",
            if row.envelope_past_the_bound { "exceed" } else { "fit inside" },
            row.maximum
        );

        assert_sealed(case, &envelope, DEFAULT_SIGNATURE_VERSION);
        assert_eq!(
            envelope.get("TopicArn").and_then(Value::as_str),
            Some(topic_arn.as_str()),
            "case {case:?}: the envelope must name the topic published to, got {text}"
        );
        assert_eq!(
            envelope.get("MessageId").and_then(Value::as_str),
            Some(message_id.as_str()),
            "case {case:?}: the envelope must carry the MessageId the publish answered, got {text}"
        );
        assert_eq!(
            envelope.get("Message").and_then(Value::as_str),
            Some(body.as_str()),
            "case {case:?}: the envelope must carry the published body whole"
        );
        assert_eq!(
            envelope.get("Subject").and_then(Value::as_str),
            row.subject,
            "case {case:?}: Subject is carried exactly as published, got {text}"
        );
        for (name, data_type, value) in row.attributes {
            let carried = &envelope["MessageAttributes"][*name];
            assert_eq!(
                (
                    carried["Type"].as_str().unwrap_or_default(),
                    carried["Value"].as_str().unwrap_or_default(),
                ),
                (*data_type, *value),
                "case {case:?}: the attribute {name:?} must arrive typed as published, got {text}"
            );
        }
        assert_eq!(
            envelope.contains_key("MessageAttributes"),
            !row.attributes.is_empty(),
            "case {case:?}: MessageAttributes is carried exactly when the publish had any, \
             got {text}"
        );

        // The SAME bytes, offered to the queue's own API by a client: bounded by the attribute
        // the delivery was exempt from, in the sentence naming that queue's limit.
        let (status, answer) = sqs_call(
            &router,
            "SendMessage",
            &json!({ "QueueUrl": url, "MessageBody": text }),
        )
        .await;
        if row.envelope_past_the_bound {
            assert_eq!(
                (status, error_code(&answer).as_str()),
                (400, INVALID_PARAMETER_VALUE),
                "case {case:?}: a client's send of the same {envelope_bytes} bytes past the \
                 queue's {}-byte bound must be refused, got {status} with {answer}",
                row.maximum
            );
            assert_eq!(
                answer["message"].as_str(),
                Some(too_long_sentence(row.maximum).as_str()),
                "case {case:?}: expected the sentence naming the queue's own limit, got {answer}"
            );
        } else {
            assert_eq!(
                status, 200,
                "case {case:?}: a client's send inside the queue's {}-byte bound must be \
                 accepted, got {status} with {answer}",
                row.maximum
            );
        }
        let left = drain(&router, case, &url).await;
        assert_eq!(
            left.len(),
            usize::from(!row.envelope_past_the_bound),
            "case {case:?}: the queue must hold exactly what the client's send left, got \
             {} message(s)",
            left.len()
        );
    }
}

/// Which lifecycle change of a device endpoint a row performs.
#[derive(Debug, Clone, Copy)]
enum Lifecycle {
    /// The registration that mints the endpoint.
    Register,
    /// A write of the endpoint's attributes.
    Write,
    /// The delete that removes it.
    Remove,
}

/// A platform application carrying the credential pair its platform documents and ONE event
/// attribute naming `topic_arn`: its ARN.
async fn given_application(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    attribute: &str,
    topic_arn: &str,
) -> String {
    let answered = sns(
        router,
        case,
        "CreatePlatformApplication",
        &[
            ("Name", name),
            ("Platform", PLATFORM),
            ("Attributes.entry.1.key", "PlatformCredential"),
            ("Attributes.entry.1.value", "client-secret-0123456789"),
            ("Attributes.entry.2.key", "PlatformPrincipal"),
            ("Attributes.entry.2.value", "client-id-0123456789"),
            ("Attributes.entry.3.key", attribute),
            ("Attributes.entry.3.value", topic_arn),
        ],
    )
    .await;
    element(&answered, "PlatformApplicationArn")
}

/// An endpoint-lifecycle event reaches the topic the application's matching attribute names,
/// delivered in the envelope into a queue whose own bound is the documented floor.
#[tokio::test]
async fn an_endpoint_lifecycle_event_reaches_the_topic_its_own_attribute_names() {
    let (floor, _) = integer_bounds(MAXIMUM_MESSAGE_SIZE);
    let account = engine::emulator_account_id();

    // (case, the attribute that configures the topic, the EventType its document carries, the
    // lifecycle the row performs). The event names are the ones the attribute's own
    // documentation gives them (`SetPlatformApplicationAttributesInput$Attributes`).
    let rows: &[(&str, &str, &str, Lifecycle)] = &[
        (
            "a registration",
            "EventEndpointCreated",
            "EndpointCreated",
            Lifecycle::Register,
        ),
        (
            "an attribute write",
            "EventEndpointUpdated",
            "EndpointUpdate",
            Lifecycle::Write,
        ),
        (
            "a removal",
            "EventEndpointDeleted",
            "EndpointDeleted",
            Lifecycle::Remove,
        ),
    ];

    let router = router();
    for (index, (case, attribute, event_type, lifecycle)) in rows.iter().enumerate() {
        let name = format!("appevt-{index}");
        // ONLY this row's lifecycle has a topic configured, so a notification on the queue is
        // evidence that the lifecycle the row performed read ITS OWN attribute.
        let (url, queue_arn) = given_queue(
            &router,
            case,
            &format!("{name}-queue"),
            &json!({ MAXIMUM_MESSAGE_SIZE: floor.to_string() }),
        )
        .await;
        let topic_arn = given_topic_subscribed(&router, case, &name, &queue_arn).await;
        let application = given_application(&router, case, &name, attribute, &topic_arn).await;

        let endpoint = element(
            &sns(
                &router,
                case,
                "CreatePlatformEndpoint",
                &[
                    ("PlatformApplicationArn", &application),
                    ("Token", &format!("device-{index}")),
                ],
            )
            .await,
            "EndpointArn",
        );
        let expected_prefix =
            format!("arn:aws:sns:{REGION}:{account}:endpoint/{CANONICAL_PLATFORM}/{name}/");
        assert!(
            endpoint.starts_with(&expected_prefix) && endpoint.len() > expected_prefix.len(),
            "case {case:?}: the endpoint ARN must be {expected_prefix}<endpoint id>, got \
             {endpoint:?}"
        );

        match lifecycle {
            Lifecycle::Register => {}
            Lifecycle::Write => {
                sns(
                    &router,
                    case,
                    "SetEndpointAttributes",
                    &[
                        ("EndpointArn", &endpoint),
                        ("Attributes.entry.1.key", "CustomUserData"),
                        ("Attributes.entry.1.value", "re-registered"),
                    ],
                )
                .await;
            }
            Lifecycle::Remove => {
                sns(
                    &router,
                    case,
                    "DeleteEndpoint",
                    &[("EndpointArn", &endpoint)],
                )
                .await;
            }
        }

        let delivered = delivered_bodies(&router, case, &url).await;
        assert_eq!(
            delivered.len(),
            1,
            "case {case:?}: expected exactly one event notification on the topic {attribute} \
             names, got {delivered:?}"
        );
        let text = delivered[0].clone();
        let envelope = envelope_of(case, &text);
        assert_sealed(case, &envelope, DEFAULT_SIGNATURE_VERSION);

        let document: Value = serde_json::from_str(
            envelope.get("Message").and_then(Value::as_str).unwrap_or_default(),
        )
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the event document must be JSON ({error}): {text}")
        });
        assert_eq!(
            document["EventType"].as_str(),
            Some(*event_type),
            "case {case:?}: expected EventType {event_type:?}, got {document}"
        );
        assert_eq!(
            document["EndpointArn"].as_str(),
            Some(endpoint.as_str()),
            "case {case:?}: expected EndpointArn {endpoint:?}, got {document}"
        );
        assert_eq!(
            document["Resource"].as_str(),
            Some(application.as_str()),
            "case {case:?}: expected the application {application:?} as the resource, got \
             {document}"
        );
    }
}
