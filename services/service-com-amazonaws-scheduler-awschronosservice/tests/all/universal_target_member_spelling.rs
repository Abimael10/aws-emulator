// SPDX-License-Identifier: CC0-1.0

//! How a UNIVERSAL target's `Target.Input` NAMES the members of the operation it
//! invokes, and what a failed occurrence of such a schedule reports: end to end over the
//! wire through the real `MultiServiceRouter`, with Amazon RDS, Amazon SQS and IAM
//! mounted beside EventBridge Scheduler on ONE engine.
//!
//! RULE: `Target.Input` for a universal target IS the invoked operation's request, and a
//! member of it names the operation's own member IGNORING CASE — the spelling that
//! operation's model gives the member and the spelling an AWS SDK gives it name the same
//! one, which matters because an SDK renames a member whose model name carries an acronym
//! by lowercasing that acronym (`DBInstanceIdentifier` -> `dbInstanceIdentifier`), a
//! difference in more than the member's first character. So the request the occurrence
//! makes carries the member under the model's own spelling whichever of those the schedule
//! wrote, and the access request the call is subject to is formed over the resource that
//! member names either way.
//!
//! RULE: an invocation that did not reach its target is reported to
//! `Target.DeadLetterConfig.Arn` under the error code and message THE ANSWER carries, in
//! the place the protocol that answer came in names its class — so a refusal from a
//! service whose protocol answers an XML document is reported by the `Code` and `Message`
//! that document carries, not as a failure nothing could be read from.
//!
//! The rows span the class rather than one recorded case: they differ along the SPELLING
//! the schedule writes the member in, along the OPERATION the universal target names,
//! along whether a second member sits beside the one that addresses the instance, and
//! along WHICH refusal ends the invocation — the execution role withholding the call, and
//! the invoked service's own answer for a request it will not serve.
//!
//! Nothing here waits for a fixed span: every wait watches for the CONDITION it expects
//! until a generous deadline.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::dead_letter_delivery::{body_document, messages_once, received};
use crate::schedule_invocation::{
    at_expression_body, given_execution_role_granting, given_queue, scheduler, seconds_from_now,
    POLL_INTERVAL, REGION,
};
use crate::wire_harness::{account_id, encode_label, xml_text};

/// How long a wait watches for the condition it expects. Every schedule below states its
/// one occurrence [`OCCURRENCE_IN`] seconds ahead, so this is room for the clock's tick,
/// the invocation and the dead-letter delivery rather than a latency expectation.
const PATIENCE: Duration = Duration::from_secs(120);

/// How long a check of an ABSENCE keeps looking, so a delivery this emulator would have
/// made has had its chance. Nothing is asserted about how long anything took.
const QUIET_WINDOW: Duration = Duration::from_secs(10);

/// How far ahead of the clock every schedule below states its one occurrence.
const OCCURRENCE_IN: u64 = 6;

/// The engine, the class and the storage every DB instance below is created with.
const ENGINE: &str = "mysql";
/// See [`ENGINE`].
const INSTANCE_CLASS: &str = "db.t3.micro";
/// See [`ENGINE`].
const ALLOCATED_STORAGE: &str = "20";

/// The status a DB instance reports once it is up, which is the one a stop is accepted
/// from.
const AVAILABLE: &str = "available";

/// The status it reports while it is shutting down, and the one it settles at.
const STOPPING: &str = "stopping";
/// See [`STOPPING`].
const STOPPED: &str = "stopped";

/// The IAM action the delivery of a dead-letter message is authorized by — the
/// `sqs:SendMessage` AWS documents for a schedule's dead-letter queue.
const SEND_MESSAGE: &str = "sqs:SendMessage";

/// The Query code AWS's authorization layer refuses a call to a service whose protocol is
/// `awsQuery` — Amazon RDS among them — with, which is the code a report of a withheld
/// call of one carries.
const QUERY_ACCESS_DENIED: &str = "AccessDenied";

/// The code Amazon RDS answers a request naming a DB instance it does not hold with, as
/// that model binds the fault's own query code
/// (`smithy_query(query="operation",operation="StopDBInstance",model_path="services/
/// service-com-amazonaws-rds-amazonrdsv19/smithy-model/
/// com-amazonaws-rds-amazonrdsv19.json")`: `DBInstanceNotFoundFault` carries
/// `awsQueryError` code `DBInstanceNotFound`).
const DB_INSTANCE_NOT_FOUND: &str = "DBInstanceNotFound";

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

