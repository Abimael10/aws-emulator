// SPDX-License-Identifier: CC0-1.0

//! What the Amazon States Language requires OF A STATE, and how the service reports a
//! state that does not carry it — driven end to end over this service's own wire
//! protocol (`awsJson1_0`, `POST /` with `X-Amz-Target: AWSStepFunctions.<Operation>`)
//! through the production router.
//!
//! The rules under test, each over a table whose rows differ along what the rule
//! generalises over rather than along the one definition that exposed it:
//!
//! 1. a state carries every field the language declares as REQUIRED for the type it
//!    names — `Type` itself, and then the field whose work that type does — and one
//!    that does not earns an ERROR `SCHEMA_VALIDATION_FAILED` diagnostic listing the
//!    fields it owes, located at the state itself — so the write path refuses the
//!    definition with `InvalidDefinition` rather than creating a state machine AWS
//!    would never accept;
//! 2. a refused write changes nothing: an `UpdateStateMachine` that carries such a
//!    definition leaves the stored definition and the revision exactly as they were;
//! 3. a diagnostic names its SUBJECT as the submitted definition declares it: the
//!    diagnostic about a state name past the language's bound quotes that name in
//!    full and locates it at the `/States/<StateName>` pointer that resolves it.
//!
//! The harness (`Emulator`, `create_machine`, `error_code`, `string`, `ROLE_ARN`,
//! `HELLO_DEFINITION`) is the one `state_machine_lifecycle` builds and the validation
//! readers are `definition_validation`'s; `tests/all/main.rs` is this binary's crate
//! root, so each is reached through `crate::<suite>`.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::asl::{
    state_fields, state_required_fields, INVALID_STATE_NAME, MAX_STATE_NAME_CHARS,
    SCHEMA_VALIDATION_FAILED, STATE_TYPES, STATE_TYPE_VALUES_MESSAGE,
};
use service_com_amazonaws_sfn_awsstepfunctions::state::{
    INVALID_STATE_MACHINE_DEFINITION, SEVERITY_ERROR, VALIDATION_RESULT_FAIL, VALIDATION_RESULT_OK,
};

use crate::definition_validation::{diagnostics, result_of, validate};
use crate::state_machine_lifecycle::{
    create_machine, error_code, string, Emulator, HELLO_DEFINITION, ROLE_ARN,
};

/// The name the state under test is declared under.
const STATE: &str = "Work";

/// The `Resource` a `Task` state under test names.
const RESOURCE: &str = "arn:aws:states:::aws-sdk:sts:getCallerIdentity";

/// One subject of an `InvalidDefinition` message, which is also one diagnostic:
/// the code, the detail, and the place it was found.
fn subject(code: &str, message: &str, location: &str) -> String {
    format!("{code}: {message} at {location}")
}

/// The whole `InvalidDefinition` message a definition carrying the one subject
/// `subject` is refused with.
fn refusal(subject: &str) -> String {
    format!("{INVALID_STATE_MACHINE_DEFINITION}: '{subject}'")
}

/// The sentence a schema diagnostic states the fields an object owes with.
fn required(fields: &str) -> String {
    format!("These fields are required: [{fields}]")
}

/// What `CreateStateMachine` answers for `definition`, as `(status, code, message)`.
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

/// The diagnostics a validation of `definition` reported, as
/// `(severity, code, message, location)` tuples in the order it reported them.
async fn reported(emulator: &Emulator, case: &str, definition: &str) -> Vec<Value> {
    let body = validate(emulator, case, &json!({ "definition": definition })).await;
    assert_eq!(
        result_of(&body),
        VALIDATION_RESULT_FAIL,
        "case {case:?}: a definition carrying a schema error fails verification, got {body:?}"
    );
    diagnostics(&body)
}

/// One ERROR diagnostic as the wire reports it.
fn error_at(code: &str, message: &str, location: &str) -> Value {
    json!({
        "severity": SEVERITY_ERROR,
        "code": code,
        "message": message,
        "location": location,
    })
}

