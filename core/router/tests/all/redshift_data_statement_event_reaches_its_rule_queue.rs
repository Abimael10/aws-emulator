// SPDX-License-Identifier: CC0-1.0

//! A Redshift Data statement submitted with event emission is announced on the default
//! EventBridge bus and delivered to the SQS target of the rule that matches it — on the
//! WHOLE assembled router, with request signatures verified.
//!
//! The RULE under test: a statement submitted with `WithEvent` — "a value that indicates
//! whether to send an event to the Amazon EventBridge event bus after the SQL statement
//! runs"
//! (`smithy_query(query="doc",shape="ExecuteStatementInput",member="WithEvent",model_path="services/service-com-amazonaws-redshiftdata-redshiftdata/smithy-model/com-amazonaws-redshiftdata-redshiftdata.json")`)
//! — has the status change it makes when it stops published once onto the account's
//! default bus, and a rule whose `EventPattern` matches that envelope delivers it to
//! every target it names whose queue the account really holds. A target naming a queue
//! nothing created delivers nowhere and takes none of the other targets' deliveries with
//! it, and a `PurgeQueue` removes only the announcements the queue already held
//! (`PurgeQueueRequest`: "deletes available messages in a queue (including in-flight
//! messages)"), so the next statement's announcement is delivered to the same queue
//! normally.
//!
//! What this adds over the crate-local suites of
//! `service-com-amazonaws-redshiftdata-redshiftdata`, which pin the same rule over five
//! services mounted alone with authentication off: the announcement is published from a
//! task that outlives the request that started it, over the emulator's own callback, on a
//! router that VERIFIES every signature — the configuration the emulator serves traffic
//! in — with every other service mounted beside it, so no second service claiming the
//! `events` or `sqs` key can swallow the publication or its delivery.
//!
//! The rows span what the rule generalises over: whether the mis-wired target sits before
//! or after the one that resolves, and whether the stale announcement a purge has to
//! remove was still waiting or already in flight. What the purge did is read off the
//! queue's OWN counters (`ApproximateNumberOfMessages` and
//! `ApproximateNumberOfMessagesNotVisible`): an announcement already handed to a receiver
//! is invisible to `ReceiveMessage` whether the purge removed it or not, so a receive
//! alone cannot tell the in-flight half of that sentence apart from a queue that kept it.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on an explicitly sized thread.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4::SigningParams;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode, opt_element};

/// Keep this at or above the size `boot_serve.rs` uses: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// What the emulator's own boot path gives each tokio WORKER thread, which is where
/// request handling — and so the deepest generated deserializer — really runs.
const WORKER_STACK_SIZE: usize = 16 * 1024 * 1024;

/// The Region every call is signed for and addressed in, and so the `region` the
/// envelopes must report.
const REGION: &str = "us-west-2";

/// The `source` and `detail-type` a statement's status change is published under, spelled
/// out here rather than imported from the service crate, so a wrong constant in the
/// implementation cannot make this suite pass.
const SOURCE: &str = "aws.redshift-data";
const DETAIL_TYPE: &str = "Redshift Data Statement Status Change";

/// The service principal EventBridge delivers to an SQS target as, which the queue's own
/// policy has to allow the send for.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The warehouse every statement here runs on, the database it is created with and the
/// database user a provisioned cluster's statements authenticate as.
const CLUSTER: &str = "statement-event-cluster";
const DATABASE: &str = "devdb";
const DB_USER: &str = "adminuser";

/// A statement the engine runs to completion.
const SQL: &str = "SELECT 1";

/// The statuses a statement's lifecycle ends at.
const TERMINAL: [&str; 3] = ["FINISHED", "FAILED", "ABORTED"];

/// Amazon Redshift's signing name and the API version its form wire carries.
const REDSHIFT_SIGNING_NAME: &str = "redshift";
const REDSHIFT_API_VERSION: &str = "2012-12-01";