/// One awsQuery call against Amazon RDS, answered as the XML text it is.
async fn rds(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> (u16, String) {
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

/// The `DBInstanceStatus` Amazon RDS reports for `identifier`, read through its own
/// `DescribeDBInstances`; `None` for an instance that service does not hold.
async fn db_instance_status(router: &MultiServiceRouter, identifier: &str) -> Option<String> {
    let (_status, described) = rds(
        router,
        "DescribeDBInstances",
        &[("DBInstanceIdentifier", identifier)],
    )
    .await;
    xml_text(&described, "DBInstanceStatus")
}

/// A DB instance of `identifier`, watched until it reports [`AVAILABLE`]: an instance RDS
/// is still bringing up has no stop of its own, so the occurrence below would be refused
/// for the wrong reason.
async fn given_db_instance(router: &MultiServiceRouter, case: &str, identifier: &str) {
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
        let reported = db_instance_status(router, identifier).await;
        if reported.as_deref() == Some(AVAILABLE) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the DB instance {identifier:?} to come up, got {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The ARN an access request over the DB instance of `identifier` is taken on, which is
/// the resource a refusal of a call on it names.
fn db_instance_arn(identifier: &str) -> String {
    format!("arn:aws:rds:{REGION}:{}:db:{identifier}", account_id())
}

/// A schedule called `name` whose one occurrence is [`OCCURRENCE_IN`] seconds ahead,
/// pointed at the universal target of `operation` with `input` as its request and
/// `dead_letter` as the queue a failed invocation is reported to.
async fn given_schedule(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    operation: &str,
    role: &str,
    input: &Value,
    dead_letter: &str,
) -> String {
    let target = format!("arn:aws:scheduler:::aws-sdk:rds:{operation}");
    let (status, created) = scheduler(
        router,
        "POST",
        &format!("/schedules/{name}"),
        Some(json!({
            "ScheduleExpression":
                format!("at({})", at_expression_body(seconds_from_now(OCCURRENCE_IN))),
            "ScheduleExpressionTimezone": "UTC",
            "FlexibleTimeWindow": { "Mode": "OFF" },
            "State": "ENABLED",
            "Target": {
                "Arn": target,
                "RoleArn": role,
                "Input": input.to_string(),
                // A refusal of the request itself is the invocation's last attempt
                // whatever the policy admits; the policy is stated all the same, so the
                // report is of an invocation that retried nothing.
                "RetryPolicy": { "MaximumRetryAttempts": 1, "MaximumEventAgeInSeconds": 3600 },
                "DeadLetterConfig": { "Arn": dead_letter },
            },
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateSchedule answered {status} {created}"
    );
    target
}

/// The `StringValue` a dead-letter message carries under the attribute `name`.
fn attribute(message: &Value, name: &str) -> String {
    message["MessageAttributes"][name]["StringValue"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// One row of the spelling table: how the schedule writes the member that addresses the
/// DB instance, which operation of that service the universal target names, and the IAM
/// action that operation publishes.
struct Row {
    case: &'static str,
    /// The member name `Target.Input` writes, which the rule says names the operation's
    /// own member whatever its case.
    written: &'static str,
    /// The operation field of the target's ARN, in the lowerCamelCase the AWS SDK family
    /// names an operation in.
    operation: &'static str,
    /// The IAM action that operation publishes, which a withheld call's refusal names.
    action: &'static str,
    /// A second member of the same request, written beside the one above — `(name,
    /// value)`, or `None` for a request of one member.
    beside: Option<(&'static str, &'static str)>,
}

/// The spellings and operations the rule is asserted over.
///
/// `DBInstanceIdentifier` is the model's own spelling of the member each of these
/// operations addresses its instance by, `dbInstanceIdentifier` is the spelling an AWS SDK
/// gives that member, and `DbInstanceIdentifier` is that SDK spelling capitalised — three
/// names of ONE member, differing in more than the first character.
const ROWS: &[Row] = &[
    Row {
        case: "a stop naming the member as the model spells it",
        written: "DBInstanceIdentifier",
        operation: "stopDBInstance",
        action: "rds:StopDBInstance",
        beside: None,
    },
    Row {
        case: "a stop naming it as an AWS SDK spells it",
        written: "dbInstanceIdentifier",
        operation: "stopDBInstance",
        action: "rds:StopDBInstance",
        beside: None,
    },
    Row {
        case: "a stop naming it in that SDK spelling capitalised, beside a second member",
        written: "DbInstanceIdentifier",
        operation: "stopDBInstance",
        // `DBSnapshotIdentifier` is "the user-supplied instance identifier of the DB
        // Snapshot created immediately before the DB instance is stopped", a second
        // member of the same request, written here in the SDK's spelling too.
        beside: Some(("dbSnapshotIdentifier", "spelling-snapshot")),
        action: "rds:StopDBInstance",
    },
    Row {
        case: "a reboot naming it as an AWS SDK spells it",
        written: "dbInstanceIdentifier",
        operation: "rebootDBInstance",
        action: "rds:RebootDBInstance",
        // `ForceFailover` is a member of that request whose model name is plain words,
        // written here in the SDK's spelling of it.
        beside: Some(("forceFailover", "false")),
    },
    Row {
        case: "a start naming it in the SDK spelling capitalised",
        written: "DbInstanceIdentifier",
        operation: "startDBInstance",
        action: "rds:StartDBInstance",
        beside: None,
    },
];

/// The request one row writes, naming the DB instance under the row's own spelling of
/// the member.
fn request_of(row: &Row, identifier: &str) -> Value {
    let mut request = ::serde_json::Map::new();
    request.insert(
        row.written.to_string(),
        Value::String(identifier.to_string()),
    );
    if let Some((member, value)) = row.beside {
        request.insert(member.to_string(), Value::String(value.to_string()));
    }
    Value::Object(request)
}

/// Every row: the member the schedule wrote names the operation's own member, so the
/// access request the occurrence's call is subject to is formed over the DB instance that
/// member names — and the role holding none of it withholds the call, which the
/// schedule's dead-letter queue reports as the authorization refusal naming that action
/// and that resource.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_universal_target_input_names_its_member_ignoring_case() {
    let emulator = emulator();
    let mut prepared = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let identifier = format!("spelling-{index}");
        let (dead_letter_url, dead_letter_arn) =
            given_queue(&emulator, case, &format!("spelling-dlq-{index}")).await;
        // The role may always write to the row's dead-letter queue and holds nothing
        // else, so the report is delivered and the invocation itself is withheld.
        let role = given_execution_role_granting(
            &emulator,
            case,
            &format!("spelling-role-{index}"),
            &[(SEND_MESSAGE, dead_letter_arn.as_str())],
        )
        .await;
        let request = request_of(row, &identifier);
        let target = given_schedule(
            &emulator,
            case,
            &format!("spelling-{index}"),
            row.operation,
            &role,
            &request,
            &dead_letter_arn,
        )
        .await;
        prepared.push((row, dead_letter_url, identifier, request, target, role));
    }

    for (row, dead_letter_url, identifier, request, target, role) in &prepared {
        let case = row.case;
        let reported = messages_once(&emulator, dead_letter_url, 1, PATIENCE).await;
        let message = reported.first().unwrap_or_else(|| {
            panic!("case {case:?}: nothing reached the dead-letter queue of {target:?}")
        });
        // The body is the request the attempt made, which for a universal target IS
        // `Target.Input` as the schedule wrote it — the spelling is resolved for the
        // CALL, not rewritten in the schedule.
        assert_eq!(
            body_document(message),
            *request,
            "case {case:?}: expected the dead-lettered request {request}, got {message}"
        );
        assert_eq!(
            attribute(message, "ERROR_CODE"),
            QUERY_ACCESS_DENIED,
            "case {case:?}: expected the invoked service's own refusal code, got {message}"
        );
        assert_eq!(
            attribute(message, "TARGET_ARN"),
            *target,
            "case {case:?}: expected the universal target to be reported, got {message}"
        );
        // AWS's authorization sentence: the session the execution role was assumed under,
        // the action the operation publishes, and the resource the member named — which
        // is the whole point of the row, since a member that named nothing would leave
        // the call unauthorized over no resource at all.
        let detail = attribute(message, "ERROR_MESSAGE");
        let role_name = role.rsplit('/').next().unwrap_or_default();
        let session = format!("User: arn:aws:sts::{}:assumed-role/{role_name}/", account_id());
        let sentence = format!(
            " is not authorized to perform: {} on resource: {} because no identity-based \
             policy allows the {} action",
            row.action,
            db_instance_arn(identifier),
            row.action
        );
        assert!(
            detail.starts_with(&session),
            "case {case:?}: expected the refusal to name the session the role was assumed \
             under ({session}<session>), got {detail:?}"
        );
        assert!(
            detail.ends_with(&sentence),
            "case {case:?}: expected the refusal to end {sentence:?}, got {detail:?}"
        );
    }
}

/// The other half of the rule: a call the execution role DOES allow is made with the
/// member under the operation's own spelling, so the invoked service answers the request
/// itself — its own refusal for an instance it does not hold, reported to the dead-letter
/// queue under the code and message that answer carries, and its own effect for one it
/// does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_allowed_call_is_answered_by_the_invoked_operation() {
    let emulator = emulator();
    // (case, the member spelling the schedule writes, whether the DB instance exists)
    let rows: [(&str, &str, bool); 4] = [
        ("an absent instance named as the model spells the member", "DBInstanceIdentifier", false),
        ("an absent instance named as an AWS SDK spells it", "dbInstanceIdentifier", false),
        ("an absent instance named in that spelling capitalised", "DbInstanceIdentifier", false),
        ("an instance that exists, named as an AWS SDK spells it", "dbInstanceIdentifier", true),
    ];
    let mut prepared = Vec::new();
    for (index, (case, written, exists)) in rows.into_iter().enumerate() {
        let identifier = format!("allowed-spelling-{index}");
        if exists {
            given_db_instance(&emulator, case, &identifier).await;
        }
        let (dead_letter_url, dead_letter_arn) =
            given_queue(&emulator, case, &format!("allowed-spelling-dlq-{index}")).await;
        let instance_arn = db_instance_arn(&identifier);
        let role = given_execution_role_granting(
            &emulator,
            case,
            &format!("allowed-spelling-role-{index}"),
            &[
                ("rds:StopDBInstance", instance_arn.as_str()),
                (SEND_MESSAGE, dead_letter_arn.as_str()),
            ],
        )
        .await;
        let mut members = ::serde_json::Map::new();
        members.insert(written.to_string(), Value::String(identifier.clone()));
        let request = Value::Object(members);
        given_schedule(
            &emulator,
            case,
            &format!("allowed-spelling-{index}"),
            "stopDBInstance",
            &role,
            &request,
            &dead_letter_arn,
        )
        .await;
        prepared.push((case, dead_letter_url, identifier, exists));
    }

    for (case, dead_letter_url, identifier, exists) in &prepared {
        if *exists {
            // The stop the occurrence made is the one RDS performed: the member reached
            // the operation, so the instance leaves `available` for the shutdown it
            // reports and the status the clock then owes it.
            let deadline = Instant::now() + PATIENCE;
            let settled = loop {
                let reported = db_instance_status(&emulator, identifier).await;
                if matches!(reported.as_deref(), Some(STOPPING | STOPPED)) {
                    break reported;
                }
                assert!(
                    Instant::now() < deadline,
                    "case {case:?}: expected the universal target's stop to reach the \
                     instance, got {reported:?}"
                );
                tokio::time::sleep(POLL_INTERVAL).await;
            };
            assert!(
                matches!(settled.as_deref(), Some(STOPPING | STOPPED)),
                "case {case:?}: expected the instance to be stopping or stopped, got {settled:?}"
            );
            // An invocation the target accepted is not reported.
            let deadline = Instant::now() + QUIET_WINDOW;
            while Instant::now() < deadline {
                let reported = received(&emulator, dead_letter_url).await;
                assert!(
                    reported.is_empty(),
                    "case {case:?}: expected nothing on the dead-letter queue, got {reported:?}"
                );
                tokio::time::sleep(POLL_INTERVAL).await;
            }
            continue;
        }
        let reported = messages_once(&emulator, dead_letter_url, 1, PATIENCE).await;
        let message = reported
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: nothing reached the dead-letter queue"));
        // The member reached the operation, so what ended the invocation is that
        // operation's OWN answer for the instance it does not hold — read off the XML
        // document its protocol answers with, code and message both.
        assert_eq!(
            attribute(message, "ERROR_CODE"),
            DB_INSTANCE_NOT_FOUND,
            "case {case:?}: expected the invoked operation's own refusal code, got {message}"
        );
        assert_eq!(
            attribute(message, "ERROR_MESSAGE"),
            format!("DBInstance {identifier} not found."),
            "case {case:?}: expected the invoked operation's own wording, got {message}"
        );
    }
}