// ---------------------------------------------------------------------------
// Rule 1: a state owes the fields its type declares
// ---------------------------------------------------------------------------

/// One state that does not carry a field its type requires, wherever it is declared.
struct Row {
    case: &'static str,
    /// The definition, whose ONE problem is the missing field.
    definition: String,
    /// The field the state owes, as the diagnostic lists it.
    field: &'static str,
    /// The pointer the diagnostic is located at.
    location: String,
    /// The same state with the field it owes, which must be created.
    complete: String,
}

/// A state machine whose one state under test is `state`.
fn machine(state: Value) -> String {
    json!({ "StartAt": STATE, "States": { STATE: state } }).to_string()
}

/// A state machine whose `Map` state runs `state` as its item processor.
fn inside_a_processor(state: Value) -> String {
    json!({
        "StartAt": "M",
        "States": {
            "M": {
                "Type": "Map",
                "ItemProcessor": { "StartAt": STATE, "States": { STATE: state } },
                "End": true,
            },
        },
    })
    .to_string()
}

/// A state machine whose `Parallel` state runs `state` as its one branch.
fn inside_a_branch(state: Value) -> String {
    json!({
        "StartAt": "P",
        "States": {
            "P": {
                "Type": "Parallel",
                "Branches": [{ "StartAt": STATE, "States": { STATE: state } }],
                "End": true,
            },
        },
    })
    .to_string()
}

/// Every state type the language declares a required field for, with and without that
/// field, at the three positions a state can be declared at.
///
/// The rows differ along WHICH type is at stake and so which field it owes, along
/// whether the requirement is satisfied by the field the language names now or by the
/// earlier spelling of the same work (a `Map`'s `Iterator`), and along WHERE the state
/// is declared — the definition's own `States`, a `Map` state's `ItemProcessor` and a
/// `Parallel` state's branch — which is what the reported pointer follows.
fn rows() -> Vec<Row> {
    // Each state is complete but for the field under test, so the definition's one
    // problem is that field: it is reachable from `StartAt` and it ends the execution.
    let task = json!({ "Type": "Task", "End": true });
    let parallel = json!({ "Type": "Parallel", "End": true });
    let map = json!({ "Type": "Map", "End": true });
    // A `Choice` goes where its rules send it, so the state it defaults to is
    // declared beside it and ends the execution.
    let choice = json!({ "StartAt": STATE, "States": {
        STATE: { "Type": "Choice", "Default": "Done" },
        "Done": { "Type": "Succeed" },
    }})
    .to_string();
    let complete_choice = json!({ "StartAt": STATE, "States": {
        STATE: {
            "Type": "Choice",
            "Choices": [{ "Variable": "$.x", "NumericEquals": 1, "Next": "Done" }],
            "Default": "Done",
        },
        "Done": { "Type": "Succeed" },
    }})
    .to_string();

    vec![
        Row {
            case: "a Task state that names no Resource",
            definition: machine(task.clone()),
            field: "Resource",
            location: format!("/States/{STATE}"),
            complete: machine(json!({ "Type": "Task", "Resource": RESOURCE, "End": true })),
        },
        Row {
            case: "a Parallel state that declares no Branches",
            definition: machine(parallel.clone()),
            field: "Branches",
            location: format!("/States/{STATE}"),
            complete: machine(json!({
                "Type": "Parallel",
                "Branches": [{ "StartAt": "B", "States": { "B": { "Type": "Pass", "End": true } } }],
                "End": true,
            })),
        },
        Row {
            case: "a Map state that declares no ItemProcessor",
            definition: machine(map.clone()),
            field: "ItemProcessor",
            location: format!("/States/{STATE}"),
            complete: machine(json!({
                "Type": "Map",
                "ItemProcessor": { "StartAt": "I", "States": { "I": { "Type": "Pass", "End": true } } },
                "End": true,
            })),
        },
        Row {
            // The requirement is satisfied by either spelling of the same work, so
            // the earlier one carries a Map state that owes nothing.
            case: "a Map state whose processor is the language's earlier Iterator",
            definition: machine(map.clone()),
            field: "ItemProcessor",
            location: format!("/States/{STATE}"),
            complete: machine(json!({
                "Type": "Map",
                "Iterator": { "StartAt": "I", "States": { "I": { "Type": "Pass", "End": true } } },
                "End": true,
            })),
        },
        Row {
            case: "a Choice state that declares no Choices",
            definition: choice,
            field: "Choices",
            location: format!("/States/{STATE}"),
            complete: complete_choice,
        },
        Row {
            case: "a Task state declared inside a Map state's ItemProcessor",
            definition: inside_a_processor(task.clone()),
            field: "Resource",
            location: format!("/States/M/ItemProcessor/States/{STATE}"),
            complete: inside_a_processor(
                json!({ "Type": "Task", "Resource": RESOURCE, "End": true }),
            ),
        },
        Row {
            case: "a Parallel state declared inside a Parallel state's branch",
            definition: inside_a_branch(parallel),
            field: "Branches",
            location: format!("/States/P/Branches[0]/States/{STATE}"),
            complete: inside_a_branch(json!({
                "Type": "Parallel",
                "Branches": [{ "StartAt": "B", "States": { "B": { "Type": "Pass", "End": true } } }],
                "End": true,
            })),
        },
    ]
}

