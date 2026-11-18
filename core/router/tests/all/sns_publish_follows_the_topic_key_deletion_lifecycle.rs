// SPDX-License-Identifier: CC0-1.0

//! A write of a message to an Amazon SNS topic encrypted with a customer managed KMS key
//! follows that key through the WHOLE of its deletion lifecycle — scheduled for deletion,
//! the deletion canceled, enabled again — driven over the assembled `MultiServiceRouter`
//! with KMS, SNS and Amazon SQS mounted on one engine, so what a client sees is what this
//! suite asserts.
//!
//! The RULE asserted here: a topic carrying `KmsMasterKeyId` has every message published to
//! it sealed under that key, and Amazon SNS asks KMS for that data key as the message is
//! published — so the KEY's own state at that moment decides the call. A key KMS can use
//! seals the message and the call answers a message id; a key that is merely `Disabled`
//! refuses it 400 `KMSDisabled`, which a client clears by enabling the key again; a key in
//! any other state that admits no cryptographic operation refuses it 400 `KMSInvalidState`.
//! Amazon SNS words neither refusal itself: the call that failed is its own call to KMS, so
//! the message is KMS's own sentence for that key state — the key named by ARN — followed by
//! the metadata block an AWS error carries, naming KMS as the refusing service, the status it
//! refused with, KMS's OWN error code for the class and the id of the one refused call.
//!
//! Three KMS calls move one key between those states, and each is a client's own call rather
//! than anything the topic was reconfigured by: `ScheduleKeyDeletion` leaves the key
//! `PendingDeletion`, `CancelKeyDeletion` leaves it `Disabled` ("when this operation succeeds,
//! the key state of the KMS key is Disabled",
//! `smithy_query(query="doc",shape="CancelKeyDeletion")`) and `EnableKey` leaves it `Enabled`
//! again. The two refusal classes are therefore reached one after the other on ONE key, with
//! the topic never touched in between, and the second is the one a client recovers from.
//!
//! The rows differ along what the rule generalises over: the stage of the lifecycle the key is
//! in, the FORM the topic names its key by (a key id, a key ARN, an alias name), the waiting
//! period the deletion was scheduled with (omitted, and each bound of the window AWS documents
//! for it), and which operation writes the message (`Publish` and `PublishBatch`, both of
//! which write a message to the topic and both of which model the `KMS*` family).
//!
//! The SNS crate's own `tests/all/topic_encryption_key_state.rs` holds the SINGLE-SHOT rows of
//! the same rule (one publish per key condition, a disabled key and a key whose deletion is
//! scheduled each a row of its own) and `tests/all/topic_encryption_key_resolved_per_publish.rs`
//! the repetition of the disabled leg. What is added here is the LIFECYCLE: the two refusal
//! classes reached one after the other on ONE key, the `CancelKeyDeletion` transition that moves
//! a key from the permanent-until-the-state-changes class into the reversible one, the recovery
//! after `EnableKey`, and the waiting period the deletion was scheduled with. Those rows live in
//! this crate rather than beside their siblings because the owning crate still roots its
//! integration tests at a hand-declared `tests/all.rs`, which a fix may not edit; the router's
//! `tests/all/` picks a new suite up with nothing to declare.
//!
//! Each stage is applied through KMS's own client calls and read back through KMS's own
//! `DescribeKey` before the publish that stage governs — the service that owns the key state is
//! what reports it. The delivery leg reads what a subscribed queue holds by POLLING it, so no
//! outcome depends on how fast the host runs.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{element, elements, form_encode};

/// The Region every call here is signed for, and so the Region the keys and the topics belong
/// to.
const REGION: &str = "us-east-1";

/// How long an arrival is given before the suite gives up: far longer than any clock-owed
/// transition on this path, so a loaded host cannot decide the outcome.
const DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between looks.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long a queue that must have taken NOTHING is watched for: an absence has no arrival to
/// poll for, so it is re-read over a window instead of once. Many times [`POLL_INTERVAL`] and
/// well inside [`DEADLINE`], so the window stays a window on a loaded host.
const EMPTY_WINDOW: Duration = Duration::from_secs(1);

