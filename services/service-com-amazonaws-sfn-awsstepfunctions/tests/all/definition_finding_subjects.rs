// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the SUBJECTS an `InvalidDefinition` message states, driven
//! over this service's own wire protocol through the production `MultiServiceRouter`.
//!
//! The rule under test: a definition is refused with EVERY problem it carries, one
//! subject per problem, and each subject states where the problem was found — the
//! `/States/<name>/<field>` position for a problem a state or a field owns, and the
//! bare word `null` for a problem about the definition AS A WHOLE, which sits at no
//! position in the document. A caller fixing the reported problems is therefore told
//! all of them at once rather than one per attempt.
//!
//! The rows differ along what the rule generalises over: how many problems the
//! definition carries (one, two, three), which KINDS they are (a transition target no
//! state declares, a state nothing reaches, a state that says nowhere to go, and a
//! workflow with no terminal state at all) and whether the problem is owned by a field,
//! by a state or by the whole definition — which is what decides the place its subject
//! states.
//!
//! The harness is `state_machine_lifecycle`'s; `tests/all.rs` is this binary's crate
//! root, so it is reached through `crate::state_machine_lifecycle`.

use serde_json::{json, Value};

use crate::state_machine_lifecycle::{Emulator, ROLE_ARN};

/// The envelope every definition refusal of this service carries.
const ENVELOPE_OPEN: &str = "Invalid State Machine Definition: '";

/// The place a subject states for a problem about the definition as a whole.
const NO_LOCATION: &str = "null";

/// The subjects the `InvalidDefinition` for `definition` quotes, in order.
async fn subjects(emulator: &Emulator, case: &str, index: usize, definition: &Value) -> String {
    submitted(emulator, case, index, &definition.to_string()).await
}

/// The same, for a definition submitted as the TEXT it is written as: a rule about the
/// order a definition writes its members in cannot be driven through a re-serialized
/// document, which writes them in an order of its own.
async fn submitted(emulator: &Emulator, case: &str, index: usize, definition: &str) -> String {
    let (status, body) = emulator
        .call(
            "CreateStateMachine",
            &json!({
                "name": format!("findings-{index}"),
                "definition": definition,
                "roleArn": ROLE_ARN,
            }),
        )
        .await;
    let code = crate::state_machine_lifecycle::error_code(&body);
    assert_eq!(
        (status, code.as_str()),
        (400, "InvalidDefinition"),
        "case {case:?}: expected 400 InvalidDefinition, got {status} with {body:?}"
    );
    let message = body
        .get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    message
        .strip_prefix(ENVELOPE_OPEN)
        .and_then(|rest| rest.strip_suffix('\''))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the definition envelope, got {message:?}")
        })
        .to_string()
}

/// A refusal states every problem the definition carries, and a problem about the
/// whole definition states its place as `null`.
#[tokio::test]
async fn a_definition_refusal_states_every_problem_and_where_it_is() {
    let emulator = Emulator::new();

    // (case, the definition, the subjects the refusal must quote, in order)
    let rows: Vec<(&str, Value, Vec<String>)> = vec![
        (
            // The recorded row: a state transitioning to a state the definition does
            // not declare, in a workflow that can therefore never stop.
            "a dangling transition target in a workflow with no terminal state",
            json!({
                "StartAt": "InvokeProcessingLambda",
                "States": {
                    "InvokeProcessingLambda": {
                        "Type": "Pass",
                        "Next": "AStateThatIsNotDefined",
                    }
                },
            }),
            vec![
                "MISSING_TRANSITION_TARGET: Missing 'Next' target: AStateThatIsNotDefined at \
                 /States/InvokeProcessingLambda/Next"
                    .to_string(),
                format!("MISSING_END_STATE: Workflow has no terminal state at {NO_LOCATION}"),
            ],
        ),
        (
            // The same dangling target, in a workflow that CAN stop: one problem, and
            // the machine-level subject is not stated at all.
            "a dangling transition target beside a terminal state",
            json!({
                "StartAt": "A",
                "States": {
                    "A": { "Type": "Choice", "Choices": [
                        { "Variable": "$.n", "NumericEquals": 1, "Next": "Nowhere" }
                    ], "Default": "Done" },
                    "Done": { "Type": "Succeed" },
                },
            }),
            vec![
                "MISSING_TRANSITION_TARGET: Missing 'Next' target: Nowhere at \
                 /States/A/Choices[0]/Next"
                    .to_string(),
            ],
        ),
        (
            // A workflow that cannot stop and nothing else: the one subject states its
            // place as `null`.
            "a workflow with no terminal state and nothing else wrong",
            json!({
                "StartAt": "Loop",
                "States": { "Loop": { "Type": "Pass", "Next": "Loop" } },
            }),
            vec![format!(
                "MISSING_END_STATE: Workflow has no terminal state at {NO_LOCATION}"
            )],
        ),
        (
            // Three problems at once: a state that says nowhere to go, a state nothing
            // reaches, and the workflow that can therefore never stop.
            "a state with no transition, an unreachable state and no terminal state",
            json!({
                "StartAt": "Start",
                "States": {
                    "Start": { "Type": "Pass" },
                    "Orphan": { "Type": "Pass", "Next": "Start" },
                },
            }),
            vec![
                "SCHEMA_VALIDATION_FAILED: state 'Start' is a 'Pass' state, so it must carry \
                 either 'Next' or \"End\": true at /States/Start"
                    .to_string(),
                "MISSING_TRANSITION_TARGET: State \"Orphan\" is not reachable. at /States/Orphan"
                    .to_string(),
                format!("MISSING_END_STATE: Workflow has no terminal state at {NO_LOCATION}"),
            ],
        ),
    ];

    for (index, (case, definition, expected)) in rows.into_iter().enumerate() {
        let stated = subjects(&emulator, case, index, &definition).await;
        let expected = expected.join(", ");
        assert_eq!(
            stated, expected,
            "case {case:?}: expected the subjects {expected:?}, got {stated:?}"
        );
    }
}

