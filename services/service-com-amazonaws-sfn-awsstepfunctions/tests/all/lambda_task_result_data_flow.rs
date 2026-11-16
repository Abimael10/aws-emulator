// SPDX-License-Identifier: CC0-1.0

//! What a `Task` state whose work is a LAMBDA FUNCTION reports as its RESULT, and what
//! the state's result-side data flow then does with it, driven over both services' own
//! wire protocols through the production `MultiServiceRouter` with Step Functions and
//! Lambda mounted on ONE engine — reached through
//! [`crate::lambda_task_control_flow::Chain`], so the function this suite created is the
//! one the task really invokes and the result asserted here is that run's own.
//!
//! RULE: a `Task` state whose `Resource` names a Lambda function INVOKES it with the
//! state's effective input as the function's event, and binds what the function RETURNED
//! as the state's result — the returned document itself where the `Resource` is the
//! function's own ARN, and the invocation's SDK response, carrying that document under
//! its payload member, where the `Resource` is the optimized `lambda:invoke` integration.
//! The state's `ResultPath` then places that result the way the Amazon States Language's
//! documented input/output processing places any state's result: a reference path MERGES
//! it into the state's raw input at the place that path names, creating the members it
//! names and overwriting only the one it ends at, rather than replacing the document;
//! `null` discards the result and keeps the raw input; and no `ResultPath` at all takes
//! the default of `$`, whose place is the whole document. The execution then reaches
//! `SUCCEEDED` and `DescribeExecution` reports that document as its `output`, with no
//! `error` and no `cause` — the two members a failed execution carries.
//!
//! Both services have to agree for that to hold — Step Functions builds the invocation
//! and places its answer, Lambda runs the handler and answers what the run produced — so
//! each row asserts the WHOLE output document, which carries the event the function was
//! handed inside the value it returned: a row therefore pins the passthrough of the
//! execution's own input into the invocation as well as the placement of the answer.
//!
//! What this suite's own axis is, beside the suites that already drive a Lambda task:
//! [`crate::lambda_task_control_flow`] pins what the DEFINITION decides around such a
//! task, [`crate::lambda_task_alias_version_and_payload`] which invocation the optimized
//! integration's `Parameters` can name, and
//! [`crate::eventbridge_delivered_lambda_task`] that a delivered execution's task binds
//! the run's answer at all; this one is the RESULT-SIDE DATA FLOW — where the answer
//! lands when the state names a place for it.
//!
//! The rows differ along what the rule generalises over: the form the `Resource` names
//! the work in (the function's own ARN, the optimized integration token), the kind of
//! place the `ResultPath` names (a top-level member, a nested one the input does not
//! carry, a bracketed member the input already carries, the default `$`, and `null`), and
//! the JSON type the function returned (an object, an array, a number, the empty object).

use serde_json::{json, Value};

use crate::lambda_task_control_flow::Chain;

/// The `Resource` of the OPTIMIZED Lambda integration, whose request is the `Parameters`
/// the state builds and whose result is the invocation's own SDK response.
const OPTIMIZED_RESOURCE: &str = "arn:aws:states:::lambda:invoke";

/// The terminal status an execution whose every state ended reaches
/// (`ExecutionStatus`: `SUCCEEDED`).
const SUCCEEDED: &str = "SUCCEEDED";

/// The member the optimized integration's result carries the function's own returned
/// document under (`Invoke`'s `Payload`), the status member beside it, and the envelope
/// member the SDK reports the call's HTTP facts in.
const PAYLOAD_MEMBER: &str = "Payload";
/// See [`PAYLOAD_MEMBER`].
const STATUS_CODE_MEMBER: &str = "StatusCode";
/// See [`PAYLOAD_MEMBER`].
const SDK_HTTP_METADATA: &str = "SdkHttpMetadata";
/// See [`PAYLOAD_MEMBER`].
const HTTP_STATUS_CODE_MEMBER: &str = "HttpStatusCode";