/// The metadata block an AWS error carries, which a relayed KMS refusal ends with.
const RELAY_TAIL: &str = "; Proxy: null)";

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_kms_trentservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// One SNS operation over its awsQuery wire, with whatever status it answered.
async fn sns(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-03-31");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
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
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One SNS operation that must succeed, answering its XML body.
async fn sns_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = sns(router, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SNS {action}, got {status} with {body}"
    );
    body
}

/// One awsJson call, over the target prefix the service's protocol names.
async fn json_call(
    router: &MultiServiceRouter,
    service: &str,
    target_prefix: &str,
    version: &str,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", format!("application/x-amz-json-{version}"))
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(service))
        .body(Bytes::from(
            serde_json::to_vec(request).expect("the request serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One KMS operation that must succeed.
async fn kms_ok(router: &MultiServiceRouter, case: &str, operation: &str, request: Value) -> Value {
    let (status, parsed) =
        json_call(router, "kms", "TrentService", "1.1", operation, &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from KMS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon SQS operation that must succeed.
async fn sqs_ok(router: &MultiServiceRouter, case: &str, operation: &str, request: Value) -> Value {
    let (status, parsed) = json_call(router, "sqs", "AmazonSQS", "1.0", operation, &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// A customer managed key this suite minted, in each of the forms a topic can name it by.
struct Key {
    id: String,
    arn: String,
    alias: String,
}

/// Which spelling of a key's identity the topic's `KmsMasterKeyId` names it by: the forms KMS
/// resolves a `KeyId` member in — "a key ID, key ARN, alias name, or alias ARN"
/// (`smithy_query(query="doc",shape="GenerateDataKeyRequest",member="KeyId")`).
#[derive(Debug, Clone, Copy)]
enum NamedBy {
    KeyId,
    KeyArn,
    Alias,
}

impl Key {
    /// The spelling `named_by` asks for.
    fn named(&self, named_by: NamedBy) -> &str {
        match named_by {
            NamedBy::KeyId => &self.id,
            NamedBy::KeyArn => &self.arn,
            NamedBy::Alias => &self.alias,
        }
    }
}

/// A key KMS minted for this suite, with an alias of its own pointing at it.
async fn given_key(router: &MultiServiceRouter, case: &str, alias: &str) -> Key {
    let created = kms_ok(router, case, "CreateKey", json!({})).await;
    let metadata = &created["KeyMetadata"];
    let key = Key {
        id: metadata["KeyId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no KeyMetadata.KeyId in {created}"))
            .to_string(),
        arn: metadata["Arn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no KeyMetadata.Arn in {created}"))
            .to_string(),
        alias: alias.to_string(),
    };
    kms_ok(
        router,
        case,
        "CreateAlias",
        json!({"AliasName": alias, "TargetKeyId": key.id}),
    )
    .await;
    key
}

/// A topic called `name` whose messages are encrypted under the key `master_key_id` names, in
/// whatever form that identifier spells it. Answers the topic's ARN.
async fn given_encrypted_topic(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    master_key_id: &str,
) -> String {
    let created = sns_ok(
        router,
        case,
        "CreateTopic",
        &[
            ("Name", name),
            ("Attributes.entry.1.key", "KmsMasterKeyId"),
            ("Attributes.entry.1.value", master_key_id),
        ],
    )
    .await;
    element(&created, "TopicArn")
}

/// Why a publish is refused, in both services' terms: the awsQuery code the publishing
/// operation answers, KMS's own error code for the class and the key state as KMS words it in
/// its own sentence.
#[derive(Debug, Clone, Copy)]
struct Refusal {
    /// The awsQuery error code the publish is refused with
    /// (`smithy_query(query="operation",operation="Publish")`), each modelled at HTTP 400.
    code: &'static str,
    /// KMS's own error code behind it, which the relayed metadata block reports.
    kms_code: &'static str,
    /// The key state as KMS's own sentence words it, which that sentence reads "<key arn> is
    /// <this>.".
    prose: &'static str,
}

/// One stage of a key's deletion lifecycle, as a client of KMS drives it.
#[derive(Debug, Clone, Copy)]
enum Stage {
    /// Before anything is scheduled.
    Enabled,
    /// After `ScheduleKeyDeletion`: "the key state of the KMS key changes to PendingDeletion
    /// and the key can't be used in cryptographic operations"
    /// (`smithy_query(query="doc",shape="ScheduleKeyDeletion")`).
    DeletionScheduled,
    /// After `CancelKeyDeletion`: "when this operation succeeds, the key state of the KMS key
    /// is Disabled" (`smithy_query(query="doc",shape="CancelKeyDeletion")`).
    DeletionCanceled,
    /// After `EnableKey`.
    EnabledAgain,
}

impl Stage {
    /// How this stage reads, for the assertion messages.
    fn case(self) -> &'static str {
        match self {
            Stage::Enabled => "while the key is enabled",
            Stage::DeletionScheduled => "while the key's deletion is scheduled",
            Stage::DeletionCanceled => "after the key's deletion is canceled",
            Stage::EnabledAgain => "after the key is enabled again",
        }
    }

    /// The `KeyState` a reader of the key reports in this stage, which is the state the
    /// publish is judged on.
    fn reported_state(self) -> &'static str {
        match self {
            Stage::Enabled | Stage::EnabledAgain => "Enabled",
            Stage::DeletionScheduled => "PendingDeletion",
            Stage::DeletionCanceled => "Disabled",
        }
    }

    /// How a publish under a key in this stage is refused, and `None` for a stage whose key
    /// seals the message.
    fn refusal(self) -> Option<Refusal> {
        match self {
            Stage::Enabled | Stage::EnabledAgain => None,
            Stage::DeletionScheduled => Some(Refusal {
                code: "KMSInvalidState",
                kms_code: "KMSInvalidStateException",
                prose: "pending deletion",
            }),
            Stage::DeletionCanceled => Some(Refusal {
                code: "KMSDisabled",
                kms_code: "DisabledException",
                prose: "disabled",
            }),
        }
    }
}

/// The lifecycle every case below runs, in order: one key, moved by a client's own KMS calls,
/// with the topic never touched in between.
const STAGES: &[Stage] = &[
    Stage::Enabled,
    Stage::DeletionScheduled,
    Stage::DeletionCanceled,
    Stage::EnabledAgain,
];

/// Move the key into `stage` through KMS's own client calls, and read the state back through
/// KMS's own `DescribeKey` — the service that owns the key state is what reports it.
///
/// `pending_window` is the waiting period the deletion is scheduled with, `None` leaving the
/// member out: "this value is optional. If you include a value, it must be between 7 and 30,
/// inclusive. If you do not include a value, it defaults to 30"
/// (`smithy_query(query="shape",shape="ScheduleKeyDeletionRequest")`, member
/// `PendingWindowInDays`).
async fn move_key_to(
    router: &MultiServiceRouter,
    case: &str,
    key: &Key,
    stage: Stage,
    pending_window: Option<u32>,
) {
    match stage {
        Stage::Enabled => {}
        Stage::DeletionScheduled => {
            let mut request = json!({"KeyId": key.id});
            if let Some(days) = pending_window {
                request["PendingWindowInDays"] = json!(days);
            }
            kms_ok(router, case, "ScheduleKeyDeletion", request).await;
        }
        Stage::DeletionCanceled => {
            kms_ok(router, case, "CancelKeyDeletion", json!({"KeyId": key.id})).await;
        }
        Stage::EnabledAgain => {
            kms_ok(router, case, "EnableKey", json!({"KeyId": key.id})).await;
        }
    }
    let described = kms_ok(router, case, "DescribeKey", json!({"KeyId": key.id})).await;
    assert_eq!(
        described["KeyMetadata"]["KeyState"].as_str(),
        Some(stage.reported_state()),
        "case {case:?}: expected KMS to report KeyState {:?}, got {described}",
        stage.reported_state()
    );
}

/// What one write of `body` to `topic` through the operation `writer` names answered: its HTTP
/// status, the awsQuery error code (`""` on a success envelope), the message ids it reports and
/// the refusal sentence it carries (`""` on a success envelope).
async fn write_message(
    router: &MultiServiceRouter,
    writer: &str,
    topic: &str,
    body: &str,
) -> (u16, String, Vec<String>, String) {
    let members: Vec<(&str, &str)> = match writer {
        "Publish" => vec![("TopicArn", topic), ("Message", body)],
        "PublishBatch" => vec![
            ("TopicArn", topic),
            ("PublishBatchRequestEntries.member.1.Id", "only"),
            ("PublishBatchRequestEntries.member.1.Message", body),
        ],
        other => panic!("no writer called {other:?}"),
    };
    let (status, text) = sns(router, writer, &members).await;
    let (code, sentence) = if status == 200 {
        (String::new(), String::new())
    } else {
        (
            elements(&text, "Code").into_iter().next().unwrap_or_default(),
            elements(&text, "Message")
                .into_iter()
                .next()
                .unwrap_or_default(),
        )
    };
    (status, code, elements(&text, "MessageId"), sentence)
}

/// The id of the ONE refused KMS call the relayed metadata block names, which is how an
/// operator correlates the refusal with that call.
fn relayed_request_id(case: &str, message: &str) -> String {
    let marker = "Request ID: ";
    let at = message
        .find(marker)
        .unwrap_or_else(|| panic!("case {case:?}: no {marker:?} in the relayed message {message:?}"))
        + marker.len();
    let rest = &message[at..];
    rest[..rest
        .find(';')
        .unwrap_or_else(|| panic!("case {case:?}: no terminated request id in {message:?}"))]
        .to_string()
}

/// Assert that `message` is KMS's own sentence for the key state the publish was refused for,
/// relayed with the metadata block of the KMS call that failed: the key named by ARN, the
/// refusing service, the status it refused with and KMS's OWN error code for the class.
///
/// Answers the id of the refused call, so a caller can assert it names ONE call.
fn assert_relayed(case: &str, message: &str, key_arn: &str, refusal: Refusal) -> String {
    let sentence = format!("{key_arn} is {}.", refusal.prose);
    assert!(
        message.starts_with(&sentence),
        "case {case:?}: expected the refusal to open with KMS's own sentence {sentence:?}, got \
         {message:?}"
    );
    let metadata = format!(
        " (Service: AWSKMS; Status Code: 400; Error Code: {}; Request ID: ",
        refusal.kms_code
    );
    assert!(
        message[sentence.len()..].starts_with(&metadata),
        "case {case:?}: expected the relayed metadata block {metadata:?} after the sentence, got \
         {message:?}"
    );
    assert!(
        message.ends_with(RELAY_TAIL),
        "case {case:?}: expected the relayed metadata block to end with {RELAY_TAIL:?}, got \
         {message:?}"
    );
    let request_id = relayed_request_id(case, message);
    assert!(
        !request_id.is_empty(),
        "case {case:?}: the relayed block must name the refused KMS call, got {message:?}"
    );
    request_id
}

#[tokio::test]
async fn a_publish_follows_the_topics_key_through_its_whole_deletion_lifecycle() {
    let router = build_router();

    // (the form the topic names its key by, the waiting period the deletion is scheduled with)
    // — the window AWS documents for `PendingWindowInDays` is 7 to 30 inclusive, and the
    // member is optional, so the rows carry each published bound and the omission.
    let namings: &[(NamedBy, Option<u32>)] = &[
        (NamedBy::KeyId, None),
        (NamedBy::KeyArn, Some(7)),
        (NamedBy::Alias, Some(30)),
    ];

    // Both operations that write a message to a topic: each resolves the topic's key and asks
    // KMS for the data key before it accepts anything.
    let writers: &[&str] = &["Publish", "PublishBatch"];

    for (index, (named_by, pending_window)) in namings.iter().enumerate() {
        for writer in writers {
            let case = format!("{writer} on a topic naming its key by {named_by:?}");
            let key = given_key(
                &router,
                &case,
                &format!("alias/lifecycle-{index}-{}", writer.to_lowercase()),
            )
            .await;
            let topic = given_encrypted_topic(
                &router,
                &case,
                &format!("sse-kms-lifecycle-{index}-{}", writer.to_lowercase()),
                key.named(*named_by),
            )
            .await;

            // Every request id a refusal of this case named: each refusal relays ONE KMS call,
            // so no two of them may carry the same id.
            let mut refused_calls: Vec<String> = Vec::new();

            for stage in STAGES {
                let case = format!("{case} {}", stage.case());
                move_key_to(&router, &case, &key, *stage, *pending_window).await;
                let (status, code, ids, sentence) =
                    write_message(&router, writer, &topic, "sealed under the topic's key").await;
                match stage.refusal() {
                    None => {
                        assert_eq!(
                            (status, code.as_str()),
                            (200, ""),
                            "case {case:?}: a write under a usable key must be accepted, got \
                             {status} / {code} / {sentence:?}"
                        );
                        assert_eq!(
                            ids.len(),
                            1,
                            "case {case:?}: an accepted write must report one MessageId, got \
                             {ids:?}"
                        );
                    }
                    Some(refusal) => {
                        assert_eq!(
                            (status, code.as_str()),
                            (400, refusal.code),
                            "case {case:?}: expected the write refused {} at HTTP 400, got \
                             {status} / {code} / {sentence:?}",
                            refusal.code
                        );
                        assert!(
                            ids.is_empty(),
                            "case {case:?}: a refused write seals nothing, so it must report no \
                             MessageId, got {ids:?}"
                        );
                        let request_id = assert_relayed(&case, &sentence, &key.arn, refusal);
                        assert!(
                            !refused_calls.contains(&request_id),
                            "case {case:?}: each refusal relays one KMS call of its own, so the \
                             id {request_id:?} must not be reported twice, got {refused_calls:?}"
                        );
                        refused_calls.push(request_id);
                    }
                }
            }
        }
    }
}

/// A queue called `name`, carrying the queue policy an SNS subscriber needs: its url and the
/// ARN a subscription names it by.
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let created = sqs_ok(router, case, "CreateQueue", json!({"QueueName": name})).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {created}"))
        .to_string();
    let reported = sqs_ok(
        router,
        case,
        "GetQueueAttributes",
        json!({"QueueUrl": url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    let arn = reported["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {reported}"))
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowSNSDelivery",
            "Effect": "Allow",
            "Principal": {"Service": "sns.amazonaws.com"},
            "Action": "sqs:SendMessage",
            "Resource": arn,
        }],
    })
    .to_string();
    sqs_ok(
        router,
        case,
        "SetQueueAttributes",
        json!({"QueueUrl": url, "Attributes": {"Policy": policy}}),
    )
    .await;
    (url, arn)
}

