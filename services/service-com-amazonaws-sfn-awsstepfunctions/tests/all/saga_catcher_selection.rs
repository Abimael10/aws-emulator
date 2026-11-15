// SPDX-License-Identifier: CC0-1.0

//! WHICH catcher of a failed `Task` state's `Catch` LIST takes its failure over, driven
//! over all three services' own wire protocols through the production
//! `MultiServiceRouter` with Step Functions, Lambda and DynamoDB mounted on ONE engine —
//! so the saga's reserve step really writes its item through the optimized
//! `dynamodb:putItem` integration, the charge step's function really runs and really
//! raises, and the compensating function's own effect is read back through DynamoDB's own
//! API.
//!
//! RULE: a `Task` state whose work reported a failure is taken over by the FIRST entry of
//! its `Catch` list whose `ErrorEquals` names that failure's error name — any one of the
//! names that entry lists, or either documented wildcard — and by that entry alone: the
//! `{Error, Cause}` error output is placed through THAT entry's own `ResultPath` and the
//! execution continues at THAT entry's `Next`, so the entries before it and after it
//! decide nothing. With no entry naming the error, none of them is applied and the
//! execution fails under the error's own name.
//!
//! The rows differ along what the rule generalises over: the POSITION in the list of the
//! entry that names the error (the first of two, the second of two, the second of a list
//! whose matching entry names several classes, the third of three, and none at all), how
//! that entry names it (one of its own several names, the task-failure wildcard, the
//! catch-all), and the class the handler raised (four of the function's own and one of the
//! language's). Each entry of a row's list routes to a compensating branch of its OWN, and
//! each branch reads the error output at ITS OWN entry's `ResultPath` — which is how a row
//! shows which single entry was applied rather than only that something was caught.
//!
//! The harness (`Chain`, `given_function_of`, `ran`, `sfn`, `ddb`) is the one
//! `lambda_task_control_flow` builds for these services, and the table both handlers act
//! on, the names its items carry, the wildcards a catcher may name and the reading of a
//! run's own error document are `saga_compensation_on_task_failure`'s; `tests/all.rs` is
//! this binary's crate root, so both are reached through `crate::<suite>`.

use serde_json::{json, Value};

use crate::lambda_task_control_flow::Chain;
use crate::saga_compensation_on_task_failure::{
    document, given_table, item, ALL_ERRORS_WILDCARD, ERROR_ATTRIBUTE, FUNCTION_TIMEOUT_SECONDS,
    KEY, RESERVED, STATUS_ATTRIBUTE, TABLE, TASK_FAILED_WILDCARD,
};
use crate::state_machine_lifecycle::string;

/// The attribute the compensating handler records the MESSAGE of the run's own error
/// document under, beside the error name it records under [`ERROR_ATTRIBUTE`].
const MESSAGE_ATTRIBUTE: &str = "message";

/// The `Resource` of the optimized DynamoDB `PutItem` integration the reserve step is,
/// and the one of the optimized Lambda integration the charge and compensating steps are.
const PUT_ITEM_RESOURCE: &str = "arn:aws:states:::dynamodb:putItem";
/// See [`PUT_ITEM_RESOURCE`].
const INVOKE_RESOURCE: &str = "arn:aws:states:::lambda:invoke";

/// The classes the charge handler raises when a row asks it for one: four of its own, and
/// one of the language's own, so a row's catchers are evaluated against a name the
/// function chose rather than one name for every row.
const CHARGE_DECLINED: &str = "ChargeDeclined";
/// See [`CHARGE_DECLINED`].
const CARD_EXPIRED: &str = "CardExpired";
/// See [`CHARGE_DECLINED`].
const INVENTORY_UNAVAILABLE: &str = "InventoryUnavailable";
/// See [`CHARGE_DECLINED`].
const GATEWAY_TIMEOUT: &str = "PaymentGatewayTimeout";
/// See [`CHARGE_DECLINED`].
const CHARGE_REFUSED: &str = "ChargeRefused";
/// See [`CHARGE_DECLINED`]: one of the language's own classes rather than the function's.
const BUILTIN_ERROR: &str = "ValueError";

/// What a row's input asks the charge handler for when the row's charge must ANSWER, so
/// no catcher of its list is evaluated at all.
const CHARGE_IT: &str = "none";

