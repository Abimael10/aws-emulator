// SPDX-License-Identifier: CC0-1.0

//! What DRIVES a Step Functions poll loop to its end, and what may cut one short, asked
//! across the three services the question spans — Step Functions interprets the
//! definition and makes its calls, IAM holds the role those calls are made as, and
//! DynamoDB holds the state the loop polls — mounted on ONE engine exactly as
//! `core/router/src/lib.rs` mounts them.
//!
//! RULE: a STANDARD execution is DRIVEN to the terminal state its own definition
//! reaches: a `Task` -> `Choice` -> `Wait` poll loop that gets to a `Succeed` ends
//! `SUCCEEDED`, carrying the data its own states computed, however many turns of work and
//! however many service calls that took. The only spans that cut a run short are the ones
//! the caller declared: the definition's own `TimeoutSeconds`, whose run ends `TIMED_OUT`
//! and names no error of its own because what ran out is the execution's allowance, and a
//! `StopExecution`. What the execution's work is done AS is the state machine's `roleArn`,
//! which the CONTROL plane does not resolve — a create naming a role this account's IAM
//! does not hold is served — and the DATA plane does: that run fails on its first task
//! with `States.TaskFailed`, and the state the task would have written is untouched.
//! Nothing the interpreter itself spends on the way is an outcome a definition can reach.
//!
//! The rows differ along what the rule generalises over: how many turns the loop takes
//! before its own data lets it through (one, two, several), whether the definition
//! declares a `TimeoutSeconds` wider than the loop needs or one its waits go past, and
//! whether the role the work is done as can be assumed at all — the last two rows being
//! machines that differ ONLY in their `roleArn`, so what separates their outcomes is the
//! role and nothing else.
//!
//! Each turn of the loop is a real `dynamodb:updateItem`, so the counter DynamoDB holds
//! when a run is over is how many turns that run really took: a loop reported as
//! `SUCCEEDED` without the calls behind it would leave that counter short. Every row is
//! created and started before any of them is read, so the waits the rows declare run at
//! the same time rather than one after another, and each row's outcome is then waited out
//! until it holds.
//!
//! Rule (b) is pinned inside the owning crate too, at the level each part of it belongs
//! to: `tests/all/definition_decides_the_outcome.rs` for the terminal status being the
//! definition's and the remaining ceilings being counted in work rather than in seconds,
//! and `tests/all/unassumable_role_cause.rs` for the wording of the unassumable-role
//! refusal. What only this target can ask is the CROSS-SERVICE half the recorded check
//! needs: the turns of the loop counted by the service that served them, and one engine
//! on which the control plane accepts the role the data plane then refuses.
//!
//! The harness — the role, the Step Functions caller and the policy statements — is
//! `all/sfn_task_target_authorization.rs`'s, which `tests/all/main.rs` makes reachable
//! through `crate::sfn_task_target_authorization`; only the DynamoDB seam and the
//! wait-out reader this suite's own spans need are stated here.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::sfn_task_target_authorization::{allow, logs_only, role, sfn, string, ACCOUNT, REGION};

/// The table the loop polls. Its one hash key is `pk`, and the attribute each turn of the
/// loop increments — what the `Choice` branches on, and what DynamoDB holds the count of
/// the turns in — is `looks`; both are written as literals below, where an object key is
/// what they are.
const TABLE: &str = "poll-ledger";

/// How long a poll here waits for the condition it is waiting on — a created table
/// becoming `ACTIVE`, an execution leaving `RUNNING`. Generous on purpose, and far wider
/// than the spans the rows themselves declare: every loop stops as soon as its condition
/// holds, so this bounds only a failure and never the verdict.
const PATIENCE: Duration = Duration::from_secs(120);

/// How long that poll waits between looks. Long enough that waiting out a span costs a
/// handful of reads rather than hundreds, and far shorter than the spans the rows declare.
const GLANCE: Duration = Duration::from_millis(250);

/// The seconds the loop waits between two looks. The walk sleeps for none of it: it is
/// the span the execution's own clock advances by per turn.
const BACKOFF_SECONDS: i64 = 1;

/// The `TimeoutSeconds` the row that declares a WIDE one gives its whole execution: far
/// more than its loop needs, so the definition's own bound does not end it.
const AMPLE_TIMEOUT_SECONDS: u64 = 300;

