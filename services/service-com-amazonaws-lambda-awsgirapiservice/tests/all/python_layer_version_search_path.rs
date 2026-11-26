// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH of an attached layer's directories a `python3.x` execution
//! environment puts on the handler's search path, and for what the rest of that layer still
//! leaves on disk: `PublishLayerVersion` (`POST /2018-10-31/layers/{LayerName}/versions`) then
//! `CreateFunction` with that layer version and `Invoke` (`POST
//! /2015-03-31/functions/{FunctionName}/invocations`) over the router.
//!
//! The rule under test has two halves, and one layer version drives both:
//!
//! * the handler's own `sys.path` LISTS the layer root's import directories, the site-packages
//!   directory named for the major and minor version of the runtime the function is configured
//!   with first and the version-independent layer root after it, so a module one layer ships in
//!   both of those directories resolves to the site-packages copy;
//! * the layer's archive is EXTRACTED in full under the layer root whatever directory an entry
//!   names, so an entry in a directory that is on no search path of this runtime is still a
//!   file of the environment and only the search path decides whether it imports at all — a
//!   name no entry of the path resolves raises the interpreter's own `ModuleNotFoundError`,
//!   reported as an unhandled function error when the handler does not catch it.
//!
//! Every row drives real HTTP requests through the router, and the rows differ along what the
//! rule generalises over: WHICH minor version the function's runtime names (four of them, one
//! per row, the recorded `python3.12` being one), which of the layer's site-packages
//! directories an entry sits in relative to that version (its own, each of the other three),
//! whether the entry is in the version-independent import directory, in a directory of the
//! layer root that is on no search path at all, or is no module's source to begin with, and
//! whether the handler catches the failed import or lets it end the invocation.

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

/// The directory an attached layer version's content is extracted into.
const LAYER_ROOT: &str = "/opt";

/// The `Runtime` identifiers this suite drives a row for: four supported releases of the
/// language, so the version the search path is named for is what varies between rows rather
/// than being one recorded value.
///
/// Source: the model's `Runtime` enum (`services/.../shared_types.rs`'s `RUNTIME_VALUES`), less
/// the releases the service reports as deprecated (`state::DEPRECATED_RUNTIMES`).
const RUNTIMES: &[&str] = &["python3.10", "python3.11", "python3.12", "python3.13"];

/// The module BOTH import directories of the layer carry a copy of, which is what the order of
/// those directories on the search path decides between.
const SHADOWED_MODULE: &str = "shadowed_dep";

/// The module the version-independent import directory of the layer root carries.
const ROOT_MODULE: &str = "root_dep";

/// What that module reports, so a reading names the directory it was resolved from.
const ROOT_MARKER: &str = "the layer root";

/// A module the layer ships in a directory of the layer root that is on NO search path of any
/// runtime: extracted like every other entry, importable through nothing.
const PAYLOAD_MODULE: &str = "payload_dep";

/// Where that module sits inside the archive.
const PAYLOAD_PATH: &str = "assets/payload_dep.py";

/// A file of the layer that is no module's source at all, in the same directory: a payload read
/// by absolute path, which is what a layer shipping a certificate bundle is made of.
const BUNDLE_PATH: &str = "assets/bundle.pem";

/// What that file holds.
const BUNDLE: &str = "-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n";

/// A path under the layer root that no entry of the archive carries, so nothing is there.
const ABSENT_PATH: &str = "/opt/assets/absent.pem";

/// How long past a due instant this suite waits before calling. The transition is owed by the
/// CLOCK, so nothing here sleeps on the emulator's behalf.
const SLACK: Duration = Duration::from_millis(150);

/// The `Timeout` every function this suite creates carries, in seconds. Sized the way the
/// sibling suites that RUN handlers size theirs: it has to cover this test binary's own
/// scheduling, not the handler's work, which is immediate.
const FUNCTION_TIMEOUT_SECONDS: i32 = 25;

/// How many times a row re-drives its invocation while the answer reports one of this test
/// binary's own host bounds rather than a run.
const HOST_BOUND_ATTEMPTS: usize = 6;

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

/// The part of a module name a runtime identifier contributes: its version with the separator
/// written the way a Python identifier may carry it (`python3.11` is `3_11`).
fn version_slug(runtime: &str) -> String {
    runtime.trim_start_matches("python").replace('.', "_")
}

/// The module this suite ships in the site-packages directory of `runtime`'s own version, and
/// in no other directory: a name that imports for exactly one of the rows.
fn site_module(runtime: &str) -> String {
    format!("site_dep_py{}", version_slug(runtime))
}

