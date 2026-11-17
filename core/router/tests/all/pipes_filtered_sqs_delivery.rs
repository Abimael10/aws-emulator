// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING EventBridge Pipe carrying `FilterCriteria` MOVES: the records its patterns
//! admit reach the target as the Amazon SQS record envelope, the records they do not are
//! consumed and never delivered, and every call on the way is made as the pipe's execution
//! role.
//!
//! Three crates have to agree for the rule to hold — EventBridge Pipes runs the pipe, Amazon
//! SQS holds the source and the target, IAM holds the role whose policies admit each stage's
//! call — so the suite lives here rather than in any one of them.
//! `services/service-com-amazonaws-pipes-pipes/tests/all/pipe_filter_body_format.rs` asks which
//! records a pattern admits; what is new here is the whole filtered path driven the way a client
//! drives it — a role IAM holds with exactly the four calls a queue-to-queue pipe makes, no
//! `TargetParameters` at all, so what the target receives is the record ENVELOPE itself — and the
//! envelope's own members asserted against what the source reported for each record.
//!
//! RULE: a pipe whose `SourceParameters` carry `FilterCriteria` polls its source with no client
//! action, admits only the records matching one of its patterns, consumes the rest without
//! touching the target, and delivers each admitted record to its target as that record's
//! envelope — `attributes`, `awsRegion`, `body`, `eventSource`, `eventSourceARN`, `md5OfBody`,
//! `md5OfMessageAttributes`, `messageAttributes`, `messageId` and `receiptHandle` — with the body
//! as the producer sent it, the id and digests the SOURCE message's own, `eventSourceARN` naming
//! the SOURCE queue, `awsRegion` the Region the records were polled in, `attributes` holding
//! exactly `ApproximateFirstReceiveTimestamp`, `ApproximateReceiveCount`, `SenderId` and
//! `SentTimestamp` with the count `"1"` on a first delivery, `messageAttributes` lowercase-keyed,
//! and the delivery itself carrying no message attributes of its own.
//!
//! The rows differ along what the rule generalises over: which record of a batch the pattern
//! admits (the first, the last, several), how many records are sent together, the batch size they
//! are polled under, how many patterns are ORed, whether an admitted record carries message
//! attributes at all, and whether the pipe was created explicitly RUNNING or left to the default.
//! The second test adds what one batch cannot show: that the pipe keeps deciding records sent
//! after it has already moved a batch.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{form_encode, opt_element};

/// The Region every call below is signed for, which is what each handler reads its Region from.
const REGION: &str = "us-east-1";

/// How long work the pipe has to do on its own is given before a case fails. Far above the data
/// plane's own poll interval and the span a pipe takes to reach RUNNING, so a case fails only
/// when the pipe did not do the work at all.
const BUDGET: Duration = Duration::from_secs(30);

/// How long an assertion that a record is NEVER delivered watches for, after everything the
/// patterns admit has arrived. A delivery that was going to be made would have: it is many times
/// the poller's own interval.
const QUIET: Duration = Duration::from_secs(2);

/// How often a queue or a pipe is looked at again while waiting.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// The visibility timeout each source queue is created with: a record the pipe took is hidden
/// from every other consumer for this long, so a record that reappears is one the pipe did not
/// finish with.
const SOURCE_VISIBILITY: &str = "60";

/// The `eventSource` an Amazon SQS record envelope reports.
const SQS_EVENT_SOURCE: &str = "aws:sqs";

/// Exactly the members an Amazon SQS record envelope carries. `md5OfMessageAttributes` is the one
/// member a record decides: Amazon SQS reports that digest for a message that CARRIES message
/// attributes and none for one with none.
const ENVELOPE_MEMBERS: [&str; 10] = [
    "attributes",
    "awsRegion",
    "body",
    "eventSource",
    "eventSourceARN",
    "md5OfBody",
    "md5OfMessageAttributes",
    "messageAttributes",
    "messageId",
    "receiptHandle",
];

/// The member the digest of a record's message attributes travels under, which is the one member
/// of [`ENVELOPE_MEMBERS`] a record without message attributes does not carry.
const MD5_OF_MESSAGE_ATTRIBUTES: &str = "md5OfMessageAttributes";

