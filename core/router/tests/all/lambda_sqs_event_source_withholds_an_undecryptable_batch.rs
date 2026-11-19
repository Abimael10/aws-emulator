// SPDX-License-Identifier: CC0-1.0

//! What an Amazon SQS event source mapping does with a batch it took out of a queue encrypted
//! with a CUSTOMER MANAGED key while its function's execution role may not open that key —
//! across the four services that own the halves of it: KMS mints the key and decides what the
//! authorities in force let a role do with it, Amazon SQS holds the queue and seals every
//! message under that key, IAM answers for the execution role, CloudFormation archives the
//! inline handler into the function's deployment package, and Lambda polls the queue and RUNS
//! that handler.
//!
//! The rule pinned here: **an enabled event source mapping READS its source queue with no
//! client action at all, so a message sent to that queue is taken in flight by the mapping —
//! invisible to every other consumer while the mapping holds it, and still on the queue; and a
//! batch out of a queue whose key the function's execution role may not use for the decrypt a
//! hand-over performs is HELD rather than consumed: nothing is handed to the function and
//! nothing is deleted, so the message becomes visible again when the hold lapses and is handed
//! over afresh, and the moment the authorities in force let that role decrypt, the SAME message
//! reaches the handler — under the higher delivery count its withheld deliveries left behind.**
//!
//! Four crates have to agree for that to hold, so the suite lives here rather than in any one
//! of them. Every observation is one a CLIENT makes: the queue's own
//! `ApproximateNumberOfMessages`/`ApproximateNumberOfMessagesNotVisible`, the marker queue the
//! handler forwards what it was handed to, and the mapping's own `LastProcessingResult`.
//!
//! The rows differ along what the rule generalises over — WHICH authority comes to permit the
//! decrypt (the role's own identity policy, a KMS grant issued to the role, or the key's own
//! policy naming the role: the three documents `authorize_use` weighs for a role, each of which
//! alone is enough) and which spelling the queue names its key by (a key id, a key ARN, an
//! alias — the forms a `KeyId` member resolves in, per
//! `smithy_query(query="doc",shape="GenerateDataKeyRequest",member="KeyId")`). Every row starts
//! from the same withheld state: the key policy `CreateKey` writes, which delegates to the
//! account, and an execution role carrying the queue reads but no `kms:Decrypt`.
//!
//! Nothing here is bounded by how fast the host runs: every arrival is waited for by POLLING
//! the condition until a generous deadline passes, and the withhold — an absence, which has no
//! arrival to poll for — is read over a window longer than one hold of the batch, so the queue
//! has handed it over again at least once inside it.
//!
//! The owning crate's own
//! `services/service-com-amazonaws-lambda-awsgirapiservice/tests/all/sqs_event_source_encrypted_queue.rs`
//! reads the same withhold-then-grant rule with the passes DRIVEN by the caller; what only this
//! suite reaches is the mapping polling of its own accord on the assembled emulator, and with it
//! the queue depths a client sees while the batch is held, the delivery count the withheld
//! deliveries leave behind, and the shape of the record the handler is finally given. Keep the
//! two in step.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for — the one the shared stack reader
/// ([`wait_for_stack_status`]) signs for.
const REGION: &str = "us-east-1";

/// The `StackStatus` each row's consumer stack has to reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// Where `CreateEventSourceMapping` is bound (`POST /2015-03-31/event-source-mappings`).
const MAPPINGS_URI: &str = "/2015-03-31/event-source-mappings";

/// The mapped function's own `Timeout`, in seconds: the longest one attempt at a batch may take.
const FUNCTION_TIMEOUT_SECONDS: i64 = 3;

/// How long the source queue hides a message it hands a consumer, in seconds, which is what one
/// withheld delivery holds the message for before it comes back.
///
/// Lambda accepts a queue mapping only while the queue's `VisibilityTimeout` is at least the
/// function's `Timeout`; this is comfortably ABOVE that floor rather than exactly at it, so the
/// rows read the withhold rule and not the boundary of that admission check.
const HOLD_SECONDS: i64 = 2 * FUNCTION_TIMEOUT_SECONDS;