/// The directory of the layer root a dependency built for `runtime` installs into, relative to
/// the archive's own root.
fn site_directory(runtime: &str) -> String {
    format!("python/lib/{runtime}/site-packages")
}

/// What a module of `runtime`'s site-packages directory reports, so a reading names the
/// directory it was resolved from.
fn site_marker(runtime: &str) -> String {
    format!("the site directory of {runtime}")
}

/// The entries of the ONE layer version every row attaches: a module of each runtime's own
/// site-packages directory, a copy of one module in every site-packages directory and in the
/// version-independent import directory beside them, and two files of a directory of the layer
/// root that is on no search path at all.
fn layer_entries() -> Vec<(String, String)> {
    let mut entries = Vec::new();
    for runtime in RUNTIMES {
        let directory = site_directory(runtime);
        entries.push((
            format!("{directory}/{}.py", site_module(runtime)),
            format!("WHERE = '{}'\n", site_marker(runtime)),
        ));
        entries.push((
            format!("{directory}/{SHADOWED_MODULE}.py"),
            format!("WHERE = '{}'\n", site_marker(runtime)),
        ));
    }
    entries.push((
        format!("python/{ROOT_MODULE}.py"),
        format!("WHERE = '{ROOT_MARKER}'\n"),
    ));
    entries.push((
        format!("python/{SHADOWED_MODULE}.py"),
        format!("WHERE = '{ROOT_MARKER}'\n"),
    ));
    entries.push((
        PAYLOAD_PATH.to_string(),
        "WHERE = 'no import directory'\n".to_string(),
    ));
    entries.push((BUNDLE_PATH.to_string(), BUNDLE.to_string()));
    entries
}

