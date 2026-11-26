// SPDX-License-Identifier: CC0-1.0

//! The CloudTrail management-event fan-out for the trail a client wires up the way AWS
//! DOCUMENTS it: a single-Region trail that logs no global-service events, its destination
//! bucket carrying the documented trail policy (`s3:GetBucketAcl` on the bucket and
//! `s3:PutObject` under `AWSLogs/<account-id>/` with the bucket-owner-full-control
//! condition), its selectors configured by the trail's own ARN rather than by its name.
//!
//! RULE: while a trail of the Region is logging management events of a call's read/write
//! class, every control-plane call the account serves — whichever service serves it — is ALSO
//! published onto that Region's DEFAULT event bus, with `source` `aws.<endpoint-prefix>` of
//! the serving service, `detail-type` `AWS API Call via CloudTrail` and the CloudTrail record
//! of the call as `detail`; a rule whose pattern names `source`, `detail-type`,
//! `detail.eventSource` and `detail.eventName` is handed exactly the events that match it and
//! nothing else. HOW the trail was addressed and written — by name or by ARN, homed in one
//! Region or every one, with or without global-service events, under whichever destination
//! policy admits the delivery — decides nothing about it, and neither does the trail's own
//! S3 leg, which carries the same record into the bucket.
//!
//! The sibling suites pin the same channel for a trail created with its defaults
//! (`all/cloudtrail_dynamodb_api_call_events` in process,
//! `all/cloudtrail_api_call_events_on_a_started_emulator` against the started binary). This
//! one is the wiring a client writes from CloudTrail's own documentation, which differs from
//! those in every fact above: the rows below span which service served the call (a control
//! plane with bus events of its own, and one with none), the event NAME a pattern pins (two
//! writes of ONE service, each reaching only its own rule and never the other's), and the
//! sink of the serving service, which holds none of the other service's events.
//!
//! Only a cross-service suite can pin it: the events are published by the services that
//! SERVED the calls (Amazon SQS and Amazon DynamoDB over awsJson1_0), the delivery decision
//! belongs to CloudTrail (awsJson1_1), the destination bucket and the log-file leg belong to
//! Amazon S3, the match and the hand-over belong to EventBridge (awsJson1_1) and what arrived
//! is read back from Amazon SQS — over ONE engine, as `core/router/src/lib.rs` mounts them.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call of this suite is served in, and the Region the trail is homed in:
/// NOT the partition's leader Region, so a single-Region trail that logs no global-service
/// events covers these calls by being homed where they are served and by nothing else.
const REGION: &str = "us-west-2";

/// The account every emulated resource belongs to (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// The `detail-type` CloudTrail's management-event fan-out carries. Spelled out here rather
/// than imported from a service crate, so a wrong constant in the implementation cannot make
/// this suite pass.
const API_CALL_DETAIL_TYPE: &str = "AWS API Call via CloudTrail";

/// The `source` and the record `eventSource` of each service whose calls are published here.
const SQS_EVENT_SOURCE: &str = "aws.sqs";
const SQS_RECORD_SOURCE: &str = "sqs.amazonaws.com";
const DYNAMODB_EVENT_SOURCE: &str = "aws.dynamodb";
const DYNAMODB_RECORD_SOURCE: &str = "dynamodb.amazonaws.com";

/// The trail, the bucket it delivers into, and the table the subject writes create and
/// remove.
const TRAIL: &str = "documented-trail-setup";
const BUCKET: &str = "documented-trail-setup-logs";
const TABLE: &str = "documented-trail-setup-subject";

/// How long a delivery is waited for, and how long between two looks at a queue or at a
/// table's status. Generous: the suite waits for the CONDITION, and the deadline is only the
/// point at which waiting longer cannot help.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The services this fan-out runs through, over ONE shared engine, as the emulator mounts
/// them.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // Signing is not the subject here, so requests are unauthenticated; the credential scope
    // is still carried, because it is what names the Region.
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

/// A CloudTrail call (`awsJson1_1`) that must answer 200.
async fn cloudtrail(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) =
        rpc(router, "cloudtrail", "CloudTrail_20131101", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudTrail {operation}, got {status} with {parsed}"
    );
    parsed
}

