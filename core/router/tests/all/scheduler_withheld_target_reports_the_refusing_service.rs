// SPDX-License-Identifier: CC0-1.0

//! WHAT a dead-letter event says about an occurrence whose call the schedule's own
//! execution role withheld, judged where a client reads it: on the Amazon SQS queue
//! `Target.DeadLetterConfig.Arn` names.
//!
//! The rule pinned here: **`Target.RoleArn` is "the IAM role that EventBridge Scheduler
//! will use for this target when the schedule is invoked", so every occurrence's call is
//! made AS that role and is refused by the TARGET's own service when that role's policies
//! withhold it; `Target.DeadLetterConfig` is the queue EventBridge Scheduler "delivers
//! failed events that could not be successfully delivered to a target" to, and the
//! `ERROR_CODE` such an event carries is the code the REFUSING SERVICE answers that call
//! with — the invoked operation's own modelled access refusal where its model declares one,
//! and otherwise the code that service's front door words AWS's authorization refusal in —
//! never one generic code minted for every service. The same event names the schedule it
//! belongs to in `SCHEDULE_ARN` and the target it was for in `TARGET_ARN`.**
//!
//! Only a cross-service suite can pin it: the schedule and the occurrence are EventBridge
//! Scheduler's, the refusal belongs to the target's own service (Amazon SNS, Amazon SQS),
//! the execution role is IAM's, and the dead-letter queue is Amazon SQS's.
//!
//! The rows differ along what the rule generalises over: the refusing SERVICE, the KIND of
//! target naming its call (a templated target, whose ARN names the resource, and a
//! universal target, whose ARN names the operation itself), and which of the three
//! spellings of that refusal the answering service uses.
//!
//! Nothing here waits for a fixed span: every schedule names an instant seconds ahead, and
//! the suite looks again for the CONDITION — the events the dead-letter queue holds, the
//! envelopes the subscribed queue holds — until a generous deadline passes.

use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for.
const REGION: &str = "eu-west-2";

/// The account every emulated resource here belongs to.
const ACCOUNT: &str = "000000000000";

/// The service principals the two hand-overs are made as: EventBridge Scheduler assumes
/// the schedule's execution role to make the occurrence's call, and Amazon SNS fans a
/// published notification out to a subscribed queue as its own principal.
const SCHEDULER_PRINCIPAL: &str = "scheduler.amazonaws.com";
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";

/// The code Amazon SNS answers a `Publish` its caller may not make with: that service
/// MODELS the refusal itself — Smithy (com.amazonaws.sns) gives `Publish` the error
/// `AuthorizationErrorException`, whose `awsQueryError` code, the one an SDK surfaces, is
/// this, under HTTP 403 — and declares no `AccessDeniedException` on that operation at all.
const SNS_AUTHORIZATION_ERROR: &str = "AuthorizationError";

/// The same refusal as answered by a QUERY-COMPATIBLE service: Amazon SQS's model carries
/// `awsQueryCompatible` (Smithy, com.amazonaws.sqs), so an SDK surfaces the legacy Query
/// code for its errors, and AWS's Query code for an unauthorized call is this.
const QUERY_ACCESS_DENIED: &str = "AccessDenied";

/// The same refusal as answered for a service whose front door spells it the modern way,
/// which is AWS's own code for a call its authorization layer withholds.
const ACCESS_DENIED_EXCEPTION: &str = "AccessDeniedException";

/// How long the suite looks for the events and envelopes its rows expect. Every schedule
/// below names an instant seconds ahead, so this is room for the clock's tick, the refusal
/// and the deliveries on a loaded host rather than a latency expectation of its own — it is
/// reached only where nothing was reported at all.
const PATIENCE: Duration = Duration::from_secs(180);

/// How long one pass of the queues sleeps before looking again.
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// How far ahead each schedule's one occurrence is placed.
const OCCURRENCE_IN: Duration = Duration::from_secs(4);

/// The services this flow spans, on ONE engine exactly as `core/router/src/lib.rs` mounts
/// them.
fn emulator() -> MultiServiceRouter {
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_scheduler_awschronosservice::EmulatedService::new(
            std::sync::Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(
            std::sync::Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        std::sync::Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked: this router runs with request authentication
/// off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One Amazon SQS operation over its `awsJson1_0` wire, which must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header("authorization", authorization("sqs"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "case {case:?}: SQS {operation} answered {status} {payload}"
    );
    payload
}

/// One awsQuery operation of `service` over its form wire, as its status and XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
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
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One Amazon SNS operation (API version 2010-03-31) that must answer 200.
async fn sns(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, "sns", "2010-03-31", action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: SNS {action} answered {status} {body}"
    );
    body
}