/// Publish one layer version holding `entries`, answering its version ARN.
async fn publish_layer(
    router: &MultiServiceRouter,
    name: &str,
    entries: &[(String, String)],
) -> String {
    let borrowed: Vec<(&str, &str)> = entries
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect();
    let published = call(
        router,
        "POST",
        &format!("{LAYERS_URI}/{name}/versions"),
        &serde_json::to_vec(&json!({
            "Content": { "ZipFile": zip_file(&borrowed) },
            // One published version serves every row, so it names each runtime the rows drive.
            "CompatibleRuntimes": RUNTIMES,
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

/// Create a function configured with `runtime` and attaching `layers`, and wait out the
/// provisioning window the create documents.
async fn create_function(
    router: &MultiServiceRouter,
    name: &str,
    runtime: &str,
    layers: &[String],
) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": runtime,
            "Handler": "index.handler",
            "Timeout": FUNCTION_TIMEOUT_SECONDS,
            "MemorySize": 256,
            "Layers": layers,
            "Code": { "ZipFile": zip_file(&[("index.py", PROBE_SOURCE)]) },
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

/// One `Invoke` request re-driven while the answer reports a bound this whole test binary
/// shares rather than a run, or `None` when every attempt did.
async fn invoked(router: &MultiServiceRouter, name: &str, event: &Value) -> Option<Answer> {
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
    Some(answer)
}

/// What one case answered, or `None` when the run was ended by a bound this whole test binary
/// shares rather than by anything the case asked for.
async fn answered(
    router: &MultiServiceRouter,
    name: &str,
    case: &str,
    event: &Value,
) -> Option<Value> {
    let answer = invoked(router, name, event).await?;
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
        "case {case}: the handler reports each failed import itself, so the invocation carries \
         no function error, got {failure:?} with {}",
        answer.text()
    );
    Some(answer.json())
}

/// The whole rule, driven END TO END as one test: the table below is real
/// `PublishLayerVersion`, `CreateFunction` and `Invoke` requests over the router.
///
/// One test and one stream of invocations, the way the sibling runtime suites are built: the
/// memory bound a run is stopped on is read from the WHOLE process
/// (`state::execution::python`) and this binary runs its suites in parallel, so fewer
/// interpreters in flight at once is fewer runs stopped for memory belonging to another suite.
/// A case whose run was stopped by one of those shared bounds says nothing about the rule and
/// is left out of the reading — but not every one can be: the tables answer how many of their
/// cases a RUN answered, and a test that read none at all is a test that asserted nothing, so
/// it fails rather than passing silently.
#[tokio::test]
async fn a_python_runs_search_path_names_its_own_version_and_the_layer_is_extracted_whole() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let entries = layer_entries();
    let layer = publish_layer(&router, "search-path-by-version", &entries).await;

    let mut read = 0_usize;
    for runtime in RUNTIMES {
        read += the_search_path_is_named_for_the_functions_own_version(&router, &layer, runtime)
            .await;
    }
    read += an_entry_on_no_search_path_raises_the_interpreters_own_refusal(&router, &layer).await;
    assert!(
        read > 0,
        "expected at least one case of the tables above to be answered by a run, got {read}: \
         every invocation reported a bound this whole test binary shares, so nothing of the \
         rule was read"
    );
}

/// One row of the table: a function configured with `runtime` reads the layer's site-packages
/// directory for THAT version first on its search path, then the version-independent one — so
/// the shadowed module resolves to the site-packages copy, the module of every other version's
/// site-packages directory is extracted and not importable, and so is a module of a directory
/// of the layer root that is on no search path at all.
///
/// Answers how many cases a run answered.
async fn the_search_path_is_named_for_the_functions_own_version(
    router: &MultiServiceRouter,
    layer: &str,
    runtime: &str,
) -> usize {
    let name = format!("probe-search-path-{}", version_slug(runtime));
    let layers = vec![layer.to_string()];
    create_function(router, &name, runtime, &layers).await;

    // Every module name the layer ships one copy of under some version's site-packages
    // directory, and the two names of the directories that are on no version's path.
    let mut names: Vec<String> = RUNTIMES.iter().map(|named| site_module(named)).collect();
    names.push(ROOT_MODULE.to_string());
    names.push(PAYLOAD_MODULE.to_string());

    // Every path the archive carries, plus one it does not: what the layer root holds is the
    // archive's entries, whichever directory each of them names.
    let mut paths: Vec<String> = Vec::new();
    for named in RUNTIMES {
        let directory = site_directory(named);
        paths.push(format!("{LAYER_ROOT}/{directory}/{}.py", site_module(named)));
        paths.push(format!("{LAYER_ROOT}/{directory}/{SHADOWED_MODULE}.py"));
    }
    paths.push(format!("{LAYER_ROOT}/python/{ROOT_MODULE}.py"));
    paths.push(format!("{LAYER_ROOT}/{PAYLOAD_PATH}"));
    paths.push(format!("{LAYER_ROOT}/{BUNDLE_PATH}"));
    paths.push(ABSENT_PATH.to_string());

    let case = format!("a function configured with {runtime}");
    let event = json!({
        "shadowed": SHADOWED_MODULE,
        "modules": names,
        "paths": paths,
    });
    let Some(document) = answered(router, &name, &case, &event).await else {
        return 0;
    };

    // The import directories of the layer root, in the order a managed runtime lists them: the
    // site-packages directory named for THIS function's version, then the version-independent
    // one beside it.
    let expected_path = json!([
        format!("{LAYER_ROOT}/{}", site_directory(runtime)),
        format!("{LAYER_ROOT}/python"),
    ]);
    let opt_path = document["opt_path"].as_array().cloned().unwrap_or_default();
    let leading = Value::Array(opt_path.iter().take(2).cloned().collect());
    assert_eq!(
        leading, expected_path,
        "case {case}: expected the layer root's import directories to open the search path as \
         {expected_path}, got {}",
        document["opt_path"]
    );
    // And no other version's site-packages directory is on it at all.
    for named in RUNTIMES.iter().filter(|named| **named != runtime) {
        let foreign = Value::String(format!("{LAYER_ROOT}/{}", site_directory(named)));
        assert!(
            !opt_path.contains(&foreign),
            "case {case}: expected {foreign} to be on no search path of this function, got {}",
            document["opt_path"]
        );
    }

    // Because the site-packages directory precedes the version-independent one, the copy it
    // holds is the one the shadowed name resolves to.
    let marker = site_marker(runtime);
    assert_eq!(
        document["shadowed"],
        json!(marker),
        "case {case}: expected the site-packages copy of {SHADOWED_MODULE} to win, got {}",
        document["shadowed"]
    );
    let shadowed_file = format!(
        "{LAYER_ROOT}/{}/{SHADOWED_MODULE}.py",
        site_directory(runtime)
    );
    assert_eq!(
        document["shadowed_file"],
        json!(shadowed_file),
        "case {case}: expected the shadowed module's file to be {shadowed_file}, got {}",
        document["shadowed_file"]
    );

    // What each name imports to: the module of this version's site-packages directory and the
    // one of the version-independent directory resolve; the module of every OTHER version's
    // site-packages directory, and the one in a directory on no path, raise the interpreter's
    // own refusal, worded with the name that was asked for.
    for named in RUNTIMES {
        let module = site_module(named);
        let expected = if *named == runtime {
            site_marker(named)
        } else {
            format!("ModuleNotFoundError: No module named '{module}'")
        };
        let actual = document["imports"].get(&module);
        assert_eq!(
            actual,
            Some(&json!(expected)),
            "case {case}: expected importing {module} to come to {expected:?}, got {actual:?}"
        );
    }
    let read: Vec<(String, String)> = vec![
        (ROOT_MODULE.to_string(), ROOT_MARKER.to_string()),
        (
            PAYLOAD_MODULE.to_string(),
            format!("ModuleNotFoundError: No module named '{PAYLOAD_MODULE}'"),
        ),
    ];
    for (module, expected) in &read {
        let actual = document["imports"].get(module);
        assert_eq!(
            actual,
            Some(&json!(expected)),
            "case {case}: expected importing {module} to come to {expected:?}, got {actual:?}"
        );
    }

    // And every entry the archive carries is a file of the layer root whatever directory it
    // names — including the three the search path never reaches — while a path the archive does
    // not carry is nothing.
    for path in &paths {
        let expected = path != ABSENT_PATH;
        let actual = document["on_disk"].get(path);
        assert_eq!(
            actual,
            Some(&json!(expected)),
            "case {case}: expected {path} on disk to be {expected}, got {actual:?}"
        );
    }
    1
}

/// A name no entry of the search path resolves, imported WITHOUT a guard: the invocation is
/// answered as an unhandled function error carrying the interpreter's own class and message,
/// which is what the layer's entry being extracted and not importable comes to for a caller.
///
/// Answers how many cases a run answered.
async fn an_entry_on_no_search_path_raises_the_interpreters_own_refusal(
    router: &MultiServiceRouter,
    layer: &str,
) -> usize {
    // A runtime of the table, and the site-packages module of ANOTHER of its versions: the file
    // is in the layer root and on no path of this function.
    let runtime = RUNTIMES[RUNTIMES.len() - 1];
    let foreign = site_module(RUNTIMES[0]);
    let name = "probe-search-path-unhandled";
    let layers = vec![layer.to_string()];
    create_function(router, name, runtime, &layers).await;
    let case = format!("{foreign} imported by a {runtime} function without a guard");
    let Some(answer) = invoked(router, name, &json!({"raises": foreign})).await else {
        return 0;
    };
    assert_eq!(
        answer.status,
        200,
        "case {case}: a handler that raised is still a 200, got {} {}",
        answer.status,
        answer.text()
    );
    assert_eq!(
        answer.header("x-amz-function-error").as_deref(),
        Some("Unhandled"),
        "case {case}: expected the unhandled signal, got {:?}",
        answer.header("x-amz-function-error")
    );
    let document = answer.json();
    assert_eq!(
        document["errorType"],
        json!("ModuleNotFoundError"),
        "case {case}: expected the interpreter's own class, got {document}"
    );
    assert_eq!(
        document["errorMessage"],
        json!(format!("No module named '{foreign}'")),
        "case {case}: expected the interpreter's own message, got {document}"
    );
    1
}

/// The probe every row drives: its handler reports the layer root's entries on `sys.path`, what
/// each name it was asked about imports to and which of the paths it was asked about is a file,
/// so a failed import is a value of the answer rather than a failed invocation — except for the
/// one event that asks for an import with no guard at all.
const PROBE_SOURCE: &str = r#"
import importlib
import os
import sys


_LAYER_ROOT = "/opt"


def _read(call):
    """What `call` answered, or the class and message of the error it raised."""
    try:
        return call()
    except BaseException as error:
        return "%s: %s" % (type(error).__name__, error)


def _attribute(name, attribute):
    """The attribute `attribute` of the module `name`, importing it first."""
    return getattr(importlib.import_module(name), attribute)


def _probe(event):
    return {
        "opt_path": [
            entry for entry in sys.path if entry.startswith(_LAYER_ROOT)
        ],
        "shadowed": _read(lambda: _attribute(event["shadowed"], "WHERE")),
        "shadowed_file": _read(lambda: _attribute(event["shadowed"], "__file__")),
        "imports": {
            name: _read(lambda name=name: _attribute(name, "WHERE"))
            for name in event["modules"]
        },
        "on_disk": {path: os.path.isfile(path) for path in event["paths"]},
    }


def handler(event, context):
    if "raises" in event:
        # No guard: what the import machinery raises is what the invocation reports.
        return _attribute(event["raises"], "WHERE")
    return _probe(event)
"#;
