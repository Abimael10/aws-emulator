// SPDX-License-Identifier: CC0-1.0

//! The JOIN between the answer a control-plane call gave its caller and the management event
//! published for that call, and the condition the publication hangs off.
//!
//! RULE: the CloudTrail record carried as `detail` by an `AWS API Call via CloudTrail` event
//! names the request its own CALLER was answered under — `detail.requestID` is the id that
//! very response reported — and carries the parameters that call was dispatched with, so a
//! client holding a response can find the event of THAT call rather than guessing which of
//! two records of one operation is its own; and no event is published at all unless a trail of
//! the account is logging calls of that class.
//!
//! The sibling suite `all/cloudtrail_dynamodb_api_call_events` spans WHICH rules such an event
//! reaches — the envelope fields a pattern may pin, the read/write class a trail's selector
//! admits, the per-item data plane that is no management event — and
//! `all/cloudtrail_api_call_events_on_a_started_emulator` pins the channel on the emulator a
//! client starts. Neither reads the id the caller was answered under, and neither drives the
//! account with no trail logging: those two are this suite's subject, and both are properties
//! of the publication rather than of any rule.
//!
//! Only a cross-service suite can pin it: the id is minted by the service that SERVED the call
//! (Amazon DynamoDB, awsJson1_0) as it renders that caller's own response, the delivery
//! decision belongs to CloudTrail (awsJson1_1), the match and the hand-over belong to
//! EventBridge (awsJson1_1) and what arrived is read back from Amazon SQS — four services over
//! ONE engine, as `core/router/src/lib.rs` mounts them.
//!
//! The rows span the control-plane calls of one service rather than the one call that
//! motivated the suite: a create, a tag write and a delete, each on its own rule and queue, so
//! the id asserted for one is never another call's.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call below is served in, and the Region the trail is homed in.
const REGION: &str = "us-west-2";

/// The account every emulated resource belongs to (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// The `detail-type` CloudTrail's management-event fan-out carries, and the `source` an event
/// published for a DynamoDB call carries. Spelled out here rather than imported from a service
/// crate, so a wrong constant in the implementation cannot make this suite pass.
const API_CALL_DETAIL_TYPE: &str = "AWS API Call via CloudTrail";
const DYNAMODB_EVENT_SOURCE: &str = "aws.dynamodb";

/// DynamoDB's `X-Amz-Target` prefix (`awsJson1_0`).
const DYNAMODB_TARGET_PREFIX: &str = "DynamoDB_20120810";

/// The header a response names the request it answers in, which is what a client reads the
/// call's own id off (`x-amzn-requestid`).
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// How long a delivery is waited for. Generous: the suite waits for the CONDITION (the
/// envelope arriving on the queue), and this is only the point at which waiting longer cannot
/// help.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(120);

/// How long between two looks at a queue, or at a table's status, while waiting for one.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The table the control-plane calls below create, tag and delete.
const TABLE: &str = "api-call-request-id-join";

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

/// What one call answered: its status, its parsed body and the id the CALLER was answered
/// under.
struct Answered {
    status: u16,
    body: Value,
    request_id: String,
}

/// One `X-Amz-Target` RPC call.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> Answered {
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
    let request_id = response
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let status = response.status().as_u16();
    let body = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    Answered {
        status,
        body,
        request_id,
    }
}

/// One EventBridge call (`awsJson1_1`) that must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let answered = rpc(router, "events", "AWSEvents", "1.1", operation, body).await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {} with {}",
        answered.status, answered.body
    );
    answered.body
}

/// One Amazon SQS call (`awsJson1_0`) that must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let answered = rpc(router, "sqs", "AmazonSQS", "1.0", operation, body).await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {} with {}",
        answered.status, answered.body
    );
    answered.body
}

/// One CloudTrail call (`awsJson1_1`) that must answer 200.
async fn cloudtrail(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let answered = rpc(
        router,
        "cloudtrail",
        "CloudTrail_20131101",
        "1.1",
        operation,
        body,
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from CloudTrail {operation}, got {} with {}",
        answered.status, answered.body
    );
    answered.body
}

