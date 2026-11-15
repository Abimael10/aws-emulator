// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the FILES a `python3.x` deployment package ships beside its modules:
//! `CreateFunction` then `Invoke` (`POST
//! /2015-03-31/functions/{FunctionName}/invocations`) over the router.
//!
//! The rule under test: a `.zip` deployment package's own files are all there for the run —
//! a handler, or a client library the package vendors, reads a file of its own package back
//! through the standard library's resource reading and gets that file's text, and a file the
//! package does not hold reads as absent. The reading takes the encoding AND the error
//! handling its caller named, because that is what the library's own by-name reads pass on.
//!
//! Every row drives real HTTP requests through the router, and the rows differ along what the
//! rule generalises over: which package holds the file (a package directory of the archive,
//! the task root beside the handler), how deep its path is, which of the library's calls
//! reads it (`files()` and the by-name reads built on it), what is asked for (its text, its
//! bytes, whether it is there, what is beside it), which encoding is named, and whether the
//! file is one the package holds at all.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ACTIVATION_DELAY;
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::invoke_request_id::{is_host_bound, one_invocation_stream_at_a_time};
use crate::node_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A SigV4 credential scope naming this service's signing name (`lambda`) in `us-east-1`.
/// Nothing in it is verified: the harness turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=emulator-test-key/20260101/us-east-1/lambda/aws4_request, \
     SignedHeaders=host, Signature=not-verified";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// How long past a due instant this suite waits before calling. The transition is owed by
/// the CLOCK, so nothing here sleeps on the emulator's behalf.
const SLACK: Duration = Duration::from_millis(150);

/// The `Timeout` the function this suite creates carries, in seconds. Sized the way the
/// sibling suites that RUN handlers size theirs: it has to cover this test binary's own
/// scheduling, not the handler's work, which is immediate.
const FUNCTION_TIMEOUT_SECONDS: i32 = 25;

/// How many times a row re-drives its invocation while the answer reports one of this test
/// binary's own host bounds rather than a run.
const HOST_BOUND_ATTEMPTS: usize = 4;

/// The text of the file the vendored package ships: a certificate bundle is the file a
/// client library reads out of its own package, so the row that stands for one is one.
const BUNDLE: &str = "-----BEGIN CERTIFICATE-----\nQUJD\n-----END CERTIFICATE-----\n";

/// The text of a file whose bytes are not ASCII, so an encoding a caller names is really
/// applied to the file's own bytes.
const TRANSLATED: &str = "{\"label\": \"r\u{00e9}sum\u{00e9}\"}\n";

/// What a dispatched request answered.
struct Answer {
    status: u16,
    headers: http::HeaderMap,
    body: bytes::Bytes,
}

impl Answer {
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }
}

/// One service mounted on its own engine, reached the way the production router reaches it.
fn router_on(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(engine)));
    router
}

/// Dispatch one request.
async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: &[u8]) -> Answer {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://lambda.us-east-1.amazonaws.com{uri}"))
        .header("host", "lambda.us-east-1.amazonaws.com")
        .header("authorization", AUTHORIZATION)
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(body.to_vec()))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// The package the probe runs out of: the handler's module, a vendored package of two
/// modules, and the FILES each of them ships beside those modules — at the vendored
/// package's root, one directory deeper, and beside the handler at the task root.
fn probe_entries() -> Vec<(&'static str, &'static str)> {
    vec![
        ("index.py", PROBE_SOURCE),
        ("vendored/__init__.py", "from vendored import reader\n"),
        ("vendored/reader.py", "MARKER = 'read'\n"),
        ("vendored/cacert.pem", BUNDLE),
        ("vendored/data/__init__.py", ""),
        ("vendored/data/table.csv", "id,total\n1,42\n"),
        ("vendored/data/labels.json", TRANSLATED),
        ("notes.txt", "beside the handler\n"),
    ]
}

/// Create the `python3.12` probe function and wait out the provisioning window the create
/// documents.
async fn create_function(router: &MultiServiceRouter, name: &str) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Timeout": FUNCTION_TIMEOUT_SECONDS,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&probe_entries())),
            },
        }))
        .expect("the create body serializes"),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateFunction {name:?} must succeed, got {} {}",
        created.status,
        created.text()
    );
    tokio::time::sleep(ACTIVATION_DELAY + SLACK).await;
}