/// A state missing the field its type requires is reported at the state, fails
/// verification, and is refused by `CreateStateMachine` with that one subject — while
/// the same state carrying the field is created.
#[tokio::test]
async fn a_state_missing_the_field_its_type_requires_is_refused() {
    let emulator = Emulator::new();

    for (index, row) in rows().into_iter().enumerate() {
        let Row {
            case,
            definition,
            field,
            location,
            complete,
        } = row;
        let expected = error_at(SCHEMA_VALIDATION_FAILED, &required(field), &location);
        let found = reported(&emulator, case, &definition).await;
        assert!(
            found.contains(&expected),
            "case {case:?}: expected the diagnostic {expected:?}, got {found:?}"
        );

        // What a create refuses the definition for is what the validation reports
        // about it, stated as the one subject of the refusal.
        let (status, code, message) =
            created(&emulator, &format!("required-{index}"), &definition).await;
        assert_eq!(
            (status, code.as_str()),
            (400, "InvalidDefinition"),
            "case {case:?}: expected 400 InvalidDefinition, got {status} {code} with message \
             {message:?}"
        );
        assert_eq!(
            message,
            refusal(&subject(
                SCHEMA_VALIDATION_FAILED,
                &required(field),
                &location
            )),
            "case {case:?}: expected the refusal to state the missing field, got {message:?}"
        );

        // The control: the same state carrying what it owes is created, so the
        // refusal above is about the missing field and nothing else.
        let body = validate(
            &emulator,
            case,
            &json!({ "definition": &complete }),
        )
        .await;
        assert_eq!(
            result_of(&body),
            VALIDATION_RESULT_OK,
            "case {case:?}: the state carrying the field it owes verifies, got {body:?}"
        );
        create_machine(&emulator, &format!("complete-{index}"), &complete).await;
    }
}

