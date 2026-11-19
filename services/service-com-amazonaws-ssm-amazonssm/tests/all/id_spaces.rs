// SPDX-License-Identifier: CC0-1.0

//! The id SPACES of this service: an id names a resource of ONE kind, and resolves in that
//! kind's own API and in no other.
//!
//! Run Command command ids and Automation execution ids are both 36-character hyphenated
//! UUIDs (`Command$CommandId`, `AutomationExecutionId`'s modelled length), and the resources
//! they name are reached through different operations: `GetAutomationExecution` is "no
//! automation execution information for the requested automation execution ID"
//! (`AutomationExecutionNotFoundException`) for a command id, and `CancelCommand` is
//! `InvalidCommandId` for an execution id — each naming the id it could not resolve as that
//! refusal words it. Two kinds never share an id either, so the probe cannot accidentally hit
//! a resource of the other kind.
//!
//! Every case goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body, dispatched by the
//! multi-service router (`OPERATIONS.md`, "Wire protocol"); every id a case probes with was
//! minted by a real call rather than written down here.
//!
//! The suites are tables: the rows differ along which kind minted the id, how many of that
//! kind were minted before it, and which API it is offered to — rather than the single pair
//! of ids a recorded unit happened to cross over.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, error_code, error_message, router};

/// A document Systems Manager publishes that IS an Automation runbook, so a start resolves
/// it without anybody creating one (`state::PUBLISHED_DOCUMENTS`), beside the one parameter
/// it declares without a default — which a start has to supply a value for.
const PUBLISHED_RUNBOOK: &str = "AWS-ConfigureS3BucketVersioning";

/// That runbook's parameter, as a start supplies it.
const RUNBOOK_PARAMETER: &str = "BucketName";

/// A command document Systems Manager publishes, so a send resolves it the same way.
const PUBLISHED_COMMAND: &str = "AWS-UpdateSSMAgent";

/// The sentence `GetAutomationExecution` refuses an id it holds no automation execution for
/// with: FIXED text, with no part of the request in it
/// (`state::automation::EXECUTION_NOT_FOUND_MESSAGE`).
const AUTOMATION_NOT_FOUND: &str = "The input automation execution does not exist.";

/// How a Run Command read words an id it holds no command for: the fixed text around the id
/// the request carried (`state::commands::command_not_found`).
fn command_not_found(command_id: &str) -> String {
    format!("Command id {command_id} does not exist.")
}

