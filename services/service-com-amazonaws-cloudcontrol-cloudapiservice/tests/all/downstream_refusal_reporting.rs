// SPDX-License-Identifier: CC0-1.0

//! How a request reports a refusal the resource type's OWNING service raised.
//!
//! The rule under test: a resource type's handler does not relay what the downstream service
//! said. An error that service raised which the handler maps to no `HandlerErrorCode` of its
//! own terminates the request `FAILED` with `ErrorCode` `GeneralServiceException` — "the
//! downstream service generated an error that doesn't map to any other handler error code"
//! (`smithy_query(query="doc",shape="GeneralServiceException")`) — and a `StatusMessage` of
//! that class's fixed text around its one variable part, the owning service's API operation
//! the handler was calling: `Error occurred during operation '<Operation>'.`
//!
//! So a client learns WHICH downstream call did not happen, whichever rule of that service
//! refused it, and nothing of that service's own wording is passed off as Cloud Control
//! API's. A refusal the handler itself judges — a property of the type's schema this handler
//! does not apply — is not a downstream error and keeps its own diagnostic under its own
//! code.
//!
//! The rows differ along what the rule generalises over: which owning service answers for the
//! type, which rule of that service the model breaks, and which of the three resource
//! operations the handler was carrying out — each of which calls a different API of that
//! service.

use serde_json::{json, Value};

use super::emulator::{progress, text, Emulator, STATUS_FAILED, STATUS_SUCCESS};

/// The handler code for an error the OWNING service raised that the type's handler maps to no
/// verdict of its own.
const DOWNSTREAM: &str = "GeneralServiceException";

/// The handler code for a fault the handler itself judges in the model against the type's
/// schema.
const MODEL_FAULT: &str = "InvalidRequest";

/// A type whose resources live in the AWS Systems Manager service's own store.
const PARAMETER: &str = "AWS::SSM::Parameter";

/// A type whose resources live in the Amazon CloudWatch Logs service's own store.
const LOG_GROUP: &str = "AWS::Logs::LogGroup";

/// The sentence a downstream refusal states for `operation`, which is the OWNING service's
/// API operation the type's handler was calling.
fn downstream_failure(operation: &str) -> String {
    format!("Error occurred during operation '{operation}'.")
}

#[tokio::test]
async fn a_downstream_refusal_names_the_call_the_handler_was_making() {
    let emulator = Emulator::with_ssm();
    // (case, desired state, the code and the sentence the terminal event carries)
    //
    // Every row is a rule only AWS Systems Manager holds the value to — the hierarchy it
    // keeps for itself, the leading separator a hierarchical name takes, the expression a
    // write states for its own value — so each is that service's own error on the one call a
    // create of this type makes.
    let rows: Vec<(&str, Value, &str, String)> = vec![
        (
            "a name in the hierarchy the owning service keeps for itself",
            json!({ "Name": "/aws/reserved", "Type": "String", "Value": "v" }),
            DOWNSTREAM,
            downstream_failure("PutParameter"),
        ),
        (
            "a hierarchical name with no leading separator",
            json!({ "Name": "team/service", "Type": "String", "Value": "v" }),
            DOWNSTREAM,
            downstream_failure("PutParameter"),
        ),
        (
            "a value the expression the write itself states does not admit",
            json!({
                "Name": "/refused/pattern",
                "Type": "String",
                "Value": "not-a-number",
                "AllowedPattern": "^[0-9]+$",
            }),
            DOWNSTREAM,
            downstream_failure("PutParameter"),
        ),
        (
            "an item of a list the same expression does not admit",
            json!({
                "Name": "/refused/list-pattern",
                "Type": "StringList",
                "Value": "1,two",
                "AllowedPattern": "^[0-9]+$",
            }),
            DOWNSTREAM,
            downstream_failure("PutParameter"),
        ),
        (
            "a property the type's schema declares that this handler does not apply",
            json!({
                "Name": "/refused/policed",
                "Type": "String",
                "Value": "v",
                "Policies": "[{\"Type\":\"Expiration\",\"Version\":\"1.0\"}]",
            }),
            MODEL_FAULT,
            format!("Policies is a property of {PARAMETER} this emulator's handler does not apply"),
        ),
    ];

    // Started together, then waited for: the creates are independent, so one span covers them
    // all.
    let mut accepted = Vec::new();
    for (case, desired, _, _) in &rows {
        let (status, body) = emulator.create(PARAMETER, desired, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a schema-valid model must be accepted, got {status} with body \
             {body:?}"
        );
        accepted.push(text(&progress(&body), "RequestToken"));
    }
    for ((case, desired, expected_code, expected_message), request_token) in
        rows.iter().zip(&accepted)
    {
        let settled = emulator.terminal(request_token).await;
        assert_eq!(
            text(&settled, "OperationStatus"),
            STATUS_FAILED,
            "case {case:?}: a model the owning service will not take must not succeed, got \
             {settled:?}"
        );
        assert_eq!(
            text(&settled, "ErrorCode"),
            *expected_code,
            "case {case:?}: expected the {expected_code} handler code, got {settled:?}"
        );
        assert_eq!(
            text(&settled, "StatusMessage"),
            *expected_message,
            "case {case:?}: expected {expected_message:?}, got {settled:?}"
        );
        // Nothing was written: a refused create leaves no resource behind.
        let (status, body) = emulator.get(PARAMETER, &text(desired, "Name")).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a refused create must leave no resource, got {status} with body \
             {body:?}"
        );
    }
}