/// Exactly the system attributes an Amazon SQS record envelope reports under `attributes`.
const ENVELOPE_ATTRIBUTES: [&str; 4] = [
    "ApproximateFirstReceiveTimestamp",
    "ApproximateReceiveCount",
    "SenderId",
    "SentTimestamp",
];

/// The message attribute a send carries, when the case gives it one. An envelope reports an
/// attribute's value under `stringValue`, beside the `dataType` it was sent with.
const PROBE_ATTRIBUTE: &str = "probe";

/// The `DataType` every message attribute below is sent with.
const PROBE_DATA_TYPE: &str = "String";

/// One message a case sends to its source queue.
struct Sent {
    /// The `MessageBody`, as the producer sends it.
    body: &'static str,
    /// The `probe` message attribute's `StringValue`, when the case gives the message one.
    probe: Option<&'static str>,
    /// Whether the pipe's `FilterCriteria` must admit it, so it reaches the target.
    admitted: bool,
}

/// One row of the filtered-delivery table.
struct Row {
    /// The case, which also names the row's pipe and its queues.
    case: &'static str,
    /// The patterns `FilterCriteria.Filters` carries, in order.
    patterns: &'static [&'static str],
    /// `BatchSize`, when the row configures one.
    batch_size: Option<i64>,
    /// `DesiredState`, when the row names one rather than leaving it to the default.
    desired_state: Option<&'static str>,
    /// What the row sends to its source, in send order.
    messages: &'static [Sent],
}

/// What one `SendMessage` reported for a record: the values that record's envelope must carry.
struct Receipt {
    message_id: String,
    md5_of_body: String,
    md5_of_message_attributes: Option<String>,
}

