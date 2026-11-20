// SPDX-License-Identifier: CC0-1.0

//! WHEN the notification an EventBridge Scheduler schedule publishes is STAMPED, judged
//! where a subscriber reads it: on the `Notification` envelope waiting on the Amazon SQS
//! queue subscribed to the Amazon SNS topic the schedule targets.
//!
//! The rule pinned here: **an occurrence is invoked AT the nominal instant its
//! `ScheduleExpression` names — read in `ScheduleExpressionTimezone`, UTC when the
//! schedule names none — and NEVER before it: with `FlexibleTimeWindow` `Mode=OFF` at
//! that instant, and with `Mode=FLEXIBLE` at an instant inside
//! `[nominal, nominal + MaximumWindowInMinutes]`, the window opening at the occurrence
//! and extending forward only. That same nominal instant is what the service renders
//! into the `<aws.scheduler.scheduled-time>` context attribute, so the instant the
//! notification is stamped at is at or after the scheduled time the very same
//! notification reports.**
//!
//! Four services have to agree for a subscriber to observe that (EventBridge Scheduler
//! evaluates the occurrence and publishes, IAM holds the execution role the publish is
//! made under, Amazon SNS stamps the notification and fans it out, Amazon SQS holds what
//! arrived), so the suite lives here rather than in any one crate. What it reads is what
//! a subscriber reads: the envelope's own `Timestamp` beside the scheduled time
//! substituted into the published message. A subscriber keying idempotency, windowing or
//! ordering on that attribute is entitled to the two agreeing.
//!
//! The rows differ along what the rule generalises over: the FORM of the expression (a
//! one-time instant and a cron expression pinned to a minute boundary), whether the
//! schedule states a `ScheduleExpressionTimezone` at all, and the `FlexibleTimeWindow`
//! `Mode` the occurrence is invoked under (`OFF`, and `FLEXIBLE` with the shortest
//! window the member admits). What the occurrence computation names over the whole class
//! of expressions, zones and window lengths is asserted without a wait in the scheduler
//! crate's own `src/state/invocation.rs` tests.
//!
//! Nothing is compressed and nothing here sleeps for an occurrence: each row's schedule
//! falls due seconds ahead of the write, the suite polls every row's queue for the
//! CONDITION — a notification carrying that row's marker — and the deadline only bounds
//! how long it looks.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_scheduler_awschronosservice::state::invocation;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for.
const REGION: &str = "eu-west-2";

/// The account every emulated resource here belongs to.
const ACCOUNT: &str = "000000000000";

/// The service principals the two hand-overs are made as: EventBridge Scheduler assumes
/// the schedule's execution role to publish, and Amazon SNS fans the published
/// notification out to the subscribed queue as its own principal.
const SCHEDULER_PRINCIPAL: &str = "scheduler.amazonaws.com";
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";

/// How long the suite keeps looking past the last instant a row's occurrence can be
/// invoked at: room for the clock's tick, the publish, the fan-out and the poll itself on
/// a loaded host. Nothing is asserted about how long any of that took — this only bounds
/// how long the suite looks.
const DELIVERY_SLACK: Duration = Duration::from_secs(60);

/// How long one poll of a row's queue sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// How far ahead of the write a one-time row states its instant: far enough that the
/// schedule is written before the instant it names, and short enough that the suite's own
/// wait is a handful of seconds. Each row checks the instant is still ahead of the clock
/// once the schedule is written, so a host slow enough to invalidate the lead says so
/// rather than failing on the rule.
const ONE_TIME_LEAD: Duration = Duration::from_secs(12);

/// How far ahead of the write the cron row's pinned minute boundary stands, for the same
/// reason.
const CRON_LEAD: Duration = Duration::from_secs(20);

/// Seconds in the minute `MaximumWindowInMinutes` states its window in.
const SECONDS_PER_MINUTE: u64 = 60;

/// One row: the schedule written, and the window its occurrence may be invoked inside.
struct Row {
    case: &'static str,
    /// The expression and the instant it names, built when the row RUNS so that instant
    /// is still ahead of the clock. It is derived by the suite from the expression it
    /// wrote — never read back off the emulator.
    expression: fn() -> (String, SystemTime),
    /// `ScheduleExpressionTimezone`, or `None` for a request that omits the member.
    timezone: Option<&'static str>,
    /// `MaximumWindowInMinutes` under `Mode=FLEXIBLE`, or `None` for `Mode=OFF`.
    window_minutes: Option<i64>,
}

