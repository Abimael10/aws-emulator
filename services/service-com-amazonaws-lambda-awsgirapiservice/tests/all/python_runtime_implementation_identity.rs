// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the IDENTITY of the interpreter a `python3.x` function's run happens
//! on: `CreateFunction` then `Invoke` (`POST /2015-03-31/functions/{FunctionName}/invocations`)
//! over the router.
//!
//! The rule under test: the identity a handler reads off the interpreter it is running on is
//! the identity of the managed runtime its function is configured with — the implementation
//! that runtime is (a `python3.x` runtime is CPython), the release the identifier names, and
//! the machine the function's `Architectures` names — and never anything of the engine this
//! emulator embeds to run it or of the machine this process happens to be on.
//!
//! Every row drives real HTTP requests through the router, and the rows differ along what the
//! rule generalises over: WHICH `Runtime` identifier the function carries (four members of the
//! model's own enum, including two the issue behind this suite never cited), which
//! `Architectures` entry it carries beside it (both values of that enum), whether the identity
//! is read while the handler's module is being IMPORTED or from the handler itself, and whether
//! the invocation is the one its execution environment was built for or one served in an
//! environment already standing.
//!
//! The requests themselves go through the harness this target already has for suites that RUN
//! Python handlers ([`crate::python_frame_source_lines`]): one router on one engine, one
//! dispatch, the poll that ends on the `Active` state a create owes, and the invocation that is
//! re-driven while the answer reports one of the emulator's own host bounds.

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};

use crate::invoke_request_id::{is_host_bound, one_invocation_stream_at_a_time};
use crate::node_invoke_run::package;
use crate::python_frame_source_lines::{call, invoke, poll_until_active, router_on, FUNCTIONS_URI};

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The `Timeout` every function this suite creates carries, in seconds. Sized the way the
/// sibling suites that RUN handlers size theirs: it has to cover this test binary's own
/// scheduling, not the handlers' work, which is immediate.
const FUNCTION_TIMEOUT_SECONDS: i32 = 25;

/// Create a function carrying `runtime` and `architecture`, whose package holds the probe as
/// `index.py`, and wait for the `Active` state the create owes it.
async fn create_function(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
    runtime: &str,
    architecture: &str,
) {
    let body = json!({
        "FunctionName": name,
        "Role": ROLE,
        "Runtime": runtime,
        "Architectures": [architecture],
        "Handler": "index.handler",
        "Timeout": FUNCTION_TIMEOUT_SECONDS,
        "Code": {
            "ZipFile": base64::engine::general_purpose::STANDARD
                .encode(package(&[("index.py", PROBE_SOURCE)])),
        },
    });
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&body).expect("the create body serializes"),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateFunction {name:?} with {runtime} must succeed, got {} {}",
        created.status,
        created.text()
    );
    // The runtime and the architecture the create was answered with are the ones it was asked
    // for, which is what the run below is the run of.
    let echoed = created.json();
    assert_eq!(
        echoed.get("Runtime").and_then(Value::as_str),
        Some(runtime),
        "CreateFunction {name:?} must report the runtime it was given, got {echoed}"
    );
    assert_eq!(
        echoed.get("Architectures").cloned().unwrap_or_default(),
        json!([architecture]),
        "CreateFunction {name:?} must report the architecture it was given, got {echoed}"
    );
    poll_until_active(router, name).await;
}

/// What one row's handler answered, or `None` when the run was ended by a bound this whole
/// test binary shares rather than by anything the row asked for.
async fn answered(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
    case: &str,
) -> Option<Value> {
    let answer = invoke(router, name, b"{}").await;
    if is_host_bound(answer.status, &answer.text()) {
        return None;
    }
    assert_eq!(
        answer.status,
        200,
        "case {case}: expected 200, got {} {}",
        answer.status,
        answer.text()
    );
    let document = answer.json();
    // The handler's module imports cleanly and reads the identity of the interpreter it is on,
    // so what comes back is the value it returned and not the document a raise is reported as.
    assert_eq!(
        document.get("errorType"),
        None,
        "case {case}: the run must answer with the handler's own value, got {document}"
    );
    Some(document)
}

/// One row of the table: the `Runtime` the function is created with, the `Architectures` entry
/// beside it, the release that runtime names and the machine that architecture is.
struct Row {
    /// The case this row is named by in every assertion.
    case: &'static str,
    /// The `Runtime` identifier the create carries.
    runtime: &'static str,
    /// The `Architectures` entry the create carries.
    architecture: &'static str,
    /// The major and minor version a handler of that function must observe.
    release: (u64, u64),
    /// The machine a handler of that function must observe, which is what the Linux kernel
    /// calls that instruction set.
    machine: &'static str,
}

