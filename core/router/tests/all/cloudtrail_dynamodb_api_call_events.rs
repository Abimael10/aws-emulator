// SPDX-License-Identifier: CC0-1.0

//! The management-event fan-out of a service with NO event source of its own, reaching the
//! EventBridge rules written over its control plane.
//!
//! RULE: while a trail of the Region is logging management events of a call's read/write
//! class, every control-plane call the account serves — whichever service serves it — is
//! ALSO published onto that Region's DEFAULT event bus, with `source` `aws.<endpoint-prefix>`
//! of the serving service, `detail-type` `AWS API Call via CloudTrail` and the CloudTrail
//! record of the call as `detail` (`eventSource`, `eventName`, `awsRegion`,
//! `recipientAccountId`, `readOnly`, `eventCategory` and the call's `requestParameters`). A
//! rule whose pattern names `source`, `detail-type`, `detail.eventSource` or
//! `detail.eventName` is handed exactly the events that match it and nothing else.
//!
//! Only a cross-service suite can pin this: the events are published by the services that
//! SERVED the calls (Amazon DynamoDB over awsJson1_0, Amazon SQS over awsJson1_0), the
//! delivery decision belongs to CloudTrail (awsJson1_1), the match and the hand-over belong
//! to EventBridge (awsJson1_1) and what arrived is read back from Amazon SQS — four services
//! over ONE engine, as `core/router/src/lib.rs` mounts them.
//!
//! The rows span what the rule generalises over rather than the one call that motivated it:
//! which service served the call (a control plane with no native bus events of its own, and
//! one that has them), the event NAME a pattern pins (two writes of ONE service, each
//! reaching only its own rule and never the other's), a pattern that names the record's
//! `eventSource` instead of the envelope's `source`, the read/write class the trail's
//! selector admits (a reporting call under a `WriteOnly` selector reaches nothing) and the
//! CLASS of call (a per-item data-plane write is no management event, so it publishes
//! nothing on this channel).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call of this suite is served in, and the Region the trail is homed in.
const REGION: &str = "us-west-2";

/// The account every emulated resource belongs to (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// The `detail-type` CloudTrail's management-event fan-out carries
/// (`service_common::api_call_events::API_CALL_DETAIL_TYPE`).
const API_CALL_DETAIL_TYPE: &str = "AWS API Call via CloudTrail";

/// The `source` an event published for an Amazon DynamoDB call carries.
const DYNAMODB_EVENT_SOURCE: &str = "aws.dynamodb";

/// The `eventSource` the CloudTrail record of an Amazon DynamoDB call carries.
const DYNAMODB_RECORD_SOURCE: &str = "dynamodb.amazonaws.com";

/// The `source` an event published for an Amazon SQS call carries.
const SQS_EVENT_SOURCE: &str = "aws.sqs";

/// The `eventSource` the CloudTrail record of an Amazon SQS call carries.
const SQS_RECORD_SOURCE: &str = "sqs.amazonaws.com";

/// The table the control-plane writes below create and delete.
const TABLE: &str = "cloudtrail-fan-out-subject";

/// How long a delivery is waited for. Generous: the suite waits for the CONDITION (the
/// envelope arriving on the queue), and this is only the point at which waiting longer
/// cannot help.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(120);

/// How long between two looks at a queue, or at a table's status, while waiting for one.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The services this fan-out runs through, over ONE shared engine.
///
/// Amazon S3 is mounted beside them because a trail's destination bucket is resolved in the
/// account's object store: the trail is created the way a client creates one, against a
/// bucket that really exists.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // Signing is not the subject here, so requests are unauthenticated; the credential
    // scope is still carried, because it is what names the Region.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service` and [`REGION`], which is
/// what routes the request and what names the Region it was signed for. The signature is
/// never checked here.
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

/// An EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`) that must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "events", "AWSEvents", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with {parsed}"
    );
    parsed
}

/// An Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "sqs", "AmazonSQS", "1.0", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// An Amazon DynamoDB call (`awsJson1_0`, `DynamoDB_20120810.<Operation>`) that must answer
/// 200.
async fn dynamodb(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "dynamodb", "DynamoDB_20120810", "1.0", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon DynamoDB {operation}, \
         got {status} with {parsed}"
    );
    parsed
}

/// A CloudTrail call (`awsJson1_1`, `CloudTrail_20131101.<Operation>`) that must answer 200.
async fn cloudtrail(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) =
        rpc(router, "cloudtrail", "CloudTrail_20131101", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudTrail {operation}, got {status} with {parsed}"
    );
    parsed
}

