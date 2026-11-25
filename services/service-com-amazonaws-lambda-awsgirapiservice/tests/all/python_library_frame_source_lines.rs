// SPDX-License-Identifier: CC0-1.0

//! What a frame INSIDE the runtime's own standard library is rendered as, in the stack a
//! failed Python invocation answers with.
//!
//! The rule under test: a module of the standard library this runtime serves is compiled INTO
//! the interpreter, under its own module name and with no file behind it, so a traceback frame
//! standing in one of them is ONE line naming the FROZEN module —
//! `  File "<frozen <module>>", line <n>, in <name>` — and carries no source text at all,
//! while a frame of a module a FILE of the execution environment holds (the deployment
//! package's own, under the task root) is its location and the source line at it
//! ([`crate::python_frame_source_lines`], which pins that half of the rule). No frame of such
//! a module names a path inside the runtime's library directory: no file of the environment
//! holds it, so a path there would name a file nothing could be read from, and the statement
//! the error was taken on would be read off text no running code was compiled from.
//!
//! The rows differ along what the rule generalises over: WHICH module of the library the
//! failure is taken in (a module at the top of the library, a module inside a package, a
//! package's own module, and a stack passing through three modules of one package), the class
//! the failure is reported as, and HOW DEEP the stack is when it is taken.
//!
//! Every row drives real HTTP requests through the router, and nothing about a frame is
//! written out here: the frozen modules the error document names are compared against the
//! units the STANDARD LIBRARY's own formatter names for the very same failure, which the
//! function itself is asked for on a second invocation. So the assertion is the rule — a
//! frozen module's frame names that module and stands on nothing — rather than one recorded
//! traceback.

use std::sync::Arc;

use serde_json::Value;

use crate::invoke_request_id::one_invocation_stream_at_a_time;
use crate::python_frame_source_lines::{
    create_function, frame_lines, invoke, located, poll_until_active, router_on, PYTHON,
};

/// The directory a managed runtime keeps the standard library it ships under, named for the
/// runtime the function is configured with (`AWS_EXECUTION_ENV`). This runtime holds no FILE
/// of a frozen module there, so no frame of one may name a path under it.
fn library_root() -> String {
    format!("/var/lang/lib/{PYTHON}")
}

/// What a frame names when it stands in a module compiled into the interpreter: the module's
/// own name, written as the frozen module it is.
fn frozen(module: &str) -> String {
    format!("<frozen {module}>")
}

/// One row: the statement the function fails on, and how many frames inside the library the
/// stack must reach at least.
struct LibraryFrameCase {
    case: &'static str,
    /// The body of the function the handler calls, which is where the failure is taken.
    statement: &'static str,
    /// The least number of frames standing in a frozen module of the library.
    least_library_frames: usize,
}

/// The rows.
///
/// Every statement fails INSIDE a module of the standard library rather than in the package's
/// own code, so the deepest frames of each stack are library frames. The modules the
/// statements reach are the ones this runtime serves frozen (`os`, `json`, `random`, `shlex`,
/// `re`), none of which is a file of the execution environment.
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

/// What one rendered frame STATES: the unit it names, the line number in it, the symbol the
/// frame is in, and the source line it carries (empty for a frame carrying none).
fn stated(frame: &str) -> Option<(String, usize, String, String)> {
    let lines = frame_lines(frame);
    let (unit, number) = located(lines.first()?)?;
    let symbol = lines
        .first()?
        .rsplit_once(", in ")
        .map(|(_located, symbol)| symbol.to_string())?;
    let source = lines.get(1).copied().unwrap_or_default().to_string();
    Some((unit, number, symbol, source))
}

/// The frames of `frames` that stand in a module no file holds: the ones whose unit is written
/// as a pseudo-file rather than as a path.
fn frozen_frames(frames: &[String]) -> Vec<String> {
    frames
        .iter()
        .filter(|frame| {
            stated(frame)
                .is_some_and(|(unit, _, _, _)| unit.starts_with('<') && unit.ends_with('>'))
        })
        .cloned()
        .collect()
}

