// SPDX-License-Identifier: CC0-1.0

//! What `Invoke` reports for a handler that RETURNED a value, including one whose value is an
//! HTTP envelope naming an error status of its own.
//!
//! RULE: a synchronous `Invoke` whose handler RETURNS is a successful invocation — HTTP 200 on
//! `Invoke$StatusCode`, NO `X-Amz-Function-Error` header, and the value the handler returned,
//! serialized as its runtime serializes it, as the whole `Payload` — whatever that value is: an
//! envelope whose own `statusCode` member names a client or a server error is still a value the
//! handler chose, so it is the payload and nothing else. Only a run that ended in an error the
//! handler did NOT handle reports `FunctionError`, and then the payload is the runtime's own
//! error document rather than anything the handler returned. A handler that CATCHES what one of
//! its own frames raised has therefore returned: its invocation carries no `FunctionError`, and
//! the only record of what went wrong inside it is what it wrote to the console, delivered to
//! the log group the function DECLARES.
//!
//! Why it matters: `FunctionError` is "if present, indicates that an error occurred during
//! function execution" (`Invoke$FunctionError`), and a client routes on it — retries, alarms and
//! dead-letter routing all treat a function error as the opposite outcome from an answer. A
//! handler-chosen `500` reported as a function error, or a function error reported as an answer,
//! inverts every one of those decisions.
//!
//! The rows differ along what the rule generalises over: the RUNTIME that ran the handler (a
//! node one and a python one, whose serializers and error documents differ), the SHAPE of the
//! value returned (an HTTP envelope, a bare string, a list, and no value at all), the STATUS the
//! envelope names (a success, a client error and a server error), HOW that status was arrived at
//! (chosen outright and chosen after catching an error a frame of the handler's own raised), and
//! whether the run ENDED in an error nobody handled, which is the one outcome that reports a
//! function error.
//!
//! Every row drives real HTTP through the production `MultiServiceRouter` with Lambda,
//! CloudWatch Logs and IAM mounted on ONE engine, and the second test reads the delivery back
//! through CloudWatch Logs' own group-wide search (`FilterLogEvents`), so nothing that did not
//! reach that store can pass.
//!
//! Both tests RUN handlers, so each takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::invocation_log_delivery::{
    create_function, emulator, groups, invocation_request_carrying, searched, Created,
    FUNCTION_ERROR_HEADER, NODE_RUNTIME, PYTHON_RUNTIME,
};
use crate::node_handler_error_wording::ROLE;

/// The `FunctionError` value a run that ended in an error nobody handled reports.
const UNHANDLED: &str = "Unhandled";

/// The prefix of every DEFAULT function log group (`/aws/lambda/<function name>`), which a
/// function that declares a group of its own does not write into.
const DEFAULT_GROUP_PREFIX: &str = "/aws/lambda/";

/// The node handler every row below is answered by: it dispatches on the event's `action`,
/// answers an envelope of its own choosing for each, raises inside a frame of its own for the
/// one event that carries no detail to read — catching that and answering its own `500` — and
/// raises OUTSIDE its own `try` for the one event that asks it to.
const NODE_HANDLER: &str = concat!(
    "function handleHeartbeat(event) {\n",
    "  return { statusCode: 200, body: event.detail.id.replace('-', ':') };\n",
    "}\n",
    "exports.handler = async (event) => {\n",
    "  if (event.action === 'crash') {\n",
    "    throw new TypeError('the caller asked for a crash');\n",
    "  }\n",
    "  try {\n",
    "    if (event.action === 'heartbeat') { return handleHeartbeat(event); }\n",
    "    if (event.action === 'text') { return 'plain text answer'; }\n",
    "    if (event.action === 'list') { return ['one', 'two']; }\n",
    "    if (event.action === 'nothing') { return; }\n",
    "    return { statusCode: 400, body: 'Unknown action' };\n",
    "  } catch (error) {\n",
    "    console.error('the handler could not answer', error.stack);\n",
    "    return { statusCode: 500, body: 'Internal server error' };\n",
    "  }\n",
    "};\n",
);