/// The whole table: four `python` members of the model's `Runtime` enum
/// (`smithy_query(query="shape", shape="Runtime")`), each paired with a value of the
/// `Architectures` enum (`smithy_query(query="shape", shape="Architecture")`).
///
/// `python3.9` and `python3.13` are members the issue behind this suite never cited, so the
/// table reads the rule rather than the one pairing one run reported.
const ROWS: &[Row] = &[
    Row {
        case: "a python3.12 function on x86_64",
        runtime: "python3.12",
        architecture: "x86_64",
        release: (3, 12),
        machine: "x86_64",
    },
    Row {
        case: "a python3.11 function on arm64",
        runtime: "python3.11",
        architecture: "arm64",
        release: (3, 11),
        machine: "aarch64",
    },
    Row {
        case: "a python3.13 function on arm64",
        runtime: "python3.13",
        architecture: "arm64",
        release: (3, 13),
        machine: "aarch64",
    },
    Row {
        case: "a python3.9 function on x86_64",
        runtime: "python3.9",
        architecture: "x86_64",
        release: (3, 9),
        machine: "x86_64",
    },
];

/// The whole rule, driven END TO END as one test: every row below is a real `CreateFunction`
/// and a real `Invoke` over the router.
///
/// One test and one stream of invocations, the way the sibling runtime suites are built: the
/// memory bound a run is stopped on is read from the WHOLE process
/// (`state::execution::python`) and this binary runs its suites in parallel, so fewer
/// interpreters in flight at once is fewer runs stopped for memory belonging to another suite.
/// A row whose run was stopped by one of those shared bounds says nothing about the rule and is
/// left out of the reading — but not every one can be: a test that read none of them at all is
/// a test that asserted nothing, so it fails rather than passing silently.
#[tokio::test]
async fn a_python_run_reports_the_identity_of_the_runtime_it_is_of() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let mut read = 0_usize;
    for (index, row) in ROWS.iter().enumerate() {
        let name = format!("probe-runtime-implementation-{index}");
        create_function(&router, &name, row.runtime, row.architecture).await;
        let case = row.case;
        let (major, minor) = row.release;
        let machine = row.machine;
        let expected = json!({
            // The implementation the standard library's `platform` module reports, which it
            // derives from the version TEXT the interpreter carries.
            "python_implementation": "CPython",
            // The implementation record a program reads the same identity off directly.
            "implementation_name": "cpython",
            "implementation_release": [major, minor],
            "cache_tag": format!("cpython-{major}{minor}"),
            // One identity, so the number the record packs the release into is the number the
            // interpreter itself reports.
            "same_hexversion": true,
            // Neither the version text nor the compiler statement in it names the engine this
            // emulator embeds, because that is no statement of the function's runtime.
            "version_names_another_implementation": false,
            "compiler_names_another_implementation": false,
            // The module body read the identity WHILE BEING IMPORTED, which is where a
            // vendored client library decides which interpreter it is on.
            "imported_implementation": "CPython",
            // The machine the identity names is the function's own architecture, the same one
            // every other reading of this execution environment reports.
            "multiarch_machine": machine,
            "uname_machine": machine,
            // And the release the identity carries is the one the runtime identifier names.
            "release": [major, minor],
        });
        // Twice: the invocation an execution environment is BUILT for, and one served in the
        // environment that is then standing — the identity belongs to the environment, so a
        // warm invocation reads the same one.
        for served_in in [
            "the environment this invocation built",
            "the environment already standing",
        ] {
            let Some(document) = answered(&router, &name, case).await else {
                continue;
            };
            read += 1;
            assert_eq!(
                document, expected,
                "case {case} ({served_in}): expected the identity of that managed runtime, got \
                 {document}"
            );
        }
    }
    assert!(
        read > 0,
        "expected at least one row of the table above to be answered by a run, got {read}: \
         every invocation reported a bound this whole test binary shares, so nothing of the \
         rule was read"
    );
}

/// The probe every row drives: its module body reads the interpreter's implementation — the
/// shape a vendored client library's own module body has — and its handler answers every
/// reading of the interpreter's identity a Python program can make.
const PROBE_SOURCE: &str = r#"
import os
import platform
import sys

# Read WHILE THIS MODULE IS BEING IMPORTED, which is where a client library decides which
# interpreter it is running on.
_IMPORTED = platform.python_implementation()


def handler(event, context):
    return {
        "python_implementation": platform.python_implementation(),
        "implementation_name": sys.implementation.name,
        "implementation_release": list(sys.implementation.version[:2]),
        "cache_tag": sys.implementation.cache_tag,
        "same_hexversion": sys.implementation.hexversion == sys.hexversion,
        "version_names_another_implementation": "rustpython" in sys.version.lower(),
        "compiler_names_another_implementation": (
            "rustpython" in platform.python_compiler().lower()
        ),
        "imported_implementation": _IMPORTED,
        "multiarch_machine": sys.implementation._multiarch.split("-")[0],
        "uname_machine": os.uname().machine,
        "release": list(sys.version_info[:2]),
    }
"#;
