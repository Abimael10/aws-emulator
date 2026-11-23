// SPDX-License-Identifier: CC0-1.0

//! What a FAILED occurrence of a UNIVERSAL (`aws-sdk`) target reports, end to end over
//! the wire through the real `MultiServiceRouter`, with Amazon RDS, Amazon SQS and IAM
//! mounted beside EventBridge Scheduler on ONE engine — so every attribute is asserted
//! where a client reads it, on the schedule's own dead-letter queue.
//!
//! RULE: at each occurrence of a schedule whose `Target.Arn` is the universal form
//! `arn:<partition>:scheduler:::aws-sdk:<service>:<operation>`, EventBridge Scheduler
//! makes that SDK operation's call as `Target.RoleArn` with `Target.Input` as the
//! operation's own request, so the call is subject to the identity policy of that role
//! for the action the operation publishes over the resource its REQUEST names. An
//! occurrence that did not reach its target is reported once to
//! `Target.DeadLetterConfig.Arn`, and the report names the failure THAT ANSWER carries:
//! `ERROR_CODE` is the error code the refusing front door spells the class by — AWS's
//! authorization code for a call the role's policy withholds, and the invoked
//! operation's own modelled code for a request that operation served and refused — while
//! `ERROR_MESSAGE` is that error's message TEXT alone, never a serialized error document
//! and never a stand-in code for a refusal the service classified itself. A refusal of
//! the request itself is never repeated, so `RETRY_ATTEMPTS` is `0` however many retries
//! the schedule's `RetryPolicy` admits.
//!
//! The rows span the class rather than one recorded case: they differ along the OPERATION
//! the universal target names (the stop, the start and the reboot of a DB instance), along
//! the SPELLING `Target.Input` writes the member that addresses the instance in (the
//! model's own, an AWS SDK's, and that SDK spelling capitalised), along WHICH of the two
//! refusal classes ends the occurrence (the execution role withholding the call, and the
//! invoked operation's own answer for an instance it does not hold), and along the retry
//! allowance the schedule states (none, one, several).
//!
//! Nothing here waits for a fixed span: every wait watches for the CONDITION it expects
//! until a generous deadline.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::dead_letter_delivery::{body_document, messages_once};
use crate::schedule_invocation::{
    at_expression_body, given_execution_role_granting, given_queue, scheduler, seconds_from_now,
    REGION,
};
use crate::wire_harness::account_id;

/// How long a row watches for the report it expects. Every schedule below states its one
/// occurrence [`OCCURRENCE_IN`] seconds ahead, so this is room for the clock's tick, the
/// attempted call and the dead-letter delivery rather than a latency expectation.
const PATIENCE: Duration = Duration::from_secs(120);

/// How far ahead of the clock every schedule below states its one occurrence.
const OCCURRENCE_IN: u64 = 6;

/// How long retries of an occurrence below would continue for: generous, so no row is
/// ended by the age its policy bounds attempts with and what each report says about
/// retries is the failure's own class.
const EVENT_AGE_SECONDS: i64 = 3_600;

/// The IAM action the delivery of a dead-letter message is authorized by — the
/// `sqs:SendMessage` AWS documents for a schedule's dead-letter queue.
const SEND_MESSAGE: &str = "sqs:SendMessage";

/// The code AWS's authorization refusal of a call to a service whose protocol is
/// `awsQuery` — Amazon RDS among them — names its class by.
const QUERY_ACCESS_DENIED: &str = "AccessDenied";

/// The code Amazon RDS answers a request addressing a DB instance it does not hold with,
/// as that model binds the fault's own query code
/// (`smithy_query(query="operation",operation="StopDBInstance,StartDBInstance,\
/// RebootDBInstance",model_path="services/service-com-amazonaws-rds-amazonrdsv19/\
/// smithy-model/com-amazonaws-rds-amazonrdsv19.json")`: `DBInstanceNotFoundFault` carries
/// `awsQueryError` code `DBInstanceNotFound`, `httpResponseCode` 404).
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
    router.register_service(service_com_amazonaws_rds_amazonrdsv19::EmulatedService::new(
        engine,
    ));
    router
}

