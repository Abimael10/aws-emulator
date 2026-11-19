// SPDX-License-Identifier: CC0-1.0

//! Several sends made through one configuration set inside its Firehose event destination's
//! buffering interval ALL reach the delivery stream's Amazon S3 destination.
//!
//! The rule under test: every event an accepted message produces is put on the delivery
//! stream a matching, enabled `KinesisFirehoseDestination` names as one record, and the
//! stream delivers each buffer it flushes to its own destination with every record that
//! buffer held on a line of its own — so a consumer reads exactly one event document per
//! event, whatever buffering interval the stream was created with and however many records
//! one flush carried.
//!
//! `KinesisFirehoseDestination` names "the Amazon Kinesis Data Firehose stream that the
//! Amazon SES API v2 sends email events to" and the role it sends them under, and
//! `BufferingHints$IntervalInSeconds` is "buffer incoming data for the specified period of
//! time, in seconds, before delivering it to the destination" (0..900), beside
//! `BufferingHints$SizeInMBs` (1..128) —
//! `smithy_query(query="shape",shape="BufferingHints",model_path="services/service-com-amazonaws-firehose-firehose-20150804/smithy-model/com-amazonaws-firehose-firehose-20150804.json")`.
//! So the hints decide WHEN a buffer is flushed, and therefore how many records one
//! delivered object carries — never WHICH events are published, nor how many of them.
//!
//! The rows differ along what the rule generalises over: the buffering interval the stream
//! was created with (the published minimum of the range, and larger intervals that hold
//! every one of a row's sends in a single buffer), the buffer size stated with it (the
//! published minimum and the published maximum), and how many messages the set carries
//! inside that interval. Each row asserts one record per send, each about a message that
//! row sent, and no record of any other event type — so no row can pass by delivering
//! something else, and none with a record lost or delivered twice.
//!
//! What a row does NOT assert is HOW the stream grouped those records into objects: which
//! buffer a record rode is the stream's own choice, and the hints are "treated as hints, and
//! therefore Firehose might choose to use different values when it is optimal"
//! (`BufferingHints`). The rows pin what does not depend on that choice: the same events
//! reach the destination, each as one document on a line of its own, whatever interval the
//! stream buffers for.
//!
//! `ses_event_publication_reaches_firehose_s3.rs` pins the other half of the same rule —
//! WHICH destinations of a set an event reaches, by matching event type and by `Enabled` —
//! over streams buffering for the shortest interval only.
//!
//! Only a cross-service suite can ask this: the flow spans SESv2 (the identity, the set,
//! the destination and the sends), IAM (the roles the publish and the delivery are made
//! under), Firehose (the stream and its buffering) and Amazon S3 (the delivered objects),
//! over ONE engine exactly as `core/router/src/lib.rs` mounts them. A crate-local test only
//! ever sees one side of each call.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{element, elements, form_encode};

/// The Region every call below is signed for, and the one every ARN here names.
const REGION: &str = "us-east-1";

/// IAM's API version, which an `awsQuery` body carries beside its `Action`.
const IAM_API_VERSION: &str = "2010-05-08";

/// The bucket every stream of this suite delivers into.
const BUCKET: &str = "ses-buffered-event-publication";

/// The role Firehose assumes to write a delivered object into that bucket.
const DELIVERY_ROLE: &str = "ses-buffered-firehose-delivery";

/// The role SESv2 sends a configuration set's events under
/// (`KinesisFirehoseDestination$IamRoleArn`).
const PUBLISHING_ROLE: &str = "ses-buffered-event-publishing";

/// The verified sending identity every message below is sent from, and an address inside it.
const IDENTITY: &str = "buffered.example.com";
const SENDER: &str = "events@buffered.example.com";

/// The event type every destination of this suite matches, and the spelling the published
/// document carries for it: `EventType`'s `SEND` is published as `Send`.
const MATCHED_EVENT_TYPE: &str = "SEND";
const PUBLISHED_EVENT_TYPE: &str = "Send";

/// The trust policy the delivery role carries: Firehose assumes it to deliver.
const DELIVERY_TRUST: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"#,
    r#""firehose.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// The trust policy the publishing role carries: SES sends the set's events under it.
const PUBLISHING_TRUST: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"#,
    r#""ses.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// The permissions each role needs: the record write into the stream and the object write
/// the delivery itself makes.
const ROLE_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"#,
    r#"["firehose:PutRecord","firehose:PutRecordBatch","s3:PutObject"],"Resource":"*"}]}"#
);

/// How long a condition the clock owes is waited for before a case fails: far longer than
/// the longest interval any row below buffers for, the span a stream takes to become
/// `ACTIVE` and the span an identity takes to verify together, so a loaded machine does not
/// fail a case the clock would have served.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);

