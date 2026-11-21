// SPDX-License-Identifier: CC0-1.0

//! An EventBridge rule's Amazon Data Firehose target writes its Amazon S3 destination only
//! where the stream's own destination ROLE may make that write — and a write that role may
//! not make is readable where the stream's `CloudWatchLoggingOptions` say.
//!
//! The rule under test: **Firehose makes a destination's object write under the role that
//! destination configuration names ("the Amazon Resource Name (ARN) of the Amazon Web
//! Services credentials" — `S3DestinationConfiguration$RoleARN`), so the write happens only
//! where the permissions in force on THAT role allow Amazon S3's own write of the object the
//! delivery would create — whichever producer the records arrived from. A delivery the role
//! may not make writes nothing at all and is reported as a delivery-error event, whose
//! `errorCode` is the fixed `S3.AccessDenied`, into the log group and log stream the
//! destination's `CloudWatchLoggingOptions` name ("Describes the Amazon CloudWatch logging
//! options for your Firehose stream"); once the permissions in force do allow the write, the
//! next delivery of the SAME stream is made — which is what tells a permission-specific
//! refusal from a stream that is simply stuck. A delivery the role may make is written and
//! reports nothing at all.**
//!
//! Only a cross-service suite can ask this of an EventBridge producer: the flow spans five
//! services over ONE engine exactly as `core/router/src/lib.rs` mounts them — EventBridge for
//! the rule and the publication, IAM for the roles the record put and the object write are
//! each decided by, Firehose for the stream that buffers and delivers, Amazon S3 for the
//! bucket and the delivered objects, and CloudWatch Logs for the delivery-error events.
//!
//! What this adds to its siblings is the PRODUCER. `events_rule_firehose_target_delivery`
//! pins which role may put the record into the stream, and stops at the delivered object;
//! `cloudformation_kinesis_sourced_firehose_delivery_role_authorization` pins the destination
//! role's decision for a stack-deployed, Kinesis-sourced stream; the Firehose crate's own
//! `delivery_role_refusal` and `destination_delivery_errors` pin it for a stream a client
//! created and puts records on itself. None of them reaches the destination role's decision
//! from a rule's publication, which is the path this suite drives — and the roles, the
//! publication, the wire encoders and the waits are the ones those sibling suites already
//! hold, called rather than restated.
//!
//! The rows differ along what the rule generalises over: WHY the destination role may or may
//! not make the write (every object of the destination bucket, only the objects under the
//! destination's own key prefix, no object write at all, an object write scoped to another
//! bucket), and which member carries the Amazon S3 location (`S3DestinationConfiguration`,
//! the `ExtendedS3DestinationConfiguration` superset). Nothing here asserts how fast the host
//! ran: every wait looks again for the CONDITION under a generous deadline, and the
//! destinations that must stay empty are re-read at every one of those readings.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The publication, the per-service wire helpers and the waits the fan-out suite already holds
// for this same set of services, so both suites publish one event, speak one wire and wait one
// way.
use crate::events_rule_fans_out_to_firehose_and_queue_targets::{
    assert_carries_the_envelope, delivered_object, events, firehose, given_an_active_stream,
    keys_under, DEADLINE, DETAIL_TYPE, EVENTS_PRINCIPAL, LOOK_AGAIN, ORDER_ID, PUT_RECORD_ACTION,
    SOURCE,
};
// The Region every call is signed for, the request encoder, the IAM wire and the role builder.
use crate::events_rule_firehose_target_delivery::{given_role, iam, send, REGION};

/// The bucket every stream of this suite delivers into.
const BUCKET: &str = "events-firehose-destination-role";

/// Another bucket Amazon S3 holds, so a row can scope its role's object write to a bucket
/// that is not its destination's.
const OTHER_BUCKET: &str = "events-firehose-destination-role-elsewhere";

/// The log stream every row's `CloudWatchLoggingOptions` name — the one a Firehose
/// destination's delivery errors go to.
const LOG_STREAM: &str = "DestinationDelivery";

/// Amazon S3's own write of an object, which is the action a delivery to an Amazon S3
/// destination is authorized under.
const OBJECT_WRITE: &str = "s3:PutObject";

/// The log write a delivery stream's own logging makes, which is the only thing the role of
/// the row carrying no object write at all states.
const LOG_WRITE: &str = "logs:PutLogEvents";

