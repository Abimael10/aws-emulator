// SPDX-License-Identifier: CC0-1.0

//! What a NODE handler reads off an error's own `stack`.
//!
//! RULE: a managed `nodejs*` runtime's `stack` is the error's own string representation —
//! `<name>: <message>`, the name alone for an error carrying no message — followed by one
//! `    at <function> (<unit>:<line>:<column>)` line per frame of the stack that was taken.
//! The head line is part of the stack whoever raised the error: the class and the message of
//! what failed are what code reporting a failure BY its stack passes on, and a handler that
//! hands that text to another service (a custom resource provider answers CloudFormation with
//! it) otherwise passes on frames with nothing in front of them saying what failed.
//!
//! The rows differ along what the rule generalises over: who raised the error (the handler's
//! own `throw`, a class it declared, the engine itself), whether the error carries a message
//! at all, whether that message spans several lines, and the class it is of. The row that
//! ASSIGNED its error a stack is the rule's other edge: an assigned stack is the whole text,
//! because what a handler put there is what reads back.
//!
//! The second test is the same rule where a CLIENT reads it: the error document a failed
//! invocation is answered with opens its trace with that one line and never repeats it.
//!
//! Every row drives real HTTP requests through the router. The package, function and
//! invocation helpers are [`crate::node_loader_errors`]'s, reused rather than copied.

use std::sync::Arc;

use serde_json::Value;

use crate::function_lifecycle::router_on;
use crate::node_loader_errors::{create_node_function_within, invoke};

/// How long a function this suite creates may RUN for, in seconds.
///
/// Generous on purpose: the timeout is a limit of the function and not one this suite asserts,
/// so a run slowed by a loaded host is answered by the handler rather than by it.
const RUN_TIMEOUT_SECONDS: i64 = 120;

/// How a stack's frame lines open, which is the form a node stack frame takes.
const FRAME: &str = "    at ";

/// The handler the first test runs: one entry per row, each raising its own error, and the
/// `name`, `message` and `stack` of what was caught reported under that row's key — so what
/// is asserted is what the runtime gave the guest.
const HANDLER_SOURCE: &str = r#"
'use strict';

const raisers = {
    'constructed': () => {
        throw new Error('the provider refused the request');
    },
    'no message': () => {
        throw new Error();
    },
    'named class': () => {
        class ProviderError extends Error {
            constructor(message) {
                super(message);
                this.name = 'ProviderError';
            }
        }
        throw new ProviderError('no such widget');
    },
    'engine type error': () => {
        const held = undefined;
        return held.member;
    },
    'engine syntax error': () => JSON.parse('{not json}'),
    'multiline message': () => {
        throw new Error('the handler failed\n\nLogs: /aws/lambda/provider\n\n');
    },
    'assigned stack': () => {
        const error = new Error('the provider refused the request');
        error.stack = 'a stack of the handler own, whole';
        throw error;
    }
};

exports.handler = async () => {
    const report = {};
    for (const key of Object.keys(raisers)) {
        try {
            raisers[key]();
            report[key] = null;
        } catch (error) {
            report[key] = {
                name: error.name,
                message: error.message,
                stack: error.stack
            };
        }
    }
    return report;
};
"#;

/// One row: which error the handler raised, and what its own code decides about it.
struct Row {
    case: &'static str,
    /// The key the handler reports this row under.
    key: &'static str,
    /// The `name` the error reports, which this row's own code decides.
    name: &'static str,
    /// The `message` it reports, where this row's own code decides it — `None` where the
    /// wording is the engine's, which this suite does not pin.
    message: Option<&'static str>,
    /// The whole stack, for the row that ASSIGNED one.
    assigned: Option<&'static str>,
    /// The lines the stack opens with, for a row worth naming them for; empty otherwise,
    /// where the head line the rule states is checked on its own.
    opens_with: &'static [&'static str],
}

/// The rows.
const ROWS: &[Row] = &[
    Row {
        case: "an error the handler itself threw",
        key: "constructed",
        name: "Error",
        message: Some("the provider refused the request"),
        assigned: None,
        opens_with: &["Error: the provider refused the request"],
    },
    Row {
        case: "an error carrying no message, whose head is the class alone",
        key: "no message",
        name: "Error",
        message: Some(""),
        assigned: None,
        opens_with: &["Error"],
    },
    Row {
        case: "an error of a class the handler declared, which names itself",
        key: "named class",
        name: "ProviderError",
        message: Some("no such widget"),
        assigned: None,
        opens_with: &["ProviderError: no such widget"],
    },
    Row {
        case: "a type error the engine itself raised",
        key: "engine type error",
        name: "TypeError",
        message: None,
        assigned: None,
        opens_with: &[],
    },
    Row {
        case: "a syntax error the engine itself raised",
        key: "engine syntax error",
        name: "SyntaxError",
        message: None,
        assigned: None,
        opens_with: &[],
    },
    Row {
        // The shape a report built from an error's stack takes: a message of its own, a blank
        // line, a line naming where to read the logs, a blank line, and the frames under it.
        case: "a message that spans lines, which the head line spans too",
        key: "multiline message",
        name: "Error",
        message: Some("the handler failed\n\nLogs: /aws/lambda/provider\n\n"),
        assigned: None,
        opens_with: &[
            "Error: the handler failed",
            "",
            "Logs: /aws/lambda/provider",
            "",
        ],
    },
    Row {
        case: "a stack the handler assigned, which is the whole text it put there",
        key: "assigned stack",
        name: "Error",
        message: Some("the provider refused the request"),
        assigned: Some("a stack of the handler own, whole"),
        opens_with: &[],
    },
];