/// The `State` a mapping reports once it polls its event source.
const STATE_ENABLED: &str = "Enabled";

/// How long an observation of work the CLOCK owes — the stack coming up, the function going
/// `Active`, the mapping settling, the mapping taking the message, the delivery after the grant
/// — waits before giving up, and how long between two looks. Deliberately far longer than any
/// of that work takes, so a row fails only when the work did not happen at all and never
/// because the host it ran on was busy.
const BUDGET: Duration = Duration::from_secs(180);
/// See [`BUDGET`].
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// How long between two looks while the withhold is being watched ([`WITHHELD_WINDOW`]): a
/// coarser cadence than [`LOOK_AGAIN`], because nothing is being waited FOR there — the window
/// re-reads an absence, so looking many times over it adds nothing a few looks do not say.
const LOOK_AGAIN_WHILE_WITHHELD: Duration = Duration::from_millis(250);

/// How long the withheld message is watched for: an absence has no arrival to poll for, so it
/// is re-read over a window instead of once. Longer than ONE hold of the batch
/// ([`HOLD_SECONDS`]), so the queue has made the message visible again and the mapping has taken
/// it afresh inside the window — which is what tells a mapping that HOLDS the batch from one
/// that quietly consumed or dropped it.
const WITHHELD_WINDOW: Duration = Duration::from_secs(8);

/// `eventSource` every record of a queue-sourced event carries.
const SQS_EVENT_SOURCE: &str = "aws:sqs";

/// The handler every row deploys inline: it forwards each record it was handed to the marker
/// queue its environment names — addressed by NAME through the service's own `GetQueueUrl`, so
/// the source carries nothing the deployment had to compute — and then succeeds, so a batch
/// that reached it is also a batch the mapping may delete.
const REPORTING_SOURCE: &str = r#"
import json
import os

import boto3


def handler(event, context):
    client = boto3.client("sqs")
    url = client.get_queue_url(QueueName=os.environ["MARKER_QUEUE"])["QueueUrl"]
    for record in event["Records"]:
        client.send_message(QueueUrl=url, MessageBody=json.dumps(record))
    return {"handled": len(event["Records"])}
"#;

/// The emulator as it is ASSEMBLED, on one engine: the handler's own SDK call is dispatched
/// back into this same router, so the marker queue it writes is the queue this suite reads.
fn emulator() -> MultiServiceRouter {
    let engine = std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: every call carries a credential scope so
    // the router can name the service it belongs to.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request. The signature itself is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire, with whatever status it answered.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> (u16, Bytes) {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed request"))
        .await;
    let (parts, body) = response.into_parts();
    (parts.status.as_u16(), body)
}

/// One awsJson call of `service`, over the target prefix its protocol names.
async fn json_call(
    router: &MultiServiceRouter,
    service: &str,
    target_prefix: &str,
    version: &str,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let (status, answered) = dispatch(
        router,
        "POST",
        &format!("{service}.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", &format!("application/x-amz-json-{version}")),
            ("x-amz-target", &format!("{target_prefix}.{operation}")),
            ("authorization", &authorization(service)),
        ],
        Bytes::from(request.to_string()),
    )
    .await;
    (status, parsed(&answered))
}

/// What a service answered, as JSON where it is JSON and as its own text where it is not.
fn parsed(answered: &Bytes) -> Value {
    if answered.is_empty() {
        return Value::Null;
    }
    serde_json::from_slice(answered)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(answered).to_string()))
}

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, request: &Value) -> Value {
    let (status, answered) = json_call(router, "sqs", "AmazonSQS", "1.0", operation, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SQS {operation}, got {status} with {answered}"
    );
    answered
}