/// The `TimeoutSeconds` the row whose waits go PAST its own bound declares, beside the
/// turns that row's loop would need: "the maximum number of seconds an execution of the
/// state machine can run", which this loop cannot finish inside.
const TIGHT_TIMEOUT_SECONDS: u64 = 2;
/// See [`TIGHT_TIMEOUT_SECONDS`].
const UNREACHABLE_TURNS: i64 = 50;

/// The service principal a state machine's execution role is assumed by, which is the
/// identity a refusal of that assume blames.
const STATES_PRINCIPAL: &str = "states.amazonaws.com";

/// The Amazon States Language's own name for the error a failed `Task` state reports,
/// written out here rather than read from the crate under test: a test that takes the name
/// from the implementation holds whatever the implementation says.
const TASK_FAILED: &str = "States.TaskFailed";

/// Wording no `cause` may carry: a span this process spent interpreting a definition is
/// about the emulator rather than about the definition, so no execution may end naming
/// one.
const INTERPRETER_WORDING: [&str; 3] =
    ["interpreted for longer", "seconds this emulator", "budget"];

/// The three services this question spans, over ONE shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_sfn_awsstepfunctions::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(engine),
    );
    router
}

/// One `awsJson1_0` call to DynamoDB, requiring a 200, as its body.
///
/// Signed for [`REGION`], which is the Region the state machine's own integration calls
/// are made in: a DynamoDB endpoint is regional and a table name is unique within its
/// Region, so the table the loop polls has to be created through the same Region the task
/// addresses it from. `all/cloudformation_dynamodb_table_in_place_update.rs`'s crate-wide
/// caller signs for another Region, which is why the seam is stated again here rather than
/// borrowed.
async fn dynamodb(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/dynamodb/aws4_request, \
         SignedHeaders=host, Signature=0000"
    );
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("dynamodb.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("DynamoDB_20120810.{operation}"))
        .header("authorization", authorization)
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {operation}: expected 200 from DynamoDB, got {status} with {parsed:?}"
    );
    parsed
}