/// The states the saga is made of, beside the per-catcher branches [`compensate_state`]
/// and [`rolled_back_state`] name — written here because the rows assert which of them an
/// execution entered.
const RESERVE_STATE: &str = "Reserve";
/// See [`RESERVE_STATE`].
const CHARGE_STATE: &str = "Charge";
/// See [`RESERVE_STATE`].
const CONFIRM_STATE: &str = "Confirm";

/// The charge step: it raises the class the document names, with a message naming the
/// order, unless that document asks it to charge — so what the task reports is the run's
/// own error document, under the class the row is about.
const CHARGE_HANDLER: &str = r#"
class ChargeDeclined(Exception):
    pass


class CardExpired(Exception):
    pass


class InventoryUnavailable(Exception):
    pass


class PaymentGatewayTimeout(Exception):
    pass


class ChargeRefused(Exception):
    pass


def handler(event, context):
    order = event["orderId"]
    raised = event["raise"]
    message = "the charge for %s was declined" % order
    if raised == "none":
        return {"orderId": order, "status": "charged"}
    if raised == "ChargeDeclined":
        raise ChargeDeclined(message)
    if raised == "CardExpired":
        raise CardExpired(message)
    if raised == "InventoryUnavailable":
        raise InventoryUnavailable(message)
    if raised == "PaymentGatewayTimeout":
        raise PaymentGatewayTimeout(message)
    if raised == "ChargeRefused":
        raise ChargeRefused(message)
    raise ValueError(message)
"#;

/// The compensating step, which every branch of every row invokes: its whole event IS the
/// error output the catcher that took the failure over placed at its own `ResultPath`, so
/// it records the failure it is compensating out of that document alone — the error's own
/// name and, out of the cause, the message the run raised.
const COMPENSATE_HANDLER: &str = r#"
import json

import boto3

CLIENT = boto3.client("dynamodb")


def handler(event, context):
    raised = json.loads(event["Cause"])
    CLIENT.put_item(
        TableName=TABLE,
        Item={
            "pk": {"S": "compensation-" + event["Error"]},
            "error": {"S": event["Error"]},
            "message": {"S": raised["errorMessage"]},
        },
    )
    return {"compensated": event["Error"]}
"#;

/// [`COMPENSATE_HANDLER`] over the table this suite's items live in: the compensating
/// function is handed the error output as its whole event, so the table it writes to is
/// the package's own rather than a member of that document.
fn compensate_handler() -> String {
    format!("TABLE = \"{TABLE}\"\n{COMPENSATE_HANDLER}")
}

/// The message the charge handler raises for `order`, built the way the handler builds it.
fn raised_message(order: &str) -> String {
    format!("the charge for {order} was declined")
}

/// The key the compensating handler records its item under for the failure named `error`,
/// built the way the handler builds it.
fn record_key(error: &str) -> String {
    format!("compensation-{error}")
}

/// The compensating state the catcher at `index` routes to, and the `Succeed`-path state
/// that branch ends through — one pair per entry of a row's list, so the branch that ran
/// names the entry that was applied.
fn compensate_state(index: usize) -> String {
    format!("Compensate{index}")
}

/// See [`compensate_state`].
fn rolled_back_state(index: usize) -> String {
    format!("RolledBack{index}")
}

/// Where the catcher at `index` places the error output: a member of the document, a
/// member two levels down that the document does not carry yet, and — for a third entry —
/// no `ResultPath` at all, whose default of `$` replaces the whole document with it. The
/// compensating branch that entry routes to reads the error output at this very path, so
/// a row applying another entry's path reaches another branch's reading of it.
fn result_path(index: usize) -> Option<&'static str> {
    match index {
        0 => Some("$.error"),
        1 => Some("$.saga.failure"),
        _ => None,
    }
}

/// One row: the `Catch` list the charge state declares, what the charge handler is asked
/// to end with, and the outcome that list then decides.
struct Row {
    case: &'static str,
    /// The `ErrorEquals` list of each entry of the charge state's `Catch`, in the order
    /// the state declares them.
    catchers: Vec<Vec<&'static str>>,
    /// The class the charge handler raises, or [`CHARGE_IT`] for a row whose charge
    /// answers.
    raises: &'static str,
    outcome: Outcome,
}

