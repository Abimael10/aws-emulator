// SPDX-License-Identifier: CC0-1.0

//! Front-door MODEL VALIDATION: the resource model a request carries is validated against
//! the resource type's CloudFormation registry schema before any handler is asked.
//!
//! The rule under test: a property whose value is outside a constraint the type's registry
//! schema declares for it is a fault in the CALL. Cloud Control API refuses the call
//! synchronously — HTTP 400 with the error code `ValidationException` and the model
//! validator's own diagnostic, which names the offending property's JSON pointer in the
//! validated document and the schema keyword the value failed — and mints no resource
//! operation request for it. `InvalidRequestException` and the `InvalidRequest` handler
//! error code stay reserved for what the type's HANDLER refuses, which a caller discovers
//! by polling the request the call accepted.
//!
//! Each suite is a table whose rows differ along what the rule generalises over: which
//! keyword the schema states (a pattern, a published set, a declared JSON type, a length
//! bound, a smallest value), which resource type and property states it, which plane
//! submits the model (a create's `DesiredState`, the model an update's patch leaves
//! behind), and each keyword's ADMITTED side, which the front door refuses nothing for.

use serde_json::{json, Value};

use super::emulator::{code, message, progress, text, Emulator, STATUS_SUCCESS};

/// The log group type, whose schema states a character class and a length for the name and
/// publishes value sets for the retention and the class.
const LOG_GROUP: &str = "AWS::Logs::LogGroup";

/// The log stream type, whose schema states a character class of its own for the stream's
/// name.
const LOG_STREAM: &str = "AWS::Logs::LogStream";

/// The parameter type, whose schema publishes the types and the tiers a parameter may be of.
const PARAMETER: &str = "AWS::SSM::Parameter";

/// The metric alarm type, whose schema declares a number for the threshold and a smallest
/// value for the counts of periods.
const ALARM: &str = "AWS::CloudWatch::Alarm";

/// The state machine type, whose schema publishes the workflow types and bounds the name.
const STATE_MACHINE: &str = "AWS::StepFunctions::StateMachine";

/// The error code a front-door refusal carries, which is NOT the modelled
/// `InvalidRequestException` a handler verdict carries.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The location a failure about a property of the type itself is reported at: the root of
/// the document that was validated.
const ROOT: &str = "#";

/// The diagnostic the model validator states a value outside a declared constraint with.
fn keyword_failure(pointer: &str, keyword: &str) -> String {
    format!(
        "Model validation failed ({ROOT}/{pointer}: failed validation constraint for keyword \
         [{keyword}])"
    )
}

/// The diagnostic it states a value outside a published set with.
fn enum_failure(pointer: &str) -> String {
    format!(
        "Model validation failed ({ROOT}/{pointer}: {ROOT}: only 1 subschema matches out of \
         2\n{ROOT}/{pointer}: failed validation constraint for keyword [enum])"
    )
}

