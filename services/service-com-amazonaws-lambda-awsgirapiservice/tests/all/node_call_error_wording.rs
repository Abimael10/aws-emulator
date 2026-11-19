// SPDX-License-Identifier: CC0-1.0

//! The MESSAGE a `nodejs` runtime reports for a CALL of a value that is not callable — the
//! `TypeError` a handler raises when it calls a member no object carries, a name that holds
//! something else, or the answer of a call that produced no function.
//!
//! RULE: a managed `nodejs` runtime reports an error the way the JavaScript engine that
//! runtime embeds words it — a call of a value that is not callable is
//! `<the expression the call named> is not a function`, with the expression as the module's
//! own source writes it and any argument list inside it elided — and that one message is
//! what the invocation's error document carries (`errorMessage`), what opens the trace
//! (`<errorType>: <errorMessage>`), and what the function's log holds for the run. An error
//! of another class keeps its own class and its own message.
//!
//! That message is the text a client debugging a handler searches its logs for: a call that
//! failed has to be findable by the expression that was called, which is the only part of
//! the failure that says WHICH call it was.
//!
//! The rows differ along what the rule generalises over: the FORM the callee is written in
//! (a bare name, a member, a chain of members, a member through a computed key, a member of
//! a module the runtime supplies, and the answer of a call), and an error of another class,
//! which keeps the engine's own wording.
//!
//! Every row RUNS a handler, so each takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use serde_json::Value;

use crate::invocation_log_delivery::{create_function, emulator, invoke, Created, NODE_RUNTIME};

/// A well-formed execution role ARN. No IAM is mounted here, so nothing is known about the
/// role and no log write is withheld.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// One row: the handler's body, and what every reader of the run must report for it.
struct Row {
    case: &'static str,
    /// The function's name, which is also the default log group's last segment.
    name: &'static str,
    /// The handler's source: a module whose `handler` makes the failing call.
    source: &'static str,
    /// The class of the error raised.
    error_type: &'static str,
    /// The message the runtime reports for it.
    message: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a call of a member no object carries",
        name: "node-calls-absent-member",
        source: "const store = { put: 1 };\n\
                 exports.handler = async () => {\n\
                 \x20 return store.write('a');\n\
                 };\n",
        error_type: "TypeError",
        message: "store.write is not a function",
    },
    Row {
        case: "a call of a member reached through a chain of them",
        name: "node-calls-absent-chained",
        source: "const config = { limits: { burst: 10 } };\n\
                 exports.handler = async () => {\n\
                 \x20 return config.limits.of(1);\n\
                 };\n",
        error_type: "TypeError",
        message: "config.limits.of is not a function",
    },
    Row {
        case: "a call of a member reached through a computed key",
        name: "node-calls-absent-computed",
        source: "const store = { put: 1 };\n\
                 exports.handler = async () => {\n\
                 \x20 const called = 'write';\n\
                 \x20 return store[called]('a');\n\
                 };\n",
        error_type: "TypeError",
        message: "store[called] is not a function",
    },
    Row {
        case: "a call of a member a module the runtime supplies does not carry",
        name: "node-calls-absent-of-module",
        source: "const path = require('path');\n\
                 exports.handler = async () => {\n\
                 \x20 return path.globify('/var/task');\n\
                 };\n",
        error_type: "TypeError",
        message: "path.globify is not a function",
    },
    Row {
        case: "a call of a name that holds something else",
        name: "node-calls-a-value",
        source: "exports.handler = async () => {\n\
                 \x20 const limit = 5;\n\
                 \x20 return limit(1);\n\
                 };\n",
        error_type: "TypeError",
        message: "limit is not a function",
    },
    Row {
        case: "a call of what a call answered",
        name: "node-calls-an-answer",
        source: "exports.handler = async () => {\n\
                 \x20 const made = () => undefined;\n\
                 \x20 return made()('a');\n\
                 };\n",
        error_type: "TypeError",
        message: "made(...) is not a function",
    },
    Row {
        case: "a call of a name nothing bound, which is another class",
        name: "node-calls-an-unbound-name",
        source: "exports.handler = async () => {\n\
                 \x20 return absent('a');\n\
                 };\n",
        error_type: "ReferenceError",
        message: "absent is not defined",
    },
];

/// The error document an invocation whose handler called a value that is not callable
/// reports the runtime's own wording of that failure — in its `errorMessage`, in the class it
/// names, in the line its trace opens with and in the log the invocation itself reports.
#[tokio::test]
async fn a_call_of_what_is_not_a_function_names_the_expression_it_called() {
    for row in ROWS {
        let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
        let case = row.case;
        let router = emulator();
        create_function(
            &router,
            &Created {
                case,
                name: row.name,
                source: row.source,
                runtime: NODE_RUNTIME,
                role: ROLE,
                log_group: None,
            },
        )
        .await;
        let (reported, payload) = invoke(&router, case, row.name).await;

        assert_eq!(
            payload.get("errorType").and_then(Value::as_str),
            Some(row.error_type),
            "case {case:?}: expected the error document to name {expected:?}, got {payload}",
            expected = row.error_type
        );
        assert_eq!(
            payload.get("errorMessage").and_then(Value::as_str),
            Some(row.message),
            "case {case:?}: expected the error document to carry {expected:?}, got {payload}",
            expected = row.message
        );
        let head = format!("{}: {}", row.error_type, row.message);
        assert_eq!(
            payload
                .get("trace")
                .and_then(Value::as_array)
                .and_then(|trace| trace.first())
                .and_then(Value::as_str),
            Some(head.as_str()),
            "case {case:?}: expected the trace to open with {head:?}, got {payload}"
        );
        // The frame under that line is the handler's own, so the failure names the module the
        // call was written in rather than anything of the runtime.
        let frame = payload
            .get("trace")
            .and_then(Value::as_array)
            .and_then(|trace| trace.get(1))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            frame.contains("/var/task/index.js:"),
            "case {case:?}: expected the failing frame to name the handler's module, got \
             {payload}"
        );
        assert!(
            reported.contains(row.message),
            "case {case:?}: expected the invocation's own log to carry {expected:?}, got \
             {reported:?}",
            expected = row.message
        );
    }
}
