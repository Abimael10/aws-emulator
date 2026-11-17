// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the IMPORT PATH a `python3.x` execution environment gives its
//! handler: `PublishLayerVersion` (`POST /2018-10-31/layers/{LayerName}/versions`) then
//! `CreateFunction` with those layers and `Invoke` (`POST
//! /2015-03-31/functions/{FunctionName}/invocations`) over the router.
//!
//! The rule under test: a module is resolved from the MOUNTS of the execution environment in
//! the order a managed runtime's own `sys.path` lists their directories — the function's own
//! deployment package at the task root first, then the import directories of the layer root
//! the function's layers are extracted to (the site-packages directory of the runtime's own
//! version, then the version-independent one), then the directories the runtime's own
//! libraries and bundled SDK live in — and each module reports the file it has under the
//! mount it was resolved from. So a layer that ships its own copy of a package the runtime
//! bundles shadows the runtime's copy, and only the function's own package shadows a layer's.
//!
//! Every row drives real HTTP requests through the router, and the rows differ along what the
//! rule generalises over: which directory of the layer root an entry sits in (a version's
//! site-packages, the version-independent directory, a site-packages of ANOTHER version, a
//! directory that is on no import path at all), whether the module is a package or a single
//! module, whether the function's own package ships a copy too, how many layers ship one,
//! and whether the name is one the runtime itself bundles.

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

/// The `Runtime` every function of this suite is configured with.
const RUNTIME: &str = "python3.12";

/// The layer directory a package of a `python3.x` runtime's OWN version is shipped in: the
/// site-packages directory under the layer's `python/lib/<runtime>` path.
const SITE_PACKAGES: &str = "python/lib/python3.12/site-packages";

/// The same directory for ANOTHER version of the runtime, which is on no import path of this
/// function at all.
const OTHER_SITE_PACKAGES: &str = "python/lib/python3.9/site-packages";

/// How long past a due instant this suite waits before calling. The transition is owed by
/// the CLOCK, so nothing here sleeps on the emulator's behalf.
const SLACK: Duration = Duration::from_millis(150);

/// The `Timeout` every function this suite creates carries, in seconds. Sized the way the
/// sibling suites that RUN handlers size theirs: it has to cover this test binary's own
/// scheduling, not the handler's work, which is immediate.
const FUNCTION_TIMEOUT_SECONDS: i32 = 25;

/// How many times a row re-drives its invocation while the answer reports one of this test
/// binary's own host bounds rather than a run.
const HOST_BOUND_ATTEMPTS: usize = 4;

/// The text of the file the layer's own package ships beside its module.
const TABLE: &str = "id,total\n1,42\n";

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