/// One row: the operation the universal target names, how its request writes the member
/// that addresses the DB instance, what the execution role holds, and the retry allowance
/// the schedule states.
struct Row {
    case: &'static str,
    /// The operation field of the target's ARN, in the lowerCamelCase the AWS SDK family
    /// names an operation in.
    operation: &'static str,
    /// The IAM action that operation publishes, which a withheld call's refusal names and
    /// which a permitted row's role is granted.
    action: &'static str,
    /// The member name `Target.Input` writes the instance under.
    written: &'static str,
    /// Whether the execution role holds [`Row::action`] on the DB instance the request
    /// names: a role holding it lets the call through to the operation itself, and a role
    /// holding only the dead-letter write withholds it.
    permitted: bool,
    /// `RetryPolicy$MaximumRetryAttempts` the schedule states.
    retries: i64,
}

/// The DB instance identifier is deliberately never created, so a row whose role permits
/// the call reaches the invoked operation's own refusal for an instance that is not there.
const ROWS: &[Row] = &[
    Row {
        case: "a stop withheld by a role holding only the dead-letter write",
        operation: "stopDBInstance",
        action: "rds:StopDBInstance",
        written: "DbInstanceIdentifier",
        permitted: false,
        retries: 1,
    },
    Row {
        case: "the same stop under a role that holds the action",
        operation: "stopDBInstance",
        action: "rds:StopDBInstance",
        written: "DbInstanceIdentifier",
        permitted: true,
        retries: 1,
    },
    Row {
        case: "a start withheld under a policy admitting no retry",
        operation: "startDBInstance",
        action: "rds:StartDBInstance",
        written: "DBInstanceIdentifier",
        permitted: false,
        retries: 0,
    },
    Row {
        case: "a permitted start naming the member as an AWS SDK spells it",
        operation: "startDBInstance",
        action: "rds:StartDBInstance",
        written: "dbInstanceIdentifier",
        permitted: true,
        retries: 3,
    },
    Row {
        case: "a reboot withheld under a policy admitting several retries",
        operation: "rebootDBInstance",
        action: "rds:RebootDBInstance",
        written: "dbInstanceIdentifier",
        permitted: false,
        retries: 3,
    },
    Row {
        case: "a permitted reboot, whose own wording of the fault differs",
        operation: "rebootDBInstance",
        action: "rds:RebootDBInstance",
        written: "DBInstanceIdentifier",
        permitted: true,
        retries: 1,
    },
];

/// The sentence Amazon RDS words its `DBInstanceNotFoundFault` in for the operation the
/// row names, read off each handler of that service rather than restated as one wording
/// for the fault: `src/ops/stop_db_instance.rs` and `src/ops/start_db_instance.rs` word it
/// `DBInstance <identifier> not found.`, while `src/ops/reboot_db_instance.rs` words the
/// same fault `DBInstance not found: <identifier>`.
fn not_found_sentence(operation: &str, identifier: &str) -> String {
    match operation {
        "rebootDBInstance" => format!("DBInstance not found: {identifier}"),
        _ => format!("DBInstance {identifier} not found."),
    }
}

/// The ARN an access request over the DB instance of `identifier` is taken on, which is
/// the resource an authorization refusal of a call on it names.
fn db_instance_arn(identifier: &str) -> String {
    format!("arn:aws:rds:{REGION}:{}:db:{identifier}", account_id())
}

/// The `StringValue` a dead-letter message carries under the attribute `name`, or `None`
/// for an attribute it does not carry.
fn attribute(message: &Value, name: &str) -> Option<String> {
    message["MessageAttributes"][name]["StringValue"]
        .as_str()
        .map(ToString::to_string)
}

