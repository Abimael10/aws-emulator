// SPDX-License-Identifier: CC0-1.0

//! An EventBridge Scheduler schedule whose target is a Step Functions state machine starts
//! that machine's execution NO EARLIER than the instant its `ScheduleExpression` names —
//! judged where a client judges it, in the `startDate` Step Functions reports for the
//! execution — with EventBridge Scheduler, Step Functions and IAM mounted on ONE engine the
//! way `core/router/src/lib.rs` mounts them.
//!
//! RULE: an occurrence is invoked AT the instant its `ScheduleExpression` names, read in
//! `ScheduleExpressionTimezone` ("the timezone in which the scheduling expression is
//! evaluated", UTC when the schedule names none), and never before it. `FlexibleTimeWindow`
//! only moves that instant FORWARD: `Mode` "determines whether the schedule is invoked within
//! a flexible time window" and `MaximumWindowInMinutes` is "the maximum time window during
//! which a schedule can be invoked", so with `Mode=OFF` the invocation is made at the
//! occurrence itself and with `Mode=FLEXIBLE` at an instant inside
//! `[occurrence, occurrence + MaximumWindowInMinutes]`. For a state-machine target the
//! invocation is `states:StartExecution`, so the execution Step Functions holds has a
//! `startDate` at or after that occurrence's own instant, and the scheduled time the
//! occurrence hands the target is that same instant.
//!
//! Three crates have to agree for that to hold (EventBridge Scheduler computes the occurrence
//! and makes the invocation, IAM holds the execution role the invocation is made under, Step
//! Functions serves `StartExecution` and records the execution's `startDate`), so the suite
//! lives here rather than in any of them.
//!
//! The rows differ along what the rule generalises over: the ZONE the expression is read in
//! (an explicit `UTC`, no member at all, and a zone whose offset is not a whole number of
//! hours), the FORM of the expression (a one-time `at(...)` instant and a minute-grained
//! `cron(...)`), and the `FlexibleTimeWindow` `Mode` the schedule asks for. Every instant
//! here is the schedule's own: nothing is compressed, so a case states an instant seconds —
//! or, for the cron form, a minute — ahead and polls for the execution until it is there.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The percent-encoder for an `application/x-www-form-urlencoded` component and the reader of
// one element of an awsQuery answer, which `all/cloudfront_key_value_store_handshake.rs` and
// `all/query_wire.rs` already own: `tests/all/main.rs` is one crate root, so IAM's wire is
// driven through those rather than through a second copy of each.
use crate::cloudfront_key_value_store_handshake::segment as form_encode;
use crate::query_wire::element;

/// The region every request below is signed for.
const REGION: &str = "us-west-2";

/// The principal EventBridge Scheduler acts as, which a schedule's execution role has to
/// trust for the invocation to be made at all.
const SCHEDULER_PRINCIPAL: &str = "scheduler.amazonaws.com";

/// The call EventBridge Scheduler makes for a state-machine target, and the action its
/// execution role therefore has to allow.
const START_EXECUTION_ACTION: &str = "states:StartExecution";

/// The attribute a schedule's `Target.Input` carries the occurrence's own instant in.
const SCHEDULED_TIME_ATTRIBUTE: &str = "<aws.scheduler.scheduled-time>";

/// The schedule group a schedule written with no `GroupName` of its own belongs to.
const DEFAULT_GROUP: &str = "default";

/// How far ahead of the write a case pins a one-time instant: long enough that the schedule
/// is armed before the occurrence falls due, short enough that a case does not wait on it.
const LEAD: Duration = Duration::from_secs(6);

/// How long a case keeps polling for an execution PAST the last instant the rule admits one
/// at. Generous: the clock's tick and the invocation itself take a span of the host's, so a
/// loaded machine must not decide the verdict.
const DISPATCH_SLACK: Duration = Duration::from_secs(45);

/// How long one turn of a poll loop waits before looking again. Short compared with every
/// instant a case states, so an execution started BEFORE the instant it is due at is seen
/// before that instant too.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The shortest flexible window `MaximumWindowInMinutes` can state (its modelled range opens
/// at 1), which is the window the flexible case asks for.
const SHORTEST_WINDOW_MINUTES: i64 = 1;

/// Seconds in the minute `MaximumWindowInMinutes` states its window in.
const SECONDS_PER_MINUTE: u64 = 60;

/// The zone the zoned row writes its local literal in, and that zone's offset from UTC.
///
/// India Standard Time is UTC+05:30 all year round — the zone observes no daylight saving —
/// so the local literal of an instant there is that instant plus this offset, and a schedule
/// read in the wrong zone lands nowhere near the instant the literal denotes.
const ZONED_TIMEZONE: &str = "Asia/Kolkata";
/// See [`ZONED_TIMEZONE`].
const ZONED_OFFSET: Duration = Duration::from_secs(5 * 3600 + 30 * 60);