/// Publish one layer version holding `entries`, answering its version ARN.
async fn publish_layer(
    router: &MultiServiceRouter,
    name: &str,
    entries: &[(&str, &str)],
) -> String {
    let published = call(
        router,
        "POST",
        &format!("{LAYERS_URI}/{name}/versions"),
        &serde_json::to_vec(&json!({
            "Content": { "ZipFile": zip_file(entries) },
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

/// Create a `python3.12` function holding `entries` with the layers `layers` names, and wait
/// out the provisioning window the create documents.
async fn create_function(
    router: &MultiServiceRouter,
    name: &str,
    entries: &[(&str, &str)],
    layers: &[String],
) {
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
            "Code": { "ZipFile": zip_file(entries) },
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
        "case {case}: the handler catches each failed import itself, so the invocation \
         carries no function error, got {failure:?} with {}",
        answer.text()
    );
    Some(answer.json())
}

/// The whole rule, driven END TO END as one test: the tables below are real
/// `PublishLayerVersion`, `CreateFunction` and `Invoke` requests over the router.
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
async fn a_python_run_resolves_its_layers_before_the_runtimes_own_modules() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let mut read = a_layers_import_directories_are_on_the_path(&router).await;
    read += a_layer_shadows_the_runtimes_bundled_sdk(&router).await;
    read += the_runtimes_own_sdk_is_reported_under_its_own_directory(&router).await;
    read += the_last_layer_mounted_over_a_path_wins(&router).await;
    assert!(
        read > 0,
        "expected at least one case of the tables above to be answered by a run, got {read}: \
         every invocation reported a bound this whole test binary shares, so nothing of the \
         rule was read"
    );
}

/// The import DIRECTORIES of the layer root are on the handler's path and nothing else of
/// the layer is, each module reporting its own path under that root — and the function's own
/// package still shadows a layer's copy.
///
/// Answers how many cases a run answered.
async fn a_layers_import_directories_are_on_the_path(router: &MultiServiceRouter) -> usize {
    let layer = publish_layer(
        router,
        "import-roots",
        &[
            // The version-independent directory of the layer root.
            ("python/dep.py", "WHERE = 'the layer'\n"),
            // The site-packages directory of the runtime's OWN version, and one of another
            // version, which is on no path at all.
            (
                &format!("{SITE_PACKAGES}/sitedep.py"),
                "WHERE = 'the site-packages of this version'\n",
            ),
            (
                &format!("{OTHER_SITE_PACKAGES}/otherdep.py"),
                "WHERE = 'another version'\n",
            ),
            // An entry of the layer that is in no import directory of it.
            ("stray.py", "WHERE = 'no import directory'\n"),
            // A copy of a module the function's own package ships too.
            ("python/shadowed.py", "WHERE = 'the layer'\n"),
            // A package of the layer, with the file it ships beside its module.
            ("python/vendored/__init__.py", "NAME = 'vendored'\n"),
            ("python/vendored/table.csv", TABLE),
        ],
    )
    .await;
    let name = "probe-layer-import-roots";
    create_function(
        router,
        name,
        &[
            ("index.py", PROBE_SOURCE),
            ("shadowed.py", "WHERE = 'the package'\n"),
        ],
        &[layer],
    )
    .await;
    let case = "the layer root's import directories";
    let Some(document) = answered(router, name, case, &json!({"case": "roots"})).await else {
        return 0;
    };
    let expected = json!({
        // A module of the version-independent directory and one of this version's
        // site-packages are both importable, and each names its own path under the layer
        // root.
        "dep": "the layer",
        "dep_file": "/opt/python/dep.py",
        "sitedep": "the site-packages of this version",
        "sitedep_file": format!("/opt/{SITE_PACKAGES}/sitedep.py"),
        // A site-packages directory of another version, and an entry in no import directory
        // of the layer at all, are on no path: neither is importable.
        "otherdep": "ModuleNotFoundError",
        "stray": "ModuleNotFoundError",
        // The function's own package precedes the layer root, so its copy wins.
        "shadowed": "the package",
        "shadowed_file": "/var/task/shadowed.py",
        // A package of a layer is a package: its module imports and the file it ships
        // beside it reads back, named under the layer root.
        "vendored": "vendored",
        "vendored_table": TABLE,
        "vendored_missing": "/opt/python/vendored/missing.csv",
    });
    assert_eq!(
        document, expected,
        "case {case}: expected every mount resolved in the order a runtime's own path lists \
         them, got {document}"
    );
    1
}

/// A layer that ships its own copy of the SDK the runtime bundles shadows the bundled one:
/// the handler's `import boto3` is the layer's module, at the layer's own path, with what the
/// layer's package defines and without what the runtime's own copy carries.
///
/// Answers how many cases a run answered.
async fn a_layer_shadows_the_runtimes_bundled_sdk(router: &MultiServiceRouter) -> usize {
    let layer = publish_layer(
        router,
        "pinned-sdk",
        &[(
            "python/boto3/__init__.py",
            "MARKER = 'from the layer'\n__version__ = '1.40.0'\n",
        )],
    )
    .await;
    let name = "probe-layer-sdk";
    create_function(router, name, &[("index.py", PROBE_SOURCE)], &[layer]).await;
    let case = "a layer shipping its own SDK";
    let Some(document) = answered(router, name, case, &json!({"case": "sdk"})).await else {
        return 0;
    };
    let expected = json!({
        "file": "/opt/python/boto3/__init__.py",
        "marker": "from the layer",
        "version": "1.40.0",
        // The layer's copy is the WHOLE module: nothing of the runtime's bundled one is
        // merged into it, so what that copy does not define is not there.
        "has_client": false,
    });
    assert_eq!(
        document, expected,
        "case {case}: expected the layer's own package, got {document}"
    );
    1
}

/// With no layer shipping one, `import boto3` is the SDK the runtime bundles — reported in
/// the runtime's OWN directory rather than in the function's package.
///
/// Answers how many cases a run answered.
async fn the_runtimes_own_sdk_is_reported_under_its_own_directory(
    router: &MultiServiceRouter,
) -> usize {
    let name = "probe-bundled-sdk";
    create_function(router, name, &[("index.py", PROBE_SOURCE)], &[]).await;
    let case = "the runtime's bundled SDK";
    let Some(document) = answered(router, name, case, &json!({"case": "sdk"})).await else {
        return 0;
    };
    let actual = document.get("file").cloned().unwrap_or(Value::Null);
    assert_eq!(
        actual,
        json!("/var/runtime/boto3/__init__.py"),
        "case {case}: expected the bundled SDK under the runtime's own directory, got \
         {document}"
    );
    let client = document.get("has_client").cloned().unwrap_or(Value::Null);
    assert_eq!(
        client,
        json!(true),
        "case {case}: expected the bundled SDK's client interface, got {document}"
    );
    1
}

/// Layers are mounted over one another at the same root in the order the function lists
/// them, so the LAST layer shipping a path is the one whose module is imported.
///
/// Answers how many cases a run answered.
async fn the_last_layer_mounted_over_a_path_wins(router: &MultiServiceRouter) -> usize {
    let first = publish_layer(
        router,
        "mounted-first",
        &[("python/dep.py", "WHERE = 'the first layer'\n")],
    )
    .await;
    let second = publish_layer(
        router,
        "mounted-second",
        &[("python/dep.py", "WHERE = 'the second layer'\n")],
    )
    .await;
    let name = "probe-layer-order";
    create_function(
        router,
        name,
        &[("index.py", PROBE_SOURCE)],
        &[first, second],
    )
    .await;
    let case = "two layers shipping one path";
    let Some(document) = answered(router, name, case, &json!({"case": "order"})).await else {
        return 0;
    };
    let expected = json!({
        "dep": "the second layer",
        "dep_file": "/opt/python/dep.py",
    });
    assert_eq!(
        document, expected,
        "case {case}: expected the layer mounted last, got {document}"
    );
    1
}

/// The probe every table drives: its handler imports each name the case asks about and
/// reports what it resolved to, so a failed import is a value of the answer rather than a
/// failed invocation.
const PROBE_SOURCE: &str = r#"
import importlib


def _refused(call):
    """What `call` answered, or the NAME of the error it raised."""
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


def _attribute(name, attribute):
    """The attribute `attribute` of the module `name`, importing it first."""
    return getattr(importlib.import_module(name), attribute)


def _roots(event):
    import importlib.resources as resources

    return {
        "dep": _refused(lambda: _attribute("dep", "WHERE")),
        "dep_file": _refused(lambda: _attribute("dep", "__file__")),
        "sitedep": _refused(lambda: _attribute("sitedep", "WHERE")),
        "sitedep_file": _refused(lambda: _attribute("sitedep", "__file__")),
        "otherdep": _refused(lambda: _attribute("otherdep", "WHERE")),
        "stray": _refused(lambda: _attribute("stray", "WHERE")),
        "shadowed": _refused(lambda: _attribute("shadowed", "WHERE")),
        "shadowed_file": _refused(lambda: _attribute("shadowed", "__file__")),
        "vendored": _refused(lambda: _attribute("vendored", "NAME")),
        "vendored_table": _refused(
            lambda: resources.files("vendored").joinpath("table.csv").read_text()
        ),
        "vendored_missing": _message(
            lambda: resources.files("vendored").joinpath("missing.csv").read_text()
        ),
    }


def _sdk(event):
    import boto3

    return {
        "file": boto3.__file__,
        "marker": getattr(boto3, "MARKER", None),
        "version": getattr(boto3, "__version__", None),
        "has_client": hasattr(boto3, "client"),
    }


def _order(event):
    return {
        "dep": _refused(lambda: _attribute("dep", "WHERE")),
        "dep_file": _refused(lambda: _attribute("dep", "__file__")),
    }


_CASES = {
    "roots": _roots,
    "sdk": _sdk,
    "order": _order,
}


def handler(event, context):
    return _CASES[event["case"]](event)
"#;
