// SPDX-License-Identifier: CC0-1.0

//! What a frame INSIDE the runtime's own standard library is rendered as, in the stack a
//! failed Python invocation answers with.
//!
//! The rule under test: every frame of a Python run's `stackTrace` that names a source file of
//! the execution environment is a `traceback`-formatted frame — its location line AND the
//! source line at that location — and a module of the standard library the runtime ships is
//! such a file, under the runtime's own library directory, exactly as a module of the
//! deployment package under the task root is ([`crate::python_frame_source_lines`], which pins
//! the same rule over the package's own modules). A frame of a library module reduced to its
//! location alone drops the statement that raised, so a client parsing the stack cannot say
//! what the error was taken on.
//!
//! The rows differ along what the rule generalises over: WHICH module of the library the
//! failure is taken in (a module at the top of the library, a module inside a package, a
//! package's own module, and a stack passing through three modules of one package), the class
//! the failure is reported as, and HOW DEEP the stack is when it is taken.
//!
//! Every row drives real HTTP requests through the router, and nothing about a frame is
//! written out here: the frames the error document carries are compared against the ones the
//! STANDARD LIBRARY's own formatter renders for the very same failure, which the function
//! itself is asked for on a second invocation. So the assertion is the rule — a frame is what
//! `traceback` makes of it — rather than one recorded traceback.

use std::sync::Arc;

use serde_json::Value;

use crate::invoke_request_id::one_invocation_stream_at_a_time;
use crate::python_frame_source_lines::{
    create_function, frame_lines, invoke, located, poll_until_active, router_on, PYTHON,
};

/// The directory the runtime's own standard library is mounted at, which is the prefix of the
/// file a frame inside one of its modules names: `/var/lang/lib/<runtime>`, as the execution
/// environment states it (`AWS_EXECUTION_ENV`).
fn library_root() -> String {
    format!("/var/lang/lib/{PYTHON}")
}

/// One row: the statement the function fails on, and how many frames inside the library the
/// stack must reach at least.
struct LibraryFrameCase {
    case: &'static str,
    /// The body of the function the handler calls, which is where the failure is taken.
    statement: &'static str,
    /// The least number of frames naming a file under the library root.
    least_library_frames: usize,
}

/// The rows.
///
/// Every statement fails INSIDE a module of the standard library rather than in the package's
/// own code, so the deepest frames of each stack are library frames. The modules the
/// statements reach are the runtime's own (`os`, `json`, `random`, `shlex`, `re`), each of
/// which this runtime offers.
fn library_frame_cases() -> Vec<LibraryFrameCase> {
    vec![
        LibraryFrameCase {
            case: "a key the environment does not hold, raised in a module at the top of the \
                   library",
            statement: "return os.environ[\"MISSING_KEY_FOR_THIS_ROW\"]",
            least_library_frames: 1,
        },
        LibraryFrameCase {
            case: "a document that is not JSON, raised inside a package of the library",
            statement: "return json.loads('{\"a\":')",
            // The package's own module and the module inside it the decoding is done by.
            least_library_frames: 2,
        },
        LibraryFrameCase {
            case: "a draw from an empty sequence, a shallower stack in another top-level module",
            statement: "return random.choice([])",
            least_library_frames: 1,
        },
        LibraryFrameCase {
            case: "an unterminated quotation, raised in a module the handler drives directly",
            statement: "return shlex.split('\\\"unterminated')",
            least_library_frames: 1,
        },
        LibraryFrameCase {
            case: "a pattern that does not parse, three modules of one package deep",
            statement: "return re.compile('[')",
            least_library_frames: 2,
        },
    ]
}

/// The handler module of one row: the statement under test in a function of its own, a handler
/// that lets the failure out, and the same handler asked to RENDER the same failure with the
/// standard library's own formatter.
fn module(statement: &str) -> String {
    [
        "import json",
        "import os",
        "import random",
        "import re",
        "import shlex",
        "import sys",
        "import traceback",
        "",
        "",
        "def failing(event):",
        &format!("    {statement}"),
        "",
        "",
        "def handler(event, context):",
        "    if event.get(\"render\"):",
        "        try:",
        "            return failing(event)",
        "        except BaseException:",
        "            return traceback.format_tb(sys.exc_info()[2])",
        "    return failing(event)",
        "",
    ]
    .join("\n")
}

/// What one rendered frame STATES: the file it names, the line number in it, the symbol the
/// frame is in, and the source line it carries (empty for a frame carrying none).
fn stated(frame: &str) -> Option<(String, usize, String, String)> {
    let lines = frame_lines(frame);
    let (file, number) = located(lines.first()?)?;
    let symbol = lines
        .first()?
        .rsplit_once(", in ")
        .map(|(_located, symbol)| symbol.to_string())?;
    let source = lines.get(1).copied().unwrap_or_default().to_string();
    Some((file, number, symbol, source))
}

