// SPDX-License-Identifier: CC0-1.0

//! What an EventBridge rule target's DEAD-LETTER queue receives when the rule's CloudWatch
//! Logs LOG GROUP target would not hold the event — end to end over the real
//! `MultiServiceRouter` with EventBridge, CloudWatch Logs and Amazon SQS mounted on one
//! engine.
//!
//! The RULE pinned here: **a delivery whose `PutLogEvents` wrote no log event is dead-lettered
//! in EventBridge's OWN vocabulary — `ERROR_CODE` is one of EventBridge's delivery conditions
//! and `ERROR_MESSAGE` is EventBridge's sentence for the call it could not complete, "Could not
//! complete putLogEvents call for <logGroupName>.", never the shape name or the prose of the
//! answer the write came back with — and WHICH condition follows from whether the group can
//! ever take that event: an instant the group's ingestion window has already left behind is a
//! permanent refusal that is retried not at all, while an instant ahead of that window is
//! retried under the target's `RetryPolicy` and reported as the batch rejection it is, with the
//! retries it made and the bound that ended them beside it.**
//!
//! The window itself is CloudWatch Logs': "events more than 2 hours in the future are rejected
//! while processing remaining valid events. Events older than 14 days or preceding the log
//! group's retention period are rejected while processing remaining valid events"
//! (`PutLogEvents`). So the rows differ along where the event's own `Time` falls relative to
//! that window — inside it, at the far edge of the past, past the 14-day horizon, far past it,
//! before the group's own retention period, and ahead of the window — which is what the rule
//! generalises over; the recorded case (a 15-day-old entry beside a current one and one three
//! hours ahead) is three of these rows.
//!
//! The second test varies the `RetryPolicy` instead (`MaximumRetryAttempts` `range 0..185`,
//! `MaximumEventAgeInSeconds` `range 60..86400`, from
//! `smithy_query(query="operation", operation="PutTargets")`), so what it pins is that the
//! retry accounting on the dead letter is the delivery's own: the retries really made, and
//! WHICH of the target's two bounds ended them.
//!
//! `core/router/tests/all/events_lambda_target_dead_letter_capture.rs` is the suite for what a
//! dead letter carries for a FUNCTION target. The window table and the retry accounting are
//! also pinned inside the owning crate by
//! `services/service-com-amazonaws-eventbridge-awsevents/tests/all/log_group_target_window_dead_letter.rs`,
//! which mounts EventBridge, CloudWatch Logs and Amazon SQS as a hand-picked set; what this
//! suite adds is that the same rule holds over the service set a client's own `events`-scoped
//! call reaches, where several mounted models resolve to the `events` key.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The `source` and `detail-type` every event this suite publishes carries, and what the
/// rules match on.
const SOURCE: &str = "logs.target.window";
const DETAIL_TYPE: &str = "Log Group Window Probe";

/// The service principal EventBridge writes into a log group as, which is what the resource
/// policy in force over that group admits.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The two calls a log-group delivery is made of, each needing its own grant.
const CREATE_STREAM_ACTION: &str = "logs:CreateLogStream";
const PUT_EVENTS_ACTION: &str = "logs:PutLogEvents";

/// The condition EventBridge names on a dead letter for a call the target would not take the
/// parameters of, spelled here rather than imported from the service so a wrong value in the
/// service would fail this suite.
const INVALID_PARAMETER: &str = "INVALID_PARAMETER";

/// The condition it names for a batch call the target answered without holding the event it
/// carried. Spelled here for the same reason.
const EVENTS_IN_BATCH_REQUEST_REJECTED: &str = "EVENTS_IN_BATCH_REQUEST_REJECTED";

/// The dead-letter attributes that account for a RETRIED delivery, and the two bounds either
/// of which ends one (`RetryPolicy`'s own members). Spelled here for the same reason.
const RETRY_ATTEMPTS: &str = "RETRY_ATTEMPTS";
const EXHAUSTED_RETRY_CONDITION: &str = "EXHAUSTED_RETRY_CONDITION";
const MAXIMUM_RETRY_ATTEMPTS: &str = "MaximumRetryAttempts";
const MAXIMUM_EVENT_AGE: &str = "MaximumEventAgeInSeconds";

/// Seconds in one hour and in one day, the units the rows state an event's `Time` offset in.
const HOUR: i64 = 60 * 60;
const DAY: i64 = 24 * HOUR;

/// How long a row waits for a condition the clock owes — a dead letter to arrive, a log event
/// to land — before it gives up: far above the span either takes, and driven off the condition
/// rather than off a fixed sleep, so a slow machine delays a row instead of breaking it.
const DEADLINE: Duration = Duration::from_secs(60);

