// SPDX-License-Identifier: CC0-1.0

//! The two Automation step actions this emulator PERFORMS, end to end: the one Amazon Web
//! Services API call an `aws:executeAwsApi` step makes, the script an `aws:executeScript`
//! step runs, and the signals `SendAutomationSignal` applies to a waiting execution.
//!
//! Every case goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body, dispatched by the
//! multi-service router (`OPERATIONS.md`, "Wire protocol") — and the API calls the steps
//! make travel over the emulator's own in-process callback back into this same router, so a
//! step's effect is state a client reads back through Parameter Store's own operations.
//!
//! The rules under test:
//!
//! * an `aws:executeAwsApi` step CALLS the `Service`/`Api` it names with the step's other
//!   inputs as that API's parameters, extracts each declared `outputs[].Selector` out of the
//!   answer under its declared `Type`, and reaches `Success` — and the API's own refusal is
//!   what fails the step;
//! * an `aws:executeScript` step RUNS its `Script` in the declared `Runtime`, calls its
//!   `Handler` with the substituted `InputPayload`, and selects the handler's return value
//!   through `$.Payload…` into its declared outputs — and the RELEASE the interpreter serving
//!   it reports is the one that `Runtime` declares;
//! * a step's outputs are what the next step's `{{ <step>.<output> }}` references resolve
//!   to, and the document's own `outputs` project them as `"<step>.<output>"`;
//! * `SendAutomationSignal` resolves the execution the signal names and applies the signal
//!   to it, answering the modelled refusal for an execution it holds none of and for a step
//!   the runbook does not declare — and leaving the execution untouched either way. A
//!   `StopStep` names the step execution it stops by the `StepExecutionId` its payload
//!   carries, and an id in no step of that execution is refused the stop's own refusal.
//!
//! An automation is `Pending` until the service picks it up, so the cases move the clock to
//! that instant through the service's own state ([`pick_up`]) rather than sleeping for it,
//! and then read the execution until it has come to rest ([`at_rest`]) — the run a parked
//! step is owed is performed by whichever call finds it owed, so nothing here depends on how
//! fast the host runs.
//!
//! The suites are tables. Each row is one input along the axis the rule generalises over —
//! the API called, the selector, the declared output type, what the script computes, the
//! runtime it declares, the signal sent — rather than the single runbook the recorded units
//! happened to run.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::{automation, ComAmazonawsSsmAmazonssmState};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request signs for. Deliberately not the emulator's default, so a step's
/// own API call proves it was made in the Region its execution runs in.
const REGION: &str = "us-west-2";

/// The `X-Amz-Target` prefix from the crate's `OPERATIONS.md` wire-protocol section.
const TARGET_PREFIX: &str = "AmazonSSM";

/// A well-formed `AutomationExecutionId` (the modelled length is exactly 36) no execution
/// was ever minted under.
const UNKNOWN_EXECUTION_ID: &str = "11111111-2222-3333-4444-555555555555";

/// The `AutomationExecutionStatus` values an execution never leaves again, from
/// `smithy_query(query="shape",shape="AutomationExecutionStatus")` as the Automation slice
/// reads them (`automation::TERMINAL_STATUSES`).
fn terminal(status: &str) -> bool {
    automation::TERMINAL_STATUSES.contains(&status)
}

/// A router with just this service mounted, and the engine it was mounted on — the handle a
/// case moves the clock with.
fn service() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signatures are not what these tests are about; the router still reads the SigV4
    // credential scope below, which is where the Region comes from.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// Call `operation` signed for [`REGION`], returning `(status, body)`.
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20240101/{REGION}/ssm/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a valid test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let raw = response.into_body();
    let mut parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    if let (Some(error_type), Value::Object(map)) = (error_type, &mut parsed) {
        map.entry("__type")
            .or_insert_with(|| Value::String(error_type));
    }
    (status, parsed)
}

/// The wire error code of a failure body (`__type`, without its namespace).
fn error_code(body: &Value) -> String {
    let rendered = body["__type"].as_str().unwrap_or("<no __type>");
    rendered
        .split('#')
        .next_back()
        .unwrap_or(rendered)
        .to_string()
}

/// The refusal message of a failure body.
fn error_message(body: &Value) -> String {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or("<no message>")
        .to_string()
}

/// A string member of a response body.
fn text(body: &Value, member: &str) -> String {
    body[member].as_str().unwrap_or("<unset>").to_string()
}