/// How long a poll keeps looking before the suite calls what it waited for unreachable,
/// and how often it looks meanwhile. Generous: what is asserted is that the announcement
/// arrives at all, never how fast the host runs.
const POLL_DEADLINE: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long the ABSENCE halves keep watching a queue that must stay empty. A bound on an
/// absence, so it is the one span a slow host may spend without changing the verdict: the
/// delivery that did land is read first, so the bus has already made every attempt the
/// same announcement produces.
const ABSENCE_WINDOW: Duration = Duration::from_secs(5);

/// The suffix that turns a queue's ARN into one naming a queue no account holds: a
/// well-formed Amazon SQS ARN whose name is the mis-typed one. Amazon SQS queue names take
/// "alphanumeric characters, hyphens (-), and underscores (_)" (`CreateQueue$QueueName`),
/// so this is a name a queue COULD have had and none has.
const MISTYPED_SUFFIX: &str = "-typo";

/// The key pair this process publishes once request authentication is on, which every call
/// below is signed with.
struct Signer {
    key_id: String,
    secret: String,
}

impl Signer {
    /// Sign `request` for `signing_name` in [`REGION`], in the `Authorization` header an
    /// SDK request carries.
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

/// One SIGNED `POST /` on the regional endpoint of `signing_name`, answering the HTTP
/// status and the body bytes as answered.
async fn call(
    router: &MultiServiceRouter,
    signer: &Signer,
    signing_name: &str,
    content_type: &str,
    target: Option<&str>,
    body: Vec<u8>,
) -> (u16, Vec<u8>) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let mut builder = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", content_type);
    if let Some(target) = target {
        builder = builder.header("x-amz-target", target);
    }
    let built = builder.body(body).expect("the test request is well formed");
    let signed = signer.sign(built, signing_name);
    let (parts, payload) = signed.into_parts();
    let request = http::Request::from_parts(parts, bytes::Bytes::from(payload));
    let response = router.dispatch(request).await;
    (response.status().as_u16(), response.body().to_vec())
}

/// One `X-Amz-Target` RPC call, requiring a 200 and answering the parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    signing_name: &str,
    content_type: &str,
    target: &str,
    body: &Value,
) -> Value {
    let (status, payload) = call(
        router,
        signer,
        signing_name,
        content_type,
        Some(target),
        body.to_string().into_bytes(),
    )
    .await;
    let parsed: Value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {target}, got {status} with {parsed}"
    );
    parsed
}

/// One EventBridge call (`awsJson1_1`).
async fn events(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    rpc(
        router,
        signer,
        case,
        "events",
        "application/x-amz-json-1.1",
        &format!("AWSEvents.{operation}"),
        body,
    )
    .await
}

/// One Amazon SQS call (`awsJson1_0`).
async fn sqs(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    rpc(
        router,
        signer,
        case,
        "sqs",
        "application/x-amz-json-1.0",
        &format!("AmazonSQS.{operation}"),
        body,
    )
    .await
}

/// One Redshift Data API call (`awsJson1_1`).
async fn data(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    rpc(
        router,
        signer,
        case,
        "redshift-data",
        "application/x-amz-json-1.1",
        &format!("RedshiftData.{operation}"),
        body,
    )
    .await
}

/// One Amazon Redshift call over its `awsQuery` FORM wire, requiring a 200 and answering
/// the XML body.
async fn redshift(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={REDSHIFT_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let (status, payload) = call(
        router,
        signer,
        REDSHIFT_SIGNING_NAME,
        "application/x-www-form-urlencoded",
        None,
        body.into_bytes(),
    )
    .await;
    let text = String::from_utf8_lossy(&payload).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Redshift {action}, got {status} with {text}"
    );
    text
}

/// The node type of the first orderable option offering the single-node cluster type, read
/// out of the catalog rather than written here.
fn single_node_node_type(body: &str) -> String {
    for option in body.split("<OrderableClusterOption>").skip(1) {
        if opt_element(option, "ClusterType").as_deref() == Some("single-node") {
            return element(option, "NodeType");
        }
    }
    panic!("the orderable catalog offers no single-node option: {body}")
}

