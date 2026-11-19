// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for HOW a path names a file of the execution environment, and for the
//! DIRECTORY surface over the same mounts: `PublishLayerVersion` (`POST
//! /2018-10-31/layers/{LayerName}/versions`) then `CreateFunction` and `Invoke` (`POST
//! /2015-03-31/functions/{FunctionName}/invocations`) over the router.
//!
//! The rule under test: one file of a mount is one file however the path reaches it — spelled
//! out from the task root the environment reports, relative to the directory the run is in,
//! walking back through a directory of the package, or handed over as a path object — and the
//! directory calls the standard library walks a tree with report exactly the entries those
//! mounts hold, in the sentences a filesystem words a refusal with.
//!
//! What each entry of a mount IS — its size, its kind, its access bits, which refusal each
//! condition earns, the import directories of the layer root, and that a path on no mount is
//! reachable in no way at all — is the sibling suite's table
//! ([`crate::python_layer_filesystem`]); this one is about the PATH and the walk.
//!
//! Every row drives real HTTP requests through the router, and the rows differ along what the
//! rule generalises over: how the path names the file (four spellings), which mount holds it
//! (the deployment package at the task root, a layer's content under the layer root), how deep
//! it is, whether it is read as text or as bytes, which call walks the directory
//! (`os.listdir`, `os.scandir`, `os.walk`), and which condition a refusal is worded for.
//!
//! The suite RUNS a handler, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

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

/// Where `PublishLayerVersion` is bound.
const LAYERS_URI: &str = "/2018-10-31/layers";

/// A SigV4 credential scope naming this service's signing name (`lambda`) in `us-east-1`.
/// Nothing in it is verified: the harness turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=emulator-test-key/20260101/us-east-1/lambda/aws4_request, \
     SignedHeaders=host, Signature=not-verified";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The `Runtime` the function of this suite is configured with.
const RUNTIME: &str = "python3.12";

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

/// The text of the file the package ships beside its handler.
const NOTES: &str = "beside the handler\n";

/// The text of the file one directory deeper.
const TABLE: &str = "id,total\n1,42\n";

/// The text of the file the LAYER ships under the layer root's version-independent import
/// directory.
const LAYER_NOTES: &str = "mounted under the layer root\n";

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

/// The base64 an archive holding `entries` travels as, which is what a `ZipFile` member
/// carries.
fn zip_file(entries: &[(&str, &str)]) -> String {
    base64::engine::general_purpose::STANDARD.encode(package(entries))
}

/// The entries the probe's own deployment package holds: its handler, the file beside it, one
/// directory deeper, and a vendored library of its own.
fn probe_entries() -> Vec<(&'static str, &'static str)> {
    vec![
        ("index.py", PROBE_SOURCE),
        ("notes.txt", NOTES),
        ("data/table.csv", TABLE),
        ("vendored/__init__.py", ""),
        ("vendored/reader.py", "MARKER = 'read'\n"),
    ]
}

/// Publish the layer whose content the probe reads out of the layer root, answering its
/// version ARN.
async fn publish_layer(router: &MultiServiceRouter, name: &str) -> String {
    let published = call(
        router,
        "POST",
        &format!("{LAYERS_URI}/{name}/versions"),
        &serde_json::to_vec(&json!({
            "Content": {
                "ZipFile": zip_file(&[
                    ("python/layer_notes.txt", LAYER_NOTES),
                    ("python/layer_mod.py", "WHERE = 'the layer'\n"),
                ]),
            },
            "CompatibleRuntimes": [RUNTIME],
        }))
        .expect("the publish body serializes"),
    )
    .await;
    assert_eq!(
        published.status,
        201,
        "PublishLayerVersion {name:?} must succeed, got {} {}",
        published.status,
        published.text()
    );
    published.json()["LayerVersionArn"]
        .as_str()
        .expect("the published version's ARN")
        .to_string()
}