/// RULE: see the module docs — a RUNNING pipe carrying `FilterCriteria` consumes its source,
/// suppresses the records no pattern admits, and delivers each admitted record to its target as
/// that record's own Amazon SQS envelope.
#[tokio::test]
async fn a_running_pipe_with_filter_criteria_delivers_the_records_its_patterns_admit() {
    let rows = [
        Row {
            // One suppressed record and one admitted control record, each carrying a message
            // attribute, polled under a batch that holds both.
            case: "one-admitted-after-one-suppressed",
            patterns: &[r#"{"body": {"kind": ["wanted"]}}"#],
            batch_size: Some(10),
            desired_state: None,
            messages: &[
                Sent {
                    body: r#"{"kind": "unwanted", "marker": "suppressed"}"#,
                    probe: Some("attr-bad"),
                    admitted: false,
                },
                Sent {
                    body: r#"{"kind": "wanted", "marker": "control"}"#,
                    probe: Some("attr-good"),
                    admitted: true,
                },
            ],
        },
        Row {
            // The admitted records FIRST, and more than one of them, one of them without any
            // message attribute: an envelope reports no attribute digest for a record that
            // carries none.
            case: "several-admitted-before-one-suppressed",
            patterns: &[r#"{"body": {"kind": ["wanted"]}}"#],
            batch_size: Some(10),
            desired_state: Some("RUNNING"),
            messages: &[
                Sent {
                    body: r#"{"kind": "wanted", "marker": "first"}"#,
                    probe: Some("attr-first"),
                    admitted: true,
                },
                Sent {
                    body: r#"{"kind": "wanted", "marker": "second"}"#,
                    probe: None,
                    admitted: true,
                },
                Sent {
                    body: r#"{"kind": "unwanted", "marker": "dropped"}"#,
                    probe: None,
                    admitted: false,
                },
            ],
        },
        Row {
            // One record per batch, so the records are moved batch by batch, and two patterns
            // ORed, so each admitted record is admitted by a different one.
            case: "one-record-per-batch-with-two-patterns",
            patterns: &[
                r#"{"body": {"kind": ["wanted"]}}"#,
                r#"{"body": {"kind": ["also-wanted"]}}"#,
            ],
            batch_size: Some(1),
            desired_state: None,
            messages: &[
                Sent {
                    body: r#"{"kind": "also-wanted", "marker": "by-the-second-pattern"}"#,
                    probe: Some("attr-second"),
                    admitted: true,
                },
                Sent {
                    body: r#"{"kind": "neither", "marker": "dropped"}"#,
                    probe: None,
                    admitted: false,
                },
                Sent {
                    body: r#"{"kind": "wanted", "marker": "by-the-first-pattern"}"#,
                    probe: None,
                    admitted: true,
                },
            ],
        },
    ];

    for row in &rows {
        run_row(row).await;
    }
}

/// Keep the table honest: every row is driven by this one body.
async fn run_row(row: &Row) {
    let case = row.case;
    let router = emulator();
    let (source_url, source_arn) = given_queue(&router, case, &format!("{case}-src"), true).await;
    let (target_url, target_arn) = given_queue(&router, case, &format!("{case}-tgt"), false).await;
    let role_arn =
        given_role(&router, case, &format!("{case}-role"), &source_arn, &target_arn).await;

    given_pipe(
        &router,
        case,
        &source_arn,
        &target_arn,
        &role_arn,
        row.desired_state,
        source_parameters(row.batch_size, row.patterns),
    )
    .await;

    // What each send reported, by the body it sent: the id and digests the envelope of that
    // record must carry are the SOURCE message's, never values computed here.
    let mut receipts: BTreeMap<String, Receipt> = BTreeMap::new();
    for message in row.messages {
        receipts.insert(
            message.body.to_string(),
            send(&router, case, &source_url, message).await,
        );
    }

    let admitted: Vec<&Sent> = row.messages.iter().filter(|sent| sent.admitted).collect();
    // Wait for everything the patterns admit, then keep watching: a record the filter must DROP
    // would arrive late, so the quiet window is what makes its absence an answer and not a race.
    let mut delivered = collect_at_least(&router, case, &target_url, admitted.len(), BUDGET).await;
    delivered.extend(collect_during(&router, case, &target_url, QUIET).await);

    let bodies: Vec<String> = delivered
        .iter()
        .map(|message| envelope_body(case, message))
        .collect();
    let seen: BTreeSet<String> = bodies.iter().cloned().collect();
    let wanted: BTreeSet<String> = admitted.iter().map(|sent| sent.body.to_string()).collect();
    assert_eq!(
        seen, wanted,
        "case {case:?}: expected the target to receive exactly the admitted records {wanted:?}, \
         got {bodies:?}"
    );
    assert_eq!(
        delivered.len(),
        admitted.len(),
        "case {case:?}: expected {} deliveries, got {bodies:?}",
        admitted.len()
    );

    for message in &delivered {
        let body = envelope_body(case, message);
        let sent = row
            .messages
            .iter()
            .find(|sent| sent.body == body)
            .unwrap_or_else(|| panic!("case {case:?}: the delivered body {body:?} was never sent"));
        assert_envelope(
            case,
            &envelope_of(case, message),
            sent,
            &receipts[&body],
            &source_arn,
        );
        // The delivery carries the record as its message BODY and no message attributes of its
        // own: a record's attributes travel inside the envelope.
        assert!(
            message["MessageAttributes"].is_null(),
            "case {case:?}: expected the delivery of {body:?} to carry no MessageAttributes of \
             its own, got {}",
            message["MessageAttributes"]
        );
    }

    // A record the filter dropped is still CONSUMED: it entered the pipe and the pipe decided
    // about it, so nothing the pipe polled is left on the source — neither waiting on it nor
    // checked out of it.
    let held = wait_for_held(&router, case, &source_url, 0, BUDGET).await;
    assert_eq!(
        held, 0,
        "case {case:?}: expected every polled record consumed, the source still holds {held}"
    );
}

/// The records one wave sends, and what the pipe must make of them.
const FIRST_WAVE: [Sent; 2] = [
    Sent {
        body: r#"{"kind": "unwanted", "wave": "first"}"#,
        probe: None,
        admitted: false,
    },
    Sent {
        body: r#"{"kind": "wanted", "wave": "first"}"#,
        probe: Some("attr-first-wave"),
        admitted: true,
    },
];

/// The records sent only once the first wave has been moved, which no single batch can show.
const SECOND_WAVE: [Sent; 2] = [
    Sent {
        body: r#"{"kind": "unwanted", "wave": "second"}"#,
        probe: None,
        admitted: false,
    },
    Sent {
        body: r#"{"kind": "wanted", "wave": "second"}"#,
        probe: Some("attr-second-wave"),
        admitted: true,
    },
];

/// RULE: a pipe polls its source for as long as it is RUNNING, so the criteria decide the records
/// sent AFTER a batch has already been moved exactly as they decided that batch — a client that
/// keeps producing keeps seeing its admitted records delivered and its suppressed ones consumed.
#[tokio::test]
async fn a_running_pipe_keeps_filtering_the_records_sent_after_it_moved_a_batch() {
    let case = "waves-after-the-first-batch";
    let router = emulator();
    let (source_url, source_arn) = given_queue(&router, case, &format!("{case}-src"), true).await;
    let (target_url, target_arn) = given_queue(&router, case, &format!("{case}-tgt"), false).await;
    let role_arn =
        given_role(&router, case, &format!("{case}-role"), &source_arn, &target_arn).await;
    given_pipe(
        &router,
        case,
        &source_arn,
        &target_arn,
        &role_arn,
        None,
        source_parameters(Some(10), &[r#"{"body": {"kind": ["wanted"]}}"#]),
    )
    .await;

    for wave in [&FIRST_WAVE, &SECOND_WAVE] {
        let mut receipts: BTreeMap<String, Receipt> = BTreeMap::new();
        for message in wave {
            receipts.insert(
                message.body.to_string(),
                send(&router, case, &source_url, message).await,
            );
        }
        let admitted: Vec<&Sent> = wave.iter().filter(|sent| sent.admitted).collect();
        let mut delivered =
            collect_at_least(&router, case, &target_url, admitted.len(), BUDGET).await;
        delivered.extend(collect_during(&router, case, &target_url, QUIET).await);

        let bodies: Vec<String> = delivered
            .iter()
            .map(|message| envelope_body(case, message))
            .collect();
        let seen: BTreeSet<String> = bodies.iter().cloned().collect();
        let wanted: BTreeSet<String> = admitted.iter().map(|sent| sent.body.to_string()).collect();
        assert_eq!(
            seen, wanted,
            "case {case:?}: expected this wave's admitted records {wanted:?} at the target, got \
             {bodies:?}"
        );
        for message in &delivered {
            let body = envelope_body(case, message);
            let sent = wave
                .iter()
                .find(|sent| sent.body == body)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: the delivered body {body:?} was never sent")
                });
            assert_envelope(
                case,
                &envelope_of(case, message),
                sent,
                &receipts[&body],
                &source_arn,
            );
        }
        let held = wait_for_held(&router, case, &source_url, 0, BUDGET).await;
        assert_eq!(
            held, 0,
            "case {case:?}: expected this wave consumed in full, the source still holds {held}"
        );
    }
}

/// Every assertion about ONE delivered envelope, against what the record's source reported.
fn assert_envelope(
    case: &str,
    envelope: &Value,
    sent: &Sent,
    receipt: &Receipt,
    source_arn: &str,
) {
    let body = sent.body;
    let members: BTreeSet<String> = envelope
        .as_object()
        .map(|members| members.keys().cloned().collect())
        .unwrap_or_default();
    let expected: BTreeSet<String> = ENVELOPE_MEMBERS
        .iter()
        .filter(|member| **member != MD5_OF_MESSAGE_ATTRIBUTES || sent.probe.is_some())
        .map(|member| (*member).to_string())
        .collect();
    assert_eq!(
        members, expected,
        "case {case:?}: expected the envelope of {body:?} to carry exactly {expected:?}, got \
         {members:?}"
    );
    assert_eq!(
        envelope["body"].as_str(),
        Some(body),
        "case {case:?}: expected the envelope to carry the body as sent, got {}",
        envelope["body"]
    );
    assert_eq!(
        envelope["messageId"].as_str(),
        Some(receipt.message_id.as_str()),
        "case {case:?}: expected the envelope of {body:?} to report the source MessageId {:?}, \
         got {}",
        receipt.message_id,
        envelope["messageId"]
    );
    assert_eq!(
        envelope["md5OfBody"].as_str(),
        Some(receipt.md5_of_body.as_str()),
        "case {case:?}: expected the envelope of {body:?} to report the source \
         MD5OfMessageBody {:?}, got {}",
        receipt.md5_of_body,
        envelope["md5OfBody"]
    );
    assert_eq!(
        envelope[MD5_OF_MESSAGE_ATTRIBUTES].as_str(),
        receipt.md5_of_message_attributes.as_deref(),
        "case {case:?}: expected the envelope of {body:?} to report the source \
         MD5OfMessageAttributes {:?}, got {}",
        receipt.md5_of_message_attributes,
        envelope[MD5_OF_MESSAGE_ATTRIBUTES]
    );
    assert_eq!(
        envelope["eventSource"].as_str(),
        Some(SQS_EVENT_SOURCE),
        "case {case:?}: expected the envelope of {body:?} to report eventSource \
         {SQS_EVENT_SOURCE:?}, got {}",
        envelope["eventSource"]
    );
    assert_eq!(
        envelope["eventSourceARN"].as_str(),
        Some(source_arn),
        "case {case:?}: expected the envelope of {body:?} to name the SOURCE queue \
         {source_arn:?}, got {}",
        envelope["eventSourceARN"]
    );
    assert_eq!(
        envelope["awsRegion"].as_str(),
        Some(REGION),
        "case {case:?}: expected the envelope of {body:?} to report awsRegion {REGION:?}, got {}",
        envelope["awsRegion"]
    );
    assert!(
        !envelope["receiptHandle"]
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "case {case:?}: expected the envelope of {body:?} to carry the record's receipt handle, \
         got {}",
        envelope["receiptHandle"]
    );
    let attributes: BTreeSet<String> = envelope["attributes"]
        .as_object()
        .map(|members| members.keys().cloned().collect())
        .unwrap_or_default();
    let wanted: BTreeSet<String> = ENVELOPE_ATTRIBUTES
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    assert_eq!(
        attributes, wanted,
        "case {case:?}: expected the envelope of {body:?} to report exactly {wanted:?} under \
         attributes, got {attributes:?}"
    );
    assert_eq!(
        envelope["attributes"]["ApproximateReceiveCount"].as_str(),
        Some("1"),
        "case {case:?}: expected the envelope of {body:?} to report ApproximateReceiveCount \
         \"1\" on its first delivery, got {}",
        envelope["attributes"]["ApproximateReceiveCount"]
    );
    match sent.probe {
        None => assert!(
            envelope["messageAttributes"]
                .as_object()
                .is_some_and(serde_json::Map::is_empty),
            "case {case:?}: expected the envelope of {body:?} to report no message attributes, \
             got {}",
            envelope["messageAttributes"]
        ),
        Some(value) => {
            assert_eq!(
                envelope["messageAttributes"][PROBE_ATTRIBUTE]["stringValue"].as_str(),
                Some(value),
                "case {case:?}: expected the envelope of {body:?} to carry the \
                 {PROBE_ATTRIBUTE} attribute's stringValue {value:?}, got {}",
                envelope["messageAttributes"]
            );
            assert_eq!(
                envelope["messageAttributes"][PROBE_ATTRIBUTE]["dataType"].as_str(),
                Some(PROBE_DATA_TYPE),
                "case {case:?}: expected the envelope of {body:?} to carry the \
                 {PROBE_ATTRIBUTE} attribute's dataType {PROBE_DATA_TYPE:?}, got {}",
                envelope["messageAttributes"]
            );
        }
    }
}

/// The envelope one delivered message carries as its body.
fn envelope_of(case: &str, message: &Value) -> Value {
    let payload = message["Body"].as_str().unwrap_or_default();
    serde_json::from_str(payload).unwrap_or_else(|error| {
        panic!("case {case:?}: a delivered body is not JSON ({error}): {payload:?}")
    })
}

/// The source message body one delivered envelope carries.
fn envelope_body(case: &str, message: &Value) -> String {
    let envelope = envelope_of(case, message);
    envelope["body"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the delivered record to carry the message body as sent, \
                 got {envelope}"
            )
        })
        .to_string()
}

