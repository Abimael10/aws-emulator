// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for how a DURABLE EXECUTION ends.
//!
//! The rule under test: an execution is the handle a client polls, and it is over when the
//! invocation that drives it is over. `GetDurableExecution` reports `RUNNING` while the
//! function is still running it, and then the terminal status that invocation reached: a
//! handler that returned ends the execution `SUCCEEDED`, with an `EndTimestamp` and an
//! `ExecutionSucceeded` history event carrying what the handler returned; a handler that
//! raised ends it `FAILED`, with an `ExecutionFailed` event and the error the run produced.
//! An execution left `RUNNING` after its invocation has ended is a wait no event would ever
//! end, which is why nothing here is reported until the run that decides it has happened.
//!
//! The rows differ along what the rule generalises over: how the handler ends (a return, a
//! raise), what it returns, and the error it raises.
//!
//! The suites below it carry the same rule for a handler written against the DURABLE
//! programming model the runtime supplies — whose execution ends `SUCCEEDED` whichever
//! identity of the function the invocation named — and the KEY that execution's payload data
//! is encrypted under: a read that asks for the data decrypts it under the key the
//! function's `DurableConfig` names, so a key the key service reports as unusable refuses
//! that read with the modelled 502 for the class of failure it reported, while the
//! metadata-only reads decrypt nothing and are served whatever state the key is in.
//!
//! The package writer is the one `synchronous_invoke_run.rs` publishes, so there is one
//! writer of a deployment archive in this target.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::{
    HOST_WAIT_CEILING, PERMIT_WAIT,
};
use service_com_amazonaws_lambda_awsgirapiservice::state::{
    FIRST_ASYNC_RETRY_DELAY, SECOND_ASYNC_RETRY_DELAY,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{
    create_function, json_call, make_activation_due, router_on, send, Answer, ROLE,
};
use crate::synchronous_invoke_run::package;

/// Where `Invoke` is bound.
const INVOCATIONS_URI: &str = "/2015-03-31/functions";

/// The request header `Invoke$DurableExecutionName` is bound to.
const DURABLE_EXECUTION_NAME_HEADER: &str = "x-amz-durable-execution-name";

/// The response header `Invoke$DurableExecutionArn` is bound to.
///
/// `pub(crate)` for the same reason [`kms_call`] is: the suites that put a key into a state
/// and then assert what Lambda does about it start their executions through the helpers here.
pub(crate) const DURABLE_EXECUTION_ARN_HEADER: &str = "x-amz-durable-execution-arn";

/// A customer managed key ARN satisfying the model's `KMSKeyArn` pattern, which the durable
/// function here names in its `DurableConfig`.
const KMS_KEY: &str = "arn:aws:kms:us-east-1:000000000000:key/durable-executions";

/// How long this suite waits for an accepted event to be delivered and the execution it
/// drives to end. The delivery happens on the runtime's own time, so the wait is a poll
/// rather than a single sleep.
///
/// The bound is what the EMULATOR itself allows one invocation, rather than a span picked
/// here: the wait for a share of the run capacity ([`PERMIT_WAIT`]) and the wait for the
/// run's own answer ([`HOST_WAIT_CEILING`]), with room for every attempt the retry policy has
/// for an event whose handler raised — separated by the waits the service puts between them
/// ([`FIRST_ASYNC_RETRY_DELAY`], [`SECOND_ASYNC_RETRY_DELAY`]) — and a margin for the queueing
/// either side of them. It is patience and not a weakened expectation — the assertions below
/// are unchanged — and a whole-workspace test run shares its cores with every other suite, so
/// a run really can wait for capacity before it starts.
const SETTLE_CEILING: Duration = PERMIT_WAIT
    .saturating_add(HOST_WAIT_CEILING)
    .saturating_add(FIRST_ASYNC_RETRY_DELAY)
    .saturating_add(SECOND_ASYNC_RETRY_DELAY)
    .saturating_add(Duration::from_secs(60));

/// How long one poll of the execution waits before asking again.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// `GetDurableExecution`: the label is not greedy, so the ARN's slashes are percent-encoded
/// in the path exactly as an SDK encodes them.
async fn get_execution(router: &MultiServiceRouter, arn: &str) -> Answer {
    read_execution(router, arn, "").await
}

/// `GetDurableExecution` with `query` appended, which is how a read states whether it asks
/// for the execution's own data (`IncludeExecutionData`).
pub(crate) async fn read_execution(
    router: &MultiServiceRouter,
    arn: &str,
    query: &str,
) -> Answer {
    json_call(
        router,
        "GET",
        &format!(
            "/2025-12-01/durable-executions/{}{query}",
            arn.replace('/', "%2F")
        ),
        &Value::Null,
    )
    .await
}

/// `GetDurableExecutionHistory` on one execution.
async fn get_history(router: &MultiServiceRouter, arn: &str) -> Answer {
    read_history(router, arn, "").await
}

/// `GetDurableExecutionHistory` with `query` appended; see [`read_execution`].
pub(crate) async fn read_history(router: &MultiServiceRouter, arn: &str, query: &str) -> Answer {
    json_call(
        router,
        "GET",
        &format!(
            "/2025-12-01/durable-executions/{}/history{query}",
            arn.replace('/', "%2F")
        ),
        &Value::Null,
    )
    .await
}

/// One row: how the handler ends, and the terminal status and history event the execution
/// must therefore reach.
struct Case {
    /// What makes this row its own case, and names its function and execution.
    case: &'static str,
    /// The handler's body.
    source: &'static str,
    /// The `ExecutionStatus` the execution must end in.
    status: &'static str,
    /// The history event type that end must record.
    event: &'static str,
    /// Text the event's own payload must carry — what the handler returned, or the error it
    /// raised.
    carries: &'static str,
}

/// An execution ends with what the invocation driving it came to, and a client polling it
/// sees that terminal status rather than waiting forever.
#[tokio::test]
async fn a_durable_execution_ends_with_what_its_invocation_came_to() {
    let rows = [
        Case {
            case: "a handler that returned",
            source: "def lambda_handler(event, context):\n    return {'step': 'done'}\n",
            status: "SUCCEEDED",
            event: "ExecutionSucceeded",
            carries: "done",
        },
        Case {
            case: "a handler that raised",
            source: "def lambda_handler(event, context):\n    raise ValueError('step broke')\n",
            status: "FAILED",
            event: "ExecutionFailed",
            carries: "step broke",
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = router_on(&engine);
        let name = format!("durable-{index}");
        let created = create_function(
            &router,
            &json!({
                "FunctionName": name,
                "Role": ROLE,
                "Runtime": "python3.12",
                "Handler": "lambda_function.lambda_handler",
                "DurableConfig": { "KMSKeyArn": KMS_KEY, "ExecutionTimeout": 3_600 },
                "Code": { "ZipFile": base64_of(&package(row.source)) },
            }),
        )
        .await;
        assert_eq!(
            created.status,
            201,
            "case {:?}: expected 201 from CreateFunction, got {} {}",
            row.case,
            created.status,
            String::from_utf8_lossy(&created.body)
        );
        // The activation the create owes is brought forward and WAITED FOR as a condition,
        // so the invocation below meets a function that is really serving.
        wait_until_active(&engine, &router, row.case, &name, "$LATEST").await;
        // No RETRY for an errored handler: no row here is about how many delivery attempts an
        // event has ("the maximum number of times to retry when the function returns an
        // error", `MaximumRetryAttempts`, whose floor is 0), and Lambda waits minutes between
        // the attempts it makes, so a row left on the default would wait those out before the
        // execution could carry what its invocation came to.
        let configured = json_call(
            &router,
            "PUT",
            &format!("/2019-09-25/functions/{name}/event-invoke-config"),
            &json!({ "MaximumRetryAttempts": 0 }),
        )
        .await;
        assert_eq!(
            configured.status,
            200,
            "case {:?}: expected 200 from PutFunctionEventInvokeConfig, got {} {}",
            row.case,
            configured.status,
            String::from_utf8_lossy(&configured.body)
        );

        let execution = format!("exec-{index}");
        let invoked = send(
            &router,
            "POST",
            &format!("{INVOCATIONS_URI}/{name}/invocations"),
            "application/json",
            br#"{"marker":"durable"}"#.to_vec(),
            &[
                ("x-amz-invocation-type", "Event".to_string()),
                (DURABLE_EXECUTION_NAME_HEADER, execution.clone()),
            ],
        )
        .await;
        assert_eq!(
            invoked.status,
            202,
            "case {:?}: expected 202 from Invoke, got {} {}",
            row.case,
            invoked.status,
            String::from_utf8_lossy(&invoked.body)
        );
        let arn = invoked
            .header(DURABLE_EXECUTION_ARN_HEADER)
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected the accepted invocation to answer an execution ARN",
                    row.case
                )
            });

        // The execution is polled the way a client polls it: the delivery happens on the
        // runtime's own time, so the terminal status is waited FOR rather than assumed.
        let deadline = std::time::Instant::now() + SETTLE_CEILING;
        let mut reported = String::new();
        while std::time::Instant::now() < deadline {
            let read = get_execution(&router, &arn).await;
            assert_eq!(
                read.status,
                200,
                "case {:?}: expected 200 from GetDurableExecution, got {} {}",
                row.case,
                read.status,
                String::from_utf8_lossy(&read.body)
            );
            reported = read
                .json()
                .get("Status")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if reported != "RUNNING" {
                // An execution that has ended reports WHEN it ended.
                assert!(
                    read.json().get("EndTimestamp").is_some(),
                    "case {:?}: expected an EndTimestamp on an execution that ended, got {}",
                    row.case,
                    String::from_utf8_lossy(&read.body)
                );
                break;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        assert_eq!(
            reported, row.status,
            "case {:?}: expected the execution to end {:?}, got {reported:?}",
            row.case, row.status
        );

        // The history records what happened, and carries what the run produced.
        let history = get_history(&router, &arn).await;
        assert_eq!(
            history.status,
            200,
            "case {:?}: expected 200 from GetDurableExecutionHistory, got {} {}",
            row.case,
            history.status,
            String::from_utf8_lossy(&history.body)
        );
        let rendered = String::from_utf8_lossy(&history.body).to_string();
        for expected in [row.event, row.carries] {
            assert!(
                rendered.contains(expected),
                "case {:?}: expected the history to carry {expected:?}, got {rendered}",
                row.case
            );
        }
    }
}

/// A blob member: restJson1 carries one base64-encoded in a JSON body.
fn base64_of(bytes: &[u8]) -> Value {
    Value::String(::base64::Engine::encode(
        &::base64::engine::general_purpose::STANDARD,
        bytes,
    ))
}

/// The `X-Amz-Target` prefix the key service routes on.
const KMS_TARGET_PREFIX: &str = "TrentService";

/// One call to the key service on the awsJson1_1 wire.
///
/// `pub(crate)` so the suites of this target that put a key into a state and then assert what
/// Lambda does about it drive the key service through ONE writer of that wire
/// (`tests/all/environment_variable_key_state.rs` is the other caller).
pub(crate) async fn kms_call(router: &MultiServiceRouter, operation: &str, body: &Value) -> Answer {
    let request = http::Request::builder()
        .method("POST")
        .uri("http://kms.us-east-1.amazonaws.com/")
        .header("host", "kms.us-east-1.amazonaws.com")
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 \
             Credential=emulator-test-key/20260101/us-east-1/kms/aws4_request, \
             SignedHeaders=host, Signature=not-verified",
        )
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{KMS_TARGET_PREFIX}.{operation}"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// Reporting a durable execution's DATA decrypts it under the key the function's
/// `DurableConfig` names, so a key the key service reports as unusable refuses the read
/// with the modelled 502 — and a read that asks for no data decrypts nothing and is served.
///
/// The rows differ along what the rule generalises over: which read is made, and whether
/// the request asks for the execution's data at all.
#[tokio::test]
async fn a_read_of_execution_data_is_refused_while_its_key_cannot_be_used() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_with_key_service(&engine);

    // A key this account really holds, and really disables.
    let key_arn = created_key(&router).await;

    let name = "durable-kms";
    let source = "def lambda_handler(event, context):\n    return {'step': 'done'}\n";
    let created_function = create_function(
        &router,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "lambda_function.lambda_handler",
            "DurableConfig": { "KMSKeyArn": key_arn, "ExecutionTimeout": 3_600 },
            "Code": { "ZipFile": base64_of(&package(source)) },
        }),
    )
    .await;
    assert_eq!(
        created_function.status,
        201,
        "expected 201 from CreateFunction, got {} {}",
        created_function.status,
        String::from_utf8_lossy(&created_function.body)
    );
    // The activation the create owes is brought forward and WAITED FOR as a condition, so
    // the invocation below meets a function that is really serving.
    wait_until_active(&engine, &router, "the durable function", name, "$LATEST").await;

    let invoked = send(
        &router,
        "POST",
        &format!("{INVOCATIONS_URI}/{name}/invocations"),
        "application/json",
        br#"{"marker":"kms"}"#.to_vec(),
        &[
            ("x-amz-invocation-type", "Event".to_string()),
            (DURABLE_EXECUTION_NAME_HEADER, "kms-exec".to_string()),
        ],
    )
    .await;
    assert_eq!(
        invoked.status,
        202,
        "expected 202 from Invoke, got {} {}",
        invoked.status,
        String::from_utf8_lossy(&invoked.body)
    );
    let arn = invoked
        .header(DURABLE_EXECUTION_ARN_HEADER)
        .unwrap_or_else(|| panic!("expected the accepted invocation to answer an execution ARN"));

    // While the key is usable every read is served.
    for (case, answer) in [
        ("the execution", get_execution(&router, &arn).await),
        ("its history", get_history(&router, &arn).await),
    ] {
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: expected 200 while the key is usable, got {} {}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
    }

    let disabled = kms_call(&router, "DisableKey", &json!({ "KeyId": key_arn })).await;
    assert_eq!(
        disabled.status,
        200,
        "expected 200 from DisableKey, got {} {}",
        disabled.status,
        String::from_utf8_lossy(&disabled.body)
    );

    // (case, the read, the status it must answer)
    let rows: [(&str, Answer, u16); 3] = [
        ("the execution's data", get_execution(&router, &arn).await, 502),
        ("its history", get_history(&router, &arn).await, 502),
        (
            "the execution's metadata alone",
            read_execution(&router, &arn, "?IncludeExecutionData=false").await,
            200,
        ),
    ];
    for (case, answer, status) in rows {
        assert_eq!(
            answer.status,
            status,
            "case {case:?}: expected {status}, got {} {}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        if status != 502 {
            continue;
        }
        assert_eq!(
            answer.error_code(),
            "KMSDisabledException",
            "case {case:?}: expected the modelled KMS refusal, got {}",
            answer.error_code()
        );
        let message = answer
            .string("Message")
            .or_else(|| answer.string("message"))
            .unwrap_or_default();
        assert!(
            message.contains("the KMS key") && message.contains(&key_arn),
            "case {case:?}: expected the disabled-key sentence to name {key_arn:?}, got \
             {message:?}"
        );
    }
}