/// One KMS call (`awsJson1_1`, `TrentService.<Operation>`) that must answer 200.
async fn kms(router: &MultiServiceRouter, case: &str, operation: &str, request: &Value) -> Value {
    let (status, answered) = json_call(router, "kms", "TrentService", "1.1", operation, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from KMS {operation}, got {status} with {answered}"
    );
    answered
}

/// One AWS Lambda call over its `restJson1` wire, with whatever status it answered.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let (status, answered) = dispatch(
        router,
        method,
        &format!("lambda.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("lambda")),
        ],
        Bytes::from(payload),
    )
    .await;
    (status, parsed(&answered))
}

/// One awsQuery call of `signing_name` at `host`, as its status and XML body.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    host: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let (status, answered) = dispatch(
        router,
        "POST",
        host,
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization(signing_name)),
        ],
        Bytes::from(body),
    )
    .await;
    (status, String::from_utf8_lossy(&answered).to_string())
}

/// One IAM call that must answer 200.
async fn iam(router: &MultiServiceRouter, case: &str, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = query(
        router,
        "iam",
        "iam.amazonaws.com",
        "2010-05-08",
        action,
        members,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from IAM {action}, got {status} with {body}"
    );
    body
}

/// One CloudFormation call that must answer 200.
async fn cloudformation(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(
        router,
        "cloudformation",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        "2010-05-15",
        action,
        members,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudFormation {action}, got {status} with {body}"
    );
    body
}

/// One customer managed key this suite minted, in each of the forms a queue can name it by.
struct Key {
    id: String,
    arn: String,
    alias: String,
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

    /// The account of the key's own ARN: the identity a key policy's administrative statement
    /// names.
    fn account(&self) -> &str {
        self.arn
            .split(':')
            .nth(4)
            .unwrap_or_else(|| panic!("no account in {}", self.arn))
    }
}

/// Which spelling of a key's identity the queue's `KmsMasterKeyId` names it by: the forms KMS
/// resolves a `KeyId` member in — "a key ID, key ARN, alias name, or alias ARN"
/// (`smithy_query(query="doc",shape="GenerateDataKeyRequest",member="KeyId")`).
#[derive(Debug, Clone, Copy)]
enum NamedBy {
    KeyId,
    KeyArn,
    Alias,
}

/// Which authority comes to permit the poller's decrypt: the three documents a use of a key AS
/// a role is weighed against, each of which alone is enough.
#[derive(Debug, Clone, Copy)]
enum Granted {
    /// The role's OWN identity policy gains `kms:Decrypt`, which the key policy's delegation to
    /// the account leaves as the missing permission.
    IdentityPolicy,
    /// A KMS GRANT issued to the role for the `Decrypt` operation, which is permission of its
    /// own — the role's identity policy still carries no `kms:Decrypt`.
    Grant,
    /// The KEY's own policy comes to name the role as a principal allowed `kms:Decrypt`, which
    /// grants outright.
    KeyPolicy,
}

/// The statement every key policy here carries: the one `CreateKey` writes by default, which
/// "gives the Amazon Web Services account that owns the KMS key permission to use it in all
/// cryptographic operations" — and so DELEGATES every principal's use to that account's own
/// identity policies, naming no role itself.
fn administers(account: &str) -> Value {
    json!({
        "Sid": "AccountAdministers",
        "Effect": "Allow",
        "Principal": {"AWS": format!("arn:aws:iam::{account}:root")},
        "Action": "kms:*",
        "Resource": "*",
    })
}

/// A key policy document: the administrative statement, then whatever the row adds.
fn key_policy(account: &str, statements: Vec<Value>) -> Value {
    let mut all = vec![administers(account)];
    all.extend(statements);
    json!({"Version": "2012-10-17", "Statement": all})
}