/// The three services this question spans, over ONE shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_scheduler_awschronosservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_sfn_awsstepfunctions::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request. The signature is never checked: this router runs with request
/// authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One Step Functions operation (`awsJson1_0`, `AWSStepFunctions.<Operation>`), answered as
/// the status and the parsed body.
async fn sfn(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("states.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AWSStepFunctions.{operation}"))
        .header("authorization", authorization("states"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// The same call, required to answer 200.
async fn sfn_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answered) = sfn(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} answered {status} {answered}"
    );
    answered
}

/// One EventBridge Scheduler operation over its `restJson1` wire.
async fn scheduler(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(Bytes::new, |json| Bytes::from(json.to_string()));
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
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One awsQuery call to IAM, required to answer 200, answered with the XML body.
async fn iam(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, String)],
) -> String {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: {action} answered {status} from IAM: {text}"
    );
    text
}

/// A role called `name` trusting `principal` and allowing `action` on `resource`, answering
/// the ARN IAM holds it under.
async fn role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    principal: &str,
    action: &str,
    resource: &str,
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    });
    let created = iam(
        router,
        case,
        "CreateRole",
        &[
            ("RoleName", name.to_string()),
            ("AssumeRolePolicyDocument", trust.to_string()),
        ],
    )
    .await;
    let arn = element(&created, "Arn");
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": action, "Resource": resource }],
    });
    iam(
        router,
        case,
        "PutRolePolicy",
        &[
            ("RoleName", name.to_string()),
            ("PolicyName", "inline".to_string()),
            ("PolicyDocument", policy.to_string()),
        ],
    )
    .await;
    arn
}

/// A trivial STANDARD state machine — one `Pass` state, so an execution of it succeeds on
/// its own — with the role Step Functions runs it as; answers its ARN.
async fn state_machine(router: &MultiServiceRouter, case: &str, slug: &str) -> String {
    let machine_role = role(
        router,
        case,
        &format!("{slug}-machine-role"),
        "states.amazonaws.com",
        "logs:PutLogEvents",
        "*",
    )
    .await;
    let definition = json!({
        "StartAt": "Pass",
        "States": { "Pass": { "Type": "Pass", "End": true } },
    });
    let created = sfn_ok(
        router,
        case,
        "CreateStateMachine",
        &json!({
            "name": format!("{slug}-machine"),
            "definition": definition.to_string(),
            "roleArn": machine_role,
            "type": "STANDARD",
        }),
    )
    .await;
    created["stateMachineArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: CreateStateMachine answers a stateMachineArn, got {created}")
        })
        .to_string()
}

/// `CreateSchedule` for one case, required to answer 200.
async fn given_schedule(router: &MultiServiceRouter, case: &str, name: &str, body: &Value) {
    let (status, created) =
        scheduler(router, "POST", &format!("/schedules/{name}"), Some(body)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateSchedule answered {status} {created}"
    );
    // The schedule a client then reads is the one the clock is arming, so the series the
    // assertions below are about is the one this read reports.
    let (status, read) = scheduler(
        router,
        "GET",
        &format!("/schedules/{name}?groupName={DEFAULT_GROUP}"),
        None,
    )
    .await;
    assert_eq!(
        (status, read["State"].as_str()),
        (200, Some("ENABLED")),
        "case {case:?}: expected the written schedule to read back ENABLED, got {status} {read}"
    );
}