/// The rows differ along the form of the expression, along whether the schedule states
/// the zone its expression is read in, and along the mode the occurrence is invoked
/// under.
const ROWS: &[Row] = &[
    Row {
        case: "a one-time instant in UTC under Mode=OFF",
        expression: || {
            let at = whole_seconds_ahead(ONE_TIME_LEAD);
            (format!("at({})", at_expression_body(at)), at)
        },
        timezone: Some("UTC"),
        window_minutes: None,
    },
    Row {
        case: "a one-time instant stating no timezone at all under Mode=OFF",
        expression: || {
            let at = whole_seconds_ahead(ONE_TIME_LEAD);
            (format!("at({})", at_expression_body(at)), at)
        },
        timezone: None,
        window_minutes: None,
    },
    Row {
        case: "a cron expression pinned to a minute boundary under Mode=OFF",
        expression: || {
            let at = next_minute_boundary(CRON_LEAD);
            (cron_pinned_to(at), at)
        },
        timezone: Some("UTC"),
        window_minutes: None,
    },
    Row {
        case: "a one-time instant inside the shortest flexible window",
        expression: || {
            let at = whole_seconds_ahead(ONE_TIME_LEAD);
            (format!("at({})", at_expression_body(at)), at)
        },
        timezone: None,
        window_minutes: Some(1),
    },
];

/// What one row holds while the suite watches it.
struct Watched {
    /// The queue subscribed to the row's topic.
    url: String,
    /// The marker the row's `Target.Input` carries, which identifies a delivery as this
    /// row's.
    marker: String,
    /// The topic the row's schedule publishes to, which every envelope names.
    topic_arn: String,
    /// The expression the row wrote, named in every assertion below.
    expression: String,
    /// The instant that expression names, derived by the suite from the expression it
    /// wrote.
    nominal: SystemTime,
    /// The window the occurrence may be invoked inside, which is `ZERO` under `Mode=OFF`.
    window: Duration,
    /// The first notification of this row's marker: the instant it was STAMPED at, and
    /// the scheduled time the same notification reports.
    delivered: Option<(SystemTime, SystemTime)>,
}

/// The four services this flow spans, on ONE engine exactly as `core/router/src/lib.rs`
/// mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_scheduler_awschronosservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: this router runs with request
/// authentication off.
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

/// A topic of this account with an SQS queue subscribed to it, as
/// `(the topic's ARN, the queue's url)`.
///
/// The queue carries the policy AWS documents for a queue subscribed to a topic: the SNS
/// service principal's own `sqs:SendMessage`, scoped to that topic.
async fn topic_with_subscriber(
    router: &MultiServiceRouter,
    case: &str,
    topic: &str,
    queue: &str,
) -> (String, String) {
    let created = sns(router, case, "CreateTopic", &[("Name", topic)]).await;
    let topic_arn = element(&created, "TopicArn");
    let answered = sqs(router, case, "CreateQueue", &json!({ "QueueName": queue })).await;
    let url = answered["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {answered}"))
        .to_string();
    let queue_arn = format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{queue}");
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
            ("TopicArn", &topic_arn),
            ("Protocol", "sqs"),
            ("Endpoint", &queue_arn),
        ],
    )
    .await;
    (topic_arn, url)
}

/// A role EventBridge Scheduler may assume to publish to `topic_arn`: the pair AWS
/// documents for an execution role — a trust policy for the scheduler service principal
/// and `sns:Publish` on the target.
async fn execution_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    topic_arn: &str,
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
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": "sns:Publish",
            "Resource": topic_arn,
        }],
    })
    .to_string();
    iam(
        router,
        case,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "publish-to-target"),
            ("PolicyDocument", &policy),
        ],
    )
    .await;
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// A whole number of seconds ahead of now: the resolution an expression states its
/// instants at, so a reading's sub-second part is never the difference.
fn whole_seconds_ahead(ahead: Duration) -> SystemTime {
    let seconds = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    SystemTime::UNIX_EPOCH + Duration::from_secs(seconds) + ahead
}

