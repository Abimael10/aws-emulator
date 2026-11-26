// SPDX-License-Identifier: CC0-1.0

//! What an EMPTY content-filter operand admits, driven against the WHOLE assembled
//! router with request authentication on — the configuration the emulator serves
//! traffic in — and observed where a client observes it: on the Amazon SQS queue each
//! rule of a custom event bus targets.
//!
//! The RULE under test: **a content-filter operand is compared as the TEXT it is, so an
//! empty operand is the empty text and not a wildcard.** `prefix` and `suffix` compare
//! one END of the leaf, and every string starts and ends with the empty text, so an
//! empty one holds for every STRING leaf — the empty string included; `wildcard` and
//! `equals-ignore-case` compare the WHOLE leaf, so an empty one holds only for the empty
//! string. No match type that compares text holds for a leaf that is not text (a number,
//! a boolean, `null`) or for a field the event does not carry — `exists` is the only
//! match type about presence. Every accepted `PutEvents` entry (`FailedEntryCount` 0,
//! one `PutEventsResultEntry` carrying an `EventId`) is matched independently against
//! every ENABLED rule of the bus it names and delivered to each matching target, and
//! `TestEventPattern` answers the same verdict for a pattern/event pair as the live rule
//! delivers by. Sources: `PutEvents` for the batch and the per-entry `EventId`,
//! `PutRule`/`PutTargets` for the rules and their targets, and the EventBridge
//! event-pattern reference for the four text-comparing match types; the model constrains
//! `EventPattern` by `@length` alone (`smithy_query(query="operation",
//! operation="PutRule,TestEventPattern")`), so the comparison itself is documented
//! behaviour.
//!
//! What this suite adds to the service crate's own
//! (`services/service-com-amazonaws-eventbridge-awsevents/tests/all/\
//! pattern_empty_operand_delivery.rs`, which pins the same rule over the two services
//! alone) is the axis that crate cannot see, and three the recorded client exercises:
//!
//! * the ASSEMBLED router — every service mounted, every signature verified — so each
//!   delivery EventBridge makes through the in-process callback is a whole signed request
//!   through the same front door a client's call arrives at;
//! * a CONTROL rule scoped to the publication's `source` alone, which carries no `detail`
//!   clause at all and must therefore receive every published case, including the leaves
//!   no text-comparing operand admits;
//! * a queue policy whose `aws:SourceArn` condition names the rule ARN the CLIENT
//!   CONSTRUCTS from the documented form before the rule exists, rather than one read
//!   back from `PutRule` — so the ARN the service mints and the ARN a client addresses
//!   that rule by have to be the same string for any delivery to be admitted at all;
//! * the leaves that are not text (a number, `null`, a boolean) and a `detail` that does
//!   not carry the field, each of which must reach the control rule's queue and NO
//!   operand rule's queue.
//!
//! The rows span the class rather than one case: the OPERAND rows differ along which end
//! of the leaf the match type compares (one end, the whole of it) and whether the
//! comparison folds case; the VALUE rows along the KIND of the leaf (string, number,
//! boolean, null, absent) and, for the strings, along their length, their case and
//! whether they are blank. The same table is then read back through `TestEventPattern`,
//! so the reporting surface and the live rule answer one verdict.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and
//! the requests driven — on an explicitly sized thread.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::SystemTime;

use aws_credential_types::Credentials;
use aws_emulator_router::{build_router, InMemoryEngine};
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4::SigningParams;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// Keep this at or above the size `boot_serve.rs` uses: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call is signed for and addressed in.
const REGION: &str = "us-east-1";

/// The service principal EventBridge delivers to an SQS target as, which is what the
/// queue's resource policy admits ("for Amazon SQS queues, EventBridge relies on
/// resource-based policies", `PutTargets`).
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The custom bus the rules live on, and the `Source` / `DetailType` every entry carries.
const BUS: &str = "empty-operand-matrix-bus";
const SOURCE: &str = "com.example.pattern-language";
const DETAIL_TYPE: &str = "Empty Operand Case";

/// The field the operand rules are about, and the member each published `detail` carries
/// so a drained message says which value row it is.
const FIELD: &str = "k";
const MARKER: &str = "case";

