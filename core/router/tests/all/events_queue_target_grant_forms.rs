// SPDX-License-Identifier: CC0-1.0

//! WHICH grants on an Amazon SQS queue let an EventBridge rule deliver to it — the document
//! that decides whether a `AWS API Call via CloudTrail` event a rule matched ever reaches its
//! queue.
//!
//! RULE: the delivery to a queue target is authorized by the documents in force over that
//! QUEUE — "for Lambda functions, Amazon SNS topics and Amazon SQS queues, EventBridge relies
//! on resource-based policies" — read as IAM reads any resource policy: the delivery is made
//! as the service principal `events.amazonaws.com`, so it is admitted by every statement that
//! allows that principal the send on that queue and refused only where no statement allows it
//! or one explicitly denies it. How the statement is WRITTEN decides nothing: a service
//! principal given as a string or as a one-element list, an action or a resource given as a
//! list, a wildcard, the action's service prefix in any letter case (IAM action matching is
//! case-insensitive), a `Statement` given as one object rather than a list, and a condition
//! scoping the grant to the invoking rule — by `aws:SourceArn` or `aws:SourceAccount` — are
//! all the same grant.
//!
//! Only a cross-service suite can pin this: the event is published by the service that SERVED
//! the call (IAM over awsQuery), the delivery decision belongs to CloudTrail (awsJson1_1), the
//! match and the hand-over belong to EventBridge (awsJson1_1) and the document that admits the
//! hand-over belongs to Amazon SQS (awsJson1_0) — over ONE engine, as `core/router/src/lib.rs`
//! mounts them.
//!
//! The rows span the forms a client writes that one grant in, each on its own rule and its own
//! queue so what arrived at one is never read as another's, beside the three documents that
//! grant nothing: no policy at all, a grant scoped to another rule, and an explicit deny
//! beside the grant.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region everything below lives in: where a GLOBAL service's management events are
/// filed, so the Region whose default bus carries them.
const REGION: &str = "us-east-1";

/// The account every emulated resource belongs to (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// IAM's API version, which every awsQuery form body of it carries.
const IAM_API_VERSION: &str = "2010-05-08";

/// The `detail-type` CloudTrail's management-event fan-out carries
/// (`service_common::api_call_events::API_CALL_DETAIL_TYPE`).
const API_CALL_DETAIL_TYPE: &str = "AWS API Call via CloudTrail";

/// The `source` an event published for an IAM call carries.
const IAM_EVENT_SOURCE: &str = "aws.iam";

/// The call every rule below is pinned to, and the role it is made on.
const EVENT_NAME: &str = "CreateRole";
const ROLE: &str = "queue-target-grant-forms";

/// The input transformation every target below delivers through, so what arrives is the whole
/// event beside the rule that matched it. The two placeholders are EventBridge's own reserved
/// fills.
const INPUT_TEMPLATE: &str =
    r#"{"rule": "<aws.events.rule-name>", "event": <aws.events.event.json>}"#;

/// A trust policy for the role below, written so it carries nothing an
/// `application/x-www-form-urlencoded` body treats specially.
const TRUST_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"AWS":"arn:aws:iam::000000000000:root"},"Action":"sts:AssumeRole"}]}"#
);

/// The service principal a delivery to a target is made as.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// How long a delivery is waited for. Generous: the suite waits for the CONDITION (the
/// envelope arriving on the queue), and this is only the point at which waiting longer cannot
/// help.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(60);

/// How long between two looks at a queue while waiting for one.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The services this fan-out runs through, over ONE shared engine.
///
/// Amazon S3 is mounted beside them because a trail's destination bucket is resolved in the
/// account's object store: the trail is created the way a client creates one, against a bucket
/// that really exists.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signing is not the subject here, so requests are unauthenticated; the credential scope
    // is still carried, because it is what names the Region.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudtrail_cloudtrail_20131101::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
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

