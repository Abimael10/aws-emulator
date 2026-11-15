// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the two member families a `StartSyncExecution` response is
//! built from, driven over this service's own wire protocol (`awsJson1_0`, `POST /`
//! with `X-Amz-Target: AWSStepFunctions.StartSyncExecution`) through the production
//! `MultiServiceRouter`, so what is asserted is what a signed AWS SDK request observes.
//!
//! The rules under test:
//!
//! 1. the DATA-DETAILS members (`inputDetails`, `outputDetails`) report whether this
//!    call carried the run's data, which is a property of the CALL and not of one
//!    value: both are sent with `included` true on every response that reports the
//!    run's data — including a run whose `output` is absent because it did not succeed
//!    ("This field is set only if the execution succeeds. If the execution fails, this
//!    field is null") — and neither is sent by a `METADATA_ONLY` call, which reports
//!    none of that data at all;
//! 2. a JSONPath written in a NAMED FIELD of a state that selects no value in the
//!    document it is resolved against fails the execution with that field's own
//!    diagnosis — the path, the field it was written for and the WHOLE document it was
//!    looked for in — under the state that was being executed, which is the form a
//!    `Fail` state's `ErrorPath`/`CausePath` is reported in just as a payload
//!    template's `"<name>.$"` member is.
//!
//! Each table spans one of those rules; the recorded synchronous run this suite's issue
//! was filed about is ONE row of each. Nothing here waits on the clock: a synchronous
//! start answers the terminal outcome in the call itself.
//!
//! The harness (`Emulator`, `string`) is the one `state_machine_lifecycle` builds and
//! `create_express` is `sync_execution`'s; `tests/all.rs` is this binary's crate root,
//! so both are reached through `crate::<suite>`.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::asl::RUNTIME_ERROR;

use crate::state_machine_lifecycle::{discriminated, string, Emulator};
use crate::sync_execution::create_express;

// ---------------------------------------------------------------------------
// inputDetails / outputDetails
// ---------------------------------------------------------------------------

/// What `CloudWatchEventsExecutionDataDetails` carries on an API response:
/// `included` is "Indicates whether input or output was included in the response.
/// Always true for API calls."
///
/// It goes on the wire with the type discriminator every nested structure of this API
/// carries (`tests/all/nested_type_discriminator.rs` is the rule for that).
fn data_details() -> Value {
    discriminated(
        "CloudWatchEventsExecutionDataDetails",
        json!({ "included": true }),
    )
}

/// One row of the data-details table: a run, and whether it ends with an `output`.
struct DetailsCase {
    case: &'static str,
    definition: &'static str,
    /// The `input` the request carries, or `None` to send none at all.
    input: Option<&'static str>,
    /// The `SyncExecutionStatus` the run must end in.
    status: &'static str,
    /// Whether the response carries an `output` member at all, which is what the
    /// details members must NOT depend on.
    has_output: bool,
}