/// The status a served `RequestResponse` invocation answers
/// (`Invoke`'s `StatusCode`: "the HTTP status code is in the 200 range for a successful
/// request. For the `RequestResponse` invocation type, this status code is 200").
const SERVED_STATUS: u64 = 200;

/// The function every row invokes: it returns a value whose shape the EVENT decides, and
/// the object form carries the event back inside it — so what the invocation was handed
/// is readable in the state's own result rather than assumed here.
///
/// [`returned`] is this function in Rust, so a row's expectation is the value this
/// handler produces for that row's event and nothing is hard-coded per row.
const SHAPING_HANDLER: &str = r#"
def handler(event, context):
    shape = event.get("shape", "object")
    if shape == "list":
        return [event.get("tenant"), 42]
    if shape == "scalar":
        return 42
    if shape == "empty":
        return {}
    return {"computed": 42, "note": "done", "saw": event}
"#;

/// How long one invocation is given, as the function's own `Timeout`: well past what the
/// handler above needs on any host, because what is under test is where the answer lands
/// and not how fast an interpreter gets through it.
const FUNCTION_TIMEOUT_SECONDS: u64 = 120;

/// What [`SHAPING_HANDLER`] returns for `event`, as the handler computes it.
fn returned(event: &Value) -> Value {
    match event.get("shape").and_then(Value::as_str).unwrap_or("object") {
        "list" => json!([event.get("tenant").cloned().unwrap_or(Value::Null), 42]),
        "scalar" => json!(42),
        "empty" => json!({}),
        _ => json!({ "computed": 42, "note": "done", "saw": event }),
    }
}

/// Where a row's `ResultPath` places the task's result.
#[derive(Clone, Copy, Debug)]
enum Placement {
    /// A reference path: the path as the definition writes it, and the chain of members
    /// it names, which is where the result is merged into the raw input.
    At(&'static str, &'static [&'static str]),
    /// No `ResultPath` at all, whose default of `$` names the whole document.
    Default,
    /// `ResultPath` of `null`, which discards the result.
    Discarded,
}

impl Placement {
    /// The document the state reports once `result` has been placed into `input`.
    fn applied(self, input: &Value, result: &Value) -> Value {
        match self {
            Self::At(_, members) => merged(input, members, result),
            Self::Default => result.clone(),
            Self::Discarded => input.clone(),
        }
    }

    /// Assert the reference path a row WRITES names exactly the chain of members the
    /// expectation is computed over, so the two halves of [`Self::At`] cannot drift into
    /// asserting one place while the definition names another.
    ///
    /// The two forms the Amazon States Language writes a reference path's steps in are
    /// the dotted one and the bracketed one, so either spelling of the chain is the path.
    fn assert_consistent(self, case: &str) {
        let Self::At(path, members) = self else {
            return;
        };
        let dotted = format!("$.{}", members.join("."));
        let bracketed = members
            .iter()
            .fold("$".to_string(), |path, member| {
                format!("{path}['{member}']")
            });
        assert!(
            path == dotted || path == bracketed,
            "case {case:?}: the ResultPath {path:?} must name {members:?}, which is spelled \
             {dotted:?} or {bracketed:?}"
        );
    }
}

/// `result` merged into `document` at the place `members` names: every member of
/// `document` is kept, the members `members` names are created where the document does
/// not carry them, and only the one the path ends at is given the result.
fn merged(document: &Value, members: &[&str], result: &Value) -> Value {
    let (member, rest) = members
        .split_first()
        .expect("a reference path names at least one member");
    let mut object = document.as_object().cloned().unwrap_or_default();
    let placed = if rest.is_empty() {
        result.clone()
    } else {
        merged(
            &object.get(*member).cloned().unwrap_or_else(|| json!({})),
            rest,
            result,
        )
    };
    object.insert((*member).to_string(), placed);
    Value::Object(object)
}

/// The value `document` carries at the place `members` names, or `None` where any member
/// of the chain is missing.
fn at<'a>(document: &'a Value, members: &[&str]) -> Option<&'a Value> {
    members
        .iter()
        .try_fold(document, |carried, member| carried.get(*member))
}