/// How many times a queue must report nothing left before its drain is over. A delivery
/// is made by the `PutEvents` request that decided it, so one empty answer already means
/// the queue is drained; the repeats are what make a message the service delivered LATE
/// — which would be a defect this suite must catch — show up as an extra rather than be
/// missed.
const QUIET_ANSWERS: usize = 3;

/// The most receives one queue's drain may make before the suite gives up on it. Far
/// above what this matrix needs (each queue holds at most the published cases, ten to a
/// receive), so a loaded host clears it many times over; it bounds the loop by the
/// CONDITION it waits for rather than by a span of the clock.
const RECEIVE_BUDGET: usize = 200;

/// One value row: its label, and the leaf the published `detail` carries at [`FIELD`] —
/// `None` for the row whose `detail` does not carry that field at all.
struct ValueRow {
    label: &'static str,
    leaf: Option<fn() -> Value>,
}

/// One operand row: the case, the whole TERM its rule's pattern carries for [`FIELD`],
/// and the labels of the value rows that term admits.
struct OperandRow {
    rule: &'static str,
    term: fn() -> Value,
    admits: &'static [&'static str],
}

/// The labels of the value rows whose leaf is a STRING, which is what a comparison
/// against one END of the leaf admits — every string starts and ends with the empty text.
const EVERY_STRING: &[&str] = &["text", "empty", "blank", "upper"];

/// The label of the only value row a comparison against the WHOLE leaf admits.
const ONLY_THE_EMPTY_STRING: &[&str] = &["empty"];

/// Every value row, so the control rule's expected set is stated once.
const EVERY_CASE: &[&str] = &[
    "text", "empty", "blank", "upper", "number", "null", "boolean", "absent",
];

/// The value rows: one published entry each, differing along the KIND of the leaf and,
/// for the strings, along length, case and blankness.
fn value_rows() -> [ValueRow; 8] {
    [
        ValueRow {
            label: "text",
            leaf: Some(|| json!("x")),
        },
        ValueRow {
            label: "empty",
            leaf: Some(|| json!("")),
        },
        ValueRow {
            label: "blank",
            leaf: Some(|| json!("   ")),
        },
        ValueRow {
            label: "upper",
            leaf: Some(|| json!("X")),
        },
        ValueRow {
            label: "number",
            leaf: Some(|| json!(5)),
        },
        ValueRow {
            label: "null",
            leaf: Some(|| json!(null)),
        },
        ValueRow {
            label: "boolean",
            leaf: Some(|| json!(true)),
        },
        ValueRow {
            label: "absent",
            leaf: None,
        },
    ]
}

/// The operand rows: the control rule, which carries no `detail` clause and so admits
/// every case, and one rule per text-comparing match type handed the empty string.
fn operand_rows() -> [OperandRow; 5] {
    [
        OperandRow {
            rule: "control",
            term: || Value::Null,
            admits: EVERY_CASE,
        },
        OperandRow {
            rule: "empty-prefix",
            term: || json!({ "prefix": "" }),
            admits: EVERY_STRING,
        },
        OperandRow {
            rule: "empty-suffix",
            term: || json!({ "suffix": "" }),
            admits: EVERY_STRING,
        },
        OperandRow {
            rule: "empty-wildcard",
            term: || json!({ "wildcard": "" }),
            admits: ONLY_THE_EMPTY_STRING,
        },
        OperandRow {
            rule: "empty-equals-ignore-case",
            term: || json!({ "equals-ignore-case": "" }),
            admits: ONLY_THE_EMPTY_STRING,
        },
    ]
}

/// The whole pattern one operand row's rule carries: the publication's own `source` —
/// which the CONTROL row is scoped by and nothing else — and, for every other row, the
/// detail type and that row's term for [`FIELD`].
fn pattern(row: &OperandRow) -> Value {
    let term = (row.term)();
    if term.is_null() {
        return json!({ "source": [SOURCE] });
    }
    json!({
        "source": [SOURCE],
        "detail-type": [DETAIL_TYPE],
        "detail": { FIELD: [term] },
    })
}

