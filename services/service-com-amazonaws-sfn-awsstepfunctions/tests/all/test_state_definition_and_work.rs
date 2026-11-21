// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what `TestState` reads out of the DEFINITION it is handed, and
//! for the work the state under test actually does — driven over this service's own
//! wire protocol (`awsJson1_0`, `POST /` with
//! `X-Amz-Target: AWSStepFunctions.TestState`) through the production
//! `MultiServiceRouter`, so what is asserted is what a signed AWS SDK request observes.
//!
//! The rules under test:
//!
//! 1. the state under test is evaluated in the query language IT declares, else in the
//!    one the SUBMITTED DEFINITION declares — so a `stateName` naming one state of a
//!    full definition is tested exactly as an execution of that definition would run
//!    it, and a JSONata state's `Output` is EVALUATED wherever the language was set;
//! 2. the definition is PARSED before anything is evaluated: a field whose JSONata
//!    expression the language cannot parse is a defect of the submitted document, so it
//!    is refused with `InvalidDefinition` naming the code, the parser's diagnostic and
//!    the pointer to the field — not reported as a state that ran and failed, and
//!    refused by the operations that STORE a definition for the same reason;
//! 3. the same ASL SCHEMA check the write path applies is applied to the submitted
//!    definition, so a definition the language does not admit is refused before the
//!    `stateName` is even selected and nothing is run for it;
//! 4. a `Task` state's own work is its integration call, and `TestState` MAKES it: the
//!    resource the call acts on exists afterwards. A `mock` stands in for that call
//!    instead, so nothing is called and no such resource exists.
//!
//! The harness (`Emulator`, `error_code`) is the one `state_machine_lifecycle` builds;
//! `tests/all/main.rs` is this binary's crate root, so each is reached through
//! `crate::<suite>`.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::{
    asl::{INVALID_JSONATA_EXPRESSION, SCHEMA_VALIDATION_FAILED},
    INVALID_STATE_MACHINE_DEFINITION,
};

use crate::state_machine_lifecycle::{
    create_machine, error_code, Emulator, ACCOUNT, REGION, ROLE_ARN,
};

/// The envelope every `InvalidDefinition` of this service states its subjects in, as
/// `crate::state::resource_error_message` builds it: the sentence, then the subject
/// quoted.
fn invalid_definition_message(subject: &str) -> String {
    format!("{INVALID_STATE_MACHINE_DEFINITION}: '{subject}'")
}

/// One subject of such a message: the diagnostic code, the detail and the place in the
/// submitted definition the problem was found at.
fn subject(code: &str, detail: &str, location: &str) -> String {
    format!("{code}: {detail} at {location}")
}

// ---------------------------------------------------------------------------
// Rule 1: the query language the state under test is evaluated in
// ---------------------------------------------------------------------------

/// A full definition of two states, written in `language` at its top level, whose
/// second state is a `Succeed` carrying `output` and `state_language` of its own.
///
/// `language` and `state_language` are each written only where they are `Some`, which
/// is what lets one definition carry the query language at the definition's top level,
/// at the state, at both, or at neither.
fn two_state_definition(
    language: Option<&str>,
    state_language: Option<&str>,
    output: Option<&str>,
) -> String {
    let mut last = json!({ "Type": "Succeed" });
    if let Some(declared) = state_language {
        last["QueryLanguage"] = json!(declared);
    }
    if let Some(output) = output {
        last["Output"] = json!(output);
    }
    let mut definition = json!({
        "StartAt": "First",
        "States": { "First": { "Type": "Pass", "Next": "Last" }, "Last": last },
    });
    if let Some(declared) = language {
        definition["QueryLanguage"] = json!(declared);
    }
    definition.to_string()
}

/// One row of the effective-language table.
struct LanguageCase {
    case: &'static str,
    /// The `QueryLanguage` the DEFINITION declares, if any.
    definition_language: Option<&'static str>,
    /// The `QueryLanguage` the state under test declares, if any.
    state_language: Option<&'static str>,
    /// The `Output` that state carries, if any.
    output: Option<&'static str>,
    /// The `input` the state is tested on.
    input: &'static str,
    /// The document the state must put out.
    expected: Value,
}

