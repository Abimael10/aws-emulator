// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the HANDLE an `Invoke` of a durable function answers.
//!
//! The rule under test: every invocation Lambda admits to a durable function — one whose
//! resolved configuration carries a `DurableConfig` — or one that names a
//! `DurableExecutionName` starts a durable execution and answers the ARN that execution is
//! addressed by in `X-Amz-Durable-Execution-Arn` ("The ARN of the durable execution that
//! was started. This is returned when invoking a durable function and provides a unique
//! identifier for tracking the execution"), whatever the invocation type the call named and
//! whether or not the call named the execution; an invocation that starts none answers no
//! ARN at all. That ARN is a handle `GetDurableExecution` RESOLVES to the execution the
//! invocation started — at the operation's modelled default as much as on a metadata-only
//! read — and the metadata that read reports is served whatever state the key the
//! function's `DurableConfig` names is in, because reporting metadata decrypts nothing.
//!
//! A client with no handle cannot poll the execution it just started, so every row here
//! reads the ARN the invocation answered back through `GetDurableExecution` over real HTTP:
//! an ARN that resolves to nothing fails here.
//!
//! A SYNCHRONOUS repeat of a live execution name is the same invocation answered again
//! rather than a second run: "If you invoke a durable function using a name that already
//! exists with the same payload, Lambda returns the existing execution instead of creating
//! a duplicate", so the repeat answers the same handle and the outcome the execution
//! already came to, leaves exactly one `ExecutionStarted` event in its history, and a
//! repeat whose payload differs is the modelled `DurableExecutionAlreadyStartedException`.
//!
//! The rows differ along what the rule generalises over: the invocation type (the default
//! one the request leaves unsaid, the synchronous one it names, the asynchronous one, the
//! dry run that invokes nothing), whether the function carries a `DurableConfig`, the
//! qualifier the call resolves (the unpublished version, a published version, an alias),
//! whether the request names the execution and with what name, the payload, and how the
//! run the handle is on ends (a return, a raise).
//!
//! The deployment package writer is `synchronous_invoke_run.rs`'s and the key service is
//! driven through `durable_execution_completion.rs`'s caller, so this target keeps one
//! writer of each.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::durable_execution_completion::{
    created_key, kms_call, router_with_key_service, wait_until_active,
};
// The modelled facts this suite shares with the start suite — the bound headers, the ARN
// infix, `DurableExecutionName`'s published bounds and the ARN spellings — are read from
// there rather than spelled a second time here.
use crate::durable_execution_start::{
    function_arn, is_execution_name, versioned_arn, ARN_INFIX, DURABLE_EXECUTION_ARN_HEADER,
    DURABLE_EXECUTION_NAME_HEADER, KMS_KEY, NAME_MAX, NAME_MIN, STATUS_RUNNING, VERSION_LATEST,
};
use crate::function_lifecycle::{
    create_function, json_call, router_on, send, zip_file, Answer, ROLE,
};
use crate::synchronous_invoke_run::package;

/// Where `Invoke` is bound, from `smithy_query(query="operation", operation="Invoke")`
/// (`POST /2015-03-31/functions/{FunctionName}/invocations`).
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The request header `Invoke$InvocationType` is bound to.
const INVOCATION_TYPE_HEADER: &str = "x-amz-invocation-type";

/// The response header `Invoke$FunctionError` is bound to.
const FUNCTION_ERROR_HEADER: &str = "x-amz-function-error";

/// The `FunctionError` signal an invocation whose handler raised carries.
const FUNCTION_ERROR_UNHANDLED: &str = "Unhandled";

/// `ExecutionStatus` values this suite expects beyond the one a fresh execution carries,
/// from `smithy_query(query="shape", shape="ExecutionStatus")` (`RUNNING | SUCCEEDED |
/// FAILED | TIMED_OUT | STOPPED`).
const STATUS_SUCCEEDED: &str = "SUCCEEDED";
const STATUS_FAILED: &str = "FAILED";

/// The `EventType` an execution's start is recorded under, from
/// `smithy_query(query="shape", shape="EventType")`.
const EXECUTION_STARTED: &str = "ExecutionStarted";

/// The runtime the handler below runs on, and where the package writer puts its source.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "lambda_function.lambda_handler";

