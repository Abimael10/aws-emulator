// SPDX-License-Identifier: CC0-1.0

//! The dead-letter report of an invocation that could not reach a FUNCTION target:
//! end to end over the wire through the real `MultiServiceRouter`, with Lambda, Amazon
//! SQS and IAM mounted beside EventBridge Scheduler on ONE engine — so the report is
//! asserted where a client reads it, in the message the schedule's dead-letter queue
//! holds.
//!
//! RULE: what a schedule's `Target.DeadLetterConfig` queue is handed is the REQUEST the
//! failed attempt made of its target's own API, carrying every member that attempt set
//! with the value it was made with — the members the schedule states, and the members
//! the target's KIND decides rather than the schedule — because a client replays the
//! failed invocation by resubmitting that document, and a member missing from it is a
//! member the replay does not state. A function target's API is Lambda's `Invoke`,
//! whose input members are `FunctionName`, `Payload` and the `InvocationType` the call
//! is made with (`smithy_query` query="operation" operation="Invoke" on Lambda's
//! model), and a schedule does not wait for its function target's answer, so every
//! such attempt is made as `Event` — the asynchronous invocation of that member's own
//! enum (`smithy_query` query="shape" shape="InvocationType": `Event`,
//! `RequestResponse`, `DryRun`) — and the reported document states it.
//!
//! The rows span the class rather than one recorded case: they differ along the FORM of
//! the payload the invocation carried (a nested document, a flat object, a top-level
//! array, text that is not JSON, and no `Input` at all), and along WHY the invocation
//! failed (the function was deleted before the occurrence, the account never held it,
//! and the execution role may not invoke the one it does hold) — none of which changes
//! the request the attempt made.
//!
//! The second suite below pins the other half of the rule: the invocation the reported
//! document states is the one the attempt actually makes, read where Lambda holds it.

use std::time::{Duration, Instant};

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

// The queue-reading and body-parsing helpers this suite shares with the other
// dead-letter suite (`tests/all/dead_letter_delivery.rs`): a report is read with every
// message attribute it carries, and its body is read as the request document it is.
use crate::dead_letter_delivery::{body_document, messages_once, received};
use crate::per_schedule_target_input::{
    emulator_with_lambda, given_function, given_invoking_role, invoked_payloads, FUNCTIONS_URI,
    INVOKE_FUNCTION,
};
use crate::schedule_invocation::{
    at_expression_body, given_queue, scheduler, seconds_from_now, POLL_INTERVAL, REGION,
};
use crate::wire_harness::account_id;

/// How long a suite waits for the reports it expects. Every schedule below states its
/// one occurrence [`OCCURRENCE_IN`] seconds ahead; the rest is room for the clock's
/// tick, the refused invocation and the dead-letter delivery.
const PATIENCE: Duration = Duration::from_secs(80);

/// How long the suite keeps watching a queue after the report it expected has arrived,
/// so a second report of one invocation has had its chance to appear. Nothing is
/// asserted about how long anything took: the window is only how long the absence is
/// watched for.
const QUIET_WINDOW: Duration = Duration::from_secs(8);

/// How far ahead of the clock every schedule below states its one occurrence.
const OCCURRENCE_IN: u64 = 5;

/// The IAM action the delivery of a dead-letter message is authorized by — the same
/// `sqs:SendMessage` a queue target's invocation is, which is the grant AWS documents
/// for a schedule's dead-letter queue.
const SEND_MESSAGE: &str = "sqs:SendMessage";

/// The member of Lambda's `Invoke` request that states the KIND of invocation, and the
/// value a schedule's function target is always invoked with.
const INVOCATION_TYPE_MEMBER: &str = "InvocationType";
/// See [`INVOCATION_TYPE_MEMBER`].
const ASYNCHRONOUS_INVOCATION: &str = "Event";

/// What a target with no `Input` of its own is handed, which is then the payload the
/// reported request carries.
const DEFAULT_PAYLOAD: &str = "{}";

/// Why a row's invocation of its function target fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failing {
    /// The function was created and then DELETED before the occurrence: Lambda answers
    /// the invoke with `ResourceNotFoundException` (404), which no further attempt
    /// would be answered differently.
    Deleted,
    /// The target ARN names a function this account never held.
    NeverHeld,
    /// The function exists, and the schedule's execution role does not allow the
    /// invocation of it.
    Withheld,
}

/// One row: the payload the schedule hands its function target, why the invocation
/// fails, and the error code the report carries for it.
struct Row {
    case: &'static str,
    /// `Target.Input`, or `None` for a schedule stating none at all.
    input: Option<&'static str>,
    failing: Failing,
    /// The error code the service that refused the call answers it with.
    refused_with: &'static str,
}