/// A router carrying Lambda and the key service on ONE engine, so the key a function's
/// durable configuration names is a key this account really holds — and really disables.
///
/// `pub(crate)` for the same reason [`kms_call`] is: the suites that put a key into a state
/// and then assert what Lambda does about it mount the two services together through ONE
/// caller (`tests/all/invoke_durable_execution_handle.rs` is the other one).
pub(crate) fn router_with_key_service(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = router_on(engine);
    router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
        Arc::clone(engine),
    ));
    router
}

/// A symmetric encryption key this account holds, answered by its ARN.
///
/// `pub(crate)`: see [`router_with_key_service`].
pub(crate) async fn created_key(router: &MultiServiceRouter) -> String {
    let created = kms_call(router, "CreateKey", &json!({})).await;
    assert_eq!(
        created.status,
        200,
        "setup \"CreateKey\": expected 200, got {} {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    created
        .json()
        .pointer("/KeyMetadata/Arn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "setup \"CreateKey\": expected a key ARN, got {}",
                String::from_utf8_lossy(&created.body)
            )
        })
        .to_string()
}

/// The runtime the durable handler below runs on: a managed Python runtime, which is what
/// supplies the durable-execution programming model on a handler's import path.
const DURABLE_RUNTIME: &str = "python3.13";

