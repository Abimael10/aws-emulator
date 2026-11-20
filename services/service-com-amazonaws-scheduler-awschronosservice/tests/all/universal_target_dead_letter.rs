// SPDX-License-Identifier: CC0-1.0

//! A UNIVERSAL target whose invocation is REFUSED is dead-lettered: end to end over the
//! wire through the real `MultiServiceRouter`, with Amazon RDS, Amazon SQS and IAM
//! mounted beside EventBridge Scheduler on ONE engine — so the failed invocation is
//! asserted where a client observes it, in the message the schedule's dead-letter queue
//! holds and in the state of the resource the call acts on.
//!
//! RULE: a universal target names the CALL itself
//! (`arn:<partition>:scheduler:::aws-sdk:<service>:<operation>`), and at each occurrence
//! EventBridge Scheduler makes that call under `Target.RoleArn` with `Target.Input` as
//! the operation's own request. The call is subject to the IAM action that operation
//! publishes, over the resource its REQUEST names, so a role whose permissions do not
//! allow it withholds the call — and an invocation that did not reach its target is
//! delivered to the queue `Target.DeadLetterConfig.Arn` names, carrying the request the
//! attempt made and the detail of the refusal that ended it. A universal target is never
//! silently unattempted for a service whose calls this emulator makes: the schedule's
//! only signal for a denied target is that dead-letter message.
//!
//! RULE: the reported `ERROR_CODE` is the code the service that would have answered the
//! call names such a refusal by — the invoked operation's own modelled access error where
//! it declares one, and otherwise the code that service's own front door words AWS's
//! authorization refusal in — and the `ERROR_MESSAGE` is AWS's authorization sentence
//! naming the session the execution role was assumed under, the action and the resource.
//!
//! The rows span the class rather than one recorded case: they differ along the OPERATION
//! the universal target names (the stop, the start and the reboot of a DB instance), along
//! WHY the role withholds the call (it holds nothing at all, it holds the action on
//! another resource, it holds another action on this one), and along the form of
//! `Target.Input` (one member, and a member beside another the operation takes).
//!
//! Nothing here waits for a fixed span: each wait watches for the CONDITION it expects
//! until a generous deadline.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::dead_letter_delivery::{body_document, messages_once, received};
use crate::schedule_invocation::{
    at_expression_body, given_execution_role_granting, given_queue, scheduler, seconds_from_now,
    POLL_INTERVAL, REGION,
};
use crate::wire_harness::{account_id, encode_label, xml_text};

/// How long a wait watches for the condition it expects. Every schedule below states its
/// one occurrence [`OCCURRENCE_IN`] seconds ahead, so this is room for the clock's tick,
/// the withheld call and the dead-letter delivery rather than a latency expectation.
const PATIENCE: Duration = Duration::from_secs(90);

/// How long a check of an ABSENCE keeps looking, so a delivery this emulator would have
/// made has had its chance. Nothing is asserted about how long anything took.
const QUIET_WINDOW: Duration = Duration::from_secs(10);

/// How far ahead of the clock every schedule below states its one occurrence.
const OCCURRENCE_IN: u64 = 6;

/// The engine and the class every DB instance below is created with, and the storage
/// `CreateDBInstance` is asked for.
const ENGINE: &str = "mysql";
/// See [`ENGINE`].
const INSTANCE_CLASS: &str = "db.t3.micro";
/// See [`ENGINE`].
const ALLOCATED_STORAGE: &str = "20";

/// The status a DB instance reports once it is up, which is the one a stop and a reboot
/// are accepted from.
const AVAILABLE: &str = "available";

/// The status a DB instance reports while it is shutting down, which an accepted stop
/// reports at once.
const STOPPING: &str = "stopping";

/// The status it reports once the stop has run.
const STOPPED: &str = "stopped";

/// The IAM action the delivery of a dead-letter message is authorized by — the
/// `sqs:SendMessage` AWS documents for a schedule's dead-letter queue.
const SEND_MESSAGE: &str = "sqs:SendMessage";

/// The Query code AWS's authorization refusal of a call to a service whose protocol is
/// `awsQuery` — Amazon RDS among them — names its class by, which is the code a report of
/// a withheld call of one carries.
const QUERY_ACCESS_DENIED: &str = "AccessDenied";