/// The `detail` one value row publishes: the marker naming the row, and the leaf itself
/// where the row carries one.
fn detail(row: &ValueRow) -> Value {
    let mut detail = serde_json::Map::new();
    detail.insert(MARKER.to_string(), json!(row.label));
    if let Some(leaf) = row.leaf {
        detail.insert(FIELD.to_string(), leaf());
    }
    Value::Object(detail)
}

/// An event envelope carrying `detail`, in the shape `TestEventPattern` is handed one:
/// "the following fields are mandatory: id account source time region resources
/// detail-type" (`TestEventPattern$Event`).
fn envelope(detail: &Value) -> String {
    json!({
        "version": "0",
        "id": "00000000-0000-0000-0000-000000000000",
        "detail-type": DETAIL_TYPE,
        "source": SOURCE,
        "account": engine::EMULATOR_ACCOUNT_ID,
        "time": "2026-01-01T00:00:00Z",
        "region": REGION,
        "resources": [],
        "detail": detail,
    })
    .to_string()
}

/// The credential every call below signs with: the pair this process publishes once
/// request authentication is on.
struct Signer {
    key_id: String,
    secret: String,
}

impl Signer {
    fn sign(
        &self,
        mut request: http::Request<Vec<u8>>,
        signing_name: &str,
    ) -> http::Request<Vec<u8>> {
        let identity = Credentials::new(&self.key_id, &self.secret, None, None, "test").into();
        let params = SigningParams::builder()
            .identity(&identity)
            .region(REGION)
            .name(signing_name)
            .time(SystemTime::now())
            .settings(SigningSettings::default())
            .build()
            .expect("the signing parameters are complete")
            .into();
        let (method, uri) = (request.method().to_string(), request.uri().to_string());
        let headers: Vec<(String, String)> = request
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().expect("a text header").to_string(),
                )
            })
            .collect();
        let signable = SignableRequest::new(
            &method,
            &uri,
            headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
            SignableBody::Bytes(request.body()),
        )
        .expect("the request is signable");
        let (instructions, _) = sign(signable, &params)
            .expect("the request signs")
            .into_parts();
        instructions.apply_to_request_http1x(&mut request);
        request
    }
}