/// One poll inside [`DEADLINE`].
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// EventBridge, CloudWatch Logs and Amazon SQS over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
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
    let parsed: Value = if response.body().is_empty() {
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
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// An SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "sqs", "AmazonSQS", "1.0", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// A CloudWatch Logs call (`awsJson1_1`, `Logs_20140328.<Operation>`) that must answer 200.
async fn logs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "logs", "Logs_20140328", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch Logs {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// Epoch seconds now, which is what an `awsJson1_1` timestamp carries.
fn epoch_now() -> i64 {
    aws_smithy_types::DateTime::from(std::time::SystemTime::now()).secs()
}

/// Every message waiting on the queue at `url`, with its attributes.
async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let answer = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({
            "QueueUrl": url,
            "MaxNumberOfMessages": 10,
            "MessageAttributeNames": ["All"],
        }),
    )
    .await;
    answer["Messages"].as_array().cloned().unwrap_or_default()
}

/// The messages on the queue at `url` once `wanted` of them have arrived, or everything that
/// arrived inside [`DEADLINE`].
///
/// The queue is read at least twice however small `wanted` is — a `wanted` of none reads it
/// twice and nothing else — so a message that must NOT be there is read for rather than
/// assumed absent.
async fn collect(router: &MultiServiceRouter, case: &str, url: &str, wanted: usize) -> Vec<Value> {
    let started = std::time::Instant::now();
    let mut arrived: Vec<Value> = Vec::new();
    loop {
        arrived.extend(drain(router, case, url).await);
        if arrived.len() >= wanted || started.elapsed() >= DEADLINE {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    arrived.extend(drain(router, case, url).await);
    arrived
}

/// The log events the stream `stream` of the group `group` holds, once at least `wanted` of
/// them are there or [`DEADLINE`] has passed.
///
/// A read the group does not answer — the stream the delivery opens is not there yet — counts
/// as none held, so the caller's own assertion reports what was found rather than the status
/// of a read taken too early.
async fn written(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    stream: &str,
    wanted: usize,
) -> Vec<Value> {
    let started = std::time::Instant::now();
    loop {
        let (status, answer) = rpc(
            router,
            "logs",
            "Logs_20140328",
            "1.1",
            "GetLogEvents",
            &json!({
                "logGroupName": group,
                "logStreamName": stream,
                "startFromHead": true,
            }),
        )
        .await;
        let held = match status {
            200 => answer["events"].as_array().cloned().unwrap_or_default(),
            _ => Vec::new(),
        };
        if held.len() >= wanted || started.elapsed() >= DEADLINE {
            assert!(
                status == 200 || held.len() >= wanted,
                "case {case:?}: expected HTTP 200 from CloudWatch Logs GetLogEvents on \
                 {group}/{stream}, got {status} with {answer}"
            );
            return held;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A dead-letter queue whose own policy admits the dead-letter write, so what reaches it is
/// decided by the log-group delivery rather than by the queue ("for … Amazon SQS queues,
/// EventBridge relies on resource-based policies"). Answers its url and its ARN, read back
/// from the queue rather than built here.
async fn given_dead_letter_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> (String, String) {
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl, got {created}"))
        .to_string();
    let attributes = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueArn, got {attributes}"))
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": EVENTS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": arn,
        }],
    });
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": policy.to_string() } }),
    )
    .await;
    (url, arn)
}

/// A log group this service may write into: created, given the retention the row asks for, and
/// carrying a resource policy that grants this service both calls a delivery makes on it.
/// Answers its name and the ARN a target names it by, read back from the group.
async fn given_log_group(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    retention_days: Option<i64>,
) -> String {
    logs(router, case, "CreateLogGroup", &json!({ "logGroupName": name })).await;
    if let Some(days) = retention_days {
        logs(
            router,
            case,
            "PutRetentionPolicy",
            &json!({ "logGroupName": name, "retentionInDays": days }),
        )
        .await;
    }
    let described = logs(
        router,
        case,
        "DescribeLogGroups",
        &json!({ "logGroupNamePrefix": name }),
    )
    .await;
    let arn = described["logGroups"][0]["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected the group's arn, got {described}"))
        .to_string();
    // `LogGroup$arn` carries the group WITH everything under it; the grant is written over
    // that form, which is what covers the stream the write goes into.
    let under_the_group = match arn.strip_suffix(":*") {
        Some(group) => format!("{group}:*"),
        None => format!("{arn}:*"),
    };
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": EVENTS_PRINCIPAL },
            "Action": [CREATE_STREAM_ACTION, PUT_EVENTS_ACTION],
            "Resource": under_the_group,
        }],
    });
    logs(
        router,
        case,
        "PutResourcePolicy",
        &json!({
            "policyName": format!("{name}-events"),
            "policyDocument": document.to_string(),
            "resourceArn": arn,
        }),
    )
    .await;
    arn
}

