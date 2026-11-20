// SPDX-License-Identifier: CC0-1.0

//! WHICH source every frame of a failed invocation's trace names.
//!
//! The rule under test: a `RequestResponse` invocation runs the deployment package, and the
//! document a run that RAISED is answered with carries the stack of that run — so every
//! frame of it names a source of the EXECUTION ENVIRONMENT: a file of the deployment
//! package under the task root, the runtime's own entry module, or an internal module of
//! the runtime. Nothing of the HOST that embeds the engine is a source a frame may name: a
//! unit of this emulator's own evaluator would read to a client as if the runtime had run
//! code the deployment package never held.
//!
//! Every row drives real HTTP requests through the router, and the rows differ along what
//! the rule generalises over: where the failure was raised (the handler's own module, a
//! function nested in it, another module of the package, a module the runtime supplies, the
//! runtime's own loader), the error class it is reported as, and how deep the stack is when
//! it is taken.
//!
//! The same rule over a PYTHON run's `stackTrace` frames — each naming a source file by
//! path, under the task root or the runtime's own library directory — is pinned by
//! [`crate::invoke_function_error_document`]. The package, function and invocation helpers
//! are [`crate::node_loader_errors`]'s, reused rather than copied.
//!
//! A frame names the unit its code was COMPILED as, so naming the runtime's own units makes
//! every module of the runtime a compilation unit of its own. The second test here is that
//! rule's other half: every core module the runtime supplies still loads when a handler asks
//! for it, so none of them depends on being compiled as anything else.

use std::sync::Arc;

use serde_json::Value;

use crate::function_lifecycle::router_on;
use crate::node_loader_errors::{create_node_function, invoke, RUNTIME_ENTRY, TASK_ROOT};

/// The URL form a frame of an ES module names its file by, which is what the runtime's own
/// entry module is.
const RUNTIME_ENTRY_UNIT: &str = "file:///var/runtime/index.mjs";

/// The namespace node names its own runtime's modules under: a public core module a handler
/// imports (`node:<module>`) and the internal module holding its implementation
/// (`node:internal/<module>`) are both named under it, and a frame inside either carries that
/// name in place of a file path.
const NODE_PREFIX: &str = "node:";

/// The names the engine embedded in this process gives a compilation it was handed no name
/// for — the host's own evaluation units. Neither is a source of the execution environment,
/// so neither may be named by a frame a client reads.
const HOST_UNITS: &[&str] = &["eval_script", "<input>"];

/// One frame of a reported stack.
#[derive(Debug)]
struct Frame {
    /// The function the frame is in, as the stack names it.
    name: String,
    /// The unit the frame's position is in: a file path, a file URL, or an internal module.
    unit: String,
}

/// The frame `line` states, or `None` when the line is not a frame at all (the error line a
/// trace opens with).
fn framed(line: &str) -> Option<Frame> {
    let rest = line.strip_prefix("    at ")?;
    // `at <name> (<unit>:<line>:<column>)`, and `at <unit>:<line>:<column>` for a frame
    // with no function to name.
    let (name, location) = match rest.strip_suffix(')') {
        Some(head) => match head.find(" (") {
            Some(cut) => (head[..cut].to_string(), head[cut + 2..].to_string()),
            None => (String::new(), head.to_string()),
        },
        None => (String::new(), rest.to_string()),
    };
    // The position is taken off the END, because a unit named by a `file://` URL carries a
    // colon of its own.
    let (head, _column) = location.rsplit_once(':')?;
    let (unit, _line) = head.rsplit_once(':')?;
    Some(Frame {
        name,
        unit: unit.to_string(),
    })
}

/// Whether `unit` is a source of the execution environment: a file of the deployment
/// package at the task root, the runtime's own entry module, or an internal module of the
/// runtime.
fn is_environment_source(unit: &str) -> bool {
    unit.starts_with(&format!("{TASK_ROOT}/"))
        || unit.starts_with(&format!("file://{TASK_ROOT}/"))
        || unit == RUNTIME_ENTRY_UNIT
        || unit == RUNTIME_ENTRY
        || unit.starts_with(NODE_PREFIX)
}

/// One row: the package, the handler string, the class the failure is reported as, and the
/// source the innermost frame of the run must name.
struct FrameCase {
    case: &'static str,
    entries: Vec<(&'static str, &'static str)>,
    handler: &'static str,
    error_type: &'static str,
    /// The unit the stack's FIRST frame names — where the failure was raised.
    innermost: String,
}

