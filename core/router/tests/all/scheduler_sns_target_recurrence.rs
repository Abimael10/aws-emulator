// SPDX-License-Identifier: CC0-1.0

//! WHEN a recurring schedule's series OPENS, judged where a client sees it: on the
//! Amazon SQS queue subscribed to the Amazon SNS topic an EventBridge Scheduler
//! schedule targets.
//!
//! The rule pinned here: **a schedule's series opens at the instant it STARTS — its
//! `StartDate`, or the instant the schedule was written when it states none — and its
//! first occurrence is the first instant its `ScheduleExpression` names at or after
//! that one. A `rate(value unit)` expression states an interval and no instant of its
//! own, so the instant the series opens at IS its first occurrence and the interval is
//! the gap between consecutive occurrences, never a wait before the first; a
//! `cron(fields)` expression names its own instants, which the opening instant only
//! bounds. Every occurrence of an `ENABLED` schedule is invoked, so the occurrence
//! after the first reaches the target too.**
//!
//! Three services have to agree for that to hold (EventBridge Scheduler evaluates the
//! series and publishes, Amazon SNS fans the notification out to its subscriptions,
//! Amazon SQS holds what arrived, and IAM holds the execution role the publish is made
//! under), so the suite lives here rather than in any one crate, and what it reads is
//! what a subscriber reads: the `Notification` envelopes on the queue.
//!
//! The rows differ along what the rule generalises over: the FORM of the expression (a
//! rate and a cron expression), the rate's own value and unit, and whether the schedule
//! states a `StartDate` the series has to open at instead. Each row is judged on the
//! instants the SERVER named — the `<aws.scheduler.scheduled-time>` each delivery
//! carries — rather than on how long this host took: a delivery's own scheduled time
//! says where the series opened and how far apart its occurrences are. Nothing is
//! compressed, so the suite polls every row's queue for the CONDITION until a generous
//! deadline passes.

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

/// The service principals the two hand-overs are made as: EventBridge Scheduler
/// assumes the schedule's execution role to publish, and Amazon SNS fans the published
/// notification out to the subscribed queue as its own principal.
const SCHEDULER_PRINCIPAL: &str = "scheduler.amazonaws.com";
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";

/// The shortest cadence a `rate(...)` expression can state ("minute | minutes | hour |
/// hours | day | days", `CreateSchedule$ScheduleExpression`), and the one every
/// minute-grained row below is spaced by.
const MINUTE: Duration = Duration::from_secs(60);

/// How long the suite keeps polling for the occurrences its rows expect, past the
/// instants those occurrences fall on: room for the clock's tick, the publish, the
/// fan-out and the poll itself on a loaded host. Nothing is asserted about how long any
/// of that took — this only bounds how long the suite looks.
const DELIVERY_SLACK: Duration = Duration::from_secs(40);

/// How far ahead of the write the `StartDate` row states its floor: far enough that every
/// row's setup can have run before it on a loaded host, and short enough that one poll loop
/// can watch it. The row checks the floor is still ahead of the clock once the schedule is
/// written, so a host slow enough to invalidate the lead says so rather than failing on the
/// rule.
const START_DATE_AHEAD: Duration = Duration::from_secs(45);

/// How long one poll of a row's queue sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// WHERE the rule says a row's series opens, which is what each row's first occurrence
/// is judged against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Opens {
    /// At the instant the schedule was written: a `rate(...)` series stating no
    /// `StartDate`.
    Write,
    /// At the `StartDate` itself: a `rate(...)` series whose floor is ahead of the
    /// write.
    StartDate,
    /// At the first instant the expression itself names at or after the write, which
    /// for a cron expression naming every minute is the next minute boundary.
    ExpressionsNextInstant,
}

/// One row: the schedule written, where its series opens, and how many of its
/// occurrences the suite collects.
struct Row {
    case: &'static str,
    expression: &'static str,
    /// The period the expression states, which consecutive occurrences are spaced by.
    period: Duration,
    /// Whether the schedule states a `StartDate` ahead of the write.
    start_date: bool,
    opens: Opens,
    /// How many occurrences this row is judged on: two where the row also holds the
    /// series to its cadence, one where only the opening instant is in question.
    wanted: usize,
}