/// One signed RPC call on the regional endpoint of `signing_name`.
async fn call(
    router: &MultiServiceRouter,
    signer: &Signer,
    signing_name: &str,
    content_type: &str,
    target: &str,
    body: &Value,
) -> (u16, Value) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let built = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", content_type)
        .header("x-amz-target", target)
        .body(body.to_string().into_bytes())
        .expect("the test request is well formed");
    let signed = signer.sign(built, signing_name);
    let (parts, payload) = signed.into_parts();
    let request = http::Request::from_parts(parts, bytes::Bytes::from(payload));
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`), answered with its status
/// and body.
async fn events(
    router: &MultiServiceRouter,
    signer: &Signer,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    call(
        router,
        signer,
        "events",
        "application/x-amz-json-1.1",
        &format!("AWSEvents.{operation}"),
        body,
    )
    .await
}

/// One EventBridge call that must answer 200.
async fn events_ok(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = events(router, signer, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that must answer 200.
async fn sqs(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = call(
        router,
        signer,
        "sqs",
        "application/x-amz-json-1.0",
        &format!("AmazonSQS.{operation}"),
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// The ARN a client addresses the rule `rule` of the custom bus [`BUS`] by, built from
/// the documented form — `arn:aws:events:<region>:<account>:rule/<bus>/<name>` for a rule
/// on a bus other than the account's `default` one — BEFORE the rule exists, which is the
/// position a client writing its target queue's policy first is in.
fn constructed_rule_arn(rule: &str) -> String {
    format!(
        "arn:aws:events:{REGION}:{}:rule/{BUS}/{rule}",
        engine::EMULATOR_ACCOUNT_ID
    )
}

/// A queue called `name` whose policy admits the deliveries of `rule_arn` and of no other
/// rule; answers its url and the ARN a rule names it by.
async fn queue(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    name: &str,
    rule_arn: &str,
) -> (String, String) {
    let created = sqs(
        router,
        signer,
        case,
        "CreateQueue",
        &json!({ "QueueName": name }),
    )
    .await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        router,
        signer,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": &url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {attributes}"))
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowThisRule",
            "Effect": "Allow",
            "Principal": { "Service": EVENTS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": &arn,
            "Condition": { "ArnEquals": { "aws:SourceArn": rule_arn } },
        }],
    })
    .to_string();
    sqs(
        router,
        signer,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": &url, "Attributes": { "Policy": policy } }),
    )
    .await;
    (url, arn)
}

/// The value-row labels of every message waiting on the queue at `url`, drained until the
/// queue has reported nothing left [`QUIET_ANSWERS`] times in a row and each message
/// deleted as it is read.
///
/// Every label is collected, including one no row expected: a delivery this matrix forbids
/// has to be observable as an extra rather than silently skipped.
async fn drained(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    url: &str,
) -> Vec<String> {
    let mut labels: Vec<String> = Vec::new();
    let mut quiet = 0usize;
    for _ in 0..RECEIVE_BUDGET {
        if quiet >= QUIET_ANSWERS {
            return labels;
        }
        let received = sqs(
            router,
            signer,
            case,
            "ReceiveMessage",
            &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
        )
        .await;
        let messages = received["Messages"].as_array().cloned().unwrap_or_default();
        if messages.is_empty() {
            quiet += 1;
            continue;
        }
        quiet = 0;
        for message in messages {
            let text = message["Body"].as_str().unwrap_or_default();
            let delivered: Value = serde_json::from_str(text).unwrap_or_else(|error| {
                panic!("case {case:?}: a delivered body is not JSON ({error}): {text:?}")
            });
            let label = delivered["detail"][MARKER]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: a delivered envelope names no value row: {delivered}")
                })
                .to_string();
            labels.push(label);
            let receipt = message["ReceiptHandle"].as_str().unwrap_or_default();
            sqs(
                router,
                signer,
                case,
                "DeleteMessage",
                &json!({ "QueueUrl": url, "ReceiptHandle": receipt }),
            )
            .await;
        }
    }
    panic!(
        "case {case:?}: the queue was still answering after {RECEIVE_BUDGET} receives, holding \
         {labels:?}"
    );
}

/// RULE: each rule's SQS target receives exactly the published cases its own empty operand
/// admits — every string for a comparison against one END of the leaf, only the empty
/// string for one against the WHOLE leaf, and no leaf that is not text or field the event
/// does not carry — while the control rule receives every case; and `TestEventPattern`
/// answers that same verdict pair by pair.
#[test]
fn an_empty_operand_admits_exactly_the_leaves_it_compares_equal() {
    std::thread::Builder::new()
        .name("empty-operand-matrix".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(drive());
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread runs to completion");
}

async fn drive() {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = build_router(engine);
    // The whole suite runs with request authentication ON, as the emulator serves
    // traffic: every call below carries the signature this process's keys produce, and so
    // does every delivery EventBridge makes through the callback.
    router.set_request_authentication(true);
    let published = ::emulator_server::SigV4Authenticator::minted_credentials()
        .expect("turning authentication on publishes this process's keys");
    let signer = Signer {
        key_id: published.access_key_id.clone(),
        secret: published.secret_access_key.clone(),
    };

    events_ok(
        &router,
        &signer,
        "the custom bus",
        "CreateEventBus",
        &json!({ "Name": BUS }),
    )
    .await;

    let operands = operand_rows();
    let values = value_rows();

    // One rule per operand row, each with a queue of its own whose policy admits that
    // rule's deliveries and no others — written from the ARN the client CONSTRUCTS, so a
    // rule ARN the service minted in another form would admit nothing.
    let mut queues: Vec<(&str, String)> = Vec::with_capacity(operands.len());
    for row in &operands {
        let case = row.rule;
        let rule_arn = constructed_rule_arn(row.rule);
        let (url, queue_arn) = queue(
            &router,
            &signer,
            case,
            &format!("empty-operand-{}", row.rule),
            &rule_arn,
        )
        .await;
        let minted = events_ok(
            &router,
            &signer,
            case,
            "PutRule",
            &json!({
                "Name": row.rule,
                "EventBusName": BUS,
                "EventPattern": pattern(row).to_string(),
                "State": "ENABLED",
            }),
        )
        .await;
        assert_eq!(
            minted["RuleArn"].as_str().unwrap_or_default(),
            rule_arn,
            "case {case:?}: expected the rule to be minted at the ARN a client constructs \
             ({rule_arn}), got {minted}"
        );
        // The rule reads back carrying the pattern it was created with, so what the
        // deliveries below are decided by is the stored pattern.
        let described = events_ok(
            &router,
            &signer,
            case,
            "DescribeRule",
            &json!({ "Name": row.rule, "EventBusName": BUS }),
        )
        .await;
        assert_eq!(
            described["EventPattern"].as_str().unwrap_or_default(),
            pattern(row).to_string(),
            "case {case:?}: expected the stored pattern to be the one created, got {described}"
        );
        let targeted = events_ok(
            &router,
            &signer,
            case,
            "PutTargets",
            &json!({
                "Rule": row.rule,
                "EventBusName": BUS,
                "Targets": [{ "Id": "queue", "Arn": queue_arn }],
            }),
        )
        .await;
        assert_eq!(
            targeted["FailedEntryCount"],
            json!(0),
            "case {case:?}: PutTargets reported a failed entry: {targeted}"
        );
        queues.push((row.rule, url));
    }

    // One publication carrying one entry per value row: every entry is accepted, with an
    // `EventId` of its own.
    let entries: Vec<Value> = values
        .iter()
        .map(|row| {
            json!({
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "EventBusName": BUS,
                "Detail": detail(row).to_string(),
            })
        })
        .collect();
    let answered = events_ok(
        &router,
        &signer,
        "the publication",
        "PutEvents",
        &json!({ "Entries": entries }),
    )
    .await;
    assert_eq!(
        answered["FailedEntryCount"],
        json!(0),
        "expected every entry of the publication to be accepted, got {answered}"
    );
    let minted: Vec<&Value> = answered["Entries"]
        .as_array()
        .map(|entries| entries.iter().collect())
        .unwrap_or_default();
    assert_eq!(
        minted.len(),
        values.len(),
        "expected one result entry per published entry, got {answered}"
    );
    let mut ids: BTreeSet<String> = BTreeSet::new();
    for (row, entry) in values.iter().zip(&minted) {
        let label = row.label;
        let id = entry["EventId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {label:?}: no EventId in {entry}"))
            .to_string();
        assert!(
            ids.insert(id.clone()),
            "case {label:?}: the EventId {id} was minted for another entry too"
        );
    }

    // What each rule's queue received: exactly the cases its own operand admits.
    for ((rule, url), row) in queues.iter().zip(&operands) {
        let case = row.rule;
        assert_eq!(*rule, row.rule, "the suite paired a queue with another rule");
        let mut found = drained(&router, &signer, case, url).await;
        found.sort();
        let mut expected: Vec<String> = row.admits.iter().map(|&label| label.to_string()).collect();
        expected.sort();
        assert_eq!(
            found, expected,
            "case {case:?}: expected the target to receive exactly {expected:?}, got {found:?}"
        );
        // The absences this matrix must prove, stated as their own assertion so a
        // non-string leaf reaching an operand rule's queue says so by name.
        if row.rule != "control" {
            for label in ["number", "null", "boolean", "absent"] {
                assert!(
                    !found.iter().any(|delivered| delivered == label),
                    "case {case:?}: no text-comparing operand admits the {label:?} leaf, yet its \
                     target received it: {found:?}"
                );
            }
        }
    }

    // The verdict the reporting surface answers is the one the live rule delivered by.
    for row in &operands {
        let case = row.rule;
        let pattern = pattern(row).to_string();
        for value in &values {
            let label = value.label;
            let expected = row.admits.contains(&label);
            let answered = events_ok(
                &router,
                &signer,
                case,
                "TestEventPattern",
                &json!({ "EventPattern": &pattern, "Event": envelope(&detail(value)) }),
            )
            .await;
            assert_eq!(
                answered["Result"],
                json!(expected),
                "case {case:?} against {label:?}: expected Result {expected}, got {answered}"
            );
        }
    }
}