/// The rows.
fn frame_cases() -> Vec<FrameCase> {
    vec![
        FrameCase {
            case: "an error the handler itself threw",
            entries: vec![(
                "index.js",
                "exports.handler = async () => {\n    throw new Error('boom');\n};\n",
            )],
            handler: "index.handler",
            error_type: "Error",
            innermost: format!("{TASK_ROOT}/index.js"),
        },
        FrameCase {
            case: "a property read on an undefined value, two calls into the handler's module",
            entries: vec![(
                "index.js",
                "function identify(event) {\n    \
                 return event.requestContext.connectionId;\n}\n\
                 exports.handler = async (event) => {\n    return identify(event);\n};\n",
            )],
            handler: "index.handler",
            error_type: "TypeError",
            innermost: format!("{TASK_ROOT}/index.js"),
        },
        FrameCase {
            case: "a throw in another module of the package",
            entries: vec![
                (
                    "index.js",
                    "const helper = require('./lib/helper');\n\
                     exports.handler = async () => helper();\n",
                ),
                (
                    "lib/helper.js",
                    "module.exports = function () {\n    \
                     throw new RangeError('out of range');\n};\n",
                ),
            ],
            handler: "index.handler",
            error_type: "RangeError",
            innermost: format!("{TASK_ROOT}/lib/helper.js"),
        },
        FrameCase {
            case: "a refusal raised inside a module the runtime supplies",
            entries: vec![(
                "index.js",
                "const crypto = require('node:crypto');\n\
                 exports.handler = async () => crypto.createHash('no-such-digest');\n",
            )],
            handler: "index.handler",
            error_type: "Error",
            innermost: format!("{NODE_PREFIX}internal/crypto"),
        },
        FrameCase {
            case: "a specifier the runtime's loader cannot resolve while the handler runs",
            entries: vec![(
                "index.js",
                "exports.handler = async () => require('absent-at-call-time');\n",
            )],
            handler: "index.handler",
            error_type: "Error",
            innermost: RUNTIME_ENTRY_UNIT.to_string(),
        },
        FrameCase {
            case: "an error thrown by a handler the package holds as an ES module",
            entries: vec![(
                "index.mjs",
                "export const handler = async () => {\n    \
                 throw new TypeError('not callable');\n};\n",
            )],
            handler: "index.handler",
            error_type: "TypeError",
            innermost: format!("file://{TASK_ROOT}/index.mjs"),
        },
    ]
}