/// The rows differ along the form of the expression, the rate's value and unit, and
/// whether a `StartDate` moves where the series opens.
const ROWS: &[Row] = &[
    Row {
        case: "a rate of one minute, stating no StartDate",
        expression: "rate(1 minute)",
        period: MINUTE,
        start_date: false,
        opens: Opens::Write,
        wanted: 2,
    },
    Row {
        case: "a rate of two minutes, stating no StartDate",
        expression: "rate(2 minutes)",
        period: Duration::from_secs(120),
        start_date: false,
        opens: Opens::Write,
        wanted: 1,
    },
    Row {
        case: "a rate of one minute whose StartDate is ahead of the write",
        expression: "rate(1 minute)",
        period: MINUTE,
        start_date: true,
        opens: Opens::StartDate,
        wanted: 1,
    },
    Row {
        case: "a cron expression naming every minute of every hour",
        expression: "cron(* * * * ? *)",
        period: MINUTE,
        start_date: false,
        opens: Opens::ExpressionsNextInstant,
        wanted: 1,
    },
];

/// What one row holds while the suite watches it.
struct Watched {
    /// The queue subscribed to the row's topic.
    url: String,
    /// The marker the row's `Target.Input` carries, which identifies a delivery as
    /// this row's.
    marker: String,
    /// The topic the row's schedule publishes to, which every envelope names.
    topic_arn: String,
    /// The instant the suite read just before writing the schedule: the latest instant
    /// the series can have opened at.
    written: SystemTime,
    /// The `StartDate` the row stated, when it stated one.
    start_date: Option<SystemTime>,
    /// Each occurrence's own scheduled time, in order, as the deliveries reported it.
    occurrences: Vec<SystemTime>,
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
/// The queue carries the policy AWS documents for a queue subscribed to a topic: the
/// SNS service principal's own `sqs:SendMessage`, scoped to that topic.
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

/// One instant as the seconds-since-the-epoch a `restJson1` timestamp member carries.
fn epoch_seconds(at: SystemTime) -> f64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// A whole number of seconds ahead of now, which is the resolution the instants a
/// schedule is bounded by are compared at.
fn whole_seconds_ahead(ahead: Duration) -> SystemTime {
    to_the_second(SystemTime::now()) + ahead
}

/// `at` as the scheduled-time attribute states it: to the SECOND, which is the
/// resolution EventBridge Scheduler publishes an occurrence's own instant at
/// (`2022-03-10T13:00:00Z`).
///
/// A reading taken by this suite is compared against a delivered occurrence at that
/// resolution, so the sub-second part of a reading is never the difference.
fn to_the_second(at: SystemTime) -> SystemTime {
    let seconds = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
}

/// Take every envelope waiting on `watched`'s queue, recording the occurrence each one
/// belongs to, and leaving the queue empty so a later look reads only what arrived
/// since.
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
            panic!("case {case:?}: the published message is not the schedule's Input ({error}): {published}")
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
        if !watched.occurrences.contains(&scheduled) {
            watched.occurrences.push(scheduled);
        }
    }
    watched.occurrences.sort_unstable();
}