/// One IAM operation (API version 2010-05-08) that must answer 200.
async fn iam(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, "iam", "2010-05-08", action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: IAM {action} answered {status} {body}"
    );
    body
}

/// One EventBridge Scheduler operation over its `restJson1` wire.
async fn scheduler(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body
        .map(|body| Bytes::from(body.to_string()))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("scheduler.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("scheduler"))
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// A queue of this account: its url and the ARN a schedule names it by.
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {created}"))
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
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {attributes}"))
        .to_string();
    (url, arn)
}

/// A topic of this account, by the ARN Amazon SNS reports for it.
async fn given_topic(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let created = sns(router, case, "CreateTopic", &[("Name", name)]).await;
    element(&created, "TopicArn")
}

/// An SQS queue subscribed to `topic_arn`, carrying the policy AWS documents for a queue a
/// topic delivers to: the SNS service principal's own `sqs:SendMessage`, scoped to that
/// topic.
async fn given_subscriber(
    router: &MultiServiceRouter,
    case: &str,
    topic_arn: &str,
    queue: &str,
) -> String {
    let (url, queue_arn) = given_queue(router, case, queue).await;
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowSNSDelivery",
            "Effect": "Allow",
            "Principal": { "Service": SNS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
            "Condition": { "ArnEquals": { "aws:SourceArn": topic_arn } },
        }],
    })
    .to_string();
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
    )
    .await;
    sns(
        router,
        case,
        "Subscribe",
        &[
            ("TopicArn", topic_arn),
            ("Protocol", "sqs"),
            ("Endpoint", &queue_arn),
        ],
    )
    .await;
    url
}

/// A role EventBridge Scheduler may assume, granted each `(action, resource)` pair: the
/// pair AWS documents for an execution role — a trust policy for the scheduler service
/// principal, and the permissions the schedule's own calls need.
async fn given_execution_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    grants: &[(&str, &str)],
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": SCHEDULER_PRINCIPAL },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    iam(
        router,
        case,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    let statements: Vec<Value> = grants
        .iter()
        .map(|(action, resource)| {
            json!({ "Effect": "Allow", "Action": action, "Resource": resource })
        })
        .collect();
    let policy = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
    iam(
        router,
        case,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "invoke-the-target"),
            ("PolicyDocument", &policy),
        ],
    )
    .await;
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// `at` as an `at(...)` expression's body states it: a local date and time with no offset,
/// read in the zone the schedule states.
fn at_expression_body(at: SystemTime) -> String {
    let rendered = ::aws_smithy_types::DateTime::from(at)
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .expect("an instant renders as ISO-8601");
    rendered.trim_end_matches('Z').to_string()
}

/// A whole number of seconds ahead of now, which is the resolution an `at(...)` expression
/// states an instant at.
fn whole_seconds_ahead(ahead: Duration) -> SystemTime {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    SystemTime::UNIX_EPOCH + Duration::from_secs(now) + ahead
}