/// An EventBridge call (`awsJson1_1`) that must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "events", "AWSEvents", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with {parsed}"
    );
    parsed
}

/// An Amazon SQS call (`awsJson1_0`) that must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "sqs", "AmazonSQS", "1.0", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// An Amazon DynamoDB call (`awsJson1_0`) that must answer 200.
async fn dynamodb(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) =
        rpc(router, "dynamodb", "DynamoDB_20120810", "1.0", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon DynamoDB {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// The destination bucket, carrying the policy CloudTrail's own documentation gives for a
/// trail: `s3:GetBucketAcl` on the bucket, and `s3:PutObject` on the keys under
/// `AWSLogs/<account-id>/` with the bucket-owner-full-control condition — the grant a client
/// writes, which is narrower than one on the whole bucket.
async fn create_bucket(router: &MultiServiceRouter) {
    // Outside the leader Region a create must name the Region it is made in
    // ("IllegalLocationConstraintException" otherwise), which is what this body carries.
    let configuration = format!(
        "<CreateBucketConfiguration><LocationConstraint>{REGION}</LocationConstraint>\
         </CreateBucketConfiguration>"
    );
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("/{BUCKET}"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("content-type", "application/xml")
        .header("authorization", authorization("s3"))
        .body(Bytes::from(configuration))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    assert_eq!(
        response.status().as_u16(),
        200,
        "setup: CreateBucket({BUCKET:?}) must succeed, got {} with {:?}",
        response.status(),
        String::from_utf8_lossy(response.body())
    );

    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Sid": "AWSCloudTrailAclCheck",
                "Effect": "Allow",
                "Principal": { "Service": "cloudtrail.amazonaws.com" },
                "Action": "s3:GetBucketAcl",
                "Resource": format!("arn:aws:s3:::{BUCKET}"),
            },
            {
                "Sid": "AWSCloudTrailWrite",
                "Effect": "Allow",
                "Principal": { "Service": "cloudtrail.amazonaws.com" },
                "Action": "s3:PutObject",
                "Resource": format!("arn:aws:s3:::{BUCKET}/AWSLogs/{ACCOUNT}/*"),
                "Condition": {
                    "StringEquals": { "s3:x-amz-acl": "bucket-owner-full-control" },
                },
            },
        ],
    })
    .to_string();
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("/{BUCKET}?policy"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("s3"))
        .body(Bytes::from(policy))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    assert!(
        (200..300).contains(&response.status().as_u16()),
        "setup: PutBucketPolicy({BUCKET:?}) must succeed, got {} with {:?}",
        response.status(),
        String::from_utf8_lossy(response.body())
    );
}

/// How many objects the bucket now holds under the trail's own log-file prefix.
async fn delivered_log_files(router: &MultiServiceRouter) -> usize {
    let prefix = format!("AWSLogs/{ACCOUNT}/CloudTrail/{REGION}/");
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("/{BUCKET}?list-type=2&prefix={prefix}"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let body = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        response.status().as_u16(),
        200,
        "the trail's bucket must be listable, got {} with {body:?}",
        response.status()
    );
    body.matches("<Key>").count()
}