/// `Invoke` on `name` with `event`, re-driven while the answer reports a bound this whole
/// test binary shares rather than a run.
async fn invoke(router: &MultiServiceRouter, name: &str, event: &Value) -> Answer {
    let mut answered = invoke_once(router, name, event).await;
    for _ in 1..HOST_BOUND_ATTEMPTS {
        if !is_host_bound(answered.status, &answered.text()) {
            break;
        }
        answered = invoke_once(router, name, event).await;
    }
    answered
}

/// One `Invoke` request on the function `name` names.
async fn invoke_once(router: &MultiServiceRouter, name: &str, event: &Value) -> Answer {
    call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        &serde_json::to_vec(event).expect("the event serializes"),
    )
    .await
}

/// What one case answered, or `None` when the run was ended by a bound this whole test
/// binary shares rather than by anything the case asked for.
async fn answered(
    router: &MultiServiceRouter,
    name: &str,
    case: &str,
    event: &Value,
) -> Option<Value> {
    let answer = invoke(router, name, event).await;
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
    let failure = answer.header("x-amz-function-error");
    assert_eq!(
        failure, None,
        "case {case}: the handler imports cleanly and catches each refusal itself, so the \
         invocation carries no function error, got {failure:?} with {}",
        answer.text()
    );
    Some(answer.json())
}

/// The whole rule, driven END TO END as one test: the table below is a real `CreateFunction`
/// and real `Invoke` requests over the router.
///
/// One test and one stream of invocations, the way the sibling runtime suites are built: the
/// memory bound a run is stopped on is read from the WHOLE process
/// (`state::execution::python`) and this binary runs its suites in parallel, so fewer
/// interpreters in flight at once is fewer runs stopped for memory belonging to another
/// suite. A case whose run was stopped by one of those shared bounds says nothing about the
/// rule and is left out of the reading — but not every one can be: each table answers how
/// many of its cases a RUN answered, and a test that read none at all is a test that asserted
/// nothing, so it fails rather than passing silently.
#[tokio::test]
async fn a_python_run_reads_the_files_its_own_package_ships() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let probe = "probe-package-resources";
    create_function(&router, probe).await;
    let mut read = a_file_the_package_ships_is_read_back(&router, probe).await;
    read += what_is_beside_a_file_is_the_packages_own(&router, probe).await;
    read += a_file_the_package_does_not_hold_reads_as_absent(&router, probe).await;
    assert!(
        read > 0,
        "expected at least one case of the tables above to be answered by a run, got {read}: \
         every invocation reported a bound this whole test binary shares, so nothing of the \
         rule was read"
    );
}

/// A file the package ships is read back, whichever call of the library reads it and whatever
/// is asked of it.
///
/// Answers how many cases a run answered.
async fn a_file_the_package_ships_is_read_back(
    router: &MultiServiceRouter,
    probe: &str,
) -> usize {
    let case = "a file the package ships";
    let Some(document) = answered(router, probe, case, &json!({"case": "held"})).await else {
        return 0;
    };
    let expected = json!({
        // The traversable the current reading walks, at the vendored package's own root and
        // one directory deeper.
        "through_files": BUNDLE,
        "through_files_nested": "id,total\n1,42\n",
        // The by-name reads built on it, which is what a library written for the advertised
        // runtime calls: the text, the text under an encoding the caller names, and the
        // bytes.
        "by_name": BUNDLE,
        "by_name_nested": "id,total\n1,42\n",
        "by_name_encoded": BUNDLE,
        "by_name_bytes": BUNDLE,
        // A file whose bytes are not ASCII, read under the encoding they really are and
        // under one they are not.
        "translated": TRANSLATED,
        "mis_encoded": "UnicodeDecodeError",
        // The handler's own module ships files too: the one beside it at the task root.
        "beside_the_handler": "beside the handler\n",
        // A module of the package, not only a package directory of it, reads the files
        // beside it.
        "beside_a_module": BUNDLE,
        // A package's own MODULE is a file of it as well, because a package ships its
        // modules and its data in one archive.
        "a_module_is_a_file": "MARKER = 'read'\n",
    });
    assert_eq!(
        document, expected,
        "case {case}: expected the file's own text back, got {document}"
    );
    1
}