/// The table every row polls, waited out to `ACTIVE` — "you can perform read and write
/// operations only on an ACTIVE table" — and answered by the ARN a policy names it by.
async fn ledger_table(router: &MultiServiceRouter) -> String {
    let created = dynamodb(
        router,
        "CreateTable",
        &json!({
            "TableName": TABLE,
            "KeySchema": [{ "AttributeName": "pk", "KeyType": "HASH" }],
            "AttributeDefinitions": [{ "AttributeName": "pk", "AttributeType": "S" }],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    let arn = created["TableDescription"]["TableArn"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateTable answers the TableArn, got {created}"))
        .to_string();
    let began = Instant::now();
    loop {
        let described = dynamodb(router, "DescribeTable", &json!({ "TableName": TABLE })).await;
        if described["Table"]["TableStatus"].as_str() == Some("ACTIVE") {
            return arn;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "expected {TABLE} to report ACTIVE within {PATIENCE:?}, got {described}"
        );
        ::tokio::time::sleep(GLANCE).await;
    }
}

/// How many turns the ledger row `key` records, and `None` where the row holds no item at
/// all — which is what "the task never happened" reads as.
async fn turns_recorded(router: &MultiServiceRouter, key: &str) -> Option<i64> {
    let read = dynamodb(
        router,
        "GetItem",
        &json!({
            "TableName": TABLE,
            "Key": { "pk": { "S": key } },
            "ConsistentRead": true,
        }),
    )
    .await;
    let counted = read["Item"]["looks"]["N"].as_str()?;
    Some(
        counted
            .parse()
            .unwrap_or_else(|error| panic!("expected a number in {read}, got {error}")),
    )
}

/// The poll loop one row runs: a `Task` that counts this turn in DynamoDB and reports the
/// count, a `Choice` that branches on it, a `Wait` that goes round again, and the
/// `Succeed` the loop reaches once the count has got to `target`.
fn poller(target: i64, timeout_seconds: Option<u64>) -> String {
    let mut definition = json!({
        "StartAt": "Look",
        "States": {
            "Look": {
                "Type": "Task",
                "Resource": "arn:aws:states:::dynamodb:updateItem",
                "Parameters": {
                    "TableName": TABLE,
                    "Key": { "pk": { "S.$": "$.key" } },
                    "UpdateExpression": "ADD looks :one",
                    "ExpressionAttributeValues": { ":one": { "N": "1" } },
                    "ReturnValues": "UPDATED_NEW",
                },
                "ResultSelector": { "looks.$": "States.StringToJson($.Attributes.looks.N)" },
                "ResultPath": "$.ledger",
                "Next": "Ready",
            },
            "Ready": {
                "Type": "Choice",
                "Choices": [{
                    "Variable": "$.ledger.looks",
                    "NumericGreaterThanEquals": target,
                    "Next": "Done",
                }],
                "Default": "Backoff",
            },
            "Backoff": { "Type": "Wait", "Seconds": BACKOFF_SECONDS, "Next": "Look" },
            "Done": { "Type": "Succeed" },
        },
    });
    if let Some(seconds) = timeout_seconds {
        definition["TimeoutSeconds"] = json!(seconds);
    }
    definition.to_string()
}

/// What a row's run must end as.
enum Ends {
    /// The loop reached its `Succeed` after exactly this many turns.
    Succeeded(i64),
    /// The definition's own `TimeoutSeconds` ran out while the loop was still going, so
    /// the run was cut short part way: the turns it did take are between one and the
    /// target it never reached.
    TimedOut { short_of: i64 },
    /// The work was never done at all, because the role it would have been done as cannot
    /// be assumed.
    TaskFailed,
}

/// One row: the turns its loop needs, the `TimeoutSeconds` its definition declares,
/// whether its role is one IAM holds, and how the run ends.
struct Row {
    case: &'static str,
    target: i64,
    timeout_seconds: Option<u64>,
    /// `false` for the row whose `roleArn` names a role this account's IAM does not hold.
    role_is_held: bool,
    ends: Ends,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a loop whose data lets it through on its first look",
            target: 1,
            timeout_seconds: None,
            role_is_held: true,
            ends: Ends::Succeeded(1),
        },
        Row {
            case: "the same loop going round once before its data lets it through",
            target: 2,
            timeout_seconds: None,
            role_is_held: true,
            ends: Ends::Succeeded(2),
        },
        Row {
            case: "the same loop going round several times",
            target: 3,
            timeout_seconds: None,
            role_is_held: true,
            ends: Ends::Succeeded(3),
        },
        Row {
            case: "the same loop under a TimeoutSeconds wider than it needs",
            target: 2,
            timeout_seconds: Some(AMPLE_TIMEOUT_SECONDS),
            role_is_held: true,
            ends: Ends::Succeeded(2),
        },
        Row {
            case: "a loop whose waits go past the TimeoutSeconds its own definition declares",
            target: UNREACHABLE_TURNS,
            timeout_seconds: Some(TIGHT_TIMEOUT_SECONDS),
            role_is_held: true,
            ends: Ends::TimedOut {
                short_of: UNREACHABLE_TURNS,
            },
        },
        Row {
            // The same definition as the second row, differing only in the roleArn the
            // machine was created with.
            case: "the same loop under a role this account's IAM does not hold",
            target: 2,
            timeout_seconds: None,
            role_is_held: false,
            ends: Ends::TaskFailed,
        },
    ]
}

/// Start one execution of `machine_arn` over `input` and answer its ARN.
async fn started(router: &MultiServiceRouter, machine_arn: &str, input: &str) -> String {
    let started = sfn(
        router,
        "StartExecution",
        &json!({ "stateMachineArn": machine_arn, "name": "only", "input": input }),
    )
    .await;
    string(&started, "executionArn")
}

/// `execution_arn`'s description, looked at again until it has left `RUNNING`.
///
/// The borrowed harness has its own reader, with a deadline sized for a definition that
/// declares no waits; the rows here declare several, so the condition is waited out
/// against [`PATIENCE`] instead.
async fn settled(router: &MultiServiceRouter, case: &str, execution_arn: &str) -> Value {
    let began = Instant::now();
    loop {
        let described = sfn(
            router,
            "DescribeExecution",
            &json!({ "executionArn": execution_arn }),
        )
        .await;
        if described.get("status").and_then(Value::as_str) != Some("RUNNING") {
            return described;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: expected the execution to reach the terminal status its definition \
             decides within {PATIENCE:?}, last saw {described}"
        );
        ::tokio::time::sleep(GLANCE).await;
    }
}

/// The `cause` a description carries, or the empty string where it carries none.
fn cause(described: &Value) -> String {
    described
        .get("cause")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Every row's execution is driven to the terminal state its own definition reaches, with
/// the turns DynamoDB recorded showing the work behind it; only the definition's own
/// `TimeoutSeconds` and an unassumable role end a run any other way.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_poll_loop_is_driven_to_the_terminal_state_its_definition_reaches() {
    let router = emulator();
    let table_arn = ledger_table(&router).await;
    let held_role = role(
        &router,
        "PollLoopRole",
        &json!({
            "Version": "2012-10-17",
            "Statement": [logs_only(), allow("dynamodb:UpdateItem", &table_arn)],
        }),
    )
    .await;
    // A role reference of this account that IAM answers nothing for, which is what the
    // control plane accepts and the data plane cannot act as.
    let unheld_role = format!("arn:aws:iam::{ACCOUNT}:role/NoSuchPollLoopRole");

    // Every row is created and started FIRST, so the waits its own definition declares
    // run at the same time as every other row's rather than one after another.
    let table = rows();
    let mut running = Vec::with_capacity(table.len());
    for (index, row) in table.iter().enumerate() {
        let case = row.case;
        let key = format!("run-{index}");
        let role_arn = if row.role_is_held {
            held_role.clone()
        } else {
            unheld_role.clone()
        };

        // The control plane does not resolve the role: every row's create is served, and
        // the record keeps the roleArn the request named.
        let created = sfn(
            &router,
            "CreateStateMachine",
            &json!({
                "name": format!("poll-loop-{index}"),
                "definition": poller(row.target, row.timeout_seconds),
                "roleArn": role_arn,
            }),
        )
        .await;
        let machine_arn = string(&created, "stateMachineArn");
        let described_machine = sfn(
            &router,
            "DescribeStateMachine",
            &json!({ "stateMachineArn": machine_arn }),
        )
        .await;
        assert_eq!(
            described_machine.get("roleArn").and_then(Value::as_str),
            Some(role_arn.as_str()),
            "case {case:?}: expected the created machine to keep the roleArn it was given, got \
             {described_machine:?}"
        );

        let input = json!({ "key": key }).to_string();
        running.push((key, started(&router, &machine_arn, &input).await));
    }

    for (row, (key, execution_arn)) in table.iter().zip(running) {
        let case = row.case;
        let described = settled(&router, case, &execution_arn).await;
        let status = described
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let reported = cause(&described);
        for wording in INTERPRETER_WORDING {
            assert!(
                !reported.contains(wording),
                "case {case:?}: expected no cause about a span this emulator spent interpreting, \
                 got {reported:?} carrying {wording:?}"
            );
        }
        let recorded = turns_recorded(&router, &key).await;

        match row.ends {
            Ends::Succeeded(turns) => {
                assert_eq!(
                    status, "SUCCEEDED",
                    "case {case:?}: expected the Succeed state its definition reaches, got \
                     {described:?}"
                );
                let output: Value = serde_json::from_str(&string(&described, "output"))
                    .unwrap_or_else(|error| {
                        panic!("case {case:?}: expected JSON output, got {error}")
                    });
                assert_eq!(
                    output["ledger"]["looks"].as_i64(),
                    Some(turns),
                    "case {case:?}: expected the output of the turn that let the loop through \
                     ({turns}), got {output}"
                );
                assert_eq!(
                    recorded,
                    Some(turns),
                    "case {case:?}: expected DynamoDB to hold the {turns} turns the loop really \
                     took, got {recorded:?}"
                );
            }
            Ends::TimedOut { short_of } => {
                assert_eq!(
                    status, "TIMED_OUT",
                    "case {case:?}: expected the status the definition's own TimeoutSeconds \
                     produces, got {described:?}"
                );
                // What ran out is the execution's allowance, not anything one state did,
                // so the record names no error of its own.
                assert_eq!(
                    described.get("error").and_then(Value::as_str),
                    None,
                    "case {case:?}: expected no error beside a timeout, got {described:?}"
                );
                let took = recorded.unwrap_or_default();
                assert!(
                    (1..short_of).contains(&took),
                    "case {case:?}: expected a run cut short part way through its loop, between \
                     one turn and the {short_of} it never reached, got {recorded:?}"
                );
            }
            Ends::TaskFailed => {
                assert_eq!(
                    (
                        status.as_str(),
                        described.get("error").and_then(Value::as_str),
                    ),
                    ("FAILED", Some(TASK_FAILED)),
                    "case {case:?}: expected the first task to fail under a role that cannot be \
                     assumed, got {described:?}"
                );
                let expected = format!(
                    "The principal {STATES_PRINCIPAL} is not authorized to assume the provided \
                     role. (role: {unheld_role})"
                );
                assert!(
                    reported.contains(&expected),
                    "case {case:?}: expected the cause {expected:?}, got {reported:?}"
                );
                assert_eq!(
                    recorded, None,
                    "case {case:?}: expected the work the task would have done never to have \
                     happened, got {recorded:?}"
                );
            }
        }
    }
}
