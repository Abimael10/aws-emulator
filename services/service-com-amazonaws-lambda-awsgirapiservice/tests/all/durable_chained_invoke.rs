// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a durable execution's CHAINED INVOCATION: the `context.invoke(…)` a
//! durable handler runs ANOTHER function with, the `CHAINED_INVOKE` operation it is on the
//! execution, and the refusal of one whose target resolves to nothing.
//!
//! The rule under test: **a durable handler's `context.invoke(function, payload)` invokes that
//! function as one `CHAINED_INVOKE` operation of the execution — the invoked function is handed
//! the payload the handler gave, RUNS, writes its own log and answers the handler — and the
//! operation is recorded on the execution's history like every other one: `ChainedInvokeStarted`
//! naming the function the operation invoked and carrying the input it carried, then
//! `ChainedInvokeSucceeded` carrying the answer. A target the service cannot resolve is refused
//! exactly as it refuses a client's own `Invoke` of that name: the operation FAILS with the
//! service's own sentence — the fixed `Function not found: ` followed by the ARN the invocation
//! resolved, qualified by the version it would have run — the history takes
//! `ChainedInvokeFailed` carrying that error, and the error reaches the handler, which fails the
//! whole execution with it unless it catches it.**
//!
//! Three things have to agree for the rows to hold — Lambda runs the durable handler, serves the
//! chained `Invoke` the handler makes and delivers the invoked function's log, and CloudWatch
//! Logs holds the group that log reaches — so the suite drives real HTTP through a
//! `MultiServiceRouter` with both mounted on ONE engine, and asserts only what
//! `GetDurableExecution`, `GetDurableExecutionHistory` and `FilterLogEvents` report.
//!
//! The rows differ along what the rule generalises over: whether the target EXISTS, the
//! documented `FunctionName` form the handler names it by (the bare name, the full ARN, the
//! partial ARN), the shape of the payload it hands over (an object, a list, a scalar), whether
//! the operation was given a name of its own, and whether the handler CATCHES the error of a
//! chained invocation that failed.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{
    create_function, json_call, make_activation_due, send, zip_file, Answer, ACCOUNT, REGION, ROLE,
};
use crate::invocation_log_delivery::searched;
use crate::synchronous_invoke_run::package;

/// Where `Invoke` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The request headers `Invoke$DurableExecutionName` and `Invoke$InvocationType` are bound to,
/// and the response header `Invoke$DurableExecutionArn` is bound to.
const NAME_HEADER: &str = "x-amz-durable-execution-name";
const INVOCATION_TYPE_HEADER: &str = "x-amz-invocation-type";
const ARN_HEADER: &str = "x-amz-durable-execution-arn";

/// The handler every package below exposes.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The runtime both functions run.
const RUNTIME: &str = "python3.12";

/// The `DurableConfig$ExecutionTimeout` the durable function runs under, in seconds: far longer
/// than any row's own work, so no execution times out as a whole.
const EXECUTION_TIMEOUT: u64 = 3_600;

/// The function `Timeout`, in seconds: generous, so nothing here turns on how fast the host
/// runs.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long this suite keeps looking for something the emulator owes — a run that has not
/// finished, an event the history has not taken yet, a log record not yet delivered — before it
/// reports that it never arrived. Generous: it bounds a POLL, so a loaded host waits longer
/// rather than failing.
const PATIENCE: Duration = Duration::from_secs(180);

/// How long between two looks of such a poll.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The `ExecutionStatus` values a settled execution carries
/// (`smithy_query(query="shape",shape="ExecutionStatus")`).
const RUNNING: &str = "RUNNING";
const SUCCEEDED: &str = "SUCCEEDED";
const FAILED: &str = "FAILED";

/// The `EventType` values a chained invocation's own history events carry
/// (`smithy_query(query="shape",shape="EventType")`).
const CHAINED_INVOKE_STARTED: &str = "ChainedInvokeStarted";
const CHAINED_INVOKE_SUCCEEDED: &str = "ChainedInvokeSucceeded";
const CHAINED_INVOKE_FAILED: &str = "ChainedInvokeFailed";

/// The fixed text of the sentence a target that resolves to nothing is refused with, and the
/// error class the service refuses such an `Invoke` with.
const NOT_FOUND_PREFIX: &str = "Function not found: ";
const NOT_FOUND_CODE: &str = "ResourceNotFoundException";

/// How this service spells the unpublished version, which is the version an identifier carrying
/// no qualifier resolves to.
const LATEST: &str = "$LATEST";