/// The python handler answering the same rows, written the way that runtime's handlers are.
const PYTHON_HANDLER: &str = concat!(
    "def handle_heartbeat(event):\n",
    "    detail = event['detail']\n",
    "    return {'statusCode': 200, 'body': detail['id'].replace('-', ':')}\n",
    "\n",
    "\n",
    "def lambda_handler(event, context):\n",
    "    if event.get('action') == 'crash':\n",
    "        raise TypeError('the caller asked for a crash')\n",
    "    try:\n",
    "        action = event.get('action')\n",
    "        if action == 'heartbeat':\n",
    "            return handle_heartbeat(event)\n",
    "        if action == 'text':\n",
    "            return 'plain text answer'\n",
    "        if action == 'list':\n",
    "            return ['one', 'two']\n",
    "        if action == 'nothing':\n",
    "            return None\n",
    "        return {'statusCode': 400, 'body': 'Unknown action'}\n",
    "    except Exception as error:\n",
    "        print('the handler could not answer', error)\n",
    "        return {'statusCode': 500, 'body': 'Internal server error'}\n",
);

/// The error class and message the one run that nobody handles reports, which both runtimes
/// word the same way for an error the handler constructed itself.
const CRASH_TYPE: &str = "TypeError";
const CRASH_MESSAGE: &str = "the caller asked for a crash";

/// One row: the event the function is invoked with, and what the invocation then reports.
struct Row {
    case: &'static str,
    /// The event delivered as the invocation's payload.
    event: fn() -> Value,
    /// The payload the invocation must answer for a handler that returned, or `None` for the
    /// row whose run ended in an error nobody handled — whose payload is the runtime's own
    /// document and is asserted member by member instead.
    payload: Option<fn() -> Value>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an action the handler serves, answered with its own 200 envelope",
            event: || json!({ "action": "heartbeat", "detail": { "id": "abc-123" } }),
            payload: Some(|| json!({ "statusCode": 200, "body": "abc:123" })),
        },
        Row {
            case: "an action the handler does not know, answered with its own 400 envelope",
            event: || json!({ "action": "no-such-action" }),
            payload: Some(|| json!({ "statusCode": 400, "body": "Unknown action" })),
        },
        Row {
            case: "an event with nothing to read, answered with the 500 the handler chose \
                   after catching what its own frame raised",
            event: || json!({ "action": "heartbeat" }),
            payload: Some(|| json!({ "statusCode": 500, "body": "Internal server error" })),
        },
        Row {
            case: "an action answered with no envelope at all, just text",
            event: || json!({ "action": "text" }),
            payload: Some(|| json!("plain text answer")),
        },
        Row {
            case: "an action answered with a list",
            event: || json!({ "action": "list" }),
            payload: Some(|| json!(["one", "two"])),
        },
        Row {
            case: "an action the handler answers with no value",
            event: || json!({ "action": "nothing" }),
            payload: Some(|| Value::Null),
        },
        Row {
            case: "an event whose run ends in an error nobody handled",
            event: || json!({ "action": "crash" }),
            payload: None,
        },
    ]
}

/// What one `Invoke` answered: its HTTP status, the `FunctionError` it signalled, and the
/// payload it carried, as TEXT beside the value that text parses to.
///
/// The text is kept because an EMPTY payload and the payload `null` are two different answers:
/// a handler that returned no value answers the four bytes `null`, which its runtime's
/// serializer wrote for the value it returned, and an empty body is no document at all.
struct Answered {
    status: u16,
    function_error: Option<String>,
    text: String,
    payload: Value,
}