/// An `UpdateStateMachine` carrying such a definition is refused in the same words,
/// and leaves the stored definition and the revision exactly as they were.
///
/// The rows differ along which type's missing field refuses the update and along
/// whether a successful update has already minted a revision, which is what a refusal
/// must not move.
#[tokio::test]
async fn an_update_refused_for_a_missing_field_stores_nothing() {
    let emulator = Emulator::new();
    let machine_arn = create_machine(&emulator, "keeps-its-definition", HELLO_DEFINITION).await;

    let describe = json!({ "stateMachineArn": &machine_arn });
    let before = emulator.ok("DescribeStateMachine", &describe).await;
    assert_eq!(
        before.get("definition").and_then(Value::as_str),
        Some(HELLO_DEFINITION),
        "expected the created definition to be the stored one, got {before:?}"
    );
    assert!(
        before.get("revisionId").is_none_or(Value::is_null),
        "expected no revisionId before any update, got {before:?}"
    );

    // (case, the state the definition declares, the definition the update carries,
    //  the field that state owes)
    let refused: Vec<(&str, &str, String, &str)> = vec![
        (
            "a Parallel state that declares no Branches",
            "FanOut",
            json!({
                "StartAt": "FanOut",
                "States": { "FanOut": { "Type": "Parallel", "End": true } },
            })
            .to_string(),
            "Branches",
        ),
        (
            "a Task state that names no Resource",
            "Invoke",
            json!({
                "StartAt": "Invoke",
                "States": { "Invoke": { "Type": "Task", "End": true } },
            })
            .to_string(),
            "Resource",
        ),
    ];

    // The definition and the revision each read answers with, which a refused update
    // may not move.
    let mut stored = HELLO_DEFINITION.to_string();
    let mut revision: Option<String> = None;

    for (case, state, definition, field) in &refused {
        let (status, body) = emulator
            .call(
                "UpdateStateMachine",
                &json!({ "stateMachineArn": &machine_arn, "definition": definition }),
            )
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidDefinition"),
            "case {case:?}: expected 400 InvalidDefinition, got {status} with {body:?}"
        );
        assert_eq!(
            body.get("message").and_then(Value::as_str),
            Some(
                refusal(&subject(
                    SCHEMA_VALIDATION_FAILED,
                    &required(field),
                    &format!("/States/{state}")
                ))
                .as_str()
            ),
            "case {case:?}: expected the refusal to state the missing field, got {body:?}"
        );

        let described = emulator.ok("DescribeStateMachine", &describe).await;
        assert_eq!(
            described.get("definition").and_then(Value::as_str),
            Some(stored.as_str()),
            "case {case:?}: a refused update leaves the stored definition, got {described:?}"
        );
        assert_eq!(
            described
                .get("revisionId")
                .and_then(Value::as_str)
                .map(str::to_string),
            revision,
            "case {case:?}: a refused update mints no revision, got {described:?}"
        );

        // A successful update between the rows: the next refusal must leave THIS
        // definition and THIS revision in place, so the rule holds for a state
        // machine that has a revision as well as for one that has none.
        let accepted = json!({
            "StartAt": "Ok",
            "States": { "Ok": { "Type": "Pass", "Result": { "case": case }, "End": true } },
        })
        .to_string();
        let updated = emulator
            .ok(
                "UpdateStateMachine",
                &json!({ "stateMachineArn": &machine_arn, "definition": &accepted }),
            )
            .await;
        let minted = string(&updated, "revisionId");
        assert_ne!(
            Some(minted.clone()),
            revision,
            "case {case:?}: an applied update mints a revision of its own, got {minted:?} again"
        );
        stored = accepted;
        revision = Some(minted);
    }
}

