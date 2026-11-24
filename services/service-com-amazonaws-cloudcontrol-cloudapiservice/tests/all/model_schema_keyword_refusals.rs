// SPDX-License-Identifier: CC0-1.0

//! Front-door MODEL VALIDATION of the three registry-schema keywords that judge a resource
//! model's SHAPE rather than one value: `required`, `additionalProperties` and `type`.
//!
//! The rule under test: the resource model a request carries — a create's `DesiredState`, the
//! model an update's patch leaves behind — is validated against the resource type's
//! CloudFormation registry schema before any handler is asked, so a model that is not a model
//! of the type is a fault in the CALL. Cloud Control API refuses it synchronously — HTTP 400
//! with the error code `ValidationException` and the model validator's own diagnostic — and
//! mints no resource operation request for it. A property the schema declares `required` that
//! the model leaves out is named as the key the validator looked for, a property the schema
//! does not declare is named as the key it will not permit, and a value whose JSON type is
//! not the declared one is named at its pointer with both type names.
//!
//! `InvalidRequestException` and the `InvalidRequest` handler error code stay reserved for
//! what the type's HANDLER refuses, which a caller discovers by polling the request the call
//! accepted.
//!
//! Each suite is a table whose rows differ along what the rule generalises over: which
//! keyword the schema states, which resource type and property states it, which JSON type the
//! model carried where, which plane submits the model, and the ADMITTED side of each keyword,
//! which the front door refuses nothing for.

use serde_json::{json, Value};

use super::emulator::{code, message, progress, text, Emulator, STATUS_FAILED, STATUS_SUCCESS};

/// The parameter type, whose schema declares two properties `required` and types the rest as
/// text.
const PARAMETER: &str = "AWS::SSM::Parameter";

/// The metric alarm type, whose schema declares two other properties `required`, a number for
/// the threshold and a flag for whether its actions run.
const ALARM: &str = "AWS::CloudWatch::Alarm";

/// The log group type, whose schema declares no required property at all.
const LOG_GROUP: &str = "AWS::Logs::LogGroup";

/// The log stream type, whose schema requires the group the stream is in.
const LOG_STREAM: &str = "AWS::Logs::LogStream";

/// The state machine type, whose schema requires the role the workflow runs as.
const STATE_MACHINE: &str = "AWS::StepFunctions::StateMachine";

/// The error code a front-door refusal carries, which is NOT the modelled
/// `InvalidRequestException` a handler verdict carries.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The location a failure about a property of the type itself is reported at: the root of the
/// document that was validated.
const ROOT: &str = "#";

/// The diagnostic the model validator states a property the model must carry and does not
/// with.
fn required_failure(property: &str) -> String {
    format!("Model validation failed ({ROOT}: required key [{property}] not found)")
}

/// The diagnostic it states a property the type's schema does not declare with.
fn extraneous_failure(property: &str) -> String {
    format!("Model validation failed ({ROOT}: extraneous key [{property}] is not permitted)")
}

/// The diagnostic it states a value whose JSON type is not the declared one with.
fn type_failure(property: &str, declared: &str, found: &str) -> String {
    format!(
        "Model validation failed ({ROOT}/{property}: expected type: {declared}, found: {found})"
    )
}

/// Check that `answer` is the front door's refusal of `case` carrying `expected`.
fn refused(case: &str, answer: &(u16, Value), expected: &str) {
    let (status, body) = answer;
    assert_eq!(
        *status, 400,
        "case {case:?}: expected HTTP 400, got {status} with body {body:?}"
    );
    assert!(
        code(body).contains(VALIDATION_EXCEPTION),
        "case {case:?}: expected {VALIDATION_EXCEPTION}, got {:?}",
        code(body)
    );
    assert_eq!(
        message(body),
        expected,
        "case {case:?}: expected the model validator's diagnostic {expected:?}, got {:?}",
        message(body)
    );
}