/// The emulator: EventBridge Scheduler, the service a universal target's call is made to,
/// the queue service a dead-letter message is delivered through, and IAM, on ONE engine —
/// the shape `core/router/src/lib.rs` mounts them in.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_scheduler_awschronosservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_rds_amazonrdsv19::EmulatedService::new(engine),
    );
    router
}

/// One awsQuery call against Amazon RDS, answered as the XML text it is — the same form
/// POST the universal target's own invocation is made as.
async fn rds(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2014-10-31");
    for (name, value) in params {
        body.push('&');
        body.push_str(&encode_label(name));
        body.push('=');
        body.push_str(&encode_label(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("rds.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTEST/20260101/{REGION}/rds/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = String::from_utf8_lossy(response.body()).to_string();
    (status, answered)
}

/// The text the first element called `name` of an answer carries, read through the ONE
/// reading of a tag's text this test crate keeps.
fn element(answered: &str, name: &str) -> Option<String> {
    xml_text(answered, name)
}

/// A DB instance of `identifier`, and the ARN an access request over it is taken on.
///
/// The instance is created and then watched until it reports [`AVAILABLE`]: an instance
/// RDS is still bringing up has no stop or reboot of its own, so the occurrence below
/// would be refused for the wrong reason.
async fn given_db_instance(router: &MultiServiceRouter, case: &str, identifier: &str) -> String {
    let (status, created) = rds(
        router,
        "CreateDBInstance",
        &[
            ("DBInstanceIdentifier", identifier),
            ("Engine", ENGINE),
            ("DBInstanceClass", INSTANCE_CLASS),
            ("AllocatedStorage", ALLOCATED_STORAGE),
            ("MasterUsername", "emuadmin"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateDBInstance answered {status} {created}"
    );
    let deadline = Instant::now() + PATIENCE;
    loop {
        let reported = db_instance_status(router, case, identifier).await;
        if reported.as_deref() == Some(AVAILABLE) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the DB instance {identifier:?} to come up, got {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    format!("arn:aws:rds:{REGION}:{}:db:{identifier}", account_id())
}

/// The `DBInstanceStatus` Amazon RDS reports for `identifier`, read through its own
/// `DescribeDBInstances`.
async fn db_instance_status(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> Option<String> {
    let (status, described) = rds(
        router,
        "DescribeDBInstances",
        &[("DBInstanceIdentifier", identifier)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeDBInstances answered {status} {described}"
    );
    element(&described, "DBInstanceStatus")
}

/// A schedule called `name` whose one occurrence is [`OCCURRENCE_IN`] seconds ahead,
/// pointed at the universal target `target` with `input` as its request and `dead_letter`
/// as the queue a failed invocation is reported to.
async fn given_schedule(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    target: &str,
    role: &str,
    input: &str,
    dead_letter: &str,
) {
    let (status, created) = scheduler(
        router,
        "POST",
        &format!("/schedules/{name}"),
        Some(json!({
            "ScheduleExpression": format!("at({})", at_expression_body(seconds_from_now(OCCURRENCE_IN))),
            "ScheduleExpressionTimezone": "UTC",
            "FlexibleTimeWindow": { "Mode": "OFF" },
            "State": "ENABLED",
            "Target": {
                "Arn": target,
                "RoleArn": role,
                "Input": input,
                // A refusal of the request itself is the invocation's last attempt
                // whatever the policy admits; the policy is stated all the same, so the
                // report is of an invocation that retried nothing.
                "RetryPolicy": { "MaximumRetryAttempts": 0 },
                "DeadLetterConfig": { "Arn": dead_letter },
            },
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateSchedule answered {status} {created}"
    );
}

/// One row of the withheld table: the operation the universal target names, the request
/// it carries, and what the execution role is granted instead of the call.
struct Row {
    case: &'static str,
    /// The operation field of the target's ARN, in the lowerCamelCase the AWS SDK family
    /// names an operation in.
    operation: &'static str,
    /// The IAM action that operation publishes, which the refusal names.
    action: &'static str,
    /// Whether `Target.Input` carries a second member beside the instance it addresses.
    second_member: bool,
    /// What the role holds besides the dead-letter write, as `(action, resource)` over
    /// the row's own instance — `None` for a role that holds nothing else at all.
    instead: Option<(&'static str, Scope)>,
}

/// Which resource a grant the row's role holds is scoped to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// The DB instance the row's own request names.
    ThisInstance,
    /// Another DB instance of the same account and Region.
    AnotherInstance,
}

const ROWS: &[Row] = &[
    Row {
        case: "a stop whose role holds nothing at all",
        operation: "stopDBInstance",
        action: "rds:StopDBInstance",
        second_member: false,
        instead: None,
    },
    Row {
        case: "a stop whose role holds the action on another instance",
        operation: "stopDBInstance",
        action: "rds:StopDBInstance",
        // `DBSnapshotIdentifier` is "the user-supplied instance identifier of the DB
        // Snapshot created immediately before the DB instance is stopped", a second
        // member of the same request.
        second_member: true,
        instead: Some(("rds:StopDBInstance", Scope::AnotherInstance)),
    },
    Row {
        case: "a reboot whose role holds another action on this instance",
        operation: "rebootDBInstance",
        action: "rds:RebootDBInstance",
        second_member: false,
        instead: Some(("rds:StopDBInstance", Scope::ThisInstance)),
    },
    Row {
        case: "a start whose role holds nothing at all",
        operation: "startDBInstance",
        action: "rds:StartDBInstance",
        second_member: false,
        instead: None,
    },
];

/// Every row: the occurrence's call is withheld by the execution role's permissions, and
/// the schedule's dead-letter queue holds ONE message carrying the request the attempt
/// made and the refusal that ended it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_withheld_universal_target_invocation_is_dead_lettered() {
    let emulator = emulator();
    // Another instance of the same account and Region, for the row whose grant is scoped
    // to a resource its own request does not name.
    let other_arn = given_db_instance(&emulator, "the withheld table", "other-instance").await;
    let mut prepared = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let identifier = format!("withheld-{index}");
        let instance_arn = given_db_instance(&emulator, case, &identifier).await;
        let (dead_letter_url, dead_letter_arn) =
            given_queue(&emulator, case, &format!("universal-dlq-{index}")).await;
        // The role may always write to the row's dead-letter queue, so the report is
        // delivered whatever withheld the invocation itself.
        let mut grants = vec![(SEND_MESSAGE, dead_letter_arn.clone())];
        if let Some((action, scope)) = row.instead {
            let resource = match scope {
                Scope::ThisInstance => instance_arn.clone(),
                Scope::AnotherInstance => other_arn.clone(),
            };
            grants.push((action, resource));
        }
        let held: Vec<(&str, &str)> = grants
            .iter()
            .map(|(action, resource)| (*action, resource.as_str()))
            .collect();
        let role =
            given_execution_role_granting(&emulator, case, &format!("withheld-role-{index}"), &held)
                .await;
        let mut request = json!({ "DBInstanceIdentifier": identifier });
        if row.second_member {
            request["DBSnapshotIdentifier"] = json!(format!("{identifier}-snapshot"));
        }
        let input = request.to_string();
        let target = format!("arn:aws:scheduler:::aws-sdk:rds:{}", row.operation);
        given_schedule(
            &emulator,
            case,
            &format!("withheld-{index}"),
            &target,
            &role,
            &input,
            &dead_letter_arn,
        )
        .await;
        prepared.push((row, dead_letter_url, instance_arn, input, target, role));
    }

    for (row, dead_letter_url, instance_arn, input, target, role) in &prepared {
        let case = row.case;
        let reported = messages_once(&emulator, dead_letter_url, 1, PATIENCE).await;
        let message = reported.first().unwrap_or_else(|| {
            panic!("case {case:?}: nothing reached the dead-letter queue of {target:?}")
        });
        // The body is the request the attempt made of the invoked operation, which for a
        // universal target IS `Target.Input`.
        let expected: serde_json::Value =
            serde_json::from_str(input).expect("the row's Input is JSON");
        assert_eq!(
            body_document(message),
            expected,
            "case {case:?}: expected the dead-lettered request {expected}, got {message}"
        );
        let attribute = |name: &str| {
            message["MessageAttributes"][name]["StringValue"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        };
        assert_eq!(
            attribute("ERROR_CODE"),
            QUERY_ACCESS_DENIED,
            "case {case:?}: expected the invoked service's own refusal code, got {message}"
        );
        assert_eq!(
            attribute("TARGET_ARN"),
            *target,
            "case {case:?}: expected the universal target to be reported, got {message}"
        );
        // The policy admitted no retry, so the report is of the invocation's first and
        // only attempt.
        assert_eq!(
            attribute("RETRY_ATTEMPTS"),
            "0",
            "case {case:?}: expected no retry to be reported, got {message}"
        );
        // AWS's authorization sentence: the session the execution role was assumed under,
        // the action the operation publishes and the resource the request named.
        let detail = attribute("ERROR_MESSAGE");
        let role_name = role.rsplit('/').next().unwrap_or_default();
        let session = format!("arn:aws:sts::{}:assumed-role/{role_name}/", account_id());
        let sentence = format!(
            " is not authorized to perform: {} on resource: {instance_arn} because no \
             identity-based policy allows the {} action",
            row.action, row.action
        );
        assert!(
            detail.starts_with(&format!("User: {session}")),
            "case {case:?}: expected the refusal to name the session the role was assumed \
             under (User: {session}<session>), got {detail:?}"
        );
        assert!(
            detail.ends_with(&sentence),
            "case {case:?}: expected the refusal to end {sentence:?}, got {detail:?}"
        );
        // Nothing the call acts on changed: the invocation was never made.
        let identifier = instance_arn.rsplit(':').next().unwrap_or_default();
        let reported = db_instance_status(&emulator, case, identifier).await;
        assert_eq!(
            reported.as_deref(),
            Some(AVAILABLE),
            "case {case:?}: expected the withheld call to leave the instance as it was, got \
             {reported:?}"
        );
    }

    // Each queue holds that ONE report and no other: a refusal of the request itself is
    // reported once.
    let deadline = Instant::now() + QUIET_WINDOW;
    while Instant::now() < deadline {
        for (row, dead_letter_url, ..) in &prepared {
            let case = row.case;
            let held = received(&emulator, dead_letter_url).await;
            assert_eq!(
                held.len(),
                1,
                "case {case:?}: expected exactly one report on the dead-letter queue, got {held:?}"
            );
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The other half of the rule: a universal target whose call the execution role DOES
/// allow is made, so the operation's own effect is there afterwards — read through Amazon
/// RDS's own `DescribeDBInstances` — and nothing is dead-lettered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_allowed_universal_target_invocation_performs_its_call() {
    let case = "a stop of a DB instance the execution role may stop";
    let emulator = emulator();
    let identifier = "allowed-instance";
    let instance_arn = given_db_instance(&emulator, case, identifier).await;
    let (dead_letter_url, dead_letter_arn) = given_queue(&emulator, case, "allowed-dlq").await;
    let role = given_execution_role_granting(
        &emulator,
        case,
        "allowed-role",
        &[
            ("rds:StopDBInstance", instance_arn.as_str()),
            (SEND_MESSAGE, dead_letter_arn.as_str()),
        ],
    )
    .await;
    given_schedule(
        &emulator,
        case,
        "allowed-stop",
        "arn:aws:scheduler:::aws-sdk:rds:stopDBInstance",
        &role,
        &json!({ "DBInstanceIdentifier": identifier }).to_string(),
        &dead_letter_arn,
    )
    .await;

    // The stop the occurrence made is the one RDS performed: the instance leaves
    // `available` for the shutdown the operation reports and the status the clock then
    // owes it.
    let deadline = Instant::now() + PATIENCE;
    let settled = loop {
        let reported = db_instance_status(&emulator, case, identifier).await;
        if reported.as_deref() == Some(STOPPING) || reported.as_deref() == Some(STOPPED) {
            break reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the universal target's stop to reach the instance, got \
             {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    assert!(
        matches!(settled.as_deref(), Some(STOPPING | STOPPED)),
        "case {case:?}: expected the instance to be stopping or stopped, got {settled:?}"
    );
    // An invocation the target accepted is not reported: nothing reaches the dead-letter
    // queue inside the window.
    let deadline = Instant::now() + QUIET_WINDOW;
    while Instant::now() < deadline {
        let reported = received(&emulator, &dead_letter_url).await;
        assert!(
            reported.is_empty(),
            "case {case:?}: expected nothing on the dead-letter queue, got {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