/// The body of the `at(yyyy-mm-ddThh:mm:ss)` expression that names `at`, which
/// `CreateSchedule$ScheduleExpression` documents as a local date and time with no zone of
/// its own: the published instant form without the marker that makes it UTC.
fn at_expression_body(at: SystemTime) -> String {
    invocation::scheduled_time_text(at)
        .strip_suffix('Z')
        .unwrap_or_default()
        .to_string()
}

/// The first minute boundary at least `ahead` from now: the instant a client pins a
/// `cron(...)` expression to.
fn next_minute_boundary(ahead: Duration) -> SystemTime {
    let seconds = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut boundary = seconds - seconds % SECONDS_PER_MINUTE + SECONDS_PER_MINUTE;
    while boundary < seconds + ahead.as_secs() {
        boundary += SECONDS_PER_MINUTE;
    }
    SystemTime::UNIX_EPOCH + Duration::from_secs(boundary)
}

/// A `cron(...)` expression pinned to the minute `at` falls on, in the six documented
/// fields "Minutes Hours Day-of-month Month Day-of-week Year": read in UTC, the next
/// instant it names is that very minute.
fn cron_pinned_to(at: SystemTime) -> String {
    let seconds = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let minute = (seconds / SECONDS_PER_MINUTE) % 60;
    let hour = (seconds / (SECONDS_PER_MINUTE * 60)) % 24;
    format!("cron({minute} {hour} * * ? *)")
}

/// `FlexibleTimeWindow` as a row states it: `Mode=FLEXIBLE` with the window when the row
/// asks for one, and `Mode=OFF` — which admits no `MaximumWindowInMinutes` at all — when
/// it does not.
fn flexible_time_window(window_minutes: Option<i64>) -> Value {
    match window_minutes {
        Some(minutes) => json!({ "Mode": "FLEXIBLE", "MaximumWindowInMinutes": minutes }),
        None => json!({ "Mode": "OFF" }),
    }
}

/// The instant a notification's own `Timestamp` states — "the time (GMT) when the
/// notification was published" — read from the envelope a subscriber receives.
fn stamped_at(case: &str, envelope: &Value) -> SystemTime {
    let text = envelope["Timestamp"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the notification carries no Timestamp: {envelope}"));
    let parsed = ::aws_smithy_types::DateTime::from_str(
        text,
        ::aws_smithy_types::date_time::Format::DateTime,
    )
    .unwrap_or_else(|error| {
        panic!("case {case:?}: the Timestamp {text:?} is not an instant ({error})")
    });
    SystemTime::try_from(parsed).unwrap_or_else(|_| {
        panic!("case {case:?}: the Timestamp {text:?} is not a clock reading")
    })
}

/// Take every notification waiting on `watched`'s queue, keeping the FIRST one that
/// carries this row's marker, and leave the queue empty so a later look reads only what
/// arrived since.
///
/// Each envelope is held to what a subscriber of a topic receives: a `Notification` of
/// that topic carrying a message id, with the schedule's own `Target.Input` as its
/// `Message` — the row's marker and the occurrence's scheduled time, substituted.
async fn collect(router: &MultiServiceRouter, case: &str, watched: &mut Watched) {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({ "QueueUrl": watched.url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    for message in received["Messages"].as_array().cloned().unwrap_or_default() {
        let body = message["Body"].as_str().unwrap_or_default().to_string();
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                router,
                case,
                "DeleteMessage",
                &json!({ "QueueUrl": watched.url, "ReceiptHandle": handle }),
            )
            .await;
        }
        let envelope: Value = serde_json::from_str(&body)
            .unwrap_or_else(|error| panic!("case {case:?}: the body is not JSON ({error}): {body}"));
        assert_eq!(
            (
                envelope["Type"].as_str(),
                envelope["TopicArn"].as_str(),
                envelope["MessageId"].as_str().is_some()
            ),
            (
                Some("Notification"),
                Some(watched.topic_arn.as_str()),
                true
            ),
            "case {case:?}: expected a Notification of {:?} carrying a MessageId, got {envelope}",
            watched.topic_arn
        );
        let published = envelope["Message"].as_str().unwrap_or_default().to_string();
        let payload: Value = serde_json::from_str(&published).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: the published message is not the schedule's Input ({error}): \
                 {published}"
            )
        });
        assert_eq!(
            payload["marker"].as_str(),
            Some(watched.marker.as_str()),
            "case {case:?}: expected the schedule's own marker {:?}, got {payload}",
            watched.marker
        );
        let stated = payload["at"].as_str().unwrap_or_default();
        let scheduled = invocation::scheduled_time_of(stated).unwrap_or_else(|| {
            panic!("case {case:?}: expected a substituted scheduled time, got {stated:?}")
        });
        if watched.delivered.is_none() {
            watched.delivered = Some((stamped_at(case, &envelope), scheduled));
        }
    }
}