/// The diagnostic it states a value whose JSON type is not the declared one with.
fn type_failure(pointer: &str, declared: &str, found: &str) -> String {
    format!("Model validation failed ({ROOT}/{pointer}: expected type: {declared}, found: {found})")
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
async fn a_value_outside_a_schema_constraint_is_refused_by_the_call_itself() {
    let emulator = Emulator::new();
    // (case, type, desired state, the diagnostic the front door states)
    let rows: Vec<(String, &str, Value, String)> = vec![
        (
            "a log group name carrying a colon the name's class excludes".to_string(),
            LOG_GROUP,
            json!({ "LogGroupName": "orders:colon-not-allowed", "RetentionInDays": 1 }),
            keyword_failure("LogGroupName", "pattern"),
        ),
        (
            "a log group name carrying a space, the same class breached elsewhere".to_string(),
            LOG_GROUP,
            json!({ "LogGroupName": "refused name" }),
            keyword_failure("LogGroupName", "pattern"),
        ),
        (
            "a log group name of nothing at all".to_string(),
            LOG_GROUP,
            json!({ "LogGroupName": "" }),
            keyword_failure("LogGroupName", "minLength"),
        ),
        (
            "a log group name one character past the most the schema admits".to_string(),
            LOG_GROUP,
            json!({ "LogGroupName": "g".repeat(513) }),
            keyword_failure("LogGroupName", "maxLength"),
        ),
        (
            "a retention outside the set the schema publishes".to_string(),
            LOG_GROUP,
            json!({ "LogGroupName": "refused-retention", "RetentionInDays": 2 }),
            enum_failure("RetentionInDays"),
        ),
        (
            "a retention that is not the whole number the schema declares".to_string(),
            LOG_GROUP,
            json!({ "LogGroupName": "refused-retention-type", "RetentionInDays": 1.5 }),
            type_failure("RetentionInDays", "Integer", "Number"),
        ),
        (
            "a log group class outside the set the schema publishes".to_string(),
            LOG_GROUP,
            json!({ "LogGroupName": "refused-class", "LogGroupClass": "PLATINUM" }),
            enum_failure("LogGroupClass"),
        ),
        (
            "a log stream name carrying a colon its own class excludes".to_string(),
            LOG_STREAM,
            json!({ "LogGroupName": "orders", "LogStreamName": "host:1" }),
            keyword_failure("LogStreamName", "pattern"),
        ),
        (
            "a log stream in a group whose name breaks the group's class".to_string(),
            LOG_STREAM,
            json!({ "LogGroupName": "orders:1", "LogStreamName": "host-1" }),
            keyword_failure("LogGroupName", "pattern"),
        ),
        (
            "a parameter type the resource type does not publish".to_string(),
            PARAMETER,
            json!({ "Name": "/refused/secure", "Type": "SecureString", "Value": "v" }),
            enum_failure("Type"),
        ),
        (
            "a parameter tier outside the set the owning service publishes".to_string(),
            PARAMETER,
            json!({ "Name": "/refused/tier", "Type": "String", "Value": "v", "Tier": "Platinum" }),
            enum_failure("Tier"),
        ),
        (
            "a threshold stated as text where the schema declares a number".to_string(),
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
            "no period at all to evaluate the alarm over".to_string(),
            ALARM,
            json!({
                "AlarmName": "refused-periods",
                "ComparisonOperator": "GreaterThanThreshold",
                "EvaluationPeriods": 0,
            }),
            keyword_failure("EvaluationPeriods", "minimum"),
        ),
        (
            "a workflow type outside the set the owning service publishes".to_string(),
            STATE_MACHINE,
            json!({
                "StateMachineName": "refused-workflow",
                "StateMachineType": "BATCH",
                "RoleArn": "arn:aws:iam::123456789012:role/states",
                "DefinitionString": "{}",
            }),
            enum_failure("StateMachineType"),
        ),
        (
            "a state machine name past the most the schema admits".to_string(),
            STATE_MACHINE,
            json!({
                "StateMachineName": "s".repeat(81),
                "RoleArn": "arn:aws:iam::123456789012:role/states",
                "DefinitionString": "{}",
            }),
            keyword_failure("StateMachineName", "maxLength"),
        ),
    ];

    for (case, type_name, desired, expected) in &rows {
        let answer = emulator.create(type_name, desired, None).await;
        refused(case, &answer, expected);
    }

    // And no resource operation request was minted for any of them: a refusal reached
    // before a handler runs leaves nothing for a caller to poll.
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
async fn a_value_the_schema_admits_is_accepted_and_provisioned() {
    let emulator = Emulator::with_cloudwatch_logs();
    // The admitted side of every keyword the refusals above state for this type: the
    // punctuation the name's class carries, the shortest and the longest name the length
    // bounds admit, both published bounds of the retention catalogue and a class the set
    // publishes beside the default.
    let rows: Vec<(String, Value)> = vec![
        (
            "a name of the punctuation the class admits".to_string(),
            json!({ "LogGroupName": "/orders.v2-1_a/#b" }),
        ),
        (
            "the shortest name the schema admits".to_string(),
            json!({ "LogGroupName": "g" }),
        ),
        (
            "the longest name the schema admits".to_string(),
            json!({ "LogGroupName": "g".repeat(512) }),
        ),
        (
            "the smallest retention the catalogue publishes".to_string(),
            json!({ "LogGroupName": "retained-briefly", "RetentionInDays": 1 }),
        ),
        (
            "the largest retention the catalogue publishes".to_string(),
            json!({ "LogGroupName": "retained-longest", "RetentionInDays": 3653 }),
        ),
        (
            "a class the published set carries beside the default".to_string(),
            json!({ "LogGroupName": "infrequent", "LogGroupClass": "INFREQUENT_ACCESS" }),
        ),
    ];

    // Started together, then waited for: the creates are independent, so one span covers
    // them all.
    let mut accepted = Vec::new();
    for (case, desired) in &rows {
        let (status, body) = emulator.create(LOG_GROUP, desired, None).await;
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
        let identifier = text(desired, "LogGroupName");
        let properties = emulator.properties(LOG_GROUP, &identifier).await;
        assert_eq!(
            properties.get("LogGroupName").and_then(Value::as_str),
            Some(identifier.as_str()),
            "case {case:?}: expected the group to read back, got {properties:?}"
        );
    }
}

#[tokio::test]
async fn the_model_a_patch_leaves_behind_is_validated_by_the_same_front_door() {
    let emulator = Emulator::with_cloudwatch_logs();
    let name = "patched-group";
    emulator
        .provision(LOG_GROUP, &json!({ "LogGroupName": name, "RetentionInDays": 7 }))
        .await;

    // (case, patch, the diagnostic the front door states)
    let rows: Vec<(String, Value, String)> = vec![
        (
            "a retention outside the set the schema publishes".to_string(),
            json!([{ "op": "replace", "path": "/RetentionInDays", "value": 2 }]),
            enum_failure("RetentionInDays"),
        ),
        (
            "a retention that is not the whole number the schema declares".to_string(),
            json!([{ "op": "replace", "path": "/RetentionInDays", "value": 7.5 }]),
            type_failure("RetentionInDays", "Integer", "Number"),
        ),
        (
            "a class outside the set the schema publishes".to_string(),
            json!([{ "op": "add", "path": "/LogGroupClass", "value": "PLATINUM" }]),
            enum_failure("LogGroupClass"),
        ),
    ];

    for (case, patch, expected) in &rows {
        let answer = emulator.update(LOG_GROUP, name, patch, None).await;
        refused(case, &answer, expected);
        // The refused patch changed nothing: the resource still carries the model it had.
        let properties = emulator.properties(LOG_GROUP, name).await;
        assert_eq!(
            properties.get("RetentionInDays").and_then(Value::as_i64),
            Some(7),
            "case {case:?}: a refused patch must leave the resource as it was, got \
             {properties:?}"
        );
    }

    // The control: a patch whose resulting model the schema admits is carried out.
    let event = emulator
        .update_and_settle(
            LOG_GROUP,
            name,
            &json!([{ "op": "replace", "path": "/RetentionInDays", "value": 14 }]),
        )
        .await;
    assert_eq!(
        text(&event, "OperationStatus"),
        STATUS_SUCCESS,
        "a patch the schema admits must be carried out, got {event:?}"
    );
    let properties = emulator.properties(LOG_GROUP, name).await;
    assert_eq!(
        properties.get("RetentionInDays").and_then(Value::as_i64),
        Some(14),
        "expected the retention the patch set, got {properties:?}"
    );
}