/// The frames of a rendering made INSIDE the run that are the library's own: the interpreter
/// names a frozen module by the unit it was compiled under, which is its module NAME, so those
/// are the frames naming no absolute path.
fn compiled_unit_frames(frames: &[String]) -> Vec<String> {
    frames
        .iter()
        .filter(|frame| stated(frame).is_some_and(|(unit, _, _, _)| !unit.starts_with('/')))
        .cloned()
        .collect()
}

/// What the error document must carry for a frame the run's own rendering states as `frame`:
/// the same position and symbol, with the unit written as the frozen module it is and no
/// source text under it.
///
/// A trailing `.py` on the compiled unit is dropped, because the unit a frozen module is
/// compiled under is its module NAME and that is what the frozen origin states.
fn expected_frozen(frame: &str) -> Option<(String, usize, String, String)> {
    let (unit, number, symbol, _source) = stated(frame)?;
    let module = unit.strip_suffix(".py").unwrap_or(&unit).to_string();
    Some((frozen(&module), number, symbol, String::new()))
}

#[tokio::test]
async fn a_library_frame_names_the_frozen_module_it_stands_in() {
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

        for (position, frame) in frames.iter().enumerate() {
            let lines = frame_lines(frame);
            let Some((unit, number, _symbol, source)) = stated(frame) else {
                panic!(
                    "case {:?}: frame {position} must open with its location, got {frame:?}",
                    case.case
                );
            };
            if unit.starts_with('/') {
                // A module a FILE of the environment holds: its location and the source line
                // at it, which is the deployment package's own modules here.
                assert_eq!(
                    lines.len(),
                    2,
                    "case {:?}: frame {position} of {unit:?} line {number} must be its \
                     location and the source line at it, got {frame:?}",
                    case.case
                );
                assert!(
                    source.starts_with("    ") && !source.trim().is_empty(),
                    "case {:?}: frame {position} of {unit:?} must carry its source line \
                     indented under the location, got {source:?}",
                    case.case
                );
                continue;
            }
            // A module compiled into the interpreter: the frozen module it is, and nothing
            // under it.
            assert!(
                unit.starts_with("<frozen ") && unit.ends_with('>'),
                "case {:?}: frame {position} stands in no file of the environment, so it must \
                 name the frozen module it is in, got {unit:?}",
                case.case
            );
            assert_eq!(
                lines.len(),
                1,
                "case {:?}: frame {position} stands in {unit:?}, which no file holds, so it \
                 carries no source text, got {frame:?}",
                case.case
            );
        }

        // Said the other way round, over the value a client must never be shown: the library
        // directory of a managed runtime holds no file of a frozen module, so no frame names a
        // path under it.
        let leaked: Vec<&String> = frames
            .iter()
            .filter(|frame| stated(frame).is_some_and(|(unit, _, _, _)| unit.starts_with(&root)))
            .collect();
        assert!(
            leaked.is_empty(),
            "case {:?}: no frame of a frozen module may name a path under {root:?}, got \
             {leaked:?}",
            case.case
        );

        // The library's own frames, and as many of them as the row reaches.
        let inside = frozen_frames(&frames);
        assert!(
            inside.len() >= case.least_library_frames,
            "case {:?}: expected at least {} frames standing in a frozen module, got \
             {frames:?}",
            case.case,
            case.least_library_frames
        );

        // The same failure, rendered by the STANDARD LIBRARY's own formatter inside the
        // function: the library frames the error document carries are exactly those frames,
        // each naming the frozen module the interpreter compiled that unit under.
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
        let expected: Vec<(String, usize, String, String)> = compiled_unit_frames(&formatted)
            .iter()
            .filter_map(|frame| expected_frozen(frame))
            .collect();
        let reported: Vec<(String, usize, String, String)> =
            inside.iter().filter_map(|frame| stated(frame)).collect();
        assert_eq!(
            reported, expected,
            "case {:?}: every library frame must name the frozen module the interpreter \
             compiled it under, at the position `traceback` states, with no source text, \
             expected {expected:?}, got {reported:?}",
            case.case
        );
    }
}