/// A handler that ECHOES its event, so what the execution reports as its result is a value
/// the run really produced — and that RAISES for the one event that asks it to, so a row
/// can drive a run that fails.
const HANDLER_SOURCE: &str = r#"
def lambda_handler(event, context):
    if isinstance(event, dict) and event.get("fail"):
        raise ValueError("the handler was asked to fail")
    return event
"#;

/// The `ErrorType` the raise above is reported under.
const RAISED_ERROR_TYPE: &str = "ValueError";

/// `Invoke` on the function `reference` names, with `query` appended, carrying `payload`,
/// naming `invocation_type` when the row names one and `execution` when it names one.
async fn invoke(
    router: &MultiServiceRouter,
    reference: &str,
    query: &str,
    invocation_type: Option<&str>,
    execution: Option<&str>,
    payload: &[u8],
) -> Answer {
    let mut headers = Vec::new();
    if let Some(kind) = invocation_type {
        headers.push((INVOCATION_TYPE_HEADER, kind.to_string()));
    }
    if let Some(name) = execution {
        headers.push((DURABLE_EXECUTION_NAME_HEADER, name.to_string()));
    }
    send(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{reference}/invocations{query}"),
        "application/json",
        payload.to_vec(),
        &headers,
    )
    .await
}

/// `GetDurableExecution` on one handle, with `query` appended — which is how a read states
/// whether it asks for the execution's own data (`IncludeExecutionData`, whose modelled
/// default is `true`). The label is not greedy, so the slashes the ARN carries are
/// percent-encoded in the path exactly as an SDK encodes them.
async fn get_execution(router: &MultiServiceRouter, arn: &str, query: &str) -> Answer {
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

/// `GetDurableExecutionHistory` on one handle.
async fn get_history(router: &MultiServiceRouter, arn: &str) -> Answer {
    json_call(
        router,
        "GET",
        &format!(
            "/2025-12-01/durable-executions/{}/history",
            arn.replace('/', "%2F")
        ),
        &Value::Null,
    )
    .await
}

/// How many `ExecutionStarted` events a history answer reports, which is how many times an
/// execution was STARTED under the name the handle addresses.
fn starts(history: &Answer) -> usize {
    history
        .json()
        .get("Events")
        .and_then(Value::as_array)
        .map(|events| {
            events
                .iter()
                .filter(|event| {
                    event.get("EventType").and_then(Value::as_str) == Some(EXECUTION_STARTED)
                })
                .count()
        })
        .unwrap_or_default()
}

/// The query that asks for the execution's METADATA alone.
const METADATA_ONLY: &str = "?IncludeExecutionData=false";

/// The function part of a handle: the function's ARN qualified by the version the
/// execution runs under, which the model's `DurableExecutionArn` pattern requires for every
/// version, `$LATEST` included — so it is not the `NameSpacedFunctionArn` spelling a read
/// REPORTS for the function (`versioned_arn`), which leaves `$LATEST` unqualified.
fn handle_function_arn(name: &str, version: &str) -> String {
    format!("{}:{version}", function_arn(name))
}

/// The execution's NAME and id, read out of the handle `arn` by the form the model's
/// `DurableExecutionArn` pattern admits: the function's versioned ARN, the infix, the name
/// and the id. Panics with the case when the handle is not of that form, which is the
/// failure a caller needs to see.
fn handle_parts(case: &str, arn: &str, function_arn: &str) -> (String, String) {
    let prefix = format!("{function_arn}{ARN_INFIX}");
    let tail = arn.strip_prefix(&prefix).unwrap_or_else(|| {
        panic!("case {case:?}: expected a handle under {prefix:?}, got {arn:?}")
    });
    let (name, id) = tail
        .split_once('/')
        .unwrap_or_else(|| panic!("case {case:?}: expected a name and an id in {arn:?}"));
    assert!(
        is_execution_name(name),
        "case {case:?}: the execution name in {arn:?} must satisfy the modelled pattern and \
         bounds, got {name:?}"
    );
    // The pattern admits the same character class for the id as for the name, and the id
    // this emulator mints (a UUID's hexadecimal) sits inside the name's published length,
    // so one check covers both segments.
    assert!(
        is_execution_name(id),
        "case {case:?}: the execution id in {arn:?} must satisfy the modelled pattern, got \
         {id:?}"
    );
    (name.to_string(), id.to_string())
}

/// Create the function `name` on `router` running [`HANDLER_SOURCE`], publish a version and
/// point an alias at it so every qualifier form resolves, and WAIT until each identity an
/// invocation below addresses is serving. `key_arn` gives it the `DurableConfig` that makes
/// it a durable function; `None` leaves it a plain one.
async fn create_invokable_function(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    name: &str,
    key_arn: Option<&str>,
) {
    let mut body = json!({
        "FunctionName": name,
        "Role": ROLE,
        "Runtime": RUNTIME,
        "Handler": HANDLER,
        "Timeout": 30,
        "Publish": true,
        "Code": { "ZipFile": zip_file(&package(HANDLER_SOURCE)) },
    });
    if let Some(key_arn) = key_arn {
        body["DurableConfig"] = json!({ "KMSKeyArn": key_arn, "ExecutionTimeout": 900 });
    }
    let created = create_function(router, &body).await;
    assert_eq!(
        created.status,
        201,
        "setup {case:?}: expected 201 from CreateFunction, got {} {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    let alias = json_call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/aliases"),
        &json!({ "Name": "live", "FunctionVersion": "1" }),
    )
    .await;
    assert_eq!(
        alias.status,
        201,
        "setup {case:?}: expected 201 from CreateAlias, got {} {}",
        alias.status,
        String::from_utf8_lossy(&alias.body)
    );
    for qualifier in [VERSION_LATEST, "1"] {
        wait_until_active(engine, router, case, name, qualifier).await;
    }
}