/// How often a condition is re-read while waiting for it.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// One row: the buffering the stream was created with, and how many messages the set
/// carries inside that buffering interval.
struct Row {
    case: &'static str,
    /// Names this row's stream, prefix, configuration set and destination, so no row reads
    /// another's records.
    slug: &'static str,
    /// `BufferingHints$IntervalInSeconds`, whose published range is 0..900.
    interval_in_seconds: u32,
    /// `BufferingHints$SizeInMBs`, whose published range is 1..128. No row's records come
    /// anywhere near it, so the interval is what flushes every buffer here.
    size_in_mibs: u32,
    /// One message per recipient, all sent through this row's set back to back — so they
    /// fall inside one buffering interval of it.
    recipients: &'static [&'static str],
}

const ROWS: &[Row] = &[
    Row {
        case: "two sends inside the interval of a one-second buffer",
        slug: "fhb-one-second",
        interval_in_seconds: 1,
        size_in_mibs: 1,
        recipients: &["first@example.net", "second@example.net"],
    },
    Row {
        case: "two sends into a stream buffering for the shortest interval the hints admit",
        slug: "fhb-no-interval",
        interval_in_seconds: 0,
        size_in_mibs: 1,
        recipients: &["third@example.net", "fourth@example.net"],
    },
    Row {
        case: "three sends inside one longer interval, under the largest buffer size",
        slug: "fhb-three-in-one",
        interval_in_seconds: 5,
        size_in_mibs: 128,
        recipients: &[
            "fifth@example.net",
            "sixth@example.net",
            "seventh@example.net",
        ],
    },
    Row {
        case: "a single send inside a multi-second interval",
        slug: "fhb-single",
        interval_in_seconds: 3,
        size_in_mibs: 64,
        recipients: &["eighth@example.net"],
    },
];

/// The subject of message `index` of the row `slug`: distinct per row and per message, so an
/// assertion names exactly which message's event it did or did not find.
fn subject(slug: &str, index: usize) -> String {
    format!("{slug} message {index}")
}

/// The emulator the suite drives: the four services the flow spans, on ONE engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signature verification is the emulator's concern, not any of these services'.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_sesv2_simpleemailservice_v2::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_firehose_firehose_20150804::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`. The signature
/// is never checked: this router runs with authentication off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One request to the service `signing_name` serves, at the endpoint host it answers on.
async fn send(
    router: &MultiServiceRouter,
    signing_name: &str,
    host: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> (u16, Vec<u8>) {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization(signing_name));
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (response.status().as_u16(), response.body().to_vec())
}

/// One SESv2 call that must be answered 200: `restJson1` under the `ses` credential scope,
/// at the `email` endpoint.
async fn sesv2_ok(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Value {
    let payload = match &body {
        Some(value) => value.to_string().into_bytes(),
        None => Vec::new(),
    };
    let (status, answered) = send(
        router,
        "ses",
        &format!("email.{REGION}.amazonaws.com"),
        method,
        path,
        &[("content-type", "application/json")],
        payload,
    )
    .await;
    let text = String::from_utf8_lossy(&answered).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: SESv2 {method} {path} must succeed, got {status} {text}"
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// One Firehose call that must succeed: `awsJson1_1`, `POST /` with the operation in
/// `X-Amz-Target`.
async fn firehose_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: Value,
) -> Value {
    let (status, answered) = send(
        router,
        "firehose",
        &format!("firehose.{REGION}.amazonaws.com"),
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &format!("Firehose_20150804.{operation}")),
        ],
        body.to_string().into_bytes(),
    )
    .await;
    let text = String::from_utf8_lossy(&answered).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: Firehose {operation} must succeed, got {status} {text}"
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// One `awsQuery` IAM action that must succeed, answering the XML document a query client
/// reads.
async fn iam_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={IAM_API_VERSION}");
    for (name, value) in parameters {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let (status, answered) = send(
        router,
        "iam",
        &format!("iam.{REGION}.amazonaws.com"),
        "POST",
        "/",
        &[("content-type", "application/x-www-form-urlencoded")],
        body.into_bytes(),
    )
    .await;
    let text = String::from_utf8_lossy(&answered).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: IAM {action} must succeed, got {status} {text}"
    );
    text
}

