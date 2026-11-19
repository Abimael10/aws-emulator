// SPDX-License-Identifier: CC0-1.0

//! What the ASL static validator reports about the FIELDS a definition carries and
//! about the transition graph its states form, driven end to end over this service's
//! own wire protocol (`awsJson1_0`, `POST /` with
//! `X-Amz-Target: AWSStepFunctions.<Operation>`) through the production router.
//!
//! The rules under test, each over a table whose rows differ along what the rule
//! generalises over rather than along the one definition that exposed it:
//!
//! 1. the fields the Amazon States Language declares on a state are a CLOSED set per
//!    state type, and a state carrying a field outside its type's set earns one
//!    `SCHEMA_VALIDATION_FAILED` ERROR per field, located at the STATE and stating the
//!    field — the same rule, and the same sentence, for a `Retry` retrier's and a
//!    `Catch` catcher's own closed sets, which also say which fields each must carry;
//! 2. `States.ALL` is admitted only as the single entry of an `ErrorEquals` list and
//!    only in the last entry of the array holding it;
//! 3. a diagnostic about an element of an array addresses it with the index
//!    BRACKETED onto the array's field name, never as a path segment of its own;
//! 4. the two halves of the transition-graph rule are reported under one code: a
//!    transition target no state declares, named as the value of the field the
//!    language calls `Next` whichever member declared it, and a state nothing in the
//!    definition names;
//! 5. what `CreateStateMachine` and `UpdateStateMachine` refuse a definition for is
//!    what the validation reports about it, stated one subject per diagnostic;
//! 6. a value the schema requires content in and the definition left EMPTY is reported
//!    with the fixed sentence for that class, the value it is about named only by
//!    `location`;
//! 7. a retrier's `BackoffRate` below the smallest value the language documents for it
//!    is an ERROR located at that figure, which is what makes `result` FAIL;
//! 8. of a group of fields the language declares as ALTERNATIVES, a state carries at
//!    most one, and one carrying more earns an ERROR naming the whole group, located
//!    at the state.
//!
//! The harness (`Emulator`, `error_code`, `ROLE_ARN`, `create_machine`) is the one
//! `state_machine_lifecycle` builds and the validation readers are
//! `definition_validation`'s; `tests/all.rs` is this binary's crate root, so each is
//! reached through `crate::<suite>`.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::asl::{
    state_fields, MISSING_TRANSITION_TARGET, SCHEMA_VALIDATION_FAILED, STATE_TYPES,
};
use service_com_amazonaws_sfn_awsstepfunctions::state::{
    INVALID_STATE_MACHINE_DEFINITION, SEVERITY_ERROR, SEVERITY_WARNING, VALIDATION_RESULT_FAIL,
};

use crate::definition_validation::{diagnostics, result_of, validate};
use crate::state_machine_lifecycle::{
    create_machine, error_code, Emulator, HELLO_DEFINITION, ROLE_ARN,
};

/// The name every state under test is declared under.
const STATE: &str = "Work";

/// The `Resource` a `Task` state under test names.
const RESOURCE: &str = "arn:aws:states:::aws-sdk:sts:getCallerIdentity";

/// The sentence the service answers a value the schema requires content in and the
/// definition left empty with — an `ErrorEquals` naming no error, a `States` map
/// declaring no state. The text is FIXED: which value was left empty is what the
/// diagnostic's `location` says.
const EMPTY_VALUE: &str = "Value cannot be empty";

/// The sentence the service answers a retrier's `BackoffRate` written below the
/// smallest value the Amazon States Language documents for it ("BackoffRate
/// (Optional) … The value must be greater than or equal to 1.0") with.
const MINIMUM_BACKOFF_RATE: &str = "Minimum value is 1.0";

/// How the service begins the sentence it answers a state carrying more than one field
/// of a group the language declares as alternatives with; the group itself follows,
/// bracketed.
const ONE_OF_FIELDS: &str = "There should only be one of the following fields: ";

/// That sentence about the group `fields`.
fn one_of(fields: &str) -> String {
    format!("{ONE_OF_FIELDS}[{fields}]")
}