/// One row of the handle table: how the invocation is made, whether it must answer a
/// handle, and the version the execution it starts runs under.
struct HandleCase {
    case: &'static str,
    /// Whether the invocation addresses the DURABLE function or the plain one.
    durable: bool,
    /// The `InvocationType` the request names, or `None` for the request that names none —
    /// "By default, Lambda invokes your function synchronously".
    invocation_type: Option<&'static str>,
    query: &'static str,
    requested_name: Option<String>,
    payload: &'static [u8],
    version: &'static str,
    status: u16,
    /// Whether this invocation starts a durable execution at all, and so whether it must
    /// answer a handle.
    starts_execution: bool,
    /// Whether the run that decides the execution happened IN this call, which is what a
    /// synchronous invocation promises ("Keep the connection open until the function
    /// returns a response"); an accepted event is delivered off the response path.
    ran_in_the_call: bool,
}

/// The rows: the default invocation type and the synchronous one named outright, at each
/// qualifier form, with a name at each published bound, with a name carrying every
/// character class the pattern admits and with none at all; the asynchronous invocation
/// whose answer "only includes a status code" and this handle; the dry run, which
/// "[v]alidate[s] parameter values" and invokes nothing; and the plain function, which
/// starts an execution only for the invocation that NAMES one.
fn handle_cases() -> Vec<HandleCase> {
    vec![
        HandleCase {
            case: "the default invocation type, with no header at all",
            durable: true,
            invocation_type: None,
            query: "",
            requested_name: Some("a".repeat(NAME_MIN)),
            payload: br#"{"marker":"default"}"#,
            version: VERSION_LATEST,
            status: 200,
            starts_execution: true,
            ran_in_the_call: true,
        },
        HandleCase {
            case: "a synchronous invocation on a published version",
            durable: true,
            invocation_type: Some("RequestResponse"),
            query: "?Qualifier=1",
            requested_name: Some("Exec-9_name".to_string()),
            payload: br#"{"marker":"handle"}"#,
            version: "1",
            status: 200,
            starts_execution: true,
            ran_in_the_call: true,
        },
        HandleCase {
            case: "a synchronous invocation through an alias, naming no execution",
            durable: true,
            invocation_type: Some("RequestResponse"),
            query: "?Qualifier=live",
            requested_name: None,
            payload: b"[]",
            version: "1",
            status: 200,
            starts_execution: true,
            ran_in_the_call: true,
        },
        HandleCase {
            case: "an accepted asynchronous invocation, with a name at the published maximum",
            durable: true,
            invocation_type: Some("Event"),
            query: "",
            requested_name: Some("n".repeat(NAME_MAX)),
            payload: br#"{"queued":true}"#,
            version: VERSION_LATEST,
            status: 202,
            starts_execution: true,
            ran_in_the_call: false,
        },
        HandleCase {
            case: "a dry run, which validates the invocation and runs nothing",
            durable: true,
            invocation_type: Some("DryRun"),
            query: "",
            requested_name: Some("dry-run-execution".to_string()),
            payload: b"{}",
            version: VERSION_LATEST,
            status: 204,
            starts_execution: false,
            ran_in_the_call: false,
        },
        HandleCase {
            case: "a function with no DurableConfig, naming no execution",
            durable: false,
            invocation_type: Some("RequestResponse"),
            query: "",
            requested_name: None,
            payload: br#"{"marker":"plain"}"#,
            version: VERSION_LATEST,
            status: 200,
            starts_execution: false,
            ran_in_the_call: true,
        },
        HandleCase {
            case: "a function with no DurableConfig whose invocation names an execution",
            durable: false,
            invocation_type: Some("RequestResponse"),
            query: "",
            requested_name: Some("named-on-a-plain-function".to_string()),
            payload: br#"{"marker":"named"}"#,
            version: VERSION_LATEST,
            status: 200,
            starts_execution: true,
            ran_in_the_call: true,
        },
    ]
}