/// One IAM awsQuery form POST, as its status and body text.
async fn iam(router: &MultiServiceRouter, body: String) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("iam.{REGION}.amazonaws.com"))
        .header("authorization", authorization("iam"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(format!("Version={IAM_API_VERSION}&{body}")))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// `PUT /<bucket>` plus `PUT /<bucket>?policy` — the bucket the trail delivers its log files
/// into, carrying the bucket policy that lets CloudTrail write into it.
async fn create_bucket(router: &MultiServiceRouter, bucket: &str) {
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("/{bucket}"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("content-type", "application/xml")
        .header("authorization", authorization("s3"))
        .body(Bytes::new())
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

/// Receive up to ten messages from `url`, draining them, as their parsed bodies.
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
/// [`DELIVERY_DEADLINE`] has passed — whichever comes first. The CONDITION is what is waited
/// for; the deadline only bounds a wait that will never be satisfied.
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

/// One grant form this suite installs on a queue target.
struct Row {
    case: &'static str,
    /// The document the queue carries, built from the queue's own ARN and the ARN of the rule
    /// that delivers to it — or `None` for a queue carrying no policy at all.
    policy: fn(&str, &str) -> Option<Value>,
    /// Whether the delivery this document decides is made.
    admits: bool,
}

/// The grant forms, each a document a client writes for an EventBridge queue target.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the form AWS's own example writes",
            policy: |queue, rule| {
                Some(json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Sid": "AllowEventBridge",
                        "Effect": "Allow",
                        "Principal": { "Service": EVENTS_PRINCIPAL },
                        "Action": "sqs:SendMessage",
                        "Resource": queue,
                        "Condition": { "ArnEquals": { "aws:SourceArn": rule } },
                    }],
                }))
            },
            admits: true,
        },
        Row {
            case: "a grant carrying no condition",
            policy: |queue, _| {
                Some(json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": EVENTS_PRINCIPAL },
                        "Action": "sqs:SendMessage",
                        "Resource": queue,
                    }],
                }))
            },
            admits: true,
        },
        Row {
            case: "the service principal as a one-element list",
            policy: |queue, rule| {
                Some(json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": [EVENTS_PRINCIPAL] },
                        "Action": ["sqs:SendMessage"],
                        "Resource": [queue],
                        "Condition": { "ArnEquals": { "aws:SourceArn": rule } },
                    }],
                }))
            },
            admits: true,
        },
        Row {
            case: "the action's service prefix in capitals, as the console writes it",
            policy: |queue, rule| {
                Some(json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": EVENTS_PRINCIPAL },
                        "Action": "SQS:SendMessage",
                        "Resource": queue,
                        "Condition": { "ArnEquals": { "aws:SourceArn": rule } },
                    }],
                }))
            },
            admits: true,
        },
        Row {
            case: "a wildcard action over the queue",
            policy: |queue, _| {
                Some(json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": EVENTS_PRINCIPAL },
                        "Action": "sqs:*",
                        "Resource": queue,
                    }],
                }))
            },
            admits: true,
        },
        Row {
            case: "a grant over every resource the document is attached to",
            policy: |_, rule| {
                Some(json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": EVENTS_PRINCIPAL },
                        "Action": "sqs:SendMessage",
                        "Resource": "*",
                        "Condition": { "ArnEquals": { "aws:SourceArn": rule } },
                    }],
                }))
            },
            admits: true,
        },
        Row {
            case: "a condition over the account's rules by wildcard",
            policy: |queue, _| {
                Some(json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": EVENTS_PRINCIPAL },
                        "Action": "sqs:SendMessage",
                        "Resource": queue,
                        "Condition": {
                            "ArnLike": {
                                "aws:SourceArn": format!("arn:aws:events:{REGION}:{ACCOUNT}:rule/*"),
                            },
                        },
                    }],
                }))
            },
            admits: true,
        },
        Row {
            case: "a condition on the account the rule belongs to",
            policy: |queue, _| {
                Some(json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": EVENTS_PRINCIPAL },
                        "Action": "sqs:SendMessage",
                        "Resource": queue,
                        "Condition": { "StringEquals": { "aws:SourceAccount": ACCOUNT } },
                    }],
                }))
            },
            admits: true,
        },
        Row {
            case: "every principal, scoped to the one rule",
            policy: |queue, rule| {
                Some(json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": "*",
                        "Action": "sqs:SendMessage",
                        "Resource": queue,
                        "Condition": { "ArnEquals": { "aws:SourceArn": rule } },
                    }],
                }))
            },
            admits: true,
        },
        Row {
            case: "one statement written as an object rather than a list",
            policy: |queue, rule| {
                Some(json!({
                    "Version": "2012-10-17",
                    "Id": "queue-target-grant",
                    "Statement": {
                        "Effect": "Allow",
                        "Principal": { "Service": EVENTS_PRINCIPAL },
                        "Action": "sqs:SendMessage",
                        "Resource": queue,
                        "Condition": { "ArnEquals": { "aws:SourceArn": rule } },
                    },
                }))
            },
            admits: true,
        },
        Row {
            case: "no document on the queue at all",
            policy: |_, _| None,
            admits: false,
        },
        Row {
            case: "a grant scoped to another rule",
            policy: |queue, _| {
                Some(json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": EVENTS_PRINCIPAL },
                        "Action": "sqs:SendMessage",
                        "Resource": queue,
                        "Condition": {
                            "ArnEquals": {
                                "aws:SourceArn":
                                    format!("arn:aws:events:{REGION}:{ACCOUNT}:rule/somebody-else"),
                            },
                        },
                    }],
                }))
            },
            admits: false,
        },
        Row {
            case: "an explicit deny beside the grant",
            policy: |queue, rule| {
                Some(json!({
                    "Version": "2012-10-17",
                    "Statement": [
                        {
                            "Effect": "Allow",
                            "Principal": { "Service": EVENTS_PRINCIPAL },
                            "Action": "sqs:SendMessage",
                            "Resource": queue,
                            "Condition": { "ArnEquals": { "aws:SourceArn": rule } },
                        },
                        {
                            "Effect": "Deny",
                            "Principal": { "Service": EVENTS_PRINCIPAL },
                            "Action": "sqs:SendMessage",
                            "Resource": queue,
                        },
                    ],
                }))
            },
            admits: false,
        },
    ]
}

