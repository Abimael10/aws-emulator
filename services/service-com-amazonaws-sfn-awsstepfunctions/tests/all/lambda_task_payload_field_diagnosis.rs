// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the DIAGNOSIS an execution reports when the optimized Lambda
//! integration (`arn:aws:states:::lambda:invoke`) could not build its invocation out of
//! the `Payload` its `Parameters` resolved to, driven over both services' own wire
//! protocols through the production `MultiServiceRouter` with Step Functions and Lambda
//! mounted on ONE engine — so a row whose parameters DO name an invocation really
//! invokes the function this suite created.
//!
//! The rule under test: the diagnosis of a parameter the invoked operation cannot be
//! called with names the FIELD that is unusable and the TYPE that field requires — "The
//! Parameters '<resolved parameters>' could not be used to start the Task: [The value for
//! the field '<field>' must be a <TYPE>]" — rather than the JSON value one run happened
//! to resolve it to. `Invoke`'s `Payload` targets a BLOB and is the operation's
//! `httpPayload` (`smithy_query(query="operation",operation="Invoke",model_path=
//! "services/service-com-amazonaws-lambda-awsgirapiservice/smithy-model/
//! com-amazonaws-lambda-awsgirapiservice.json")`), so the type that field requires is a
//! STRING: a parameter resolved to a JSON string IS that text, and one resolved to an
//! object or an array is the document the invocation is made with, while any other bare
//! JSON value is neither and names no invocation. Such a task fails on its own parameters
//! with the Amazon States Language's runtime error, and the state's own failure event
//! reports that same refusal, with no invocation handed to Lambda's own family of events.
//!
//! The rows differ along what the rule generalises over: the JSON type the `Payload`
//! parameter is given (object, array, string, number, boolean), the value inside that
//! type (a whole number, a negative fraction, zero, both booleans, the empty string),
//! whether a path or the definition itself gave the parameter, the null document, and
//! parameters that name no `Payload` at all.
//!
//! No figure of the cause is written into a row: the event id it names is read out of
//! `GetExecutionHistory` and the resolved parameters are rendered from the row's own
//! value, so the two readings of one execution cannot drift.
//!
//! The harness (`Chain`, `given_function_of`, `ran`) is the one
//! `lambda_task_control_flow` builds for the same two services, the history reader is
//! `task_work_event_families`' and the type list is `lambda_function_timeout`'s;
//! `tests/all/main.rs` is this binary's crate root, so each is reached through
//! `crate::<suite>`.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::asl::RUNTIME_ERROR;

use crate::lambda_function_timeout::event_types;
use crate::lambda_task_control_flow::Chain;
use crate::task_work_event_families::history;

/// The `Resource` of the OPTIMIZED Lambda integration, whose request IS the `Parameters`
/// the state built.
const OPTIMIZED_RESOURCE: &str = "arn:aws:states:::lambda:invoke";

/// The one state every row runs.
const STATE: &str = "Work";

/// The event one `Task` state is entered by, whose id the cause names.
const ENTERED_EVENT: &str = "TaskStateEntered";

/// The event the optimized integration's unit of work — one API call — is handed over
/// by, which a row whose parameters DO name an invocation writes.
const SCHEDULED_EVENT: &str = "TaskScheduled";

/// The event the state's own failure is reported by, and the detail member it carries.
const FAILED_EVENT: &str = "TaskFailed";

/// See [`FAILED_EVENT`].
const FAILED_EVENT_DETAILS: &str = "taskFailedEventDetails";

/// The hand-over event of the OTHER family, the one an invocation of a function is
/// recorded under: no invocation was built, so nothing of this family is written.
const INVOCATION_SCHEDULED_EVENT: &str = "LambdaFunctionScheduled";

/// The field the diagnosis names, as `Invoke`'s model spells the member.
const FIELD: &str = "Payload";

/// The type that field requires, as the diagnosis names it: `Payload` targets a BLOB, so
/// the value the invocation carries for it is text.
const FIELD_TYPE: &str = "STRING";

/// The handler each row invokes: it echoes the event it was handed, so what a usable
/// `Payload` resolved to is readable in the task's own result.
const ECHO_SOURCE: &str = r#"
def handler(event, context):
    return {"echo": event}
"#;

