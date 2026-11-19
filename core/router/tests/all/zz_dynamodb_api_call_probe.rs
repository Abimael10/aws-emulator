// SPDX-License-Identifier: CC0-1.0

//! A probe of the BUS an AWS-published management event reaches, and of how many rules of it
//! one publication is handed to.
//!
//! RULE: an event AWS itself publishes — here CloudTrail's management-event fan-out for a
//! served control-plane call — is published onto the account's DEFAULT event bus and onto no
//! other, so EVERY rule of that bus whose pattern agrees with the envelope is handed its own
//! copy, while a rule carrying the very same pattern on a custom bus of the same account is
//! handed nothing.
//!
//! The sibling suites pin the other halves of the channel: `all/cloudtrail_dynamodb_api_call_events`
//! spans the envelope fields a pattern may pin and the call classes that are published at all,
//! `all/dynamodb_api_call_events` the join from a record back to the call its caller was
//! answered for, and `all/cloudtrail_api_call_events_by_region` the Region whose bus carries a
//! call's event. Which BUS of that Region carries it, and that one publication fans out to
//! every agreeing rule rather than to the first one found, is this suite's subject.
//!
//! The rows span what the rule generalises over: two rules of the default bus holding ONE
//! pattern between them (each with its own queue, so a single delivery cannot be read as two),
//! the same pattern on a bus of the account's own making, and — as the control for both — the
//! near miss of an event name nothing served.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call below is served in, and the Region the trail is homed in.
const REGION: &str = "us-west-2";

/// The account every emulated resource belongs to (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// The bus an AWS-published event is published onto: "AWS events are published to the default
/// event bus".
const DEFAULT_BUS: &str = "default";

/// A bus of the account's own making, which no AWS-published event reaches.
const OWN_BUS: &str = "api-call-own-bus";

/// The `detail-type` CloudTrail's management-event fan-out carries, and the `source` and
/// `eventSource` a DynamoDB call's event and record carry. Spelled out here rather than
/// imported from a service crate, so a wrong constant in the implementation cannot make this
/// suite pass.
const API_CALL_DETAIL_TYPE: &str = "AWS API Call via CloudTrail";
const DYNAMODB_EVENT_SOURCE: &str = "aws.dynamodb";
const DYNAMODB_RECORD_SOURCE: &str = "dynamodb.amazonaws.com";

/// The table the served control-plane call creates.
const TABLE: &str = "api-call-bus-reach";

/// How long a delivery is waited for, and how long between two looks. Generous: the suite
/// waits for the CONDITION (the envelope arriving), and the deadline only bounds a wait that
/// will never be satisfied.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The services this hop runs through, over ONE shared engine, as the emulator mounts them.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // Signing is not the subject here, so requests are unauthenticated; the credential scope
    // is still carried, because it is what names the Region.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request and what names the Region it was signed for. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// One `X-Amz-Target` RPC call, as its status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One EventBridge call (`awsJson1_1`) that must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "events", "AWSEvents", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon SQS call (`awsJson1_0`) that must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "sqs", "AmazonSQS", "1.0", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One CloudTrail call (`awsJson1_1`) that must answer 200.