/// Create the probe function with the layer attached, and wait out the provisioning window
/// the create documents.
async fn create_function(router: &MultiServiceRouter, name: &str, layers: &[String]) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": RUNTIME,
            "Handler": "index.handler",
            "Timeout": FUNCTION_TIMEOUT_SECONDS,
            "MemorySize": 256,
            "Layers": layers,
            "Code": { "ZipFile": zip_file(&probe_entries()) },
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
    let mut answer = invoke_once(router, name, event).await;
    for _ in 1..HOST_BOUND_ATTEMPTS {
        if !is_host_bound(answer.status, &answer.text()) {
            break;
        }
        answer = invoke_once(router, name, event).await;
    }
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
        "case {case}: the handler reads its own files and catches each refusal itself, so the \
         invocation carries no function error, got {failure:?} with {}",
        answer.text()
    );
    Some(answer.json())
}

/// The whole rule, driven END TO END as one test: the tables below are a real
/// `PublishLayerVersion`, a real `CreateFunction` and real `Invoke` requests over the router.
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
async fn one_file_of_a_mount_is_one_file_however_the_path_reaches_it() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let probe = "probe-environment-paths";
    let layer = publish_layer(&router, "environment-paths").await;
    create_function(&router, probe, &[layer]).await;
    let mut read = every_spelling_of_a_path_names_the_same_file(&router, probe).await;
    read += the_directories_of_a_mount_hold_what_the_archive_carried(&router, probe).await;
    read += a_refusal_of_a_mount_is_worded_as_a_filesystem_words_it(&router, probe).await;
    assert!(
        read > 0,
        "expected at least one case of the tables above to be answered by a run, got {read}: \
         every invocation reported a bound this whole test binary shares, so nothing of the \
         rule was read"
    );
}

/// Every spelling of a path names the same file of the same mount, and the bytes are the ones
/// the archive carried.
///
/// Answers how many cases a run answered.
async fn every_spelling_of_a_path_names_the_same_file(
    router: &MultiServiceRouter,
    probe: &str,
) -> usize {
    let case = "every spelling of a path";
    let Some(document) = answered(router, probe, case, &json!({"case": "read"})).await else {
        return 0;
    };
    let expected = json!({
        // The handler's OWN module source, read as bytes out of the task root the environment
        // reports — which is what a check that two deployments of the same bytes are the same
        // deployment reads.
        "own_source": PROBE_SOURCE,
        "own_source_bytes": PROBE_SOURCE.len(),
        // One file of the package, named four ways: spelled out from the task root, relative
        // to the directory the run is in, walking back out of a directory of the package, and
        // handed over as a path object.
        "spelled_out": NOTES,
        "relative_to_the_run": NOTES,
        "walked_through_a_directory": NOTES,
        "as_a_path_object": NOTES,
        // The files of the two mounts: one directory deeper into the package, a module of it,
        // and one under the layer root the function's layer is extracted to.
        "one_directory_deeper": TABLE,
        "a_module_is_a_file_too": "MARKER = 'read'\n",
        "under_the_layer_root": LAYER_NOTES,
    });
    assert_eq!(
        document, expected,
        "case {case}: expected the bytes the archive carried, got {document}"
    );
    1
}

/// The DIRECTORIES of a mount hold exactly the entries the archive carried, whichever call
/// walks them.
///
/// Answers how many cases a run answered.
async fn the_directories_of_a_mount_hold_what_the_archive_carried(
    router: &MultiServiceRouter,
    probe: &str,
) -> usize {
    let case = "the directories of a mount";
    let Some(document) = answered(router, probe, case, &json!({"case": "walk"})).await else {
        return 0;
    };
    let expected = json!({
        // One level of each directory: the task root, named and as the directory the run is
        // in, a directory of the package, and the layer root's own import directory.
        "listed": ["data", "index.py", "notes.txt", "vendored"],
        "listed_without_a_path": ["data", "index.py", "notes.txt", "vendored"],
        "listed_one_directory_deeper": ["table.csv"],
        "listed_under_the_layer_root": ["layer_mod.py", "layer_notes.txt"],
        // A scan of a directory says which of the two each entry is...
        "scanned": ["data:dir", "index.py:file", "notes.txt:file", "vendored:dir"],
        // ... and a WALK of the mount reaches every file of it, which is the same tree.
        "walked": [
            "data/table.csv",
            "index.py",
            "notes.txt",
            "vendored/__init__.py",
            "vendored/reader.py",
        ],
    });
    assert_eq!(
        document, expected,
        "case {case}: expected the mount's own entries, got {document}"
    );
    1
}