#[tokio::test]
async fn a_model_the_type_schema_does_not_admit_is_refused_by_the_call_itself() {
    let emulator = Emulator::new();
    // (case, type, desired state, the diagnostic the front door states)
    let rows: Vec<(String, &str, Value, String)> = vec![
        (
            "a parameter carrying no value, one of the two keys its schema requires"
                .to_string(),
            PARAMETER,
            json!({ "Name": "/refused/valueless", "Type": "String" }),
            required_failure("Value"),
        ),
        (
            "a parameter carrying no type, the other required key".to_string(),
            PARAMETER,
            json!({ "Name": "/refused/typeless", "Value": "v" }),
            required_failure("Type"),
        ),
        (
            "an alarm comparing nothing, a required key of another type".to_string(),
            ALARM,
            json!({ "AlarmName": "refused-operatorless", "EvaluationPeriods": 1 }),
            required_failure("ComparisonOperator"),
        ),
        (
            "a log stream naming no group to be in".to_string(),
            LOG_STREAM,
            json!({ "LogStreamName": "refused-orphan" }),
            required_failure("LogGroupName"),
        ),
        (
            "a state machine naming no role to run as".to_string(),
            STATE_MACHINE,
            json!({ "StateMachineName": "refused-roleless", "DefinitionString": "{}" }),
            required_failure("RoleArn"),
        ),
        (
            "a parameter carrying a property its schema does not declare".to_string(),
            PARAMETER,
            json!({
                "Name": "/refused/stranger",
                "Type": "String",
                "Value": "v",
                "NotAProperty": "x",
            }),
            extraneous_failure("NotAProperty"),
        ),
        (
            "a log group carrying one, which the same keyword refuses".to_string(),
            LOG_GROUP,
            json!({ "LogGroupName": "refused-stranger", "Flavour": "vanilla" }),
            extraneous_failure("Flavour"),
        ),
        (
            "an alarm carrying a property spelled in the wrong case, which names no \
             property of the type"
                .to_string(),
            ALARM,
            json!({
                "AlarmName": "refused-miscased",
                "ComparisonOperator": "GreaterThanThreshold",
                "EvaluationPeriods": 1,
                "threshold": 1,
            }),
            extraneous_failure("threshold"),
        ),
        (
            "a parameter value stated as a whole number where the schema declares text"
                .to_string(),
            PARAMETER,
            json!({ "Name": "/refused/numeric", "Type": "String", "Value": 12_345 }),
            type_failure("Value", "String", "Integer"),
        ),
        (
            "a parameter value stated as a fraction".to_string(),
            PARAMETER,
            json!({ "Name": "/refused/fraction", "Type": "String", "Value": 1.5 }),
            type_failure("Value", "String", "Number"),
        ),
        (
            "a parameter value stated as a flag".to_string(),
            PARAMETER,
            json!({ "Name": "/refused/flag", "Type": "String", "Value": true }),
            type_failure("Value", "String", "Boolean"),
        ),
        (
            "an alarm threshold stated as text where the schema declares a number"
                .to_string(),
            ALARM,
            json!({
                "AlarmName": "refused-threshold",
                "ComparisonOperator": "GreaterThanThreshold",
                "EvaluationPeriods": 1,
                "Threshold": "not-a-number",
            }),
            type_failure("Threshold", "Number", "String"),
        ),
        (
            "an alarm enabling its actions with text where the schema declares a flag"
                .to_string(),
            ALARM,
            json!({
                "AlarmName": "refused-enabled",
                "ComparisonOperator": "GreaterThanThreshold",
                "EvaluationPeriods": 1,
                "ActionsEnabled": "true",
            }),
            type_failure("ActionsEnabled", "Boolean", "String"),
        ),
        (
            "an alarm evaluating over a fraction of a period, where the schema declares a \
             whole number"
                .to_string(),
            ALARM,
            json!({
                "AlarmName": "refused-fractional",
                "ComparisonOperator": "GreaterThanThreshold",
                "EvaluationPeriods": 1.5,
            }),
            type_failure("EvaluationPeriods", "Integer", "Number"),
        ),
    ];

    for (case, type_name, desired, expected) in &rows {
        let answer = emulator.create(type_name, desired, None).await;
        refused(case, &answer, expected);
    }

    // And no resource operation request was minted for any of them: a refusal reached before
    // a handler runs leaves nothing for a caller to poll.
    let listed = emulator.cc_ok("ListResourceRequests", json!({})).await;
    let summaries = listed
        .get("ResourceRequestStatusSummaries")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    assert_eq!(
        summaries, 0,
        "a refusal reached before any handler must mint no request, got {listed:?}"
    );
}