/// The error class the durable SDK raises an operation whose work did not answer as, which is
/// what the execution's own `Error` reports for an uncaught one.
const CHAIN_ERROR_TYPE: &str = "CallableRuntimeError";

/// The function the chained invocations of this suite invoke, and the name no function is held
/// under that the refused rows invoke.
const TARGET: &str = "chain-target";
const ABSENT: &str = "chain-absent";

/// The durable function whose handler takes the chained invocation.
const CALLER: &str = "chain-caller";

/// By default "Lambda functions send logs to a default log group named
/// `/aws/lambda/<function name>`" (`smithy_query(query="doc",shape="LoggingConfig",
/// member="LogGroup")`), which is where the INVOKED function's own log goes.
const DEFAULT_GROUP_PREFIX: &str = "/aws/lambda/";

/// The marker the invoked function prints, ahead of the event it was handed: what proves the
/// payload the caller gave reached the function that ran.
const TARGET_MARKER: &str = "target-saw ";

/// The durable handler every row runs: it takes ONE chained invocation of the function the event
/// names, carrying the payload the event carries, and reports what came back — or, for a row
/// that asks it to, what the chained invocation failed with.
const CALLER_SOURCE: &str = r#"
from aws_durable_execution_sdk_python import CallableRuntimeError, durable_execution


@durable_execution
def lambda_handler(event, context):
    try:
        answered = context.invoke(
            event["target"], event["payload"], name=event.get("name")
        )
    except CallableRuntimeError as error:
        if not event.get("catch"):
            raise
        return {"ended": "caught", "message": str(error)}
    return {"ended": "answered", "answered": answered}
"#;

/// The handler the INVOKED function runs: it prints the event it was handed — which is what
/// reaches its own log group — and answers it, so what the caller reads back is this function's
/// own answer.
const TARGET_SOURCE: &str = r#"
import json


def lambda_handler(event, context):
    print("target-saw " + json.dumps(event, sort_keys=True, separators=(",", ":")))
    return {"echoed": event, "ran": context.function_name}
"#;

/// Lambda and CloudWatch Logs over ONE shared engine, as `core/router/src/lib.rs` mounts them:
/// the chained invocation the handler makes and the delivery of the invoked function's log are
/// both dispatched back into this same router.
fn emulator(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            engine,
        )),
    );
    router
}