/// The `SourceParameters` a case creates its pipe with.
fn source_parameters(batch_size: Option<i64>, patterns: &[&str]) -> Value {
    let mut queue_parameters = serde_json::Map::new();
    if let Some(size) = batch_size {
        queue_parameters.insert("BatchSize".to_string(), json!(size));
    }
    let filters: Vec<Value> = patterns
        .iter()
        .map(|pattern| json!({ "Pattern": pattern }))
        .collect();
    json!({
        "SqsQueueParameters": Value::Object(queue_parameters),
        "FilterCriteria": { "Filters": filters },
    })
}

/// Pipes, Amazon SQS and IAM on ONE engine, exactly as `core/router/src/lib.rs` mounts them.
/// Request authentication is off, as in every other suite here; the calls the pipe's own stages
/// make are signed either way (the credential scope is what routes them).
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_pipes_pipes::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`], which is
/// what routes the request. The signature is never checked: this router runs with request
/// authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// One Amazon SQS call over its `awsJson1.0` wire.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header("authorization", authorization("sqs"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice::<Value>(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One EventBridge Pipes call over its `restJson1` wire, on the URI the operation's `@http`
/// binding declares.
async fn pipes(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body
        .map(|body| Bytes::from(body.to_string()))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("pipes.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("pipes"))
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One IAM call: IAM speaks `awsQuery`, so its parameters travel as a form body and its answer
/// comes back as XML.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("iam.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// A queue called `name`, with the source's own visibility timeout when `source`; answered with
/// its url and the ARN a pipe names it by.
async fn given_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    source: bool,
) -> (String, String) {
    let mut request = json!({ "QueueName": name });
    if source {
        request["Attributes"] = json!({ "VisibilityTimeout": SOURCE_VISIBILITY });
    }
    let (status, created) = sqs(router, "CreateQueue", &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateQueue answered {status} {created}"
    );
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {created}"))
        .to_string();
    let (status, attributes) = sqs(
        router,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetQueueAttributes answered {status} {attributes}"
    );
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {attributes}"))
        .to_string();
    (url, arn)
}

/// A role trusting EventBridge Pipes and allowing exactly the calls a queue-to-queue pipe's
/// stages make — the source's receive, delete and attribute reads, and the target's send, each on
/// the queue the stage makes it on. Answered with the ARN IAM persisted.
async fn given_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    source_arn: &str,
    target_arn: &str,
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "pipes.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let (status, created) = iam(
        router,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateRole answered {status} {created}"
    );
    let arn =
        opt_element(&created, "Arn").unwrap_or_else(|| panic!("case {case:?}: no Arn in {created}"));
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": ["sqs:ReceiveMessage", "sqs:DeleteMessage", "sqs:GetQueueAttributes"],
                "Resource": source_arn,
            },
            {
                "Effect": "Allow",
                "Action": "sqs:SendMessage",
                "Resource": target_arn,
            },
        ],
    })
    .to_string();
    let (status, written) = iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "move-records"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRolePolicy answered {status} {written}"
    );
    arn
}

/// A pipe named after the case, from `source_arn` to `target_arn`, carrying `parameters` and no
/// target parameters at all, waited on until it reports `CurrentState` RUNNING — which is when it
/// polls.
async fn given_pipe(
    router: &MultiServiceRouter,
    case: &str,
    source_arn: &str,
    target_arn: &str,
    role_arn: &str,
    desired_state: Option<&str>,
    parameters: Value,
) {
    let mut body = json!({
        "Source": source_arn,
        "Target": target_arn,
        "RoleArn": role_arn,
        "SourceParameters": parameters,
    });
    if let Some(desired) = desired_state {
        body["DesiredState"] = json!(desired);
    }
    let (status, created) = pipes(router, "POST", &format!("/v1/pipes/{case}"), Some(body)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreatePipe answered {status} {created}"
    );
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, described) = pipes(router, "GET", &format!("/v1/pipes/{case}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribePipe answered {status} {described}"
        );
        let state = described["CurrentState"].as_str().unwrap_or_default();
        if state == "RUNNING" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the pipe to reach CurrentState RUNNING, it reports \
             {state:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Send one message to the queue at `url`, answered with what Amazon SQS reported for it — the id
/// and digests that record's envelope must carry.
async fn send(router: &MultiServiceRouter, case: &str, url: &str, message: &Sent) -> Receipt {
    let mut request = json!({ "QueueUrl": url, "MessageBody": message.body });
    if let Some(probe) = message.probe {
        request["MessageAttributes"] = json!({
            PROBE_ATTRIBUTE: { "DataType": PROBE_DATA_TYPE, "StringValue": probe },
        });
    }
    let (status, sent) = sqs(router, "SendMessage", &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: SendMessage of {:?} answered {status} {sent}",
        message.body
    );
    Receipt {
        message_id: sent["MessageId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no MessageId in {sent}"))
            .to_string(),
        md5_of_body: sent["MD5OfMessageBody"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no MD5OfMessageBody in {sent}"))
            .to_string(),
        md5_of_message_attributes: sent["MD5OfMessageAttributes"].as_str().map(str::to_string),
    }
}

/// One `ReceiveMessage` of up to ten messages at `url`, asking for every attribute a delivery
/// could carry, each deleted so a later look does not count it twice.
async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let (status, received) = sqs(
        router,
        "ReceiveMessage",
        &json!({
            "QueueUrl": url,
            "MaxNumberOfMessages": 10,
            "MessageSystemAttributeNames": ["All"],
            "MessageAttributeNames": ["All"],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ReceiveMessage answered {status} {received}"
    );
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    for message in &messages {
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            let (status, deleted) = sqs(
                router,
                "DeleteMessage",
                &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: DeleteMessage answered {status} {deleted}"
            );
        }
    }
    messages
}

/// Every message delivered to `url`, collected until there are at least `want` of them or
/// `budget` lapses.
async fn collect_at_least(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    want: usize,
    budget: Duration,
) -> Vec<Value> {
    let deadline = Instant::now() + budget;
    let mut collected: Vec<Value> = Vec::new();
    loop {
        collected.extend(drain(router, case, url).await);
        if collected.len() >= want || Instant::now() >= deadline {
            return collected;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Every message that arrives at `url` during the whole of `budget`.
async fn collect_during(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    budget: Duration,
) -> Vec<Value> {
    let deadline = Instant::now() + budget;
    let mut collected: Vec<Value> = Vec::new();
    while Instant::now() < deadline {
        collected.extend(drain(router, case, url).await);
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    collected
}

/// The attributes that together say how many records a queue still HOLDS: the ones waiting on it
/// plus the ones a consumer has taken and not deleted ("messages that are in flight",
/// `ApproximateNumberOfMessagesNotVisible`), which is what a record a pipe took and did not
/// finish with is one of.
const HELD_ATTRIBUTES: [&str; 2] = [
    "ApproximateNumberOfMessages",
    "ApproximateNumberOfMessagesNotVisible",
];

/// Wait until the queue at `url` holds `want` records, answering what it held last.
async fn wait_for_held(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    want: i64,
    budget: Duration,
) -> i64 {
    let deadline = Instant::now() + budget;
    loop {
        let (status, answered) = sqs(
            router,
            "GetQueueAttributes",
            &json!({ "QueueUrl": url, "AttributeNames": HELD_ATTRIBUTES }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetQueueAttributes answered {status} {answered}"
        );
        let counted: i64 = HELD_ATTRIBUTES
            .iter()
            .map(|attribute| {
                answered["Attributes"][attribute]
                    .as_str()
                    .unwrap_or_default()
                    .parse()
                    .unwrap_or(-1)
            })
            .sum();
        if counted == want || Instant::now() >= deadline {
            return counted;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}
