// SPDX-License-Identifier: CC0-1.0

//! WHAT one frame of a failed NODE run's trace says: the function it is in, and where in the
//! deployed file it is.
//!
//! Two rules, over the frames a client reads off a failed invocation's error document.
//!
//! A frame names the function it is in by the name that function carries, and a function
//! written without a name of its own carries the name the language INFERS for it — for a
//! function expression assigned to a property, the assignment path it was written at
//! (`exports.handler` for the handler a CommonJS module exports). The rows of
//! [`a_frame_names_the_function_by_the_name_inferred_where_it_was_written`] differ along what
//! that rule generalises over: the form the function was written in (arrow, function
//! expression, named function expression, declaration assigned by reference), the export it
//! was assigned to (a member of the exports object, the exports object itself, a member of an
//! object replacing it), the module system the module is in, and whether the frame is the
//! handler's own or one of a module under it.
//!
//! A frame of a PROPERTY ACCESS that failed names the position the property is written at —
//! which is what tells a client WHICH access on the line failed, rather than where the
//! expression it was read from begins. The rows of
//! [`a_property_access_failure_names_the_position_the_property_is_written_at`] differ along
//! the value accessed (`undefined`, `null`), the direction of the access (read, write), how
//! deep the chain is, whether the line carries a longer property name beginning with the same
//! text, and which module of the package the access is written in.
//!
//! The third test is the shape of the stack under those frames: the runtime invokes the
//! handler from a job of its OWN, so the only frame of the runtime beneath the deployment
//! package's frames is the one that calls the handler — a client reading a trace is not shown
//! the chain that scheduled the call.
//!
//! Every row drives real HTTP requests through the router. The package, function and
//! invocation helpers are [`crate::node_loader_errors`]'s, reused rather than copied; WHICH
//! source a frame may name at all is [`crate::runtime_frame_naming`]'s rule.

use std::sync::Arc;

use serde_json::Value;

use crate::function_lifecycle::router_on;
use crate::node_loader_errors::{create_node_function, invoke, RUNTIME_ENTRY, TASK_ROOT};

/// How a stack frame line opens.
const FRAME: &str = "    at ";