/// What a row's list decides about the failure its charge step reported.
#[derive(Debug, Clone, Copy)]
enum Outcome {
    /// The entry at this position named the error, so its own branch ran.
    Caught { entry: usize },
    /// No entry named the error, so none of them was applied.
    Uncaught,
    /// The charge answered, so no entry was evaluated at all.
    Charged,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the first of two entries names the raised class",
            catchers: vec![vec![CHARGE_DECLINED], vec![ALL_ERRORS_WILDCARD]],
            raises: CHARGE_DECLINED,
            outcome: Outcome::Caught { entry: 0 },
        },
        Row {
            case: "the second entry names it and the first does not",
            catchers: vec![vec![INVENTORY_UNAVAILABLE], vec![CARD_EXPIRED]],
            raises: CARD_EXPIRED,
            outcome: Outcome::Caught { entry: 1 },
        },
        Row {
            case: "an entry naming several classes is applied for one of them",
            catchers: vec![
                vec![GATEWAY_TIMEOUT],
                vec![CARD_EXPIRED, INVENTORY_UNAVAILABLE, CHARGE_REFUSED],
            ],
            raises: INVENTORY_UNAVAILABLE,
            outcome: Outcome::Caught { entry: 1 },
        },
        Row {
            case: "the catch-all last takes over a class no earlier entry names",
            catchers: vec![
                vec![CARD_EXPIRED],
                vec![CHARGE_DECLINED, INVENTORY_UNAVAILABLE],
                vec![ALL_ERRORS_WILDCARD],
            ],
            raises: BUILTIN_ERROR,
            outcome: Outcome::Caught { entry: 2 },
        },
        Row {
            case: "the task-failure wildcard first, before a catch-all that never runs",
            catchers: vec![vec![TASK_FAILED_WILDCARD], vec![ALL_ERRORS_WILDCARD]],
            raises: GATEWAY_TIMEOUT,
            outcome: Outcome::Caught { entry: 0 },
        },
        Row {
            case: "no entry of the list names the raised class",
            catchers: vec![vec![CARD_EXPIRED], vec![INVENTORY_UNAVAILABLE]],
            raises: CHARGE_REFUSED,
            outcome: Outcome::Uncaught,
        },
        Row {
            case: "a charge step that answers, so no entry is evaluated at all",
            catchers: vec![vec![ALL_ERRORS_WILDCARD]],
            raises: CHARGE_IT,
            outcome: Outcome::Charged,
        },
    ]
}