/// One row: how the definition writes the `Payload` parameter, the execution input a
/// path resolves it against, and what the parameter resolves to.
struct Row {
    case: &'static str,
    /// The `Payload` member as the definition writes it: `"Payload.$": "$"` for a row
    /// whose value comes out of the execution input, `"Payload": <value>` for one the
    /// definition wrote itself, and nothing at all for a row that names no payload.
    written: Value,
    /// The execution input, which is a JSON text of its own — what `StartExecution`
    /// takes.
    input: Value,
    /// What the parameter resolved to, which the rendered parameters carry; `None` for a
    /// row whose parameters name no `Payload`.
    resolved: Option<Value>,
    /// Whether these parameters name an invocation the call can be built from.
    usable: bool,
}

fn rows() -> Vec<Row> {
    let from_input = json!({ "Payload.$": "$" });
    vec![
        Row {
            case: "an object the path selected",
            written: from_input.clone(),
            input: json!({ "n": 1 }),
            resolved: Some(json!({ "n": 1 })),
            usable: true,
        },
        Row {
            case: "an array the path selected",
            written: from_input.clone(),
            input: json!([3, "two", { "k": null }]),
            resolved: Some(json!([3, "two", { "k": null }])),
            usable: true,
        },
        Row {
            // A JSON string IS the text the blob member carries, so it names an
            // invocation exactly as a document does.
            case: "a string the path selected",
            written: from_input.clone(),
            input: json!("text"),
            resolved: Some(json!("text")),
            usable: true,
        },
        Row {
            case: "the empty string",
            written: from_input.clone(),
            input: json!(""),
            resolved: Some(json!("")),
            usable: true,
        },
        Row {
            case: "a whole number the path selected",
            written: from_input.clone(),
            input: json!(42),
            resolved: Some(json!(42)),
            usable: false,
        },
        Row {
            case: "a negative fractional number",
            written: from_input.clone(),
            input: json!(-1.5),
            resolved: Some(json!(-1.5)),
            usable: false,
        },
        Row {
            case: "zero",
            written: from_input.clone(),
            input: json!(0),
            resolved: Some(json!(0)),
            usable: false,
        },
        Row {
            case: "a true boolean",
            written: from_input.clone(),
            input: json!(true),
            resolved: Some(json!(true)),
            usable: false,
        },
        Row {
            case: "a false boolean",
            written: from_input,
            input: json!(false),
            resolved: Some(json!(false)),
            usable: false,
        },
        Row {
            case: "a number the definition wrote itself",
            written: json!({ "Payload": 7 }),
            input: json!({ "n": 1 }),
            resolved: Some(json!(7)),
            usable: false,
        },
        Row {
            // The null document names no value, which is what parameters carrying no
            // payload member name: a function invoked with no event.
            case: "the null document the definition wrote",
            written: json!({ "Payload": Value::Null }),
            input: json!({ "n": 1 }),
            resolved: Some(Value::Null),
            usable: true,
        },
        Row {
            case: "no Payload parameter at all",
            written: json!({}),
            input: json!({ "n": 1 }),
            resolved: None,
            usable: true,
        },
    ]
}

/// The one-`Task` definition a row runs, whose work is the optimized integration with
/// `parameters`.
fn definition(parameters: Value) -> Value {
    json!({
        "StartAt": STATE,
        "States": {
            STATE: {
                "Type": "Task",
                "Resource": OPTIMIZED_RESOURCE,
                "Parameters": parameters,
                "End": true,
            },
        },
    })
}

/// The diagnosis the service words for parameters it could not start the task with: the
/// parameters AS RESOLVED, and then the field that could not be used with the type it
/// requires. This is what the state's own failure EVENT reports.
fn diagnosis(parameters: &Value) -> String {
    format!(
        "The Parameters '{parameters}' could not be used to start the Task: [The value for the \
         field '{FIELD}' must be a {FIELD_TYPE}]"
    )
}

/// The cause the EXECUTION reports for it, which words that diagnosis around the state
/// that failed and the id of the event it was entered at.
fn expected_cause(entered: i64, parameters: &Value) -> String {
    format!(
        "An error occurred while executing the state '{STATE}' (entered at the event id \
         #{entered}). {}",
        diagnosis(parameters)
    )
}

/// The one event of type `event_type` in `events`.
fn event<'a>(events: &'a [Value], case: &str, event_type: &str) -> &'a Value {
    let matching: Vec<&Value> = events
        .iter()
        .filter(|event| event.get("type").and_then(Value::as_str) == Some(event_type))
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "case {case:?}: expected exactly one {event_type}, got {:?}",
        event_types(events)
    );
    matching[0]
}