/// The same definition `ValidateStateMachineDefinition` reports a diagnostic list for
/// is the one a create refuses, problem for problem: the create's subjects are the
/// validation's ERROR diagnostics, so the two cannot disagree about what is wrong.
#[tokio::test]
async fn a_create_refuses_what_the_validation_reports() {
    let emulator = Emulator::new();
    let definition = json!({
        "StartAt": "InvokeProcessingLambda",
        "States": {
            "InvokeProcessingLambda": { "Type": "Pass", "Next": "AStateThatIsNotDefined" }
        },
    });

    let validated = emulator
        .ok(
            "ValidateStateMachineDefinition",
            &json!({ "definition": definition.to_string() }),
        )
        .await;
    let diagnostics = validated
        .get("diagnostics")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let codes: Vec<&str> = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.get("severity").and_then(Value::as_str) == Some("ERROR"))
        .filter_map(|diagnostic| diagnostic.get("code").and_then(Value::as_str))
        .collect();
    assert_eq!(
        codes,
        vec!["MISSING_TRANSITION_TARGET", "MISSING_END_STATE"],
        "expected both problems to be reported, got {validated:?}"
    );

    let stated = subjects(&emulator, "the same definition on a create", 99, &definition).await;
    for code in codes {
        assert!(
            stated.contains(code),
            "expected the create's subjects to carry {code:?}, got {stated:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The order the subjects are stated in: the definition's own
// ---------------------------------------------------------------------------

/// What a subject says about a field the Amazon States Language does not declare
/// (`asl::field_not_supported_message`), stated at `location`.
fn unsupported_field(field: &str, location: &str) -> String {
    format!("SCHEMA_VALIDATION_FAILED: Field '{field}' is not supported at {location}")
}

/// What a subject says about a reserved `$states` term an expression read where the
/// language does not bind it (`asl::field_does_not_exist`), stated at `location`.
fn does_not_exist(field: &str, location: &str) -> String {
    format!("UNSUPPORTED_JSONATA_EXPRESSION: Field '{field}' does not exist. at {location}")
}

/// Every ERROR diagnostic `ValidateStateMachineDefinition` reports for `definition`,
/// each stated the way a refusal's subject states it, in the order they are reported.
///
/// The two reports are one walk's, so a row below asserts the order through both: what
/// the validation lists and what the create quotes cannot disagree.
async fn diagnosed(emulator: &Emulator, case: &str, definition: &str) -> Vec<String> {
    let validated = emulator
        .ok(
            "ValidateStateMachineDefinition",
            &json!({ "definition": definition }),
        )
        .await;
    validated
        .get("diagnostics")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|diagnostic| {
            let member = |name: &str| {
                diagnostic
                    .get(name)
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: expected a '{name}' on {diagnostic:?}")
                    })
                    .to_string()
            };
            let location = diagnostic
                .get("location")
                .and_then(Value::as_str)
                .unwrap_or(NO_LOCATION);
            format!("{}: {} at {location}", member("code"), member("message"))
        })
        .collect()
}