/// A one-time ENABLED schedule whose target is `target`, carrying `input`, reporting its
/// failed events to `dead_letter_arn` — answered with the ARN EventBridge Scheduler holds
/// it under, which is what its own dead-letter event has to name.
///
/// The schedule admits no retry (`MaximumRetryAttempts` 0, the lowest the member's modelled
/// `range 0..185` admits), so the one attempt the occurrence makes is its last and its
/// report is owed as soon as that attempt is refused.
async fn given_schedule(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    target: &str,
    role: &str,
    dead_letter_arn: &str,
    input: &str,
) -> String {
    let (status, created) = scheduler(
        router,
        "POST",
        &format!("/schedules/{name}"),
        Some(json!({
            "ScheduleExpression":
                format!("at({})", at_expression_body(whole_seconds_ahead(OCCURRENCE_IN))),
            "ScheduleExpressionTimezone": "UTC",
            "FlexibleTimeWindow": { "Mode": "OFF" },
            "State": "ENABLED",
            "Target": {
                "Arn": target,
                "RoleArn": role,
                "Input": input,
                "RetryPolicy": { "MaximumRetryAttempts": 0 },
                "DeadLetterConfig": { "Arn": dead_letter_arn },
            },
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateSchedule answered {status} {created}"
    );
    let arn = created["ScheduleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no ScheduleArn in {created}"))
        .to_string();
    arn
}

/// Every message the queue at `url` holds, with its attributes, left ON the queue: each
/// pass of a poll loop reads the same events again, so a report is never consumed by the
/// look that found it.
async fn messages(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({
            "QueueUrl": url,
            "MaxNumberOfMessages": 10,
            "VisibilityTimeout": 0,
            "MessageAttributeNames": ["All"],
        }),
    )
    .await;
    received["Messages"].as_array().cloned().unwrap_or_default()
}

/// One message attribute of a dead-letter event, as the text it carries.
fn attribute(message: &Value, name: &str) -> String {
    message["MessageAttributes"][name]["StringValue"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Every event the dead-letter queue holds, keyed by the schedule its `SCHEDULE_ARN` names.
async fn reported_by_schedule(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
) -> BTreeMap<String, Value> {
    let mut reported = BTreeMap::new();
    for message in messages(router, case, url).await {
        reported.insert(attribute(&message, "SCHEDULE_ARN"), message);
    }
    reported
}

/// WHICH call a row's schedule names, which is what the refusal then belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Names {
    /// A TEMPLATED target: `Target.Arn` names the topic, and the call is Amazon SNS's
    /// `Publish` — "the templated target type for the Amazon SNS `Publish` API operation".
    Topic,
    /// A templated target naming a queue, whose call is Amazon SQS's `SendMessage`.
    Queue,
    /// A UNIVERSAL target naming that same publish outright, whose `Input` IS the
    /// `Publish` request.
    PublishCall,
    /// A universal target naming Step Functions' `StartExecution`, a call whose service
    /// models no access refusal of its own.
    StartExecutionCall,
}

/// One row: the call its schedule names, the IAM action that call is authorized by (which
/// no row's role is granted), and the code the refusing service answers it with.
struct Row {
    case: &'static str,
    names: Names,
    expected_code: &'static str,
}

/// The rows differ along the refusing service, along the KIND of target naming its call,
/// and along which spelling of AWS's authorization refusal the answering service uses.
const ROWS: &[Row] = &[
    Row {
        case: "a topic target, whose Publish models its own refusal",
        names: Names::Topic,
        expected_code: SNS_AUTHORIZATION_ERROR,
    },
    Row {
        case: "a universal target naming that same publish",
        names: Names::PublishCall,
        expected_code: SNS_AUTHORIZATION_ERROR,
    },
    Row {
        case: "a queue target, whose service is query-compatible",
        names: Names::Queue,
        expected_code: QUERY_ACCESS_DENIED,
    },
    Row {
        case: "a universal target of a service whose front door answers the modern code",
        names: Names::StartExecutionCall,
        expected_code: ACCESS_DENIED_EXCEPTION,
    },
];

/// Every row: the occurrence's call is withheld by the execution role, so nothing reaches
/// the target and the failed event reaches the schedule's dead-letter queue carrying the
/// REFUSING SERVICE's own code for that refusal, the schedule's own ARN and its target.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_withheld_occurrence_is_reported_under_the_refusing_services_own_code() {
    let router = emulator();
    let setup = "the resources every row's schedule targets";
    let (dead_letter_url, dead_letter_arn) =
        given_queue(&router, setup, "withheld-dead-letters").await;
    let topic_arn = given_topic(&router, setup, "withheld-topic").await;
    let (_, queue_arn) = given_queue(&router, setup, "withheld-target-queue").await;
    // A machine of this account that was never created: the role withholds the call, so it
    // is never made, and what the row turns on is the refusal rather than the resource the
    // request names.
    let machine_arn = format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:withheld-machine");
    // Every row's role may write to the dead-letter queue and NOTHING else, so every row's
    // own call is the one its permissions withhold.
    let role = given_execution_role(
        &router,
        setup,
        "withheld-role",
        &[("sqs:SendMessage", dead_letter_arn.as_str())],
    )
    .await;

    // (the row, the ARN EventBridge Scheduler holds its schedule under, its target)
    let mut written: Vec<(&Row, String, String)> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let marker = json!({ "marker": format!("withheld-{index}") }).to_string();
        let (target, input) = match row.names {
            Names::Topic => (topic_arn.clone(), marker),
            Names::Queue => (queue_arn.clone(), marker),
            Names::PublishCall => (
                "arn:aws:scheduler:::aws-sdk:sns:publish".to_string(),
                json!({ "TopicArn": topic_arn, "Message": format!("withheld-{index}") })
                    .to_string(),
            ),
            Names::StartExecutionCall => (
                "arn:aws:scheduler:::aws-sdk:sfn:startExecution".to_string(),
                json!({ "StateMachineArn": machine_arn, "Input": marker }).to_string(),
            ),
        };
        let schedule_arn = given_schedule(
            &router,
            case,
            &format!("withheld-{index}"),
            &target,
            &role,
            &dead_letter_arn,
            &input,
        )
        .await;
        written.push((row, schedule_arn, target));
    }

    // Look again for the CONDITION — an event for every schedule written — rather than
    // waiting a span: the occurrences fall seconds ahead, and the deadline only bounds how
    // long this looks.
    let began = Instant::now();
    let mut reported = reported_by_schedule(&router, setup, &dead_letter_url).await;
    while !written
        .iter()
        .all(|(_, schedule_arn, _)| reported.contains_key(schedule_arn))
    {
        assert!(
            began.elapsed() < PATIENCE,
            "every withheld occurrence must be reported to the dead-letter queue: expected \
             one event per schedule in {:?}, got {reported:?}",
            written
                .iter()
                .map(|(_, schedule_arn, _)| schedule_arn.clone())
                .collect::<Vec<_>>()
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
        reported = reported_by_schedule(&router, setup, &dead_letter_url).await;
    }

    for (row, schedule_arn, target) in &written {
        let case = row.case;
        let event = reported
            .get(schedule_arn)
            .unwrap_or_else(|| panic!("case {case:?}: no event names the schedule {schedule_arn}"));
        assert_eq!(
            attribute(event, "ERROR_CODE"),
            row.expected_code,
            "case {case:?}: expected the refusing service's code {:?}, got {event}",
            row.expected_code
        );
        assert_eq!(
            (
                attribute(event, "SCHEDULE_ARN"),
                attribute(event, "TARGET_ARN")
            ),
            (schedule_arn.clone(), target.clone()),
            "case {case:?}: expected the event to name the schedule {schedule_arn} and the \
             target {target}, got {event}"
        );
        // The refusal's own sentence names the SESSION the execution role was assumed
        // under, which is the principal the withheld call would have been made by.
        let refusal = attribute(event, "ERROR_MESSAGE");
        assert!(
            refusal.contains(&format!("arn:aws:sts::{ACCOUNT}:assumed-role/withheld-role/")),
            "case {case:?}: expected the refusal to name the assumed-role session, got \
             {refusal:?}"
        );
    }
    assert_eq!(
        reported.len(),
        written.len(),
        "expected one event per withheld occurrence and nothing else, got {reported:?}"
    );
}

/// The marker the permitted arm's `Target.Input` carries, and the one the denied arm's
/// does: what tells a delivery apart from the arm it belongs to.
const PERMITTED_MARKER: &str = "permitted-publish";
/// See [`PERMITTED_MARKER`].
const DENIED_MARKER: &str = "denied-publish";

/// The action an SNS refusal names, as that service words one: "an SNS action is
/// `SNS:<OperationName>`".
const REPORTED_PUBLISH_ACTION: &str = "SNS:Publish";

/// Two sibling schedules of ONE topic, identical in everything but the role their
/// occurrences are made as: the one whose role grants `sns:Publish` on the topic delivers a
/// `Notification` to the topic's subscriber, and the one whose role grants nothing but the
/// dead-letter write delivers NOTHING there and is reported on the dead-letter queue — under
/// the code Amazon SNS answers a refused `Publish` with, naming its OWN schedule and never
/// its sibling's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_denied_publish_is_reported_while_its_permitted_sibling_is_delivered() {
    let case = "two arms of one topic, differing only in their execution role";
    let router = emulator();
    let topic_arn = given_topic(&router, case, "two-arm-topic").await;
    let inbox_url = given_subscriber(&router, case, &topic_arn, "two-arm-inbox").await;
    let (dead_letter_url, dead_letter_arn) = given_queue(&router, case, "two-arm-dlq").await;
    let permitted_role = given_execution_role(
        &router,
        case,
        "two-arm-permitted",
        &[
            ("sns:Publish", topic_arn.as_str()),
            ("sqs:SendMessage", dead_letter_arn.as_str()),
        ],
    )
    .await;
    // The denied arm's role may report a failed event and nothing else: it carries no
    // `sns:Publish` anywhere.
    let denied_role = given_execution_role(
        &router,
        case,
        "two-arm-denied",
        &[("sqs:SendMessage", dead_letter_arn.as_str())],
    )
    .await;

    let mut arns = Vec::new();
    for (name, marker, role) in [
        ("two-arm-permitted", PERMITTED_MARKER, &permitted_role),
        ("two-arm-denied", DENIED_MARKER, &denied_role),
    ] {
        let input = json!({ "marker": marker, "at": "<aws.scheduler.scheduled-time>" }).to_string();
        arns.push(
            given_schedule(
                &router,
                case,
                name,
                &topic_arn,
                role,
                &dead_letter_arn,
                &input,
            )
            .await,
        );
    }
    let (permitted_arn, denied_arn) = (arns[0].clone(), arns[1].clone());

    // Look again until BOTH arms have been answered: an envelope carrying the permitted
    // marker on the subscriber, and an event for the denied schedule on the dead-letter
    // queue.
    let began = Instant::now();
    let mut delivered: BTreeMap<String, Value> = BTreeMap::new();
    let mut reported: BTreeMap<String, Value> = BTreeMap::new();
    loop {
        for envelope in messages(&router, case, &inbox_url).await {
            let id = envelope["MessageId"].as_str().unwrap_or_default().to_string();
            delivered.insert(id, envelope);
        }
        reported.extend(reported_by_schedule(&router, case, &dead_letter_url).await);
        let published = delivered.values().any(|envelope| {
            envelope["Body"]
                .as_str()
                .is_some_and(|body| body.contains(PERMITTED_MARKER))
        });
        if published && reported.contains_key(&denied_arn) {
            break;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "the permitted arm must publish and the denied arm must be reported: got the \
             envelopes {delivered:?} and the events {reported:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }

    // What the subscriber received is a `Notification` of that topic, carrying the
    // permitted arm's own `Target.Input` as its `Message` — and no envelope of the denied
    // arm ever arrived.
    for envelope in delivered.values() {
        let body = envelope["Body"].as_str().unwrap_or_default();
        let notification: Value = serde_json::from_str(body).unwrap_or_else(|error| {
            panic!("case {case:?}: the body is no envelope ({error}): {body}")
        });
        assert_eq!(
            (
                notification["Type"].as_str(),
                notification["TopicArn"].as_str(),
                notification["MessageId"].as_str().is_some(),
            ),
            (Some("Notification"), Some(topic_arn.as_str()), true),
            "case {case:?}: expected a Notification of {topic_arn} carrying a MessageId, got \
             {notification}"
        );
        let published = notification["Message"].as_str().unwrap_or_default();
        let payload: Value = serde_json::from_str(published).unwrap_or_else(|error| {
            panic!("case {case:?}: the published message is no Input ({error}): {published}")
        });
        assert_eq!(
            payload["marker"].as_str(),
            Some(PERMITTED_MARKER),
            "case {case:?}: the denied arm's marker must never reach the subscriber, got \
             {payload}"
        );
    }

    // The denied arm's report: the code Amazon SNS answers a refused `Publish` with, the
    // topic it was for, and its OWN schedule — never its sibling's, which was delivered and
    // is reported nowhere.
    let event = reported
        .get(&denied_arn)
        .unwrap_or_else(|| panic!("case {case:?}: no event names the denied schedule"));
    assert_eq!(
        (
            attribute(event, "ERROR_CODE"),
            attribute(event, "SCHEDULE_ARN"),
            attribute(event, "TARGET_ARN"),
        ),
        (
            SNS_AUTHORIZATION_ERROR.to_string(),
            denied_arn.clone(),
            topic_arn.clone(),
        ),
        "case {case:?}: expected the refused publish reported as {SNS_AUTHORIZATION_ERROR:?} \
         for {denied_arn} on {topic_arn}, got {event}"
    );
    let refusal = attribute(event, "ERROR_MESSAGE");
    for named in [
        &format!("arn:aws:sts::{ACCOUNT}:assumed-role/two-arm-denied/"),
        REPORTED_PUBLISH_ACTION,
        topic_arn.as_str(),
    ] {
        assert!(
            refusal.contains(named),
            "case {case:?}: expected the refusal to name {named:?}, got {refusal:?}"
        );
    }
    assert!(
        !reported.contains_key(&permitted_arn),
        "case {case:?}: the permitted arm's occurrence was delivered, so nothing of it is \
         reported, got {reported:?}"
    );
}
