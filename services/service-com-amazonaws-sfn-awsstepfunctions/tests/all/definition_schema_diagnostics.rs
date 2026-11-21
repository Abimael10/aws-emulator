// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the Amazon States Language SCHEMA rules `CreateStateMachine`
//! and `UpdateStateMachine` run over the submitted `definition`, driven over this
//! service's own wire protocol through the production `MultiServiceRouter`, so what is
//! asserted is what a signed SDK request observes.
//!
//! The rules under test, each over a table whose rows differ along what the rule
//! generalises over — the member, the value, the object the member is written in and the
//! nesting of that object — rather than along the one definition that exposed it:
//!
//! 1. every rule of a `Choice` state's own `Choices` array owes the member that says
//!    where the execution goes when that rule holds, and a rule nested under `And`, `Or`
//!    or `Not` — a condition of the rule holding it — owes none;
//! 2. a state machine object declares itself written in a version of the language the
//!    language defines, and 1.0 is the only one;
//! 3. a WINDOW the language documents as a positive, non-zero number of seconds — a
//!    `Task`'s `TimeoutSeconds` and `HeartbeatSeconds`, and a state machine object's own
//!    `TimeoutSeconds` — admits no value below one second, and the figures whose
//!    documented default IS none keep admitting it;
//! 4. a FRACTIONAL figure states each end of its range as the decimal the language
//!    writes it as, which `Map`'s `ToleratedFailurePercentage` is bounded at;
//! 5. a state carrying a field the type its `Type` names does not declare is reported
//!    ONCE, by the closed field set of that type — `Next` on a state that ends an
//!    execution included — and an `UpdateStateMachine` refused for it leaves the stored
//!    definition and the revision untouched;
//! 6. a workflow with no terminal state is reported at no position in the document,
//!    however deeply the machine missing one is nested.
//!
//! Each refusal is checked against the validation of the same definition: ONE walk
//! answers both, so a definition `CreateStateMachine` refuses is one
//! `ValidateStateMachineDefinition` reports `FAIL` for.
//!
//! The harness (`Emulator`, `create_machine`, `error_code`, `string`, `ROLE_ARN`) is the
//! one `state_machine_lifecycle` builds and the validation readers are
//! `definition_validation`'s; `tests/all/main.rs` picks this file up, so each is reached
//! through `crate::<suite>`.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::asl::{
    MISSING_END_STATE, MISSING_TRANSITION_TARGET,
};
use service_com_amazonaws_sfn_awsstepfunctions::state::{
    INVALID_STATE_MACHINE_DEFINITION, VALIDATION_RESULT_FAIL,
};

use crate::definition_validation::{diagnostics, result_of, validate};
use crate::state_machine_lifecycle::{
    create_machine, error_code, string, Emulator, HELLO_DEFINITION, ROLE_ARN,
};

/// The code every defect of the language's own schema is reported under.
const SCHEMA_VALIDATION_FAILED: &str = "SCHEMA_VALIDATION_FAILED";

/// The place a subject states for a problem bound to no position in the document.
const NO_LOCATION: &str = "null";

/// One subject of an `InvalidDefinition` message: the code, the detail and the place.
fn subject(code: &str, detail: &str, location: &str) -> String {
    format!("{code}: {detail} at {location}")
}

/// The subjects the `InvalidDefinition` message for `definition` quotes, in order,
/// with the envelope every definition refusal of this service carries stripped off.
async fn refused(emulator: &Emulator, case: &str, name: &str, definition: &Value) -> String {
    let (status, body) = emulator
        .call(
            "CreateStateMachine",
            &json!({
                "name": name,
                "definition": definition.to_string(),
                "roleArn": ROLE_ARN,
            }),
        )
        .await;
    let code = error_code(&body);
    assert_eq!(
        (status, code.as_str()),
        (400, "InvalidDefinition"),
        "case {case:?}: expected 400 InvalidDefinition, got {status} {code} with {body:?}"
    );
    let message = string(&body, "message");
    let open = format!("{INVALID_STATE_MACHINE_DEFINITION}: '");
    message
        .strip_prefix(open.as_str())
        .and_then(|rest| rest.strip_suffix('\''))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the definition envelope, got {message:?}")
        })
        .to_string()
}