/// One look at the queue at `url`: the bodies it handed over, which may be none, each taken off
/// the queue.
async fn taken_off(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let answer = sqs_ok(
        router,
        case,
        "ReceiveMessage",
        json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    let messages = answer["Messages"].as_array().cloned().unwrap_or_default();
    let mut bodies = Vec::new();
    for message in messages {
        bodies.push(message["Body"].as_str().unwrap_or_default().to_string());
        let receipt = message["ReceiptHandle"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no ReceiptHandle in {message}"));
        sqs_ok(
            router,
            case,
            "DeleteMessage",
            json!({"QueueUrl": url, "ReceiptHandle": receipt}),
        )
        .await;
    }
    bodies
}

/// POLL the queue at `url` until it has handed over `expected` messages, answering their bodies
/// in the order they were received. The wait is for the CONDITION rather than for a time.
async fn received(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    expected: usize,
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    let mut held: Vec<String> = Vec::new();
    loop {
        held.extend(taken_off(router, case, url).await);
        if held.len() >= expected {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {expected} message(s) on the queue, got {held:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The bodies that ever turn up on the queue at `url` while it is watched over
/// [`EMPTY_WINDOW`]: what a queue that must hold nothing is asserted empty by. An absence has
/// no arrival to poll for, so it is re-read over a window instead of once — a delivery that
/// leaked through on a later leg is still seen.
///
/// The window is abandoned the moment a body turns up, which is what the caller asserts on: the
/// watch is what shows an absence, so it has nothing left to wait for once there is an arrival.
async fn nothing_arrives(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let until = Instant::now() + EMPTY_WINDOW;
    let mut seen: Vec<String> = Vec::new();
    loop {
        seen.extend(taken_off(router, case, url).await);
        if !seen.is_empty() || Instant::now() >= until {
            return seen;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A refused publish seals nothing, so it delivers nothing: what a subscriber holds is exactly
/// the messages published while the topic's key could be used.
#[tokio::test]
async fn a_publish_refused_for_the_key_state_delivers_nothing_to_a_subscriber() {
    let router = build_router();
    let case = "a queue subscribed to an encrypted topic";
    let key = given_key(&router, case, "alias/lifecycle-delivery").await;
    let topic = given_encrypted_topic(&router, case, "sse-kms-delivery", &key.arn).await;
    let (url, queue_arn) = given_queue(&router, case, "sse-kms-subscriber").await;
    let subscription = sns_ok(
        &router,
        case,
        "Subscribe",
        &[
            ("TopicArn", topic.as_str()),
            ("Protocol", "sqs"),
            ("Endpoint", queue_arn.as_str()),
            ("Attributes.entry.1.key", "RawMessageDelivery"),
            ("Attributes.entry.1.value", "true"),
        ],
    )
    .await;
    assert!(
        element(&subscription, "SubscriptionArn").contains(':'),
        "case {case:?}: an SQS subscription is confirmed at once, got {subscription}"
    );

    for stage in STAGES {
        let case = format!("{case} {}", stage.case());
        move_key_to(&router, &case, &key, *stage, Some(7)).await;
        let body = stage.case();
        let (status, code, ids, sentence) = write_message(&router, "Publish", &topic, body).await;
        match stage.refusal() {
            None => {
                assert_eq!(
                    (status, code.as_str()),
                    (200, ""),
                    "case {case:?}: the publish must be accepted, got {status} / {code} / \
                     {sentence:?}"
                );
                assert_eq!(
                    ids.len(),
                    1,
                    "case {case:?}: an accepted publish must report one MessageId, got {ids:?}"
                );
                let held = received(&router, &case, &url, 1).await;
                assert_eq!(
                    held,
                    vec![body.to_string()],
                    "case {case:?}: the subscriber must receive the message sealed under the \
                     usable key, got {held:?}"
                );
            }
            Some(refusal) => {
                assert_eq!(
                    (status, code.as_str()),
                    (400, refusal.code),
                    "case {case:?}: expected the publish refused {}, got {status} / {code} / \
                     {sentence:?}",
                    refusal.code
                );
                let delivered = nothing_arrives(&router, &case, &url).await;
                assert!(
                    delivered.is_empty(),
                    "case {case:?}: a refused publish seals and delivers nothing, so the \
                     subscriber must stay empty, got {delivered:?}"
                );
            }
        }
    }

    // Nothing of the refused legs turns up late either: what the subscriber held is exactly
    // what was published while the key could be used.
    let trailing = nothing_arrives(&router, case, &url).await;
    assert!(
        trailing.is_empty(),
        "case {case:?}: the refused publishes must deliver nothing at all, got {trailing:?}"
    );
}