/// The first execution of the machine at `machine_arn`, DESCRIBED, once Step Functions holds
/// one — looking again until it does or `patience` has passed with it not doing so.
async fn awaited_execution(
    router: &MultiServiceRouter,
    case: &str,
    machine_arn: &str,
    patience: Duration,
) -> Value {
    let began = Instant::now();
    loop {
        let listed = sfn_ok(
            router,
            case,
            "ListExecutions",
            &json!({ "stateMachineArn": machine_arn }),
        )
        .await;
        let held = listed["executions"].as_array().cloned().unwrap_or_default();
        if let Some(execution) = held.first() {
            let arn = execution["executionArn"].as_str().unwrap_or_else(|| {
                panic!("case {case:?}: an execution carries its own ARN, got {execution}")
            });
            return sfn_ok(
                router,
                case,
                "DescribeExecution",
                &json!({ "executionArn": arn }),
            )
            .await;
        }
        assert!(
            began.elapsed() < patience,
            "case {case:?}: the schedule started no execution of {machine_arn} inside \
             {patience:?}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The timestamp member `name` of `body`, in seconds since the epoch: an `awsJson1_0`
/// timestamp is epoch seconds with a fractional part.
fn epoch_seconds(case: &str, body: &Value, name: &str) -> f64 {
    body.get(name)
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("case {case:?}: expected the timestamp {name:?} in {body}"))
}

/// One clock reading in seconds since the epoch, which is how an instant this suite computed
/// is compared with one Step Functions reported.
fn instant_seconds(at: SystemTime) -> f64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// `at` as the body of an `at(...)` expression: "at(yyyy-mm-ddThh:mm:ss)", a local date and
/// time with no offset at all, read in the schedule's own zone.
fn expression_body(at: SystemTime) -> String {
    ::aws_smithy_types::DateTime::from(at)
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .expect("an instant renders as ISO-8601")
        .trim_end_matches('Z')
        .to_string()
}

/// The instant the occurrence hands its target as the scheduled time: the occurrence's own
/// instant, as `<aws.scheduler.scheduled-time>` carries it.
fn scheduled_time_text(at: SystemTime) -> String {
    ::aws_smithy_types::DateTime::from(at)
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .expect("an instant renders as ISO-8601")
}

/// A whole number of seconds ahead of now, which is the resolution an `at(...)` expression
/// states an instant at.
fn whole_seconds_ahead(ahead: Duration) -> SystemTime {
    let seconds = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    SystemTime::UNIX_EPOCH + Duration::from_secs(seconds) + ahead
}

/// The first minute boundary at least `ahead` from now: the instant a minute-grained
/// `cron(...)` expression can be pinned to.
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

/// A `cron(...)` expression naming the minute of the hour `at` falls on, in the six
/// documented fields "Minutes Hours Day-of-month Month Day-of-week Year" — so read in UTC the
/// next instant it names is `at` itself.
fn cron_pinned_to(at: SystemTime) -> String {
    let rendered = scheduled_time_text(at);
    let field = |from: usize, to: usize| -> u32 {
        rendered
            .get(from..to)
            .and_then(|text| text.parse().ok())
            .unwrap_or_else(|| panic!("the instant {rendered} carries an hour and a minute"))
    };
    // "yyyy-mm-ddThh:mm:ssZ": the hour opens at index 11 and the minute at 14.
    format!("cron({} {} * * ? *)", field(14, 16), field(11, 13))
}

/// The `Target.Input` a case hands its state machine: a marker that ties the execution to
/// this schedule, and the occurrence's own instant.
fn target_input(marker: &str) -> String {
    json!({ "marker": marker, "at": SCHEDULED_TIME_ATTRIBUTE }).to_string()
}

/// Assert that the execution Step Functions holds for the schedule of `machine_arn` started
/// at or after `nominal` and no later than the end of the window that opens there, and that
/// the occurrence handed the target `nominal` as its scheduled time.
async fn assert_started_inside_the_window(
    router: &MultiServiceRouter,
    case: &str,
    machine_arn: &str,
    marker: &str,
    nominal: SystemTime,
    window: Duration,
) {
    let patience = nominal
        .duration_since(SystemTime::now())
        .unwrap_or_default()
        + window
        + DISPATCH_SLACK;
    let execution = awaited_execution(router, case, machine_arn, patience).await;
    let started = epoch_seconds(case, &execution, "startDate");
    let opens = instant_seconds(nominal);
    assert!(
        started >= opens,
        "case {case:?}: the execution started at {started} s, {} s BEFORE the instant {opens} s \
         the expression names; a schedule is never invoked before its occurrence ({execution})",
        opens - started
    );
    let closes = instant_seconds(nominal + window + DISPATCH_SLACK);
    assert!(
        started <= closes,
        "case {case:?}: the execution started at {started} s, past the end {closes} s of the \
         window that opens at {opens} s ({execution})"
    );
    let input: Value = serde_json::from_str(execution["input"].as_str().unwrap_or_default())
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the execution's input is not the JSON the schedule handed it ({error}): {execution}")
        });
    assert_eq!(
        input["marker"].as_str(),
        Some(marker),
        "case {case:?}: expected the execution started by this schedule, got {input}"
    );
    assert_eq!(
        input["at"].as_str(),
        Some(scheduled_time_text(nominal).as_str()),
        "case {case:?}: expected the occurrence's own instant {:?} as the scheduled time, got \
         {input}",
        scheduled_time_text(nominal)
    );
}

/// One row of the `Mode=OFF` table: the zone the expression is read in, and how the
/// expression's body states the instant it names in that zone.
struct OffRow {
    case: &'static str,
    slug: &'static str,
    /// `ScheduleExpressionTimezone`, or `None` for a schedule stating none at all.
    timezone: Option<&'static str>,
    /// The local literal of `nominal` in that zone.
    body: fn(SystemTime) -> String,
}