/// A customer managed key with the alias `alias`, carrying the policy `CreateKey` writes: the
/// three spellings of its identity.
async fn given_key(router: &MultiServiceRouter, case: &str, alias: &str) -> Key {
    let created = kms(router, case, "CreateKey", &json!({})).await;
    let id = created["KeyMetadata"]["KeyId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no KeyMetadata.KeyId in {created}"))
        .to_string();
    let arn = created["KeyMetadata"]["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no KeyMetadata.Arn in {created}"))
        .to_string();
    kms(
        router,
        case,
        "CreateAlias",
        &json!({"AliasName": alias, "TargetKeyId": id}),
    )
    .await;
    Key {
        id,
        arn,
        alias: alias.to_string(),
    }
}

/// The row's execution role: trusted by Lambda, granting the reads a queue mapping's poller
/// makes on its source and the writes the handler makes to the marker queue — and NO use of any
/// KMS key, which is the withheld state every row starts from. Answers the ARN IAM reports.
async fn given_role(router: &MultiServiceRouter, case: &str, role_name: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = iam(
        router,
        case,
        "CreateRole",
        &[
            ("RoleName", role_name),
            ("AssumeRolePolicyDocument", &trust),
        ],
    )
    .await;
    let arn = element(&created, "Arn");
    let permissions = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": [
                "sqs:ReceiveMessage",
                "sqs:DeleteMessage",
                "sqs:GetQueueAttributes",
                "sqs:GetQueueUrl",
                "sqs:SendMessage",
            ],
            // The row's queues are created after the role, so the grant names the actions the
            // poller and the handler make rather than each queue.
            "Resource": "*",
        }],
    })
    .to_string();
    iam(
        router,
        case,
        "PutRolePolicy",
        &[
            ("RoleName", role_name),
            ("PolicyName", "consume-and-report"),
            ("PolicyDocument", &permissions),
        ],
    )
    .await;
    arn
}

/// A queue called `name` hiding a delivered message for [`HOLD_SECONDS`], encrypted with the key
/// `master_key_id` names when it names one at all; answers its url and the ARN Amazon SQS itself
/// reports for it.
async fn given_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    master_key_id: Option<&str>,
) -> (String, String) {
    let mut attributes = json!({ "VisibilityTimeout": HOLD_SECONDS.to_string() });
    if let Some(master_key_id) = master_key_id {
        attributes["KmsMasterKeyId"] = json!(master_key_id);
    }
    let created = sqs(
        router,
        case,
        "CreateQueue",
        &json!({ "QueueName": name, "Attributes": attributes }),
    )
    .await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: CreateQueue {name:?} reports no QueueUrl, got {created}")
        })
        .to_string();
    let read = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = read["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the queue {name:?} reports no QueueArn"))
        .to_string();
    (url, arn)
}

/// Deploy the row's consumer: a stack holding the one function, whose inline code
/// CloudFormation archives into the deployment package Lambda runs.
async fn given_function(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    function: &str,
    marker_queue: &str,
    role_arn: &str,
) {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Consumer": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT_SECONDS,
                    "Environment": { "Variables": { "MARKER_QUEUE": marker_queue } },
                    "Code": { "ZipFile": REPORTING_SOURCE },
                },
            },
        },
    })
    .to_string();
    cloudformation(
        router,
        case,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &template)],
    )
    .await;
    let (reached, described) = wait_for_stack_status(router, stack, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "case {case:?}: expected the consumer's stack to reach {CREATE_COMPLETE}, got {described}"
    );
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{function}/configuration"),
            None,
        )
        .await;
        if status == 200 && configuration["State"].as_str() == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: Lambda never reported {function} Active, last read {status} \
             {configuration}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The enabled mapping on the row's source queue, one message per batch; answers its `UUID`