/// The id of the one event of type `event_type` in `events`.
fn event_id(events: &[Value], case: &str, event_type: &str) -> i64 {
    let found = event(events, case, event_type);
    found
        .get("id")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("case {case:?}: expected an id on {found}"))
}

/// The document an execution's `output` carries, for a row that reads the task's result.
fn output_of(case: &str, described: &Value) -> Value {
    serde_json::from_str(
        described
            .get("output")
            .and_then(Value::as_str)
            .unwrap_or("null"),
    )
    .unwrap_or_else(|error| panic!("case {case:?}: the output is a JSON document, got {error}"))
}

/// A `Payload` parameter the invocation cannot be built from fails the task with the
/// diagnosis naming that field and the type it requires, and hands nothing over.
#[tokio::test]
async fn an_unusable_payload_parameter_names_the_field_and_the_type_it_requires() {
    let chain = Chain::new();
    let name = "payload-field";
    let arn = chain.given_function_of(name, ECHO_SOURCE, None).await;

    for (index, row) in rows().into_iter().enumerate() {
        let Row {
            case,
            written,
            input,
            resolved,
            usable,
        } = row;
        // The parameters the definition carries, and the parameters they RESOLVE to,
        // which is what the diagnosis renders: both name the function the same way.
        let mut parameters = json!({ "FunctionName": arn });
        let mut rendered = json!({ "FunctionName": arn });
        for (member, value) in written.as_object().expect("each row names parameters") {
            parameters[member] = value.clone();
        }
        match &resolved {
            Some(value) => rendered[FIELD] = value.clone(),
            None => {
                rendered
                    .as_object_mut()
                    .expect("the rendered parameters are an object")
                    .remove(FIELD);
            }
        }

        let described = chain
            .ran(
                case,
                &format!("payload-field-{index}"),
                &definition(parameters),
                &input,
            )
            .await;
        let execution = described
            .get("executionArn")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected an executionArn in {described}"))
            .to_string();
        let events = history(&chain, case, &execution, json!({})).await;
        let types = event_types(&events);

        if usable {
            assert_eq!(
                described.get("status").and_then(Value::as_str),
                Some("SUCCEEDED"),
                "case {case:?}: these parameters name an invocation, got {described}"
            );
            // The work really was handed over, so the refusal above is the parameters'
            // own rather than something every row gets.
            assert!(
                types.iter().any(|event| event == SCHEDULED_EVENT),
                "case {case:?}: expected a {SCHEDULED_EVENT} event, got {types:?}"
            );
            // And what the parameter resolved to reached the function unchanged, for a
            // row that named a value at all: the handler echoes the event it was handed.
            if let Some(value) = resolved.filter(|value| !value.is_null()) {
                let output = output_of(case, &described);
                assert_eq!(
                    output.pointer("/Payload/echo"),
                    Some(&value),
                    "case {case:?}: expected {value} to reach the function unchanged, got {output}"
                );
            }
            continue;
        }

        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(RUNTIME_ERROR)),
            "case {case:?}: expected FAILED with {RUNTIME_ERROR}, got {described}"
        );
        let entered = event_id(&events, case, ENTERED_EVENT);
        let expected = expected_cause(entered, &rendered);
        let cause = described
            .get("cause")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            cause, expected,
            "case {case:?}: expected the cause {expected:?}, got {cause:?}"
        );
        // The history and the execution's own report cannot disagree about the refusal:
        // the state's failure event carries the diagnosis itself, which the execution's
        // cause words around the state it failed in.
        let details = event(&events, case, FAILED_EVENT)
            .get(FAILED_EVENT_DETAILS)
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected {FAILED_EVENT_DETAILS}"));
        let reported = diagnosis(&rendered);
        assert_eq!(
            (
                details.get("error").and_then(Value::as_str),
                details.get("cause").and_then(Value::as_str),
            ),
            (Some(RUNTIME_ERROR), Some(reported.as_str())),
            "case {case:?}: expected the history to report {reported:?}, got {details}"
        );
        // And no invocation was ever built, so nothing of the family an invocation of a
        // function is recorded under was written.
        assert!(
            !types.iter().any(|event| event == INVOCATION_SCHEDULED_EVENT),
            "case {case:?}: expected no {INVOCATION_SCHEDULED_EVENT} event, got {types:?}"
        );
    }
}