/// A refusal of a mount is WORDED the way a filesystem words it: the path the entry would be
/// at, under the error number the condition carries.
///
/// Which refusal each condition earns is the sibling suite's table; what is read here is the
/// SENTENCE, which is what a handler's own error handling prints.
///
/// Answers how many cases a run answered.
async fn a_refusal_of_a_mount_is_worded_as_a_filesystem_words_it(
    router: &MultiServiceRouter,
    probe: &str,
) -> usize {
    let case = "the wording of a refusal";
    let Some(document) = answered(router, probe, case, &json!({"case": "refused"})).await else {
        return 0;
    };
    let expected = json!({
        // The path a file the package does not hold would be at, under the task root.
        "a_file_the_package_does_not_hold":
            "[Errno 2] No such file or directory: '/var/task/nothing.txt'",
        // ... and the path a write names, which the mount being read-only refuses.
        "written": "[Errno 30] Read-only file system: '/var/task/written.txt'",
    });
    assert_eq!(
        document, expected,
        "case {case}: expected the sentence a filesystem words, got {document}"
    );
    1
}

/// The probe every table drives: its handler reads the files of its own execution environment
/// the ways a Python program reads one, and reports what each one answered.
const PROBE_SOURCE: &str = r#"
import os
import pathlib

TASK_ROOT = os.environ["LAMBDA_TASK_ROOT"]
LAYER_ROOT = "/opt"


def _message(call):
    """The MESSAGE of the error `call` raised, or an empty string when it raised none."""
    try:
        call()
    except BaseException as error:
        return str(error)
    return ""


def _read(event):
    with open(os.path.join(TASK_ROOT, "index.py"), "rb") as reading:
        source = reading.read()
    with open(os.path.join(TASK_ROOT, "notes.txt")) as reading:
        spelled_out = reading.read()
    with open("notes.txt") as reading:
        relative = reading.read()
    with open("./data/../notes.txt") as reading:
        walked = reading.read()
    with open(pathlib.Path(TASK_ROOT) / "notes.txt") as reading:
        as_a_path = reading.read()
    with open(os.path.join(TASK_ROOT, "data", "table.csv")) as reading:
        deeper = reading.read()
    with open(os.path.join(TASK_ROOT, "vendored", "reader.py")) as reading:
        module = reading.read()
    with open(os.path.join(LAYER_ROOT, "python", "layer_notes.txt")) as reading:
        layered = reading.read()
    return {
        "own_source": source.decode("utf-8"),
        "own_source_bytes": len(source),
        "spelled_out": spelled_out,
        "relative_to_the_run": relative,
        "walked_through_a_directory": walked,
        "as_a_path_object": as_a_path,
        "one_directory_deeper": deeper,
        "a_module_is_a_file_too": module,
        "under_the_layer_root": layered,
    }


def _walk(event):
    return {
        "listed": sorted(os.listdir(TASK_ROOT)),
        "listed_without_a_path": sorted(os.listdir()),
        "listed_one_directory_deeper": sorted(os.listdir(os.path.join(TASK_ROOT, "data"))),
        "listed_under_the_layer_root": sorted(
            os.listdir(os.path.join(LAYER_ROOT, "python"))
        ),
        "scanned": sorted(
            "%s:%s" % (entry.name, "dir" if entry.is_dir() else "file")
            for entry in os.scandir(TASK_ROOT)
        ),
        "walked": sorted(
            os.path.relpath(os.path.join(base, name), TASK_ROOT)
            for base, directories, files in os.walk(TASK_ROOT)
            for name in files
        ),
    }


def _refusals(event):
    nothing = os.path.join(TASK_ROOT, "nothing.txt")
    written = os.path.join(TASK_ROOT, "written.txt")
    return {
        "a_file_the_package_does_not_hold": _message(lambda: open(nothing)),
        "written": _message(lambda: open(written, "w")),
    }


_CASES = {
    "read": _read,
    "walk": _walk,
    "refused": _refusals,
}


def handler(event, context):
    return _CASES[event["case"]](event)
"#;