#[tokio::test]
async fn an_invocation_of_a_durable_function_answers_the_handle_of_the_execution_it_started() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let durable_function = "durable-handle";
    let plain_function = "plain-handle";
    create_invokable_function(
        &router,
        &engine,
        "the durable function",
        durable_function,
        Some(KMS_KEY),
    )
    .await;
    create_invokable_function(&router, &engine, "the plain function", plain_function, None).await;

    for row in handle_cases() {
        let function = if row.durable {
            durable_function
        } else {
            plain_function
        };
        let answered = invoke(
            &router,
            function,
            row.query,
            row.invocation_type,
            row.requested_name.as_deref(),
            row.payload,
        )
        .await;
        assert_eq!(
            answered.status,
            row.status,
            "case {:?}: expected {} from Invoke, got {} {}",
            row.case,
            row.status,
            answered.status,
            String::from_utf8_lossy(&answered.body)
        );
        assert_eq!(
            answered.header(FUNCTION_ERROR_HEADER),
            None,
            "case {:?}: expected no function error, got {:?}",
            row.case,
            answered.header(FUNCTION_ERROR_HEADER)
        );
        if !row.starts_execution {
            // An invocation that starts no durable execution answers no handle: there is
            // nothing for one to address.
            assert_eq!(
                answered.header(DURABLE_EXECUTION_ARN_HEADER),
                None,
                "case {:?}: an invocation that starts no execution must answer no ARN, got {:?}",
                row.case,
                answered.headers
            );
            continue;
        }
        let arn = answered
            .header(DURABLE_EXECUTION_ARN_HEADER)
            .unwrap_or_default();
        assert!(
            !arn.is_empty(),
            "case {:?}: the invocation must answer the started execution's ARN in \
             {DURABLE_EXECUTION_ARN_HEADER}, got {:?}",
            row.case,
            answered.headers
        );
        let (name, _id) =
            handle_parts(row.case, &arn, &handle_function_arn(function, row.version));
        if let Some(requested) = row.requested_name.as_deref() {
            assert_eq!(
                name, requested,
                "case {:?}: the handle must address the execution the request named, got {arn:?}",
                row.case
            );
        }

        // The handle RESOLVES: the read it is for reports the execution the invocation
        // started, by the same ARN, under the version the call resolved — on the
        // metadata-only read and at the operation's modelled default alike.
        for (read_case, query) in [
            ("the metadata-only read", METADATA_ONLY),
            ("the read at its modelled default", ""),
        ] {
            let read = get_execution(&router, &arn, query).await;
            assert_eq!(
                read.status,
                200,
                "case {:?}: expected 200 from {read_case} of {arn:?}, got {} {}",
                row.case,
                read.status,
                String::from_utf8_lossy(&read.body)
            );
            let body = read.json();
            for (member, expected) in [
                ("DurableExecutionArn", arn.clone()),
                ("DurableExecutionName", name.clone()),
                ("FunctionArn", versioned_arn(function, row.version)),
            ] {
                assert_eq!(
                    body.get(member).and_then(Value::as_str),
                    Some(expected.as_str()),
                    "case {:?}: expected {read_case} to report {member} as {expected:?}, got \
                     {body}",
                    row.case
                );
            }
            // `StartTimestamp` is a modelled timestamp, which restJson1 renders as epoch
            // seconds — a number, not the text of one.
            assert!(
                body.get("StartTimestamp").and_then(Value::as_f64).is_some(),
                "case {:?}: expected {read_case} to report a StartTimestamp in the modelled \
                 form, got {body}",
                row.case
            );
            let status = body.get("Status").and_then(Value::as_str).unwrap_or_default();
            if row.ran_in_the_call {
                // The run that decides the execution happened in the call that answered
                // this handle, so the handle resolves to an execution that has ENDED.
                assert_eq!(
                    status, STATUS_SUCCEEDED,
                    "case {:?}: expected {read_case} to report the execution the call ran as \
                     {STATUS_SUCCEEDED}, got {body}",
                    row.case
                );
            } else {
                // The queued event is delivered off the response path, so the execution is
                // running from the instant it was started until that delivery settles it.
                assert!(
                    status == STATUS_RUNNING || status == STATUS_SUCCEEDED,
                    "case {:?}: expected {read_case} to report the execution as running or \
                     settled, got {body}",
                    row.case
                );
            }
            let data_included = query != METADATA_ONLY;
            assert_eq!(
                body.get("ExecutionDataIncluded").and_then(Value::as_bool),
                Some(data_included),
                "case {:?}: expected {read_case} to report ExecutionDataIncluded as \
                 {data_included}, got {body}",
                row.case
            );
            let payload_text = std::str::from_utf8(row.payload).expect("the row's payload is text");
            let input = body.get("InputPayload").and_then(Value::as_str);
            if data_included {
                // The data read reports the payload the START was given, which is this
                // invocation's own.
                assert_eq!(
                    input,
                    Some(payload_text),
                    "case {:?}: expected {read_case} to report the payload the execution was \
                     started with, got {body}",
                    row.case
                );
                if row.ran_in_the_call {
                    // The handler echoes its event, so the result the read reports is the
                    // value the run really produced.
                    assert_eq!(
                        body.get("Result")
                            .and_then(Value::as_str)
                            .map(|result| serde_json::from_str::<Value>(result).unwrap_or_default()),
                        Some(
                            serde_json::from_str::<Value>(payload_text)
                                .expect("the row's payload is JSON")
                        ),
                        "case {:?}: expected {read_case} to report what the run returned, got \
                         {body}",
                        row.case
                    );
                }
            } else {
                // A read that asks for no data reports none of it.
                assert_eq!(
                    input, None,
                    "case {:?}: expected {read_case} to withhold the execution's data, got \
                     {body}",
                    row.case
                );
            }
        }
    }
}