/// Send a command whose targets select no node — a command exists either way, and this suite
/// is about its ID — answering the `CommandId` it minted.
async fn send_command(router: &MultiServiceRouter, case: &str) -> String {
    let (status, response) = call(
        router,
        "SendCommand",
        &json!({
            "DocumentName": PUBLISHED_COMMAND,
            "Targets": [{ "Key": "tag:env", "Values": ["absent"] }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the send must succeed, got {status} {response}"
    );
    response["Command"]["CommandId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a send reports a CommandId, got {response}"))
        .to_string()
}

/// Start an automation on a published runbook, answering the `AutomationExecutionId` it
/// minted.
async fn start_automation(router: &MultiServiceRouter, case: &str) -> String {
    let (status, response) = call(
        router,
        "StartAutomationExecution",
        &json!({
            "DocumentName": PUBLISHED_RUNBOOK,
            "Parameters": { RUNBOOK_PARAMETER: ["an-id-space-probe"] },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the start must succeed, got {status} {response}"
    );
    response["AutomationExecutionId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: a start reports an AutomationExecutionId, got {response}")
        })
        .to_string()
}

/// RULE: the ids of two kinds of resource are drawn from disjoint spaces — no id this
/// service mints for one kind is an id it mints for another, however many of each are
/// minted.
#[tokio::test]
async fn no_two_kinds_of_resource_share_an_id() {
    let router = router();

    // Several of each kind, interleaved, so a collision between the Nth id of one kind and
    // the Nth of another cannot hide behind the order they were minted in.
    let mut minted: Vec<(&str, String)> = Vec::new();
    for round in 0..3 {
        let case = format!("round {round}");
        minted.push(("a command", send_command(&router, &case).await));
        minted.push(("an automation execution", start_automation(&router, &case).await));
    }

    for (left_index, (left_kind, left)) in minted.iter().enumerate() {
        for (right_kind, right) in minted.iter().skip(left_index + 1) {
            assert_ne!(
                left, right,
                "case {left_kind} vs {right_kind}: two resources must not share the id \
                 {left:?}"
            );
        }
    }
}

/// RULE: `GetAutomationExecution` resolves ids of ITS OWN kind. An id of another kind — a
/// command id, whatever shape it has in common with an execution id — is refused with the
/// operation's modelled not-found error.
#[tokio::test]
async fn an_automation_read_refuses_an_id_of_another_kind() {
    let router = router();
    let command_id = send_command(&router, "the command whose id is probed").await;
    let execution_id = start_automation(&router, "the automation that must still resolve").await;

    // (case, the id probed, the operation)
    let rows: [(&str, &str, &str); 3] = [
        (
            "a command id read as an automation execution",
            command_id.as_str(),
            "GetAutomationExecution",
        ),
        (
            "a command id whose step executions are read",
            command_id.as_str(),
            "DescribeAutomationStepExecutions",
        ),
        (
            "a command id stopped as an automation execution",
            command_id.as_str(),
            "StopAutomationExecution",
        ),
    ];

    for (case, id, operation) in rows {
        let (status, response) = call(
            &router,
            operation,
            &json!({ "AutomationExecutionId": id }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: {operation} must refuse it, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "AutomationExecutionNotFoundException",
            "case {case:?}: {operation} must answer the modelled not-found error, got \
             {response}"
        );
        let message = error_message(&response);
        assert_eq!(
            message, AUTOMATION_NOT_FOUND,
            "case {case:?}: expected {AUTOMATION_NOT_FOUND:?}, got {message:?}"
        );
    }

    // The automation's OWN id still resolves, so the refusals above are about the id space
    // rather than about an empty store.
    let (status, read) = call(
        &router,
        "GetAutomationExecution",
        &json!({ "AutomationExecutionId": execution_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the automation's own id must resolve, got {status} {read}"
    );
    assert_eq!(
        read["AutomationExecution"]["AutomationExecutionId"].as_str(),
        Some(execution_id.as_str()),
        "expected the execution the start minted, got {read}"
    );
}

/// RULE, symmetrically: a Run Command read resolves ids of its own kind, and an automation
/// execution id is `InvalidCommandId` — worded with the id the request carried.
#[tokio::test]
async fn a_run_command_read_refuses_an_id_of_another_kind() {
    let router = router();
    let execution_id = start_automation(&router, "the automation whose id is probed").await;
    let command_id = send_command(&router, "the command that must still resolve").await;

    // (case, the operation, the body it takes)
    let rows: [(&str, &str, Value); 3] = [
        (
            "an execution id cancelled as a command",
            "CancelCommand",
            json!({ "CommandId": execution_id }),
        ),
        (
            "an execution id listed as a command",
            "ListCommands",
            json!({ "CommandId": execution_id }),
        ),
        (
            "an execution id whose command invocations are listed",
            "ListCommandInvocations",
            json!({ "CommandId": execution_id }),
        ),
    ];

    for (case, operation, body) in rows {
        let expected = command_not_found(&execution_id);
        let (status, response) = call(&router, operation, &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: {operation} must refuse it, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "InvalidCommandId",
            "case {case:?}: {operation} must answer the modelled invalid-id error, got \
             {response}"
        );
        let message = error_message(&response);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }

    // The command's OWN id still resolves.
    let (status, listed) = call(
        &router,
        "ListCommands",
        &json!({ "CommandId": command_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the command's own id must resolve, got {status} {listed}"
    );
    assert_eq!(
        listed["Commands"][0]["CommandId"].as_str(),
        Some(command_id.as_str()),
        "expected the command the send minted, got {listed}"
    );
}