/// What one row runs over: a log group, a dead-letter queue, and a rule with a log-group
/// target wired to both.
struct Wiring {
    group: String,
    group_arn: String,
    rule: String,
    rule_arn: String,
    dead_letter_url: String,
}

/// Wire one row: the group (with its retention), the dead-letter queue, the rule, and the
/// log-group target carrying the row's own `RetryPolicy`.
async fn wire(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    retention_days: Option<i64>,
    maximum_retry_attempts: i64,
    maximum_event_age: i64,
) -> Wiring {
    let group = format!("/emulator/events/{name}");
    let group_arn = given_log_group(router, case, &group, retention_days).await;
    let (dead_letter_url, dead_letter_arn) =
        given_dead_letter_queue(router, case, &format!("{name}-dlq")).await;
    let rule = format!("{name}-rule");
    let created = events(
        router,
        case,
        "PutRule",
        &json!({
            "Name": rule,
            "EventPattern": json!({ "source": [SOURCE], "detail-type": [DETAIL_TYPE] })
                .to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    let rule_arn = created["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a RuleArn, got {created}"))
        .to_string();
    let wired = events(
        router,
        case,
        "PutTargets",
        &json!({
            "Rule": rule,
            "Targets": [{
                "Id": "group",
                "Arn": group_arn,
                "DeadLetterConfig": { "Arn": dead_letter_arn },
                "RetryPolicy": {
                    "MaximumRetryAttempts": maximum_retry_attempts,
                    "MaximumEventAgeInSeconds": maximum_event_age,
                },
            }],
        }),
    )
    .await;
    assert_eq!(
        wired["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected the log-group target to be accepted, got {wired}"
    );
    Wiring {
        group,
        group_arn,
        rule,
        rule_arn,
        dead_letter_url,
    }
}

/// Publish ONE event carrying `time` as its own `Time`, and answer the `EventId` it was
/// accepted under.
async fn publish(router: &MultiServiceRouter, case: &str, time: i64) -> String {
    let published = events(
        router,
        case,
        "PutEvents",
        &json!({
            "Entries": [{
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "probe": case }).to_string(),
                "Time": time,
            }],
        }),
    )
    .await;
    assert_eq!(
        published["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected the event to be accepted, got {published}"
    );
    published["Entries"][0]["EventId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected an EventId, got {published}"))
        .to_string()
}

/// The value of the dead letter's `name` message attribute, or `None` when it carries none.
fn attribute(message: &Value, name: &str) -> Option<String> {
    message["MessageAttributes"][name]["StringValue"]
        .as_str()
        .map(ToString::to_string)
}

/// What the dead-letter queue must hold for one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// Nothing: the group held the event, so the delivery succeeded.
    Written,
    /// One dead letter under this condition.
    DeadLettered(&'static str),
}

/// One row of the window table: where the event's own `Time` sits relative to now, the
/// retention the group carries, and what the delivery must come to.
struct Row {
    case: &'static str,
    /// The row's own suffix for every resource it creates.
    name: &'static str,
    /// How far the event's `Time` sits from now, in seconds.
    offset_seconds: i64,
    /// `retentionInDays` on the group, which narrows the window it ingests; `None` for a
    /// group that never expires its events.
    retention_days: Option<i64>,
    expected: Expected,
}

/// The window the rows span: CloudWatch Logs ingests an event whose instant is inside 14 days
/// of the past — or the group's own retention period, where that is shorter — and up to 2
/// hours of the future, and holds no other.
fn window_rows() -> Vec<Row> {
    vec![
        Row {
            case: "an event inside the window is written and dead-letters nothing",
            name: "inside",
            offset_seconds: 0,
            retention_days: None,
            expected: Expected::Written,
        },
        Row {
            case: "an event near the far edge of the window's past is still written",
            name: "near-edge",
            offset_seconds: -13 * DAY,
            retention_days: None,
            expected: Expected::Written,
        },
        Row {
            case: "an event past the ingestion horizon is dead-lettered permanently",
            name: "fifteen-days",
            offset_seconds: -15 * DAY,
            retention_days: None,
            expected: Expected::DeadLettered(INVALID_PARAMETER),
        },
        Row {
            case: "an event far past that horizon is dead-lettered the same way",
            name: "forty-days",
            offset_seconds: -40 * DAY,
            retention_days: None,
            expected: Expected::DeadLettered(INVALID_PARAMETER),
        },
        Row {
            case: "an event preceding the group's own retention period is dead-lettered \
                   permanently",
            name: "retention",
            offset_seconds: -3 * DAY,
            retention_days: Some(1),
            expected: Expected::DeadLettered(INVALID_PARAMETER),
        },
        Row {
            case: "an event ahead of the window is dead-lettered as a rejected batch",
            name: "three-hours-ahead",
            offset_seconds: 3 * HOUR,
            retention_days: None,
            expected: Expected::DeadLettered(EVENTS_IN_BATCH_REQUEST_REJECTED),
        },
        Row {
            case: "an event far ahead of the window is dead-lettered the same way",
            name: "a-day-ahead",
            offset_seconds: 25 * HOUR,
            retention_days: None,
            expected: Expected::DeadLettered(EVENTS_IN_BATCH_REQUEST_REJECTED),
        },
    ]
}

/// A log-group delivery the group would not hold is dead-lettered in EventBridge's own
/// vocabulary, under the condition the event's instant earns, and one it holds is not
/// dead-lettered at all.
#[tokio::test]
async fn an_out_of_window_log_group_delivery_dead_letters_in_eventbridges_own_vocabulary() {
    let router = router();
    for row in window_rows() {
        let case = row.case;
        // No retry is allowed, so a retried delivery's accounting is this row's only
        // variable and the second test is the one that varies it.
        let wiring = wire(&router, case, row.name, row.retention_days, 0, 60).await;
        let time = epoch_now() + row.offset_seconds;
        let event_id = publish(&router, case, time).await;

        let wanted = usize::from(row.expected != Expected::Written);
        let dead_letters = collect(&router, case, &wiring.dead_letter_url, wanted).await;
        assert_eq!(
            dead_letters.len(),
            wanted,
            "case {case:?}: expected {wanted} dead letter(s), got {dead_letters:?}"
        );

        let Expected::DeadLettered(expected_code) = row.expected else {
            // The delivery succeeded, so the group holds the event — under the event's own
            // instant, which is the log event's timestamp.
            let held = written(&router, case, &wiring.group, &wiring.rule, 1).await;
            assert_eq!(
                held.len(),
                1,
                "case {case:?}: expected the group to hold the delivered event, got {held:?}"
            );
            assert_eq!(
                held[0]["timestamp"],
                json!(time * 1_000),
                "case {case:?}: expected the log event's timestamp to be the event's own \
                 instant {}, got {:?}",
                time * 1_000,
                held[0]
            );
            let carried: Value = serde_json::from_str(
                held[0]["message"].as_str().unwrap_or_default(),
            )
            .unwrap_or(Value::Null);
            assert_eq!(
                carried["id"],
                json!(event_id),
                "case {case:?}: expected the written message to be the envelope of \
                 {event_id:?}, got {:?}",
                held[0]["message"]
            );
            continue;
        };

        let message = &dead_letters[0];
        let body: Value =
            serde_json::from_str(message["Body"].as_str().unwrap_or_default()).unwrap_or(Value::Null);
        assert_eq!(
            body["id"],
            json!(event_id),
            "case {case:?}: expected the dead letter to carry the envelope of {event_id:?}, \
             got {:?}",
            message["Body"]
        );
        assert_eq!(
            attribute(message, "ERROR_CODE").as_deref(),
            Some(expected_code),
            "case {case:?}: expected the dead letter's ERROR_CODE to be {expected_code:?}, got \
             {:?}",
            attribute(message, "ERROR_CODE")
        );
        let expected_message = format!("Could not complete putLogEvents call for {}.", wiring.group);
        assert_eq!(
            attribute(message, "ERROR_MESSAGE").as_deref(),
            Some(expected_message.as_str()),
            "case {case:?}: expected the dead letter's ERROR_MESSAGE to be \
             {expected_message:?}, got {:?}",
            attribute(message, "ERROR_MESSAGE")
        );
        assert_eq!(
            attribute(message, "RULE_ARN").as_deref(),
            Some(wiring.rule_arn.as_str()),
            "case {case:?}: expected the dead letter's RULE_ARN to name {:?}, got {:?}",
            wiring.rule_arn,
            attribute(message, "RULE_ARN")
        );
        assert_eq!(
            attribute(message, "TARGET_ARN").as_deref(),
            Some(wiring.group_arn.as_str()),
            "case {case:?}: expected the dead letter's TARGET_ARN to name {:?}, got {:?}",
            wiring.group_arn,
            attribute(message, "TARGET_ARN")
        );
        // A permanent refusal retried nothing, so it accounts for no retries at all; the
        // retryable one is retried under the policy and names both.
        let retried = expected_code == EVENTS_IN_BATCH_REQUEST_REJECTED;
        for name in [RETRY_ATTEMPTS, EXHAUSTED_RETRY_CONDITION] {
            assert_eq!(
                attribute(message, name).is_some(),
                retried,
                "case {case:?}: expected the dead letter to {} a {name} attribute, got {:?}",
                if retried { "carry" } else { "carry no" },
                attribute(message, name)
            );
        }
    }
}

/// One row of the retry table: the bounds the target's `RetryPolicy` carries, and the retry
/// accounting the dead letter of a delivery the group would not hold YET must report.
struct RetryRow {
    case: &'static str,
    name: &'static str,
    /// `MaximumRetryAttempts` (`range 0..185`).
    maximum_retry_attempts: i64,
    /// `MaximumEventAgeInSeconds` (`range 60..86400`).
    maximum_event_age: i64,
    /// `RETRY_ATTEMPTS`: how many retries the delivery really made.
    attempts: &'static str,
    /// `EXHAUSTED_RETRY_CONDITION`: which of the two bounds ended it.
    exhausted: &'static str,
}

/// The retry accounting a dead letter reports is the delivery's own: the retries it made under
/// the target's allowance, and the bound that ended them — the allowance when it is the
/// smaller, the event's age bound when the retry pacing reaches it first.
#[tokio::test]
async fn an_ahead_of_window_log_group_delivery_reports_the_retries_its_policy_bounded() {
    let rows = [
        RetryRow {
            case: "a target allowing no retry at all is stopped by its allowance",
            name: "no-retry",
            maximum_retry_attempts: 0,
            maximum_event_age: 60,
            attempts: "0",
            exhausted: MAXIMUM_RETRY_ATTEMPTS,
        },
        RetryRow {
            case: "a target allowing two retries makes both and is stopped by its allowance",
            name: "two-retries",
            maximum_retry_attempts: 2,
            maximum_event_age: 60,
            attempts: "2",
            exhausted: MAXIMUM_RETRY_ATTEMPTS,
        },
        RetryRow {
            // 1 + 2 + 4 + 8 + 16 seconds of exponential pacing fall inside the smallest age
            // bound the model admits, and the sixth retry would be made at 63 seconds —
            // past it — so the age bound ends this delivery, not the allowance.
            case: "a target whose allowance outlasts the event's age bound is stopped by the \
                   age bound",
            name: "age-bound",
            maximum_retry_attempts: 185,
            maximum_event_age: 60,
            attempts: "5",
            exhausted: MAXIMUM_EVENT_AGE,
        },
    ];
    let router = router();
    for row in rows {
        let case = row.case;
        let wiring = wire(
            &router,
            case,
            row.name,
            None,
            row.maximum_retry_attempts,
            row.maximum_event_age,
        )
        .await;
        // Three hours ahead of the window the group ingests, so every attempt this delivery
        // makes is refused and the retry bounds are what end it.
        publish(&router, case, epoch_now() + 3 * HOUR).await;

        let dead_letters = collect(&router, case, &wiring.dead_letter_url, 1).await;
        assert_eq!(
            dead_letters.len(),
            1,
            "case {case:?}: expected exactly one dead letter, got {dead_letters:?}"
        );
        let message = &dead_letters[0];
        assert_eq!(
            attribute(message, "ERROR_CODE").as_deref(),
            Some(EVENTS_IN_BATCH_REQUEST_REJECTED),
            "case {case:?}: expected the dead letter's ERROR_CODE to be \
             {EVENTS_IN_BATCH_REQUEST_REJECTED:?}, got {:?}",
            attribute(message, "ERROR_CODE")
        );
        assert_eq!(
            attribute(message, RETRY_ATTEMPTS).as_deref(),
            Some(row.attempts),
            "case {case:?}: expected {RETRY_ATTEMPTS} {:?}, got {:?}",
            row.attempts,
            attribute(message, RETRY_ATTEMPTS)
        );
        assert_eq!(
            attribute(message, EXHAUSTED_RETRY_CONDITION).as_deref(),
            Some(row.exhausted),
            "case {case:?}: expected {EXHAUSTED_RETRY_CONDITION} {:?}, got {:?}",
            row.exhausted,
            attribute(message, EXHAUSTED_RETRY_CONDITION)
        );
    }
}