/// The service principal Firehose assumes a destination's role as.
const FIREHOSE_PRINCIPAL: &str = "firehose.amazonaws.com";

/// The `errorCode` a destination whose role may not make the write reports — fixed text.
const REFUSED_ERROR_CODE: &str = "S3.AccessDenied";

/// How long the destinations that must hold NOTHING go on being re-read once every permitted
/// destination holds its object — spent re-reading them rather than sleeping once, so what the
/// assertions judge is the latest reading.
const SILENCE_WINDOW: Duration = Duration::from_secs(2);

/// What a row's destination role allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// The object write, on every object of the destination's OWN bucket.
    EveryObjectOfTheBucket,
    /// The object write, on the objects under the destination's own key prefix only — a
    /// least-privilege grant that still covers the delivery.
    ObjectsUnderTheDestinationPrefix,
    /// No object write at all: the role states only the log write, which is the role the
    /// delivery's authorization has nothing to read an object write from.
    OnlyTheLogWrite,
    /// The object write, scoped to another bucket altogether — a least-privilege mistake, so
    /// the delivery to this row's destination is not a write this role may make.
    ObjectsOfAnotherBucket,
}

/// What a row's destination holds once its rule's target has put the published event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The event, as a delivered object under this row's own prefix.
    Delivered,
    /// Nothing: the destination role may not make the write.
    Withheld,
}

/// One row: a rule, its delivery-stream target, and the destination role that target's stream
/// delivers under.
struct Row {
    case: &'static str,
    /// The infix of this row's resource names and of its key prefix, so no two rows collide.
    key: &'static str,
    /// The `CreateDeliveryStream` member this row declares its Amazon S3 destination under.
    member: &'static str,
    grant: Grant,
    outcome: Outcome,
}

impl Row {
    /// The `Prefix` this row's destination delivers under, which no other row's shares.
    fn prefix(&self) -> String {
        format!("{}/", self.key)
    }

    /// The delivery stream this row's rule targets.
    fn stream(&self) -> String {
        format!("{}-stream", self.key)
    }

    /// The rule whose target names that stream.
    fn rule(&self) -> String {
        format!("{}-rule", self.key)
    }

    /// The role the stream DELIVERS under, whose permissions decide the object write.
    fn delivery_role(&self) -> String {
        format!("{}-delivery-role", self.key)
    }

    /// The role EventBridge puts the record into the stream AS, which every row allows the
    /// put, so the only thing that differs between the rows is the destination role.
    fn target_role(&self) -> String {
        format!("{}-target-role", self.key)
    }

    /// The log group this row's `CloudWatchLoggingOptions` name, one per row so a reported
    /// delivery error is attributable to the stream that made it.
    fn log_group(&self) -> String {
        format!("/aws/kinesisfirehose/{}", self.key)
    }

    /// The statements this row's delivery role carries.
    fn statements(&self) -> Vec<Value> {
        let written = match self.grant {
            Grant::EveryObjectOfTheBucket => format!("{}/*", bucket_arn(BUCKET)),
            Grant::ObjectsUnderTheDestinationPrefix => {
                format!("{}/{}*", bucket_arn(BUCKET), self.prefix())
            }
            Grant::ObjectsOfAnotherBucket => format!("{}/*", bucket_arn(OTHER_BUCKET)),
            // The role of the stream whose operator granted it the log write and forgot the
            // object write: the condition the delivery error is the only signal of.
            Grant::OnlyTheLogWrite => {
                return vec![allow(LOG_WRITE, &log_group_arn(&self.log_group()))];
            }
        };
        vec![allow(OBJECT_WRITE, &written)]
    }
}

/// The rows under test. Two must deliver and two must not, so neither answer can be had by a
/// store that delivers everything it is given or one that delivers nothing.
const ROWS: &[Row] = &[
    Row {
        case: "a destination role allowed every object of its bucket",
        key: "allowed",
        member: "ExtendedS3DestinationConfiguration",
        grant: Grant::EveryObjectOfTheBucket,
        outcome: Outcome::Delivered,
    },
    Row {
        case: "a destination role allowed only the objects under its own prefix",
        key: "scoped",
        member: "S3DestinationConfiguration",
        grant: Grant::ObjectsUnderTheDestinationPrefix,
        outcome: Outcome::Delivered,
    },
    Row {
        case: "a destination role stating the log write and no object write",
        key: "unstated",
        member: "ExtendedS3DestinationConfiguration",
        grant: Grant::OnlyTheLogWrite,
        outcome: Outcome::Withheld,
    },
    Row {
        case: "a destination role whose object write is scoped to another bucket",
        key: "elsewhere",
        member: "S3DestinationConfiguration",
        grant: Grant::ObjectsOfAnotherBucket,
        outcome: Outcome::Withheld,
    },
];