/// The problems found in a definition are reported in the order the SUBMITTED
/// definition writes the members they are about — not sorted by location, and not in
/// the order a parse that sorts an object's names would visit them.
///
/// The rows differ along what the rule generalises over: WHERE the members sit (the
/// state machine object, one state, a payload template under a state, an entry of a
/// `Catch` array, a state list nested in a `Parallel` branch, the `States` map itself,
/// an element of a list inside a template member), how the written order relates to the
/// sorted one (reversed, mixed, equal) and which KIND of problem is found there (a field
/// the language does not declare, a reserved term read out of scope). Two of the rows are
/// the same definition with its two offending members written the other way round, so
/// what decides the order is the document and nothing else.
#[tokio::test]
async fn the_subjects_follow_the_order_the_definition_writes_its_members_in() {
    // (case, the definition AS SUBMITTED, the subjects it must be refused with, in
    // order)
    let rows: Vec<(&str, &str, Vec<String>)> = vec![
        (
            "two fields of one state, written in reverse of their sorted order",
            r#"{"StartAt":"S","States":{"S":{"Type":"Pass","End":true,"zeta":1,"alpha":2}}}"#,
            vec![
                unsupported_field("zeta", "/States/S"),
                unsupported_field("alpha", "/States/S"),
            ],
        ),
        (
            "the same two fields, written in their sorted order",
            r#"{"StartAt":"S","States":{"S":{"Type":"Pass","End":true,"alpha":1,"zeta":2}}}"#,
            vec![
                unsupported_field("alpha", "/States/S"),
                unsupported_field("zeta", "/States/S"),
            ],
        ),
        (
            "two fields of the state machine object, written around the ones it owes",
            r#"{"zeta":1,"StartAt":"S","alpha":2,"States":{"S":{"Type":"Succeed"}}}"#,
            vec![
                unsupported_field("zeta", "/"),
                unsupported_field("alpha", "/"),
            ],
        ),
        (
            "one field in each of two states, the states written in reverse order",
            r#"{"StartAt":"Zed","States":{"Zed":{"Type":"Pass","Next":"Able","zeta":1},"Able":{"Type":"Pass","End":true,"alpha":2}}}"#,
            vec![
                unsupported_field("zeta", "/States/Zed"),
                unsupported_field("alpha", "/States/Able"),
            ],
        ),
        (
            "two fields of a Catch entry",
            r#"{"StartAt":"S","States":{"S":{"Type":"Task","Resource":"arn:aws:lambda:us-west-2:000000000000:function:f","Catch":[{"ErrorEquals":["States.ALL"],"Next":"Done","zeta":1,"alpha":2}],"End":true},"Done":{"Type":"Succeed"}}}"#,
            vec![
                unsupported_field("zeta", "/States/S/Catch[0]"),
                unsupported_field("alpha", "/States/S/Catch[0]"),
            ],
        ),
        (
            "two fields of a state inside a Parallel branch",
            r#"{"StartAt":"P","States":{"P":{"Type":"Parallel","Branches":[{"StartAt":"In","States":{"In":{"Type":"Pass","End":true,"zulu":1,"bravo":2}}}],"End":true}}}"#,
            vec![
                unsupported_field("zulu", "/States/P/Branches[0]/States/In"),
                unsupported_field("bravo", "/States/P/Branches[0]/States/In"),
            ],
        ),
        (
            "two members of one JSONata Output reading the error output",
            r#"{"QueryLanguage":"JSONata","StartAt":"T","States":{"T":{"Type":"Pass","Output":{"error":"{% $states.errorOutput.Error %}","cause":"{% $states.errorOutput.Cause %}"},"End":true}}}"#,
            vec![
                does_not_exist("$states.errorOutput.Error", "/States/T/Output/error"),
                does_not_exist("$states.errorOutput.Cause", "/States/T/Output/cause"),
            ],
        ),
        (
            "the same Output with its two members written the other way round",
            r#"{"QueryLanguage":"JSONata","StartAt":"T","States":{"T":{"Type":"Pass","Output":{"cause":"{% $states.errorOutput.Cause %}","error":"{% $states.errorOutput.Error %}"},"End":true}}}"#,
            vec![
                does_not_exist("$states.errorOutput.Cause", "/States/T/Output/cause"),
                does_not_exist("$states.errorOutput.Error", "/States/T/Output/error"),
            ],
        ),
        (
            // A field inside a LIST is written where its member is: its elements keep
            // the order the list writes them, and the member written after the list
            // follows all of them — where sorting the locations would put `after`
            // first.
            "offending fields inside a list, and a member written after the list",
            r#"{"QueryLanguage":"JSONata","StartAt":"T","States":{"T":{"Type":"Pass","Output":{"all":["{% $states.errorOutput.Cause %}","{% $states.errorOutput.Error %}"],"after":"{% $states.errorOutput.Why %}"},"End":true}}}"#,
            vec![
                does_not_exist("$states.errorOutput.Cause", "/States/T/Output/all[0]"),
                does_not_exist("$states.errorOutput.Error", "/States/T/Output/all[1]"),
                does_not_exist("$states.errorOutput.Why", "/States/T/Output/after"),
            ],
        ),
    ];

    let emulator = Emulator::new();
    for (index, (case, definition, expected)) in rows.into_iter().enumerate() {
        let stated = submitted(&emulator, case, 200 + index, definition).await;
        let wanted = expected.join(", ");
        assert_eq!(
            stated, wanted,
            "case {case:?}: expected the subjects {wanted:?}, got {stated:?}"
        );

        let reported = diagnosed(&emulator, case, definition).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the validation to report {expected:?} in that order, got \
             {reported:?}"
        );
    }
}