#[tokio::test]
async fn a_property_the_schema_declares_that_no_handler_applies_is_not_an_undeclared_one() {
    // The other side of the `additionalProperties` rule, and the reason the declared set is
    // the type PROVIDER's rather than this stage's own list: a property the schema DOES
    // declare — one this emulator's handler for the type does not apply — is not a property
    // the type lacks. The call is accepted, and the handler states what it cannot apply on
    // the request's own terminal event, which is a different fault a caller fixes differently.
    //
    // One row per type the front door judges the shape of, each naming a property of its
    // schema that no handler here applies.
    let rows: Vec<(&str, Emulator, &str, Value, &str)> = vec![
        (
            "the metric queries an alarm over metric math watches",
            Emulator::with_cloudwatch(),
            ALARM,
            json!({
                "AlarmName": "declared-metrics",
                "ComparisonOperator": "GreaterThanThreshold",
                "EvaluationPeriods": 1,
                "Metrics": [{ "Id": "m1" }],
            }),
            "Metrics",
        ),
        (
            "the data protection policy a log group carries",
            Emulator::with_cloudwatch_logs(),
            LOG_GROUP,
            json!({ "LogGroupName": "declared-document", "DataProtectionPolicy": {} }),
            "DataProtectionPolicy",
        ),
        (
            "the policies a parameter carries",
            Emulator::with_ssm(),
            PARAMETER,
            json!({
                "Name": "/declared/policed",
                "Type": "String",
                "Value": "v",
                "Policies": "[{\"Type\":\"Expiration\",\"Version\":\"1.0\"}]",
            }),
            "Policies",
        ),
        (
            "the tracing configuration a state machine carries",
            Emulator::with_step_functions(),
            STATE_MACHINE,
            json!({
                "StateMachineName": "declared-traced",
                "RoleArn": "arn:aws:iam::123456789012:role/states",
                "DefinitionString": "{\"StartAt\":\"Done\",\"States\":{\"Done\":{\"Type\":\"Succeed\"}}}",
                "TracingConfiguration": { "Enabled": true },
            }),
            "TracingConfiguration",
        ),
    ];

    for (case, emulator, type_name, desired, property) in &rows {
        let (status, body) = emulator.create(type_name, desired, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a property the schema declares must not be refused by the front \
             door, got {status} with body {body:?}"
        );
        let settled = emulator
            .terminal(&text(&progress(&body), "RequestToken"))
            .await;
        assert_eq!(
            text(&settled, "OperationStatus"),
            STATUS_FAILED,
            "case {case:?}: a property no handler applies must not be applied, got {settled:?}"
        );
        assert_eq!(
            text(&settled, "ErrorCode"),
            "InvalidRequest",
            "case {case:?}: expected the handler's own verdict, got {settled:?}"
        );
        assert_eq!(
            text(&settled, "StatusMessage"),
            format!("{property} is a property of {type_name} this emulator's handler does not apply"),
            "case {case:?}: expected the handler to name what it does not apply, got {settled:?}"
        );
    }
}