/// One row of the repeat table: the payload the first invocation carries, whether the run
/// it drives FAILS, and the payload a repeat carries instead to be refused.
struct RepeatCase {
    case: &'static str,
    execution: &'static str,
    payload: &'static [u8],
    other_payload: &'static [u8],
    fails: bool,
}

/// The rows: a repeat of an execution whose run returned and one of an execution whose run
/// raised, so the outcome a repeat replays is asserted for both ends a run can come to.
fn repeat_cases() -> [RepeatCase; 2] {
    [
        RepeatCase {
            case: "a repeat of an execution whose run returned",
            execution: "repeat-returned",
            payload: br#"{"marker":"repeat"}"#,
            other_payload: br#"{"marker":"other"}"#,
            fails: false,
        },
        RepeatCase {
            case: "a repeat of an execution whose run raised",
            execution: "repeat-raised",
            payload: br#"{"fail":true}"#,
            other_payload: br#"{"fail":true,"again":true}"#,
            fails: true,
        },
    ]
}

/// A member of a function's error document, whichever of the two every runtime's document
/// carries is asked for.
fn error_member(body: &[u8], member: &str) -> String {
    serde_json::from_slice::<Value>(body)
        .unwrap_or_default()
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn a_synchronous_repeat_of_a_live_execution_name_runs_nothing_again() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let function = "durable-repeat";
    create_invokable_function(
        &router,
        &engine,
        "the durable function",
        function,
        Some(KMS_KEY),
    )
    .await;

    for row in repeat_cases() {
        let first = invoke(
            &router,
            function,
            "",
            Some("RequestResponse"),
            Some(row.execution),
            row.payload,
        )
        .await;
        assert_eq!(
            first.status,
            200,
            "case {:?}: expected 200 from Invoke, got {} {}",
            row.case,
            first.status,
            String::from_utf8_lossy(&first.body)
        );
        let signal = first.header(FUNCTION_ERROR_HEADER);
        assert_eq!(
            signal.as_deref(),
            row.fails.then_some(FUNCTION_ERROR_UNHANDLED),
            "case {:?}: unexpected function-error signal, got {signal:?}",
            row.case
        );
        let arn = first
            .header(DURABLE_EXECUTION_ARN_HEADER)
            .unwrap_or_default();
        assert!(
            !arn.is_empty(),
            "case {:?}: the invocation must answer the started execution's ARN, got {:?}",
            row.case,
            first.headers
        );

        // The repeat carrying the SAME payload is the same invocation answered again: the
        // handler does not run a second time, so the answer is the one the execution it
        // names already came to, under the same handle.
        let repeat = invoke(
            &router,
            function,
            "",
            Some("RequestResponse"),
            Some(row.execution),
            row.payload,
        )
        .await;
        assert_eq!(
            repeat.status,
            200,
            "case {:?}: expected 200 from the repeat, got {} {}",
            row.case,
            repeat.status,
            String::from_utf8_lossy(&repeat.body)
        );
        assert_eq!(
            repeat.header(DURABLE_EXECUTION_ARN_HEADER).as_deref(),
            Some(arn.as_str()),
            "case {:?}: the repeat must answer the execution already started, got {:?}",
            row.case,
            repeat.headers
        );
        assert_eq!(
            repeat.header(FUNCTION_ERROR_HEADER),
            signal,
            "case {:?}: the repeat must carry the failure signal the run came to, got {:?}",
            row.case,
            repeat.header(FUNCTION_ERROR_HEADER)
        );
        if row.fails {
            // The two members every runtime's error document carries are the ones the
            // execution holds, so the repeat reports the error the run really failed with.
            for member in ["errorMessage", "errorType"] {
                assert_eq!(
                    error_member(&repeat.body, member),
                    error_member(&first.body, member),
                    "case {:?}: the repeat must report the run's own {member}, got {}",
                    row.case,
                    String::from_utf8_lossy(&repeat.body)
                );
            }
            assert_eq!(
                error_member(&repeat.body, "errorType"),
                RAISED_ERROR_TYPE,
                "case {:?}: expected the error type the handler raised, got {}",
                row.case,
                String::from_utf8_lossy(&repeat.body)
            );
        } else {
            assert_eq!(
                repeat.body, first.body,
                "case {:?}: the repeat must replay the result the execution returned, got {}",
                row.case,
                String::from_utf8_lossy(&repeat.body)
            );
        }

        // Nothing started a second time: the history of the execution the handle addresses
        // holds exactly ONE start.
        let history = get_history(&router, &arn).await;
        assert_eq!(
            history.status,
            200,
            "case {:?}: expected 200 from GetDurableExecutionHistory, got {} {}",
            row.case,
            history.status,
            String::from_utf8_lossy(&history.body)
        );
        assert_eq!(
            starts(&history),
            1,
            "case {:?}: expected exactly one {EXECUTION_STARTED} event, got {}",
            row.case,
            String::from_utf8_lossy(&history.body)
        );
        // And the execution kept the status its own run settled it in.
        let read = get_execution(&router, &arn, METADATA_ONLY).await;
        let expected_status = if row.fails {
            STATUS_FAILED
        } else {
            STATUS_SUCCEEDED
        };
        assert_eq!(
            read.json().get("Status").and_then(Value::as_str),
            Some(expected_status),
            "case {:?}: expected the execution to report {expected_status}, got {}",
            row.case,
            String::from_utf8_lossy(&read.body)
        );

        // A repeat of the same name carrying a DIFFERENT payload is no repeat of that
        // execution at all: it is the modelled 409.
        let refused = invoke(
            &router,
            function,
            "",
            Some("RequestResponse"),
            Some(row.execution),
            row.other_payload,
        )
        .await;
        assert_eq!(
            refused.status,
            409,
            "case {:?}: expected 409 for a repeat whose payload differs, got {} {}",
            row.case,
            refused.status,
            String::from_utf8_lossy(&refused.body)
        );
        assert_eq!(
            refused.error_code(),
            "DurableExecutionAlreadyStartedException",
            "case {:?}: expected the modelled refusal, got {}",
            row.case,
            refused.error_code()
        );
        let message = refused
            .string("Message")
            .or_else(|| refused.string("message"))
            .unwrap_or_default();
        assert!(
            message.contains(row.execution),
            "case {:?}: expected the refusal to name the execution {:?}, got {message:?}",
            row.case,
            row.execution
        );
        // The refusal started nothing either.
        let history = get_history(&router, &arn).await;
        assert_eq!(
            starts(&history),
            1,
            "case {:?}: a refused repeat must start nothing, got {}",
            row.case,
            String::from_utf8_lossy(&history.body)
        );
    }
}