/// The locations the diagnostics whose message is `message` were reported at, in the
/// order the validation reported them, each held to the severity and code a schema
/// error carries.
fn located(body: &Value, case: &str, message: &str) -> Vec<String> {
    diagnostics(body)
        .iter()
        .filter(|diagnostic| diagnostic["message"].as_str() == Some(message))
        .map(|diagnostic| {
            assert_eq!(
                (
                    diagnostic["severity"].as_str(),
                    diagnostic["code"].as_str()
                ),
                (Some(SEVERITY_ERROR), Some(SCHEMA_VALIDATION_FAILED)),
                "case {case:?}: expected a schema-validation ERROR, got {diagnostic:?}"
            );
            diagnostic["location"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// One ERROR diagnostic as the wire reports it.
fn error_at(code: &str, location: &str, message: &str) -> Value {
    json!({
        "severity": SEVERITY_ERROR,
        "code": code,
        "location": location,
        "message": message,
    })
}

/// A state machine whose one state under test is `state`, with the `Succeed` a
/// continuing state transitions to declared beside it.
fn machine(state: Value) -> String {
    json!({
        "StartAt": STATE,
        "States": { STATE: state, "Done": { "Type": "Succeed" } },
    })
    .to_string()
}

/// A minimal valid state of `state_type` that transitions to `Done`, as the rows
/// below start from.
///
/// The members are the ones the type needs to satisfy every OTHER rule of the walk, so
/// a row that fails does so on the field it ADDS.
fn minimal(state_type: &str) -> Value {
    match state_type {
        "Choice" => json!({
            "Type": "Choice",
            "Choices": [{ "Variable": "$.x", "NumericEquals": 1, "Next": "Done" }],
            "Default": "Done",
        }),
        "Fail" => json!({ "Type": "Fail", "Error": "Boom", "Cause": "on purpose" }),
        "Succeed" => json!({ "Type": "Succeed" }),
        "Map" => json!({
            "Type": "Map",
            "ItemProcessor": { "StartAt": "P", "States": { "P": { "Type": "Pass", "End": true } } },
            "Next": "Done",
        }),
        "Parallel" => json!({
            "Type": "Parallel",
            "Branches": [{ "StartAt": "B", "States": { "B": { "Type": "Pass", "End": true } } }],
            "Next": "Done",
        }),
        "Task" => json!({ "Type": "Task", "Resource": RESOURCE, "Next": "Done" }),
        "Wait" => json!({ "Type": "Wait", "Seconds": 1, "Next": "Done" }),
        _ => json!({ "Type": state_type, "Next": "Done" }),
    }
}

/// What `CreateStateMachine` answers for `definition`, as
/// `(status, error code, message)`.
async fn created(emulator: &Emulator, name: &str, definition: &str) -> (u16, String, String) {
    let (status, body) = emulator
        .call(
            "CreateStateMachine",
            &json!({ "name": name, "definition": definition, "roleArn": ROLE_ARN }),
        )
        .await;
    let message = body
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    (status, error_code(&body), message)
}

// ---------------------------------------------------------------------------
// Rule 1: a state's fields are the closed set its TYPE declares
// ---------------------------------------------------------------------------

/// A state carrying a field its type does not declare is an ERROR stating that field,
/// located at the state; a field the type DOES declare is never reported that way.
///
/// The rows are the CATALOG itself: for every one of the language's state types
/// (`service_com_amazonaws_sfn_awsstepfunctions::state::asl::STATE_TYPES`) every field
/// any other type declares is driven against it
/// (`…::state::asl::state_fields`, the per-type sets), so the rule is pinned over
/// every (type, field) pair rather than over the pair the issue cited.
#[tokio::test]
async fn a_field_the_state_type_does_not_declare_is_reported_at_the_state() {
    let emulator = Emulator::new();

    // Every field the language declares on any state, which is what each type's own
    // set is judged against.
    let every_field: BTreeSet<&str> = STATE_TYPES
        .iter()
        .flat_map(|state_type| state_fields(state_type).iter().copied())
        .collect();
    assert!(
        every_field.contains("Retry") && every_field.contains("Catch"),
        "case the catalog: expected the per-type sets to declare the error-handling \
         fields, got {every_field:?}"
    );

    for state_type in STATE_TYPES {
        let declared: BTreeSet<&str> = state_fields(state_type).iter().copied().collect();
        assert!(
            !declared.is_empty(),
            "case {state_type:?}: every type the language defines declares fields, got none"
        );

        for field in &every_field {
            let case = format!("the field {field:?} on a {state_type} state");
            let mut state = minimal(state_type);
            let members = state.as_object_mut().expect("a state is an object");
            // The value is immaterial: what is reported is the field's NAME, so one
            // value serves every row and no row can pass by carrying a value the
            // field would be refused for anyway.
            members.insert((*field).to_string(), json!("x"));
            let definition = machine(state);

            let body = validate(
                &emulator,
                &case,
                &json!({ "definition": &definition, "severity": SEVERITY_WARNING }),
            )
            .await;
            let reported = diagnostics(&body);
            let expected = error_at(
                SCHEMA_VALIDATION_FAILED,
                &format!("/States/{STATE}"),
                &format!("Field '{field}' is not supported"),
            );
            if declared.contains(field) {
                assert!(
                    !reported.contains(&expected),
                    "case {case:?}: a field the type declares is not reported as \
                     unsupported, got {reported:?}"
                );
                continue;
            }
            assert!(
                reported.contains(&expected),
                "case {case:?}: expected the diagnostic {expected:?}, got {reported:?}"
            );
            assert_eq!(
                result_of(&body),
                VALIDATION_RESULT_FAIL,
                "case {case:?}: a field the type does not declare fails the definition, got \
                 {body:?}"
            );
        }
    }
}

/// The same rule over a `Retry` retrier's and a `Catch` catcher's own closed sets,
/// which also say which fields each entry must carry — reported at the ENTRY, with the
/// index bracketed onto the array's field name.
///
/// Rows differ along WHICH array the entry sits in, which field is missing or
/// undeclared, where in the array the entry sits, and which state type declares the
/// array.
#[tokio::test]
async fn a_retrier_and_a_catcher_are_judged_against_the_fields_they_declare() {
    let emulator = Emulator::new();

    // (case, the members to add to a Task state, the diagnostics expected)
    let rows: Vec<(&str, Value, Vec<Value>)> = vec![
        (
            "a retrier that names no errors",
            json!({ "Retry": [{ "MaxAttempts": 2 }] }),
            vec![error_at(
                SCHEMA_VALIDATION_FAILED,
                &format!("/States/{STATE}/Retry[0]"),
                "These fields are required: [ErrorEquals]",
            )],
        ),
        (
            "a catcher that names neither the errors it takes nor where it goes",
            json!({ "Catch": [{ "ResultPath": "$.error" }] }),
            vec![error_at(
                SCHEMA_VALIDATION_FAILED,
                &format!("/States/{STATE}/Catch[0]"),
                "These fields are required: [ErrorEquals, Next]",
            )],
        ),
        (
            "a catcher that names no state to continue at",
            json!({ "Catch": [{ "ErrorEquals": ["States.ALL"] }] }),
            vec![error_at(
                SCHEMA_VALIDATION_FAILED,
                &format!("/States/{STATE}/Catch[0]"),
                "These fields are required: [Next]",
            )],
        ),
        (
            "a retrier carrying a field only a catcher declares",
            json!({ "Retry": [{ "ErrorEquals": ["States.Timeout"], "Next": "Done" }] }),
            vec![error_at(
                SCHEMA_VALIDATION_FAILED,
                &format!("/States/{STATE}/Retry[0]"),
                "Field 'Next' is not supported",
            )],
        ),
        (
            "a catcher carrying a field only a retrier declares",
            json!({
                "Catch": [
                    { "ErrorEquals": ["States.Timeout"], "Next": "Done" },
                    { "ErrorEquals": ["States.ALL"], "Next": "Done", "MaxAttempts": 2 },
                ],
            }),
            vec![error_at(
                SCHEMA_VALIDATION_FAILED,
                &format!("/States/{STATE}/Catch[1]"),
                "Field 'MaxAttempts' is not supported",
            )],
        ),
        (
            "a retrier carrying a field the language declares nowhere",
            json!({ "Retry": [{ "ErrorEquals": ["States.ALL"], "Backoff": 2 }] }),
            vec![error_at(
                SCHEMA_VALIDATION_FAILED,
                &format!("/States/{STATE}/Retry[0]"),
                "Field 'Backoff' is not supported",
            )],
        ),
        (
            "a Retry that is not a list of retriers at all",
            json!({ "Retry": { "ErrorEquals": ["States.ALL"] } }),
            vec![error_at(
                SCHEMA_VALIDATION_FAILED,
                &format!("/States/{STATE}/Retry"),
                "'Retry' must be an array",
            )],
        ),
        (
            "a catcher that is not an object",
            json!({ "Catch": ["States.ALL"] }),
            vec![error_at(
                SCHEMA_VALIDATION_FAILED,
                &format!("/States/{STATE}/Catch[0]"),
                "every entry of 'Catch' must be a JSON object",
            )],
        ),
        (
            "a retrier whose ErrorEquals is one name rather than a list of them",
            json!({ "Retry": [{ "ErrorEquals": "States.ALL" }] }),
            vec![error_at(
                SCHEMA_VALIDATION_FAILED,
                &format!("/States/{STATE}/Retry[0]/ErrorEquals"),
                "'ErrorEquals' must be an array",
            )],
        ),
        (
            "a retrier that names no error at all",
            json!({ "Retry": [{ "ErrorEquals": [] }] }),
            vec![error_at(
                SCHEMA_VALIDATION_FAILED,
                &format!("/States/{STATE}/Retry[0]/ErrorEquals"),
                EMPTY_VALUE,
            )],
        ),
        (
            "a catcher naming an error that is not a name",
            json!({ "Catch": [{ "ErrorEquals": ["States.Timeout", 7], "Next": "Done" }] }),
            vec![error_at(
                SCHEMA_VALIDATION_FAILED,
                &format!("/States/{STATE}/Catch[0]/ErrorEquals[1]"),
                "'ErrorEquals' must name errors as strings",
            )],
        ),
        (
            "the retriers and catchers the language declares, carried in full",
            json!({
                "Retry": [{
                    "ErrorEquals": ["States.Timeout", "Lambda.TooManyRequestsException"],
                    "IntervalSeconds": 2,
                    "MaxAttempts": 4,
                    "BackoffRate": 1.5,
                    "MaxDelaySeconds": 10,
                    "JitterStrategy": "FULL",
                    "Comment": "every declared field",
                }],
                "Catch": [{
                    "ErrorEquals": ["States.ALL"],
                    "Next": "Done",
                    "ResultPath": "$.error",
                    "Comment": "every declared field",
                }],
            }),
            vec![],
        ),
    ];

    for (index, (case, extra, expected)) in rows.into_iter().enumerate() {
        let mut state = minimal("Task");
        let members = state.as_object_mut().expect("a state is an object");
        for (member, value) in extra.as_object().expect("the members are an object") {
            members.insert(member.clone(), value.clone());
        }
        let definition = machine(state);

        let body = validate(
            &emulator,
            case,
            &json!({ "definition": &definition, "severity": SEVERITY_WARNING }),
        )
        .await;
        let reported = diagnostics(&body);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected exactly the diagnostics {expected:?}, got {reported:?}"
        );

        // What a create does with the same definition follows from the same walk.
        let (status, code, message) =
            created(&emulator, &format!("handler-{index}"), &definition).await;
        if expected.is_empty() {
            assert_eq!(
                status, 200,
                "case {case:?}: a definition the validation passes is created, got {status} \
                 {code} {message:?}"
            );
            continue;
        }
        assert_eq!(
            (status, code.as_str()),
            (400, "InvalidDefinition"),
            "case {case:?}: expected the create to refuse the definition, got {status} {code}"
        );
        for diagnostic in &expected {
            let subject = format!(
                "{SCHEMA_VALIDATION_FAILED}: {} at {}",
                diagnostic["message"].as_str().unwrap_or_default(),
                diagnostic["location"].as_str().unwrap_or_default(),
            );
            assert!(
                message.contains(&subject),
                "case {case:?}: expected the refusal to state {subject:?}, got {message:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 2: where `States.ALL` may be named
// ---------------------------------------------------------------------------

/// The sentence the service answers an `ErrorEquals` naming the catch-all beside
/// another name, or in an entry another entry follows.
const ALL_ALONE: &str = "States.ALL must appear alone and at end of list";

/// `States.ALL` is admitted only as the single entry of an `ErrorEquals` list, and
/// only in the last entry of the array that holds that list.
///
/// Rows differ along WHICH array holds the entry (`Retry`, `Catch`), how many entries
/// it holds, where in the array the entry naming the catch-all sits, and where in its
/// `ErrorEquals` the catch-all itself sits.
#[tokio::test]
async fn states_all_is_admitted_only_alone_and_in_the_last_entry() {
    let emulator = Emulator::new();

    // (case, the member, its entries, the locations the catch-all is reported at)
    let rows: Vec<(&str, &str, Value, Vec<String>)> = vec![
        (
            "the catch-all alone, in the only retrier",
            "Retry",
            json!([{ "ErrorEquals": ["States.ALL"] }]),
            vec![],
        ),
        (
            "the catch-all first in a retrier's list",
            "Retry",
            json!([{ "ErrorEquals": ["States.ALL", "States.Timeout"] }]),
            vec![format!("/States/{STATE}/Retry[0]/ErrorEquals[0]")],
        ),
        (
            "the catch-all last in a retrier's list, but not alone",
            "Retry",
            json!([{ "ErrorEquals": ["States.Timeout", "States.ALL"] }]),
            vec![format!("/States/{STATE}/Retry[0]/ErrorEquals[1]")],
        ),
        (
            "a retrier naming the catch-all with another retrier after it",
            "Retry",
            json!([
                { "ErrorEquals": ["States.ALL"] },
                { "ErrorEquals": ["States.Timeout"] },
            ]),
            vec![format!("/States/{STATE}/Retry[0]/ErrorEquals[0]")],
        ),
        (
            "a retrier naming the catch-all last of three",
            "Retry",
            json!([
                { "ErrorEquals": ["States.Timeout"] },
                { "ErrorEquals": ["Lambda.ServiceException"] },
                { "ErrorEquals": ["States.ALL"] },
            ]),
            vec![],
        ),
        (
            "a catcher naming the catch-all with another catcher after it",
            "Catch",
            json!([
                { "ErrorEquals": ["States.ALL"], "Next": "Done" },
                { "ErrorEquals": ["States.Timeout"], "Next": "Done" },
            ]),
            vec![format!("/States/{STATE}/Catch[0]/ErrorEquals[0]")],
        ),
        (
            "a catcher naming the catch-all beside a custom name, last of two",
            "Catch",
            json!([
                { "ErrorEquals": ["States.Timeout"], "Next": "Done" },
                { "ErrorEquals": ["MyError", "States.ALL"], "Next": "Done" },
            ]),
            vec![format!("/States/{STATE}/Catch[1]/ErrorEquals[1]")],
        ),
        (
            "two entries each naming the catch-all",
            "Catch",
            json!([
                { "ErrorEquals": ["States.ALL"], "Next": "Done" },
                { "ErrorEquals": ["States.ALL", "MyError"], "Next": "Done" },
            ]),
            vec![
                format!("/States/{STATE}/Catch[0]/ErrorEquals[0]"),
                format!("/States/{STATE}/Catch[1]/ErrorEquals[0]"),
            ],
        ),
        (
            "a catcher naming the catch-all alone and last",
            "Catch",
            json!([
                { "ErrorEquals": ["States.Timeout"], "Next": "Done" },
                { "ErrorEquals": ["States.ALL"], "Next": "Done" },
            ]),
            vec![],
        ),
    ];

    for (index, (case, member, entries, locations)) in rows.into_iter().enumerate() {
        let mut state = minimal("Task");
        state
            .as_object_mut()
            .expect("a state is an object")
            .insert(member.to_string(), entries);
        let definition = machine(state);

        let body = validate(
            &emulator,
            case,
            &json!({ "definition": &definition, "severity": SEVERITY_WARNING }),
        )
        .await;
        let expected: Vec<Value> = locations
            .iter()
            .map(|location| error_at(SCHEMA_VALIDATION_FAILED, location, ALL_ALONE))
            .collect();
        let reported = diagnostics(&body);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected exactly the diagnostics {expected:?}, got {reported:?}"
        );

        let (status, code, message) =
            created(&emulator, &format!("catch-all-{index}"), &definition).await;
        if locations.is_empty() {
            assert_eq!(
                status, 200,
                "case {case:?}: a definition the validation passes is created, got {status} \
                 {code} {message:?}"
            );
            continue;
        }
        // The refusal states one subject per error, in the words the validation
        // reported it in.
        let subjects: Vec<String> = locations
            .iter()
            .map(|location| format!("{SCHEMA_VALIDATION_FAILED}: {ALL_ALONE} at {location}"))
            .collect();
        let expected_message = format!(
            "{INVALID_STATE_MACHINE_DEFINITION}: '{}'",
            subjects.join(", ")
        );
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (400, "InvalidDefinition", expected_message.as_str()),
            "case {case:?}: expected 400 InvalidDefinition stating {expected_message:?}, got \
             {status} {code} {message:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 3: an array element carries a bracketed index
// ---------------------------------------------------------------------------

/// Every diagnostic about an element of an array addresses that element as
/// `<Field>[<index>]`, so no location carries an index as a path segment of its own.
///
/// Rows differ along WHICH array the element belongs to — a `Catch`, a `Choices`, a
/// `Parallel`'s `Branches`, an `ErrorEquals`, and a list inside a payload a state
/// builds — and along the index itself.
#[tokio::test]
async fn an_array_element_is_located_with_a_bracketed_index() {
    let emulator = Emulator::new();

    // (case, the definition, the location of the diagnostic about the element)
    let rows: Vec<(&str, String, String)> = vec![
        (
            "the Next of the second catcher",
            machine(json!({
                "Type": "Task",
                "Resource": RESOURCE,
                "Catch": [
                    { "ErrorEquals": ["States.Timeout"], "Next": "Done" },
                    { "ErrorEquals": ["States.ALL"], "Next": "Nowhere" },
                ],
                "Next": "Done",
            })),
            format!("/States/{STATE}/Catch[1]/Next"),
        ),
        (
            "the Next of the first Choice rule",
            machine(json!({
                "Type": "Choice",
                "Choices": [
                    { "Variable": "$.x", "NumericEquals": 1, "Next": "Nowhere" },
                    { "Variable": "$.x", "NumericEquals": 2, "Next": "Done" },
                ],
                "Default": "Done",
            })),
            format!("/States/{STATE}/Choices[0]/Next"),
        ),
        (
            "the second branch of a Parallel",
            machine(json!({
                "Type": "Parallel",
                "Branches": [
                    { "StartAt": "B", "States": { "B": { "Type": "Pass", "End": true } } },
                    { "States": { "B": { "Type": "Pass", "End": true } } },
                ],
                "Next": "Done",
            })),
            format!("/States/{STATE}/Branches[1]"),
        ),
        (
            "the catch-all in a retrier's ErrorEquals",
            machine(json!({
                "Type": "Task",
                "Resource": RESOURCE,
                "Retry": [{ "ErrorEquals": ["States.ALL", "States.Timeout"] }],
                "Next": "Done",
            })),
            format!("/States/{STATE}/Retry[0]/ErrorEquals[0]"),
        ),
        (
            "an element of a list a JSONata state puts out",
            json!({
                "QueryLanguage": "JSONata",
                "StartAt": STATE,
                "States": {
                    STATE: {
                        "Type": "Pass",
                        "Output": { "all": ["kept", "{% $states.result %}"] },
                        "Next": "Done",
                    },
                    "Done": { "Type": "Succeed" },
                },
            })
            .to_string(),
            format!("/States/{STATE}/Output/all[1]"),
        ),
    ];

    for (case, definition, location) in rows {
        let body = validate(
            &emulator,
            case,
            &json!({ "definition": definition, "severity": SEVERITY_WARNING }),
        )
        .await;
        let reported: Vec<String> = diagnostics(&body)
            .iter()
            .map(|diagnostic| {
                diagnostic
                    .get("location")
                    .and_then(Value::as_str)
                    .unwrap_or("<absent>")
                    .to_string()
            })
            .collect();
        assert!(
            reported.contains(&location),
            "case {case:?}: expected a diagnostic located at {location:?}, got {reported:?}"
        );
        // No location addresses an element the other way round: an index is never a
        // segment of its own.
        for reported in &reported {
            let numbered = reported
                .split('/')
                .any(|segment| segment.chars().all(|character| character.is_ascii_digit()) && !segment.is_empty());
            assert!(
                !numbered,
                "case {case:?}: expected every location to bracket its indexes, got \
                 {reported:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 4: the transition graph, read both ways
// ---------------------------------------------------------------------------

/// An unresolved transition target names the target as the value of `Next` whichever
/// member declared it, and a state nothing names is reported at that state — both
/// under the one code the transition graph's problems are reported under.
///
/// Rows differ along WHICH member declares the transition (a state's own `Next`, a
/// `Choice` rule's, the `Default` beside them, a `Catch` catcher's, the machine's own
/// `StartAt`), where the state nothing names sits (the definition, a `Map`'s
/// processor, a `Parallel` branch), and which members DO name a state, which earn no
/// diagnostic at all.
#[tokio::test]
async fn the_transition_graph_is_reported_from_both_ends() {
    let emulator = Emulator::new();

    // (case, the definition, the diagnostics expected)
    let rows: Vec<(&str, String, Vec<Value>)> = vec![
        (
            "a state's own Next naming nothing",
            json!({
                "StartAt": STATE,
                "States": {
                    STATE: { "Type": "Pass", "Next": "Nowhere" },
                    "Done": { "Type": "Succeed" },
                },
            })
            .to_string(),
            vec![
                error_at(
                    MISSING_TRANSITION_TARGET,
                    &format!("/States/{STATE}/Next"),
                    "Missing 'Next' target: Nowhere",
                ),
                error_at(
                    MISSING_TRANSITION_TARGET,
                    "/States/Done",
                    "State \"Done\" is not reachable.",
                ),
            ],
        ),
        (
            "a Choice's Default naming nothing, which the language still calls Next",
            json!({
                "StartAt": STATE,
                "States": {
                    STATE: {
                        "Type": "Choice",
                        "Choices": [{ "Variable": "$.x", "NumericEquals": 1, "Next": "Done" }],
                        "Default": "Elsewhere",
                    },
                    "Done": { "Type": "Succeed" },
                },
            })
            .to_string(),
            vec![error_at(
                MISSING_TRANSITION_TARGET,
                &format!("/States/{STATE}/Default"),
                "Missing 'Next' target: Elsewhere",
            )],
        ),
        (
            "the machine's own StartAt naming nothing, which the language still calls Next",
            json!({
                "StartAt": "Bootstrap",
                "States": { STATE: { "Type": "Succeed" } },
            })
            .to_string(),
            vec![
                error_at(
                    MISSING_TRANSITION_TARGET,
                    "/StartAt",
                    "Missing 'Next' target: Bootstrap",
                ),
                error_at(
                    MISSING_TRANSITION_TARGET,
                    &format!("/States/{STATE}"),
                    &format!("State \"{STATE}\" is not reachable."),
                ),
            ],
        ),
        (
            "a state only a Catch catcher names, which is a state something names",
            json!({
                "StartAt": STATE,
                "States": {
                    STATE: {
                        "Type": "Task",
                        "Resource": RESOURCE,
                        "Catch": [{ "ErrorEquals": ["States.ALL"], "Next": "Recover" }],
                        "End": true,
                    },
                    "Recover": { "Type": "Succeed" },
                },
            })
            .to_string(),
            vec![],
        ),
        (
            "a state only a Choice rule names",
            json!({
                "StartAt": STATE,
                "States": {
                    STATE: {
                        "Type": "Choice",
                        "Choices": [{ "Variable": "$.x", "IsPresent": true, "Next": "Done" }],
                        "Default": "Stop",
                    },
                    "Done": { "Type": "Succeed" },
                    "Stop": { "Type": "Fail", "Error": "Stopped" },
                },
            })
            .to_string(),
            vec![],
        ),
        (
            "a state nothing names inside a Map's processor",
            json!({
                "StartAt": STATE,
                "States": {
                    STATE: {
                        "Type": "Map",
                        "ItemProcessor": {
                            "StartAt": "P",
                            "States": {
                                "P": { "Type": "Pass", "End": true },
                                "Orphan": { "Type": "Succeed" },
                            },
                        },
                        "End": true,
                    },
                },
            })
            .to_string(),
            vec![error_at(
                MISSING_TRANSITION_TARGET,
                &format!("/States/{STATE}/ItemProcessor/States/Orphan"),
                "State \"Orphan\" is not reachable.",
            )],
        ),
        (
            "a state nothing names inside a Parallel branch",
            json!({
                "StartAt": STATE,
                "States": {
                    STATE: {
                        "Type": "Parallel",
                        "Branches": [{
                            "StartAt": "B",
                            "States": {
                                "B": { "Type": "Pass", "End": true },
                                "Orphan": { "Type": "Succeed" },
                            },
                        }],
                        "End": true,
                    },
                },
            })
            .to_string(),
            vec![error_at(
                MISSING_TRANSITION_TARGET,
                &format!("/States/{STATE}/Branches[0]/States/Orphan"),
                "State \"Orphan\" is not reachable.",
            )],
        ),
    ];

    for (index, (case, definition, expected)) in rows.into_iter().enumerate() {
        let body = validate(
            &emulator,
            case,
            &json!({ "definition": &definition, "severity": SEVERITY_WARNING }),
        )
        .await;
        let reported = diagnostics(&body);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected exactly the diagnostics {expected:?}, got {reported:?}"
        );

        let (status, code, message) =
            created(&emulator, &format!("graph-{index}"), &definition).await;
        if expected.is_empty() {
            assert_eq!(
                status, 200,
                "case {case:?}: a definition the validation passes is created, got {status} \
                 {code} {message:?}"
            );
            continue;
        }
        // Every error the walk found is stated, one subject per diagnostic, in the
        // order it was found: a caller fixing the definition is told all of them.
        let subjects: Vec<String> = expected
            .iter()
            .map(|diagnostic| {
                format!(
                    "{MISSING_TRANSITION_TARGET}: {} at {}",
                    diagnostic["message"].as_str().unwrap_or_default(),
                    diagnostic["location"].as_str().unwrap_or_default(),
                )
            })
            .collect();
        let expected_message = format!(
            "{INVALID_STATE_MACHINE_DEFINITION}: '{}'",
            subjects.join(", ")
        );
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (400, "InvalidDefinition", expected_message.as_str()),
            "case {case:?}: expected 400 InvalidDefinition stating {expected_message:?}, got \
             {status} {code} {message:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 5: an update reads the same walk as a create
// ---------------------------------------------------------------------------

/// `UpdateStateMachine` refuses a definition for exactly what the validation reports
/// about it, in the same words — so the three operations that read the walk cannot
/// disagree.
///
/// Rows differ along which rule the definition breaks, so what is pinned is that the
/// update states the walk's subjects rather than one definition's refusal.
#[tokio::test]
async fn an_update_states_the_same_subjects_as_the_validation() {
    let emulator = Emulator::new();
    let machine_arn = create_machine(&emulator, "updated", HELLO_DEFINITION).await;

    let rows: Vec<(&str, String)> = vec![
        (
            "a catcher whose Next names nothing",
            machine(json!({
                "Type": "Task",
                "Resource": RESOURCE,
                "Catch": [{ "ErrorEquals": ["States.ALL"], "Next": "HandleError" }],
                "Next": "Done",
            })),
        ),
        (
            "a retrier naming the catch-all beside another name",
            machine(json!({
                "Type": "Task",
                "Resource": RESOURCE,
                "Retry": [{ "ErrorEquals": ["States.ALL", "States.Timeout"] }],
                "Next": "Done",
            })),
        ),
        (
            "a Wait state carrying a field only a Task declares",
            machine(json!({ "Type": "Wait", "Seconds": 1, "Retry": [], "Next": "Done" })),
        ),
    ];

    for (case, definition) in rows {
        let body = validate(
            &emulator,
            case,
            &json!({ "definition": &definition, "severity": SEVERITY_ERROR }),
        )
        .await;
        let subjects: Vec<String> = diagnostics(&body)
            .iter()
            .map(|diagnostic| {
                format!(
                    "{}: {} at {}",
                    diagnostic["code"].as_str().unwrap_or_default(),
                    diagnostic["message"].as_str().unwrap_or_default(),
                    diagnostic["location"].as_str().unwrap_or_default(),
                )
            })
            .collect();
        assert!(
            !subjects.is_empty(),
            "case {case:?}: expected the validation to report an error, got {body:?}"
        );

        let (status, updated) = emulator
            .call(
                "UpdateStateMachine",
                &json!({ "stateMachineArn": &machine_arn, "definition": &definition }),
            )
            .await;
        let expected_message = format!(
            "{INVALID_STATE_MACHINE_DEFINITION}: '{}'",
            subjects.join(", ")
        );
        let message = updated
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            (status, error_code(&updated).as_str(), message),
            (400, "InvalidDefinition", expected_message.as_str()),
            "case {case:?}: expected the update to state {expected_message:?}, got {status} \
             with {updated:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 6: a value the schema requires content in, left empty
// ---------------------------------------------------------------------------

/// A value the schema requires content in and the definition left EMPTY is one
/// `SCHEMA_VALIDATION_FAILED` ERROR whose message is the fixed sentence for that
/// class, with the value it is about named only by `location`.
///
/// Rows differ along WHICH value was left empty (an `ErrorEquals`, a `States` map),
/// which array holds the entry that left it empty, where in that array the entry sits,
/// and how deeply the machine declaring it is nested — the sentence never moves with
/// any of them.
#[tokio::test]
async fn a_value_left_empty_is_reported_with_the_fixed_sentence_for_that_class() {
    let emulator = Emulator::new();

    // (case, the definition, the locations the empty-value error is reported at)
    let rows: Vec<(&str, String, Vec<String>)> = vec![
        (
            "a retrier that names no error",
            machine(json!({
                "Type": "Task",
                "Resource": RESOURCE,
                "Retry": [{ "ErrorEquals": [] }],
                "Next": "Done",
            })),
            vec![format!("/States/{STATE}/Retry[0]/ErrorEquals")],
        ),
        (
            "a catcher that names no error",
            machine(json!({
                "Type": "Task",
                "Resource": RESOURCE,
                "Catch": [{ "ErrorEquals": [], "Next": "Done" }],
                "Next": "Done",
            })),
            vec![format!("/States/{STATE}/Catch[0]/ErrorEquals")],
        ),
        (
            "the second retrier of the array",
            machine(json!({
                "Type": "Task",
                "Resource": RESOURCE,
                "Retry": [
                    { "ErrorEquals": ["States.Timeout"] },
                    { "ErrorEquals": [] },
                ],
                "Next": "Done",
            })),
            vec![format!("/States/{STATE}/Retry[1]/ErrorEquals")],
        ),
        (
            "every retrier of the array",
            machine(json!({
                "Type": "Task",
                "Resource": RESOURCE,
                "Retry": [{ "ErrorEquals": [] }, { "ErrorEquals": [] }],
                "Next": "Done",
            })),
            vec![
                format!("/States/{STATE}/Retry[0]/ErrorEquals"),
                format!("/States/{STATE}/Retry[1]/ErrorEquals"),
            ],
        ),
        (
            "a retrier of a state the Map state's own machine declares",
            machine(json!({
                "Type": "Map",
                "ItemProcessor": {
                    "StartAt": "P",
                    "States": {
                        "P": {
                            "Type": "Task",
                            "Resource": RESOURCE,
                            "Retry": [{ "ErrorEquals": [] }],
                            "End": true,
                        },
                    },
                },
                "Next": "Done",
            })),
            vec![format!(
                "/States/{STATE}/ItemProcessor/States/P/Retry[0]/ErrorEquals"
            )],
        ),
        (
            "a States map declaring no state",
            json!({ "StartAt": STATE, "States": {} }).to_string(),
            vec!["/States".to_string()],
        ),
        (
            "the States map of the machine a Map state runs",
            machine(json!({
                "Type": "Map",
                "ItemProcessor": { "StartAt": "P", "States": {} },
                "Next": "Done",
            })),
            vec![format!("/States/{STATE}/ItemProcessor/States")],
        ),
        (
            "an ErrorEquals that names an error, which is not empty at all",
            machine(json!({
                "Type": "Task",
                "Resource": RESOURCE,
                "Retry": [{ "ErrorEquals": ["States.ALL"] }],
                "Next": "Done",
            })),
            vec![],
        ),
    ];

    for (case, definition, expected) in rows {
        let body = validate(
            &emulator,
            case,
            &json!({ "definition": &definition, "severity": SEVERITY_WARNING }),
        )
        .await;
        let reported = located(&body, case, EMPTY_VALUE);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {EMPTY_VALUE:?} at {expected:?}, got {reported:?} in \
             {body:?}"
        );
        if expected.is_empty() {
            continue;
        }
        assert_eq!(
            result_of(&body),
            VALIDATION_RESULT_FAIL,
            "case {case:?}: an empty value fails the definition, got {body:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 7: the bound the language documents for a retrier's own figure
// ---------------------------------------------------------------------------

/// A retrier's `BackoffRate` below the smallest value the language documents for it is
/// one `SCHEMA_VALIDATION_FAILED` ERROR located at that figure, which is what makes
/// `result` FAIL and what a create refuses the same definition for; a rate at or above
/// that bound is reported nowhere.
///
/// Rows differ along the VALUE the definition writes — the bound itself, the bound in
/// its integer form, the documented default, one above it, one just below it, zero, a
/// negative rate, and a value that is no figure at all — and along which retrier of the
/// array carries it.
#[tokio::test]
async fn a_retriers_backoff_rate_is_held_to_the_minimum_the_language_documents() {
    let emulator = Emulator::new();

    // (case, the Retry array, the locations the bound is reported at, whether the rate
    // is a FIGURE this bound judges at all — a value of another type is a schema defect
    // of a kind this rule says nothing about, so what a create does with it belongs to
    // that rule and is not asserted here)
    let rows: Vec<(&str, Value, Vec<String>, bool)> = vec![
        (
            "the bound itself",
            json!([{ "ErrorEquals": ["States.ALL"], "BackoffRate": 1.0 }]),
            vec![],
            true,
        ),
        (
            "the bound written as a whole number",
            json!([{ "ErrorEquals": ["States.ALL"], "BackoffRate": 1 }]),
            vec![],
            true,
        ),
        (
            "the default the language documents",
            json!([{ "ErrorEquals": ["States.ALL"], "BackoffRate": 2.0 }]),
            vec![],
            true,
        ),
        (
            "a rate well above the bound",
            json!([{ "ErrorEquals": ["States.ALL"], "BackoffRate": 10.5 }]),
            vec![],
            true,
        ),
        (
            "a rate just below the bound",
            json!([{ "ErrorEquals": ["States.ALL"], "BackoffRate": 0.9 }]),
            vec![format!("/States/{STATE}/Retry[0]/BackoffRate")],
            true,
        ),
        (
            "a rate of zero",
            json!([{ "ErrorEquals": ["States.ALL"], "BackoffRate": 0 }]),
            vec![format!("/States/{STATE}/Retry[0]/BackoffRate")],
            true,
        ),
        (
            "a negative rate",
            json!([{ "ErrorEquals": ["States.ALL"], "BackoffRate": -2.5 }]),
            vec![format!("/States/{STATE}/Retry[0]/BackoffRate")],
            true,
        ),
        (
            "the second retrier of the array",
            json!([
                { "ErrorEquals": ["States.Timeout"], "BackoffRate": 2.0 },
                { "ErrorEquals": ["States.ALL"], "BackoffRate": 0.5 },
            ]),
            vec![format!("/States/{STATE}/Retry[1]/BackoffRate")],
            true,
        ),
        (
            "both retriers of the array",
            json!([
                { "ErrorEquals": ["States.Timeout"], "BackoffRate": 0.25 },
                { "ErrorEquals": ["States.ALL"], "BackoffRate": 0.5 },
            ]),
            vec![
                format!("/States/{STATE}/Retry[0]/BackoffRate"),
                format!("/States/{STATE}/Retry[1]/BackoffRate"),
            ],
            true,
        ),
        (
            "a rate that is no figure at all, which this bound says nothing about",
            json!([{ "ErrorEquals": ["States.ALL"], "BackoffRate": "0.5" }]),
            vec![],
            false,
        ),
    ];

    for (index, (case, retry, expected, a_figure)) in rows.into_iter().enumerate() {
        let definition = machine(json!({
            "Type": "Task",
            "Resource": RESOURCE,
            "Retry": retry,
            "Next": "Done",
        }));

        let body = validate(
            &emulator,
            case,
            &json!({ "definition": &definition, "severity": SEVERITY_WARNING }),
        )
        .await;
        let reported = located(&body, case, MINIMUM_BACKOFF_RATE);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {MINIMUM_BACKOFF_RATE:?} at {expected:?}, got {reported:?} \
             in {body:?}"
        );

        if !a_figure {
            continue;
        }
        // What a create does with the same definition follows from the same walk: a
        // rate the validation reports is a rate `CreateStateMachine` refuses.
        let (status, code, message) =
            created(&emulator, &format!("backoff-{index}"), &definition).await;
        if expected.is_empty() {
            assert_eq!(
                status, 200,
                "case {case:?}: a definition the validation passes is created, got {status} \
                 {code} {message:?}"
            );
            continue;
        }
        assert_eq!(
            result_of(&body),
            VALIDATION_RESULT_FAIL,
            "case {case:?}: a rate below the bound fails the definition, got {body:?}"
        );
        assert_eq!(
            (status, code.as_str()),
            (400, "InvalidDefinition"),
            "case {case:?}: expected the create to refuse the definition, got {status} {code} \
             {message:?}"
        );
        for location in &expected {
            let subject = format!(
                "{SCHEMA_VALIDATION_FAILED}: {MINIMUM_BACKOFF_RATE} at {location}"
            );
            assert!(
                message.contains(&subject),
                "case {case:?}: expected the refusal to state {subject:?}, got {message:?}"
            );
        }
    }
}

/// The same bound wherever a `Retry` is declared: on each state type that declares one
/// and in each machine a state nests, located at the retrier that carries it. A
/// `BackoffRate` written on a CATCHER is not a retrier's figure at all, so it is
/// reported as the unsupported field it is and not against this bound.
///
/// Rows differ along which state type declares the `Retry`, and along how deeply the
/// machine declaring it is nested.
#[tokio::test]
async fn the_backoff_rate_bound_holds_for_every_retry_a_definition_declares() {
    let emulator = Emulator::new();

    /// A retrier naming the catch-all with a rate below the bound.
    fn under() -> Value {
        json!([{ "ErrorEquals": ["States.ALL"], "BackoffRate": 0.5 }])
    }

    // (case, the definition, the locations the bound is reported at)
    let rows: Vec<(&str, String, Vec<String>)> = vec![
        (
            "a Task state's own Retry",
            machine(json!({
                "Type": "Task",
                "Resource": RESOURCE,
                "Retry": under(),
                "Next": "Done",
            })),
            vec![format!("/States/{STATE}/Retry[0]/BackoffRate")],
        ),
        (
            "a Map state's own Retry",
            machine(json!({
                "Type": "Map",
                "ItemProcessor": {
                    "StartAt": "P",
                    "States": { "P": { "Type": "Pass", "End": true } },
                },
                "Retry": under(),
                "Next": "Done",
            })),
            vec![format!("/States/{STATE}/Retry[0]/BackoffRate")],
        ),
        (
            "a Parallel state's own Retry",
            machine(json!({
                "Type": "Parallel",
                "Branches": [{
                    "StartAt": "B",
                    "States": { "B": { "Type": "Pass", "End": true } },
                }],
                "Retry": under(),
                "Next": "Done",
            })),
            vec![format!("/States/{STATE}/Retry[0]/BackoffRate")],
        ),
        (
            "the Retry of a state the Map state's own machine declares",
            machine(json!({
                "Type": "Map",
                "ItemProcessor": {
                    "StartAt": "P",
                    "States": {
                        "P": {
                            "Type": "Task",
                            "Resource": RESOURCE,
                            "Retry": under(),
                            "End": true,
                        },
                    },
                },
                "Next": "Done",
            })),
            vec![format!(
                "/States/{STATE}/ItemProcessor/States/P/Retry[0]/BackoffRate"
            )],
        ),
        (
            "the Retry of a state a Parallel branch declares",
            machine(json!({
                "Type": "Parallel",
                "Branches": [{
                    "StartAt": "B",
                    "States": {
                        "B": {
                            "Type": "Task",
                            "Resource": RESOURCE,
                            "Retry": under(),
                            "End": true,
                        },
                    },
                }],
                "Next": "Done",
            })),
            vec![format!(
                "/States/{STATE}/Branches[0]/States/B/Retry[0]/BackoffRate"
            )],
        ),
        (
            "a catcher carrying the figure, which a catcher does not declare",
            machine(json!({
                "Type": "Task",
                "Resource": RESOURCE,
                "Catch": [{ "ErrorEquals": ["States.ALL"], "Next": "Done", "BackoffRate": 0.5 }],
                "Next": "Done",
            })),
            vec![],
        ),
    ];

    for (case, definition, expected) in rows {
        let body = validate(
            &emulator,
            case,
            &json!({ "definition": &definition, "severity": SEVERITY_WARNING }),
        )
        .await;
        let reported = located(&body, case, MINIMUM_BACKOFF_RATE);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {MINIMUM_BACKOFF_RATE:?} at {expected:?}, got {reported:?} \
             in {body:?}"
        );
        assert_eq!(
            result_of(&body),
            VALIDATION_RESULT_FAIL,
            "case {case:?}: the definition carries a schema error either way, got {body:?}"
        );
        if !expected.is_empty() {
            continue;
        }
        // The one row whose figure is in no retrier: what it earns is the sentence for
        // a field the entry does not declare, at the entry.
        let unsupported = error_at(
            SCHEMA_VALIDATION_FAILED,
            &format!("/States/{STATE}/Catch[0]"),
            "Field 'BackoffRate' is not supported",
        );
        let all = diagnostics(&body);
        assert!(
            all.contains(&unsupported),
            "case {case:?}: expected the diagnostic {unsupported:?}, got {all:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 8: the fields the language declares as ALTERNATIVES
// ---------------------------------------------------------------------------

/// A state carrying more than one field of a group the language declares as
/// ALTERNATIVES — a value and the `*Path` that reads it, the four ways a `Wait` waits,
/// the two ways a state continues — is one `SCHEMA_VALIDATION_FAILED` ERROR naming the
/// whole group, located at the STATE.
///
/// Rows differ along WHICH group the state carries twice, which state type declares
/// that group, which of its fields the row carries, and how deeply the state is
/// nested; the control rows carry one field of a group, two fields of DIFFERENT groups
/// (which is what a `Fail` state naming both an error and a cause is), and a group's
/// fields on a type that declares neither.
#[tokio::test]
async fn a_state_carries_at_most_one_field_of_a_group_of_alternatives() {
    let emulator = Emulator::new();

    /// The minimal state of `state_type` with `extra` merged into it, as the rows that
    /// add a second field of a group start from.
    fn with(state_type: &str, extra: Value) -> Value {
        let mut state = minimal(state_type);
        let members = state.as_object_mut().expect("a state is an object");
        for (member, value) in extra.as_object().expect("the members are an object") {
            members.insert(member.clone(), value.clone());
        }
        state
    }

    // (case, the definition, the diagnostic the group earns)
    let rows: Vec<(&str, String, Option<Value>)> = vec![
        (
            "a Fail state naming its error twice",
            machine(json!({ "Type": "Fail", "Error": "Boom", "ErrorPath": "$.error" })),
            Some(error_at(
                SCHEMA_VALIDATION_FAILED,
                &format!("/States/{STATE}"),
                &one_of("Error, ErrorPath"),
            )),
        ),
        (
            "a Fail state naming its cause twice",
            machine(json!({ "Type": "Fail", "Cause": "why", "CausePath": "$.cause" })),
            Some(error_at(
                SCHEMA_VALIDATION_FAILED,
                &format!("/States/{STATE}"),
                &one_of("Cause, CausePath"),
            )),
        ),
        (
            "a Task state bounding its work twice",
            machine(with(
                "Task",
                json!({ "TimeoutSeconds": 5, "TimeoutSecondsPath": "$.timeout" }),
            )),
            Some(error_at(
                SCHEMA_VALIDATION_FAILED,
                &format!("/States/{STATE}"),
                &one_of("TimeoutSeconds, TimeoutSecondsPath"),
            )),
        ),
        (
            "a Task state bounding its heartbeat twice",
            machine(with(
                "Task",
                json!({ "HeartbeatSeconds": 5, "HeartbeatSecondsPath": "$.beat" }),
            )),
            Some(error_at(
                SCHEMA_VALIDATION_FAILED,
                &format!("/States/{STATE}"),
                &one_of("HeartbeatSeconds, HeartbeatSecondsPath"),
            )),
        ),
        (
            "a Wait state waiting both for a span and until an instant",
            machine(json!({
                "Type": "Wait",
                "Seconds": 1,
                "Timestamp": "2024-01-01T00:00:00Z",
                "Next": "Done",
            })),
            Some(error_at(
                SCHEMA_VALIDATION_FAILED,
                &format!("/States/{STATE}"),
                &one_of("SecondsPath, Seconds, TimestampPath, Timestamp"),
            )),
        ),
        (
            "a Wait state waiting for a span and for the path to one",
            machine(json!({
                "Type": "Wait",
                "Seconds": 1,
                "SecondsPath": "$.seconds",
                "Next": "Done",
            })),
            Some(error_at(
                SCHEMA_VALIDATION_FAILED,
                &format!("/States/{STATE}"),
                &one_of("SecondsPath, Seconds, TimestampPath, Timestamp"),
            )),
        ),
        (
            "a state both continuing and ending",
            machine(json!({ "Type": "Pass", "Next": "Done", "End": true })),
            Some(error_at(
                SCHEMA_VALIDATION_FAILED,
                &format!("/States/{STATE}"),
                &one_of("Next, End"),
            )),
        ),
        (
            "a Map state naming its items twice",
            machine(with("Map", json!({ "Items": [1, 2], "ItemsPath": "$.items" }))),
            Some(error_at(
                SCHEMA_VALIDATION_FAILED,
                &format!("/States/{STATE}"),
                &one_of("Items, ItemsPath"),
            )),
        ),
        (
            "a Map state bounding its concurrency twice",
            machine(with(
                "Map",
                json!({ "MaxConcurrency": 2, "MaxConcurrencyPath": "$.concurrency" }),
            )),
            Some(error_at(
                SCHEMA_VALIDATION_FAILED,
                &format!("/States/{STATE}"),
                &one_of("MaxConcurrency, MaxConcurrencyPath"),
            )),
        ),
        (
            "a Map state tolerating a count twice",
            machine(with(
                "Map",
                json!({
                    "ToleratedFailureCount": 1,
                    "ToleratedFailureCountPath": "$.tolerated",
                }),
            )),
            Some(error_at(
                SCHEMA_VALIDATION_FAILED,
                &format!("/States/{STATE}"),
                &one_of("ToleratedFailureCount, ToleratedFailureCountPath"),
            )),
        ),
        (
            "a Map state tolerating a percentage twice",
            machine(with(
                "Map",
                json!({
                    "ToleratedFailurePercentage": 1,
                    "ToleratedFailurePercentagePath": "$.tolerated",
                }),
            )),
            Some(error_at(
                SCHEMA_VALIDATION_FAILED,
                &format!("/States/{STATE}"),
                &one_of("ToleratedFailurePercentage, ToleratedFailurePercentagePath"),
            )),
        ),
        (
            "a state of the machine a Map state runs, naming its error twice",
            machine(json!({
                "Type": "Map",
                "ItemProcessor": {
                    "StartAt": "P",
                    "States": {
                        "P": { "Type": "Fail", "Error": "Boom", "ErrorPath": "$.error" },
                    },
                },
                "Next": "Done",
            })),
            Some(error_at(
                SCHEMA_VALIDATION_FAILED,
                &format!("/States/{STATE}/ItemProcessor/States/P"),
                &one_of("Error, ErrorPath"),
            )),
        ),
        (
            "a Fail state naming an error and a cause, which are not alternatives",
            machine(json!({ "Type": "Fail", "Error": "Boom", "Cause": "on purpose" })),
            None,
        ),
        (
            "a Fail state naming each of them by its path alone",
            machine(json!({ "Type": "Fail", "ErrorPath": "$.error", "CausePath": "$.cause" })),
            None,
        ),
        (
            "a Wait state waiting one way",
            machine(json!({ "Type": "Wait", "Timestamp": "2024-01-01T00:00:00Z", "Next": "Done" })),
            None,
        ),
        (
            "a Pass state carrying a group of fields its type declares neither of",
            machine(json!({
                "Type": "Pass",
                "Error": "Boom",
                "ErrorPath": "$.error",
                "End": true,
            })),
            None,
        ),
    ];

    for (case, definition, expected) in rows {
        let body = validate(
            &emulator,
            case,
            &json!({ "definition": &definition, "severity": SEVERITY_WARNING }),
        )
        .await;
        let all = diagnostics(&body);
        let grouped: Vec<Value> = all
            .iter()
            .filter(|diagnostic| {
                diagnostic["message"]
                    .as_str()
                    .is_some_and(|message| message.starts_with(ONE_OF_FIELDS))
            })
            .cloned()
            .collect();
        let Some(expected) = expected else {
            assert!(
                grouped.is_empty(),
                "case {case:?}: expected no alternative-field diagnostic, got {grouped:?}"
            );
            continue;
        };
        assert_eq!(
            grouped,
            vec![expected.clone()],
            "case {case:?}: expected exactly the diagnostic {expected:?}, got {grouped:?} in \
             {all:?}"
        );
        assert_eq!(
            result_of(&body),
            VALIDATION_RESULT_FAIL,
            "case {case:?}: a state carrying two of a group fails the definition, got {body:?}"
        );
    }
}