/// A one-`Task` definition whose work is the function `resource` names, with the
/// `ResultPath` `placement` asks for.
fn task_definition(resource: &str, parameters: Option<Value>, placement: Placement) -> Value {
    let mut task = json!({ "Type": "Task", "Resource": resource, "End": true });
    if let Some(parameters) = parameters {
        task["Parameters"] = parameters;
    }
    match placement {
        Placement::At(path, _) => task["ResultPath"] = json!(path),
        Placement::Discarded => task["ResultPath"] = Value::Null,
        Placement::Default => {}
    }
    json!({ "StartAt": "Work", "States": { "Work": task } })
}

/// The document an execution's `output` carries.
fn output_of(case: &str, described: &Value) -> Value {
    let text = described
        .get("output")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: a succeeded execution reports an output, got {described}")
        });
    serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("case {case:?}: the output is a JSON document, got {error} in {text:?}")
    })
}

/// Assert the execution SUCCEEDED carrying neither of the members a failed execution
/// carries.
fn assert_succeeded(case: &str, described: &Value) {
    assert_eq!(
        described.get("status").and_then(Value::as_str),
        Some(SUCCEEDED),
        "case {case:?}: expected status {SUCCEEDED}, got {described}"
    );
    assert!(
        described.get("error").is_none(),
        "case {case:?}: expected no error, got {described}"
    );
    assert!(
        described.get("cause").is_none(),
        "case {case:?}: expected no cause, got {described}"
    );
}

/// One row of either table: the input the execution starts with and where the state's
/// `ResultPath` places the task's result.
struct Row {
    case: &'static str,
    input: Value,
    placement: Placement,
}

/// The direct-ARN table: every place a `ResultPath` can name, over every JSON type the
/// function returns.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a reference path merges the result beside the input's own members",
            input: json!({ "tenant": "acme", "retries": 0 }),
            placement: Placement::At("$.result", &["result"]),
        },
        Row {
            case: "a nested reference path creates every member it names",
            input: json!({ "tenant": "beta" }),
            placement: Placement::At("$.outcome.lambda", &["outcome", "lambda"]),
        },
        Row {
            case: "a bracketed reference path overwrites only the member it names",
            input: json!({ "tenant": "gamma", "result": "stale" }),
            placement: Placement::At("$['result']", &["result"]),
        },
        Row {
            case: "no ResultPath at all replaces the document with the result",
            input: json!({ "tenant": "delta", "nested": { "keep": [1, 2] } }),
            placement: Placement::Default,
        },
        Row {
            case: "a null ResultPath discards the result and keeps the raw input",
            input: json!({ "tenant": "epsilon", "nested": { "keep": [1, 2] } }),
            placement: Placement::Discarded,
        },
        Row {
            case: "an array result is merged at the reference path as it stands",
            input: json!({ "tenant": "zeta", "shape": "list" }),
            placement: Placement::At("$.result", &["result"]),
        },
        Row {
            case: "a number result is merged at the reference path as it stands",
            input: json!({ "tenant": "eta", "shape": "scalar" }),
            placement: Placement::At("$.result", &["result"]),
        },
        Row {
            case: "an empty object result is merged into an input carrying its own nesting",
            input: json!({ "tenant": "theta", "shape": "empty", "nested": { "keep": [1, 2] } }),
            placement: Placement::At("$.outcome.lambda", &["outcome", "lambda"]),
        },
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_function_arn_task_binds_the_run_and_the_result_path_merges_it() {
    let chain = Chain::new();
    let function_arn = chain
        .given_function_of(
            "result-shaper",
            SHAPING_HANDLER,
            Some(FUNCTION_TIMEOUT_SECONDS),
        )
        .await;
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        row.placement.assert_consistent(case);
        let definition = task_definition(&function_arn, None, row.placement);
        let described = chain
            .ran(case, &format!("direct-{index}"), &definition, &row.input)
            .await;
        assert_succeeded(case, &described);
        let output = output_of(case, &described);
        let expected = row.placement.applied(&row.input, &returned(&row.input));
        assert_eq!(
            output, expected,
            "case {case:?}: expected the output {expected}, got {output}"
        );
    }
}