/// `Invoke` the function `name` synchronously with `event` as its payload.
async fn invoked(router: &MultiServiceRouter, name: &str, event: &Value) -> Answered {
    let request = invocation_request_carrying(name, &[], event.to_string().as_bytes());
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let text = String::from_utf8_lossy(&body).to_string();
    Answered {
        status: parts.status.as_u16(),
        function_error: parts
            .headers
            .get(FUNCTION_ERROR_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        payload: serde_json::from_str(&text).unwrap_or(Value::Null),
        text,
    }
}

/// Whatever a handler returns, the invocation that ran it is a success carrying that value —
/// and only a run that ended in an unhandled error reports a function error.
#[tokio::test]
async fn a_handler_that_returns_reports_its_own_value_and_no_function_error() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let router = emulator();
    let functions = [
        (NODE_RUNTIME, "invoke-reported-status-node", NODE_HANDLER),
        (
            PYTHON_RUNTIME,
            "invoke-reported-status-python",
            PYTHON_HANDLER,
        ),
    ];
    for (runtime, name, source) in functions {
        create_function(
            &router,
            &Created {
                case: runtime,
                name,
                source,
                runtime,
                role: ROLE,
                log_group: None,
            },
        )
        .await;

        for row in rows() {
            let case = format!("{runtime}: {}", row.case);
            let event = (row.event)();
            let answered = invoked(&router, name, &event).await;
            assert_eq!(
                answered.status, 200,
                "case {case:?}: expected HTTP 200 from Invoke, got {} with {}",
                answered.status, answered.payload
            );
            match row.payload {
                Some(expected) => {
                    assert_eq!(
                        answered.function_error, None,
                        "case {case:?}: expected a handler that returned to report no function \
                         error, got {:?} with {}",
                        answered.function_error, answered.payload
                    );
                    let expected = expected();
                    // The payload is read from the TEXT the runtime wrote, so a row expecting
                    // the document `null` is not satisfied by an answer carrying no document.
                    let parsed: Value = serde_json::from_str(&answered.text).unwrap_or_else(
                        |error| {
                            panic!(
                                "case {case:?}: expected the payload to be the document the \
                                 runtime wrote, got {:?}: {error}",
                                answered.text
                            )
                        },
                    );
                    assert_eq!(
                        parsed, expected,
                        "case {case:?}: expected the payload {expected}, got {:?}",
                        answered.text
                    );
                }
                None => {
                    assert_eq!(
                        answered.function_error.as_deref(),
                        Some(UNHANDLED),
                        "case {case:?}: expected the run that nobody handled to report \
                         {UNHANDLED:?}, got {:?} with {}",
                        answered.function_error,
                        answered.payload
                    );
                    assert_eq!(
                        answered.payload.get("errorType"),
                        Some(&json!(CRASH_TYPE)),
                        "case {case:?}: expected the runtime's document to name the class \
                         {CRASH_TYPE:?}, got {}",
                        answered.payload
                    );
                    assert_eq!(
                        answered.payload.get("errorMessage"),
                        Some(&json!(CRASH_MESSAGE)),
                        "case {case:?}: expected the runtime's document to carry the message \
                         {CRASH_MESSAGE:?}, got {}",
                        answered.payload
                    );
                    assert_eq!(
                        answered.payload.get("statusCode"),
                        None,
                        "case {case:?}: expected the runtime's own document rather than an \
                         envelope of the handler's, got {}",
                        answered.payload
                    );
                }
            }
        }
    }
}

/// The group the function below declares, which is NOT the default one its name would give it.
const DECLARED_GROUP: &str = "/declared/invoke-reported-status";

/// The function that declares it.
const DECLARING_FUNCTION: &str = "invoke-reported-status-declared-group";

/// What the error the handler catches is worded as, and the frame of the handler's own that
/// raised it: values of the handler's code and of the thrown error, which is what a client
/// searching the group for the run that caught one has to find. The wording is the whole
/// sentence the engine a managed node runtime embeds states for that class, as
/// [`crate::node_handler_error_wording`] owns it.
const CAUGHT_MESSAGE: &str = "Cannot read properties of undefined (reading 'id')";
const RAISING_FRAME: &str = "handleHeartbeat";

/// The invocation that caught its own error answers the handler's value with no function error,
/// and the trace it wrote reaches the group the function DECLARES — the only record of what went
/// wrong inside it.
#[tokio::test]
async fn a_caught_errors_trace_reaches_the_group_the_function_declares() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "a node handler that catches what its own frame raised";
    let router = emulator();
    create_function(
        &router,
        &Created {
            case,
            name: DECLARING_FUNCTION,
            source: NODE_HANDLER,
            runtime: NODE_RUNTIME,
            role: ROLE,
            log_group: Some(DECLARED_GROUP),
        },
    )
    .await;

    let answered = invoked(&router, DECLARING_FUNCTION, &json!({ "action": "heartbeat" })).await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} with {}",
        answered.status, answered.payload
    );
    assert_eq!(
        answered.function_error, None,
        "case {case:?}: expected no function error for a handler that caught its own, got {:?}",
        answered.function_error
    );
    assert_eq!(
        answered.payload,
        json!({ "statusCode": 500, "body": "Internal server error" }),
        "case {case:?}: expected the handler's own envelope as the payload, got {}",
        answered.payload
    );

    let held = searched(&router, case, DECLARED_GROUP).await;
    assert!(
        held.iter().any(|message| message.contains(CAUGHT_MESSAGE)
            && message.contains(RAISING_FRAME)),
        "case {case:?}: expected an event of {DECLARED_GROUP:?} carrying the wording \
         {CAUGHT_MESSAGE:?} and the frame {RAISING_FRAME:?} that raised, got {held:?}"
    );
    // The delivery went to the group the function DECLARES: the default group its name would
    // otherwise give it holds nothing, because none was created.
    let defaults = groups(&router, case, DEFAULT_GROUP_PREFIX).await;
    assert!(
        defaults.is_empty(),
        "case {case:?}: expected a function that declares {DECLARED_GROUP:?} to leave no \
         default group, got {defaults:?}"
    );
}