/// Create one function running `source`, with the `DurableConfig` a durable function carries
/// when `durable` is set.
async fn given_function(router: &MultiServiceRouter, name: &str, source: &str, durable: bool) {
    let mut request = json!({
        "FunctionName": name,
        "Role": ROLE,
        "Runtime": RUNTIME,
        "Handler": HANDLER,
        "Timeout": FUNCTION_TIMEOUT,
        "Code": { "ZipFile": zip_file(&package(source)) },
    });
    if durable {
        request["DurableConfig"] = json!({ "ExecutionTimeout": EXECUTION_TIMEOUT });
    }
    let created = create_function(router, &request).await;
    assert_eq!(
        created.status,
        201,
        "CreateFunction {name:?} must answer 201, got {} {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
}

/// Start one durable execution ASYNCHRONOUSLY — the run happens off the response path — and
/// answer the ARN it was registered under.
async fn start_execution(
    router: &MultiServiceRouter,
    execution: &str,
    event: &Value,
) -> String {
    let invoked = send(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{CALLER}/invocations"),
        "application/json",
        serde_json::to_vec(event).expect("the event serializes"),
        &[
            (NAME_HEADER, execution.to_string()),
            (INVOCATION_TYPE_HEADER, "Event".to_string()),
        ],
    )
    .await;
    assert_eq!(
        invoked.status,
        202,
        "an asynchronous invocation of {CALLER:?} must be accepted, got {} {}",
        invoked.status,
        String::from_utf8_lossy(&invoked.body)
    );
    invoked
        .header(ARN_HEADER)
        .unwrap_or_else(|| panic!("the invocation of a durable function must answer its ARN"))
}

/// `GetDurableExecution` on `arn`. The label is not greedy, so the ARN's slashes travel
/// percent-encoded exactly as an SDK encodes them.
async fn get_execution(router: &MultiServiceRouter, arn: &str) -> Answer {
    json_call(
        router,
        "GET",
        &format!("/2025-12-01/durable-executions/{}", arn.replace('/', "%2F")),
        &Value::Null,
    )
    .await
}

/// The execution `arn` once it has SETTLED, looked for again until it has or [`PATIENCE`] runs
/// out.
async fn settled(router: &MultiServiceRouter, arn: &str, case: &str) -> Value {
    let deadline = Instant::now() + PATIENCE;
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let read = get_execution(router, arn).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: GetDurableExecution must answer 200, got {} {}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        last = read.json();
        if last["Status"].as_str() != Some(RUNNING) {
            return last;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("case {case:?}: expected the execution to settle, got {last}");
}

/// Every event of `arn`'s history, in the order it took them.
async fn history(router: &MultiServiceRouter, arn: &str, case: &str) -> Vec<Value> {
    let read = json_call(
        router,
        "GET",
        &format!(
            "/2025-12-01/durable-executions/{}/history",
            arn.replace('/', "%2F")
        ),
        &Value::Null,
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetDurableExecutionHistory must answer 200, got {} {}",
        read.status,
        String::from_utf8_lossy(&read.body)
    );
    read.json()["Events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The one event of `events` whose `EventType` is `event_type`.
fn event_of(events: &[Value], event_type: &str, case: &str) -> Value {
    events
        .iter()
        .find(|event| event["EventType"].as_str() == Some(event_type))
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a {event_type} event in the history, got {events:?}")
        })
}

/// The JSON a member holding JSON TEXT carries — a result, an input payload — and `Null` for a
/// member holding none.
fn carried(held: &Value) -> Value {
    held.as_str()
        .map(|text| serde_json::from_str(text).unwrap_or(Value::Null))
        .unwrap_or(Value::Null)
}

/// Whether the target's own log group carries the line it prints for `payload`, looked for again
/// until it does or [`PATIENCE`] runs out. The delivery is owed by the emulator, so this waits
/// for the CONDITION.
async fn target_logged(router: &MultiServiceRouter, case: &str, payload: &Value) -> bool {
    let group = format!("{DEFAULT_GROUP_PREFIX}{TARGET}");
    let expected = format!("{TARGET_MARKER}{payload}");
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        if searched(router, case, &group)
            .await
            .iter()
            .any(|line| line.contains(&expected))
        {
            return true;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    false
}

/// One row: how the handler names the function it invokes, what it hands over, and what the
/// chained invocation must come to.
struct Row {
    case: &'static str,
    /// The `FunctionName` the handler's `context.invoke` names, in one of the documented forms.
    reference: String,
    /// The payload the handler hands over.
    payload: Value,
    /// The name the operation is given, when the row gives one.
    name: Option<&'static str>,
    /// Whether the function the reference names EXISTS.
    exists: bool,
    /// Whether the handler CATCHES the error of a chained invocation that failed.
    catches: bool,
}

/// The rows: a target that exists, addressed by each documented form and handed each shape of
/// payload, and a target that does not, addressed by each of those forms — the last of them
/// caught by the handler instead of failing the execution.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an object payload, the target named by its bare name",
            reference: TARGET.to_string(),
            payload: json!({ "items": [1, 2, 3], "order": "o-1" }),
            name: Some("charge-the-order"),
            exists: true,
            catches: false,
        },
        Row {
            case: "a list payload, the target named by its full ARN",
            reference: format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{TARGET}"),
            payload: json!([1, "two", { "three": true }]),
            name: None,
            exists: true,
            catches: false,
        },
        Row {
            case: "a scalar payload, the target named by its partial ARN",
            reference: format!("{ACCOUNT}:function:{TARGET}"),
            payload: json!(42),
            name: Some("probe"),
            exists: true,
            catches: false,
        },
        Row {
            case: "a function never created, named by its bare name",
            reference: ABSENT.to_string(),
            payload: json!({ "order": "o-2" }),
            name: Some("charge-the-order"),
            exists: false,
            catches: false,
        },
        Row {
            case: "a function never created, named by its full ARN",
            reference: format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{ABSENT}"),
            payload: json!({ "order": "o-3" }),
            name: None,
            exists: false,
            catches: false,
        },
        Row {
            case: "a function never created, caught by the handler",
            reference: format!("{ACCOUNT}:function:{ABSENT}"),
            payload: json!({ "order": "o-4" }),
            name: Some("charge-the-order"),
            exists: false,
            catches: true,
        },
    ]
}