/// The refusal Lambda answers an invoke of a function it does not hold with.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The refusal AWS's authorization layer answers a call the caller's permissions
/// withhold with, in the modern form Lambda's protocol spells it in.
const ACCESS_DENIED: &str = "AccessDeniedException";

const ROWS: &[Row] = &[
    Row {
        case: "a nested document whose function was deleted before the occurrence",
        input: Some(
            r#"{"marker":"nested","list":[1,2.5,null,true],"unicode":"café ✓","quote":"she said \"hi\""}"#,
        ),
        failing: Failing::Deleted,
        refused_with: RESOURCE_NOT_FOUND,
    },
    Row {
        case: "a flat object whose function this account never held",
        input: Some(r#"{"marker":"flat","tenant":"b"}"#),
        failing: Failing::NeverHeld,
        refused_with: RESOURCE_NOT_FOUND,
    },
    Row {
        case: "a top-level array whose function was deleted before the occurrence",
        input: Some(r#"[1,"two",{"three":3}]"#),
        failing: Failing::Deleted,
        refused_with: RESOURCE_NOT_FOUND,
    },
    Row {
        case: "text that is not JSON, for a function this account never held",
        input: Some("a line of plain text, not JSON"),
        failing: Failing::NeverHeld,
        refused_with: RESOURCE_NOT_FOUND,
    },
    Row {
        case: "no Input at all, for a function this account never held",
        input: None,
        failing: Failing::NeverHeld,
        refused_with: RESOURCE_NOT_FOUND,
    },
    Row {
        case: "a flat object whose execution role may not invoke the function",
        input: Some(r#"{"marker":"withheld"}"#),
        failing: Failing::Withheld,
        refused_with: ACCESS_DENIED,
    },
];

/// Every row: the one message the schedule's dead-letter queue holds carries the whole
/// Lambda `Invoke` request the attempt made — the function it addressed, the payload it
/// carried, and the asynchronous `InvocationType` it was made with — under the error
/// code the service that refused the call answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_function_targets_dead_letter_report_states_the_whole_invoke_request() {
    let (emulator, _engine) = emulator_with_lambda();
    let occurrence = seconds_from_now(OCCURRENCE_IN);
    // Every row's schedule is written on ONE emulator, each with its own dead-letter
    // queue, so the suite waits for one occurrence rather than one per row.
    let mut prepared = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let (dead_letter_url, dead_letter_arn) =
            given_queue(&emulator, case, &format!("fn-dlq-{index}")).await;
        let function = format!("dead-letter-fn-{index}");
        let function_arn = match row.failing {
            // A function this account never held: the ARN is the one Lambda would hold
            // such a function under, and nothing holds it.
            Failing::NeverHeld => format!(
                "arn:aws:lambda:{REGION}:{}:function:{function}",
                account_id()
            ),
            Failing::Deleted => {
                let arn = given_function(&emulator, case, &function).await;
                delete_function(&emulator, case, &function).await;
                arn
            }
            Failing::Withheld => given_function(&emulator, case, &function).await,
        };
        // The role may always write to the row's dead-letter queue, so the report is
        // delivered whatever withheld the invocation itself.
        let mut granted = vec![(SEND_MESSAGE, dead_letter_arn.as_str())];
        if row.failing != Failing::Withheld {
            granted.push((INVOKE_FUNCTION, function_arn.as_str()));
        }
        let role =
            given_invoking_role(&emulator, case, &format!("fn-dlq-role-{index}"), &granted).await;
        let mut target = json!({
            "Arn": function_arn,
            "RoleArn": role,
            // The failures below are refusals of the request itself, which no further
            // attempt would be answered differently; the policy is stated all the same,
            // so the report is of an invocation that retried nothing.
            "RetryPolicy": { "MaximumRetryAttempts": 0 },
            "DeadLetterConfig": { "Arn": dead_letter_arn },
        });
        if let Some(input) = row.input {
            target["Input"] = json!(input);
        }
        let (status, created) = scheduler(
            &emulator,
            "POST",
            &format!("/schedules/fn-dead-letter-{index}"),
            Some(json!({
                "ScheduleExpression": format!("at({})", at_expression_body(occurrence)),
                "ScheduleExpressionTimezone": "UTC",
                "FlexibleTimeWindow": { "Mode": "OFF" },
                "State": "ENABLED",
                "Target": target,
            })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateSchedule answered {status} {created}"
        );
        prepared.push((row, dead_letter_url, function_arn));
    }

    for (row, dead_letter_url, function_arn) in &prepared {
        let case = row.case;
        let expected = json!({
            "FunctionName": function_arn,
            INVOCATION_TYPE_MEMBER: ASYNCHRONOUS_INVOCATION,
            "Payload": row.input.unwrap_or(DEFAULT_PAYLOAD),
        });
        let reported = messages_once(&emulator, dead_letter_url, 1, PATIENCE).await;
        let message = reported
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: nothing was dead-lettered"));
        assert_eq!(
            body_document(message),
            expected,
            "case {case:?}: expected the dead-lettered request {expected}, got {message}"
        );
        assert_eq!(
            message["MessageAttributes"]["ERROR_CODE"]["StringValue"].as_str(),
            Some(row.refused_with),
            "case {case:?}: expected {:?} to be reported, got {message}",
            row.refused_with
        );
        assert_eq!(
            message["MessageAttributes"]["TARGET_ARN"]["StringValue"].as_str(),
            Some(function_arn.as_str()),
            "case {case:?}: expected the target {function_arn:?} to be reported, got {message}"
        );
        // The policy admitted no retry, so the report is of the invocation's first and
        // only attempt.
        assert_eq!(
            message["MessageAttributes"]["RETRY_ATTEMPTS"]["StringValue"].as_str(),
            Some("0"),
            "case {case:?}: expected no retry to be reported, got {message}"
        );
    }

    // Each queue holds that ONE report and no other: an invocation whose failure is a
    // refusal of the request is reported once.
    let deadline = Instant::now() + QUIET_WINDOW;
    while Instant::now() < deadline {
        for (row, dead_letter_url, _) in &prepared {
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

/// The payload a schedule whose function target IS invocable hands it, which the suite
/// below reads where Lambda holds the invocation.
const DELIVERED_INPUT: &str = r#"{"marker":"delivered"}"#;

/// The invocation the reported request states is the one the attempt MAKES: a schedule
/// whose function target accepts the call is held by Lambda as an ASYNCHRONOUS
/// invocation, which is the `InvocationType` a report of the same target's failure
/// names, and nothing is reported to the dead-letter queue.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_function_target_is_invoked_as_the_reported_invocation_type_says() {
    let case = "a function target that accepts the invocation";
    let (emulator, engine) = emulator_with_lambda();
    let (dead_letter_url, dead_letter_arn) = given_queue(&emulator, case, "fn-quiet-dlq").await;
    let function = "invoked-handler";
    let function_arn = given_function(&emulator, case, function).await;
    let role = given_invoking_role(
        &emulator,
        case,
        "fn-invoking-role",
        &[
            (INVOKE_FUNCTION, function_arn.as_str()),
            (SEND_MESSAGE, dead_letter_arn.as_str()),
        ],
    )
    .await;
    let (status, created) = scheduler(
        &emulator,
        "POST",
        "/schedules/fn-invoked",
        Some(json!({
            "ScheduleExpression": format!("at({})", at_expression_body(seconds_from_now(OCCURRENCE_IN))),
            "ScheduleExpressionTimezone": "UTC",
            "FlexibleTimeWindow": { "Mode": "OFF" },
            "State": "ENABLED",
            "Target": {
                "Arn": function_arn,
                "RoleArn": role,
                "Input": DELIVERED_INPUT,
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

    // Lambda holds the invocation among the ASYNCHRONOUS ones it queued for the
    // function — the `Event` invocation the reported request names — rather than having
    // answered a caller that waited for the result.
    let deadline = Instant::now() + PATIENCE;
    let held = loop {
        let held = invoked_payloads(&engine, function);
        if !held.is_empty() || Instant::now() >= deadline {
            break held;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    assert!(
        held.contains(DELIVERED_INPUT),
        "case {case:?}: expected Lambda to hold the asynchronous invocation carrying \
         {DELIVERED_INPUT:?}, got {held:?}"
    );
    // An invocation the target accepted is not reported: nothing reaches the
    // dead-letter queue inside the window.
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

/// Delete the function of `name`, so an invocation of it is refused by Lambda the way
/// it refuses one of a function nothing holds.
///
/// `DeleteFunction` is bound to `DELETE /2015-03-31/functions/{FunctionName}` and
/// answers 204 No Content.
pub(crate) async fn delete_function(router: &MultiServiceRouter, case: &str, name: &str) {
    let request = http::Request::builder()
        .method("DELETE")
        .uri(format!("{FUNCTIONS_URI}/{name}"))
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTEST/20260101/{REGION}/lambda/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::new())
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let deleted = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 204,
        "case {case:?}: DeleteFunction answered {status} {deleted}"
    );
}