/// The frames of `frames` that name a file under the runtime's own library directory.
fn library_frames(frames: &[String], root: &str) -> Vec<String> {
    frames
        .iter()
        .filter(|frame| {
            frame_lines(frame)
                .first()
                .and_then(|line| located(line))
                .is_some_and(|(file, _line)| file.starts_with(root))
        })
        .cloned()
        .collect()
}

/// The POSITION and the source line of each of `frames`: the line number, the symbol and the
/// source line, with the file left out.
///
/// The file is left out because the two renderings compared here name it differently by
/// design: the runtime renders a library module's frame as that module's file under the
/// runtime's own library directory, while a rendering made INSIDE the run names the unit the
/// module was compiled under — which is the naming rule
/// [`crate::invoke_function_error_document`] pins. What this suite is about is the other half
/// of the frame: the position, and the source line at it.
fn positions(frames: &[String]) -> Vec<(usize, String, String)> {
    frames
        .iter()
        .filter_map(|frame| stated(frame))
        .map(|(_file, number, symbol, source)| (number, symbol, source))
        .collect()
}

/// The frames of a rendering made INSIDE the run that are the library's own: a module of the
/// library is named by the unit it was compiled under rather than by a path, so its frames are
/// the ones naming no absolute file.
fn compiled_unit_frames(frames: &[String]) -> Vec<String> {
    frames
        .iter()
        .filter(|frame| {
            frame_lines(frame)
                .first()
                .and_then(|line| located(line))
                .is_some_and(|(file, _line)| !file.starts_with('/'))
        })
        .cloned()
        .collect()
}

#[tokio::test]
async fn a_library_frame_carries_the_source_line_it_stands_on() {
    let _stream = one_invocation_stream_at_a_time().await;
    let root = library_root();
    // One engine for the whole table: every row is its own function on it, so the
    // provisioning window the creates document is waited out once.
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let cases = library_frame_cases();
    let sources: Vec<String> = cases
        .iter()
        .map(|case| module(case.statement))
        .collect();
    for (index, source) in sources.iter().enumerate() {
        create_function(
            &router,
            &format!("python-library-frame-{index}"),
            &[("index.py", source.as_str())],
            "index.handler",
        )
        .await;
    }
    for index in 0..cases.len() {
        poll_until_active(&router, &format!("python-library-frame-{index}")).await;
    }

    for (index, case) in cases.iter().enumerate() {
        let name = format!("python-library-frame-{index}");

        // The failure let out of the handler, as a client reads it: the error document of a
        // run that raised, answered with the recorded status of one.
        let raised = invoke(&router, &name, b"{}").await;
        assert_eq!(
            raised.status,
            200,
            "case {:?}: a run that raised is still an HTTP 200, got {} {}",
            case.case,
            raised.status,
            raised.text()
        );
        let frames = raised.frames();
        assert!(
            !frames.is_empty(),
            "case {:?}: expected the raised error's own frames, got {}",
            case.case,
            raised.text()
        );

        // Every frame naming a source FILE of the environment — the package's own modules
        // under the task root and the library's under its own directory — is its location and
        // the source line at it.
        for (position, frame) in frames.iter().enumerate() {
            let lines = frame_lines(frame);
            let Some((file, number)) = lines.first().and_then(|line| located(line)) else {
                panic!(
                    "case {:?}: frame {position} must open with its location, got {frame:?}",
                    case.case
                );
            };
            assert_eq!(
                lines.len(),
                2,
                "case {:?}: frame {position} of {file:?} line {number} must be its location \
                 and the source line at it, got {frame:?}",
                case.case
            );
            let statement = lines[1];
            assert!(
                statement.starts_with("    ") && !statement.trim().is_empty(),
                "case {:?}: frame {position} of {file:?} must carry its source line indented \
                 under the location, got {statement:?}",
                case.case
            );
        }

        // The library's own frames, and as many of them as the row reaches.
        let inside = library_frames(&frames, &root);
        assert!(
            inside.len() >= case.least_library_frames,
            "case {:?}: expected at least {} frames inside {root:?}, got {frames:?}",
            case.case,
            case.least_library_frames
        );

        // The same failure, rendered by the STANDARD LIBRARY's own formatter inside the
        // function: the frames the error document carries for the library are exactly the
        // ones `traceback` makes of them.
        let rendered = invoke(&router, &name, br#"{"render": true}"#).await;
        assert_eq!(
            rendered.status,
            200,
            "case {:?}: the rendering invocation must answer, got {} {}",
            case.case,
            rendered.status,
            rendered.text()
        );
        let formatted: Vec<String> = rendered
            .json()
            .as_array()
            .map(|frames| {
                frames
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            !formatted.is_empty(),
            "case {:?}: expected the handler's own rendering of the same failure, got {}",
            case.case,
            rendered.text()
        );
        let formatted_inside = positions(&compiled_unit_frames(&formatted));
        let inside_positions = positions(&inside);
        assert_eq!(
            inside_positions,
            formatted_inside,
            "case {:?}: the library's frames must read as `traceback` renders them \
             (line, symbol, source line), expected {formatted_inside:?}, got \
             {inside_positions:?}",
            case.case
        );
    }
}