/// That `ValidateStateMachineDefinition` reports the same definition as FAILING, which
/// is the other half of every refusal below: one walk answers both.
async fn fails_validation(emulator: &Emulator, case: &str, definition: &Value) -> Value {
    let validated = validate(
        emulator,
        case,
        &json!({ "definition": definition.to_string() }),
    )
    .await;
    assert_eq!(
        result_of(&validated),
        VALIDATION_RESULT_FAIL,
        "case {case:?}: expected the validation to FAIL too, got {validated:?}"
    );
    validated
}

/// That `CreateStateMachine` accepts `definition`, which is what makes each table above
/// about the rule rather than about the member it is written at.
async fn admitted(emulator: &Emulator, case: &str, name: &str, definition: &Value) {
    let (status, body) = emulator
        .call(
            "CreateStateMachine",
            &json!({
                "name": name,
                "definition": definition.to_string(),
                "roleArn": ROLE_ARN,
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the definition to be admitted, got {status} with {body:?}"
    );
}

/// A state machine definition declaring `states` and starting at the first of them.
fn machine(start: &str, states: Value) -> Value {
    json!({ "StartAt": start, "States": states })
}

/// A one-state definition: the state `name`, of the shape `state`.
fn one_state(name: &str, state: Value) -> Value {
    machine(name, json!({ name: state }))
}

/// A `Task` state carrying `members` beside the work it performs.
fn task_state(members: Value) -> Value {
    let mut state = json!({
        "Type": "Task",
        "Resource": "arn:aws:states:::aws-sdk:sts:getCallerIdentity",
        "End": true,
    });
    for (member, value) in members.as_object().expect("the members of a Task state") {
        state[member] = value.clone();
    }
    state
}

/// A `Map` state carrying `members` beside the processor every one of them needs.
///
/// The processor runs in DISTRIBUTED mode, which is the mode the tolerances below are
/// declared for, and the language declares `ProcessorConfig` on the processor itself.
fn map_state(members: Value) -> Value {
    let mut state = json!({
        "Type": "Map",
        "ItemProcessor": {
            "ProcessorConfig": { "Mode": "DISTRIBUTED" },
            "StartAt": "P",
            "States": { "P": { "Type": "Pass", "End": true } },
        },
        "End": true,
    });
    for (member, value) in members.as_object().expect("the members of a Map state") {
        state[member] = value.clone();
    }
    state
}

/// A `Parallel` state running the one branch `branch`.
fn parallel_state(branch: Value) -> Value {
    json!({ "Type": "Parallel", "Branches": [branch], "End": true })
}

// ---------------------------------------------------------------------------
// Rule 1: a Choice state's own rules each say where the execution goes
// ---------------------------------------------------------------------------

/// A rule of a `Choice` state's `Choices` array that names no transition is refused
/// where the definition is submitted, stating the member the rule owes and the
/// transition it leaves missing; a rule nested under `And`, `Or` or `Not` owes none.
///
/// The rows differ along the rule's INDEX in the array, the comparator it tests with
/// (including the JSONata `Condition`), the name of the state that holds it and how
/// deeply that state is nested — a `Parallel` branch and a `Map` state's processor each
/// declare a state machine whose rules are judged like the definition's own.
#[tokio::test]
async fn every_rule_of_a_choice_state_owes_the_transition_it_takes() {
    let emulator = Emulator::new();

    // (case, the definition, the pointer of the rule that names no transition)
    let rows: Vec<(&str, Value, &str)> = vec![
        (
            "the one rule of a Choice state",
            json!({
                "StartAt": "C",
                "States": {
                    "C": {
                        "Type": "Choice",
                        "Choices": [{ "Variable": "$.x", "BooleanEquals": true }],
                        "Default": "No",
                    },
                    "No": { "Type": "Pass", "Result": "no", "End": true },
                },
            }),
            "/States/C/Choices[0]",
        ),
        (
            "the second of two rules, the first of which names one",
            json!({
                "StartAt": "C",
                "States": {
                    "C": {
                        "Type": "Choice",
                        "Choices": [
                            { "Variable": "$.a", "StringEquals": "x", "Next": "Done" },
                            { "Variable": "$.b", "IsPresent": true },
                        ],
                        "Default": "Done",
                    },
                    "Done": { "Type": "Succeed" },
                },
            }),
            "/States/C/Choices[1]",
        ),
        (
            "a rule of a Choice state of another name, testing a number",
            json!({
                "StartAt": "Decide",
                "States": {
                    "Decide": {
                        "Type": "Choice",
                        "Choices": [{ "Variable": "$.n", "NumericGreaterThan": 10 }],
                        "Default": "Done",
                    },
                    "Done": { "Type": "Succeed" },
                },
            }),
            "/States/Decide/Choices[0]",
        ),
        (
            // The nested rules are CONDITIONS of this one and owe no transition of
            // their own, so the one pointer reported is the enclosing rule's.
            "a rule whose conditions are nested under And",
            json!({
                "StartAt": "C",
                "States": {
                    "C": {
                        "Type": "Choice",
                        "Choices": [{
                            "And": [
                                { "Variable": "$.a", "IsPresent": true },
                                { "Not": { "Variable": "$.b", "IsNull": true } },
                            ],
                        }],
                        "Default": "Done",
                    },
                    "Done": { "Type": "Succeed" },
                },
            }),
            "/States/C/Choices[0]",
        ),
        (
            "a JSONata rule carrying a Condition and no transition",
            json!({
                "QueryLanguage": "JSONata",
                "StartAt": "C",
                "States": {
                    "C": {
                        "Type": "Choice",
                        "Choices": [{ "Condition": "{% true %}" }],
                        "Default": "Done",
                    },
                    "Done": { "Type": "Succeed" },
                },
            }),
            "/States/C/Choices[0]",
        ),
        (
            "a rule of a Choice state inside a Parallel branch",
            one_state(
                "P",
                parallel_state(json!({
                    "StartAt": "C",
                    "States": {
                        "C": {
                            "Type": "Choice",
                            "Choices": [{ "Variable": "$.x", "IsString": true }],
                            "Default": "Done",
                        },
                        "Done": { "Type": "Succeed" },
                    },
                })),
            ),
            "/States/P/Branches[0]/States/C/Choices[0]",
        ),
        (
            "a rule of a Choice state inside a Map state's processor",
            one_state(
                "M",
                json!({
                    "Type": "Map",
                    "ItemProcessor": {
                        "StartAt": "C",
                        "States": {
                            "C": {
                                "Type": "Choice",
                                "Choices": [{ "Variable": "$.x", "IsNumeric": true }],
                                "Default": "Done",
                            },
                            "Done": { "Type": "Succeed" },
                        },
                    },
                    "End": true,
                }),
            ),
            "/States/M/ItemProcessor/States/C/Choices[0]",
        ),
    ];

    for (index, (case, definition, pointer)) in rows.iter().enumerate() {
        let expected = [
            subject(
                SCHEMA_VALIDATION_FAILED,
                "These fields are required: [Next]",
                pointer,
            ),
            subject(
                MISSING_TRANSITION_TARGET,
                "Missing transition pointer",
                &format!("{pointer}/Next"),
            ),
        ]
        .join(", ");
        let stated = refused(&emulator, case, &format!("choice-rule-{index}"), definition).await;
        assert_eq!(
            stated, expected,
            "case {case:?}: expected the subjects {expected:?}, got {stated:?}"
        );
        fails_validation(&emulator, case, definition).await;
    }

    // The complement: a rule that DOES name a transition is admitted, however its
    // conditions are written.
    let rows: Vec<(&str, Value)> = vec![
        (
            "a rule naming the state the execution goes to",
            json!({
                "StartAt": "C",
                "States": {
                    "C": {
                        "Type": "Choice",
                        "Choices": [{ "Variable": "$.x", "BooleanEquals": true, "Next": "Done" }],
                        "Default": "Done",
                    },
                    "Done": { "Type": "Succeed" },
                },
            }),
        ),
        (
            "a rule whose conditions nest under And, Or and Not",
            json!({
                "StartAt": "C",
                "States": {
                    "C": {
                        "Type": "Choice",
                        "Choices": [{
                            "Or": [
                                { "Variable": "$.a", "IsPresent": true },
                                {
                                    "And": [
                                        { "Variable": "$.b", "IsString": true },
                                        { "Not": { "Variable": "$.c", "IsNull": true } },
                                    ],
                                },
                            ],
                            "Next": "Done",
                        }],
                        "Default": "Done",
                    },
                    "Done": { "Type": "Succeed" },
                },
            }),
        ),
        (
            "every rule of a two-rule Choice naming one",
            json!({
                "StartAt": "C",
                "States": {
                    "C": {
                        "Type": "Choice",
                        "Choices": [
                            { "Variable": "$.a", "StringEquals": "x", "Next": "Done" },
                            { "Variable": "$.b", "IsPresent": true, "Next": "Done" },
                        ],
                        "Default": "Done",
                    },
                    "Done": { "Type": "Succeed" },
                },
            }),
        ),
    ];
    for (index, (case, definition)) in rows.iter().enumerate() {
        admitted(&emulator, case, &format!("choice-rule-ok-{index}"), definition).await;
    }
}

// ---------------------------------------------------------------------------
// Rule 2: the version of the language a state machine object declares
// ---------------------------------------------------------------------------

/// A state machine object declaring a `Version` the Amazon States Language does not
/// define is refused where the definition is submitted, stating the versions it defines
/// at the member that declared one.
///
/// The one version the language defines is 1.0 — the language specification's "State
/// Machine Structure" declares `Version` as "the version of the States language used in
/// the state machine (default is \"1.0\")" — so the rows span values on either side of
/// it, a value of another JSON type, and the nested state machine objects a definition
/// declares beside its own.
#[tokio::test]
async fn a_state_machine_object_declares_a_version_the_language_defines() {
    let emulator = Emulator::new();

    /// The sentence the refusal states, naming the versions the language defines.
    const ONE_OF: &str = "Value should be one of the following: [1.0]";

    let stops = json!({ "StartAt": "A", "States": { "A": { "Type": "Succeed" } } });

    // (case, the definition, the pointer the version is reported at)
    let rows: Vec<(&str, Value, &str)> = vec![
        (
            "a version past the one the language defines",
            json!({
                "Version": "2.0",
                "StartAt": "A",
                "States": { "A": { "Type": "Succeed" } },
            }),
            "/Version",
        ),
        (
            "a revision of the version the language defines",
            json!({
                "Version": "1.1",
                "StartAt": "A",
                "States": { "A": { "Type": "Succeed" } },
            }),
            "/Version",
        ),
        (
            "a version below the one the language defines",
            json!({
                "Version": "0.9",
                "StartAt": "A",
                "States": { "A": { "Type": "Succeed" } },
            }),
            "/Version",
        ),
        (
            "the version written without its fraction",
            json!({
                "Version": "1",
                "StartAt": "A",
                "States": { "A": { "Type": "Succeed" } },
            }),
            "/Version",
        ),
        (
            "an empty version",
            json!({
                "Version": "",
                "StartAt": "A",
                "States": { "A": { "Type": "Succeed" } },
            }),
            "/Version",
        ),
        (
            "the version written as a number rather than as the string it is",
            json!({
                "Version": 1.0,
                "StartAt": "A",
                "States": { "A": { "Type": "Succeed" } },
            }),
            "/Version",
        ),
        (
            "a version declared by a Parallel state's branch",
            one_state(
                "P",
                parallel_state(json!({
                    "Version": "2.0",
                    "StartAt": "B",
                    "States": { "B": { "Type": "Succeed" } },
                })),
            ),
            "/States/P/Branches[0]/Version",
        ),
        (
            "a version declared by a Map state's processor",
            one_state(
                "M",
                json!({
                    "Type": "Map",
                    "ItemProcessor": {
                        "Version": "2.0",
                        "StartAt": "I",
                        "States": { "I": { "Type": "Succeed" } },
                    },
                    "End": true,
                }),
            ),
            "/States/M/ItemProcessor/Version",
        ),
    ];

    for (index, (case, definition, pointer)) in rows.iter().enumerate() {
        let expected = subject(SCHEMA_VALIDATION_FAILED, ONE_OF, pointer);
        let stated = refused(&emulator, case, &format!("version-{index}"), definition).await;
        assert_eq!(
            stated, expected,
            "case {case:?}: expected the subject {expected:?}, got {stated:?}"
        );
        fails_validation(&emulator, case, definition).await;
    }

    // The complement: the version the language defines, and the definition that
    // declares none and is written in it by default.
    let rows: Vec<(&str, Value)> = vec![
        (
            "the version the language defines",
            json!({
                "Version": "1.0",
                "StartAt": "A",
                "States": { "A": { "Type": "Succeed" } },
            }),
        ),
        ("no version at all", stops.clone()),
        (
            "the version the language defines, declared by a branch",
            one_state(
                "P",
                parallel_state(json!({
                    "Version": "1.0",
                    "StartAt": "B",
                    "States": { "B": { "Type": "Succeed" } },
                })),
            ),
        ),
    ];
    for (index, (case, definition)) in rows.iter().enumerate() {
        admitted(&emulator, case, &format!("version-ok-{index}"), definition).await;
    }
}

// ---------------------------------------------------------------------------
// Rule 3: a window the language documents as a positive number of seconds
// ---------------------------------------------------------------------------

/// A WINDOW the language documents as a positive, non-zero number of seconds admits no
/// value below one second, stating that bound at the figure itself — while the figures
/// whose documented default IS none keep admitting none.
///
/// The windows are the ones the Amazon States Language declares: a `Task` state's
/// `TimeoutSeconds` ("if the task runs longer than the specified seconds, this state
/// fails") and `HeartbeatSeconds` ("if more time than the specified seconds elapses
/// between heartbeats from the task, this state fails"), and a state machine object's own
/// `TimeoutSeconds` ("the maximum number of seconds an execution of the state machine can
/// run"). The rows probe each at the bound (admitted), at the value one below it and at a
/// value further below, and in a nested state machine object as well as in the
/// definition's own.
#[tokio::test]
async fn a_window_of_seconds_admits_nothing_below_one_second() {
    let emulator = Emulator::new();

    /// The bound such a window states, as the language writes it.
    const MINIMUM: &str = "Minimum value is 1";

    // (case, the definition, the pointer the bound is reported at)
    let rows: Vec<(&str, Value, &str)> = vec![
        (
            "a Task state's TimeoutSeconds of no seconds at all",
            one_state("T", task_state(json!({ "TimeoutSeconds": 0 }))),
            "/States/T/TimeoutSeconds",
        ),
        (
            "a Task state's TimeoutSeconds below none at all",
            one_state("T", task_state(json!({ "TimeoutSeconds": -5 }))),
            "/States/T/TimeoutSeconds",
        ),
        (
            "a Task state's HeartbeatSeconds of no seconds at all",
            one_state("T", task_state(json!({ "HeartbeatSeconds": 0 }))),
            "/States/T/HeartbeatSeconds",
        ),
        (
            "a Task state's HeartbeatSeconds below none at all",
            one_state("T", task_state(json!({ "HeartbeatSeconds": -1 }))),
            "/States/T/HeartbeatSeconds",
        ),
        (
            "the state machine object's own TimeoutSeconds of no seconds at all",
            json!({
                "TimeoutSeconds": 0,
                "StartAt": "A",
                "States": { "A": { "Type": "Succeed" } },
            }),
            "/TimeoutSeconds",
        ),
        (
            "a Parallel branch's own TimeoutSeconds of no seconds at all",
            one_state(
                "P",
                parallel_state(json!({
                    "TimeoutSeconds": 0,
                    "StartAt": "B",
                    "States": { "B": { "Type": "Succeed" } },
                })),
            ),
            "/States/P/Branches[0]/TimeoutSeconds",
        ),
        (
            "a Task state's TimeoutSeconds inside a Map state's processor",
            one_state(
                "M",
                json!({
                    "Type": "Map",
                    "ItemProcessor": {
                        "StartAt": "T",
                        "States": { "T": task_state(json!({ "TimeoutSeconds": 0 })) },
                    },
                    "End": true,
                }),
            ),
            "/States/M/ItemProcessor/States/T/TimeoutSeconds",
        ),
    ];

    for (index, (case, definition, pointer)) in rows.iter().enumerate() {
        let expected = subject(SCHEMA_VALIDATION_FAILED, MINIMUM, pointer);
        let stated = refused(&emulator, case, &format!("window-{index}"), definition).await;
        assert_eq!(
            stated, expected,
            "case {case:?}: expected the subject {expected:?}, got {stated:?}"
        );
        fails_validation(&emulator, case, definition).await;
    }

    // The complement: the bound itself, a window well past it, and the figures the
    // language documents a default of NONE for, which keep admitting none.
    let rows: Vec<(&str, Value)> = vec![
        (
            "a Task window of the shortest the language admits",
            one_state(
                "T",
                task_state(json!({ "TimeoutSeconds": 1, "HeartbeatSeconds": 1 })),
            ),
        ),
        (
            "a Task window well past the bound",
            one_state(
                "T",
                task_state(json!({ "TimeoutSeconds": 3600, "HeartbeatSeconds": 60 })),
            ),
        ),
        (
            "a state machine timeout of the shortest the language admits",
            json!({
                "TimeoutSeconds": 1,
                "StartAt": "A",
                "States": { "A": { "Type": "Succeed" } },
            }),
        ),
        (
            // "MaxConcurrency: its default value is 0, which places no limit on
            // concurrency", and "MaxAttempts: a value of 0 means the error is never
            // retried": none is the documented default of each, so none is admitted.
            "a Map concurrency and a retrier attempt count of none at all",
            one_state(
                "M",
                map_state(json!({
                    "MaxConcurrency": 0,
                    "Retry": [{ "ErrorEquals": ["States.ALL"], "MaxAttempts": 0 }],
                })),
            ),
        ),
        (
            // "Seconds: a time, in seconds, to wait before beginning the state
            // specified in the Next field" — a wait of none is a wait the language
            // admits.
            "a Wait of no seconds at all",
            one_state("W", json!({ "Type": "Wait", "Seconds": 0, "End": true })),
        ),
    ];
    for (index, (case, definition)) in rows.iter().enumerate() {
        admitted(&emulator, case, &format!("window-ok-{index}"), definition).await;
    }
}

// ---------------------------------------------------------------------------
// Rule 4: a fractional figure states its bounds as decimals
// ---------------------------------------------------------------------------

/// A `Map` state's `ToleratedFailurePercentage` is bounded at both ends of the
/// percentage range, and each end is stated as the decimal the language writes it as.
///
/// "ToleratedFailurePercentage: the percentage of failed items" — a percentage, so the
/// range is 0.0 to 100.0. The rows probe each end at the bound (admitted), just past it
/// and far past it, so what is asserted is the bound rather than the sign of a number.
#[tokio::test]
async fn a_percentage_states_each_end_of_its_range_as_a_decimal() {
    let emulator = Emulator::new();

    // (case, the percentage written, the bound stated)
    let rows: Vec<(&str, Value, &str)> = vec![
        ("just past the top of the range", json!(100.1), "Maximum value is 100.0"),
        ("one whole point past the top", json!(101), "Maximum value is 100.0"),
        ("far past the top", json!(1000), "Maximum value is 100.0"),
        ("just below the bottom", json!(-0.1), "Minimum value is 0.0"),
        ("far below the bottom", json!(-50), "Minimum value is 0.0"),
    ];

    for (index, (case, percentage, bound)) in rows.iter().enumerate() {
        let definition = one_state(
            "M",
            map_state(json!({ "ToleratedFailurePercentage": percentage })),
        );
        let expected = subject(
            SCHEMA_VALIDATION_FAILED,
            bound,
            "/States/M/ToleratedFailurePercentage",
        );
        let stated = refused(&emulator, case, &format!("percentage-{index}"), &definition).await;
        assert_eq!(
            stated, expected,
            "case {case:?}: expected the subject {expected:?}, got {stated:?}"
        );
        fails_validation(&emulator, case, &definition).await;
    }

    for (index, (case, percentage)) in [
        ("the bottom of the range", json!(0)),
        ("a fraction inside the range", json!(0.5)),
        ("half the range", json!(50)),
        ("the top of the range", json!(100)),
    ]
    .iter()
    .enumerate()
    {
        let definition = one_state(
            "M",
            map_state(json!({ "ToleratedFailurePercentage": percentage })),
        );
        admitted(
            &emulator,
            case,
            &format!("percentage-ok-{index}"),
            &definition,
        )
        .await;
    }
}

// ---------------------------------------------------------------------------
// Rule 5: a field the state's type does not declare is reported once
// ---------------------------------------------------------------------------

/// A state carrying a field the type its `Type` names does not declare is reported
/// ONCE — by the closed field set of that type — whichever type and field it is, and an
/// update refused for it leaves the stored definition and the revision as they were.
///
/// The rows differ along the type whose set does not declare the field (the two types
/// that END an execution, which declare no transition at all, and `Choice`, which goes
/// where its own rules send it) and along the state's name.
#[tokio::test]
async fn a_field_a_state_type_does_not_declare_is_reported_once() {
    let emulator = Emulator::new();

    // (case, the definition, the state the field was written on, the field)
    let rows: Vec<(&str, Value, &str, &str)> = vec![
        (
            "a Succeed state carrying Next",
            json!({
                "StartAt": "A",
                "States": {
                    "A": { "Type": "Succeed", "Next": "B" },
                    "B": { "Type": "Pass", "End": true },
                },
            }),
            "A",
            "Next",
        ),
        (
            "a Fail state carrying Next",
            json!({
                "StartAt": "Stop",
                "States": {
                    "Stop": { "Type": "Fail", "Error": "Stopped", "Next": "B" },
                    "B": { "Type": "Pass", "End": true },
                },
            }),
            "Stop",
            "Next",
        ),
        (
            "a Choice state carrying Next",
            json!({
                "StartAt": "C",
                "States": {
                    "C": {
                        "Type": "Choice",
                        "Choices": [{ "Variable": "$.x", "IsPresent": true, "Next": "B" }],
                        "Default": "B",
                        "Next": "B",
                    },
                    "B": { "Type": "Pass", "End": true },
                },
            }),
            "C",
            "Next",
        ),
        (
            "a Succeed state carrying End",
            json!({
                "StartAt": "A",
                "States": { "A": { "Type": "Succeed", "End": true } },
            }),
            "A",
            "End",
        ),
    ];

    for (index, (case, definition, state, field)) in rows.iter().enumerate() {
        let expected = subject(
            SCHEMA_VALIDATION_FAILED,
            &format!("Field '{field}' is not supported"),
            &format!("/States/{state}"),
        );
        let stated = refused(&emulator, case, &format!("one-finding-{index}"), definition).await;
        assert_eq!(
            stated, expected,
            "case {case:?}: expected exactly the subject {expected:?}, got {stated:?}"
        );
        fails_validation(&emulator, case, definition).await;
    }

    // The same walk judges an UPDATE, and a refused update changes nothing: the stored
    // definition and the revision it was on are what a later describe reports.
    let arn = create_machine(&emulator, "kept-on-refusal", HELLO_DEFINITION).await;
    let before = emulator
        .ok("DescribeStateMachine", &json!({ "stateMachineArn": arn }))
        .await;
    for (index, (case, definition, state, field)) in rows.iter().enumerate() {
        let (status, body) = emulator
            .call(
                "UpdateStateMachine",
                &json!({
                    "stateMachineArn": arn,
                    "definition": definition.to_string(),
                }),
            )
            .await;
        let code = error_code(&body);
        assert_eq!(
            (status, code.as_str()),
            (400, "InvalidDefinition"),
            "case {case:?}: expected the update to be refused, got {status} {code} with {body:?}"
        );
        let expected = format!(
            "{INVALID_STATE_MACHINE_DEFINITION}: '{}'",
            subject(
                SCHEMA_VALIDATION_FAILED,
                &format!("Field '{field}' is not supported"),
                &format!("/States/{state}"),
            )
        );
        let message = string(&body, "message");
        assert_eq!(
            message, expected,
            "case {case:?}: expected the update refusal {expected:?}, got {message:?}"
        );
        let after = emulator
            .ok("DescribeStateMachine", &json!({ "stateMachineArn": arn }))
            .await;
        assert_eq!(
            string(&after, "definition"),
            string(&before, "definition"),
            "case {case:?} (row {index}): a refused update kept the definition it was \
             handed, got {after:?}"
        );
        assert_eq!(
            after.get("revisionId"),
            before.get("revisionId"),
            "case {case:?} (row {index}): a refused update minted a revision, got {after:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 6: a workflow with no terminal state sits nowhere in the document
// ---------------------------------------------------------------------------

/// A state machine declaring no state that ends an execution is reported with no
/// position in the document — the refusal states its place as the bare word `null` and
/// the validation reports the diagnostic with no `location` at all — however deeply the
/// machine missing one is nested.
///
/// The rows differ along WHERE the machine that cannot stop sits: the definition itself,
/// a `Parallel` branch, a `Map` state's processor, and a branch nested inside a branch.
#[tokio::test]
async fn a_workflow_with_no_terminal_state_is_reported_nowhere_in_the_document() {
    let emulator = Emulator::new();

    let endless = json!({ "StartAt": "B", "States": { "B": { "Type": "Pass", "Next": "B" } } });

    // (case, the definition)
    let rows: Vec<(&str, Value)> = vec![
        (
            "the definition itself declares nothing that ends an execution",
            json!({
                "StartAt": "A",
                "States": { "A": { "Type": "Pass", "Next": "A" } },
            }),
        ),
        (
            "a Parallel branch declares nothing that ends an execution",
            one_state("P", parallel_state(endless.clone())),
        ),
        (
            "a Map state's processor declares nothing that ends an execution",
            one_state(
                "M",
                json!({
                    "Type": "Map",
                    "ItemProcessor": endless.clone(),
                    "End": true,
                }),
            ),
        ),
        (
            "a branch of a Parallel state inside a Parallel branch",
            one_state(
                "P",
                parallel_state(json!({
                    "StartAt": "Q",
                    "States": { "Q": parallel_state(endless.clone()) },
                })),
            ),
        ),
    ];

    for (index, (case, definition)) in rows.iter().enumerate() {
        let expected = subject(
            MISSING_END_STATE,
            "Workflow has no terminal state",
            NO_LOCATION,
        );
        let stated = refused(&emulator, case, &format!("endless-{index}"), definition).await;
        assert_eq!(
            stated, expected,
            "case {case:?}: expected the subject {expected:?}, got {stated:?}"
        );
        let validated = fails_validation(&emulator, case, definition).await;
        let found: Vec<Value> = diagnostics(&validated)
            .into_iter()
            .filter(|diagnostic| {
                diagnostic.get("code").and_then(Value::as_str) == Some(MISSING_END_STATE)
            })
            .collect();
        assert_eq!(
            found.len(),
            1,
            "case {case:?}: expected one {MISSING_END_STATE} diagnostic, got {validated:?}"
        );
        assert!(
            found[0].get("location").is_none(),
            "case {case:?}: expected the diagnostic to carry no location, got {:?}",
            found[0]
        );
    }
}