#[tokio::test]
async fn a_model_the_type_schema_admits_is_accepted_and_provisioned() {
    let emulator = Emulator::with_ssm();
    // The admitted side of each keyword the refusals above state: every required key carried,
    // every other declared property of the type carried too — including the two the schema
    // declares write-only, which no model this API reports carries back — and each value of
    // the JSON type the schema declares for it.
    let rows: Vec<(&str, Value)> = vec![
        (
            "nothing but the keys the schema requires, and the name it is addressed by",
            json!({ "Name": "/admitted/minimal", "Type": "String", "Value": "v" }),
        ),
        (
            "every property the schema declares, each of its declared type",
            json!({
                "Name": "/admitted/complete",
                "Type": "StringList",
                "Value": "a,b",
                "Description": "the admitted side",
                "Tier": "Standard",
                "DataType": "text",
                "AllowedPattern": "^[ab]$",
                "Tags": { "team": "orders" },
            }),
        ),
    ];

    // Started together, then waited for: the creates are independent, so one span covers
    // them all.
    let mut accepted = Vec::new();
    for (case, desired) in &rows {
        let (status, body) = emulator.create(PARAMETER, desired, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a model the schema admits must be accepted, got {status} with \
             body {body:?}"
        );
        accepted.push(text(&progress(&body), "RequestToken"));
    }
    for ((case, desired), request_token) in rows.iter().zip(&accepted) {
        let settled = emulator.terminal(request_token).await;
        assert_eq!(
            text(&settled, "OperationStatus"),
            STATUS_SUCCESS,
            "case {case:?}: expected the create to succeed, got {settled:?}"
        );
        // And the resource reads back, so the accepted model was really provisioned.
        let identifier = text(desired, "Name");
        let properties = emulator.properties(PARAMETER, &identifier).await;
        assert_eq!(
            properties.get("Value"),
            desired.get("Value"),
            "case {case:?}: expected the value the create carried, got {properties:?}"
        );
    }
}

#[tokio::test]
async fn the_model_a_patch_leaves_behind_is_judged_by_the_same_keywords() {
    let emulator = Emulator::with_ssm();
    let name = "/patched/parameter";
    emulator
        .provision(
            PARAMETER,
            &json!({ "Name": name, "Type": "String", "Value": "before" }),
        )
        .await;

    // (case, patch, the diagnostic the front door states)
    //
    // The same three keywords over the model the patch leaves behind: a required key the
    // patch removes, a property the schema does not declare that it adds, and a declared
    // value it replaces with another JSON type.
    let rows: Vec<(&str, Value, String)> = vec![
        (
            "a patch removing a required key",
            json!([{ "op": "remove", "path": "/Value" }]),
            required_failure("Value"),
        ),
        (
            "a patch adding a property the schema does not declare",
            json!([{ "op": "add", "path": "/NotAProperty", "value": "x" }]),
            extraneous_failure("NotAProperty"),
        ),
        (
            "a patch replacing a declared value with another JSON type",
            json!([{ "op": "replace", "path": "/Value", "value": 12_345 }]),
            type_failure("Value", "String", "Integer"),
        ),
    ];

    for (case, patch, expected) in &rows {
        let answer = emulator.update(PARAMETER, name, patch, None).await;
        refused(case, &answer, expected);
        // The refused patch changed nothing: the resource still carries the model it had.
        let properties = emulator.properties(PARAMETER, name).await;
        assert_eq!(
            properties.get("Value").and_then(Value::as_str),
            Some("before"),
            "case {case:?}: a refused patch must leave the resource as it was, got \
             {properties:?}"
        );
    }

    // The control: a patch whose resulting model the schema admits is carried out.
    let event = emulator
        .update_and_settle(
            PARAMETER,
            name,
            &json!([{ "op": "replace", "path": "/Value", "value": "after" }]),
        )
        .await;
    assert_eq!(
        text(&event, "OperationStatus"),
        STATUS_SUCCESS,
        "a patch the schema admits must be carried out, got {event:?}"
    );
    let properties = emulator.properties(PARAMETER, name).await;
    assert_eq!(
        properties.get("Value").and_then(Value::as_str),
        Some("after"),
        "expected the value the patch set, got {properties:?}"
    );
}