/// `bucket` as the `BucketARN` a destination names it by, and as the event of a refused
/// delivery reports the destination it was going to.
fn bucket_arn(bucket: &str) -> String {
    format!("arn:aws:s3:::{bucket}")
}

/// `group` as the ARN an identity statement about its log events names it by.
fn log_group_arn(group: &str) -> String {
    format!(
        "arn:aws:logs:{REGION}:{}:log-group:{group}:*",
        engine::emulator_account_id()
    )
}

/// One statement allowing `action` on `resource`.
fn allow(action: &str, resource: &str) -> Value {
    json!({ "Effect": "Allow", "Action": action, "Resource": resource })
}

/// `statements` as an identity policy document.
fn policy(statements: Vec<Value>) -> String {
    json!({ "Version": "2012-10-17", "Statement": statements }).to_string()
}

/// Put the inline policy `name` on the role `role`.
async fn put_role_policy(
    router: &MultiServiceRouter,
    case: &str,
    role: &str,
    name: &str,
    document: &str,
) {
    iam(
        router,
        case,
        "PutRolePolicy",
        &[
            ("RoleName", role),
            ("PolicyName", name),
            ("PolicyDocument", document),
        ],
    )
    .await;
}

/// One CloudWatch Logs operation (`awsJson1_1`), answering its status and parsed body: a group
/// or stream the account does not hold yet is one of the answers this suite reads, so the
/// status is returned rather than asserted.
async fn logs(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    let (status, answered) = send(
        router,
        "logs",
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &format!("Logs_20140328.{operation}")),
        ],
        body.to_string().into_bytes(),
    )
    .await;
    let parsed = if answered.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&answered).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Every delivery-error document `row`'s own log stream holds, read back through CloudWatch