/// Every row: the series opens where the rule says it does — at the write for a rate
/// expression stating no `StartDate`, at the `StartDate` when it states one, and at the
/// expression's own next instant for a cron expression — and its consecutive
/// occurrences are one period apart, each delivered to the subscriber of the topic the
/// schedule targets.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recurring_series_opens_where_it_starts_and_keeps_its_cadence() {
    let router = emulator();
    let mut watching: Vec<Watched> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let (topic_arn, url) = topic_with_subscriber(
            &router,
            case,
            &format!("series-topic-{index}"),
            &format!("series-queue-{index}"),
        )
        .await;
        let role = execution_role(&router, case, &format!("series-role-{index}"), &topic_arn).await;
        let marker = format!("series-{index}");
        let input = json!({ "marker": marker, "at": "<aws.scheduler.scheduled-time>" }).to_string();
        let mut body = json!({
            "ScheduleExpression": row.expression,
            "ScheduleExpressionTimezone": "UTC",
            "FlexibleTimeWindow": { "Mode": "OFF" },
            "State": "ENABLED",
            "Target": { "Arn": topic_arn, "RoleArn": role, "Input": input },
        });
        let start_date = row
            .start_date
            .then(|| whole_seconds_ahead(START_DATE_AHEAD));
        if let Some(start_date) = start_date {
            body["StartDate"] = json!(epoch_seconds(start_date));
        }
        // Read just BEFORE the write: the series of a schedule stating no StartDate
        // opens at the write itself, so this is the latest instant its first
        // occurrence can fall on.
        let written = SystemTime::now();
        let (status, created) = scheduler(
            &router,
            "POST",
            &format!("/schedules/series-{index}"),
            Some(body),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateSchedule answered {status} {created}"
        );
        if let Some(start_date) = start_date {
            assert!(
                start_date > SystemTime::now(),
                "case {case:?}: the StartDate {:?} was already behind the clock once the \
                 schedule was written, so this row's setup took longer than its own lead of \
                 {START_DATE_AHEAD:?}",
                invocation::scheduled_time_text(start_date)
            );
        }
        watching.push(Watched {
            url,
            marker,
            topic_arn,
            written,
            start_date,
            occurrences: Vec::new(),
        });
    }

    // Every row's own occurrences fall inside one period of its write (for the rate
    // rows), one period past the StartDate, or the next minute boundary and the one
    // after it; the rest is room for the clock's tick and the two hand-overs. Each pass
    // looks again for the CONDITION — the occurrences each row is judged on — and the
    // deadline only bounds how long the suite looks.
    let longest = ROWS
        .iter()
        .map(|row| row.period * u32::try_from(row.wanted).unwrap_or(1))
        .max()
        .unwrap_or(MINUTE);
    let deadline = SystemTime::now() + START_DATE_AHEAD + longest + DELIVERY_SLACK;
    loop {
        let mut collected = true;
        for (index, watched) in watching.iter_mut().enumerate() {
            collect(&router, ROWS[index].case, watched).await;
            collected &= watched.occurrences.len() >= ROWS[index].wanted;
        }
        if collected || SystemTime::now() >= deadline {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    for (index, watched) in watching.iter().enumerate() {
        let row = &ROWS[index];
        let case = row.case;
        let occurrences: Vec<String> = watched
            .occurrences
            .iter()
            .map(|at| invocation::scheduled_time_text(*at))
            .collect();
        assert!(
            watched.occurrences.len() >= row.wanted,
            "case {case:?}: expected at least {} occurrence(s) of {:?} delivered to the \
             subscriber, got {occurrences:?}",
            row.wanted,
            row.expression
        );
        let first = watched.occurrences[0];
        // No occurrence of any form is scheduled before the series opens.
        let opened = to_the_second(watched.start_date.unwrap_or(watched.written));
        assert!(
            first >= opened,
            "case {case:?}: the first occurrence is scheduled for {:?}, before the instant \
             {:?} the series opens at",
            invocation::scheduled_time_text(first),
            invocation::scheduled_time_text(opened)
        );
        match row.opens {
            // The interval a rate expression states is the gap between occurrences, so
            // the series opens AT the write: its first occurrence is not a whole period
            // later.
            Opens::Write => assert!(
                first < watched.written + row.period,
                "case {case:?}: the first occurrence of {:?} is scheduled for {:?}, a whole \
                 period after the write at {:?}: the series opened a period late",
                row.expression,
                invocation::scheduled_time_text(first),
                invocation::scheduled_time_text(watched.written)
            ),
            // A StartDate is a floor the series opens AT, so a rate expression's first
            // occurrence is that instant itself.
            Opens::StartDate => assert_eq!(
                Some(first),
                watched.start_date,
                "case {case:?}: expected the first occurrence of {:?} at the StartDate {:?}, \
                 got {occurrences:?}",
                row.expression,
                watched.start_date.map(invocation::scheduled_time_text)
            ),
            // A cron expression names its own instants: every minute of every hour, so
            // the first occurrence is the next minute boundary and nothing between.
            Opens::ExpressionsNextInstant => {
                let since_epoch = first
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                assert!(
                    since_epoch % 60 == 0 && first <= watched.written + row.period,
                    "case {case:?}: expected the first occurrence of {:?} on the next minute \
                     boundary after the write at {:?}, got {occurrences:?}",
                    row.expression,
                    invocation::scheduled_time_text(watched.written)
                );
            }
        }
        for pair in watched.occurrences.windows(2) {
            let gap = pair[1].duration_since(pair[0]).unwrap_or_default();
            assert_eq!(
                gap, row.period,
                "case {case:?}: expected consecutive occurrences of {:?} {:?} apart, got \
                 {occurrences:?}",
                row.expression, row.period
            );
        }
    }
}