/// The state a `stateName` names is tested in the query language the definition set for
/// it, so a JSONata `Output` is EVALUATED whichever of the two places the language was
/// declared in.
///
/// Rows vary along WHERE the language is set (the definition's top level, the state
/// itself, both, neither) and along what the expression computes (arithmetic on a
/// member, an object it builds, a string it concatenates, a literal that is no
/// expression at all) — so what is asserted is the rule and not one recorded
/// expression.
#[tokio::test]
async fn the_state_under_test_is_evaluated_in_the_language_its_definition_set() {
    let cases = [
        LanguageCase {
            case: "the definition's own top-level QueryLanguage",
            definition_language: Some("JSONata"),
            state_language: None,
            output: Some("{% $states.input.value * 2 %}"),
            input: r#"{"value":21}"#,
            expected: json!(42),
        },
        LanguageCase {
            case: "the state's own QueryLanguage, the definition declaring none",
            definition_language: None,
            state_language: Some("JSONata"),
            output: Some("{% $states.input.value * 2 %}"),
            input: r#"{"value":21}"#,
            expected: json!(42),
        },
        LanguageCase {
            case: "both places naming the same language",
            definition_language: Some("JSONata"),
            state_language: Some("JSONata"),
            output: Some("{% $states.input.value * 2 %}"),
            input: r#"{"value":21}"#,
            expected: json!(42),
        },
        LanguageCase {
            case: "an expression that builds an object",
            definition_language: Some("JSONata"),
            state_language: None,
            output: Some("{% { 'doubled': $states.input.value * 2, 'seen': $states.input.name } %}"),
            input: r#"{"value":4,"name":"first"}"#,
            expected: json!({ "doubled": 8, "seen": "first" }),
        },
        LanguageCase {
            case: "an expression that concatenates strings",
            definition_language: None,
            state_language: Some("JSONata"),
            output: Some("{% $states.input.name & '-tested' %}"),
            input: r#"{"name":"only"}"#,
            expected: json!("only-tested"),
        },
        LanguageCase {
            case: "an Output that carries no delimiters is the literal it is written as",
            definition_language: Some("JSONata"),
            state_language: None,
            output: Some("a plain string"),
            input: r#"{"value":21}"#,
            expected: json!("a plain string"),
        },
        LanguageCase {
            case: "a JSONata state carrying no Output puts out its own input",
            definition_language: Some("JSONata"),
            state_language: None,
            output: None,
            input: r#"{"value":21}"#,
            expected: json!({ "value": 21 }),
        },
        LanguageCase {
            // The counter-case: neither place names JSONata, so the state is written in
            // the other language and carries no `Output` member at all.
            case: "neither place naming a language leaves the state in JSONPath",
            definition_language: None,
            state_language: None,
            output: None,
            input: r#"{"value":21}"#,
            expected: json!({ "value": 21 }),
        },
    ];

    let emulator = Emulator::new();
    for case in cases {
        let definition =
            two_state_definition(case.definition_language, case.state_language, case.output);
        let answered = emulator
            .ok(
                "TestState",
                &json!({
                    "definition": definition,
                    "stateName": "Last",
                    "input": case.input,
                    "roleArn": ROLE_ARN,
                }),
            )
            .await;
        let label = case.case;
        assert_eq!(
            answered.get("status").and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {label:?}: expected SUCCEEDED for {definition}, got {answered:?}"
        );
        let output = answered
            .get("output")
            .and_then(Value::as_str)
            .map(|text| serde_json::from_str::<Value>(text).expect("the output is JSON"));
        assert_eq!(
            output,
            Some(case.expected.clone()),
            "case {label:?}: expected the output {:?}, got {answered:?}",
            case.expected
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 2: an expression the language cannot parse is a defect of the definition
// ---------------------------------------------------------------------------

/// One row of the unparsable-expression table.
struct UnparsedCase {
    case: &'static str,
    /// The state under test, as the definition declares it.
    state: Value,
    /// Whether the request names the `stateName` (a full definition) or sends the
    /// state alone.
    named: bool,
    /// A further state the full definition declares beside the two it always does,
    /// for a state under test whose own transitions need one to reach.
    reached: Option<(&'static str, Value)>,
    /// The pointer the refusal must locate the problem at.
    location: &'static str,
    /// The parser's own diagnostic for the offending expression.
    diagnostic: &'static str,
}

/// A JSONata expression the language cannot PARSE is refused as a defect of the
/// definition — HTTP 400 `InvalidDefinition`, naming the code, the parser's diagnostic
/// and the pointer to the field — rather than reported as a state that ran and failed.
///
/// Rows vary along the FIELD the expression is written in (a `Succeed`'s `Output`, a
/// `Pass`'s `Assign` member, a `Task`'s nested `Arguments` member, a `Choice` rule's
/// `Condition`) and along what is wrong with the text, and along whether the request
/// named a `stateName` or sent the single state alone.
#[tokio::test]
async fn a_jsonata_expression_that_does_not_parse_is_refused_as_an_invalid_definition() {
    let cases = [
        UnparsedCase {
            case: "a Succeed state's Output, truncated after an operator",
            state: json!({ "Type": "Succeed", "Output": "{% $states.input.value * %}" }),
            named: true,
            reached: None,
            location: "/States/Last/Output",
            diagnostic: "Unexpected end of expression",
        },
        UnparsedCase {
            case: "the same state sent alone, with no stateName",
            state: json!({
                "Type": "Succeed",
                "QueryLanguage": "JSONata",
                "Output": "{% $states.input.value * %}",
            }),
            named: false,
            reached: None,
            location: "/Output",
            diagnostic: "Unexpected end of expression",
        },
        UnparsedCase {
            case: "an Assign member whose expression leaves a string unterminated",
            state: json!({
                "Type": "Succeed",
                "Assign": { "total": "{% 'unterminated %}" },
            }),
            named: true,
            reached: None,
            location: "/States/Last/Assign/total",
            diagnostic: "String literal must be terminated by a matching quote",
        },
        UnparsedCase {
            case: "a nested Arguments member of a Task state",
            state: json!({
                "Type": "Task",
                "Resource": "arn:aws:states:::aws-sdk:sfn:createActivity",
                "Arguments": { "Nested": { "Name": "{% $states.input.name & %}" } },
                "End": true,
            }),
            named: true,
            reached: None,
            location: "/States/Last/Arguments/Nested/Name",
            diagnostic: "Unexpected end of expression",
        },
        UnparsedCase {
            case: "a Choice rule's Condition",
            state: json!({
                "Type": "Choice",
                "Choices": [{ "Condition": "{% $states.input.value > %}", "Next": "Done" }],
                "Default": "Done",
            }),
            named: true,
            reached: Some(("Done", json!({ "Type": "Succeed" }))),
            location: "/States/Last/Choices[0]/Condition",
            diagnostic: "Unexpected end of expression",
        },
    ];

    let emulator = Emulator::new();
    for case in cases {
        let (definition, request_state_name) = match case.named {
            true => {
                let mut states =
                    json!({ "First": { "Type": "Pass", "Next": "Last" }, "Last": case.state });
                if let Some((name, declared)) = case.reached {
                    states[name] = declared;
                }
                (
                    json!({
                        "QueryLanguage": "JSONata",
                        "StartAt": "First",
                        "States": states,
                    })
                    .to_string(),
                    Some("Last"),
                )
            }
            false => (case.state.to_string(), None),
        };
        let mut request = json!({
            "definition": definition,
            "input": r#"{"value":21,"name":"only"}"#,
            "roleArn": ROLE_ARN,
        });
        if let Some(named) = request_state_name {
            request["stateName"] = json!(named);
        }
        let label = case.case;
        let (status, body) = emulator.call("TestState", &request).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidDefinition"),
            "case {label:?}: expected a 400 InvalidDefinition for {definition}, got {status} \
             {body:?}"
        );
        let stated = subject(INVALID_JSONATA_EXPRESSION, case.diagnostic, case.location);
        // A definition sent WITH a `stateName` is a state machine definition and is
        // refused in the envelope every state machine definition of this service is
        // refused in; one sent alone "accepts the definition of a single state", which
        // is refused in its own terms — the subject, with no state machine named around
        // it.
        let expected = match case.named {
            true => invalid_definition_message(&stated),
            false => stated,
        };
        assert_eq!(
            body.get("message").and_then(Value::as_str),
            Some(expected.as_str()),
            "case {label:?}: expected the message {expected:?}, got {body:?}"
        );
    }
}

/// The parse is part of the definition's own VALIDATION, so the write path refuses the
/// same definition with the same subject: a state machine whose JSONata expression the
/// language cannot parse could never run that state, whichever API was handed it.
///
/// Rows span the operations that judge a definition — the two that store one and the one
/// that only reports on it — and assert the one diagnostic each of them reaches.
#[tokio::test]
async fn the_write_path_refuses_the_same_unparsable_expression() {
    let broken = two_state_definition(Some("JSONata"), None, Some("{% $states.input.value * %}"));
    let sound = two_state_definition(Some("JSONata"), None, Some("{% $states.input.value * 2 %}"));
    let stated = subject(
        INVALID_JSONATA_EXPRESSION,
        "Unexpected end of expression",
        "/States/Last/Output",
    );
    let expected = invalid_definition_message(&stated);

    let emulator = Emulator::new();
    // The definition that DOES parse is stored, which is what the refused one is told
    // apart from — and what gives the update below a state machine to be refused for.
    let held = create_machine(&emulator, "parsed-definition", &sound).await;

    // (case, the operation, its request with the unparsable definition)
    let refused: [(&str, &str, Value); 2] = [
        (
            "CreateStateMachine",
            "CreateStateMachine",
            json!({ "name": "broken-definition", "definition": &broken, "roleArn": ROLE_ARN }),
        ),
        (
            "UpdateStateMachine",
            "UpdateStateMachine",
            json!({ "stateMachineArn": &held, "definition": &broken }),
        ),
    ];
    for (case, operation, request) in refused {
        let (status, body) = emulator.call(operation, &request).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidDefinition"),
            "case {case:?}: expected a 400 InvalidDefinition, got {status} {body:?}"
        );
        assert_eq!(
            body.get("message").and_then(Value::as_str),
            Some(expected.as_str()),
            "case {case:?}: expected the message {expected:?}, got {body:?}"
        );
    }

    // And the operation that only REPORTS on a definition reports it as the error
    // diagnostic it is, at the same code and location, rather than refusing the call.
    let reported = emulator
        .ok(
            "ValidateStateMachineDefinition",
            &json!({ "definition": &broken }),
        )
        .await;
    assert_eq!(
        reported.get("result").and_then(Value::as_str),
        Some("FAIL"),
        "a definition carrying an unparsable expression does not validate, got {reported:?}"
    );
    let found = reported
        .get("diagnostics")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("expected a 'diagnostics' array, got {reported:?}"));
    assert!(
        found.iter().any(|diagnostic| {
            diagnostic.get("code").and_then(Value::as_str) == Some(INVALID_JSONATA_EXPRESSION)
                && diagnostic.get("location").and_then(Value::as_str)
                    == Some("/States/Last/Output")
        }),
        "expected a {INVALID_JSONATA_EXPRESSION} diagnostic at /States/Last/Output, got \
         {reported:?}"
    );
    // The sound definition carries no such diagnostic, so the code above is not reported
    // for every JSONata expression.
    let clean = emulator
        .ok(
            "ValidateStateMachineDefinition",
            &json!({ "definition": &sound }),
        )
        .await;
    assert_eq!(
        clean.get("result").and_then(Value::as_str),
        Some("OK"),
        "a definition whose expression parses validates, got {clean:?}"
    );
}

/// The counter-case to the rule above: an expression the language DOES parse is
/// evaluated, whatever it then comes to — so a definition is not refused for an
/// expression that merely fails over the data it reads.
#[tokio::test]
async fn an_expression_that_parses_is_evaluated_rather_than_refused() {
    // (case, the Output expression, the status the state ends in)
    let cases: [(&str, &str, &str); 3] = [
        ("an expression that computes a value", "{% $states.input.value + 1 %}", "SUCCEEDED"),
        // It parses and resolves to NOTHING, which is the state's own query-evaluation
        // failure (`States.QueryEvaluationError`) rather than the definition being
        // invalid: the expression was evaluated, which is what this rule is about.
        ("an expression reading an absent member", "{% $states.input.absent %}", "FAILED"),
        // It parses and THROWS when evaluated, which is the state failing rather than
        // the definition being invalid.
        ("an expression that throws over its data", "{% $states.input.name + 1 %}", "FAILED"),
    ];

    let emulator = Emulator::new();
    for (case, output, expected) in cases {
        let definition = two_state_definition(Some("JSONata"), None, Some(output));
        let answered = emulator
            .ok(
                "TestState",
                &json!({
                    "definition": definition,
                    "stateName": "Last",
                    "input": r#"{"value":1,"name":"only"}"#,
                    "roleArn": ROLE_ARN,
                }),
            )
            .await;
        assert_eq!(
            answered.get("status").and_then(Value::as_str),
            Some(expected),
            "case {case:?}: expected status {expected:?} for {definition}, got {answered:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 3: the ASL schema check runs over the submitted definition
// ---------------------------------------------------------------------------

/// A definition the Amazon States Language does not admit is refused before the state
/// the request named is selected, so nothing is run for it.
///
/// Rows vary along the schema rule broken — two different groups of fields the language
/// declares as ALTERNATIVES carried together, a transition naming no declared state, a
/// field the state's type does not declare — each of which is a defect of the
/// DEFINITION rather than an outcome of running one state of it.
#[tokio::test]
async fn test_state_runs_the_asl_schema_check_over_the_definition_it_was_given() {
    // (case, the states the definition declares, the subject the refusal must state)
    let cases: [(&str, Value, String); 4] = [
        (
            "a state carrying both Next and End",
            json!({
                "First": { "Type": "Pass", "Next": "Last" },
                "Last": { "Type": "Pass", "End": true, "Next": "First" },
            }),
            subject(
                SCHEMA_VALIDATION_FAILED,
                "There should only be one of the following fields: [Next, End]",
                "/States/Last",
            ),
        ),
        (
            "a Wait state declaring two of the four ways it waits",
            json!({
                "First": { "Type": "Pass", "Next": "Last" },
                "Last": {
                    "Type": "Wait",
                    "Seconds": 1,
                    "Timestamp": "2026-01-01T00:00:00Z",
                    "End": true,
                },
            }),
            subject(
                SCHEMA_VALIDATION_FAILED,
                "There should only be one of the following fields: [SecondsPath, Seconds, \
                 TimestampPath, Timestamp]",
                "/States/Last",
            ),
        ),
        (
            "a Choice rule naming no declared state",
            json!({
                "First": { "Type": "Pass", "Next": "Last" },
                "Last": {
                    "Type": "Choice",
                    "Choices": [{ "Variable": "$.k", "StringEquals": "v", "Next": "Nowhere" }],
                    "Default": "Done",
                },
                "Done": { "Type": "Succeed" },
            }),
            subject(
                "MISSING_TRANSITION_TARGET",
                "Missing 'Next' target: Nowhere",
                "/States/Last/Choices[0]/Next",
            ),
        ),
        (
            "a field the state's type does not declare",
            json!({
                "First": { "Type": "Pass", "Next": "Last" },
                "Last": { "Type": "Succeed", "Seconds": 1 },
            }),
            subject(
                SCHEMA_VALIDATION_FAILED,
                "Field 'Seconds' is not supported",
                "/States/Last",
            ),
        ),
    ];

    let emulator = Emulator::new();
    for (case, states, stated) in cases {
        let definition = json!({ "StartAt": "First", "States": states }).to_string();
        let (status, body) = emulator
            .call(
                "TestState",
                &json!({
                    "definition": definition,
                    "stateName": "Last",
                    "input": r#"{"k":"v"}"#,
                    "roleArn": ROLE_ARN,
                }),
            )
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidDefinition"),
            "case {case:?}: expected a 400 InvalidDefinition for {definition}, got {status} \
             {body:?}"
        );
        let expected = invalid_definition_message(&stated);
        assert_eq!(
            body.get("message").and_then(Value::as_str),
            Some(expected.as_str()),
            "case {case:?}: expected the message {expected:?}, got {body:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 4: an unmocked Task state's integration call is MADE
// ---------------------------------------------------------------------------

/// A `Task` state whose work is this service's own `CreateActivity`, so the call the
/// state makes is served by the same emulator the test drives — and what it leaves
/// behind is readable through this API.
fn create_activity_task(resource: &str) -> String {
    json!({
        "Type": "Task",
        "Resource": resource,
        "Parameters": { "Name.$": "$.name" },
        "End": true,
    })
    .to_string()
}

/// Whether this emulator holds an activity of `name`, asked through the API that
/// reports one.
async fn activity_exists(emulator: &Emulator, name: &str) -> bool {
    let answered = emulator.ok("ListActivities", &json!({})).await;
    answered
        .get("activities")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("expected an 'activities' array, got {answered:?}"))
        .iter()
        .any(|activity| activity.get("name").and_then(Value::as_str) == Some(name))
}

/// `TestState` on a `Task` state with no `mock` MAKES the state's integration call: the
/// state SUCCEEDS, its `output` is the call's own answer, and the resource the call
/// created exists afterwards. A `mock` stands in for the call instead, so nothing is
/// called and no such resource exists.
///
/// Rows vary along the integration family the `Resource` names — the AWS SDK one and
/// the optimized one, which are two spellings of the same operation — and along whether
/// a mock was supplied, which is the one thing that decides whether the call happens.
#[tokio::test]
async fn an_unmocked_task_state_makes_its_integration_call() {
    // (case, the Resource, whether a mock stands in, the activity name the input names)
    let rows: [(&str, &str, bool, &str); 4] = [
        (
            "the AWS SDK family, with no mock",
            "arn:aws:states:::aws-sdk:sfn:createActivity",
            false,
            "sdk-unmocked",
        ),
        (
            "the optimized family, with no mock",
            "arn:aws:states:::states:createActivity",
            false,
            "optimized-unmocked",
        ),
        (
            "the AWS SDK family, with a mock standing in",
            "arn:aws:states:::aws-sdk:sfn:createActivity",
            true,
            "sdk-mocked",
        ),
        (
            "the optimized family, with a mock standing in",
            "arn:aws:states:::states:createActivity",
            true,
            "optimized-mocked",
        ),
    ];

    let emulator = Emulator::new();
    for (case, resource, mocked, name) in rows {
        let mut request = json!({
            "definition": create_activity_task(resource),
            "input": json!({ "name": name }).to_string(),
            "roleArn": ROLE_ARN,
        });
        if mocked {
            request["mock"] = json!({
                "result": json!({
                    "ActivityArn": format!("arn:aws:states:{REGION}:{ACCOUNT}:activity:{name}"),
                    "CreationDate": 0,
                })
                .to_string(),
            });
        }
        let answered = emulator.ok("TestState", &request).await;
        assert_eq!(
            answered.get("status").and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {case:?}: expected the state to succeed, got {answered:?}"
        );
        let held = activity_exists(&emulator, name).await;
        assert_eq!(
            held, !mocked,
            "case {case:?}: expected the activity {name:?} to exist={} after {answered:?}",
            !mocked
        );
        // Whichever way the state got its result, what it puts out is that result: the
        // ARN of the activity the call names.
        let output = answered
            .get("output")
            .and_then(Value::as_str)
            .map(|text| serde_json::from_str::<Value>(text).expect("the output is JSON"))
            .unwrap_or_else(|| panic!("case {case:?}: expected an output in {answered:?}"));
        let arn = format!("arn:aws:states:{REGION}:{ACCOUNT}:activity:{name}");
        assert!(
            output.to_string().contains(&arn),
            "case {case:?}: expected the result to name {arn:?}, got {output}"
        );
    }
}

/// The work `TestState` does NOT do without a mock standing in for it: an answer that
/// comes from outside the call that asked.
///
/// "TestState only supports the following when a mock is specified: Activity tasks,
/// .sync or .waitForTaskToken service integration patterns" — an activity's work is a
/// worker's, a run-a-job task's outcome is the job's own end, and a callback task's is
/// whoever holds its token, so none of the three can be answered inside this call. Each
/// is reported as work that did not happen rather than as a synthesised result.
#[tokio::test]
async fn work_answered_from_outside_this_call_is_not_run_without_a_mock() {
    // (case, the Resource the Task state names)
    let rows: [(&str, String); 3] = [
        (
            "an activity task, whose work a worker polls for",
            format!("arn:aws:states:{REGION}:{ACCOUNT}:activity:polled"),
            ),
        (
            "a run-a-job task, whose outcome is the job's own end",
            "arn:aws:states:::states:startExecution.sync".to_string(),
        ),
        (
            "a callback task, whose outcome is whoever holds its token",
            "arn:aws:states:::states:startExecution.waitForTaskToken".to_string(),
        ),
    ];

    let emulator = Emulator::new();
    for (case, resource) in rows {
        let definition = json!({
            "Type": "Task",
            "Resource": resource,
            "Parameters": { "Name.$": "$.name" },
            "End": true,
        })
        .to_string();
        let answered = emulator
            .ok(
                "TestState",
                &json!({
                    "definition": definition,
                    "input": r#"{"name":"unrun"}"#,
                    "roleArn": ROLE_ARN,
                }),
            )
            .await;
        assert_eq!(
            answered.get("status").and_then(Value::as_str),
            Some("FAILED"),
            "case {case:?}: expected the state to fail, got {answered:?}"
        );
        assert_eq!(
            answered.get("error").and_then(Value::as_str),
            Some("States.Runtime"),
            "case {case:?}: expected ASL's runtime error, got {answered:?}"
        );
        let cause = answered
            .get("cause")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            cause.contains("nothing was run for it"),
            "case {case:?}: the cause must say nothing ran, got {cause:?}"
        );
        assert!(
            !activity_exists(&emulator, "unrun").await,
            "case {case:?}: nothing may be created for work that did not happen"
        );
    }
}