/// once Lambda reports it `Enabled`.
async fn given_mapping(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    queue_arn: &str,
) -> String {
    let (status, created) = lambda(
        router,
        "POST",
        MAPPINGS_URI,
        Some(&json!({
            "FunctionName": function,
            "EventSourceArn": queue_arn,
            "BatchSize": 1,
            "Enabled": true,
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "case {case:?}: expected HTTP 202 from CreateEventSourceMapping, got {status} with \
         {created}"
    );
    let uuid = created["UUID"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the created mapping reports no UUID, got {created}")
        })
        .to_string();
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, read) = lambda(router, "GET", &format!("{MAPPINGS_URI}/{uuid}"), None).await;
        if status == 200 && read["State"].as_str() == Some(STATE_ENABLED) {
            return uuid;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the mapping {uuid} never reported {STATE_ENABLED}, last read \
             {status} {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// What `GetEventSourceMapping` reports as the result of the mapping's last processing
/// attempt, or `None` while it has made none.
async fn last_processing_result(
    router: &MultiServiceRouter,
    case: &str,
    uuid: &str,
) -> Option<String> {
    let (status, read) = lambda(router, "GET", &format!("{MAPPINGS_URI}/{uuid}"), None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetEventSourceMapping {uuid} answered {status} {read}"
    );
    read["LastProcessingResult"].as_str().map(str::to_string)
}

/// The queue's own depth as a client reads it: the messages waiting to be handed over, and the
/// ones a consumer is holding — `ApproximateNumberOfMessages` and
/// `ApproximateNumberOfMessagesNotVisible`.
async fn depth(router: &MultiServiceRouter, case: &str, url: &str) -> (u64, u64) {
    let read = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({
            "QueueUrl": url,
            "AttributeNames": [
                "ApproximateNumberOfMessages",
                "ApproximateNumberOfMessagesNotVisible",
            ],
        }),
    )
    .await;
    let count = |name: &str| {
        read["Attributes"][name]
            .as_str()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or_else(|| panic!("case {case:?}: the queue reports no {name}, got {read}"))
    };
    (
        count("ApproximateNumberOfMessages"),
        count("ApproximateNumberOfMessagesNotVisible"),
    )
}

/// Poll the queue until it reports a message IN FLIGHT — invisible, because the mapping is
/// holding it — and answer the depth that look read.
async fn await_in_flight(router: &MultiServiceRouter, case: &str, url: &str) -> (u64, u64) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let read = depth(router, case, url).await;
        if read.1 >= 1 {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: an enabled mapping must take the message in flight, {url} reported \
             {read:?} (visible, not visible) at its last look"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Poll the queue until it holds nothing at all: neither waiting nor in flight.
async fn await_drained(router: &MultiServiceRouter, case: &str, url: &str) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let read = depth(router, case, url).await;
        if read == (0, 0) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: a processed message leaves its queue, {url} still reported {read:?} \
             (visible, not visible)"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Take whatever is waiting on the marker queue `url` addresses, deleting each message so a
/// later look finds only later markers, and answer the records the handler forwarded.
async fn take_markers(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut records = Vec::with_capacity(messages.len());
    for message in &messages {
        let text = message["Body"].as_str().unwrap_or_default().to_string();
        sqs(
            router,
            case,
            "DeleteMessage",
            &json!({
                "QueueUrl": url,
                "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
            }),
        )
        .await;
        records.push(serde_json::from_str(&text).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: expected a forwarded record as a JSON document, got {text:?} \
                 ({error})"
            )
        }));
    }
    records
}

/// Poll the marker queue until the handler has reported a record, and answer the first one.
async fn await_record(router: &MultiServiceRouter, case: &str, url: &str) -> Value {
    let deadline = Instant::now() + BUDGET;
    loop {
        if let Some(record) = take_markers(router, case, url).await.into_iter().next() {
            return record;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: once the role may decrypt, the mapping must hand the message to its \
             function; nothing reached {url}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The `ApproximateReceiveCount` a forwarded record carries, as a number — 0 when it carries
/// none, which fails the assertion that reads it.
fn receive_count(record: &Value) -> u64 {
    record["attributes"]["ApproximateReceiveCount"]
        .as_str()
        .and_then(|count| count.parse().ok())
        .unwrap_or_default()
}

/// Let the authority this row varies permit the poller's `kms:Decrypt` on the queue's key.
async fn permit_decrypt(
    router: &MultiServiceRouter,
    case: &str,
    granted: Granted,
    key: &Key,
    role_name: &str,
    role_arn: &str,
) {
    match granted {
        Granted::IdentityPolicy => {
            let document = json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Action": "kms:Decrypt",
                    "Resource": key.arn,
                }],
            })
            .to_string();
            iam(
                router,
                case,
                "PutRolePolicy",
                &[
                    ("RoleName", role_name),
                    ("PolicyName", "open-the-source-queue"),
                    ("PolicyDocument", &document),
                ],
            )
            .await;
        }
        Granted::Grant => {
            kms(
                router,
                case,
                "CreateGrant",
                &json!({
                    "KeyId": key.arn,
                    "GranteePrincipal": role_arn,
                    "Operations": ["Decrypt"],
                    "Name": "poller-opens-the-source-queue",
                }),
            )
            .await;
        }
        Granted::KeyPolicy => {
            let document = key_policy(
                key.account(),
                vec![json!({
                    "Sid": "ExecutionRoleDecrypts",
                    "Effect": "Allow",
                    "Principal": {"AWS": role_arn},
                    "Action": "kms:Decrypt",
                    "Resource": "*",
                })],
            )
            .to_string();
            kms(
                router,
                case,
                "PutKeyPolicy",
                &json!({
                    "KeyId": key.arn,
                    "PolicyName": "default",
                    "Policy": document,
                }),
            )
            .await;
        }
    }
}