/// Create the cluster the statements run on and wait until Amazon Redshift reports it
/// available, the way a client does before submitting any statement to it.
async fn given_available_cluster(router: &MultiServiceRouter, signer: &Signer, case: &str) {
    let catalog = redshift(
        router,
        signer,
        case,
        "DescribeOrderableClusterOptions",
        &[],
    )
    .await;
    let node_type = single_node_node_type(&catalog);
    redshift(
        router,
        signer,
        case,
        "CreateCluster",
        &[
            ("ClusterIdentifier", CLUSTER),
            ("NodeType", &node_type),
            ("MasterUsername", DB_USER),
            ("MasterUserPassword", "Probe-Password-1"),
            ("ClusterType", "single-node"),
            ("DBName", DATABASE),
        ],
    )
    .await;
    let started = Instant::now();
    loop {
        let described = redshift(
            router,
            signer,
            case,
            "DescribeClusters",
            &[("ClusterIdentifier", CLUSTER)],
        )
        .await;
        if opt_element(&described, "ClusterStatus").as_deref() == Some("available") {
            return;
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case {case:?}: the cluster never became available: {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A queue a rule can target: its url and the ARN a rule names it by, with its own policy
/// granting EventBridge the send for the rule `rule_arn` and no other.
async fn given_target_queue(
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
        &json!({"QueueName": name}),
    )
    .await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        router,
        signer,
        case,
        "GetQueueAttributes",
        &json!({"QueueUrl": &url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueArn in {attributes}"))
        .to_string();
    // The grant a client really writes: the service principal, the one action, the queue,
    // and the condition that scopes it to the rule whose deliveries it is for.
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowEventBridgeDelivery",
            "Effect": "Allow",
            "Principal": {"Service": EVENTS_PRINCIPAL},
            "Action": "sqs:SendMessage",
            "Resource": &arn,
            "Condition": {"ArnEquals": {"aws:SourceArn": rule_arn}},
        }],
    })
    .to_string();
    sqs(
        router,
        signer,
        case,
        "SetQueueAttributes",
        &json!({"QueueUrl": &url, "Attributes": {"Policy": policy}}),
    )
    .await;
    (url, arn)
}

/// An ENABLED rule on the default bus matching every statement status change this service
/// publishes, answering the ARN that rule carries.
async fn given_rule(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    name: &str,
) -> String {
    let put = events(
        router,
        signer,
        case,
        "PutRule",
        &json!({
            "Name": name,
            "EventPattern": json!({"source": [SOURCE], "detail-type": [DETAIL_TYPE]}).to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    put["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a RuleArn in {put}"))
        .to_string()
}

/// Point the rule `name` at `target_arns`, in the order given, and require that every
/// entry was accepted: `PutTargets` validates the ARN it is handed, not the existence of
/// what that ARN names.
async fn given_targets(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    name: &str,
    target_arns: &[&str],
) {
    let targets: Vec<Value> = target_arns
        .iter()
        .enumerate()
        .map(|(slot, arn)| json!({"Id": format!("target-{slot}"), "Arn": arn}))
        .collect();
    let answered = events(
        router,
        signer,
        case,
        "PutTargets",
        &json!({"Rule": name, "Targets": targets}),
    )
    .await;
    assert_eq!(
        answered["FailedEntryCount"],
        json!(0),
        "case {case:?}: every target of {name:?} must be accepted, got {answered}"
    );
}

/// Submit `SQL` with event emission asked for and WITHOUT waiting for it inside the
/// submit — the path a client driven by the event takes — answering the `Id` the
/// submission was answered with.
async fn submit_announced(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    statement_name: &str,
) -> String {
    let answered = data(
        router,
        signer,
        case,
        "ExecuteStatement",
        &json!({
            "ClusterIdentifier": CLUSTER,
            "Database": DATABASE,
            "DbUser": DB_USER,
            "Sql": SQL,
            "WithEvent": true,
            "StatementName": statement_name,
        }),
    )
    .await;
    answered["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected an Id in {answered}"))
        .to_string()
}

/// Poll `DescribeStatement` until the statement `id` reports a terminal status, answering
/// the record that reported it.
async fn wait_for_terminal(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    id: &str,
) -> Value {
    let started = Instant::now();
    loop {
        let described = data(
            router,
            signer,
            case,
            "DescribeStatement",
            &json!({"Id": id}),
        )
        .await;
        let status = described["Status"].as_str().unwrap_or_default().to_string();
        if TERMINAL.contains(&status.as_str()) {
            return described;
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case {case:?}: {id} reported {status:?} and never stopped: {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Receive everything waiting on the queue at `url` and delete it, answering the parsed
/// envelopes its message bodies carried.
async fn drain(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    url: &str,
) -> Vec<Value> {
    let received = sqs(
        router,
        signer,
        case,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut envelopes = Vec::new();
    for message in &messages {
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                router,
                signer,
                case,
                "DeleteMessage",
                &json!({"QueueUrl": url, "ReceiptHandle": handle}),
            )
            .await;
        }
        envelopes.push(
            serde_json::from_str::<Value>(message["Body"].as_str().unwrap_or_default())
                .unwrap_or(Value::Null),
        );
    }
    envelopes
}

/// Receive everything waiting on the queue at `url` WITHOUT deleting any of it, so what it
/// hands back stays in flight.
async fn receive_without_deleting(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    url: &str,
) -> Vec<Value> {
    let received = sqs(
        router,
        signer,
        case,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    received["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|message| {
            serde_json::from_str::<Value>(message["Body"].as_str().unwrap_or_default())
                .unwrap_or(Value::Null)
        })
        .collect()
}

/// Look at the queue at `url` until the announcement of `statement_id` is there,
/// answering it. Deleting what it reads, so a later read of the same queue is about later
/// announcements.
async fn await_announcement(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    url: &str,
    statement_id: &str,
) -> Value {
    let started = Instant::now();
    let mut seen: Vec<Value> = Vec::new();
    loop {
        seen.extend(drain(router, signer, case, url).await);
        if let Some(found) = seen
            .iter()
            .find(|envelope| envelope.pointer("/detail/statementId") == Some(&json!(statement_id)))
        {
            return found.clone();
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case {case:?}: the announcement of {statement_id} never reached {url}, got {seen:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// How many messages the queue at `url` holds, and how many of those are in flight.
async fn depth(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    url: &str,
) -> (i64, i64) {
    let attributes = sqs(
        router,
        signer,
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
    let read = |name: &str| {
        attributes["Attributes"][name]
            .as_str()
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or_else(|| panic!("case {case:?}: a queue reports {name}: {attributes}"))
    };
    (
        read("ApproximateNumberOfMessages"),
        read("ApproximateNumberOfMessagesNotVisible"),
    )
}

/// Assert that the queue at `url` receives nothing at all over [`ABSENCE_WINDOW`].
async fn assert_stays_empty(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    url: &str,
    what: &str,
) {
    let watching = Instant::now();
    while watching.elapsed() < ABSENCE_WINDOW {
        let leaked = drain(router, signer, case, url).await;
        assert!(
            leaked.is_empty(),
            "case {case:?}: {what} must receive nothing, got {leaked:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let (waiting, in_flight) = depth(router, signer, case, url).await;
    assert_eq!(
        (waiting, in_flight),
        (0, 0),
        "case {case:?}: {what} must stay empty, got {waiting} waiting and {in_flight} in flight"
    );
}

/// The envelope identity every announcement of this service carries, whichever queue it
/// was delivered to.
fn assert_identity(case: &str, envelope: &Value, statement_id: &str, status: &Value) {
    for (pointer, expected) in [
        ("/source", json!(SOURCE)),
        ("/detail-type", json!(DETAIL_TYPE)),
        ("/region", json!(REGION)),
        ("/account", json!(engine::EMULATOR_ACCOUNT_ID)),
        ("/detail/statementId", json!(statement_id)),
        ("/detail/state", status.clone()),
    ] {
        let actual = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {pointer} to be {expected}, got {actual} in {envelope}"
        );
    }
    // The warehouse the statement ran on, named in the envelope's `resources`.
    let resources = envelope.pointer("/resources").cloned().unwrap_or(Value::Null);
    assert!(
        resources.as_array().is_some_and(|named| named
            .iter()
            .any(|resource| resource
                .as_str()
                .is_some_and(|arn| arn.ends_with(&format!("cluster:{CLUSTER}"))))),
        "case {case:?}: expected the announcement to name the cluster it ran on, got \
         {resources} in {envelope}"
    );
}

/// Where among a rule's targets the mis-typed one sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    /// The target names the queue created for this slot.
    Wired,
    /// The target names a queue nothing created, so the queue created for this slot is the
    /// one the caller MEANT and never gets anything.
    Mistyped,
}

/// Whether the stale announcement a purge has to remove was still waiting, or had already
/// been handed to a receiver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stale {
    Waiting,
    InFlight,
}

#[test]
fn a_with_event_statement_reaches_its_rule_s_queue_on_an_authenticating_router() {
    std::thread::Builder::new()
        .name("redshift-data-statement-events".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            // A MULTI-THREADED runtime, as the emulator's own boot path builds: the
            // announcement is published by a task that outlives the request which
            // started it, so it runs on another worker than the caller's poll.
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                // The workers run the deepest generated deserializers, exactly as the
                // emulator's own boot path sizes them.
                .thread_stack_size(WORKER_STACK_SIZE)
                .enable_all()
                .build()
                .expect("build a multi-threaded runtime");
            runtime.block_on(drive());
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread runs to completion");
}

/// Every row, against ONE assembled router.
async fn drive() {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(Arc::clone(&engine));
    // The whole suite runs with request authentication ON, as the emulator serves traffic.
    router.set_request_authentication(true);
    let published = ::emulator_server::SigV4Authenticator::minted_credentials()
        .expect("turning authentication on publishes this process's keys");
    let signer = Signer {
        key_id: published.access_key_id.clone(),
        secret: published.secret_access_key.clone(),
    };

    given_available_cluster(&router, &signer, "the cluster every statement runs on").await;

    // Half one: WHERE an announcement lands among a rule's targets.
    let wirings: [(&str, &[Slot]); 2] = [
        (
            "the mis-typed target comes first",
            &[Slot::Mistyped, Slot::Wired],
        ),
        (
            "the mis-typed target comes last",
            &[Slot::Wired, Slot::Mistyped],
        ),
    ];
    for (index, (case, slots)) in wirings.iter().enumerate() {
        let rule = format!("statement-events-{index}");
        let rule_arn = given_rule(&router, &signer, case, &rule).await;
        let mut queues: Vec<(Slot, String, String)> = Vec::new();
        for (slot, kind) in slots.iter().enumerate() {
            let (url, arn) = given_target_queue(
                &router,
                &signer,
                case,
                &format!("statement-events-{index}-{slot}"),
                &rule_arn,
            )
            .await;
            let target_arn = match kind {
                Slot::Wired => arn,
                Slot::Mistyped => format!("{arn}{MISTYPED_SUFFIX}"),
            };
            queues.push((*kind, url, target_arn));
        }
        let targets: Vec<&str> = queues.iter().map(|(_, _, arn)| arn.as_str()).collect();
        given_targets(&router, &signer, case, &rule, &targets).await;

        let id = submit_announced(&router, &signer, case, "wiring-statement").await;
        let record = wait_for_terminal(&router, &signer, case, &id).await;
        assert_eq!(
            record["Status"],
            json!("FINISHED"),
            "case {case:?}: expected the statement to finish, got {record}"
        );

        for (kind, url, _) in &queues {
            if *kind != Slot::Wired {
                continue;
            }
            let envelope = await_announcement(&router, &signer, case, url, &id).await;
            assert_identity(case, &envelope, &id, &record["Status"]);
        }
        // Read AFTER a delivery that did land, so the bus has already made every attempt
        // this announcement produces.
        for (kind, url, target_arn) in &queues {
            if *kind != Slot::Mistyped {
                continue;
            }
            assert_stays_empty(
                &router,
                &signer,
                case,
                url,
                &format!("the queue the mis-typed target {target_arn} was meant to name"),
            )
            .await;
        }
    }

    // Half two: a purge removes the announcements the queue already held and no later one.
    for (index, stale) in [Stale::Waiting, Stale::InFlight].into_iter().enumerate() {
        let case: &str = match stale {
            Stale::Waiting => "a purge of an announcement still waiting",
            Stale::InFlight => "a purge of an announcement already in flight",
        };
        let rule = format!("purged-statement-events-{index}");
        let rule_arn = given_rule(&router, &signer, case, &rule).await;
        let (url, arn) = given_target_queue(
            &router,
            &signer,
            case,
            &format!("purged-statement-events-{index}"),
            &rule_arn,
        )
        .await;
        given_targets(&router, &signer, case, &rule, &[arn.as_str()]).await;

        let stale_id = submit_announced(&router, &signer, case, "stale-statement").await;
        let stale_record = wait_for_terminal(&router, &signer, case, &stale_id).await;
        match stale {
            // Look until the queue reports it waiting, WITHOUT receiving it, so the
            // message the purge has to remove is still visible when the purge arrives.
            Stale::Waiting => {
                let started = Instant::now();
                loop {
                    let (waiting, in_flight) = depth(&router, &signer, case, &url).await;
                    if waiting > 0 {
                        break;
                    }
                    assert!(
                        started.elapsed() < POLL_DEADLINE,
                        "case {case:?}: the queue never reported a waiting announcement, got \
                         {waiting} waiting and {in_flight} in flight"
                    );
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
            }
            // Received and not deleted, so it is in flight when the purge arrives.
            Stale::InFlight => {
                let started = Instant::now();
                loop {
                    let held = receive_without_deleting(&router, &signer, case, &url).await;
                    if let Some(envelope) = held
                        .iter()
                        .find(|e| e.pointer("/detail/statementId") == Some(&json!(&stale_id)))
                    {
                        // The whole stale envelope, so a purge row that received the
                        // WRONG announcement fails here rather than satisfying the loop
                        // on an id alone.
                        assert_identity(case, envelope, &stale_id, &stale_record["Status"]);
                        break;
                    }
                    assert!(
                        started.elapsed() < POLL_DEADLINE,
                        "case {case:?}: the stale announcement of {stale_id} never reached {url}"
                    );
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
                // Received and not deleted is IN FLIGHT, which is the half of the purge
                // this row exists to pin: the queue reports it as not visible, and
                // `ReceiveMessage` cannot see it either way, so what the purge did to it
                // is only readable off the counters below.
                let (waiting, in_flight) = depth(&router, &signer, case, &url).await;
                assert_eq!(
                    (waiting, in_flight),
                    (0, 1),
                    "case {case:?}: a received announcement is in flight until it is \
                     deleted, got {waiting} waiting and {in_flight} in flight"
                );
            }
        }

        sqs(
            &router,
            &signer,
            case,
            "PurgeQueue",
            &json!({"QueueUrl": &url}),
        )
        .await;
        let left = drain(&router, &signer, case, &url).await;
        assert!(
            left.is_empty(),
            "case {case:?}: a purged queue holds none of the announcements it held, got {left:?}"
        );
        // The counters, not the receive: an announcement already handed to a receiver is
        // invisible to `ReceiveMessage` whether the purge removed it or not, so only
        // `ApproximateNumberOfMessages` and `ApproximateNumberOfMessagesNotVisible` can
        // say that "deletes available messages in a queue (including in-flight messages)"
        // really covered it.
        let (waiting, in_flight) = depth(&router, &signer, case, &url).await;
        assert_eq!(
            (waiting, in_flight),
            (0, 0),
            "case {case:?}: a purged queue holds nothing it held, waiting or in flight, got \
             {waiting} waiting and {in_flight} in flight"
        );

        // What the purge did NOT remove: the next statement's announcement.
        let fresh_id = submit_announced(&router, &signer, case, "fresh-statement").await;
        let record = wait_for_terminal(&router, &signer, case, &fresh_id).await;
        let envelope = await_announcement(&router, &signer, case, &url, &fresh_id).await;
        assert_identity(case, &envelope, &fresh_id, &record["Status"]);
        assert_ne!(
            envelope.pointer("/detail/statementId"),
            Some(&json!(&stale_id)),
            "case {case:?}: a purged announcement never comes back, got {envelope}"
        );
    }
}