fn details_cases() -> Vec<DetailsCase> {
    vec![
        DetailsCase {
            case: "a run that succeeds with an object output",
            definition: r#"{"StartAt":"P","States":{"P":{"Type":"Pass","Result":{"ok":true},"End":true}}}"#,
            input: Some(r#"{"n":1}"#),
            status: "SUCCEEDED",
            has_output: true,
        },
        DetailsCase {
            case: "a run that succeeds with a string output and no request input",
            definition: r#"{"StartAt":"P","States":{"P":{"Type":"Pass","Result":"done","End":true}}}"#,
            input: None,
            status: "SUCCEEDED",
            has_output: true,
        },
        DetailsCase {
            case: "a run that succeeds carrying its own input through",
            definition: r#"{"StartAt":"P","States":{"P":{"Type":"Pass","End":true}}}"#,
            input: Some(r#"[1,2,3]"#),
            status: "SUCCEEDED",
            has_output: true,
        },
        DetailsCase {
            case: "a run a Fail state ends, so there is no output",
            definition: r#"{"StartAt":"F","States":{"F":{"Type":"Fail","Error":"ConfigError","Cause":"the definition said so"}}}"#,
            input: Some(r#"{"field":"targetBucket"}"#),
            status: "FAILED",
            has_output: false,
        },
        DetailsCase {
            case: "a run the interpreter ends with a runtime error, so there is no output",
            definition: r#"{"StartAt":"P","States":{"P":{"Type":"Pass","OutputPath":"$.absent","End":true}}}"#,
            input: Some(r#"{"present":true}"#),
            status: "FAILED",
            has_output: false,
        },
    ]
}

/// Both details members are reported with `included` true on every response that
/// carries the run's data, whether or not the member each describes carries a value.
///
/// Rows differ along the status the run ends in, whether an `output` was produced at
/// all, the shape of the data (an object, a string, an array) and whether the request
/// carried an `input` of its own.
#[tokio::test]
async fn the_data_details_members_are_reported_whatever_the_run_produced() {
    let emulator = Emulator::new();
    for (index, case) in details_cases().iter().enumerate() {
        let label = case.case;
        let machine = create_express(&emulator, &format!("details-{index}"), case.definition).await;
        let mut request = json!({ "stateMachineArn": machine, "name": "run" });
        if let Some(input) = case.input {
            request["input"] = Value::String(input.to_string());
        }
        let answered = emulator.ok("StartSyncExecution", &request).await;

        assert_eq!(
            answered.get("status").and_then(Value::as_str),
            Some(case.status),
            "case {label:?}: expected status {:?}, got {answered:?}",
            case.status
        );
        // The row's premise: whether this run produced an output at all.
        assert_eq!(
            answered
                .get("output")
                .is_some_and(|output| !output.is_null()),
            case.has_output,
            "case {label:?}: expected an output present {}, got {answered:?}",
            case.has_output
        );
        for member in ["inputDetails", "outputDetails"] {
            assert_eq!(
                answered.get(member).cloned(),
                Some(data_details()),
                "case {label:?}: expected {member} {} whatever the run produced, got {answered:?}",
                data_details()
            );
        }
    }
}

/// The complement that bounds the rule: a `METADATA_ONLY` call reports none of the
/// run's data, so it reports no details structure about data it did not report.
#[tokio::test]
async fn a_metadata_only_call_reports_neither_details_member() {
    let emulator = Emulator::new();
    for (index, case) in details_cases().iter().enumerate() {
        let label = case.case;
        let machine = create_express(&emulator, &format!("metadata-{index}"), case.definition)
            .await;
        let mut request = json!({
            "stateMachineArn": machine,
            "name": "run",
            "includedData": "METADATA_ONLY",
        });
        if let Some(input) = case.input {
            request["input"] = Value::String(input.to_string());
        }
        let answered = emulator.ok("StartSyncExecution", &request).await;

        assert_eq!(
            answered.get("status").and_then(Value::as_str),
            Some(case.status),
            "case {label:?}: expected status {:?}, got {answered:?}",
            case.status
        );
        for member in ["input", "inputDetails", "output", "outputDetails"] {
            assert!(
                answered.get(member).is_none(),
                "case {label:?}: a metadata-only call reports no {member:?}, got {answered:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// A named field whose JSONPath selects nothing
// ---------------------------------------------------------------------------

/// The name the failing `Fail` state carries in every row below.
const FAILING: &str = "Reject";

/// One row of the named-field table: a `Fail` state member carrying a path the state's
/// input does not resolve.
struct FieldCase {
    case: &'static str,
    /// The `Fail` state member the path is written in.
    field: &'static str,
    /// The path written in it.
    path: &'static str,
    /// The execution input, which carries no value for `path`.
    input: &'static str,
    /// The document the path was resolved against, as the response must quote it: the
    /// state's own input for a reference path, and the CONTEXT OBJECT for one that
    /// begins with `$$`, which is why that row quotes nothing of the input.
    searched: Option<&'static str>,
    /// How many `Pass` states run before the failing one, which is what moves the
    /// event id the cause names.
    before: usize,
    /// The other member of the pair, which the state declares as a literal so the
    /// definition names both an error and a cause.
    literal: (&'static str, &'static str),
}

fn field_cases() -> Vec<FieldCase> {
    vec![
        FieldCase {
            case: "an ErrorPath naming a member the input does not carry",
            field: "ErrorPath",
            path: "$.err",
            input: r#"{"field": "targetBucket"}"#,
            searched: Some(r#"{"field": "targetBucket"}"#),
            before: 0,
            literal: ("Cause", "the definition said so"),
        },
        FieldCase {
            case: "a CausePath naming a member the input does not carry",
            field: "CausePath",
            path: "$.why",
            input: r#"{"other":1}"#,
            searched: Some(r#"{"other":1}"#),
            before: 0,
            literal: ("Error", "Boom"),
        },
        FieldCase {
            case: "an ErrorPath in bracket notation, two Pass states into the run",
            field: "ErrorPath",
            path: "$['err']",
            input: r#"{"kept":[]}"#,
            searched: Some(r#"{"kept":[]}"#),
            before: 2,
            literal: ("Cause", "on purpose"),
        },
        FieldCase {
            case: "a CausePath into a nested object the input stops short of",
            field: "CausePath",
            path: "$.detail.why",
            input: r#"{"detail":{}}"#,
            searched: Some(r#"{"detail":{}}"#),
            before: 1,
            literal: ("Error", "Nested"),
        },
        FieldCase {
            case: "an ErrorPath at an array index the input stops short of",
            field: "ErrorPath",
            path: "$.errors[2]",
            input: r#"{"errors":["one","two"]}"#,
            searched: Some(r#"{"errors":["one","two"]}"#),
            before: 0,
            literal: ("Cause", "out of range"),
        },
        FieldCase {
            case: "a CausePath into the context object, resolved against that object",
            field: "CausePath",
            path: "$$.Map.Item.Value",
            input: r#"{"anything":true}"#,
            // A path that begins with `$$` is resolved against the context object, so
            // the document the diagnosis quotes is that object and not the input.
            searched: None,
            before: 0,
            literal: ("Error", "NoContext"),
        },
    ]
}

/// The definition one row runs: `before` `Pass` states, then the failing `Fail` state.
fn field_definition(case: &FieldCase) -> String {
    let mut states = serde_json::Map::new();
    let mut start_at = FAILING.to_string();
    for index in 0..case.before {
        let name = format!("Step{index}");
        let next = if index + 1 == case.before {
            FAILING.to_string()
        } else {
            format!("Step{}", index + 1)
        };
        if index == 0 {
            start_at = name.clone();
        }
        states.insert(name, json!({ "Type": "Pass", "Next": next }));
    }
    let (literal, value) = case.literal;
    let mut failing = serde_json::Map::new();
    failing.insert("Type".to_string(), json!("Fail"));
    failing.insert(literal.to_string(), json!(value));
    failing.insert(case.field.to_string(), json!(case.path));
    states.insert(FAILING.to_string(), Value::Object(failing));
    json!({ "StartAt": start_at, "States": Value::Object(states) }).to_string()
}

/// A `Fail` state's `*Path` that selected no value fails the run with the field's own
/// diagnosis, under the state that was being executed.
///
/// Rows differ along which member carries the path (`ErrorPath`, `CausePath`), the
/// path's own form (a member, a nested member, a bracketed member, an array index, a
/// path into the context object), the input it was looked for in, and the state's
/// POSITION in the definition — which is what makes the event id differ from row to row.
#[tokio::test]
async fn an_unresolved_field_path_names_the_field_and_the_document_it_searched() {
    let emulator = Emulator::new();
    for (index, case) in field_cases().iter().enumerate() {
        let label = case.case;
        let definition = field_definition(case);
        let machine = create_express(&emulator, &format!("field-{index}"), &definition).await;
        let answered = emulator
            .ok(
                "StartSyncExecution",
                &json!({
                    "stateMachineArn": machine,
                    "name": "run",
                    "input": case.input,
                }),
            )
            .await;

        assert_eq!(
            (
                answered.get("status").and_then(Value::as_str),
                answered.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(RUNTIME_ERROR)),
            "case {label:?}: expected FAILED with {RUNTIME_ERROR}, got {answered:?}"
        );

        // The history numbers its events from one: `ExecutionStarted` is #1 and every
        // state before the failing one contributes the two events it is entered and
        // left by, so the state is entered at #2 + 2 * `before`. That numbering is
        // read back out of `GetExecutionHistory` for a STANDARD run of the same shape
        // in `unresolved_path_cause`; here it is what the cause must name.
        let entered = 2 + 2 * i64::try_from(case.before).expect("the count fits");
        let expected = format!(
            "An error occurred while executing the state '{FAILING}' (entered at the event id \
             #{entered}). The JSONPath '{path}' specified for the field '{field}' could not be \
             found in the input '{searched}'",
            path = case.path,
            field = case.field,
            // The context object this emulator builds for the run carries no `Map`
            // member, so that row's quoted document is whatever the object itself is:
            // the assertion below holds the row to the sentence AROUND it.
            searched = case.searched.unwrap_or_default(),
        );
        let cause = string(&answered, "cause");
        match case.searched {
            Some(_) => assert_eq!(
                cause, expected,
                "case {label:?}: expected the cause {expected:?}, got {cause:?}"
            ),
            // A path into the context object is resolved against that object, whose
            // contents are the run's own, so the row requires the sentence up to the
            // document it quotes rather than the document itself.
            None => {
                let opening = expected
                    .strip_suffix("''")
                    .unwrap_or_else(|| panic!("case {label:?}: the sentence quotes a document"));
                assert!(
                    cause.starts_with(opening),
                    "case {label:?}: expected the cause to open {opening:?}, got {cause:?}"
                );
            }
        }
    }
}