/// Every row: the occurrence's own failure is what its dead-letter report names — the
/// authorization code and sentence for a call the role withheld, and the invoked
/// operation's own code and sentence for a request it answered — with the request the
/// attempt made as the body and no retry counted for either.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_universal_occurrence_reports_the_answers_own_code_and_message() {
    let emulator = emulator();
    let mut prepared = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let identifier = format!("refusal-report-{index}");
        let instance_arn = db_instance_arn(&identifier);
        let (dead_letter_url, dead_letter_arn) =
            given_queue(&emulator, case, &format!("refusal-report-dlq-{index}")).await;
        // Every row's role may write to its own dead-letter queue, so the report is
        // delivered whichever refusal ended the occurrence; only the row's own action is
        // what the rows differ on.
        let mut grants = vec![(SEND_MESSAGE, dead_letter_arn.clone())];
        if row.permitted {
            grants.push((row.action, instance_arn.clone()));
        }
        let held: Vec<(&str, &str)> = grants
            .iter()
            .map(|(action, resource)| (*action, resource.as_str()))
            .collect();
        let role = given_execution_role_granting(
            &emulator,
            case,
            &format!("refusal-report-role-{index}"),
            &held,
        )
        .await;
        let mut members = ::serde_json::Map::new();
        members.insert(row.written.to_string(), Value::String(identifier.clone()));
        let request = Value::Object(members);
        let target = format!("arn:aws:scheduler:::aws-sdk:rds:{}", row.operation);
        let (status, created) = scheduler(
            &emulator,
            "POST",
            &format!("/schedules/refusal-report-{index}"),
            Some(json!({
                "ScheduleExpression":
                    format!("at({})", at_expression_body(seconds_from_now(OCCURRENCE_IN))),
                "ScheduleExpressionTimezone": "UTC",
                "FlexibleTimeWindow": { "Mode": "OFF" },
                "State": "ENABLED",
                "Target": {
                    "Arn": target,
                    "RoleArn": role,
                    "Input": request.to_string(),
                    "RetryPolicy": {
                        "MaximumRetryAttempts": row.retries,
                        "MaximumEventAgeInSeconds": EVENT_AGE_SECONDS,
                    },
                    "DeadLetterConfig": { "Arn": dead_letter_arn },
                },
            })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateSchedule answered {status} {created}"
        );
        let schedule_arn = created["ScheduleArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no ScheduleArn in {created}"))
            .to_string();
        prepared.push((
            row,
            dead_letter_url,
            identifier,
            instance_arn,
            request,
            target,
            role,
            schedule_arn,
        ));
    }

    for (row, dead_letter_url, identifier, instance_arn, request, target, role, schedule_arn) in
        &prepared
    {
        let case = row.case;
        let reported = messages_once(&emulator, dead_letter_url, 1, PATIENCE).await;
        let message = reported.first().unwrap_or_else(|| {
            panic!("case {case:?}: nothing reached the dead-letter queue of {target:?}")
        });
        // The failed event is the request the occurrence attempted, which for a universal
        // target IS `Target.Input` as the schedule wrote it.
        assert_eq!(
            body_document(message),
            *request,
            "case {case:?}: expected the dead-lettered request {request}, got {message}"
        );
        let code = attribute(message, "ERROR_CODE").unwrap_or_default();
        let expected_code = if row.permitted {
            DB_INSTANCE_NOT_FOUND
        } else {
            QUERY_ACCESS_DENIED
        };
        // The code the answering front door spells the class by, and so never the code this
        // service reports a failure nothing could be read from under.
        assert_eq!(
            code, expected_code,
            "case {case:?}: expected the code the answering front door spells the class by \
             ({expected_code}), got {code:?} in {message}"
        );
        let detail = attribute(message, "ERROR_MESSAGE").unwrap_or_default();
        if row.permitted {
            // The member reached the operation under its own spelling, so what ended the
            // occurrence is that operation's OWN answer for an instance it does not hold —
            // which also shows the identifier arrived: a request missing it would be
            // refused on the member instead.
            let sentence = not_found_sentence(row.operation, identifier);
            assert_eq!(
                detail, sentence,
                "case {case:?}: expected the invoked operation's own wording {sentence:?}, got \
                 {detail:?}"
            );
        } else {
            // AWS's authorization sentence: the session the execution role was assumed
            // under, the action the operation publishes, and the resource the request
            // named. The session segment is per-invocation, so only the text around it is
            // asserted.
            let role_name = role.rsplit('/').next().unwrap_or_default();
            let session = format!("User: arn:aws:sts::{}:assumed-role/{role_name}/", account_id());
            let sentence = format!(
                " is not authorized to perform: {} on resource: {instance_arn} because no \
                 identity-based policy allows the {} action",
                row.action, row.action
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
        // Whichever class ended the occurrence, the message is that error's TEXT: none of
        // the markup a serialized refusal of a service answering XML carries, and nothing
        // of the envelope around the error.
        for markup in ["<?xml", "<ErrorResponse", "<Code>", "<Message>", "RequestId"] {
            assert!(
                !detail.contains(markup),
                "case {case:?}: expected the error's message text alone, got {markup:?} in \
                 {detail:?}"
            );
        }
        for (name, expected) in [
            ("TARGET_ARN", target.clone()),
            ("SCHEDULE_ARN", schedule_arn.clone()),
            // Neither class of refusal is worth repeating, so no retry was made of either
            // however many the row's policy admitted.
            ("RETRY_ATTEMPTS", "0".to_string()),
        ] {
            let carried = attribute(message, name);
            assert_eq!(
                carried,
                Some(expected.clone()),
                "case {case:?}: expected {name} to carry {expected:?}, got {message}"
            );
        }
    }
}