/// Every row: the occurrence reaches the subscriber of the topic the schedule targets,
/// reporting as its scheduled time the instant the expression names, and the notification
/// carrying that scheduled time is stamped at or after it — never before.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_notification_is_never_stamped_before_the_scheduled_time_it_reports() {
    let router = emulator();
    let mut watching: Vec<Watched> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let (topic_arn, url) = topic_with_subscriber(
            &router,
            case,
            &format!("stamp-topic-{index}"),
            &format!("stamp-queue-{index}"),
        )
        .await;
        let role = execution_role(&router, case, &format!("stamp-role-{index}"), &topic_arn).await;
        let marker = format!("stamp-{index}");
        let (expression, nominal) = (row.expression)();
        let input = json!({ "marker": marker, "at": "<aws.scheduler.scheduled-time>" }).to_string();
        let mut body = json!({
            "ScheduleExpression": expression,
            "FlexibleTimeWindow": flexible_time_window(row.window_minutes),
            "State": "ENABLED",
            "Target": { "Arn": topic_arn, "RoleArn": role, "Input": input },
        });
        if let Some(timezone) = row.timezone {
            body["ScheduleExpressionTimezone"] = json!(timezone);
        }
        let (status, created) = scheduler(
            &router,
            "POST",
            &format!("/schedules/stamp-{index}"),
            Some(body),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateSchedule answered {status} {created}"
        );
        assert!(
            nominal > SystemTime::now(),
            "case {case:?}: the instant {:?} the expression {expression:?} names was already \
             behind the clock once the schedule was written, so this row's setup took longer \
             than its own lead",
            invocation::scheduled_time_text(nominal)
        );
        watching.push(Watched {
            url,
            marker,
            topic_arn,
            expression,
            nominal,
            window: Duration::from_secs(
                u64::try_from(row.window_minutes.unwrap_or_default()).unwrap_or_default()
                    * SECONDS_PER_MINUTE,
            ),
            delivered: None,
        });
    }

    // Every row's occurrence falls inside the window that opens at the instant its own
    // expression names; the rest is room for the clock's tick and the two hand-overs.
    // Each pass looks again for the CONDITION — a notification of the row's marker — and
    // the deadline only bounds how long the suite looks.
    let deadline = watching
        .iter()
        .map(|watched| watched.nominal + watched.window)
        .max()
        .unwrap_or_else(SystemTime::now)
        + DELIVERY_SLACK;
    loop {
        let mut delivered = true;
        for (index, watched) in watching.iter_mut().enumerate() {
            if watched.delivered.is_none() {
                collect(&router, ROWS[index].case, watched).await;
            }
            delivered &= watched.delivered.is_some();
        }
        if delivered || SystemTime::now() >= deadline {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    for (index, watched) in watching.iter().enumerate() {
        let case = ROWS[index].case;
        let (stamped, scheduled) = watched.delivered.unwrap_or_else(|| {
            panic!(
                "case {case:?}: the expression {:?} delivered no notification of {:?} to the \
                 subscriber",
                watched.expression, watched.marker
            )
        });
        // The scheduled time the delivery reports is the instant the expression names:
        // the occurrence's own instant, not the instant it happened to be published at.
        assert_eq!(
            invocation::scheduled_time_text(scheduled),
            invocation::scheduled_time_text(watched.nominal),
            "case {case:?}: expected the occurrence of {:?} to report the instant it names, got \
             {:?}",
            watched.expression,
            invocation::scheduled_time_text(scheduled)
        );
        // The rule: the stamp is at or after the scheduled time the same notification
        // reports, under either mode.
        assert!(
            stamped >= scheduled,
            "case {case:?}: the notification of {:?} was stamped {:?}, BEFORE the scheduled time \
             {:?} it reports itself",
            watched.expression,
            invocation::scheduled_time_text(stamped),
            invocation::scheduled_time_text(scheduled)
        );
    }
}