/// What is BESIDE a file is the package's own: the entries of a directory, and whether a
/// name is a file of it at all.
///
/// Answers how many cases a run answered.
async fn what_is_beside_a_file_is_the_packages_own(
    router: &MultiServiceRouter,
    probe: &str,
) -> usize {
    let case = "what is beside a file";
    let Some(document) = answered(router, probe, case, &json!({"case": "beside"})).await else {
        return 0;
    };
    let expected = json!({
        // The entries of the vendored package's own directory: the files it ships, the
        // directory below it, and its own modules, which are files of it too.
        "entries": ["__init__.py", "cacert.pem", "data", "reader.py"],
        "nested_entries": ["__init__.py", "labels.json", "table.csv"],
        // Whether a name is a file of the package.
        "is_a_file": true,
        "is_a_file_nested": true,
        "is_not_a_file": false,
        "a_directory_is_not_a_file": false,
        // And the traversable says which of the two each one is.
        "directory_is_a_directory": true,
        "file_is_not_a_directory": false,
    });
    assert_eq!(
        document, expected,
        "case {case}: expected the package's own entries, got {document}"
    );
    1
}

/// A file the package does not hold reads as ABSENT, whichever call asks for it.
///
/// Answers how many cases a run answered.
async fn a_file_the_package_does_not_hold_reads_as_absent(
    router: &MultiServiceRouter,
    probe: &str,
) -> usize {
    let case = "a file the package does not hold";
    let Some(document) = answered(router, probe, case, &json!({"case": "absent"})).await else {
        return 0;
    };
    let expected = json!({
        "through_files": "FileNotFoundError",
        "by_name": "FileNotFoundError",
        "by_name_bytes": "FileNotFoundError",
        "nested": "FileNotFoundError",
        // The message names the path under the task root, which is where the file would be.
        "named_path": "/var/task/vendored/missing.pem",
    });
    assert_eq!(
        document, expected,
        "case {case}: expected the absence of the file rather than a failure of the library, \
         got {document}"
    );
    1
}

/// The probe every table drives: its handler reads the files of its own package back the
/// ways the standard library offers, and reports what each one answered.
const PROBE_SOURCE: &str = r#"
import importlib.resources as resources


def _refused(call):
    """The NAME of the error `call` raised, or its own value when it raised none."""
    try:
        return call()
    except BaseException as error:
        return type(error).__name__


def _message(call):
    """The MESSAGE of the error `call` raised, or an empty string when it raised none."""
    try:
        call()
    except BaseException as error:
        return str(error)
    return ""


def _held(event):
    vendored = resources.files("vendored")
    return {
        "through_files": vendored.joinpath("cacert.pem").read_text(),
        "through_files_nested": vendored.joinpath("data", "table.csv").read_text(),
        "by_name": resources.read_text("vendored", "cacert.pem"),
        "by_name_nested": resources.read_text("vendored.data", "table.csv"),
        "by_name_encoded": resources.read_text("vendored", "cacert.pem", encoding="ascii"),
        "by_name_bytes": resources.read_binary("vendored", "cacert.pem").decode("ascii"),
        "translated": resources.read_text("vendored.data", "labels.json"),
        "mis_encoded": _refused(
            lambda: resources.read_text("vendored.data", "labels.json", encoding="ascii")
        ),
        "beside_the_handler": resources.files("index").joinpath("notes.txt").read_text(),
        "beside_a_module": resources.files("vendored.reader")
        .joinpath("cacert.pem")
        .read_text(),
        "a_module_is_a_file": vendored.joinpath("reader.py").read_text(),
    }


def _beside(event):
    vendored = resources.files("vendored")
    return {
        "entries": sorted(entry.name for entry in vendored.iterdir()),
        "nested_entries": sorted(
            entry.name for entry in vendored.joinpath("data").iterdir()
        ),
        "is_a_file": vendored.joinpath("cacert.pem").is_file(),
        "is_a_file_nested": vendored.joinpath("data/table.csv").is_file(),
        "is_not_a_file": vendored.joinpath("missing.pem").is_file(),
        "a_directory_is_not_a_file": vendored.joinpath("data").is_file(),
        "directory_is_a_directory": vendored.joinpath("data").is_dir(),
        "file_is_not_a_directory": vendored.joinpath("cacert.pem").is_dir(),
    }


def _absent(event):
    vendored = resources.files("vendored")
    return {
        "through_files": _refused(lambda: vendored.joinpath("missing.pem").read_text()),
        "by_name": _refused(lambda: resources.read_text("vendored", "missing.pem")),
        "by_name_bytes": _refused(lambda: resources.read_binary("vendored", "missing.pem")),
        "nested": _refused(
            lambda: vendored.joinpath("data", "missing.csv").read_text()
        ),
        "named_path": _message(lambda: vendored.joinpath("missing.pem").read_text()),
    }


_CASES = {
    "held": _held,
    "beside": _beside,
    "absent": _absent,
}


def handler(event, context):
    return _CASES[event["case"]](event)
"#;