/// The lines of the trace `document` carries.
fn trace(document: &Value) -> Vec<String> {
    document
        .get("trace")
        .and_then(Value::as_array)
        .map(|lines| {
            lines
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The frame lines of that trace: every line from the first frame on.
fn frames(document: &Value) -> Vec<String> {
    let lines = trace(document);
    let first = lines
        .iter()
        .position(|line| line.starts_with(FRAME))
        .unwrap_or(lines.len());
    lines[first..].to_vec()
}

/// The function `frame` names, as the frame states it.
fn function_of(frame: &str) -> String {
    let rest = frame.strip_prefix(FRAME).unwrap_or(frame);
    match rest.find(" (") {
        Some(cut) => rest[..cut].to_string(),
        None => String::new(),
    }
}

/// Create the function `entries` holds, invoke it, and answer the error document of the run
/// that failed.
async fn failed_run(
    name: &str,
    entries: &[(&'static str, &'static str)],
    handler: &'static str,
) -> Value {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    create_node_function(&router, &engine, name, entries, handler).await;
    let answered = invoke(&router, name).await;
    assert_eq!(
        answered.status,
        200,
        "{name}: a run that failed is still an HTTP 200, got {} {}",
        answered.status,
        String::from_utf8_lossy(&answered.body)
    );
    assert_eq!(
        answered.header("x-amz-function-error").as_deref(),
        Some("Unhandled"),
        "{name}: expected the Unhandled signal, got {:?}",
        answered.header("x-amz-function-error")
    );
    answered.json()
}

/// One row of the name table: the package, the handler string, and the function each of the
/// package's own frames must name, innermost first.
struct NameCase {
    case: &'static str,
    entries: Vec<(&'static str, &'static str)>,
    handler: &'static str,
    functions: Vec<&'static str>,
}

/// The rows.
fn name_cases() -> Vec<NameCase> {
    vec![
        NameCase {
            case: "an arrow assigned to a member of the exports object",
            entries: vec![(
                "index.js",
                "exports.handler = async () => {\n    throw new Error('boom');\n};\n",
            )],
            handler: "index.handler",
            functions: vec!["exports.handler"],
        },
        NameCase {
            case: "a function expression assigned to a member of the exports object",
            entries: vec![(
                "index.js",
                "exports.handler = async function () {\n    throw new Error('boom');\n};\n",
            )],
            handler: "index.handler",
            functions: vec!["exports.handler"],
        },
        NameCase {
            // A name the language STATES is the name the function carries, so nothing is
            // inferred for it.
            case: "a named function expression, which carries the name it was written with",
            entries: vec![(
                "index.js",
                "exports.handler = async function refuse() {\n    \
                 throw new Error('boom');\n};\n",
            )],
            handler: "index.handler",
            functions: vec!["refuse"],
        },
        NameCase {
            case: "a declaration exported by reference, which carries its declared name",
            entries: vec![(
                "index.js",
                "async function refuse() {\n    throw new Error('boom');\n}\n\
                 exports.handler = refuse;\n",
            )],
            handler: "index.handler",
            functions: vec!["refuse"],
        },
        NameCase {
            case: "a module whose exports ARE an anonymous function, called by the handler",
            entries: vec![
                (
                    "index.js",
                    "const refuse = require('./lib/refuse');\n\
                     exports.handler = async () => refuse();\n",
                ),
                (
                    "lib/refuse.js",
                    "module.exports = function () {\n    throw new Error('boom');\n};\n",
                ),
            ],
            handler: "index.handler",
            functions: vec!["module.exports", "exports.handler"],
        },
        NameCase {
            case: "a member of an object the module replaced its exports with",
            entries: vec![(
                "index.js",
                "module.exports = {};\n\
                 module.exports.handler = async () => {\n    throw new Error('boom');\n};\n",
            )],
            handler: "index.handler",
            functions: vec!["module.exports.handler"],
        },
        NameCase {
            // The language states the name here — a variable initialiser — so the frame
            // carries it with nothing inferred.
            case: "an ES module's exported binding, named by its own declaration",
            entries: vec![(
                "index.mjs",
                "export const handler = async () => {\n    throw new Error('boom');\n};\n",
            )],
            handler: "index.handler",
            functions: vec!["handler"],
        },
    ]
}

#[tokio::test]
async fn a_frame_names_the_function_by_the_name_inferred_where_it_was_written() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for (index, case) in name_cases().into_iter().enumerate() {
        let document = failed_run(
            &format!("node-frame-names-{index}"),
            &case.entries,
            case.handler,
        )
        .await;
        let framed = frames(&document);
        for (position, expected) in case.functions.iter().enumerate() {
            let held = framed.get(position).cloned().unwrap_or_default();
            assert_eq!(
                function_of(&held),
                *expected,
                "case {:?}: frame {position} must name the function {expected:?}, got {:?}",
                case.case,
                framed
            );
        }
    }
}

/// One row of the position table: the package, and the frame the access that failed must be
/// reported at, innermost first.
struct PositionCase {
    case: &'static str,
    entries: Vec<(&'static str, &'static str)>,
    /// The frame the access was made in: the function, the unit, and the position the
    /// property is written at.
    innermost: String,
    /// What the frame UNDER it must open with, where the access is written in a function the
    /// handler called: the position a frame names a CALL by is the engine's own, so only the
    /// function and the unit are stated.
    caller: Option<String>,
    /// What the failure reports as its message, which names the property the access named.
    message: &'static str,
}

/// The rows. Every position is counted off the source the row itself holds.
fn position_cases() -> Vec<PositionCase> {
    vec![
        PositionCase {
            // `    return event.requestContext.connectionId;`: `connectionId` is written at
            // column 33, where `event` begins at column 12.
            case: "a read on undefined, two properties into the chain",
            entries: vec![(
                "index.js",
                "'use strict';\n\
                 exports.handler = async (event) => {\n\
                 \x20   return event.requestContext.connectionId;\n\
                 };\n",
            )],
            innermost: format!("    at exports.handler ({TASK_ROOT}/index.js:3:33)"),
            caller: None,
            message: "Cannot read properties of undefined (reading 'connectionId')",
        },
        PositionCase {
            // `    return held.payload.length;`: `length` is written at column 25.
            case: "a read on null",
            entries: vec![(
                "index.js",
                "'use strict';\n\
                 exports.handler = async () => {\n\
                 \x20   const held = { payload: null };\n\
                 \x20   return held.payload.length;\n\
                 };\n",
            )],
            innermost: format!("    at exports.handler ({TASK_ROOT}/index.js:4:25)"),
            caller: None,
            message: "Cannot read properties of null (reading 'length')",
        },
        PositionCase {
            // `    return event.identity.user.id;`: the line carries `.identity` before the
            // access that failed, and `id` is written at column 32 — the longer name
            // beginning with the same text is another property, not this access.
            case: "a read whose line carries a longer property name beginning with it",
            entries: vec![(
                "index.js",
                "'use strict';\n\
                 exports.handler = async (event) => {\n\
                 \x20   event.identity = {};\n\
                 \x20   return event.identity.user.id;\n\
                 };\n",
            )],
            innermost: format!("    at exports.handler ({TASK_ROOT}/index.js:4:32)"),
            caller: None,
            message: "Cannot read properties of undefined (reading 'id')",
        },
        PositionCase {
            // `    held.target.name = 'written';`: `name` is written at column 17.
            case: "a write on undefined",
            entries: vec![(
                "index.js",
                "'use strict';\n\
                 exports.handler = async () => {\n\
                 \x20   const held = {};\n\
                 \x20   held.target.name = 'written';\n\
                 \x20   return held;\n\
                 };\n",
            )],
            innermost: format!("    at exports.handler ({TASK_ROOT}/index.js:4:17)"),
            caller: None,
            message: "Cannot set properties of undefined (setting 'name')",
        },
        PositionCase {
            // The access is written in another module of the package, whose own file and
            // position the innermost frame names: `connectionId` at column 26 of line 2.
            case: "a read written in another module of the package",
            entries: vec![
                (
                    "index.js",
                    "const identify = require('./lib/identify');\n\
                     exports.handler = async (event) => identify(event);\n",
                ),
                (
                    "lib/identify.js",
                    "module.exports = function (event) {\n\
                     \x20   return event.context.connectionId;\n\
                     };\n",
                ),
            ],
            innermost: format!("    at module.exports ({TASK_ROOT}/lib/identify.js:2:26)"),
            caller: Some(format!("    at exports.handler ({TASK_ROOT}/index.js:2:")),
            message: "Cannot read properties of undefined (reading 'connectionId')",
        },
        PositionCase {
            // The same rule in a module the package holds as an ES module, whose unit a frame
            // names by its url: `connectionId` at column 33 of line 2.
            case: "a read written in an ES module of the package",
            entries: vec![(
                "index.mjs",
                "export const handler = async (event) => {\n\
                 \x20   return event.requestContext.connectionId;\n\
                 };\n",
            )],
            innermost: format!("    at handler (file://{TASK_ROOT}/index.mjs:2:33)"),
            caller: None,
            message: "Cannot read properties of undefined (reading 'connectionId')",
        },
    ]
}

#[tokio::test]
async fn a_property_access_failure_names_the_position_the_property_is_written_at() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for (index, case) in position_cases().into_iter().enumerate() {
        let document = failed_run(
            &format!("node-frame-positions-{index}"),
            &case.entries,
            "index.handler",
        )
        .await;
        assert_eq!(
            document.get("errorType").and_then(Value::as_str),
            Some("TypeError"),
            "case {:?}: expected the class of a failed property access, got {document}",
            case.case
        );
        assert_eq!(
            document.get("errorMessage").and_then(Value::as_str),
            Some(case.message),
            "case {:?}: expected the message {:?}, got {document}",
            case.case,
            case.message
        );
        let framed = frames(&document);
        assert_eq!(
            framed.first().map(String::as_str),
            Some(case.innermost.as_str()),
            "case {:?}: expected the access to be reported at {:?}, got {framed:?}",
            case.case,
            case.innermost
        );
        if let Some(caller) = &case.caller {
            let held = framed.get(1).cloned().unwrap_or_default();
            assert!(
                held.starts_with(caller.as_str()),
                "case {:?}: expected the frame under it to open with {caller:?}, got \
                 {framed:?}",
                case.case
            );
        }
    }
}

/// One row of the stack-shape table: the package, the handler string, and how many frames of
/// the package's own code the run takes before the runtime's one.
struct ShapeCase {
    case: &'static str,
    entries: Vec<(&'static str, &'static str)>,
    handler: &'static str,
    package_frames: usize,
}

/// The rows: the ways a run reaches its failure, each ending in the one frame the runtime
/// contributes.
fn shape_cases() -> Vec<ShapeCase> {
    vec![
        ShapeCase {
            case: "an asynchronous handler that threw",
            entries: vec![(
                "index.js",
                "exports.handler = async () => {\n    throw new Error('boom');\n};\n",
            )],
            handler: "index.handler",
            package_frames: 1,
        },
        ShapeCase {
            case: "a handler that threw before returning at all",
            entries: vec![(
                "index.js",
                "exports.handler = function () {\n    throw new Error('boom');\n};\n",
            )],
            handler: "index.handler",
            package_frames: 1,
        },
        ShapeCase {
            case: "a failure two calls deep in the package",
            entries: vec![
                (
                    "index.js",
                    "const refuse = require('./lib/refuse');\n\
                     exports.handler = async () => refuse();\n",
                ),
                (
                    "lib/refuse.js",
                    "module.exports = function () {\n    throw new RangeError('boom');\n};\n",
                ),
            ],
            handler: "index.handler",
            package_frames: 2,
        },
        ShapeCase {
            case: "a property access the handler made on undefined",
            entries: vec![(
                "index.js",
                "exports.handler = async (event) => event.requestContext.connectionId;\n",
            )],
            handler: "index.handler",
            package_frames: 1,
        },
    ]
}

#[tokio::test]
async fn the_runtime_contributes_one_frame_under_the_packages_own_frames() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for (index, case) in shape_cases().into_iter().enumerate() {
        let document = failed_run(
            &format!("node-frame-shape-{index}"),
            &case.entries,
            case.handler,
        )
        .await;
        let framed = frames(&document);
        let runtime: Vec<&String> = framed
            .iter()
            .filter(|frame| frame.contains(RUNTIME_ENTRY))
            .collect();
        assert_eq!(
            runtime.len(),
            1,
            "case {:?}: the runtime contributes exactly one frame, got {framed:?}",
            case.case
        );
        assert_eq!(
            framed.len(),
            case.package_frames + 1,
            "case {:?}: expected {} frame(s) of the package and the runtime's one, got \
             {framed:?}",
            case.case,
            case.package_frames
        );
        assert_eq!(
            framed.last().map(String::as_str),
            runtime.first().map(|frame| frame.as_str()),
            "case {:?}: the runtime's frame is the last one, got {framed:?}",
            case.case
        );
    }
}
