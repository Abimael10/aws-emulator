// SPDX-License-Identifier: CC0-1.0

//! The modules a deployment package VENDORS, imported by the handler: Lambda alone, a
//! `python3.x` function whose package ships a dependency of its own beside `index.py`, and the
//! value that dependency computed answered as the invocation's payload.
//!
//! RULE: the standard library of the runtime a function declares is on the import path of
//! EVERY module the deployment package holds, not only of the handler's own — so a dependency
//! a package vendors imports the socket interface, the TLS layer and the HTTP client built on
//! them at its module body, exactly as the handler's module does, and `Invoke` answers 200
//! with no `X-Amz-Function-Error` and the handler's own value. A capability the sandbox cannot
//! serve is refused by the CALL that needs it and never by the import: a module-load failure
//! blames a deployment package that is not at fault, and it is what a function vendoring a
//! client library of its own would die of.
//!
//! The rows differ along what the rule generalises over: WHERE the vendored module sits in the
//! package (beside the handler, and inside a package directory of its own with an
//! `__init__.py`), HOW the handler reaches it (`import <name>`, and `from <package> import
//! <module>`), and WHICH of the runtime's own standard-library modules the vendored body
//! imports. `python_stdlib_module_surface.rs` pins the same rule for the HANDLER's own module
//! body and for the import system at run time; what is new here is a module of the package
//! that the handler itself imports.
//!
//! The suite RUNS a handler, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;

use base64::Engine as _;
use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{await_function_active, call, router_on};
// The one .zip writer this crate's suites package a handler with, exported by the suite that
// first needed it rather than written again here.
use crate::node_invoke_run::package;

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern. Nothing here is
/// about authorization: the handler makes no call of its own.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// Lambda on its own engine, reached the way the production router reaches it: this suite's
/// handler calls no other service.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    router_on(&engine)
}

/// The module body of a vendored dependency: it imports what a client library of its own
/// imports — the socket interface, the TLS layer over it, the standard library's HTTP client
/// and the waiting interface a connection pool is built on — and answers the names it found,
/// so what the handler reports is read out of the modules themselves.
const VENDORED_SOURCE: &str = r#"
import http.client
import select
import socket
import ssl

NAMES = sorted(
    name
    for name in (
        "socket" if hasattr(socket, "socket") else "",
        "create_connection" if hasattr(socket, "create_connection") else "",
        "SSLContext" if hasattr(ssl, "SSLContext") else "",
        "HTTPSConnection" if hasattr(http.client, "HTTPSConnection") else "",
        "select" if hasattr(select, "select") else "",
    )
    if name
)


def imported():
    """What this module imported at its body, as the handler reports it."""
    return NAMES
"#;

/// The handler of the row whose vendored module sits BESIDE it: it imports that module and
/// answers what the module computed at its own body.
const FLAT_HANDLER_SOURCE: &str = r#"
import vendored_client


def handler(event, context):
    return {"imported": vendored_client.imported()}
"#;

/// The handler of the row whose vendored module sits inside a package DIRECTORY: the same
/// import reached the other way a dependency is shipped.
const PACKAGED_HANDLER_SOURCE: &str = r#"
from vendor.client import imported


def handler(event, context):
    return {"imported": imported()}
"#;

/// What every row's vendored module must report having imported: the names its body read off
/// the runtime's own standard library, in the order it sorts them.
const IMPORTED: &[&str] = &[
    "HTTPSConnection",
    "SSLContext",
    "create_connection",
    "select",
    "socket",
];

/// One row: where the vendored module sits, how the handler reaches it, and the function it is
/// deployed as.
struct Row {
    case: &'static str,
    function: &'static str,
    handler: &'static str,
    entries: &'static [(&'static str, &'static str)],
}

/// The rows, spanning where a package may hold a dependency of its own.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a vendored module beside the handler",
            function: "vendored-flat-caller",
            handler: "index.handler",
            entries: &[
                ("index.py", FLAT_HANDLER_SOURCE),
                ("vendored_client.py", VENDORED_SOURCE),
            ],
        },
        Row {
            case: "a vendored module inside a package directory",
            function: "vendored-packaged-caller",
            handler: "index.handler",
            entries: &[
                ("index.py", PACKAGED_HANDLER_SOURCE),
                ("vendor/__init__.py", ""),
                ("vendor/client.py", VENDORED_SOURCE),
            ],
        },
    ]
}

/// RULE: a module the deployment package VENDORS imports the runtime's own standard library at
/// its body, so the handler that imports it runs and the invocation answers its value rather
/// than a `Runtime.ImportModuleError`.
#[tokio::test]
async fn a_vendored_module_imports_the_runtimes_own_standard_library() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let router = emulator();

    for row in rows() {
        let case = row.case;
        let created = call(
            &router,
            "POST",
            FUNCTIONS_URI,
            &json!({
                "FunctionName": row.function,
                "Role": ROLE,
                "Runtime": "python3.12",
                "Handler": row.handler,
                "Timeout": 25,
                "Code": {
                    "ZipFile": base64::engine::general_purpose::STANDARD
                        .encode(package(row.entries)),
                },
            }),
        )
        .await;
        assert_eq!(
            created.status,
            201,
            "case {case:?}: expected CreateFunction to answer 201, got {} {}",
            created.status,
            created.text()
        );
        await_function_active(&router, case, row.function).await;

        let answered = call(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/{}/invocations", row.function),
            &json!({}),
        )
        .await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
            answered.status,
            answered.text()
        );
        // The import fault this rule is about is reported exactly here: a 200 carrying the
        // runtime's own fault document instead of the handler's value.
        assert_eq!(
            answered.header("x-amz-function-error"),
            None,
            "case {case:?}: the vendored module must import and the handler must run, got {}",
            answered.text()
        );
        let payload = answered.json();
        assert_eq!(
            payload["imported"],
            json!(IMPORTED),
            "case {case:?}: expected the names the vendored module read off the runtime's own \
             standard library, got {payload}"
        );
    }
}