/// One Amazon DynamoDB call (`awsJson1_0`) that must answer 200, as its body and the id its
/// own caller was answered under.
async fn dynamodb(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> (Value, String) {
    let answered = rpc(
        router,
        "dynamodb",
        DYNAMODB_TARGET_PREFIX,
        "1.0",
        operation,
        body,
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from Amazon DynamoDB {operation}, got {} with {}",
        answered.status, answered.body
    );
    assert!(
        !answered.request_id.is_empty(),
        "case {case:?}: the answer to {operation} must name the request it answers"
    );
    (answered.body, answered.request_id)
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

/// The rule `slug` on the default bus, pinned to one control-plane call of one service, with
/// its own queue admitting EventBridge's send on behalf of that rule and installed as its
/// target; answers the queue's url.
async fn wired(router: &MultiServiceRouter, slug: &str, event_name: &str) -> String {
    let created = sqs(router, slug, "CreateQueue", &json!({ "QueueName": slug })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: expected a QueueUrl for {slug:?}, got {created}"))
        .to_string();
    let queue_arn = format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{slug}");
    let pattern = json!({
        "source": [DYNAMODB_EVENT_SOURCE],
        "detail-type": [API_CALL_DETAIL_TYPE],
        "detail": { "eventName": [event_name] },
    });
    let put = events(
        router,
        slug,
        "PutRule",
        &json!({ "Name": slug, "EventPattern": pattern.to_string() }),
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

/// Look at the queue at `url` until one envelope has arrived, or [`DELIVERY_DEADLINE`] passes.
/// The CONDITION is what is waited for; the deadline only bounds a wait that will never be
/// satisfied.
async fn delivered(router: &MultiServiceRouter, case: &str, url: &str) -> Value {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        if let Some(envelope) = drained(router, case, url).await.into_iter().next() {
            return envelope;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: no envelope reached the queue inside {DELIVERY_DEADLINE:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The smallest `CreateTable` request Amazon DynamoDB accepts for `name`.
fn create_table_request(name: &str) -> Value {
    json!({
        "TableName": name,
        "KeySchema": [{ "AttributeName": "id", "KeyType": "HASH" }],
        "AttributeDefinitions": [{ "AttributeName": "id", "AttributeType": "S" }],
        "BillingMode": "PAY_PER_REQUEST",
    })
}

/// Poll `DescribeTable` until the table reports `wanted`, or [`DELIVERY_DEADLINE`] passes — a
/// table a delete may act on is one that is no longer being created.
async fn wait_for_table(router: &MultiServiceRouter, case: &str, wanted: &str) {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let (described, _) =
            dynamodb(router, case, "DescribeTable", &json!({ "TableName": TABLE })).await;
        let seen = described["Table"]["TableStatus"].clone();
        if seen == json!(wanted) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the table to report {wanted:?} inside \
             {DELIVERY_DEADLINE:?}, last saw {seen}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: the record each published event carries names the request its own caller was answered
/// under, and the parameters that call was dispatched with.
#[tokio::test]
async fn each_published_record_names_the_request_its_own_caller_was_answered_for() {
    let router = router();

    // A trail of the account, logging. Its selectors are the defaults a trail is created with,
    // which log every management call of the account.
    create_bucket(&router, "api-call-request-id-trail").await;
    cloudtrail(
        &router,
        "setup",
        "CreateTrail",
        &json!({
            "Name": "api-call-request-id",
            "S3BucketName": "api-call-request-id-trail",
        }),
    )
    .await;
    cloudtrail(
        &router,
        "setup",
        "StartLogging",
        &json!({ "Name": "api-call-request-id" }),
    )
    .await;

    // One rule per control-plane call below, each on its own queue so the id asserted for one
    // is never another call's.
    let create_sink = wired(&router, "api-call-request-id-create", "CreateTable").await;
    let tag_sink = wired(&router, "api-call-request-id-tag", "TagResource").await;
    let delete_sink = wired(&router, "api-call-request-id-delete", "DeleteTable").await;

    // The calls: a create, a tag write and a delete of one table, each answering its own
    // caller under its own id.
    let (created, create_id) = dynamodb(
        &router,
        "CreateTable",
        "CreateTable",
        &create_table_request(TABLE),
    )
    .await;
    let table_arn = created["TableDescription"]["TableArn"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a TableArn, got {created}"))
        .to_string();
    let (_, tag_id) = dynamodb(
        &router,
        "TagResource",
        "TagResource",
        &json!({
            "ResourceArn": &table_arn,
            "Tags": [{ "Key": "owner", "Value": "api-call-events" }],
        }),
    )
    .await;
    wait_for_table(&router, "the created table", "ACTIVE").await;
    let (_, delete_id) = dynamodb(
        &router,
        "DeleteTable",
        "DeleteTable",
        &json!({ "TableName": TABLE }),
    )
    .await;

    // Each call's own event carries the id that call's caller was answered under, and the
    // member that call addressed its resource with.
    for (case, sink, answered_as, parameter, expected) in [
        (
            "the create",
            &create_sink,
            &create_id,
            "tableName",
            json!(TABLE),
        ),
        (
            "the tag write",
            &tag_sink,
            &tag_id,
            "resourceArn",
            json!(&table_arn),
        ),
        (
            "the delete",
            &delete_sink,
            &delete_id,
            "tableName",
            json!(TABLE),
        ),
    ] {
        let envelope = delivered(&router, case, sink).await;
        let detail = &envelope["detail"];
        assert_eq!(
            detail["requestID"],
            json!(answered_as),
            "case {case:?}: expected the record of the call answered under {answered_as:?}, \
             got {detail}"
        );
        assert_eq!(
            detail["requestParameters"][parameter],
            expected,
            "case {case:?}: expected requestParameters.{parameter} {expected}, got {detail}"
        );
    }

    // The three ids are three calls' own: a record naming any of them names exactly one call.
    assert!(
        create_id != tag_id && tag_id != delete_id && create_id != delete_id,
        "the three calls must be answered under three ids, got {create_id:?}, {tag_id:?}, \
         {delete_id:?}"
    );
}

/// RULE: no trail logging, no publication — the condition the fan-out hangs off, driven with
/// the same rule and the same kind of call as above, in the two ways an account can have no
/// logging trail.
#[tokio::test]
async fn a_control_plane_call_publishes_nothing_while_no_trail_logs() {
    let router = router();
    let sink = wired(&router, "api-call-no-trail-create", "CreateTable").await;

    // Row 1: no trail of the account was ever created, so nothing the account serves is
    // logged — and the rule's own call publishes nothing.
    dynamodb(
        &router,
        "no trail at all",
        "CreateTable",
        &create_table_request("api-call-no-trail-first"),
    )
    .await;
    let arrived = drained(&router, "no trail at all", &sink).await;
    assert!(
        arrived.is_empty(),
        "with no trail logging, a control-plane call publishes nothing, got {arrived:?}"
    );

    // Row 2: a trail that was created, started and then STOPPED is the same condition — what
    // decides the publication is whether a trail is logging when the call is served.
    create_bucket(&router, "api-call-no-trail-trail").await;
    cloudtrail(
        &router,
        "setup",
        "CreateTrail",
        &json!({ "Name": "api-call-no-trail", "S3BucketName": "api-call-no-trail-trail" }),
    )
    .await;
    cloudtrail(
        &router,
        "setup",
        "StartLogging",
        &json!({ "Name": "api-call-no-trail" }),
    )
    .await;
    cloudtrail(
        &router,
        "setup",
        "StopLogging",
        &json!({ "Name": "api-call-no-trail" }),
    )
    .await;
    dynamodb(
        &router,
        "a trail that was stopped",
        "CreateTable",
        &create_table_request("api-call-no-trail-second"),
    )
    .await;
    let arrived = drained(&router, "a trail that was stopped", &sink).await;
    assert!(
        arrived.is_empty(),
        "with the account's only trail stopped, a control-plane call publishes nothing, got \
         {arrived:?}"
    );

    // And the rule itself is live: with that trail logging again, the same call is delivered
    // to it — so the two rows above are the condition's doing and not a dead wiring.
    cloudtrail(
        &router,
        "setup",
        "StartLogging",
        &json!({ "Name": "api-call-no-trail" }),
    )
    .await;
    dynamodb(
        &router,
        "the trail logging again",
        "CreateTable",
        &create_table_request("api-call-no-trail-third"),
    )
    .await;
    let envelope = delivered(&router, "the trail logging again", &sink).await;
    assert_eq!(
        envelope["detail"]["requestParameters"]["tableName"],
        json!("api-call-no-trail-third"),
        "the delivered record must be the third call's own, got {envelope}"
    );
}