/// `PUT /<bucket>` plus `PUT /<bucket>?policy` — the bucket the trail delivers its log files
/// into, carrying the bucket policy that lets CloudTrail write into it.
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
            "Condition": {
                "StringEquals": { "s3:x-amz-acl": "bucket-owner-full-control" },
            },
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

/// Create a queue and hand back its url and ARN. Nothing may send to it yet: the grant is
/// written once the rule that delivers to it exists ([`admitting_rule`]), because it names
/// that rule's ARN.
async fn create_queue(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let created = sqs(router, name, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: expected a QueueUrl for {name:?}, got {created}"))
        .to_string();
    let arn = format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}");
    (url, arn)
}

/// Let `rule_arn` — and only it — send to the queue at `url`: the resource policy a client
/// writes for an EventBridge target, naming the service principal the delivery is made as
/// and scoping it to the rule that makes it.
async fn admitting_rule(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    queue_arn: &str,
    rule_arn: &str,
) {
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "events.amazonaws.com" },
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
            "Condition": { "ArnEquals": { "aws:SourceArn": rule_arn } },
        }],
    });
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": policy.to_string() } }),
    )
    .await;
}

/// Receive up to ten messages from `url`, draining them, as their parsed bodies — which for
/// a queue target carrying no input transformation is the whole event envelope.
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

/// Look at `url` until it has handed back at least `wanted` envelopes, or
/// [`DELIVERY_DEADLINE`] has passed — whichever comes first, and then ONCE more. The
/// CONDITION is what is waited for; the deadline only bounds a wait that will never be
/// satisfied.
///
/// The extra look is what lets a row read "exactly one envelope": a SECOND envelope for the
/// same rule arrives a moment after the first, so a drain that stopped at the one it was
/// waiting for could not tell one delivery from two.
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
            seen.extend(drained(router, case, url).await);
            return seen;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One rule this suite installs: the pattern it matches with, and which call's record — if
/// any — must reach it.
struct Row {
    case: &'static str,
    /// The rule's `EventPattern`.
    pattern: Value,
    /// The `source` and `eventName` of the record this row expects, or `None` for a row
    /// that must stay empty.
    expects: Option<(&'static str, &'static str)>,
}

/// The rules, each on its own queue so what arrived at one is never read as another's.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the control write of a service that has its own bus events",
            pattern: json!({
                "source": [SQS_EVENT_SOURCE],
                "detail-type": [API_CALL_DETAIL_TYPE],
                "detail": {
                    "eventSource": [SQS_RECORD_SOURCE],
                    "eventName": ["TagQueue"],
                },
            }),
            expects: Some((SQS_EVENT_SOURCE, "TagQueue")),
        },
        Row {
            case: "a control plane with no bus events of its own, pinned to its create",
            pattern: json!({
                "source": [DYNAMODB_EVENT_SOURCE],
                "detail-type": [API_CALL_DETAIL_TYPE],
                "detail": {
                    "eventSource": [DYNAMODB_RECORD_SOURCE],
                    "eventName": ["CreateTable"],
                },
            }),
            expects: Some((DYNAMODB_EVENT_SOURCE, "CreateTable")),
        },
        Row {
            case: "the near-miss rule of the same service, pinned to its delete",
            pattern: json!({
                "source": [DYNAMODB_EVENT_SOURCE],
                "detail-type": [API_CALL_DETAIL_TYPE],
                "detail": {
                    "eventSource": [DYNAMODB_RECORD_SOURCE],
                    "eventName": ["DeleteTable"],
                },
            }),
            expects: Some((DYNAMODB_EVENT_SOURCE, "DeleteTable")),
        },
        Row {
            case: "a rule naming only the record's own eventSource",
            pattern: json!({
                "detail-type": [API_CALL_DETAIL_TYPE],
                "detail": {
                    "eventSource": [DYNAMODB_RECORD_SOURCE],
                    "eventName": ["UpdateTimeToLive"],
                },
            }),
            expects: Some((DYNAMODB_EVENT_SOURCE, "UpdateTimeToLive")),
        },
        Row {
            case: "a rule reaching into the record's own requestParameters",
            pattern: json!({
                "source": [DYNAMODB_EVENT_SOURCE],
                "detail-type": [API_CALL_DETAIL_TYPE],
                "detail": {
                    "eventName": ["UpdateContinuousBackups"],
                    "requestParameters": { "tableName": [TABLE] },
                },
            }),
            expects: Some((DYNAMODB_EVENT_SOURCE, "UpdateContinuousBackups")),
        },
        Row {
            case: "a reporting call of that control plane, outside a WriteOnly selector",
            pattern: json!({
                "source": [DYNAMODB_EVENT_SOURCE],
                "detail-type": [API_CALL_DETAIL_TYPE],
                "detail": { "eventName": ["DescribeTable", "ListTables"] },
            }),
            expects: None,
        },
        Row {
            case: "the per-item data plane, which is no management event at all",
            pattern: json!({
                "source": [DYNAMODB_EVENT_SOURCE],
                "detail-type": [API_CALL_DETAIL_TYPE],
                "detail": { "eventName": ["PutItem", "GetItem"] },
            }),
            expects: None,
        },
    ]
}