#[tokio::test]
async fn each_resource_operation_names_the_call_that_carries_it_out() {
    let emulator = Emulator::with_ssm();
    let name = "/downstream/parameter";
    emulator
        .provision(
            PARAMETER,
            &json!({ "Name": name, "Type": "String", "Value": "123" }),
        )
        .await;

    // An UPDATE of this type makes the same write a create does, so a refusal of one names
    // that call: the patch states an expression the value the parameter holds does not
    // satisfy.
    let (status, accepted) = emulator
        .update(
            PARAMETER,
            name,
            &json!([{ "op": "add", "path": "/AllowedPattern", "value": "^[a-z]+$" }]),
            None,
        )
        .await;
    assert_eq!(
        status, 200,
        "the patch must be accepted, got {status} with body {accepted:?}"
    );
    let settled = emulator
        .terminal(&text(&progress(&accepted), "RequestToken"))
        .await;
    assert_eq!(
        (
            text(&settled, "OperationStatus"),
            text(&settled, "ErrorCode"),
            text(&settled, "StatusMessage"),
        ),
        (
            STATUS_FAILED.to_string(),
            DOWNSTREAM.to_string(),
            downstream_failure("PutParameter"),
        ),
        "a patch the owning service refuses must name the write it refused, got {settled:?}"
    );

    // A DELETE of another type makes that type's own removal call, so a refusal of one names
    // THAT call rather than the write: the group carries the protection its owning service
    // refuses a delete under.
    let logs = Emulator::with_cloudwatch_logs();
    let group = "protected-group";
    logs.provision(LOG_GROUP, &json!({ "LogGroupName": group }))
        .await;
    logs.logs_ok(
        "PutLogGroupDeletionProtection",
        json!({ "logGroupIdentifier": group, "deletionProtectionEnabled": true }),
    )
    .await;
    let (status, accepted) = logs.delete(LOG_GROUP, group).await;
    assert_eq!(
        status, 200,
        "the delete must be accepted, got {status} with body {accepted:?}"
    );
    let settled = logs
        .terminal(&text(&progress(&accepted), "RequestToken"))
        .await;
    assert_eq!(
        (
            text(&settled, "OperationStatus"),
            text(&settled, "ErrorCode"),
            text(&settled, "StatusMessage"),
        ),
        (
            STATUS_FAILED.to_string(),
            DOWNSTREAM.to_string(),
            downstream_failure("DeleteLogGroup"),
        ),
        "a delete the owning service refuses must name the removal it refused, got {settled:?}"
    );
}

#[tokio::test]
async fn the_expression_a_write_states_is_applied_rather_than_only_judged() {
    let emulator = Emulator::with_ssm();
    let name = "/applied/pattern";
    // The control for the rule above: the expression is the OWNING service's to hold, so a
    // write that states one and satisfies it succeeds AND leaves that service holding the
    // expression — a value only its own reads report, this type declaring it write-only.
    let accepted = emulator
        .create_ok(
            PARAMETER,
            &json!({
                "Name": name,
                "Type": "String",
                "Value": "42",
                "AllowedPattern": "^[0-9]+$",
            }),
            None,
        )
        .await;
    let settled = emulator.settled(&text(&accepted, "RequestToken")).await;
    assert_eq!(
        text(&settled, "OperationStatus"),
        STATUS_SUCCESS,
        "a value the expression admits must be stored, got {settled:?}"
    );
    let described = emulator
        .ssm_ok(
            "DescribeParameters",
            json!({ "ParameterFilters": [{ "Key": "Name", "Values": [name] }] }),
        )
        .await;
    let reported = described["Parameters"]
        .as_array()
        .and_then(|parameters| parameters.first())
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(
        reported["AllowedPattern"].as_str(),
        Some("^[0-9]+$"),
        "the owning service must hold the expression the write stated, got {described:?}"
    );

    // And it is in force afterwards: that service's own write of a value it does not admit is
    // refused by the pattern the create left behind.
    let (status, body) = emulator
        .ssm(
            "PutParameter",
            json!({ "Name": name, "Value": "not-a-number", "Overwrite": true }),
        )
        .await;
    assert_eq!(
        status, 400,
        "the stored expression must refuse a value it does not admit, got {status} with body \
         {body:?}"
    );
}