/// RULE: a chained invocation invokes the function it names with the payload it carries, is
/// recorded on the execution's history, and fails with the service's own refusal for a target
/// that resolves to nothing.
#[tokio::test]
async fn a_chained_invocation_runs_its_target_and_is_recorded_on_the_execution() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator(&engine);
    given_function(&router, TARGET, TARGET_SOURCE, false).await;
    given_function(&router, CALLER, CALLER_SOURCE, true).await;
    make_activation_due(&engine);

    // The resource a chained invocation of the absent name resolves: the function's own ARN in
    // this Region, qualified by the version the invocation would have run.
    let not_found = format!(
        "{NOT_FOUND_PREFIX}arn:aws:lambda:{REGION}:{ACCOUNT}:function:{ABSENT}:{LATEST}"
    );

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let mut event = json!({ "target": row.reference, "payload": row.payload });
        if let Some(name) = row.name {
            event["name"] = json!(name);
        }
        if row.catches {
            event["catch"] = json!(true);
        }
        let execution = format!("chain-{index}");
        let arn = start_execution(&router, &execution, &event).await;
        let reported = settled(&router, &arn, case).await;
        let events = history(&router, &arn, case).await;

        // Whatever the chain came to, the operation itself is on the history, naming the
        // function it invoked and carrying the input it carried.
        let started = event_of(&events, CHAINED_INVOKE_STARTED, case);
        assert_eq!(
            started["ChainedInvokeStartedDetails"]["FunctionName"].as_str(),
            Some(row.reference.as_str()),
            "case {case:?}: the started event names the function the operation invoked, got \
             {started}"
        );
        assert_eq!(
            carried(&started["ChainedInvokeStartedDetails"]["Input"]["Payload"]),
            row.payload,
            "case {case:?}: the started event carries the input the invocation carried, got \
             {started}"
        );
        assert_eq!(
            started["Name"].as_str(),
            row.name,
            "case {case:?}: the started event reports the name the operation was given, got \
             {started}"
        );

        if row.exists {
            // The invoked function RAN: it was handed the payload, answered it, and the
            // handler read that answer back.
            assert_eq!(
                reported["Status"].as_str(),
                Some(SUCCEEDED),
                "case {case:?}: expected the execution to succeed, got {reported}"
            );
            assert_eq!(
                carried(&reported["Result"]),
                json!({
                    "ended": "answered",
                    "answered": { "echoed": row.payload, "ran": TARGET },
                }),
                "case {case:?}: expected the handler to read the invoked function's own \
                 answer, got {reported}"
            );
            let succeeded = event_of(&events, CHAINED_INVOKE_SUCCEEDED, case);
            assert_eq!(
                carried(&succeeded["ChainedInvokeSucceededDetails"]["Result"]["Payload"]),
                json!({ "echoed": row.payload, "ran": TARGET }),
                "case {case:?}: the succeeded event carries what the invoked function \
                 answered, got {succeeded}"
            );
            assert!(
                target_logged(&router, case, &row.payload).await,
                "case {case:?}: expected the invoked function's own log group to carry the \
                 payload it was handed ({})",
                row.payload
            );
            continue;
        }

        // Nothing resolves for the target: the operation failed with the service's own
        // refusal, and the history carries that error under the class the service named it
        // with.
        let failed = event_of(&events, CHAINED_INVOKE_FAILED, case);
        let error = failed["ChainedInvokeFailedDetails"]["Error"]["Payload"].clone();
        assert_eq!(
            error["ErrorMessage"].as_str(),
            Some(not_found.as_str()),
            "case {case:?}: the failed event carries the service's own refusal, got {failed}"
        );
        assert_eq!(
            error["ErrorType"].as_str(),
            Some(NOT_FOUND_CODE),
            "case {case:?}: the failed event names the class the service refused it with, got \
             {failed}"
        );

        if row.catches {
            // The handler caught it, so the execution ran to its own end carrying what the
            // chain failed with.
            assert_eq!(
                reported["Status"].as_str(),
                Some(SUCCEEDED),
                "case {case:?}: expected the execution to run to its own end, got {reported}"
            );
            assert_eq!(
                carried(&reported["Result"]),
                json!({
                    "ended": "caught",
                    "message": not_found,
                }),
                "case {case:?}: expected the handler to be told what the chain failed with, \
                 got {reported}"
            );
            continue;
        }

        // Uncaught, so the EXECUTION fails with that very error.
        assert_eq!(
            reported["Status"].as_str(),
            Some(FAILED),
            "case {case:?}: expected the execution to fail, got {reported}"
        );
        assert_eq!(
            reported["Error"]["ErrorMessage"].as_str(),
            Some(not_found.as_str()),
            "case {case:?}: expected the execution's error to name the target it could not \
             invoke, got {reported}"
        );
        assert_eq!(
            reported["Error"]["ErrorType"].as_str(),
            Some(CHAIN_ERROR_TYPE),
            "case {case:?}: expected the error the durable SDK raises a failed chain as, got \
             {reported}"
        );
    }
}