/// One row of the key table: the invocation type the call names and the status it answers.
struct KeyCase {
    case: &'static str,
    invocation_type: &'static str,
    execution: &'static str,
    status: u16,
}

/// The rows: a synchronous invocation and an asynchronous one, so the handle the client
/// holds is read against a disabled key whichever invocation answered it.
fn key_cases() -> [KeyCase; 2] {
    [
        KeyCase {
            case: "a synchronous invocation's handle",
            invocation_type: "RequestResponse",
            execution: "sync-metadata",
            status: 200,
        },
        KeyCase {
            case: "an asynchronous invocation's handle",
            invocation_type: "Event",
            execution: "async-metadata",
            status: 202,
        },
    ]
}

#[tokio::test]
async fn a_handle_still_reports_its_execution_metadata_while_its_key_is_disabled() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_with_key_service(&engine);
    // A key this account really holds, and really disables.
    let key_arn = created_key(&router).await;
    let function = "durable-handle-kms";
    create_invokable_function(
        &router,
        &engine,
        "the durable function",
        function,
        Some(&key_arn),
    )
    .await;

    let mut handles = Vec::new();
    for row in key_cases() {
        let answered = invoke(
            &router,
            function,
            "",
            Some(row.invocation_type),
            Some(row.execution),
            br#"{"marker":"kms"}"#,
        )
        .await;
        assert_eq!(
            answered.status,
            row.status,
            "case {:?}: expected {} from Invoke, got {} {}",
            row.case,
            row.status,
            answered.status,
            String::from_utf8_lossy(&answered.body)
        );
        let arn = answered
            .header(DURABLE_EXECUTION_ARN_HEADER)
            .unwrap_or_default();
        assert!(
            !arn.is_empty(),
            "case {:?}: the invocation must answer the started execution's ARN in \
             {DURABLE_EXECUTION_ARN_HEADER}, got {:?}",
            row.case,
            answered.headers
        );
        handles.push((row, arn));
    }

    let disabled = kms_call(&router, "DisableKey", &json!({ "KeyId": key_arn })).await;
    assert_eq!(
        disabled.status,
        200,
        "setup \"DisableKey\": expected 200, got {} {}",
        disabled.status,
        String::from_utf8_lossy(&disabled.body)
    );

    for (row, arn) in handles {
        // Reporting the METADATA decrypts nothing, so it is served whatever state the key
        // the function's `DurableConfig` names is in.
        let read = get_execution(&router, &arn, METADATA_ONLY).await;
        assert_eq!(
            read.status,
            200,
            "case {:?}: expected the metadata read of {arn:?} to be served while the key is \
             disabled, got {} {}",
            row.case,
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        let body = read.json();
        assert_eq!(
            body.get("DurableExecutionName").and_then(Value::as_str),
            Some(row.execution),
            "case {:?}: expected the read to report the execution the handle addresses, got \
             {body}",
            row.case
        );
        assert_eq!(
            body.get("DurableExecutionArn").and_then(Value::as_str),
            Some(arn.as_str()),
            "case {:?}: expected the read to report the handle it was asked about, got {body}",
            row.case
        );
        let status = body.get("Status").and_then(Value::as_str).unwrap_or_default();
        assert!(
            status == STATUS_RUNNING || status == STATUS_SUCCEEDED,
            "case {:?}: expected the read to report the execution as running or settled, got \
             {body}",
            row.case
        );
    }
}