async fn cloudtrail(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = rpc(
        router,
        "cloudtrail",
        "CloudTrail_20131101",
        "1.1",
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudTrail {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon DynamoDB call (`awsJson1_0`) that must answer 200.
async fn dynamodb(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = rpc(
        router,
        "dynamodb",
        "DynamoDB_20120810",
        "1.0",
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon DynamoDB {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// `PUT /<bucket>` plus `PUT /<bucket>?policy` — the bucket a trail delivers its log files
/// into, carrying the policy that lets CloudTrail write into it, created the way a client
/// creates it because CloudTrail resolves the destination in the account's object store.
async fn create_bucket(router: &MultiServiceRouter, bucket: &str) {
    // Outside the leader Region a create must name the Region it is made in
    // ("IllegalLocationConstraintException" otherwise), which is what this body carries.
    let configuration = format!(
        "<CreateBucketConfiguration><LocationConstraint>{REGION}</LocationConstraint>\
         </CreateBucketConfiguration>"
    );
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("/{bucket}"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("content-type", "application/xml")
        .header("authorization", authorization("s3"))
        .body(Bytes::from(configuration))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    assert_eq!(
        response.status().as_u16(),
        200,
        "setup: CreateBucket({bucket:?}) must succeed, got {} with {:?}",
        response.status(),
        String::from_utf8_lossy(response.body())
    );
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AWSCloudTrailWrite",
            "Effect": "Allow",
            "Principal": { "Service": "cloudtrail.amazonaws.com" },
            "Action": "s3:PutObject",
            "Resource": format!("arn:aws:s3:::{bucket}/*"),
            "Condition": { "StringEquals": { "s3:x-amz-acl": "bucket-owner-full-control" } },
        }],
    })
    .to_string();
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("/{bucket}?policy"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("s3"))
        .body(Bytes::from(policy))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    assert!(
        (200..300).contains(&response.status().as_u16()),
        "setup: PutBucketPolicy({bucket:?}) must succeed, got {} with {:?}",
        response.status(),
        String::from_utf8_lossy(response.body())
    );
}

/// One rule this suite installs: the bus it lives on, the event name its pattern pins, and
/// whether the served call below must reach it.
struct Row {
    case: &'static str,
    bus: &'static str,
    event_name: &'static str,
    expects: bool,
}

/// The rows, each on its own queue so one delivery can never be read as two.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a rule of the default bus",
            bus: DEFAULT_BUS,
            event_name: "CreateTable",
            expects: true,
        },
        Row {
            case: "a second rule of the default bus holding the same pattern",
            bus: DEFAULT_BUS,
            event_name: "CreateTable",
            expects: true,
        },
        Row {
            case: "the same pattern on a bus of the account's own making",
            bus: OWN_BUS,
            event_name: "CreateTable",
            expects: false,
        },
        Row {
            case: "a near miss on the event name, on the default bus",
            bus: DEFAULT_BUS,
            event_name: "DeleteTable",
            expects: false,
        },
    ]
}

/// The rule `slug` on `bus` pinned to `event_name`, with its own queue admitting EventBridge's
/// send on behalf of that rule and installed as its target; answers the queue's url.
async fn wired(
    router: &MultiServiceRouter,
    slug: &str,
    bus: &str,
    event_name: &str,
) -> String {
    let created = sqs(router, slug, "CreateQueue", &json!({ "QueueName": slug })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: expected a QueueUrl for {slug:?}, got {created}"))
        .to_string();
    let attributes = sqs(
        router,
        slug,
        "GetQueueAttributes",
        &json!({ "QueueUrl": &url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let queue_arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: expected a QueueArn for {slug:?}, got {attributes}"))
        .to_string();
    let pattern = json!({
        "source": [DYNAMODB_EVENT_SOURCE],
        "detail-type": [API_CALL_DETAIL_TYPE],
        "detail": {
            "eventSource": [DYNAMODB_RECORD_SOURCE],
            "eventName": [event_name],
        },
    });
    let put = events(
        router,
        slug,
        "PutRule",
        &json!({
            "Name": slug,
            "EventBusName": bus,
            "EventPattern": pattern.to_string(),
        }),
    )
    .await;
    let rule_arn = put["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: expected a RuleArn for {slug:?}, got {put}"))
        .to_string();
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "events.amazonaws.com" },
            "Action": "sqs:SendMessage",
            "Resource": &queue_arn,
            "Condition": { "ArnEquals": { "aws:SourceArn": rule_arn } },
        }],
    });
    sqs(
        router,
        slug,
        "SetQueueAttributes",
        &json!({ "QueueUrl": &url, "Attributes": { "Policy": document.to_string() } }),
    )
    .await;
    let targeted = events(
        router,
        slug,
        "PutTargets",
        &json!({
            "Rule": slug,
            "EventBusName": bus,
            "Targets": [{ "Id": "the-queue", "Arn": &queue_arn }],
        }),
    )
    .await;
    assert_eq!(
        targeted["FailedEntryCount"],
        json!(0),
        "case {slug:?}: the queue target must be accepted, got {targeted}"
    );
    url
}