/// Logs' own `GetLogEvents`.
async fn reported_events(router: &MultiServiceRouter, row: &Row) -> Vec<Value> {
    let (status, body) = logs(
        router,
        "GetLogEvents",
        json!({
            "logGroupName": row.log_group(),
            "logStreamName": LOG_STREAM,
            "startFromHead": true,
        }),
    )
    .await;
    if status != 200 {
        return Vec::new();
    }
    body["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|event| event["message"].as_str())
        .filter_map(|message| serde_json::from_str::<Value>(message).ok())
        .collect()
}

/// The `errorCode`s `row`'s log stream has reported so far.
async fn reported_codes(router: &MultiServiceRouter, row: &Row) -> Vec<String> {
    reported_events(router, row)
        .await
        .iter()
        .map(|document| {
            document["errorCode"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// Require every destination whose role may not make the write to hold nothing right now.
async fn assert_holding_nothing(router: &MultiServiceRouter) {
    for row in ROWS {
        if row.outcome != Outcome::Withheld {
            continue;
        }
        let keys = keys_under(router, row.case, BUCKET, &row.prefix()).await;
        assert!(
            keys.is_empty(),
            "case {:?}: a delivery the destination role may not make writes nothing under \
             {:?}, got {keys:?}",
            row.case,
            row.prefix()
        );
    }
}

/// Wait until every destination whose role allows the write holds an object, requiring every
/// destination that must hold nothing to be empty at each reading taken meanwhile.
async fn await_delivered(router: &MultiServiceRouter) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        assert_holding_nothing(router).await;
        let mut pending: Vec<&str> = Vec::new();
        for row in ROWS {
            if row.outcome == Outcome::Delivered
                && keys_under(router, row.case, BUCKET, &row.prefix())
                    .await
                    .is_empty()
            {
                pending.push(row.case);
            }
        }
        if pending.is_empty() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected a delivered object for each of {pending:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Wait until `row`'s own log stream carries a delivery-error event, and require that event to
/// name the stream, the destination and the condition.
async fn await_reported(router: &MultiServiceRouter, row: &Row, stream_arn: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Some(document) = reported_events(router, row).await.first() {
            assert_eq!(
                document["errorCode"],
                json!(REFUSED_ERROR_CODE),
                "case {:?}: expected the reported errorCode {REFUSED_ERROR_CODE:?}, got \
                 {document}",
                row.case
            );
            // The variable parts are read off the stream and its configuration rather than
            // fixed here: the stream the records were put on, the destination they were going
            // to, and the stream's version at the time of the delivery.
            assert_eq!(
                document["deliveryStreamARN"],
                json!(stream_arn),
                "case {:?}: expected the event to name its own stream, got {document}",
                row.case
            );
            assert_eq!(
                document["destination"],
                json!(bucket_arn(BUCKET)),
                "case {:?}: expected the event to name the destination it was going to, got \
                 {document}",
                row.case
            );
            assert_eq!(
                document["deliveryStreamVersionId"],
                json!(1),
                "case {:?}: expected the event to name the stream's version, got {document}",
                row.case
            );
            assert!(
                document["message"]
                    .as_str()
                    .is_some_and(|message| !message.is_empty()),
                "case {:?}: expected the event to state what the code means, got {document}",
                row.case
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: expected a delivery-error event in {:?} / {LOG_STREAM:?}, got none",
            row.case,
            row.log_group()
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The emulator the suite drives: the five services the flow spans, on ONE engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signature verification is the emulator's concern, not any of these services'.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_firehose_firehose_20150804::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// Create `bucket`, which every row's destination either delivers into or is scoped to.
async fn given_a_bucket(router: &MultiServiceRouter, bucket: &str) {
    let (status, body) = send(
        router,
        "s3",
        "PUT",
        &format!("/{bucket}"),
        &[],
        format!(
            "<CreateBucketConfiguration><LocationConstraint>{REGION}\
             </LocationConstraint></CreateBucketConfiguration>"
        )
        .into_bytes(),
    )
    .await;
    assert_eq!(
        status,
        200,
        "setup: the bucket {bucket} must be created, got {status} {}",
        String::from_utf8_lossy(&body)
    );
}

/// Publish the one event every row's rule matches.
async fn publish(router: &MultiServiceRouter, case: &str) {
    let published = events(
        router,
        case,
        "PutEvents",
        json!({
            "Entries": [{
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "orderId": ORDER_ID }).to_string(),
            }],
        }),
    )
    .await;
    assert_eq!(
        published["FailedEntryCount"],
        json!(0),
        "case {case:?}: the publication must be accepted whole, got {published}"
    );
}

#[tokio::test]
async fn a_rules_firehose_target_delivers_only_where_the_destination_role_allows_the_write() {
    let router = emulator();
    let account = engine::emulator_account_id();

    given_a_bucket(&router, BUCKET).await;
    given_a_bucket(&router, OTHER_BUCKET).await;

    // Every row's log group, delivery role, stream and rule — created together, so the suite
    // spends ONE provisioning span for all of them.
    let mut stream_arns: Vec<String> = Vec::new();
    for row in ROWS {
        let case = row.case;
        let (status, answered) = logs(
            &router,
            "CreateLogGroup",
            json!({ "logGroupName": row.log_group() }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the log group must be created, got {status} {answered}"
        );
        let delivery_role =
            given_role(&router, case, &row.delivery_role(), FIREHOSE_PRINCIPAL, None).await;
        put_role_policy(
            &router,
            case,
            &row.delivery_role(),
            "delivery",
            &policy(row.statements()),
        )
        .await;

        let destination = json!({
            "RoleARN": delivery_role,
            "BucketARN": bucket_arn(BUCKET),
            "Prefix": row.prefix(),
            // The shortest buffering the hints admit (`SizeInMBs` 1..128,
            // `IntervalInSeconds` 0..900), so a row waits for the emulator's own delivery
            // span and nothing longer.
            "BufferingHints": { "SizeInMBs": 1, "IntervalInSeconds": 0 },
            "CloudWatchLoggingOptions": {
                "Enabled": true,
                "LogGroupName": row.log_group(),
                "LogStreamName": LOG_STREAM,
            },
        });
        let mut request = json!({
            "DeliveryStreamName": row.stream(),
            "DeliveryStreamType": "DirectPut",
        });
        request[row.member] = destination;
        let created = firehose(&router, case, "CreateDeliveryStream", request).await;
        let stream_arn = created["DeliveryStreamARN"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the create answers the stream's ARN"))
            .to_string();
        stream_arns.push(stream_arn);

        let target_role = given_role(
            &router,
            case,
            &row.target_role(),
            EVENTS_PRINCIPAL,
            Some(PUT_RECORD_ACTION),
        )
        .await;
        events(
            &router,
            case,
            "PutRule",
            json!({
                "Name": row.rule(),
                "EventPattern": json!({
                    "source": [SOURCE],
                    "detail-type": [DETAIL_TYPE],
                })
                .to_string(),
                "State": "ENABLED",
            }),
        )
        .await;
        let answered = events(
            &router,
            case,
            "PutTargets",
            json!({
                "Rule": row.rule(),
                "Targets": [{
                    "Id": "stream",
                    "Arn": format!(
                        "arn:aws:firehose:{REGION}:{account}:deliverystream/{}",
                        row.stream()
                    ),
                    "RoleArn": target_role,
                }],
            }),
        )
        .await;
        assert_eq!(
            answered["FailedEntryCount"],
            json!(0),
            "case {case:?}: the target must be accepted, got {answered}"
        );
    }

    // Firehose refuses a record put at a stream that is still being created, so the
    // publication waits for every stream to REPORT that it can take one.
    for row in ROWS {
        given_an_active_stream(&router, row.case, &row.stream()).await;
    }

    // ONE publication, which every row's rule matches: what each row's destination role
    // allows is the only thing that differs between them.
    publish(&router, "the first publication").await;

    // The destinations whose role allows the write hold the published envelope; the others
    // hold nothing at every reading taken while that is being waited for.
    await_delivered(&router).await;
    for row in ROWS.iter().filter(|row| row.outcome == Outcome::Delivered) {
        let body = delivered_object(&router, row.case, BUCKET, &row.prefix()).await;
        assert_carries_the_envelope(row.case, "stream", &body);
    }

    // And every delivery the role may not make is reported where that stream's own logging
    // says, under the one error code that condition carries.
    for (row, stream_arn) in ROWS
        .iter()
        .zip(&stream_arns)
        .filter(|(row, _)| row.outcome == Outcome::Withheld)
    {
        await_reported(&router, row, stream_arn).await;
    }

    // The refused destinations still hold nothing, re-read throughout one more window, and a
    // destination that TOOK its delivery reports nothing at all.
    let until = Instant::now() + SILENCE_WINDOW;
    while Instant::now() < until {
        assert_holding_nothing(&router).await;
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    assert_holding_nothing(&router).await;
    for row in ROWS {
        let codes = reported_codes(&router, row).await;
        match row.outcome {
            Outcome::Delivered => assert!(
                codes.is_empty(),
                "case {:?}: a delivery the destination role allows reports nothing, got \
                 {codes:?}",
                row.case
            ),
            Outcome::Withheld => assert!(
                !codes.is_empty() && codes.iter().all(|code| code == REFUSED_ERROR_CODE),
                "case {:?}: every reported delivery error is {REFUSED_ERROR_CODE:?}, got \
                 {codes:?}",
                row.case
            ),
        }
    }

    // The permissions in force on each refused row's role are widened to cover the write its
    // destination makes — the role, the stream, the rule and the target are untouched — and
    // the SAME publication now lands an object under that row's own prefix: the refusal was
    // the role's permissions and not a stream that had stopped delivering.
    for row in ROWS.iter().filter(|row| row.outcome == Outcome::Withheld) {
        put_role_policy(
            &router,
            row.case,
            &row.delivery_role(),
            "repair",
            &policy(vec![allow(OBJECT_WRITE, &format!("{}/*", bucket_arn(BUCKET)))]),
        )
        .await;
    }
    publish(&router, "the publication after the repair").await;
    for row in ROWS.iter().filter(|row| row.outcome == Outcome::Withheld) {
        let body = delivered_object(&router, row.case, BUCKET, &row.prefix()).await;
        // Whatever the stream still owed is delivered beside what the repaired publication
        // put, so the destination holds one or more copies of the envelope rather than
        // exactly one: what this row is about is that the identical publication now LANDS.
        for expected in [SOURCE, DETAIL_TYPE, ORDER_ID] {
            assert!(
                body.contains(expected),
                "case {:?}: once the role allows the write, the delivered object carries the \
                 published envelope; expected {expected:?} in {body:?}",
                row.case
            );
        }
    }
}