/// One row: the slug its resources are named from, the body it sends, which spelling the queue
/// names its key by, and which authority comes to permit the decrypt.
struct Row {
    case: &'static str,
    slug: &'static str,
    body: &'static str,
    named_by: NamedBy,
    granted: Granted,
}

/// The rows: one per authority that can come to permit a role's use of a key, each paired with
/// a different spelling of the key on the queue, so the rule is read over both dimensions it
/// generalises over.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the role's own identity policy comes to allow the decrypt, on a queue keyed \
                   by key id",
            slug: "esm-kms-identity",
            body: "{\"order\": \"A-1\"}",
            named_by: NamedBy::KeyId,
            granted: Granted::IdentityPolicy,
        },
        Row {
            case: "a KMS grant issued to the role permits the decrypt, on a queue keyed by key \
                   ARN",
            slug: "esm-kms-grant",
            body: "a body that is not a JSON document",
            named_by: NamedBy::KeyArn,
            granted: Granted::Grant,
        },
        Row {
            case: "the key's own policy comes to name the role, on a queue keyed by alias",
            slug: "esm-kms-key-policy",
            body: "{\"order\": \"C-3\", \"items\": 2}",
            named_by: NamedBy::Alias,
            granted: Granted::KeyPolicy,
        },
    ]
}