/// Create `role_name` with `trust` as its trust policy and the permissions both writes need,
/// answering the role ARN IAM minted for it.
async fn given_role(router: &MultiServiceRouter, role_name: &str, trust: &str) -> String {
    let created = iam_ok(
        router,
        "setup",
        "CreateRole",
        &[
            ("RoleName", role_name),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    iam_ok(
        router,
        "setup",
        "PutRolePolicy",
        &[
            ("RoleName", role_name),
            ("PolicyName", "buffered-publication"),
            ("PolicyDocument", ROLE_POLICY),
        ],
    )
    .await;
    let arn = element(&created, "Arn");
    assert!(
        arn.ends_with(&format!(":role/{role_name}")),
        "setup: CreateRole({role_name}) must answer the ARN it minted, got {arn}"
    );
    arn
}

/// Every key the bucket holds under `prefix`, read through `ListObjectsV2`.
async fn keys_under(router: &MultiServiceRouter, case: &str, prefix: &str) -> Vec<String> {
    let (status, body) = send(
        router,
        "s3",
        &format!("s3.{REGION}.amazonaws.com"),
        "GET",
        &format!("/{BUCKET}?list-type=2&prefix={prefix}"),
        &[],
        Vec::new(),
    )
    .await;
    let text = String::from_utf8_lossy(&body).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: listing {prefix:?} must succeed, got {status} {text}"
    );
    let mut keys = elements(&text, "Key");
    keys.sort();
    keys
}

/// Every delivered object under `prefix`, in key order, as the JSON documents its own lines
/// carry: a delivered object is the records of one flushed buffer concatenated, and each
/// record is one event document on a line of its own, however many records that buffer held.
async fn delivered_objects(
    router: &MultiServiceRouter,
    case: &str,
    prefix: &str,
) -> Vec<(String, Vec<Value>)> {
    let mut objects = Vec::new();
    for key in keys_under(router, case, prefix).await {
        let (status, body) = send(
            router,
            "s3",
            &format!("s3.{REGION}.amazonaws.com"),
            "GET",
            &format!("/{BUCKET}/{key}"),
            &[],
            Vec::new(),
        )
        .await;
        let text = String::from_utf8_lossy(&body).to_string();
        assert_eq!(
            status, 200,
            "case {case:?}: reading {key:?} must succeed, got {status} {text}"
        );
        let documents = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str::<Value>(line).unwrap_or_else(|error| {
                    panic!(
                        "case {case:?}: every line of {key:?} must be one published event \
                         document, got {line:?}: {error}"
                    )
                })
            })
            .collect();
        objects.push((key, documents));
    }
    objects
}