/// Look at the table's status until it reports `wanted`, or [`DELIVERY_DEADLINE`] has
/// passed. A create reaches `ACTIVE` on its own clock, so the CONDITION is waited for.
async fn table_status(router: &MultiServiceRouter, wanted: &str) -> String {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let described = dynamodb(
            router,
            "setup",
            "DescribeTable",
            &json!({ "TableName": TABLE }),
        )
        .await;
        let status = described["Table"]["TableStatus"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if status == wanted || Instant::now() >= deadline {
            return status;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn cloudtrail_sourced_api_call_events_reach_the_rules_written_over_a_control_plane() {
    let router = router();

    // A trail of the account, logging the WRITE management events of its own Region.
    create_bucket(&router, "cloudtrail-fan-out-trail").await;
    cloudtrail(
        &router,
        "setup",
        "CreateTrail",
        &json!({
            "Name": "cloudtrail-fan-out",
            "S3BucketName": "cloudtrail-fan-out-trail",
        }),
    )
    .await;
    cloudtrail(
        &router,
        "setup",
        "PutEventSelectors",
        &json!({
            "TrailName": "cloudtrail-fan-out",
            "EventSelectors": [{ "ReadWriteType": "WriteOnly", "IncludeManagementEvents": true }],
        }),
    )
    .await;
    cloudtrail(
        &router,
        "setup",
        "StartLogging",
        &json!({ "Name": "cloudtrail-fan-out" }),
    )
    .await;
    let status = cloudtrail(
        &router,
        "setup",
        "GetTrailStatus",
        &json!({ "Name": "cloudtrail-fan-out" }),
    )
    .await;
    assert_eq!(
        status["IsLogging"],
        json!(true),
        "setup: the trail must be logging, got {status}"
    );

    // One rule per row, each on its own queue, all on the DEFAULT bus — the only bus an
    // AWS-published event reaches. The target carries no input transformation, so what a
    // consumer reads is the whole envelope.
    let rows = rows();
    let mut queues = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let name = format!("cloudtrail-fan-out-sink-{index}");
        let (url, queue_arn) = create_queue(&router, &name).await;
        let rule = format!("cloudtrail-fan-out-rule-{index}");
        let created = events(
            &router,
            row.case,
            "PutRule",
            &json!({ "Name": rule, "EventPattern": row.pattern.to_string() }),
        )
        .await;
        let rule_arn = created["RuleArn"]
            .as_str()
            .unwrap_or_else(|| panic!("setup: expected a RuleArn for {rule:?}, got {created}"))
            .to_string();
        let put = events(
            &router,
            row.case,
            "PutTargets",
            &json!({
                "Rule": rule,
                "Targets": [{ "Id": "sink", "Arn": queue_arn }],
            }),
        )
        .await;
        assert_eq!(
            put["FailedEntryCount"],
            json!(0),
            "case {:?}: the target must be installed, got {put}",
            row.case
        );
        admitting_rule(&router, row.case, &url, &queue_arn, &rule_arn).await;
        queues.push(url);
    }

    // A queue for the control write to act on, so the control rule sees the record of a
    // call the account really served.
    let (control_url, _) = create_queue(&router, "cloudtrail-fan-out-control").await;
    sqs(
        &router,
        "control",
        "TagQueue",
        &json!({ "QueueUrl": control_url, "Tags": { "owner": "api-call-events" } }),
    )
    .await;

    // The calls the rows above are about: the create, a reporting call, a per-item write,
    // a second control-plane write and the delete.
    dynamodb(
        &router,
        "subject",
        "CreateTable",
        &json!({
            "TableName": TABLE,
            "AttributeDefinitions": [{ "AttributeName": "pk", "AttributeType": "S" }],
            "KeySchema": [{ "AttributeName": "pk", "KeyType": "HASH" }],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    let active = table_status(&router, "ACTIVE").await;
    assert_eq!(
        active, "ACTIVE",
        "setup: the table must reach ACTIVE, got {active:?}"
    );
    dynamodb(
        &router,
        "subject",
        "PutItem",
        &json!({ "TableName": TABLE, "Item": { "pk": { "S": "a-row" } } }),
    )
    .await;
    dynamodb(
        &router,
        "subject",
        "UpdateTimeToLive",
        &json!({
            "TableName": TABLE,
            "TimeToLiveSpecification": { "AttributeName": "expires", "Enabled": true },
        }),
    )
    .await;
    dynamodb(
        &router,
        "subject",
        "UpdateContinuousBackups",
        &json!({
            "TableName": TABLE,
            "PointInTimeRecoverySpecification": { "PointInTimeRecoveryEnabled": true },
        }),
    )
    .await;
    dynamodb(
        &router,
        "subject",
        "DeleteTable",
        &json!({ "TableName": TABLE }),
    )
    .await;

    // The rows expecting a delivery are read first: once each of them has its envelope, the
    // whole fan-out has run, so a row that must stay empty is read after it would have
    // arrived rather than before.
    let mut arrived: Vec<Vec<Value>> = Vec::new();
    for (row, url) in rows.iter().zip(queues.iter()) {
        let wanted = usize::from(row.expects.is_some());
        arrived.push(delivered(&router, row.case, url, wanted).await);
    }

    for ((row, url), delivery) in rows.iter().zip(queues.iter()).zip(arrived.iter()) {
        let case = row.case;
        let Some((source, event_name)) = row.expects else {
            assert!(
                delivery.is_empty(),
                "case {case:?}: expected no delivery on {url}, got {delivery:?}"
            );
            continue;
        };
        assert_eq!(
            delivery.len(),
            1,
            "case {case:?}: expected exactly one delivered envelope, got {delivery:?}"
        );
        let envelope = &delivery[0];
        assert_eq!(
            envelope["source"],
            json!(source),
            "case {case:?}: expected source {source:?}, got {envelope}"
        );
        assert_eq!(
            envelope["detail-type"],
            json!(API_CALL_DETAIL_TYPE),
            "case {case:?}: expected detail-type {API_CALL_DETAIL_TYPE:?}, got {envelope}"
        );
        assert_eq!(
            envelope["account"],
            json!(ACCOUNT),
            "case {case:?}: expected account {ACCOUNT:?}, got {envelope}"
        );
        assert_eq!(
            envelope["region"],
            json!(REGION),
            "case {case:?}: expected region {REGION:?}, got {envelope}"
        );
        let detail = &envelope["detail"];
        assert_eq!(
            detail["eventName"],
            json!(event_name),
            "case {case:?}: expected detail.eventName {event_name:?}, got {detail}"
        );
        assert_eq!(
            detail["awsRegion"],
            json!(REGION),
            "case {case:?}: expected detail.awsRegion {REGION:?}, got {detail}"
        );
        assert_eq!(
            detail["recipientAccountId"],
            json!(ACCOUNT),
            "case {case:?}: expected detail.recipientAccountId {ACCOUNT:?}, got {detail}"
        );
        assert_eq!(
            detail["readOnly"],
            json!(false),
            "case {case:?}: a write must be recorded readOnly=false, got {detail}"
        );
        assert_eq!(
            detail["eventCategory"],
            json!("Management"),
            "case {case:?}: expected a management record, got {detail}"
        );
    }

    // What each served call's record carries, from the one envelope that reached its row:
    // the parameters the call was dispatched with.
    let tagged = &arrived[0][0]["detail"];
    assert_eq!(
        tagged["eventSource"],
        json!(SQS_RECORD_SOURCE),
        "the control write's record must name the serving service, got {tagged}"
    );
    assert_eq!(
        tagged["requestParameters"]["queueUrl"],
        json!(control_url),
        "the control write's record must carry the queue it was dispatched with, got {tagged}"
    );
    for (case, index) in [("the create", 1), ("the delete", 2)] {
        let record = &arrived[index][0]["detail"];
        assert_eq!(
            record["eventSource"],
            json!(DYNAMODB_RECORD_SOURCE),
            "case {case:?}: the record must name the serving service, got {record}"
        );
        assert_eq!(
            record["requestParameters"]["tableName"],
            json!(TABLE),
            "case {case:?}: the record must carry the table it was dispatched with, got {record}"
        );
    }
}