/// Every frame of a failed run's trace names a source of the execution environment, and the
/// innermost one names where the failure was raised.
#[tokio::test]
async fn every_reported_frame_names_a_source_of_the_execution_environment() {
    for (index, case) in frame_cases().into_iter().enumerate() {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = router_on(&engine);
        let function = format!("node-frames-{index}");
        create_node_function(&router, &engine, &function, &case.entries, case.handler).await;

        let answer = invoke(&router, &function).await;
        assert_eq!(
            answer.status,
            200,
            "case {:?}: a run that raised is still an HTTP 200, got {} {}",
            case.case,
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        assert_eq!(
            answer
                .headers
                .get("x-amz-function-error")
                .and_then(|value| value.to_str().ok()),
            Some("Unhandled"),
            "case {:?}: expected the Unhandled signal, got {:?}",
            case.case,
            answer.headers.get("x-amz-function-error")
        );
        let document = answer.json();
        assert_eq!(
            document.get("errorType").and_then(Value::as_str),
            Some(case.error_type),
            "case {:?}: expected the error class {}, got {document}",
            case.case,
            case.error_type
        );
        let trace: Vec<String> = document
            .get("trace")
            .and_then(Value::as_array)
            .map(|lines| {
                lines
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let frames: Vec<Frame> = trace.iter().filter_map(|line| framed(line)).collect();
        assert!(
            !frames.is_empty(),
            "case {:?}: expected the raised error's own frames, got {trace:?}",
            case.case
        );
        // Every line under the error line the trace opens with is a frame this reader
        // understands: the error line itself spans one entry per line of the message (a
        // `Cannot find module` message names its require stack), and from the first frame
        // on there is nothing but frames.
        let first = trace
            .iter()
            .position(|line| line.starts_with("    at "))
            .unwrap_or(trace.len());
        let unread = trace
            .iter()
            .skip(first)
            .filter(|line| framed(line).is_none())
            .count();
        assert_eq!(
            unread, 0,
            "case {:?}: every line from the first frame on is a frame, got {trace:?}",
            case.case
        );
        // The rule itself.
        for (position, frame) in frames.iter().enumerate() {
            assert!(
                is_environment_source(&frame.unit),
                "case {:?}: frame {position} must name a source of the execution \
                 environment, got {:?} in {trace:?}",
                case.case,
                frame.unit
            );
            assert!(
                !frame.name.is_empty(),
                "case {:?}: frame {position} must name the function it is in, got \
                 {trace:?}",
                case.case
            );
        }
        assert_eq!(
            frames[0].unit, case.innermost,
            "case {:?}: the innermost frame must name where the failure was raised, got \
             {trace:?}",
            case.case
        );
        // Said the other way round, over the units a client must never be shown: the frames
        // the RUNTIME itself contributes are its own module's, so what a trace shows is the
        // runtime that ran the handler and nothing of the host that embeds the engine.
        for (position, frame) in frames.iter().enumerate() {
            assert!(
                !HOST_UNITS.contains(&frame.unit.as_str()),
                "case {:?}: frame {position} names one of the host's own evaluation units \
                 {HOST_UNITS:?}, got {:?} in {trace:?}",
                case.case,
                frame.unit
            );
        }
        assert!(
            frames.iter().any(|frame| frame.unit == RUNTIME_ENTRY_UNIT),
            "case {:?}: the runtime's own entry module is on the stack that called the \
             handler, got {trace:?}",
            case.case
        );
    }
}

/// Every core module this runtime supplies, by the name a handler imports it under.
///
/// The catalog is `CORE_MODULES` in
/// `src/state/execution/node/core_modules.rs`; every name it holds is listed here, because
/// each is compiled as its own unit and a module that needed some other kind of compilation
/// would fail to load rather than merely be named differently in a frame.
const CORE_MODULE_NAMES: &[&str] = &[
    "path",
    "util",
    "events",
    "buffer",
    "child_process",
    "crypto",
    "fs",
    "fs/promises",
    "timers",
    "timers/promises",
    "stream",
    "stream/promises",
    "string_decoder",
    "os",
    "http",
    "https",
    "assert",
    "assert/strict",
    "querystring",
    "url",
];

/// Every core module the runtime supplies loads when the handler asks for it, under the bare
/// name and under the `node:` name — so each of them is a compilation unit of its own and
/// none is left depending on how the runtime used to compile it.
#[tokio::test]
async fn every_core_module_loads_as_its_own_compilation_unit() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let names = serde_json::to_string(CORE_MODULE_NAMES).expect("the catalog serializes");
    let source = format!(
        "const names = {names};\n\
         exports.handler = async () => {{\n    \
         const loaded = {{}};\n    \
         for (const name of names) {{\n        \
         loaded[name] = [typeof require(name), typeof require('node:' + name)];\n    \
         }}\n    \
         return loaded;\n\
         }};\n"
    );
    create_node_function(
        &router,
        &engine,
        "node-core-modules",
        &[("index.js", source.as_str())],
        "index.handler",
    )
    .await;

    let answer = invoke(&router, "node-core-modules").await;
    assert_eq!(
        answer.status,
        200,
        "requiring every core module must answer the handler's own value, got {} {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    assert_eq!(
        answer.headers.get("x-amz-function-error"),
        None,
        "requiring every core module must not fail the run, got {}",
        String::from_utf8_lossy(&answer.body)
    );
    let loaded = answer.json();
    for name in CORE_MODULE_NAMES {
        let reported = loaded.get(*name).and_then(Value::as_array).cloned();
        let kinds: Vec<String> = reported
            .unwrap_or_default()
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        assert_eq!(
            kinds.len(),
            2,
            "case {name:?}: expected the module under both of its names, got {loaded}"
        );
        for kind in &kinds {
            // A module exports an object or, like `assert`, a callable.
            assert!(
                kind == "object" || kind == "function",
                "case {name:?}: expected the module's own exports, got {kind:?} in {loaded}"
            );
        }
    }
}