/// RULE: an enabled SQS event source mapping takes a sent message in flight with no client
/// action at all; a batch out of an SSE-KMS queue whose key the execution role may not use is
/// held rather than consumed — nothing invoked, nothing deleted, the message handed over again
/// when the hold lapses — and once an authority permits that role's `kms:Decrypt` the same
/// message reaches the handler, under the higher delivery count the withheld deliveries left.
#[tokio::test]
async fn an_undecryptable_batch_is_held_in_flight_until_the_role_may_decrypt() {
    let router = emulator();

    for row in rows() {
        let case = row.case;
        let slug = row.slug;
        let role_name = format!("{slug}-consumer");
        let role_arn = given_role(&router, case, &role_name).await;
        let key = given_key(&router, case, &format!("alias/{slug}")).await;

        let marker_name = format!("{slug}-marker");
        let (marker_url, _) = given_queue(&router, case, &marker_name, None).await;
        let (source_url, source_arn) = given_queue(
            &router,
            case,
            &format!("{slug}-source"),
            Some(key.named(row.named_by)),
        )
        .await;

        let function = format!("{slug}-function");
        given_function(
            &router,
            case,
            &format!("{slug}-stack"),
            &function,
            &marker_name,
            &role_arn,
        )
        .await;
        let uuid = given_mapping(&router, case, &function, &source_arn).await;

        let sent = sqs(
            &router,
            case,
            "SendMessage",
            &json!({ "QueueUrl": source_url, "MessageBody": row.body }),
        )
        .await;
        let message_id = sent["MessageId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: SendMessage reports no MessageId, got {sent}"))
            .to_string();

        // The mapping POLLS: the message it took is invisible to every other consumer while it
        // holds it, which is what the client reads as in flight.
        let in_flight = await_in_flight(&router, case, &source_url).await;
        assert_eq!(
            in_flight,
            (0, 1),
            "case {case:?}: the message the mapping holds must be in flight and still on the \
             queue, got {in_flight:?} (visible, not visible)"
        );

        // While the role may not open the queue's key the batch is HELD: nothing reaches the
        // handler and nothing is deleted, read over a window longer than one hold so the queue
        // has handed the message over again inside it.
        let withheld_until = Instant::now() + WITHHELD_WINDOW;
        while Instant::now() < withheld_until {
            let forwarded = take_markers(&router, case, &marker_url).await;
            assert!(
                forwarded.is_empty(),
                "case {case:?}: a batch the role cannot decrypt must reach no handler, it \
                 forwarded {forwarded:?}"
            );
            let read = depth(&router, case, &source_url).await;
            assert_eq!(
                read.0 + read.1,
                1,
                "case {case:?}: a withheld message is neither consumed nor dropped, the queue \
                 reported {read:?} (visible, not visible)"
            );
            tokio::time::sleep(LOOK_AGAIN_WHILE_WITHHELD).await;
        }
        assert_eq!(
            last_processing_result(&router, case, &uuid).await,
            None,
            "case {case:?}: no batch reached the function, so the mapping has no processing \
             attempt to report"
        );

        // The authority this row varies now permits the decrypt, and the SAME message is handed
        // to the handler — under the delivery count its withheld deliveries left behind.
        permit_decrypt(&router, case, row.granted, &key, &role_name, &role_arn).await;
        let record = await_record(&router, case, &marker_url).await;
        assert_eq!(
            record["messageId"].as_str(),
            Some(message_id.as_str()),
            "case {case:?}: the delivered record must be the message that was withheld, got \
             {record}"
        );
        assert_eq!(
            record["body"].as_str(),
            Some(row.body),
            "case {case:?}: the record must carry the body as the queue holds it, got {record}"
        );
        assert_eq!(
            record["eventSource"].as_str(),
            Some(SQS_EVENT_SOURCE),
            "case {case:?}: the record must name its event source, got {record}"
        );
        assert_eq!(
            record["eventSourceARN"].as_str(),
            Some(source_arn.as_str()),
            "case {case:?}: the record must name the queue it came from, got {record}"
        );
        let deliveries = receive_count(&record);
        assert!(
            deliveries >= 2,
            "case {case:?}: the withheld deliveries count against the message, so the delivered \
             record must carry at least 2 deliveries, got {deliveries}"
        );

        // A batch the function processed leaves the queue, and the mapping reports the attempt.
        await_drained(&router, case, &source_url).await;
        assert_eq!(
            last_processing_result(&router, case, &uuid).await,
            // "The result of the event source mapping's last processing attempt": what AWS
            // reports for a batch the function processed, asserted as the wire value rather than
            // through the emulator's own constant.
            Some("OK".to_string()),
            "case {case:?}: the mapping must report its processed batch"
        );
    }
}