/// The string representation of an error carrying `name` and `message`, which is the line a
/// stack opens with.
fn representation(name: &str, message: &str) -> String {
    if name.is_empty() {
        return message.to_string();
    }
    if message.is_empty() {
        return name.to_string();
    }
    format!("{name}: {message}")
}

#[tokio::test]
async fn an_errors_stack_opens_with_the_error_itself_and_then_its_frames() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let function = "node-error-stack";
    create_node_function_within(
        &router,
        &engine,
        function,
        &[("index.js", HANDLER_SOURCE)],
        "index.handler",
        RUN_TIMEOUT_SECONDS,
    )
    .await;

    let answered = invoke(&router, function).await;
    assert_eq!(
        answered.status,
        200,
        "expected HTTP 200 from Invoke, got {} {}",
        answered.status,
        String::from_utf8_lossy(&answered.body)
    );
    let report = answered.json();

    for row in ROWS {
        let held = report.get(row.key).cloned().unwrap_or(Value::Null);
        let text = |member: &str| {
            held.get(member)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let name = text("name");
        let message = text("message");
        let stack = text("stack");
        assert_eq!(
            name, row.name,
            "case {:?}: expected the error to report the class {:?}, got {held}",
            row.case, row.name
        );
        if let Some(expected) = row.message {
            assert_eq!(
                message, expected,
                "case {:?}: expected the message {expected:?}, got {held}",
                row.case
            );
        } else {
            assert!(
                !message.is_empty(),
                "case {:?}: expected the engine's own message, got {held}",
                row.case
            );
        }

        if let Some(expected) = row.assigned {
            assert_eq!(
                stack, expected,
                "case {:?}: expected the assigned stack {expected:?}, got {stack:?}",
                row.case
            );
            continue;
        }

        let head = representation(&name, &message);
        assert!(
            stack == head || stack.starts_with(&format!("{head}\n")),
            "case {:?}: expected the stack to open with the error itself ({head:?}), got \
             {stack:?}",
            row.case
        );
        let frames: Vec<&str> = stack[head.len()..]
            .lines()
            .filter(|line| !line.is_empty())
            .collect();
        assert!(
            !frames.is_empty(),
            "case {:?}: expected at least one frame under the head line, got {stack:?}",
            row.case
        );
        for frame in &frames {
            assert!(
                frame.starts_with(FRAME),
                "case {:?}: expected every line under the head line to be a frame, got \
                 {frame:?} in {stack:?}",
                row.case
            );
        }
        if !row.opens_with.is_empty() {
            let opened: Vec<&str> = stack.split('\n').take(row.opens_with.len()).collect();
            assert_eq!(
                opened, row.opens_with,
                "case {:?}: expected the stack to open with {:?}, got {stack:?}",
                row.case, row.opens_with
            );
        }
    }
}

/// One row of the document table: the handler, and what the failure it reports carries.
struct DocumentRow {
    case: &'static str,
    source: &'static str,
    error_type: &'static str,
    message: &'static str,
    /// The trace's only line under the head one, for a handler that ASSIGNED its stack;
    /// `None` where the frames the engine took are what follows it.
    tail: Option<&'static str>,
}

/// The rows: an error whose stack is the one the engine took, and one whose stack the handler
/// replaced.
const DOCUMENT_ROWS: &[DocumentRow] = &[
    DocumentRow {
        case: "an error the handler threw",
        source: "exports.handler = async () => {\n    throw new Error('boom');\n};\n",
        error_type: "Error",
        message: "boom",
        tail: None,
    },
    DocumentRow {
        case: "an error whose stack the handler assigned",
        source: "exports.handler = async () => {\n    const error = new Error('boom');\n    \
                 error.stack = 'only this';\n    throw error;\n};\n",
        error_type: "Error",
        message: "boom",
        tail: Some("only this"),
    },
];

#[tokio::test]
async fn a_failed_invocations_trace_opens_with_that_one_line_and_does_not_repeat_it() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for row in DOCUMENT_ROWS {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = router_on(&engine);
        let function = "node-error-document";
        create_node_function_within(
            &router,
            &engine,
            function,
            &[("index.js", row.source)],
            "index.handler",
            RUN_TIMEOUT_SECONDS,
        )
        .await;

        let answered = invoke(&router, function).await;
        assert_eq!(
            answered.status,
            200,
            "case {:?}: a function error is still an HTTP 200, got {} {}",
            row.case,
            answered.status,
            String::from_utf8_lossy(&answered.body)
        );
        let document = answered.json();
        assert_eq!(
            (
                document.get("errorType").and_then(Value::as_str),
                document.get("errorMessage").and_then(Value::as_str),
            ),
            (Some(row.error_type), Some(row.message)),
            "case {:?}: expected the failure to report {}: {}, got {document}",
            row.case,
            row.error_type,
            row.message
        );
        let trace: Vec<&str> = document
            .get("trace")
            .and_then(Value::as_array)
            .map(|lines| lines.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let head = representation(row.error_type, row.message);
        assert_eq!(
            trace.first().copied(),
            Some(head.as_str()),
            "case {:?}: expected the trace to open with {head:?}, got {document}",
            row.case
        );
        let tail = &trace[1.min(trace.len())..];
        match row.tail {
            Some(assigned) => assert_eq!(
                tail,
                [assigned],
                "case {:?}: expected the assigned stack under the head line, got {document}",
                row.case
            ),
            None => {
                assert!(
                    !tail.is_empty(),
                    "case {:?}: expected the frames under the head line, got {document}",
                    row.case
                );
                for line in tail {
                    assert!(
                        line.starts_with(FRAME),
                        "case {:?}: expected every line under the head one to be a frame, got \
                         {line:?} in {document}",
                        row.case
                    );
                }
            }
        }
    }
}