/// Create a queue and hand back its url and its ARN, as the account's own store reports them.
async fn create_queue(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let created = sqs(router, name, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: expected a QueueUrl for {name:?}, got {created}"))
        .to_string();
    let attributes = sqs(
        router,
        name,
        "GetQueueAttributes",
        &json!({ "QueueUrl": &url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: expected a QueueArn for {name:?}, got {attributes}"))
        .to_string();
    (url, arn)
}

/// Let `rule_arn` — and only it — send to the queue at `url`: the resource policy a client
/// writes for an EventBridge target, naming the service principal the delivery is made as and
/// scoping it to the rule that makes it.
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
            "Sid": "AllowEventBridge",
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

/// Receive up to ten messages from `url`, draining them, as their parsed bodies — which for a
/// queue target carrying no input transformation is the whole event envelope.
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
/// [`DELIVERY_DEADLINE`] has passed — whichever comes first, and then ONCE more. The CONDITION
/// is what is waited for; the deadline only bounds a wait that will never be satisfied.
///
/// The extra look is what lets a row read "exactly one envelope": a second envelope for the
/// same rule would arrive a moment after the first, so a drain that stopped at the one it was
/// waiting for could not tell one delivery from two.
async fn delivered(router: &MultiServiceRouter, case: &str, url: &str, wanted: usize) -> Vec<Value> {
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

/// Look at the table's status until it reports `wanted`, or [`DELIVERY_DEADLINE`] has passed.
/// A create reaches `ACTIVE` on its own clock, so the CONDITION is waited for.
async fn table_status(router: &MultiServiceRouter, wanted: &str) -> String {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let described =
            dynamodb(router, "setup", "DescribeTable", &json!({ "TableName": TABLE })).await;
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

/// One rule this suite installs: the pattern it matches with, and the `source` and
/// `eventName` of the ONE record that must reach it.
struct Row {
    case: &'static str,
    pattern: Value,
    expects: (&'static str, &'static str),
}

/// The rows, each on its own queue so what arrived at one is never read as another's.
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
            expects: (SQS_EVENT_SOURCE, "TagQueue"),
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
            expects: (DYNAMODB_EVENT_SOURCE, "CreateTable"),
        },
        Row {
            case: "the near-miss rule of that same service, pinned to its delete",
            pattern: json!({
                "source": [DYNAMODB_EVENT_SOURCE],
                "detail-type": [API_CALL_DETAIL_TYPE],
                "detail": {
                    "eventSource": [DYNAMODB_RECORD_SOURCE],
                    "eventName": ["DeleteTable"],
                },
            }),
            expects: (DYNAMODB_EVENT_SOURCE, "DeleteTable"),
        },
    ]
}

#[tokio::test]
async fn api_call_events_reach_their_rules_for_a_trail_wired_as_aws_documents_it() {
    let router = router();

    // A trail of this Region alone, logging no global-service events, delivering into a
    // bucket that carries the documented trail policy.
    create_bucket(&router).await;
    let created = cloudtrail(
        &router,
        "setup",
        "CreateTrail",
        &json!({
            "Name": TRAIL,
            "S3BucketName": BUCKET,
            "IsMultiRegionTrail": false,
            "IncludeGlobalServiceEvents": false,
        }),
    )
    .await;
    let trail_arn = created["TrailARN"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: expected a TrailARN, got {created}"))
        .to_string();
    assert_eq!(
        trail_arn,
        format!("arn:aws:cloudtrail:{REGION}:{ACCOUNT}:trail/{TRAIL}"),
        "setup: the trail must be homed in the Region it was created in, got {created}"
    );

    // The selectors, the switch and the status are all addressed by the trail's own ARN —
    // "the ARN or name of the trail", so the same trail as the name addresses.
    let selectors = cloudtrail(
        &router,
        "setup",
        "PutEventSelectors",
        &json!({
            "TrailName": &trail_arn,
            "EventSelectors": [{ "ReadWriteType": "WriteOnly", "IncludeManagementEvents": true }],
        }),
    )
    .await;
    assert_eq!(
        selectors["EventSelectors"][0]["ReadWriteType"],
        json!("WriteOnly"),
        "setup: the selectors must be stored as written, got {selectors}"
    );
    assert_eq!(
        selectors["EventSelectors"][0]["IncludeManagementEvents"],
        json!(true),
        "setup: the selectors must include management events, got {selectors}"
    );
    cloudtrail(&router, "setup", "StartLogging", &json!({ "Name": &trail_arn })).await;
    let status = cloudtrail(
        &router,
        "setup",
        "GetTrailStatus",
        &json!({ "Name": &trail_arn }),
    )
    .await;
    assert_eq!(
        status["IsLogging"],
        json!(true),
        "setup: the trail must report itself logging, got {status}"
    );

    // One rule per row, each on its own queue, all on the DEFAULT bus — the only bus an
    // AWS-published event reaches. No input transformation, so what a consumer reads is the
    // whole envelope.
    let rows = rows();
    let mut queues = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let name = format!("documented-trail-setup-sink-{index}");
        let (url, queue_arn) = create_queue(&router, &name).await;
        let rule = format!("documented-trail-setup-rule-{index}");
        let created = events(
            &router,
            row.case,
            "PutRule",
            &json!({ "Name": &rule, "EventPattern": row.pattern.to_string() }),
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
            &json!({ "Rule": &rule, "Targets": [{ "Id": "sink", "Arn": &queue_arn }] }),
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

    // The control write: a tag written on the control rule's OWN sink, which is a queue the
    // account holds and a call it really served.
    sqs(
        &router,
        "control",
        "TagQueue",
        &json!({ "QueueUrl": &queues[0], "Tags": { "owner": "documented-trail-setup" } }),
    )
    .await;

    // Then the two control-plane writes of the service that has no bus events of its own.
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
    dynamodb(&router, "subject", "DeleteTable", &json!({ "TableName": TABLE })).await;

    let mut arrived: Vec<Vec<Value>> = Vec::new();
    for (row, url) in rows.iter().zip(queues.iter()) {
        arrived.push(delivered(&router, row.case, url, 1).await);
    }

    for ((row, url), delivery) in rows.iter().zip(queues.iter()).zip(arrived.iter()) {
        let case = row.case;
        let (source, event_name) = row.expects;
        assert_eq!(
            delivery.len(),
            1,
            "case {case:?}: expected exactly one delivered envelope on {url}, got {delivery:?}"
        );
        let envelope = &delivery[0];
        let record_source = if source == SQS_EVENT_SOURCE {
            SQS_RECORD_SOURCE
        } else {
            DYNAMODB_RECORD_SOURCE
        };
        for (pointer, expected) in [
            ("/source", json!(source)),
            ("/detail-type", json!(API_CALL_DETAIL_TYPE)),
            ("/account", json!(ACCOUNT)),
            ("/region", json!(REGION)),
            ("/detail/eventSource", json!(record_source)),
            ("/detail/eventName", json!(event_name)),
            ("/detail/awsRegion", json!(REGION)),
            ("/detail/recipientAccountId", json!(ACCOUNT)),
            ("/detail/readOnly", json!(false)),
            ("/detail/eventCategory", json!("Management")),
        ] {
            let actual = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {pointer} to be {expected}, got {actual} in {envelope}"
            );
        }
    }

    // What each record carries of the call it was made for: the parameters that call was
    // dispatched with, taken from the request and not from any fixed text.
    let control = &arrived[0][0]["detail"];
    assert_eq!(
        control["requestParameters"]["queueUrl"],
        json!(&queues[0]),
        "the control write's record must carry the queue it was dispatched with, got {control}"
    );
    for (case, index) in [("the create", 1), ("the delete", 2)] {
        let record = &arrived[index][0]["detail"];
        assert_eq!(
            record["requestParameters"]["tableName"],
            json!(TABLE),
            "case {case:?}: the record must carry the table it was dispatched with, got {record}"
        );
    }

    // The isolation the rows are about: no sink holds another row's event name, and the sink
    // of the service with its own bus events holds nothing of the other service.
    for ((row, delivery), url) in rows.iter().zip(arrived.iter()).zip(queues.iter()) {
        let case = row.case;
        let (source, event_name) = row.expects;
        let strangers: Vec<&Value> = delivery
            .iter()
            .filter(|envelope| {
                envelope["source"] != json!(source)
                    || envelope["detail"]["eventName"] != json!(event_name)
            })
            .collect();
        assert!(
            strangers.is_empty(),
            "case {case:?}: {url} must hold only {source} {event_name} envelopes, \
             got {strangers:?}"
        );
    }

    // The trail's own S3 leg, under the documented destination policy: the same records
    // CloudTrail published are delivered into the bucket the trail was created with.
    let files = delivered_log_files(&router).await;
    assert!(
        files > 0,
        "the logging trail must have delivered log files under its own prefix, got {files}"
    );
}