/// The `State` a function serves invocations in.
const ACTIVE: &str = "Active";

/// The `ExecutionStatus` a run that returned leaves the execution in, and the `EventType`
/// values the start and that end are recorded under (`smithy_query(query="shape",
/// shape="ExecutionStatus")`, `smithy_query(query="shape", shape="EventType")`).
const SUCCEEDED: &str = "SUCCEEDED";
const EXECUTION_STARTED: &str = "ExecutionStarted";
const EXECUTION_SUCCEEDED: &str = "ExecutionSucceeded";

/// Lambda's own fixed sentence for the execution DATA it could not access, one per class of
/// KMS failure a read of that data binds (`smithy_query(query="operation",
/// operation="GetDurableExecution")`: `KMSDisabledException`, `KMSInvalidStateException`,
/// `KMSNotFoundException`, `KMSAccessDeniedException`, each `@httpError(502)`). Every word
/// of the sentence is fixed; the only variable part of the whole message is the key ARN the
/// clause that relays the key service's own answer names, which is the key the function's
/// `DurableConfig` carries.
pub(crate) const DATA_DISABLED_SENTENCE: &str = "Lambda was unable to access your execution \
                                                 data because the KMS key used is disabled. \
                                                 Check the KMS key configured in the \
                                                 function's durable config.";