/// Create an Automation runbook of `name` with `content`, asserting it succeeded.
async fn runbook(router: &MultiServiceRouter, name: &str, content: &Value) {
    let (status, created) = call(
        router,
        "CreateDocument",
        &json!({
            "Name": name,
            "DocumentType": "Automation",
            "Content": content.to_string(),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the runbook {name} must be created first, got {status} with {created:?}"
    );
}

/// Start an automation, asserting it succeeded, and answering the id it minted.
async fn started(router: &MultiServiceRouter, case: &str, body: &Value) -> String {
    let (status, response) = call(router, "StartAutomationExecution", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with {response:?}"
    );
    text(&response, "AutomationExecutionId")
}

/// Read one execution back, asserting the read succeeded.
async fn read(router: &MultiServiceRouter, case: &str, execution_id: &str) -> Value {
    let (status, response) = call(
        router,
        "GetAutomationExecution",
        &json!({"AutomationExecutionId": execution_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetAutomationExecution, got {status} with \
         {response:?}"
    );
    response["AutomationExecution"].clone()
}

/// How many reads an execution is given to come to rest.
///
/// Each read performs the runs the execution's parked steps are owed, so one read is enough
/// for every runbook here; this is generous headroom for a loaded host, and the loop waits
/// for the CONDITION — a terminal status — rather than for a span.
const READS_UNTIL_AT_REST: usize = 200;

/// Read `execution_id` until it has come to rest, and answer the execution it came to rest
/// as.
async fn at_rest(router: &MultiServiceRouter, case: &str, execution_id: &str) -> Value {
    let mut last = Value::Null;
    for _ in 0..READS_UNTIL_AT_REST {
        last = read(router, case, execution_id).await;
        if terminal(&text(&last, "AutomationExecutionStatus")) {
            return last;
        }
        ::tokio::task::yield_now().await;
    }
    panic!("case {case:?}: the automation never came to rest, last read {last}")
}

/// An instant well in the past, which is what the pickup is moved to below.
fn long_ago() -> SystemTime {
    SystemTime::now() - Duration::from_secs(3_600)
}

/// Let the clock reach the pickup every execution of [`REGION`] is waiting for.
///
/// The service owes a started automation ONE transition, a span after the start
/// (`automation::PICKUP_DELAY`); a case that asserts what the walk does moves the clock to it
/// through the store rather than sleeping for it.
fn pick_up(engine: &Arc<engine::InMemoryEngine>) {
    let state = engine.state::<ComAmazonawsSsmAmazonssmState>();
    let mut store = state.lock();
    for ((held, _), execution) in store.automation_executions.iter_mut() {
        if held == REGION && execution.runnable_at.is_some() {
            execution.runnable_at = Some(long_ago());
        }
    }
}

/// One step of an execution, by name.
fn step(execution: &Value, name: &str) -> Value {
    execution["StepExecutions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|step| text(step, "StepName") == name)
        .unwrap_or(Value::Null)
}

/// Put one parameter through the wire, so a step's API call reads state a client really
/// stored.
async fn put_parameter(router: &MultiServiceRouter, name: &str, value: &str) {
    let (status, stored) = call(
        router,
        "PutParameter",
        &json!({"Name": name, "Value": value, "Type": "String"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "the parameter {name} must be stored first, got {status} with {stored:?}"
    );
}

/// One parameter as Parameter Store reports it, or `None` when this Region holds none.
async fn parameter(router: &MultiServiceRouter, name: &str) -> Option<Value> {
    let (status, response) = call(router, "GetParameter", &json!({"Name": name})).await;
    (status == 200).then(|| response["Parameter"].clone())
}

/// A runbook of one step, with the document-level outputs the case declares.
fn one_step_runbook(step: Value, outputs: Vec<&str>, parameters: Value) -> Value {
    let mut content = json!({
        "schemaVersion": "0.3",
        "parameters": parameters,
        "mainSteps": [step],
    });
    if !outputs.is_empty() {
        content["outputs"] = json!(outputs);
    }
    content
}

/// A document name for one row of a table: the case it is about, with everything a document
/// name may not carry replaced ([`CreateDocument`]'s `Name` pattern,
/// `^[a-zA-Z0-9_\-.]{3,128}$`).
fn document_name(prefix: &str, case: &str) -> String {
    let case: String = case
        .chars()
        .map(|letter| if letter.is_ascii_alphanumeric() { letter } else { '-' })
        .collect();
    format!("{prefix}-{case}")
}

/// One row of the API-call table: the step to run, what its outputs must come to, and what
/// the execution's own projected outputs must be.
struct ApiRow {
    case: &'static str,
    /// The `Api` the step calls, and the parameters it carries beside `Service`.
    api: &'static str,
    parameters: Value,
    /// The outputs the step declares.
    outputs: Value,
    /// The document-level outputs the runbook declares.
    projected: Vec<&'static str>,
    /// The step's `Outputs` as the report must carry them.
    expected: Value,
    /// The execution's `Outputs` as the report must carry them.
    expected_projection: Value,
}

/// The parameter the read rows below read, and the value a client stored in it.
const SOURCE_PARAMETER: &str = "/suite/source";

/// See [`SOURCE_PARAMETER`].
const SOURCE_VALUE: &str = "source-value";

/// An `aws:executeAwsApi` step calls the API it names with the parameters the walk resolved,
/// selects its declared outputs out of that API's own answer, and reaches `Success`.
#[tokio::test]
async fn an_execute_aws_api_step_calls_the_api_it_names_and_selects_its_outputs() {
    let (router, engine) = service();
    put_parameter(&router, SOURCE_PARAMETER, SOURCE_VALUE).await;

    // One row per axis the rule generalises over: which API is called (a read of one value,
    // a read of several, a write), which member of the answer the selector names, which
    // declared type the value is read as, and whether the step declares outputs at all.
    let rows = vec![
        ApiRow {
            case: "a read API selecting one text value",
            api: "GetParameter",
            parameters: json!({"Name": "{{ Source }}"}),
            outputs: json!([{"Name": "Value", "Selector": "$.Parameter.Value", "Type": "String"}]),
            projected: vec!["call.Value"],
            expected: json!({"Value": [SOURCE_VALUE]}),
            expected_projection: json!({"call.Value": [SOURCE_VALUE]}),
        },
        ApiRow {
            case: "the same API selecting another member of the same answer",
            api: "GetParameter",
            parameters: json!({"Name": "{{ Source }}"}),
            outputs: json!([
                {"Name": "Kind", "Selector": "$.Parameter.Type", "Type": "String"},
                {"Name": "Version", "Selector": "$.Parameter.Version", "Type": "Integer"},
            ]),
            projected: vec!["call.Kind", "call.Version"],
            expected: json!({"Kind": ["String"], "Version": ["1"]}),
            expected_projection: json!({"call.Kind": ["String"], "call.Version": ["1"]}),
        },
        ApiRow {
            case: "a read API selecting a list",
            api: "GetParameters",
            parameters: json!({"Names": ["{{ Source }}", "/suite/absent"]}),
            outputs: json!([
                {"Name": "Missing", "Selector": "$.InvalidParameters", "Type": "StringList"},
                {"Name": "First", "Selector": "$.Parameters[0].Value", "Type": "String"},
            ]),
            projected: vec!["call.Missing", "call.First"],
            expected: json!({"Missing": ["/suite/absent"], "First": [SOURCE_VALUE]}),
            expected_projection: json!({
                "call.Missing": ["/suite/absent"],
                "call.First": [SOURCE_VALUE],
            }),
        },
        ApiRow {
            case: "a write API answering a number",
            api: "PutParameter",
            parameters: json!({
                "Name": "/suite/written-by-a-step",
                "Value": "{{ Source }}-echo",
                "Type": "String",
            }),
            outputs: json!([{"Name": "Version", "Selector": "$.Version", "Type": "Integer"}]),
            projected: vec!["call.Version"],
            expected: json!({"Version": ["1"]}),
            expected_projection: json!({"call.Version": ["1"]}),
        },
        ApiRow {
            case: "a step that declares no outputs at all",
            api: "PutParameter",
            parameters: json!({
                "Name": "/suite/written-without-outputs",
                "Value": "no-outputs",
                "Type": "String",
            }),
            outputs: json!([]),
            projected: vec![],
            // The two parameter maps a read always carries, holding nothing: the step
            // declared no output, and the runbook projects none.
            expected: json!({}),
            expected_projection: json!({}),
        },
    ];

    for row in rows {
        let ApiRow {
            case,
            api,
            parameters,
            outputs,
            projected,
            expected,
            expected_projection,
        } = row;
        let name = document_name("Call", case);
        let mut inputs = json!({"Service": "ssm", "Api": api});
        for (member, value) in parameters.as_object().cloned().unwrap_or_default() {
            inputs[member] = value;
        }
        let mut declared = json!({"name": "call", "action": "aws:executeAwsApi", "inputs": inputs});
        if !outputs.as_array().cloned().unwrap_or_default().is_empty() {
            declared["outputs"] = outputs;
        }
        runbook(
            &router,
            &name,
            &one_step_runbook(
                declared,
                projected,
                json!({"Source": {"type": "String", "default": SOURCE_PARAMETER}}),
            ),
        )
        .await;
        let id = started(&router, case, &json!({"DocumentName": name})).await;
        pick_up(&engine);
        let execution = at_rest(&router, case, &id).await;

        assert_eq!(
            text(&execution, "AutomationExecutionStatus"),
            "Success",
            "case {case:?}: expected the automation to reach Success, got {execution}"
        );
        let call_step = step(&execution, "call");
        assert_eq!(
            text(&call_step, "StepStatus"),
            "Success",
            "case {case:?}: expected the step to reach Success, got {call_step}"
        );
        assert_eq!(
            call_step["Outputs"], expected,
            "case {case:?}: expected the step outputs {expected}, got {call_step}"
        );
        assert_eq!(
            execution["Outputs"], expected_projection,
            "case {case:?}: expected the projected outputs {expected_projection}, got {execution}"
        );
    }

    // The write rows' side effects are real state: both parameters the steps created are
    // served by Parameter Store's own read, at the version a creation makes.
    for (case, name, value) in [
        (
            "the parameter the step with outputs wrote",
            "/suite/written-by-a-step",
            format!("{SOURCE_PARAMETER}-echo"),
        ),
        (
            "the parameter the step without outputs wrote",
            "/suite/written-without-outputs",
            "no-outputs".to_string(),
        ),
    ] {
        let held = parameter(&router, name).await;
        assert_eq!(
            held.as_ref().map(|held| (text(held, "Value"), held["Version"].clone())),
            Some((value.clone(), json!(1))),
            "case {case:?}: expected {name} at Version 1 carrying {value:?}, got {held:?}"
        );
    }
}

/// An API that REFUSES the step's call fails the step with that service's own refusal, and
/// the execution fails with it.
#[tokio::test]
async fn an_api_that_refuses_the_call_fails_the_step_with_its_own_refusal() {
    let (router, engine) = service();
    put_parameter(&router, SOURCE_PARAMETER, SOURCE_VALUE).await;

    // One row per refusal a called API answers: a read of a parameter this Region holds
    // none of, and a write of a parameter that already exists.
    let rows = vec![
        (
            "a read of a parameter that does not exist",
            "GetParameter",
            json!({"Name": "/suite/never-stored"}),
            "ParameterNotFound",
        ),
        (
            "a write the called API refuses",
            "PutParameter",
            json!({"Name": SOURCE_PARAMETER, "Value": "again", "Type": "String"}),
            "ParameterAlreadyExists",
        ),
    ];

    for (case, api, parameters, expected_code) in rows {
        let name = format!("Refused-{api}-{expected_code}");
        let mut inputs = json!({"Service": "ssm", "Api": api});
        for (member, value) in parameters.as_object().cloned().unwrap_or_default() {
            inputs[member] = value;
        }
        runbook(
            &router,
            &name,
            &one_step_runbook(
                json!({
                    "name": "call",
                    "action": "aws:executeAwsApi",
                    "inputs": inputs,
                    "outputs": [{"Name": "Value", "Selector": "$.Parameter.Value", "Type": "String"}],
                }),
                vec!["call.Value"],
                json!({}),
            ),
        )
        .await;
        let id = started(&router, case, &json!({"DocumentName": name})).await;
        pick_up(&engine);
        let execution = at_rest(&router, case, &id).await;

        assert_eq!(
            text(&execution, "AutomationExecutionStatus"),
            "Failed",
            "case {case:?}: expected the automation to fail with the API's refusal, got \
             {execution}"
        );
        let failure = text(&execution, "FailureMessage");
        assert!(
            failure.contains(expected_code) && failure.contains(api),
            "case {case:?}: expected the failure to carry the {expected_code} refusal of \
             {api}, got {failure:?}"
        );
        assert_eq!(
            (
                &execution["Outputs"],
                &step(&execution, "call")["Outputs"]
            ),
            (&json!({}), &json!({})),
            "case {case:?}: expected no outputs for a call that was refused, got {execution}"
        );
    }
}

/// One row of the script table: the script to run, the runtime it declares, what it is
/// passed, and what its declared outputs must come to.
struct ScriptRow {
    case: &'static str,
    runtime: &'static str,
    handler: &'static str,
    script: &'static str,
    payload: Value,
    outputs: Value,
    projected: Vec<&'static str>,
    /// The step's `Outputs`, or `None` for a row whose script must fail the step.
    expected: Option<Value>,
    /// What the failure of such a row must name.
    failure_names: &'static str,
}

/// An `aws:executeScript` step runs its script in the declared runtime and selects the
/// handler's own return value through `$.Payload…` under each declared type.
#[tokio::test]
async fn an_execute_script_step_computes_its_outputs_in_the_declared_runtime() {
    let (router, engine) = service();

    // One row per axis the rule generalises over: which type the declared output is read
    // as, what the script computes from its payload, which published python runtime it
    // declares, and what a script that cannot run comes to.
    let rows = vec![
        ScriptRow {
            case: "a text value computed from the payload",
            runtime: "python3.11",
            handler: "handler",
            script: "def handler(events, context):\n    \
                     return {\"value\": events[\"Seed\"].upper() + \"-done\"}\n",
            payload: json!({"Seed": "{{ Seed }}"}),
            outputs: json!([{"Name": "value", "Selector": "$.Payload.value", "Type": "String"}]),
            projected: vec!["compute.value"],
            expected: Some(json!({"value": ["SEED-7-done"]})),
            failure_names: "",
        },
        ScriptRow {
            case: "a list the script builds",
            runtime: "python3.10",
            handler: "handler",
            script: "def handler(events, context):\n    \
                     return {\"items\": [part for part in events[\"Seed\"].split(\"-\")]}\n",
            payload: json!({"Seed": "{{ Seed }}"}),
            outputs: json!([
                {"Name": "items", "Selector": "$.Payload.items", "Type": "StringList"}
            ]),
            projected: vec!["compute.items"],
            expected: Some(json!({"items": ["seed", "7"]})),
            failure_names: "",
        },
        ScriptRow {
            case: "a whole number the script computes",
            runtime: "python3.9",
            handler: "handler",
            script: "def handler(events, context):\n    \
                     return {\"count\": len(events[\"Seed\"]) * 2}\n",
            payload: json!({"Seed": "{{ Seed }}"}),
            outputs: json!([{"Name": "count", "Selector": "$.Payload.count", "Type": "Integer"}]),
            projected: vec!["compute.count"],
            expected: Some(json!({"count": ["12"]})),
            failure_names: "",
        },
        ScriptRow {
            case: "a mapping and a boolean out of one answer",
            runtime: "python3.11",
            handler: "main",
            script: "def main(events, context):\n    \
                     return {\"mapping\": {\"seed\": events[\"Seed\"]}, \"ok\": True}\n",
            payload: json!({"Seed": "{{ Seed }}"}),
            outputs: json!([
                {"Name": "mapping", "Selector": "$.Payload.mapping", "Type": "StringMap"},
                {"Name": "ok", "Selector": "$.Payload.ok", "Type": "Boolean"},
            ]),
            projected: vec!["compute.mapping", "compute.ok"],
            expected: Some(json!({"mapping": [r#"{"seed":"seed-7"}"#], "ok": ["true"]})),
            failure_names: "",
        },
        ScriptRow {
            case: "a script that imports the standard library",
            runtime: "python3.11",
            handler: "handler",
            script: "import json\n\n\ndef handler(events, context):\n    \
                     return {\"value\": json.dumps(sorted(events.keys()))}\n",
            payload: json!({"Seed": "{{ Seed }}", "Other": "x"}),
            outputs: json!([{"Name": "value", "Selector": "$.Payload.value", "Type": "String"}]),
            projected: vec!["compute.value"],
            expected: Some(json!({"value": [r#"["Other", "Seed"]"#]})),
            failure_names: "",
        },
        ScriptRow {
            case: "a script that raises",
            runtime: "python3.11",
            handler: "handler",
            script: "def handler(events, context):\n    \
                     raise ValueError(\"the seed is not a number\")\n",
            payload: json!({"Seed": "{{ Seed }}"}),
            outputs: json!([{"Name": "value", "Selector": "$.Payload.value", "Type": "String"}]),
            projected: vec!["compute.value"],
            expected: None,
            failure_names: "ValueError",
        },
        ScriptRow {
            case: "a handler the script does not define",
            runtime: "python3.11",
            handler: "absent",
            script: "def handler(events, context):\n    return {}\n",
            payload: json!({}),
            outputs: json!([]),
            projected: vec![],
            expected: None,
            failure_names: "absent",
        },
        ScriptRow {
            case: "a runtime this emulator embeds no engine for",
            runtime: "PowerShell Core 6.0",
            handler: "handler",
            script: "$out = @{}\n",
            payload: json!({}),
            outputs: json!([]),
            projected: vec![],
            expected: None,
            failure_names: "PowerShell Core 6.0",
        },
        ScriptRow {
            case: "an output the script's answer holds no value at",
            runtime: "python3.11",
            handler: "handler",
            script: "def handler(events, context):\n    return {\"other\": 1}\n",
            payload: json!({}),
            outputs: json!([{"Name": "value", "Selector": "$.Payload.value", "Type": "String"}]),
            projected: vec!["compute.value"],
            expected: None,
            failure_names: "$.Payload.value",
        },
        // The three bounds a run of caller-supplied code is held to, each reached from the
        // script itself: the step budget a script that computes without end spends, the
        // recursion limit a script that recurses without end reaches, and the cap on how
        // large a value one script may answer with. Each one STOPS the run, which fails the
        // step — nothing is reported as computed for a run a bound ended. Which bound the
        // host reports for the first row is its own reading (its step budget, or the host
        // time one run may take on a loaded machine), so the row asserts that the run was
        // STOPPED rather than naming one of them.
        ScriptRow {
            case: "a script that computes without end",
            runtime: "python3.11",
            handler: "handler",
            script: "def handler(events, context):\n    while True:\n        pass\n",
            payload: json!({}),
            outputs: json!([{"Name": "value", "Selector": "$.Payload.value", "Type": "String"}]),
            projected: vec!["compute.value"],
            expected: None,
            failure_names: "was stopped",
        },
        ScriptRow {
            case: "a script that recurses without end",
            runtime: "python3.11",
            handler: "handler",
            script: "def handler(events, context):\n    \
                     def deeper(depth):\n        \
                     return deeper(depth + 1)\n    \
                     return {\"value\": str(deeper(0))}\n",
            payload: json!({}),
            outputs: json!([{"Name": "value", "Selector": "$.Payload.value", "Type": "String"}]),
            projected: vec!["compute.value"],
            expected: None,
            failure_names: "RecursionError",
        },
        ScriptRow {
            case: "a script that answers with more than one may",
            runtime: "python3.11",
            handler: "handler",
            // Past `actions::script_budget`'s answer cap, built in one step rather than in a
            // loop, so what the run spends is the answer's size and not the step budget.
            script: "def handler(events, context):\n    \
                     return {\"value\": \"x\" * (2 * 1024 * 1024)}\n",
            payload: json!({}),
            outputs: json!([{"Name": "value", "Selector": "$.Payload.value", "Type": "String"}]),
            projected: vec!["compute.value"],
            expected: None,
            failure_names: "was stopped",
        },
    ];

    for row in rows {
        let ScriptRow {
            case,
            runtime,
            handler,
            script,
            payload,
            outputs,
            projected,
            expected,
            failure_names,
        } = row;
        let (router, engine) = (&router, &engine);
        let name = document_name("Script", case);
        let mut declared = json!({
            "name": "compute",
            "action": "aws:executeScript",
            "inputs": {
                "Runtime": runtime,
                "Handler": handler,
                "Script": script,
                "InputPayload": payload,
            },
        });
        if !outputs.as_array().cloned().unwrap_or_default().is_empty() {
            declared["outputs"] = outputs;
        }
        runbook(
            router,
            &name,
            &one_step_runbook(
                declared,
                projected,
                json!({"Seed": {"type": "String", "default": "seed-7"}}),
            ),
        )
        .await;
        let id = started(router, case, &json!({"DocumentName": name})).await;
        pick_up(engine);
        let execution = at_rest(router, case, &id).await;
        let compute = step(&execution, "compute");

        match expected {
            Some(expected) => {
                assert_eq!(
                    text(&execution, "AutomationExecutionStatus"),
                    "Success",
                    "case {case:?}: expected the automation to reach Success, got {execution}"
                );
                assert_eq!(
                    compute["Outputs"], expected,
                    "case {case:?}: expected the step outputs {expected}, got {compute}"
                );
                // The document's own outputs project the same values under
                // `<step>.<output>`.
                for (output, values) in expected.as_object().cloned().unwrap_or_default() {
                    assert_eq!(
                        execution["Outputs"][format!("compute.{output}")],
                        values,
                        "case {case:?}: expected compute.{output} to project {values}, got \
                         {execution}"
                    );
                }
            }
            None => {
                assert_eq!(
                    text(&execution, "AutomationExecutionStatus"),
                    "Failed",
                    "case {case:?}: expected the automation to fail, got {execution}"
                );
                let failure = text(&execution, "FailureMessage");
                assert!(
                    failure.contains(failure_names),
                    "case {case:?}: expected the failure to name {failure_names:?}, got \
                     {failure:?}"
                );
                assert_eq!(
                    (&execution["Outputs"], &compute["Outputs"]),
                    (&json!({}), &json!({})),
                    "case {case:?}: expected no outputs for a script that produced none, got \
                     {execution}"
                );
            }
        }
    }
}

/// A step's outputs are what the NEXT step's references resolve to, and the effect that next
/// step makes is the computed value.
#[tokio::test]
async fn one_steps_outputs_are_what_the_next_steps_references_resolve() {
    let (router, engine) = service();
    let name = "ComputeThenPersist";
    runbook(
        &router,
        name,
        &json!({
            "schemaVersion": "0.3",
            "parameters": {
                "Seed": {"type": "String", "default": "11"},
                "Target": {"type": "String", "default": "/suite/computed"},
            },
            "outputs": ["compute.value", "persist.Version"],
            "mainSteps": [
                {
                    "name": "compute",
                    "action": "aws:executeScript",
                    "inputs": {
                        "Runtime": "python3.11",
                        "Handler": "handler",
                        "Script": "def handler(events, context):\n    \
                                   return {\"value\": str(int(events[\"Seed\"]) * 3)}\n",
                        "InputPayload": {"Seed": "{{ Seed }}"},
                    },
                    "outputs": [
                        {"Name": "value", "Selector": "$.Payload.value", "Type": "String"}
                    ],
                },
                {
                    "name": "persist",
                    "action": "aws:executeAwsApi",
                    "inputs": {
                        "Service": "ssm",
                        "Api": "PutParameter",
                        "Name": "{{ Target }}",
                        "Value": "{{ compute.value }}",
                        "Type": "String",
                    },
                    "outputs": [
                        {"Name": "Version", "Selector": "$.Version", "Type": "Integer"}
                    ],
                    "isEnd": true,
                },
            ],
        }),
    )
    .await;

    // One row per seed: what the script computes differs, so what the next step writes
    // differs with it — the reference resolves to the value the step produced rather than
    // to anything the runbook spells.
    for (case, seed, written) in [
        ("the runbook's own default seed", None, "33"),
        ("a seed the start supplies", Some("5"), "15"),
        ("a larger seed", Some("1000"), "3000"),
    ] {
        let target = format!("/suite/computed-{}", written);
        let mut start = json!({"DocumentName": name, "Parameters": {"Target": [&target]}});
        if let Some(seed) = seed {
            start["Parameters"]["Seed"] = json!([seed]);
        }
        let id = started(&router, case, &start).await;
        pick_up(&engine);
        let execution = at_rest(&router, case, &id).await;

        assert_eq!(
            text(&execution, "AutomationExecutionStatus"),
            "Success",
            "case {case:?}: expected the automation to reach Success, got {execution}"
        );
        assert_eq!(
            execution["Outputs"],
            json!({"compute.value": [written], "persist.Version": ["1"]}),
            "case {case:?}: expected the projected outputs of both steps, got {execution}"
        );
        // A step's `Inputs` reports each resolved value as the JSON literal it is, so a
        // resolved string carries its own quotes.
        assert_eq!(
            step(&execution, "persist")["Inputs"]["Value"],
            json!(format!("\"{written}\"")),
            "case {case:?}: expected the resolved input of the next step to be the computed \
             value, got {execution}"
        );
        let held = parameter(&router, &target).await;
        assert_eq!(
            held.as_ref().map(|held| text(held, "Value")),
            Some(written.to_string()),
            "case {case:?}: expected {target} to carry the computed value, got {held:?}"
        );
    }
}

/// A runbook of one step that waits for a caller's signal, and the step's own name.
fn waiting_runbook(action: &str) -> (String, Value) {
    let step = "hold".to_string();
    (
        step.clone(),
        json!({
            "schemaVersion": "0.3",
            "mainSteps": [
                {"name": step, "action": action, "inputs": {}},
                {
                    "name": "after",
                    "action": "aws:executeScript",
                    "inputs": {
                        "Runtime": "python3.11",
                        "Handler": "handler",
                        "Script": "def handler(events, context):\n    \
                                   return {\"value\": \"resumed\"}\n",
                        "InputPayload": {},
                    },
                    "outputs": [
                        {"Name": "value", "Selector": "$.Payload.value", "Type": "String"}
                    ],
                },
            ],
            "outputs": ["after.value"],
        }),
    )
}

/// Start a runbook that waits, and answer `(execution id, the step it waits at)` once it is
/// `Waiting`.
async fn waiting(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    name: &str,
    action: &str,
) -> (String, String) {
    let (step_name, content) = waiting_runbook(action);
    runbook(router, name, &content).await;
    let id = started(router, case, &json!({"DocumentName": name})).await;
    pick_up(engine);
    for _ in 0..READS_UNTIL_AT_REST {
        let execution = read(router, case, &id).await;
        if text(&execution, "AutomationExecutionStatus") == "Waiting" {
            return (id, step_name);
        }
        ::tokio::task::yield_now().await;
    }
    panic!("case {case:?}: the automation never reached Waiting")
}

/// The execution id a signal names has to be one this endpoint holds, and the step a
/// `Resume` names has to be one the runbook declares — and both refusals leave the execution
/// exactly as it was.
#[tokio::test]
async fn a_signal_is_refused_for_an_execution_or_a_step_that_does_not_exist() {
    let (router, engine) = service();
    let (id, step_name) = waiting(
        &router,
        &engine,
        "the paused execution",
        "Paused",
        "aws:pause",
    )
    .await;
    let document_version = text(&read(&router, "the paused execution", &id).await, "DocumentVersion");

    // One row per refusal: a well-formed id this endpoint holds nothing for, under each
    // signal type that names a step; and a step name the runbook does not declare, under
    // each of those signal types.
    for signal in ["Resume", "StartStep"] {
        let case = format!("an execution id nothing was minted under, signalled {signal}");
        let (status, refused) = call(
            &router,
            "SendAutomationSignal",
            &json!({
                "AutomationExecutionId": UNKNOWN_EXECUTION_ID,
                "SignalType": signal,
                "Payload": {"StepName": [&step_name]},
            }),
        )
        .await;
        assert_eq!(
            (
                status,
                error_code(&refused).as_str(),
                error_message(&refused).as_str()
            ),
            (
                400,
                "AutomationExecutionNotFoundException",
                "The input automation execution does not exist."
            ),
            "case {case:?}: expected the modelled not-found refusal, got {status} with {refused}"
        );

        // And nothing was created for that id: a read of it is refused too.
        let (status, read_back) = call(
            &router,
            "GetAutomationExecution",
            &json!({"AutomationExecutionId": UNKNOWN_EXECUTION_ID}),
        )
        .await;
        assert_eq!(
            (status, error_code(&read_back).as_str()),
            (400, "AutomationExecutionNotFoundException"),
            "case {case:?}: expected no execution to be fabricated for the id, got {status} \
             with {read_back}"
        );

        // One row per step name the runbook does not declare: the message is built from the
        // signal, the name the payload carried and the execution's own document and
        // version, so every one of them varies with the request.
        for bad_step in ["NoSuchStepXYZ", "hold-2", "After"] {
            let case = format!("the step {bad_step} the runbook does not declare, signalled {signal}");
            let (status, refused) = call(
                &router,
                "SendAutomationSignal",
                &json!({
                    "AutomationExecutionId": &id,
                    "SignalType": signal,
                    "Payload": {"StepName": [bad_step]},
                }),
            )
            .await;
            assert_eq!(
                (
                    status,
                    error_code(&refused).as_str(),
                    error_message(&refused)
                ),
                (
                    400,
                    "AutomationStepNotFoundException",
                    format!(
                        "Failed to process signal {signal}. Step {bad_step} is not defined in \
                         document: Paused (version {document_version})."
                    )
                ),
                "case {case:?}: expected the modelled step-not-found refusal, got {status} \
                 with {refused}"
            );
        }
    }

    // The execution the refusals were about is untouched: still waiting, at the same step,
    // with nothing run past it.
    let execution = read(&router, "after the refusals", &id).await;
    assert_eq!(
        (
            text(&execution, "AutomationExecutionStatus"),
            text(&step(&execution, &step_name), "StepStatus"),
            text(&step(&execution, "after"), "StepStatus"),
        ),
        (
            "Waiting".to_string(),
            "Waiting".to_string(),
            "Pending".to_string()
        ),
        "expected the refusals to leave the execution waiting at its own step, got {execution}"
    );
}

/// A signal the execution's state admits carries the walk on from the step it waits at: the
/// steps after it run, and the execution reaches its own end.
#[tokio::test]
async fn a_signal_the_execution_is_waiting_for_carries_the_walk_on() {
    // One row per signal that releases a waiting step, with the action that waits for it and
    // what the execution then comes to rest in.
    let rows = vec![
        ("a resumed pause", "aws:pause", "Resume", "Success"),
        ("an approved approval", "aws:approve", "Approve", "Success"),
        ("a rejected approval", "aws:approve", "Reject", "Failed"),
    ];

    for (case, action, signal, expected_status) in rows {
        let (router, engine) = service();
        let name = format!("Signalled-{signal}");
        let (id, step_name) = waiting(&router, &engine, case, &name, action).await;

        let (status, answered) = call(
            &router,
            "SendAutomationSignal",
            &json!({
                "AutomationExecutionId": &id,
                "SignalType": signal,
                "Payload": {"StepName": [&step_name], "Comment": ["looks good"]},
            }),
        )
        .await;
        assert_eq!(
            (status, &answered),
            (200, &json!({})),
            "case {case:?}: expected an empty 200 for a signal the execution waits for, got \
             {status} with {answered}"
        );

        let execution = at_rest(&router, case, &id).await;
        assert_eq!(
            text(&execution, "AutomationExecutionStatus"),
            expected_status,
            "case {case:?}: expected the signalled execution to reach {expected_status}, got \
             {execution}"
        );
        if expected_status == "Success" {
            // The step the signal released ended, and the step after it RAN — which is the
            // whole point of the signal.
            assert_eq!(
                execution["Outputs"],
                json!({"after.value": ["resumed"]}),
                "case {case:?}: expected the step after the wait to have run, got {execution}"
            );
        } else {
            assert_eq!(
                text(&step(&execution, "after"), "StepStatus"),
                "Pending",
                "case {case:?}: expected nothing to run past a rejected approval, got \
                 {execution}"
            );
        }
    }
}

/// A signal the execution's own state does not admit is refused as the modelled invalid
/// signal, and a `StopStep` stops the step its payload names.
#[tokio::test]
async fn a_signal_the_state_does_not_admit_is_refused_and_a_stop_step_stops_the_step() {
    let (router, engine) = service();
    let (id, step_name) = waiting(&router, &engine, "the paused execution", "Paused", "aws:pause").await;

    // One row per signal this execution's state does not admit: an approval of a step that
    // is no approval, a resume of a step the execution is not waiting at, a resume with no
    // step named at all, a revoke of an automation that is no change request, a stop naming
    // no step at all, and a stop of a step this execution is not standing at.
    //
    // The third member is the message the refusal must carry, where that message is the
    // service's own sentence for the fault rather than the class's: a signal that is about
    // one step and names none is refused by the payload member it wants.
    let rows: Vec<(&str, Value, &str, Option<&str>)> = vec![
        (
            "an approval of a step that is no approval",
            json!({"SignalType": "Approve"}),
            "InvalidAutomationSignalException",
            None,
        ),
        (
            "a resume naming a step the execution is not waiting at",
            json!({"SignalType": "Resume", "Payload": {"StepName": ["after"]}}),
            "InvalidAutomationSignalException",
            None,
        ),
        (
            "a resume naming no step at all",
            json!({"SignalType": "Resume"}),
            "InvalidAutomationSignalException",
            Some("One StepName is required when signal type is Resume."),
        ),
        (
            "a revoke of an automation that is no change request",
            json!({"SignalType": "Revoke"}),
            "InvalidAutomationSignalException",
            None,
        ),
        (
            "a stop naming no step at all",
            json!({"SignalType": "StopStep"}),
            "InvalidAutomationSignalException",
            Some("One StepName is required when signal type is StopStep."),
        ),
        (
            "a stop of a step this execution is not standing at",
            json!({
                "SignalType": "StopStep",
                "Payload": {"StepName": ["after"]},
            }),
            "AutomationStepNotFoundException",
            None,
        ),
    ];

    for (case, signal, expected_code, expected_message) in rows {
        let mut body = json!({"AutomationExecutionId": &id});
        for (member, value) in signal.as_object().cloned().unwrap_or_default() {
            body[member] = value;
        }
        let (status, refused) = call(&router, "SendAutomationSignal", &body).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, expected_code),
            "case {case:?}: expected the {expected_code} refusal, got {status} with {refused}"
        );
        if let Some(expected_message) = expected_message {
            assert_eq!(
                error_message(&refused),
                expected_message,
                "case {case:?}: expected the refusal to name the payload member it wants, \
                 got {refused}"
            );
        }
        assert_eq!(
            text(&read(&router, case, &id).await, "AutomationExecutionStatus"),
            "Waiting",
            "case {case:?}: expected the refusal to leave the execution waiting"
        );
    }

    // The stop its payload DOES name stops that step, and the automation with it.
    let (status, answered) = call(
        &router,
        "SendAutomationSignal",
        &json!({
            "AutomationExecutionId": &id,
            "SignalType": "StopStep",
            "Payload": {"StepName": [&step_name]},
        }),
    )
    .await;
    assert_eq!(
        (status, &answered),
        (200, &json!({})),
        "expected an empty 200 for a stop of the step the automation stands at, got {status} \
         with {answered}"
    );
    let execution = read(&router, "the stopped execution", &id).await;
    assert_eq!(
        (
            text(&execution, "AutomationExecutionStatus"),
            text(&step(&execution, &step_name), "StepStatus"),
        ),
        ("Cancelled".to_string(), "Cancelled".to_string()),
        "expected the stopped step to cancel the automation, got {execution}"
    );

    // And a signal to an execution that has already ended is refused as the invalid signal
    // it is, whichever type it carries.
    for signal in ["Resume", "Approve", "StopStep"] {
        let (status, refused) = call(
            &router,
            "SendAutomationSignal",
            &json!({
                "AutomationExecutionId": &id,
                "SignalType": signal,
                "Payload": {"StepName": [&step_name], "StepExecutionId": ["x"]},
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "InvalidAutomationSignalException"),
            "case {signal:?} on an execution that has ended: expected the invalid-signal \
             refusal, got {status} with {refused}"
        );
    }
}

/// The `StepExecutionId` of one step of an execution, by step name.
fn step_execution_id(execution: &Value, name: &str) -> String {
    text(&step(execution, name), "StepExecutionId")
}

/// The sentence a `StopStep` whose payload names a step execution this execution holds none
/// of is refused with (`automation::STOP_STEP_NOT_FOUND_MESSAGE`).
const STOP_STEP_NOT_FOUND: &str = "The step you are trying to stop doesn't exist.";

/// A `StepExecutionId` of the 8-4-4-4-12 hexadecimal form this service mints them in that no
/// step of any execution was minted under.
const UNKNOWN_STEP_EXECUTION_ID: &str = "99999999-8888-7777-6666-555555555555";

/// RULE: a `StopStep` signal names the step execution it stops through its PAYLOAD — by the
/// step execution's own id ("for the StopStep signal type, you must send the step execution ID
/// as the payload", `SendAutomationSignalRequest$Payload`) — and an id that is in no step of
/// the execution the signal was sent to is refused `AutomationStepNotFoundException` with the
/// stop's own sentence, leaving the execution exactly as it was. A payload naming no step at
/// all is still refused by the member the service asks for.
///
/// The rows differ along what the rule generalises over: which payload member names the step,
/// whether the id is one of this execution's steps, one of ANOTHER execution's, or one nothing
/// minted, and whether the payload names a step at all.
#[tokio::test]
async fn a_stop_step_resolves_the_step_execution_its_payload_names() {
    let (router, engine) = service();
    let (held, step_name) = waiting(&router, &engine, "the stopped execution", "Stopped", "aws:pause").await;
    // A SECOND execution of its own, so the row that names one execution's step id while
    // signalling the other shows the ids are resolved per execution.
    let (other, _) = waiting(&router, &engine, "the other execution", "Other", "aws:pause").await;

    let standing = step_execution_id(&read(&router, "the stopped execution", &held).await, &step_name);
    let not_reached = step_execution_id(&read(&router, "the stopped execution", &held).await, "after");
    let elsewhere = step_execution_id(&read(&router, "the other execution", &other).await, &step_name);
    assert!(
        [&standing, &not_reached, &elsewhere]
            .iter()
            .all(|id| id.len() == 36),
        "every step of an execution carries its own minted id, got {standing:?}, \
         {not_reached:?} and {elsewhere:?}"
    );

    // (case, the payload the stop carries, the code it is refused with, the message where the
    // service words that refusal itself)
    let refused: Vec<(&str, Value, &str, Option<&str>)> = vec![
        (
            "a step execution id nothing was minted under",
            json!({"StepExecutionId": [UNKNOWN_STEP_EXECUTION_ID]}),
            "AutomationStepNotFoundException",
            Some(STOP_STEP_NOT_FOUND),
        ),
        (
            "a step execution id of ANOTHER execution's step",
            json!({"StepExecutionId": [&elsewhere]}),
            "AutomationStepNotFoundException",
            Some(STOP_STEP_NOT_FOUND),
        ),
        (
            "an id that is no id this service mints at all",
            json!({"StepExecutionId": ["not-a-step-execution-id"]}),
            "AutomationStepNotFoundException",
            Some(STOP_STEP_NOT_FOUND),
        ),
        (
            "a payload naming neither member",
            json!({"Comment": ["stop it"]}),
            "InvalidAutomationSignalException",
            Some("One StepName is required when signal type is StopStep."),
        ),
        (
            // A step the walk has not reached is running nothing to stop, which is the
            // execution's own state rather than a step that does not exist.
            "the id of a step the walk has not reached",
            json!({"StepExecutionId": [&not_reached]}),
            "AutomationStepNotFoundException",
            None,
        ),
    ];

    for (case, payload, expected_code, expected_message) in refused {
        let (status, body) = call(
            &router,
            "SendAutomationSignal",
            &json!({
                "AutomationExecutionId": &held,
                "SignalType": "StopStep",
                "Payload": payload,
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, expected_code),
            "case {case:?}: expected the {expected_code} refusal, got {status} with {body}"
        );
        if let Some(expected_message) = expected_message {
            assert_eq!(
                error_message(&body),
                expected_message,
                "case {case:?}: expected the stop's own sentence, got {body}"
            );
        }
        let execution = read(&router, case, &held).await;
        assert_eq!(
            (
                text(&execution, "AutomationExecutionStatus"),
                text(&step(&execution, &step_name), "StepStatus"),
            ),
            ("Waiting".to_string(), "Waiting".to_string()),
            "case {case:?}: expected the refusal to leave the execution waiting, got {execution}"
        );
    }

    // The id of the step the execution STANDS at stops that step, and the automation with it.
    let (status, answered) = call(
        &router,
        "SendAutomationSignal",
        &json!({
            "AutomationExecutionId": &held,
            "SignalType": "StopStep",
            "Payload": {"StepExecutionId": [&standing]},
        }),
    )
    .await;
    assert_eq!(
        (status, &answered),
        (200, &json!({})),
        "expected an empty 200 for a stop of the step the automation stands at, got {status} \
         with {answered}"
    );
    let execution = read(&router, "the stopped execution", &held).await;
    assert_eq!(
        (
            text(&execution, "AutomationExecutionStatus"),
            text(&step(&execution, &step_name), "StepStatus"),
        ),
        ("Cancelled".to_string(), "Cancelled".to_string()),
        "expected the stopped step to cancel the automation, got {execution}"
    );

    // The OTHER execution is untouched by any of it, and its own step id stops its own step.
    let (status, answered) = call(
        &router,
        "SendAutomationSignal",
        &json!({
            "AutomationExecutionId": &other,
            "SignalType": "StopStep",
            "Payload": {"StepExecutionId": [&elsewhere]},
        }),
    )
    .await;
    assert_eq!(
        (status, &answered),
        (200, &json!({})),
        "expected the other execution's own step id to stop its own step, got {status} with \
         {answered}"
    );
    assert_eq!(
        text(
            &read(&router, "the other execution", &other).await,
            "AutomationExecutionStatus"
        ),
        "Cancelled",
        "expected the other execution to be the one its own stop cancelled"
    );
}

/// The script every row of the runtime table runs: it reports the release of the interpreter
/// it is running in, read the two ways a Python program reads one.
const RELEASE_SCRIPT: &str = "import platform\nimport sys\n\n\ndef handler(events, context):\n    \
     return {\n        \"version\": platform.python_version(),\n        \
     \"line\": \"%d.%d\" % (sys.version_info[0], sys.version_info[1]),\n    }\n";

/// RULE: an `aws:executeScript` step declares the runtime its script runs in ("the language
/// runtime environment in which to run the provided script"), so the release the interpreter
/// serving that step reports is the one the step declared — not the build this emulator
/// happens to embed. The patch level is the release's own; what the step declares is the line.
///
/// The rows are published `python3.x` runtimes an `aws:executeScript` step may declare,
/// including ones no recorded check cited, so the reported release follows the DECLARATION
/// rather than any one value. Three of them, because each row stands a whole interpreter up.
#[tokio::test]
async fn an_execute_script_step_runs_in_the_release_its_runtime_declares() {
    let (router, engine) = service();

    for runtime in ["python3.9", "python3.11", "python3.12"] {
        let line = runtime
            .strip_prefix("python")
            .expect("the row names a python runtime");
        let name = document_name("Release", runtime);
        runbook(
            &router,
            &name,
            &one_step_runbook(
                json!({
                    "name": "release",
                    "action": "aws:executeScript",
                    "inputs": {
                        "Runtime": runtime,
                        "Handler": "handler",
                        "Script": RELEASE_SCRIPT,
                        "InputPayload": {},
                    },
                    "outputs": [
                        {"Name": "version", "Selector": "$.Payload.version", "Type": "String"},
                        {"Name": "line", "Selector": "$.Payload.line", "Type": "String"},
                    ],
                }),
                vec!["release.version", "release.line"],
                json!({}),
            ),
        )
        .await;
        let id = started(&router, runtime, &json!({"DocumentName": name})).await;
        pick_up(&engine);
        let execution = at_rest(&router, runtime, &id).await;
        assert_eq!(
            text(&execution, "AutomationExecutionStatus"),
            "Success",
            "case {runtime:?}: expected the script step to reach Success, got {execution}"
        );
        let reported = step(&execution, "release");
        let version = reported["Outputs"]["version"][0]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            version.starts_with(&format!("{line}.")),
            "case {runtime:?}: expected the reported version to be a release of {line}, got \
             {version:?}"
        );
        assert_eq!(
            reported["Outputs"]["line"][0].as_str(),
            Some(line),
            "case {runtime:?}: expected the version fields to report the {line} line, got \
             {reported}"
        );
    }
}