/// `Type` is the field EVERY state owes, reported like every other field an object
/// owes — and a `Type` whose value is not one of the types the language defines is
/// reported at the member, naming the values it admits.
///
/// The rows differ along whether `Type` is absent, is a string outside the catalog
/// (a name the language has no state for, the wrong case, a defined name with
/// whitespace) or is a value of another JSON type altogether, and along where the
/// state is declared.
#[tokio::test]
async fn a_state_owes_a_type_the_language_defines() {
    let emulator = Emulator::new();

    // (case, the state under test, the pointer the diagnostic is located at, the
    //  message it carries)
    let rows: Vec<(&str, Value, String, String)> = vec![
        (
            "a state with no Type at all",
            json!({ "End": true }),
            format!("/States/{STATE}"),
            required("Type"),
        ),
        (
            "a Type the language has no state for",
            json!({ "Type": "Teleport", "End": true }),
            format!("/States/{STATE}/Type"),
            STATE_TYPE_VALUES_MESSAGE.to_string(),
        ),
        (
            "a defined type in the wrong case",
            json!({ "Type": "pass", "End": true }),
            format!("/States/{STATE}/Type"),
            STATE_TYPE_VALUES_MESSAGE.to_string(),
        ),
        (
            "a defined type with whitespace around it",
            json!({ "Type": " Pass", "End": true }),
            format!("/States/{STATE}/Type"),
            STATE_TYPE_VALUES_MESSAGE.to_string(),
        ),
        (
            // `Type` selects what the state does, so a value of any other JSON type
            // is outside the set it selects from like any unknown name is.
            "a Type that is not a string",
            json!({ "Type": 7, "End": true }),
            format!("/States/{STATE}/Type"),
            STATE_TYPE_VALUES_MESSAGE.to_string(),
        ),
        (
            "a state with no Type inside a Map state's ItemProcessor",
            json!({ "End": true }),
            format!("/States/M/ItemProcessor/States/{STATE}"),
            required("Type"),
        ),
    ];

    for (index, (case, state, location, message)) in rows.into_iter().enumerate() {
        let definition = if location.starts_with("/States/M/") {
            inside_a_processor(state)
        } else {
            machine(state)
        };
        let expected = error_at(SCHEMA_VALIDATION_FAILED, &message, &location);
        let found = reported(&emulator, case, &definition).await;
        assert!(
            found.contains(&expected),
            "case {case:?}: expected the diagnostic {expected:?}, got {found:?}"
        );

        let (status, code, answered) =
            created(&emulator, &format!("typed-{index}"), &definition).await;
        assert_eq!(
            (status, code.as_str()),
            (400, "InvalidDefinition"),
            "case {case:?}: expected 400 InvalidDefinition, got {status} {code} with message \
             {answered:?}"
        );
        assert!(
            answered.contains(&subject(SCHEMA_VALIDATION_FAILED, &message, &location)),
            "case {case:?}: expected the refusal to state the diagnostic, got {answered:?}"
        );
    }
}

/// The catalog itself: the types the language requires a field of are the ones whose
/// work ONE field declares, and every field named is one the type's own closed set
/// declares.
///
/// The source is the Amazon States Language's own per-type structure, read through
/// this crate's tables (`…::state::asl::state_required_fields` against
/// `…::state::asl::state_fields`), so the rule is pinned over every type the language
/// defines rather than over the one the issue cited.
#[tokio::test]
async fn the_required_fields_of_a_type_are_fields_that_type_declares() {
    for state_type in STATE_TYPES {
        for alternatives in state_required_fields(state_type) {
            assert!(
                !alternatives.is_empty(),
                "case {state_type:?}: a requirement names at least one field, got none"
            );
            for field in *alternatives {
                assert!(
                    state_fields(state_type).contains(field),
                    "case {state_type:?}: the required field {field:?} must be one the type \
                     declares, got {:?}",
                    state_fields(state_type)
                );
            }
        }
    }

    // The types whose work one field declares, and the ones that declare theirs
    // through fields none of which they must carry alone.
    let owing: Vec<&str> = STATE_TYPES
        .iter()
        .copied()
        .filter(|state_type| !state_required_fields(state_type).is_empty())
        .collect();
    assert_eq!(
        owing,
        vec!["Choice", "Map", "Parallel", "Task"],
        "case the catalog: expected the four types whose work one field declares, got {owing:?}"
    );
    // A type the language does not define requires nothing, because nothing says what
    // such a state would do.
    assert!(
        state_required_fields("Teleport").is_empty(),
        "case an undefined type: expected no requirement, got {:?}",
        state_required_fields("Teleport")
    );
}

// ---------------------------------------------------------------------------
// Rule 3: a diagnostic names its subject as the definition declares it
// ---------------------------------------------------------------------------