#[tokio::test]
async fn every_form_of_the_queue_grant_admits_the_rules_delivery() {
    let router = router();

    // A trail of the account, logging the WRITE management events of every Region and the
    // partition's global services with them, so each IAM write is published onto the default
    // bus of the Region its record is filed in.
    create_bucket(&router, "queue-target-grant-forms-trail").await;
    cloudtrail(
        &router,
        "setup",
        "CreateTrail",
        &json!({
            "Name": "queue-target-grant-forms",
            "S3BucketName": "queue-target-grant-forms-trail",
            "IsMultiRegionTrail": true,
            "IncludeGlobalServiceEvents": true,
        }),
    )
    .await;
    cloudtrail(
        &router,
        "setup",
        "PutEventSelectors",
        &json!({
            "TrailName": "queue-target-grant-forms",
            "EventSelectors": [{ "ReadWriteType": "WriteOnly", "IncludeManagementEvents": true }],
        }),
    )
    .await;
    cloudtrail(
        &router,
        "setup",
        "StartLogging",
        &json!({ "Name": "queue-target-grant-forms" }),
    )
    .await;
    let status = cloudtrail(
        &router,
        "setup",
        "GetTrailStatus",
        &json!({ "Name": "queue-target-grant-forms" }),
    )
    .await;
    assert_eq!(
        status["IsLogging"],
        json!(true),
        "setup: the trail must be logging, got {status}"
    );

    // One rule per row, each on its own queue, all on the DEFAULT bus and all pinned to the
    // same call — so the one write below is matched by every rule and what decides each
    // delivery is the row's own document.
    let rows = rows();
    let mut queues = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let queue = format!("queue-target-grant-sink-{index}");
        let created = sqs(
            &router,
            row.case,
            "CreateQueue",
            &json!({ "QueueName": queue }),
        )
        .await;
        let url = created["QueueUrl"]
            .as_str()
            .unwrap_or_else(|| panic!("case {:?}: expected a QueueUrl, got {created}", row.case))
            .to_string();
        let attributes = sqs(
            &router,
            row.case,
            "GetQueueAttributes",
            &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
        )
        .await;
        let queue_arn = attributes["Attributes"]["QueueArn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected a QueueArn, got {attributes}",
                    row.case
                )
            })
            .to_string();
        let rule = format!("queue-target-grant-rule-{index}");
        let put_rule = events(
            &router,
            row.case,
            "PutRule",
            &json!({
                "Name": rule,
                "EventBusName": "default",
                "State": "ENABLED",
                "EventPattern": json!({
                    "source": [IAM_EVENT_SOURCE],
                    "detail-type": [API_CALL_DETAIL_TYPE],
                    "detail": { "eventName": [EVENT_NAME] },
                })
                .to_string(),
            }),
        )
        .await;
        let rule_arn = put_rule["RuleArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {:?}: expected a RuleArn, got {put_rule}", row.case))
            .to_string();
        let put_targets = events(
            &router,
            row.case,
            "PutTargets",
            &json!({
                "Rule": rule,
                "EventBusName": "default",
                "Targets": [{
                    "Id": "sink",
                    "Arn": queue_arn,
                    "InputTransformer": { "InputTemplate": INPUT_TEMPLATE },
                }],
            }),
        )
        .await;
        assert_eq!(
            put_targets["FailedEntryCount"],
            json!(0),
            "case {:?}: the target must be installed, got {put_targets}",
            row.case
        );
        if let Some(policy) = (row.policy)(&queue_arn, &rule_arn) {
            sqs(
                &router,
                row.case,
                "SetQueueAttributes",
                &json!({
                    "QueueUrl": url,
                    "Attributes": { "Policy": policy.to_string() },
                }),
            )
            .await;
        }
        queues.push((url, rule));
    }

    // The one write every rule matches.
    let (status, body) = iam(
        &router,
        format!("Action=CreateRole&RoleName={ROLE}&AssumeRolePolicyDocument={TRUST_POLICY}"),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole must succeed, got {status}: {body}"
    );

    // The rows expecting a delivery are read first: once each of them has its envelope, the
    // whole fan-out has run, so a row that must stay empty is read after it would have arrived
    // rather than before.
    let mut arrived: Vec<Vec<Value>> = Vec::new();
    for (row, (url, _)) in rows.iter().zip(queues.iter()) {
        let wanted = usize::from(row.admits);
        arrived.push(delivered(&router, row.case, url, wanted).await);
    }

    for ((row, (_, rule)), delivery) in rows.iter().zip(queues.iter()).zip(arrived.iter()) {
        let case = row.case;
        if !row.admits {
            assert!(
                delivery.is_empty(),
                "case {case:?}: this document grants the delivery nothing, so the queue must \
                 stay empty, got {delivery:?}"
            );
            continue;
        }
        assert_eq!(
            delivery.len(),
            1,
            "case {case:?}: expected exactly one delivered message, got {delivery:?}"
        );
        assert_eq!(
            delivery[0]["rule"],
            json!(rule),
            "case {case:?}: expected the matched rule {rule:?}, got {}",
            delivery[0]
        );
        let envelope = &delivery[0]["event"];
        assert_eq!(
            envelope["source"],
            json!(IAM_EVENT_SOURCE),
            "case {case:?}: expected source {IAM_EVENT_SOURCE:?}, got {envelope}"
        );
        assert_eq!(
            envelope["detail-type"],
            json!(API_CALL_DETAIL_TYPE),
            "case {case:?}: expected detail-type {API_CALL_DETAIL_TYPE:?}, got {envelope}"
        );
        assert_eq!(
            envelope["detail"]["eventName"],
            json!(EVENT_NAME),
            "case {case:?}: expected detail.eventName {EVENT_NAME:?}, got {envelope}"
        );
        assert_eq!(
            envelope["detail"]["requestParameters"]["roleName"],
            json!(ROLE),
            "case {case:?}: the record must carry the role the call named, got {envelope}"
        );
    }
}