/// The saga a row runs: the optimized `PutItem` reserve step, the charge task carrying the
/// row's whole `Catch` list, one compensating branch per entry of that list reading the
/// error output where that entry places it, and a `Succeed` state on every path.
fn definition(row: &Row, charge: &str, compensate: &str) -> Value {
    let mut catchers = Vec::new();
    let mut states = json!({
        RESERVE_STATE: {
            "Type": "Task",
            "Resource": PUT_ITEM_RESOURCE,
            "Parameters": {
                "TableName": TABLE,
                "Item": { KEY: { "S.$": "$.orderId" }, STATUS_ATTRIBUTE: { "S": RESERVED } },
            },
            "ResultPath": "$.reserved",
            "Next": CHARGE_STATE,
        },
        CONFIRM_STATE: { "Type": "Succeed" },
    });
    let written = states.as_object_mut().expect("the states are an object");
    for (index, error_equals) in row.catchers.iter().enumerate() {
        let branch = compensate_state(index);
        let settled = rolled_back_state(index);
        let mut catcher = json!({ "ErrorEquals": error_equals, "Next": branch });
        if let Some(path) = result_path(index) {
            catcher["ResultPath"] = json!(path);
        }
        catchers.push(catcher);
        written.insert(
            branch,
            json!({
                "Type": "Task",
                "Resource": INVOKE_RESOURCE,
                "Parameters": {
                    "FunctionName": compensate,
                    "Payload.$": result_path(index).unwrap_or("$"),
                },
                "Next": settled.clone(),
            }),
        );
        // What the execution PUTS OUT once this branch ran, which is how a row reads
        // which single entry of the list was applied.
        written.insert(
            settled,
            json!({
                "Type": "Pass",
                "Result": { "compensatedBy": compensate_state(index) },
                "End": true,
            }),
        );
    }
    written.insert(
        CHARGE_STATE.to_string(),
        json!({
            "Type": "Task",
            "Resource": INVOKE_RESOURCE,
            "Parameters": { "FunctionName": charge, "Payload.$": "$" },
            "Catch": catchers,
            "Next": CONFIRM_STATE,
        }),
    );
    json!({ "StartAt": RESERVE_STATE, "States": states })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_first_catcher_naming_the_error_is_the_one_applied() {
    let chain = Chain::with_dynamodb();
    given_table(&chain, "the saga's table").await;
    let charge = chain
        .given_function_of(
            "charge-the-order",
            CHARGE_HANDLER,
            Some(FUNCTION_TIMEOUT_SECONDS),
        )
        .await;
    let compensate = chain
        .given_function_of(
            "record-the-compensation",
            &compensate_handler(),
            Some(FUNCTION_TIMEOUT_SECONDS),
        )
        .await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let order = format!("order-{index}");
        let described = chain
            .ran(
                case,
                &format!("saga-catchers-{index}"),
                &definition(&row, &charge, &compensate),
                &json!({ "orderId": order, "raise": row.raises }),
            )
            .await;
        let status = described.get("status").and_then(Value::as_str);
        // Whatever the list decided, the reserve step ran before the charge was ever
        // attempted, so its item stands for every row.
        let reserved = item(&chain, case, &order).await;
        assert_eq!(
            reserved,
            Some(json!({
                KEY: { "S": order },
                STATUS_ATTRIBUTE: { "S": RESERVED },
            })),
            "case {case:?}: expected the reserved order, got {reserved:?}"
        );
        let recorded = item(&chain, case, &record_key(row.raises)).await;

        match row.outcome {
            Outcome::Caught { entry } => {
                // What a client reads off the execution: the branch the applied entry
                // names ran to its own end, so the execution SUCCEEDED...
                assert_eq!(
                    (
                        status,
                        described.get("error").and_then(Value::as_str),
                        described.get("cause").and_then(Value::as_str),
                    ),
                    (Some("SUCCEEDED"), None, None),
                    "case {case:?}: expected the compensated saga to succeed, got {described}"
                );
                // ...and its output names THAT entry's own branch, rather than another
                // entry's or the charge state's success path.
                let output: Value = document(&string(&described, "output"), case);
                assert_eq!(
                    output,
                    json!({ "compensatedBy": compensate_state(entry) }),
                    "case {case:?}: expected the branch of entry {entry} to have run, got {output}"
                );
                // The compensation really ran, on the error output that very entry placed
                // at its own ResultPath: the record carries the failure's own name and the
                // message the run raised.
                assert_eq!(
                    recorded,
                    Some(json!({
                        KEY: { "S": record_key(row.raises) },
                        ERROR_ATTRIBUTE: { "S": row.raises },
                        MESSAGE_ATTRIBUTE: { "S": raised_message(&order) },
                    })),
                    "case {case:?}: expected the error output at {:?} to reach the branch of \
                     entry {entry}, got {recorded:?}",
                    result_path(entry).unwrap_or("$")
                );
            }
            Outcome::Uncaught => {
                // No entry names this error, so none of them is applied: the execution
                // fails under the error document's own class and carries that document as
                // its cause.
                assert_eq!(
                    (status, described.get("error").and_then(Value::as_str)),
                    (Some("FAILED"), Some(row.raises)),
                    "case {case:?}: expected the failure to stand under {:?}, got {described}",
                    row.raises
                );
                let raised = document(&string(&described, "cause"), case);
                assert_eq!(
                    (
                        raised.get("errorType").and_then(Value::as_str),
                        raised.get("errorMessage").and_then(Value::as_str),
                    ),
                    (Some(row.raises), Some(raised_message(&order).as_str())),
                    "case {case:?}: expected the run's own error document, got {raised}"
                );
                assert_eq!(
                    recorded, None,
                    "case {case:?}: expected no compensation at all, got {recorded:?}"
                );
            }
            Outcome::Charged => {
                // The charge answered, so the list is never evaluated: the success path is
                // the one taken and nothing compensated anything.
                assert_eq!(
                    status,
                    Some("SUCCEEDED"),
                    "case {case:?}: expected the execution to succeed, got {described}"
                );
                assert_eq!(
                    recorded, None,
                    "case {case:?}: expected no compensation at all, got {recorded:?}"
                );
            }
        }
    }
}