/// The rows differ along the ZONE the one expression body is read in: an explicit `UTC`, no
/// member at all (which is read in UTC too), and a zone whose offset from UTC is not a whole
/// number of hours.
const OFF_ROWS: &[OffRow] = &[
    OffRow {
        case: "a one-time instant in an explicit UTC",
        slug: "nominal-utc",
        timezone: Some("UTC"),
        body: expression_body,
    },
    OffRow {
        case: "a one-time instant stating no zone at all",
        slug: "nominal-default-zone",
        timezone: None,
        body: expression_body,
    },
    OffRow {
        case: "a one-time instant written as a local time in a zone offset by half an hour",
        slug: "nominal-zoned",
        timezone: Some(ZONED_TIMEZONE),
        body: |nominal| expression_body(nominal + ZONED_OFFSET),
    },
];

/// Every row: a `Mode=OFF` schedule starts its state machine's execution at the instant its
/// expression names, read in its own zone, and never before it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mode_off_schedule_starts_no_execution_before_its_nominal_instant() {
    for row in OFF_ROWS {
        let case = row.case;
        let router = emulator();
        let machine_arn = state_machine(&router, case, row.slug).await;
        let execution_role = role(
            &router,
            case,
            &format!("{}-schedule-role", row.slug),
            SCHEDULER_PRINCIPAL,
            START_EXECUTION_ACTION,
            &machine_arn,
        )
        .await;
        let nominal = whole_seconds_ahead(LEAD);
        let expression = format!("at({})", (row.body)(nominal));
        let mut body = json!({
            "ScheduleExpression": expression,
            "FlexibleTimeWindow": { "Mode": "OFF" },
            "State": "ENABLED",
            "Target": {
                "Arn": machine_arn,
                "RoleArn": execution_role,
                "Input": target_input(row.slug),
            },
        });
        if let Some(timezone) = row.timezone {
            body["ScheduleExpressionTimezone"] = json!(timezone);
        }
        given_schedule(&router, case, row.slug, &body).await;

        assert_started_inside_the_window(
            &router,
            case,
            &machine_arn,
            row.slug,
            nominal,
            Duration::ZERO,
        )
        .await;
    }
}

/// A `Mode=FLEXIBLE` schedule's window opens AT the occurrence and extends forward only: the
/// execution starts inside `[occurrence, occurrence + MaximumWindowInMinutes]` and never
/// before the occurrence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_flexible_window_is_clamped_below_by_its_nominal_instant() {
    let case = "a one-time instant inside the shortest flexible window";
    let slug = "flexible-window";
    let router = emulator();
    let machine_arn = state_machine(&router, case, slug).await;
    let execution_role = role(
        &router,
        case,
        &format!("{slug}-schedule-role"),
        SCHEDULER_PRINCIPAL,
        START_EXECUTION_ACTION,
        &machine_arn,
    )
    .await;
    let nominal = whole_seconds_ahead(LEAD);
    given_schedule(
        &router,
        case,
        slug,
        &json!({
            "ScheduleExpression": format!("at({})", expression_body(nominal)),
            "ScheduleExpressionTimezone": "UTC",
            "FlexibleTimeWindow": {
                "Mode": "FLEXIBLE",
                "MaximumWindowInMinutes": SHORTEST_WINDOW_MINUTES,
            },
            "State": "ENABLED",
            "Target": {
                "Arn": machine_arn,
                "RoleArn": execution_role,
                "Input": target_input(slug),
            },
        }),
    )
    .await;

    let window = Duration::from_secs(
        u64::try_from(SHORTEST_WINDOW_MINUTES).unwrap_or_default() * SECONDS_PER_MINUTE,
    );
    assert_started_inside_the_window(&router, case, &machine_arn, slug, nominal, window).await;
}

/// The same rule over the other expression form that names its own instants: a minute-grained
/// `cron(...)` schedule starts its execution at the minute it names and never in the minute
/// before it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cron_schedule_starts_no_execution_before_the_minute_it_names() {
    let case = "a cron expression pinned to one minute of the hour";
    let slug = "cron-nominal";
    let router = emulator();
    let machine_arn = state_machine(&router, case, slug).await;
    let execution_role = role(
        &router,
        case,
        &format!("{slug}-schedule-role"),
        SCHEDULER_PRINCIPAL,
        START_EXECUTION_ACTION,
        &machine_arn,
    )
    .await;
    // Far enough ahead that the schedule is armed before the minute it names arrives.
    let nominal = next_minute_boundary(LEAD);
    given_schedule(
        &router,
        case,
        slug,
        &json!({
            "ScheduleExpression": cron_pinned_to(nominal),
            "ScheduleExpressionTimezone": "UTC",
            "FlexibleTimeWindow": { "Mode": "OFF" },
            "State": "ENABLED",
            "Target": {
                "Arn": machine_arn,
                "RoleArn": execution_role,
                "Input": target_input(slug),
            },
        }),
    )
    .await;

    assert_started_inside_the_window(
        &router,
        case,
        &machine_arn,
        slug,
        nominal,
        Duration::ZERO,
    )
    .await;
}