/// The delivered objects under `prefix` once they carry at least `want` event documents in
/// all — or a failure naming the case once [`SETTLE_DEADLINE`] has passed.
async fn delivered_events(
    router: &MultiServiceRouter,
    case: &str,
    prefix: &str,
    want: usize,
) -> Vec<(String, Vec<Value>)> {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let objects = delivered_objects(router, case, prefix).await;
        let delivered: usize = objects.iter().map(|(_, documents)| documents.len()).sum();
        if delivered >= want {
            return objects;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected at least {want} published event document(s) under \
             {prefix:?}, got {objects:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Wait until the stream `name` reports `ACTIVE`, the state a stream takes records in.
async fn given_active_stream(router: &MultiServiceRouter, case: &str, name: &str) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let described = firehose_ok(
            router,
            case,
            "DescribeDeliveryStream",
            json!({"DeliveryStreamName": name}),
        )
        .await;
        let status = described["DeliveryStreamDescription"]["DeliveryStreamStatus"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if status == "ACTIVE" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the stream {name:?} must become ACTIVE, got {status:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Create the sending identity and wait until it reports that it is verified for sending,
/// which is what a send from an address inside it needs.
async fn given_verified_identity(router: &MultiServiceRouter) {
    sesv2_ok(
        router,
        "setup",
        "POST",
        "/v2/email/identities",
        Some(json!({"EmailIdentity": IDENTITY})),
    )
    .await;
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = sesv2_ok(
            router,
            "setup",
            "GET",
            &format!("/v2/email/identities/{IDENTITY}"),
            None,
        )
        .await;
        if read["VerifiedForSendingStatus"].as_bool() == Some(true) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the identity {IDENTITY:?} must become verified for sending, got {read}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn a_firehose_event_destination_delivers_every_send_of_a_buffering_interval() {
    let router = emulator();
    let delivery_role = given_role(&router, DELIVERY_ROLE, DELIVERY_TRUST).await;
    let publishing_role = given_role(&router, PUBLISHING_ROLE, PUBLISHING_TRUST).await;
    let (status, created_bucket) = send(
        &router,
        "s3",
        &format!("s3.{REGION}.amazonaws.com"),
        "PUT",
        &format!("/{BUCKET}"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(
        status,
        200,
        "setup: the destination bucket must be created, got {status} {}",
        String::from_utf8_lossy(&created_bucket)
    );
    given_verified_identity(&router).await;

    // Every row's stream, set and destination, created together so one wait covers the span
    // the creates armed.
    for row in ROWS {
        let case = row.case;
        let stream = format!("{}-stream", row.slug);
        let created = firehose_ok(
            &router,
            case,
            "CreateDeliveryStream",
            json!({
                "DeliveryStreamName": stream,
                "DeliveryStreamType": "DirectPut",
                "ExtendedS3DestinationConfiguration": {
                    "RoleARN": delivery_role,
                    "BucketARN": format!("arn:aws:s3:::{BUCKET}"),
                    "Prefix": format!("{}/", row.slug),
                    "CompressionFormat": "UNCOMPRESSED",
                    "BufferingHints": {
                        "SizeInMBs": row.size_in_mibs,
                        "IntervalInSeconds": row.interval_in_seconds,
                    },
                },
            }),
        )
        .await;
        let stream_arn = created["DeliveryStreamARN"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a DeliveryStreamARN, got {created}"))
            .to_string();

        sesv2_ok(
            &router,
            case,
            "POST",
            "/v2/email/configuration-sets",
            Some(json!({"ConfigurationSetName": format!("{}-set", row.slug)})),
        )
        .await;
        sesv2_ok(
            &router,
            case,
            "POST",
            &format!(
                "/v2/email/configuration-sets/{}-set/event-destinations",
                row.slug
            ),
            Some(json!({
                "EventDestinationName": format!("{}-destination", row.slug),
                "EventDestination": {
                    "Enabled": true,
                    "MatchingEventTypes": [MATCHED_EVENT_TYPE],
                    "KinesisFirehoseDestination": {
                        "IamRoleArn": publishing_role,
                        "DeliveryStreamArn": stream_arn,
                    },
                },
            })),
        )
        .await;
    }
    for row in ROWS {
        given_active_stream(&router, row.case, &format!("{}-stream", row.slug)).await;
    }

    // The sends of each row, back to back, so they fall inside one interval of that row's
    // stream: one message per recipient, each through the row's own set.
    let mut sent: Vec<Vec<(String, String)>> = Vec::new();
    for row in ROWS {
        let mut messages = Vec::new();
        for (index, recipient) in row.recipients.iter().enumerate() {
            let subject = subject(row.slug, index);
            let answered = sesv2_ok(
                &router,
                row.case,
                "POST",
                "/v2/email/outbound-emails",
                Some(json!({
                    "FromEmailAddress": SENDER,
                    "Destination": {"ToAddresses": [recipient]},
                    "ConfigurationSetName": format!("{}-set", row.slug),
                    "Content": {"Simple": {
                        "Subject": {"Data": subject},
                        "Body": {"Text": {"Data": "the body of a buffered message"}},
                    }},
                })),
            )
            .await;
            let message_id = answered["MessageId"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: SendEmail must answer the MessageId it minted, got {answered}",
                        row.case
                    )
                })
                .to_string();
            messages.push((message_id, subject));
        }
        sent.push(messages);
    }

    for (row, messages) in ROWS.iter().zip(&sent) {
        let case = row.case;
        let prefix = format!("{}/", row.slug);
        let objects = delivered_events(&router, case, &prefix, messages.len()).await;

        // One event document per send, no matter how the buffering split them over objects:
        // nothing lost, nothing delivered twice.
        let delivered: Vec<&Value> = objects
            .iter()
            .flat_map(|(_, documents)| documents.iter())
            .collect();
        assert_eq!(
            delivered.len(),
            messages.len(),
            "case {case:?}: expected one published event document per send ({}), got {objects:?}",
            messages.len()
        );
        for (key, documents) in &objects {
            assert!(
                !documents.is_empty(),
                "case {case:?}: a delivered object carries the records of its buffer, one \
                 document per line, got {key:?} with none"
            );
        }

        let event_types: BTreeSet<&str> = delivered
            .iter()
            .map(|document| document["eventType"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            event_types,
            BTreeSet::from([PUBLISHED_EVENT_TYPE]),
            "case {case:?}: a destination receives only the events its MatchingEventTypes \
             names, got {objects:?}"
        );

        let delivered_messages: BTreeSet<(&str, &str)> = delivered
            .iter()
            .map(|document| {
                (
                    document["mail"]["messageId"].as_str().unwrap_or_default(),
                    document["mail"]["commonHeaders"]["subject"]
                        .as_str()
                        .unwrap_or_default(),
                )
            })
            .collect();
        let expected: BTreeSet<(&str, &str)> = messages
            .iter()
            .map(|(id, subject)| (id.as_str(), subject.as_str()))
            .collect();
        assert_eq!(
            delivered_messages, expected,
            "case {case:?}: every send of the interval must be published exactly once, got \
             {objects:?}"
        );
        for document in &delivered {
            assert_eq!(
                document["mail"]["tags"]["ses:configuration-set"],
                json!([format!("{}-set", row.slug)]),
                "case {case:?}: a published event names the configuration set it came from, got \
                 {document:?}"
            );
        }
    }
}