/// See [`DATA_DISABLED_SENTENCE`]; the state of the key does not admit a decrypt.
pub(crate) const DATA_INVALID_STATE_SENTENCE: &str = "Lambda was unable to access your execution \
                                                      data because the state of the KMS key \
                                                      used is not valid for Decrypt. Check the \
                                                      KMS key configured in the function's \
                                                      durable config.";

/// A handler written against the durable-execution programming model the runtime supplies:
/// it runs one unit of work through `context.step(...)`, so what the execution reports as
/// its result is a value a CHECKPOINTED step really produced.
const DURABLE_STEP_HANDLER: &str = r#"
from aws_durable_execution_sdk_python import durable_execution, durable_step


@durable_step
def charge(context, amount):
    return {"charged": amount}


@durable_execution
def lambda_handler(event, context):
    return context.step(charge(event["amount"]))
"#;

/// The event every invocation below carries.
fn durable_event() -> Value {
    json!({ "amount": 7 })
}

/// What [`DURABLE_STEP_HANDLER`] answers for [`durable_event`].
pub(crate) fn durable_result() -> Value {
    json!({ "charged": 7 })
}

/// A durable function on `name` running [`DURABLE_STEP_HANDLER`], its payload data encrypted
/// under `key_arn`.
pub(crate) async fn create_durable_function(
    router: &MultiServiceRouter,
    name: &str,
    key_arn: &str,
) {
    let created = create_function(
        router,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": DURABLE_RUNTIME,
            "Handler": "lambda_function.lambda_handler",
            "Timeout": 30,
            "DurableConfig": { "KMSKeyArn": key_arn, "ExecutionTimeout": 900 },
            "Code": { "ZipFile": base64_of(&package(DURABLE_STEP_HANDLER)) },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "setup {name:?}: expected 201 from CreateFunction, got {} {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
}

/// `PublishVersion` on `name`, answering the version it minted.
pub(crate) async fn publish_version(router: &MultiServiceRouter, name: &str) -> String {
    let published = json_call(
        router,
        "POST",
        &format!("{INVOCATIONS_URI}/{name}/versions"),
        &json!({}),
    )
    .await;
    assert_eq!(
        published.status,
        201,
        "setup {name:?}: expected 201 from PublishVersion, got {} {}",
        published.status,
        String::from_utf8_lossy(&published.body)
    );
    published.string("Version").unwrap_or_else(|| {
        panic!(
            "setup {name:?}: expected PublishVersion to report a Version, got {}",
            String::from_utf8_lossy(&published.body)
        )
    })
}

/// Bring the activation the clock owes forward and LOOK until Lambda reports `qualifier` of
/// `name` active, which is the state an invocation needs. The wait is on the condition, with
/// a generous deadline, so no case here turns on how fast the host runs.
pub(crate) async fn wait_until_active(
    engine: &Arc<engine::InMemoryEngine>,
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    qualifier: &str,
) {
    make_activation_due(engine);
    let deadline = std::time::Instant::now() + SETTLE_CEILING;
    let mut last = Value::Null;
    while std::time::Instant::now() < deadline {
        last = json_call(
            router,
            "GET",
            &format!("{INVOCATIONS_URI}/{name}/configuration?Qualifier={qualifier}"),
            &Value::Null,
        )
        .await
        .json();
        if last.get("State").and_then(Value::as_str) == Some(ACTIVE) {
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!(
        "case {case:?}: expected {name}:{qualifier} to reach {ACTIVE}, last saw {last}"
    );
}

/// `Invoke` on `reference` (with `query` appended), SYNCHRONOUSLY, naming `execution` as the
/// durable execution the invocation starts.
pub(crate) async fn invoke_durable(
    router: &MultiServiceRouter,
    reference: &str,
    query: &str,
    execution: &str,
) -> Answer {
    send(
        router,
        "POST",
        &format!("{INVOCATIONS_URI}/{reference}/invocations{query}"),
        "application/json",
        serde_json::to_vec(&durable_event()).expect("the event serializes"),
        &[
            ("x-amz-invocation-type", "RequestResponse".to_string()),
            (DURABLE_EXECUTION_NAME_HEADER, execution.to_string()),
        ],
    )
    .await
}

/// The `Message` an answer carries, whichever casing the channel that rendered it used.
pub(crate) fn message_of(answer: &Answer) -> String {
    answer
        .string("Message")
        .or_else(|| answer.string("message"))
        .unwrap_or_default()
}

/// The RESULT payload a served read reports, however that read carries it: `Result` on the
/// execution itself, and the `ExecutionSucceeded` event's own result in a history. `None`
/// when the read carries none — which is what a read asking for no execution data reports.
pub(crate) fn result_payload(body: &Value) -> Option<Value> {
    let payload = body
        .get("Result")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            body.get("Events")
                .and_then(Value::as_array)?
                .iter()
                .find(|event| {
                    event.get("EventType").and_then(Value::as_str) == Some(EXECUTION_SUCCEEDED)
                })?
                .pointer("/ExecutionSucceededDetails/Result/Payload")
                .and_then(Value::as_str)
                .map(str::to_string)
        })?;
    Some(serde_json::from_str(&payload).unwrap_or(Value::Null))
}

/// The `EventType` of each event a history reports, in the order it reported them.
fn event_types(body: &Value) -> Vec<String> {
    body.get("Events")
        .and_then(Value::as_array)
        .map(|events| {
            events
                .iter()
                .map(|event| {
                    event
                        .get("EventType")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One row of the identity table: which identity of the function the invocation names, and
/// the version the execution it starts therefore runs under.
struct Identity {
    /// What makes this row its own case.
    case: &'static str,
    /// The function reference the invocation's path names, from the function's name.
    reference: fn(&str) -> String,
    /// What the invocation's URI carries after that path.
    query: &'static str,
    /// The `Version` the execution must report ("This ensures that all replays during the
    /// execution use the same function version").
    version: &'static str,
}

/// A synchronous invocation of a durable function RUNS its handler through the
/// durable-execution programming model the runtime supplies, so the execution it answers a
/// handle on ends in the terminal status that run reached — and a read that asks for no
/// execution data reports that status with the execution's metadata and none of its
/// payloads.
///
/// The rows differ along what the rule generalises over: which identity of the function the
/// invocation names, and therefore which version the execution runs under.
#[tokio::test]
async fn a_durable_handlers_run_ends_the_execution_whichever_version_was_invoked() {
    let rows = [
        Identity {
            case: "a published version named in the function reference",
            reference: |name| format!("{name}:1"),
            query: "",
            version: "1",
        },
        Identity {
            case: "a published version named as the invocation's qualifier",
            reference: str::to_string,
            query: "?Qualifier=1",
            version: "1",
        },
        Identity {
            case: "the unpublished version",
            reference: str::to_string,
            query: "",
            version: "$LATEST",
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = router_with_key_service(&engine);
        let key_arn = created_key(&router).await;
        let name = format!("durable-identity-{index}");
        create_durable_function(&router, &name, &key_arn).await;
        wait_until_active(&engine, &router, row.case, &name, "$LATEST").await;
        // Every row publishes, so the version a row names exists whether or not its own
        // invocation addresses it.
        let published = publish_version(&router, &name).await;
        assert_eq!(
            published, "1",
            "case {:?}: expected PublishVersion to mint version 1, got {published:?}",
            row.case
        );
        wait_until_active(&engine, &router, row.case, &name, &published).await;

        let execution = format!("exec-identity-{index}");
        let invoked =
            invoke_durable(&router, &(row.reference)(&name), row.query, &execution).await;
        assert_eq!(
            invoked.status,
            200,
            "case {:?}: expected 200 from Invoke, got {} {}",
            row.case,
            invoked.status,
            String::from_utf8_lossy(&invoked.body)
        );
        assert_eq!(
            invoked.header("x-amz-function-error"),
            None,
            "case {:?}: the durable handler must run without faulting, got {}",
            row.case,
            String::from_utf8_lossy(&invoked.body)
        );
        assert_eq!(
            invoked.json(),
            durable_result(),
            "case {:?}: expected the handler's own value as the answer, got {}",
            row.case,
            String::from_utf8_lossy(&invoked.body)
        );
        let arn = invoked
            .header(DURABLE_EXECUTION_ARN_HEADER)
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: the invocation of a durable function must answer an execution \
                     ARN",
                    row.case
                )
            });

        // The read that asks for the data: the terminal status the run reached, and the
        // payloads the execution really carries.
        let read = read_execution(&router, &arn, "?IncludeExecutionData=true").await;
        assert_eq!(
            read.status,
            200,
            "case {:?}: expected 200 from GetDurableExecution, got {} {}",
            row.case,
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        let reported = read.json();
        assert_eq!(
            reported.get("Status").and_then(Value::as_str),
            Some(SUCCEEDED),
            "case {:?}: expected the execution to end {SUCCEEDED}, got {reported}",
            row.case
        );
        assert_eq!(
            reported.get("Version").and_then(Value::as_str),
            Some(row.version),
            "case {:?}: expected the execution to run under version {:?}, got {reported}",
            row.case,
            row.version
        );
        assert_eq!(
            result_payload(&reported),
            Some(durable_result()),
            "case {:?}: expected the step's own value as the execution's result, got \
             {reported}",
            row.case
        );
        assert_eq!(
            reported
                .get("InputPayload")
                .and_then(Value::as_str)
                .map(|payload| serde_json::from_str(payload).unwrap_or(Value::Null)),
            Some(durable_event()),
            "case {:?}: expected the input the invocation carried, got {reported}",
            row.case
        );

        // The read that asks for NO data: the same metadata, and none of the payloads.
        let compact = read_execution(&router, &arn, "?IncludeExecutionData=false").await;
        assert_eq!(
            compact.status,
            200,
            "case {:?}: expected 200 from the metadata-only read, got {} {}",
            row.case,
            compact.status,
            String::from_utf8_lossy(&compact.body)
        );
        let metadata = compact.json();
        assert_eq!(
            metadata
                .get("ExecutionDataIncluded")
                .and_then(Value::as_bool),
            Some(false),
            "case {:?}: expected ExecutionDataIncluded false, got {metadata}",
            row.case
        );
        for member in ["InputPayload", "Result", "Error"] {
            assert!(
                metadata.get(member).is_none(),
                "case {:?}: expected no {member} on a metadata-only read, got {metadata}",
                row.case
            );
        }
        for member in ["Status", "StartTimestamp", "EndTimestamp", "DurableExecutionName"] {
            assert!(
                metadata.get(member).is_some(),
                "case {:?}: expected {member} on a metadata-only read, got {metadata}",
                row.case
            );
        }
        assert_eq!(
            metadata.get("StartTimestamp"),
            reported.get("StartTimestamp"),
            "case {:?}: expected the execution's own StartTimestamp whether the data is \
             asked for, got {metadata}",
            row.case
        );
        assert_eq!(
            metadata
                .pointer("/DurableConfig/KMSKeyArn")
                .and_then(Value::as_str),
            Some(key_arn.as_str()),
            "case {:?}: expected the configured key on the metadata-only read, got \
             {metadata}",
            row.case
        );

        // The history runs from the start of the execution to the end the run gave it, and
        // carries the step's own payload only where the read asks for the data.
        for (asks_for_data, query) in [
            (true, "?IncludeExecutionData=true"),
            (false, "?IncludeExecutionData=false"),
        ] {
            let history = read_history(&router, &arn, query).await;
            assert_eq!(
                history.status,
                200,
                "case {:?} ({query}): expected 200 from GetDurableExecutionHistory, got {} {}",
                row.case,
                history.status,
                String::from_utf8_lossy(&history.body)
            );
            let events = event_types(&history.json());
            assert_eq!(
                (
                    events.first().map(String::as_str),
                    events.last().map(String::as_str)
                ),
                (Some(EXECUTION_STARTED), Some(EXECUTION_SUCCEEDED)),
                "case {:?} ({query}): expected the history to run from {EXECUTION_STARTED} \
                 to {EXECUTION_SUCCEEDED}, got {events:?}",
                row.case
            );
            let carried = result_payload(&history.json());
            assert_eq!(
                carried.is_some(),
                asks_for_data,
                "case {:?} ({query}): expected the event's result payload only where the \
                 read asks for the data, got {carried:?}",
                row.case
            );
        }
    }
}

/// One row of the key-state table: the state the key is moved into, and how a read that asks
/// for the execution's data must then answer.
struct KeyState {
    /// What makes this row its own case.
    case: &'static str,
    /// The key-service call that puts the key into this state, and whether it carries the
    /// pending window `ScheduleKeyDeletion` takes.
    key_call: Option<(&'static str, bool)>,
    /// The modelled 502 a read that asks for the data must be refused with, or `None` while
    /// the data is readable.
    refusal: Option<&'static str>,
    /// Lambda's own sentence for the data it could not access.
    sentence: &'static str,
    /// The exception class the key service stated the refusal under, which the refusal
    /// relays.
    kms_code: &'static str,
    /// What the key service said about the key, after the key's own ARN.
    key_reason: &'static str,
}

/// A read of a durable execution's DATA is served exactly while the key that data is
/// encrypted under can be used, and is otherwise refused with the modelled 502 for the class
/// of failure the key service reported — Lambda's own sentence for the data it could not
/// access followed by the key service's own exception class and sentence. The metadata-only
/// reads decrypt nothing, so they are served whatever state that key is in, and nothing is
/// recorded: the key is asked at each read, so a key that can be used again serves every
/// read again.
///
/// The rows differ along what the rule generalises over: the state the key is in (usable,
/// disabled, scheduled for deletion, the deletion cancelled, enabled again) and which read
/// is made. The key really is in the state a row says it is: it is moved between states with
/// the key service's own operations, over the same engine.
///
/// Two of the four classes the model binds have no row, because no row here could put the
/// key in them: a key the key service holds no record of is deliberately NOT a refusal of
/// this read, and the access-denied class needs the key service to refuse the describe this
/// gate makes, which it does not do for a call made over the in-process seam. Those two
/// sentences are covered by the unit test beside the wording itself.
#[tokio::test]
async fn a_refused_read_of_execution_data_relays_the_key_services_answer() {
    let rows = [
        KeyState {
            case: "a key the key service reports as usable",
            key_call: None,
            refusal: None,
            sentence: "",
            kms_code: "",
            key_reason: "",
        },
        KeyState {
            case: "a disabled key",
            key_call: Some(("DisableKey", false)),
            refusal: Some("KMSDisabledException"),
            sentence: DATA_DISABLED_SENTENCE,
            kms_code: "DisabledException",
            key_reason: "is disabled.",
        },
        KeyState {
            case: "a key scheduled for deletion",
            key_call: Some(("ScheduleKeyDeletion", true)),
            refusal: Some("KMSInvalidStateException"),
            sentence: DATA_INVALID_STATE_SENTENCE,
            kms_code: "KMSInvalidStateException",
            key_reason: "is pending deletion.",
        },
        KeyState {
            // Cancelling a scheduled deletion leaves the key DISABLED, so the class the
            // refusal relays changes with the key's state.
            case: "the scheduled deletion cancelled",
            key_call: Some(("CancelKeyDeletion", false)),
            refusal: Some("KMSDisabledException"),
            sentence: DATA_DISABLED_SENTENCE,
            kms_code: "DisabledException",
            key_reason: "is disabled.",
        },
        KeyState {
            case: "the key enabled again",
            key_call: Some(("EnableKey", false)),
            refusal: None,
            sentence: "",
            kms_code: "",
            key_reason: "",
        },
    ];

    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_with_key_service(&engine);
    let key_arn = created_key(&router).await;
    let name = "durable-data-key";
    create_durable_function(&router, name, &key_arn).await;
    wait_until_active(&engine, &router, "setup", name, "$LATEST").await;
    let version = publish_version(&router, name).await;
    wait_until_active(&engine, &router, "setup", name, &version).await;

    let invoked =
        invoke_durable(&router, &format!("{name}:{version}"), "", "exec-data-key").await;
    assert_eq!(
        invoked.status,
        200,
        "setup: expected 200 from Invoke, got {} {}",
        invoked.status,
        String::from_utf8_lossy(&invoked.body)
    );
    let arn = invoked
        .header(DURABLE_EXECUTION_ARN_HEADER)
        .unwrap_or_else(|| panic!("setup: the invocation must answer an execution ARN"));
    // The instant the execution started is the one every later read reports, whatever the
    // key's state: it is metadata, not data.
    let started = read_execution(&router, &arn, "?IncludeExecutionData=false")
        .await
        .json()
        .get("StartTimestamp")
        .cloned()
        .unwrap_or(Value::Null);
    assert!(
        !started.is_null(),
        "setup: expected the execution to report a StartTimestamp"
    );

    for row in &rows {
        if let Some((operation, with_window)) = row.key_call {
            let mut body = json!({ "KeyId": key_arn });
            if with_window {
                // The floor of `ScheduleKeyDeletionRequest$PendingWindowInDays`
                // (`@range(min: 7, max: 30)`).
                body["PendingWindowInDays"] = json!(7);
            }
            let answered = kms_call(&router, operation, &body).await;
            assert_eq!(
                answered.status,
                200,
                "case {:?}: expected 200 from {operation}, got {} {}",
                row.case,
                answered.status,
                String::from_utf8_lossy(&answered.body)
            );
        }

        // Every read that asks for the execution's data, including the one that asks for it
        // by leaving `IncludeExecutionData` at its documented default of true.
        let data_reads = [
            (
                "GetDurableExecution, asking for the data by default",
                read_execution(&router, &arn, "").await,
            ),
            (
                "GetDurableExecution, asking for the data",
                read_execution(&router, &arn, "?IncludeExecutionData=true").await,
            ),
            (
                "GetDurableExecutionHistory, asking for the data",
                read_history(&router, &arn, "?IncludeExecutionData=true").await,
            ),
        ];
        for (read, answer) in data_reads {
            let case = format!("{} read by {read}", row.case);
            match row.refusal {
                None => {
                    assert_eq!(
                        answer.status,
                        200,
                        "case {case:?}: expected 200 while the key is usable, got {} {}",
                        answer.status,
                        String::from_utf8_lossy(&answer.body)
                    );
                    assert_eq!(
                        result_payload(&answer.json()),
                        Some(durable_result()),
                        "case {case:?}: expected the execution's own result, got {}",
                        String::from_utf8_lossy(&answer.body)
                    );
                }
                Some(expected) => {
                    assert_eq!(
                        answer.status,
                        502,
                        "case {case:?}: expected the modelled 502, got {} {}",
                        answer.status,
                        String::from_utf8_lossy(&answer.body)
                    );
                    assert_eq!(
                        answer.error_code(),
                        expected,
                        "case {case:?}: expected {expected}, got {}",
                        answer.error_code()
                    );
                    let expected_message = format!(
                        "{} KMS Exception: {} KMS Message: {key_arn} {}",
                        row.sentence, row.kms_code, row.key_reason
                    );
                    assert_eq!(
                        message_of(&answer),
                        expected_message,
                        "case {case:?}: expected the refusal to state the data it could not \
                         access and relay the key service's answer, got {:?}",
                        message_of(&answer)
                    );
                }
            }
        }

        // The metadata-only reads decrypt nothing, so they are served whatever state the key
        // is in — with the status the run reached, the instant it started, the configured key
        // and none of the payloads.
        let compact = read_execution(&router, &arn, "?IncludeExecutionData=false").await;
        assert_eq!(
            compact.status,
            200,
            "case {:?}: expected the metadata-only read to be served, got {} {}",
            row.case,
            compact.status,
            String::from_utf8_lossy(&compact.body)
        );
        let metadata = compact.json();
        assert_eq!(
            metadata.get("Status").and_then(Value::as_str),
            Some(SUCCEEDED),
            "case {:?}: expected the metadata-only read to report {SUCCEEDED}, got {metadata}",
            row.case
        );
        assert_eq!(
            metadata
                .get("ExecutionDataIncluded")
                .and_then(Value::as_bool),
            Some(false),
            "case {:?}: expected ExecutionDataIncluded false, got {metadata}",
            row.case
        );
        for member in ["InputPayload", "Result", "Error"] {
            assert!(
                metadata.get(member).is_none(),
                "case {:?}: expected no {member} on a metadata-only read, got {metadata}",
                row.case
            );
        }
        assert_eq!(
            metadata.get("StartTimestamp"),
            Some(&started),
            "case {:?}: expected the execution's original StartTimestamp, got {metadata}",
            row.case
        );
        assert_eq!(
            metadata
                .pointer("/DurableConfig/KMSKeyArn")
                .and_then(Value::as_str),
            Some(key_arn.as_str()),
            "case {:?}: expected the configured key to stay reported, got {metadata}",
            row.case
        );

        let history = read_history(&router, &arn, "?IncludeExecutionData=false").await;
        assert_eq!(
            history.status,
            200,
            "case {:?}: expected the metadata-only history to be served, got {} {}",
            row.case,
            history.status,
            String::from_utf8_lossy(&history.body)
        );
        let events = event_types(&history.json());
        assert_eq!(
            (
                events.first().map(String::as_str),
                events.last().map(String::as_str)
            ),
            (Some(EXECUTION_STARTED), Some(EXECUTION_SUCCEEDED)),
            "case {:?}: expected the metadata-only history to run from {EXECUTION_STARTED} \
             to {EXECUTION_SUCCEEDED}, got {events:?}",
            row.case
        );
        assert_eq!(
            result_payload(&history.json()),
            None,
            "case {:?}: expected no payload on a metadata-only history, got {}",
            row.case,
            String::from_utf8_lossy(&history.body)
        );
    }
}