/// Every envelope now waiting on the queue at `url`, as their parsed bodies.
async fn drained(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let answer = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    answer["Messages"]
        .as_array()
        .map(|messages| {
            messages
                .iter()
                .map(|message| {
                    let body = message["Body"].as_str().unwrap_or_default();
                    serde_json::from_str(body).unwrap_or_else(|error| {
                        panic!("case {case:?}: a delivered envelope is JSON ({error}): {body}")
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Look at the queue at `url` until it has handed over at least `wanted` envelopes, or
/// [`DELIVERY_DEADLINE`] passes — whichever comes first.
async fn delivered(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    wanted: usize,
) -> Vec<Value> {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    let mut seen = Vec::new();
    loop {
        seen.extend(drained(router, case, url).await);
        if seen.len() >= wanted || Instant::now() >= deadline {
            return seen;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: one AWS-published management event reaches every agreeing rule of the DEFAULT bus and
/// no rule of any other bus.
#[tokio::test]
async fn an_aws_published_management_event_reaches_the_default_bus_alone() {
    let router = router();

    // A trail of the account, logging. Its selectors are the defaults a trail is created with,
    // which log every management call of the account.
    create_bucket(&router, "api-call-bus-reach-trail").await;
    cloudtrail(
        &router,
        "setup",
        "CreateTrail",
        &json!({ "Name": "api-call-bus-reach", "S3BucketName": "api-call-bus-reach-trail" }),
    )
    .await;
    cloudtrail(
        &router,
        "setup",
        "StartLogging",
        &json!({ "Name": "api-call-bus-reach" }),
    )
    .await;

    // The account's own bus, which the rows below put a rule on.
    let own = events(
        &router,
        "setup",
        "CreateEventBus",
        &json!({ "Name": OWN_BUS }),
    )
    .await;
    assert_eq!(
        own["EventBusArn"],
        json!(format!("arn:aws:events:{REGION}:{ACCOUNT}:event-bus/{OWN_BUS}")),
        "setup: the account's own bus must be created, got {own}"
    );

    let rows = rows();
    let mut queues = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let slug = format!("api-call-bus-reach-{index}");
        queues.push(wired(&router, &slug, row.bus, row.event_name).await);
    }

    // The one served control-plane call every row above is decided against.
    let created = dynamodb(
        &router,
        "CreateTable",
        "CreateTable",
        &json!({
            "TableName": TABLE,
            "KeySchema": [{ "AttributeName": "id", "KeyType": "HASH" }],
            "AttributeDefinitions": [{ "AttributeName": "id", "AttributeType": "S" }],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    assert_eq!(
        created["TableDescription"]["TableName"],
        json!(TABLE),
        "the subject call must create the table, got {created}"
    );

    // The rows expecting a delivery are read first: once each of them has its envelope the
    // publication has been matched against every rule of the account, so a row that must stay
    // empty is read after it would have arrived rather than before.
    let mut arrived = Vec::new();
    for (row, url) in rows.iter().zip(queues.iter()) {
        let wanted = usize::from(row.expects);
        arrived.push(delivered(&router, row.case, url, wanted).await);
    }

    for ((row, url), delivery) in rows.iter().zip(queues.iter()).zip(arrived.iter()) {
        let case = row.case;
        if !row.expects {
            assert!(
                delivery.is_empty(),
                "case {case:?}: expected no delivery on {url}, got {delivery:?}"
            );
            continue;
        }
        assert_eq!(
            delivery.len(),
            1,
            "case {case:?}: expected exactly one delivered envelope, got {delivery:?}"
        );
        let envelope = &delivery[0];
        for (pointer, expected) in [
            ("/source", json!(DYNAMODB_EVENT_SOURCE)),
            ("/detail-type", json!(API_CALL_DETAIL_TYPE)),
            ("/region", json!(REGION)),
            ("/account", json!(ACCOUNT)),
            ("/detail/eventName", json!(row.event_name)),
            ("/detail/eventSource", json!(DYNAMODB_RECORD_SOURCE)),
            ("/detail/requestParameters/tableName", json!(TABLE)),
        ] {
            let actual = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {pointer} to be {expected}, got {actual} in {envelope}"
            );
        }
    }
}