/// The optimized-integration table: the same placements over the integration whose
/// result is the invocation's SDK response rather than the function's document.
fn envelope_rows() -> Vec<Row> {
    vec![
        Row {
            case: "the envelope merged at a reference path beside the input's members",
            input: json!({ "tenant": "iota", "retries": 2 }),
            placement: Placement::At("$.result", &["result"]),
        },
        Row {
            case: "the envelope as the whole output, under the default place",
            input: json!({ "tenant": "kappa" }),
            placement: Placement::Default,
        },
        Row {
            case: "the envelope of an empty-object run merged at a nested reference path",
            input: json!({ "tenant": "mu", "shape": "empty" }),
            placement: Placement::At("$.outcome.lambda", &["outcome", "lambda"]),
        },
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_optimized_integration_reports_the_run_under_its_payload_member() {
    let chain = Chain::new();
    let function_arn = chain
        .given_function_of(
            "envelope-shaper",
            SHAPING_HANDLER,
            Some(FUNCTION_TIMEOUT_SECONDS),
        )
        .await;
    for (index, row) in envelope_rows().into_iter().enumerate() {
        let case = row.case;
        row.placement.assert_consistent(case);
        // `Payload.$` of `$` hands the state's whole effective input to the function, so
        // the event the run sees is the execution's own input for this form too.
        let parameters = json!({ "FunctionName": function_arn, "Payload.$": "$" });
        let definition = task_definition(OPTIMIZED_RESOURCE, Some(parameters), row.placement);
        let described = chain
            .ran(case, &format!("optimized-{index}"), &definition, &row.input)
            .await;
        assert_succeeded(case, &described);
        let output = output_of(case, &described);
        let envelope = match row.placement {
            Placement::At(_, members) => at(&output, members).unwrap_or_else(|| {
                panic!("case {case:?}: expected the result at {members:?}, got {output}")
            }),
            Placement::Default => &output,
            // The result IS the envelope for this table's rule, so a row that discards it
            // has nothing to read it from: such a row belongs to the direct-ARN table,
            // whose rows assert the whole document instead.
            Placement::Discarded => panic!(
                "case {case:?}: a discarded result reports no envelope to read, so this table \
                 states no expectation for it"
            ),
        };
        assert_eq!(
            envelope.get(PAYLOAD_MEMBER),
            Some(&returned(&row.input)),
            "case {case:?}: expected the run's own document under {PAYLOAD_MEMBER}, got {envelope}"
        );
        assert_eq!(
            envelope.get(STATUS_CODE_MEMBER).and_then(Value::as_u64),
            Some(SERVED_STATUS),
            "case {case:?}: expected {STATUS_CODE_MEMBER} {SERVED_STATUS}, got {envelope}"
        );
        assert_eq!(
            envelope
                .get(SDK_HTTP_METADATA)
                .and_then(|http| http.get(HTTP_STATUS_CODE_MEMBER))
                .and_then(Value::as_u64),
            Some(SERVED_STATUS),
            "case {case:?}: expected the envelope to report HTTP {SERVED_STATUS}, got {envelope}"
        );
        // The merge keeps the input: every member the execution started with is still
        // reported, unchanged, beside the place the result landed in.
        if let Placement::At(_, members) = row.placement {
            let input = row
                .input
                .as_object()
                .expect("the row's input is a JSON object");
            for (member, value) in input {
                if member == members[0] {
                    continue;
                }
                assert_eq!(
                    output.get(member),
                    Some(value),
                    "case {case:?}: expected the input's {member:?} to survive the merge, got \
                     {output}"
                );
            }
        }
    }
}