/// What the service answers for a state name past the language's bound: the name,
/// quoted in full.
fn too_long(name: &str) -> String {
    format!(
        "Invalid State name: State '{name}' exceeds the {MAX_STATE_NAME_CHARS}-character limit \
         allowed by the service"
    )
}

/// The diagnostic about a state name past the bound quotes the name in full and
/// locates it at the pointer that resolves that state in the submitted definition.
///
/// The rows differ along how far past the bound the name is, along whether its
/// characters are one byte each or several, and along where the state is declared —
/// which is what the reported pointer follows. The last row is the bound itself,
/// which earns no diagnostic at all.
#[tokio::test]
async fn the_over_long_state_name_diagnostic_names_the_state_in_full() {
    let emulator = Emulator::new();

    // (case, the state name, the pointer prefix the machine declaring it sits at,
    //  whether the name is past the bound)
    let rows: Vec<(&str, String, &str, bool)> = vec![
        (
            "a name one character past the bound",
            "n".repeat(MAX_STATE_NAME_CHARS + 1),
            "",
            true,
        ),
        (
            "a name well past the bound",
            "w".repeat(MAX_STATE_NAME_CHARS + 40),
            "",
            true,
        ),
        (
            // The bound is in CHARACTERS, so a name of multi-byte characters is past
            // it at the same count.
            "a multi-byte name one character past the bound",
            "é".repeat(MAX_STATE_NAME_CHARS + 1),
            "",
            true,
        ),
        (
            "a nested name one character past the bound",
            "i".repeat(MAX_STATE_NAME_CHARS + 1),
            "/States/M/ItemProcessor",
            true,
        ),
        (
            "a name at the bound",
            "b".repeat(MAX_STATE_NAME_CHARS),
            "",
            false,
        ),
    ];

    for (index, (case, name, prefix, past)) in rows.into_iter().enumerate() {
        let state = json!({ "StartAt": &name, "States": { &name: { "Type": "Succeed" } } });
        let definition = if prefix.is_empty() {
            state.to_string()
        } else {
            json!({
                "StartAt": "M",
                "States": { "M": { "Type": "Map", "ItemProcessor": state, "End": true } },
            })
            .to_string()
        };
        let location = format!("{prefix}/States/{name}");
        let expected = error_at(INVALID_STATE_NAME, &too_long(&name), &location);

        if !past {
            let body = validate(&emulator, case, &json!({ "definition": &definition })).await;
            assert_eq!(
                result_of(&body),
                VALIDATION_RESULT_OK,
                "case {case:?}: a name at the bound verifies, got {body:?}"
            );
            create_machine(&emulator, &format!("named-{index}"), &definition).await;
            continue;
        }

        let found = reported(&emulator, case, &definition).await;
        assert!(
            found.contains(&expected),
            "case {case:?}: expected the diagnostic {expected:?}, got {found:?}"
        );

        // The pointer carries the name as the definition declares it: a client reads
        // it to find the offending state in the document it submitted.
        let locations: Vec<String> = found
            .iter()
            .filter_map(|diagnostic| diagnostic.get("location"))
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        assert!(
            locations.contains(&location),
            "case {case:?}: expected a diagnostic located at {location:?}, got {locations:?}"
        );
        for reported in &locations {
            assert!(
                !reported.contains('\u{2026}'),
                "case {case:?}: expected no shortened pointer, got {reported:?}"
            );
        }

        // And the refusal of a create states that same diagnostic.
        let (status, code, message) =
            created(&emulator, &format!("named-{index}"), &definition).await;
        assert_eq!(
            (status, code.as_str()),
            (400, "InvalidDefinition"),
            "case {case:?}: expected 400 InvalidDefinition, got {status} {code} with message \
             {message:?}"
        );
        assert_eq!(
            message,
            refusal(&subject(INVALID_STATE_NAME, &too_long(&name), &location)),
            "case {case:?}: expected the refusal to quote the state name, got {message:?}"
        );
    }
}
