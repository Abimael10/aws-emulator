// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the FILESYSTEM a `python3.x` execution environment gives its handler:
//! `PublishLayerVersion` (`POST /2018-10-31/layers/{LayerName}/versions`) then
//! `CreateFunction` with those layers and `Invoke` (`POST
//! /2015-03-31/functions/{FunctionName}/invocations`) over the router.
//!
//! The rule under test: every entry of a mount of the execution environment — the deployment
//! package at the task root, each attached layer's content under the layer root — is an
//! ordinary READ-ONLY file at its own path under that mount, so a filesystem call names it,
//! reads it, lists the directory holding it and refuses to write it; the directories of those
//! mounts are what the handler's `sys.path` lists, in the order a managed runtime lists them,
//! with a path-configuration file of a layer's import directory extending that path; and a path
//! on no mount of the function is reachable in no way at all, because the environment has no
//! host filesystem behind it.
//!
//! Every row drives real HTTP requests through the router, and the rows differ along what the
//! rule generalises over: which mount an entry is on (the function's own package, one layer,
//! another layer), which directory of the layer root it is in (an import directory, a site
//! directory of this runtime's version, a site directory of another version, a directory on no
//! import path at all), how deep it is nested, how large it is, whether it is read as text or
//! as bytes, whether the path names a file, a directory, an absent entry of a mount or a path
//! of no mount, and which mode a call asks for.

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

/// The site directory of the layer root for THIS runtime's version, which is where a
/// dependency layer built against a site directory installs its packages.
const SITE_PACKAGES: &str = "python/lib/python3.12/site-packages";

/// The same directory for ANOTHER version of the runtime: its entries are extracted like every
/// other, and nothing of it is on this function's import path.
const OTHER_SITE_PACKAGES: &str = "python/lib/python3.9/site-packages";

/// How long past a due instant this suite waits before calling. The transition is owed by the
/// CLOCK, so nothing here sleeps on the emulator's behalf.
const SLACK: Duration = Duration::from_millis(150);

/// The `Timeout` every function this suite creates carries, in seconds. Sized the way the
/// sibling suites that RUN handlers size theirs: it has to cover this test binary's own
/// scheduling, not the handler's work, which is immediate.
const FUNCTION_TIMEOUT_SECONDS: i32 = 25;

/// How many times a row re-drives its invocation while the answer reports one of this test
/// binary's own host bounds rather than a run.
const HOST_BOUND_ATTEMPTS: usize = 4;

/// The text of a small data file a layer ships, read back as text and as bytes.
const TABLE: &str = "id,total\n1,42\n";

/// The text of a data file of the function's OWN package, which is a file of the task root.
const NOTES: &str = "the package's own file\n";

/// How many characters the larger data file of the layer holds, so a row reads a size that is
/// not the small file's.
const BLOB_CHARACTERS: usize = 4096;

/// A certificate bundle a layer ships, framed as PEM: a store named by a PATH is read out of
/// the files this environment has, which is one more reader of the same mounts.
const BUNDLE: &str = "-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n";

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
async fn publish_layer(router: &MultiServiceRouter, name: &str, entries: &[(&str, &str)]) -> String {
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

/// What one case answered, or `None` when the run was ended by a bound this whole test binary
/// shares rather than by anything the case asked for.
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
        "case {case}: the handler reports each refusal itself, so the invocation carries no \
         function error, got {failure:?} with {}",
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
/// interpreters in flight at once is fewer runs stopped for memory belonging to another suite.
/// A case whose run was stopped by one of those shared bounds says nothing about the rule and
/// is left out of the reading — but not every one can be: each table answers how many of its
/// cases a RUN answered, and a test that read none at all is a test that asserted nothing, so
/// it fails rather than passing silently.
#[tokio::test]
async fn a_python_run_reads_the_files_of_its_mounts_and_nothing_else() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let mut read = every_entry_of_a_mount_is_a_read_only_file(&router).await;
    read += the_import_path_lists_the_directories_of_the_mounts(&router).await;
    read += a_path_configuration_file_of_a_site_directory_extends_the_path(&router).await;
    assert!(
        read > 0,
        "expected at least one case of the tables above to be answered by a run, got {read}: \
         every invocation reported a bound this whole test binary shares, so nothing of the \
         rule was read"
    );
}

/// Every entry of the task root and of the layer root is a file at its own path: its size is
/// its own, its text and its bytes read back, the directory holding it lists it, a write to it
/// is refused for the mount being read-only, and a path no mount holds is absent.
///
/// Answers how many cases a run answered.
async fn every_entry_of_a_mount_is_a_read_only_file(router: &MultiServiceRouter) -> usize {
    let blob = "0".repeat(BLOB_CHARACTERS);
    let layer = publish_layer(
        router,
        "mounted-files",
        &[
            ("python/dep.py", "WHERE = 'the layer'\n"),
            ("python/vendored/table.csv", TABLE),
            (&format!("{SITE_PACKAGES}/sitedep.py"), "WHERE = 'site'\n"),
            (
                &format!("{OTHER_SITE_PACKAGES}/otherdep.py"),
                "WHERE = 'another version'\n",
            ),
            ("blob/zeros.bin", &blob),
            ("bin/tool", "#!/bin/sh\necho ran\n"),
            ("certs/bundle.pem", BUNDLE),
        ],
    )
    .await;
    let name = "probe-mount-files";
    create_function(
        router,
        name,
        &[("index.py", PROBE_SOURCE), ("notes.txt", NOTES)],
        &[layer],
    )
    .await;
    let case = "the files of the mounts";
    let Some(document) = answered(router, name, case, &json!({"case": "files"})).await else {
        return 0;
    };
    let expected = json!({
        // A file of a layer's import directory, of a package inside it, of a directory of
        // the layer root that is on no import path at all, and of a site directory of
        // another version: every one of them is extracted, whatever directory it is in.
        "sizes": {
            "/opt/python/dep.py": "WHERE = 'the layer'\n".len(),
            "/opt/python/vendored/table.csv": TABLE.len(),
            "/opt/blob/zeros.bin": BLOB_CHARACTERS,
            "/opt/bin/tool": "#!/bin/sh\necho ran\n".len(),
            "/opt/python/lib/python3.9/site-packages/otherdep.py":
                "WHERE = 'another version'\n".len(),
            // The function's own package is a mount like them, at the task root.
            "/var/task/notes.txt": NOTES.len(),
            "/var/task/index.py": PROBE_SOURCE.len(),
        },
        // The text and the bytes of one file are the text the archive carried.
        "text": TABLE,
        "bytes_length": TABLE.len(),
        "first_line": "id,total",
        // What a path IS, and what a path of no mount of this function is: nothing.
        "kinds": {
            "/opt": "directory",
            "/opt/python": "directory",
            "/opt/python/vendored": "directory",
            "/opt/python/dep.py": "file",
            "/var/task": "directory",
            "/var/task/notes.txt": "file",
            "/opt/python/absent.py": "absent",
            "/srv/host.txt": "absent",
            "/tmp/written.txt": "absent",
            // A path that walks OUT of a mount is the path it collapses to, which is a path
            // of the host and so of no mount at all.
            "/opt/python/../../srv/host.txt": "absent",
        },
        // The directory a file is in lists it, and a listing is of that directory alone.
        "listed_opt": ["bin", "blob", "certs", "python"],
        "listed_vendored": ["table.csv"],
        "listed_task": ["index.py", "notes.txt"],
        "walked_blob": ["zeros.bin"],
        // A mount is read-only, so a write to one is refused for that and not for anything
        // being absent; a listing of a file is not a listing at all.
        "write_layer": "OSError: Read-only file system",
        "write_task": "OSError: Read-only file system",
        "list_file": "NotADirectoryError",
        "read_directory": "IsADirectoryError",
        "read_absent": "FileNotFoundError",
        // A read that walks out of a mount reaches no file, and neither does a write to a
        // path of no mount: there is no host filesystem behind this environment.
        "read_escaped": "OSError",
        "write_outside": "OSError",
        // A file of a mount can be read and never written; a path of no mount is reachable
        // in no way at all.
        "access": {
            "/opt/python/dep.py": ["readable"],
            "/opt/bin/tool": ["readable"],
            "/var/task/notes.txt": ["readable"],
            "/srv/host.txt": [],
        },
        // The descriptor calls read the same file, and the stream of one reads its tail.
        "descriptor": TABLE,
        "descriptor_tail": "1,42\n",
        // A layer's executable directory is on the search path the environment states, and
        // its library directory on the loader's.
        "path_tail": "/opt/bin",
        "library_path_tail": "/opt/lib",
        // Another reader of the same files: a certificate store named by a path is read out
        // of the bundle the layer ships, and a path the layer ships nothing at is absent.
        "store_from_layer": 1,
        "store_absent": "FileNotFoundError",
    });
    assert_eq!(
        document, expected,
        "case {case}: expected every entry of the mounts to be a read-only file of the \
         environment, got {document}"
    );
    1
}

/// The handler's `sys.path` lists the directories of the mounts in the order a managed runtime
/// lists them: the task root, then the layer root's site directory for this version and its
/// version-independent directory, then the runtime's own directories.
///
/// Answers how many cases a run answered.
async fn the_import_path_lists_the_directories_of_the_mounts(
    router: &MultiServiceRouter,
) -> usize {
    let layer = publish_layer(
        router,
        "path-order",
        &[
            ("python/dep.py", "WHERE = 'the layer'\n"),
            (&format!("{SITE_PACKAGES}/sitedep.py"), "WHERE = 'site'\n"),
        ],
    )
    .await;
    let name = "probe-mount-path";
    create_function(router, name, &[("index.py", PROBE_SOURCE)], &[layer]).await;
    let case = "the import path of the environment";
    let Some(document) = answered(router, name, case, &json!({"case": "path"})).await else {
        return 0;
    };
    let expected = json!({
        "first": "/var/task",
        "layer_entries": [format!("/opt/{SITE_PACKAGES}"), "/opt/python"],
        "task_before_layers": true,
        "layers_before_runtime": true,
        "cwd": "/var/task",
        // Every entry of the path is a directory of this environment, not a path of a host.
        "entries_are_directories": true,
    });
    assert_eq!(
        document, expected,
        "case {case}: expected the directories of the mounts in a managed runtime's own \
         order, got {document}"
    );
    1
}

/// A path-configuration file of the layer root's site directory extends the import path with
/// the directories it names, and a module shipped only there is importable through it — while
/// a site directory of another version is processed for no runtime but its own.
///
/// Answers how many cases a run answered.
async fn a_path_configuration_file_of_a_site_directory_extends_the_path(
    router: &MultiServiceRouter,
) -> usize {
    let layer = publish_layer(
        router,
        "site-configured",
        &[
            // Two path-configuration files, read in the order of their names: a comment, a
            // blank line, a relative directory, an absolute one, a directory the layer holds
            // nothing at, and an `import` statement.
            (
                &format!("{SITE_PACKAGES}/01-first.pth"),
                "# a comment\n\n../../../extra\n",
            ),
            (
                &format!("{SITE_PACKAGES}/02-second.pth"),
                "/opt/also\nmissing\nimport sys\n",
            ),
            ("python/extra/configured.py", "WHERE = 'the extra tree'\n"),
            ("also/alongside.py", "WHERE = 'beside the layer root'\n"),
            // The version-independent import directory of the layer root is a site directory
            // too, so its own path-configuration file is processed as well.
            ("python/10-rooted.pth", "rooted\n"),
            ("python/rooted/from_root.py", "WHERE = 'the rooted tree'\n"),
            // The same layout under ANOTHER version's site directory, which no runtime of
            // this version processes.
            (
                &format!("{OTHER_SITE_PACKAGES}/03-other.pth"),
                "../../../elsewhere\n",
            ),
            ("python/elsewhere/unconfigured.py", "WHERE = 'not added'\n"),
        ],
    )
    .await;
    let name = "probe-mount-site";
    create_function(router, name, &[("index.py", PROBE_SOURCE)], &[layer]).await;
    let case = "the path configuration of a site directory";
    let Some(document) = answered(router, name, case, &json!({"case": "site"})).await else {
        return 0;
    };
    let expected = json!({
        // The directories the files name are on the path, in the order their own site
        // directories and names were read, and behind every directory of the mounts
        // themselves.
        "added": ["/opt/python/extra", "/opt/also", "/opt/python/rooted"],
        "added_after_roots": true,
        // A module of an added directory imports, and reports its own file under the layer
        // root.
        "configured": "the extra tree",
        "configured_file": "/opt/python/extra/configured.py",
        "alongside": "beside the layer root",
        "from_root": "the rooted tree",
        // A directory named by the site directory of another version is on no path of this
        // runtime, so what it holds is not importable — though the file is still there.
        "unconfigured": "ModuleNotFoundError",
        "unconfigured_on_disk": true,
    });
    assert_eq!(
        document, expected,
        "case {case}: expected the path extended by the site directory's own configuration, \
         got {document}"
    );
    1
}

/// The probe every table drives: its handler reports what each filesystem call answered, so a
/// refusal is a value of the answer rather than a failed invocation.
const PROBE_SOURCE: &str = r#"
import io
import os
import sys


_TASK_ROOT = "/var/task"
_LAYER_ROOT = "/opt"
_RUNTIME_ROOT = "/var/runtime"
# A path of the host, on no mount of this function: it is reachable in no way, and a path that
# walks out of a mount with `..` collapses onto it.
_OFF_MOUNT = "/srv/host.txt"


def _refused(call):
    """What `call` answered, or the NAME of the error it raised."""
    try:
        return call()
    except BaseException as error:
        return type(error).__name__


def _refusal(call):
    """The CLASS and the reason of the error `call` raised, or what it answered."""
    try:
        return call()
    except OSError as error:
        return "%s: %s" % (type(error).__name__, error.strerror)
    except BaseException as error:
        return type(error).__name__


def _kind(path):
    """What `path` is to this environment."""
    if os.path.isdir(path):
        return "directory"
    if os.path.isfile(path):
        return "file"
    return "absent"


def _reachable(path):
    """The ways `path` is reachable, out of the two a read-only mount decides.

    Readability and writability are what a mount being read-only settles for every entry of
    it. Executability is not asserted: the archive's own permission bits are not carried by
    the read that produces these entries, so no row claims them.
    """
    ways = []
    if os.access(path, os.R_OK):
        ways.append("readable")
    if os.access(path, os.W_OK):
        ways.append("writable")
    return ways


def _sizes(paths):
    return {path: _refused(lambda path=path: os.path.getsize(path)) for path in paths}


def _files(event):
    import ssl

    table = _LAYER_ROOT + "/python/vendored/table.csv"
    with open(table) as reading:
        text = reading.read()
    with open(table, "rb") as reading:
        held = reading.read()
    descriptor = os.open(table, os.O_RDONLY)
    try:
        read = os.read(descriptor, 4096).decode("utf-8")
        os.lseek(descriptor, len(text) - 5, 0)
        tail = os.read(descriptor, 4096).decode("utf-8")
    finally:
        os.close(descriptor)
    walked = []
    for _, _, names in os.walk(_LAYER_ROOT + "/blob"):
        walked.extend(names)
    store = ssl.create_default_context()
    store.load_verify_locations(cafile=_LAYER_ROOT + "/certs/bundle.pem")
    return {
        "sizes": _sizes(
            [
                _LAYER_ROOT + "/python/dep.py",
                _LAYER_ROOT + "/python/vendored/table.csv",
                _LAYER_ROOT + "/blob/zeros.bin",
                _LAYER_ROOT + "/bin/tool",
                _LAYER_ROOT + "/python/lib/python3.9/site-packages/otherdep.py",
                _TASK_ROOT + "/notes.txt",
                _TASK_ROOT + "/index.py",
            ]
        ),
        "text": text,
        "bytes_length": len(held),
        "first_line": io.StringIO(text).readline().rstrip("\n"),
        "kinds": {
            path: _kind(path)
            for path in (
                _LAYER_ROOT,
                _LAYER_ROOT + "/python",
                _LAYER_ROOT + "/python/vendored",
                _LAYER_ROOT + "/python/dep.py",
                _TASK_ROOT,
                _TASK_ROOT + "/notes.txt",
                _LAYER_ROOT + "/python/absent.py",
                _OFF_MOUNT,
                "/tmp/written.txt",
                _LAYER_ROOT + "/python/../.." + _OFF_MOUNT,
            )
        },
        "listed_opt": sorted(os.listdir(_LAYER_ROOT)),
        "listed_vendored": sorted(os.listdir(_LAYER_ROOT + "/python/vendored")),
        "listed_task": sorted(os.listdir(_TASK_ROOT)),
        "walked_blob": sorted(walked),
        "write_layer": _refusal(lambda: open(_LAYER_ROOT + "/python/dep.py", "w")),
        "write_task": _refusal(lambda: open(_TASK_ROOT + "/notes.txt", "a")),
        "list_file": _refused(lambda: os.listdir(_LAYER_ROOT + "/python/dep.py")),
        "read_directory": _refused(lambda: open(_LAYER_ROOT + "/python").read()),
        "read_absent": _refused(lambda: open(_LAYER_ROOT + "/python/absent.py").read()),
        "read_escaped": _refused(
            lambda: open(_LAYER_ROOT + "/python/../.." + _OFF_MOUNT, "rb").read()
        ),
        "write_outside": _refused(lambda: open("/tmp/written.txt", "w")),
        "access": {
            path: _reachable(path)
            for path in (
                _LAYER_ROOT + "/python/dep.py",
                _LAYER_ROOT + "/bin/tool",
                _TASK_ROOT + "/notes.txt",
                _OFF_MOUNT,
            )
        },
        "descriptor": read,
        "descriptor_tail": tail,
        "path_tail": os.environ.get("PATH", "").rpartition(":")[2],
        "library_path_tail": os.environ.get("LD_LIBRARY_PATH", "").rpartition(":")[2],
        "store_from_layer": len(store.get_ca_certs(binary_form=True)),
        "store_absent": _refused(
            lambda: ssl.create_default_context().load_verify_locations(
                cafile=_LAYER_ROOT + "/certs/missing.pem"
            )
        ),
    }


def _path(event):
    entries = list(sys.path)
    layer_entries = [entry for entry in entries if entry.startswith(_LAYER_ROOT + "/")]
    runtime_at = [
        index for index, entry in enumerate(entries) if entry == _RUNTIME_ROOT
    ]
    layer_at = [
        index for index, entry in enumerate(entries) if entry in layer_entries
    ]
    return {
        "first": entries[0] if entries else None,
        "layer_entries": layer_entries,
        "task_before_layers": entries.index(_TASK_ROOT) < min(layer_at),
        "layers_before_runtime": max(layer_at) < min(runtime_at),
        "cwd": os.getcwd(),
        "entries_are_directories": all(os.path.isdir(entry) for entry in entries),
    }


def _site(event):
    import importlib

    roots = [
        _LAYER_ROOT + "/" + "python/lib/python3.12/site-packages",
        _LAYER_ROOT + "/python",
    ]
    added = [
        entry
        for entry in sys.path
        if entry.startswith(_LAYER_ROOT + "/") and entry not in roots
    ]
    last_root = max(sys.path.index(root) for root in roots if root in sys.path)
    return {
        "added": added,
        "added_after_roots": all(sys.path.index(entry) > last_root for entry in added),
        "configured": _refused(
            lambda: importlib.import_module("configured").WHERE
        ),
        "configured_file": _refused(
            lambda: importlib.import_module("configured").__file__
        ),
        "alongside": _refused(lambda: importlib.import_module("alongside").WHERE),
        "from_root": _refused(lambda: importlib.import_module("from_root").WHERE),
        "unconfigured": _refused(
            lambda: importlib.import_module("unconfigured").WHERE
        ),
        "unconfigured_on_disk": os.path.isfile(
            _LAYER_ROOT + "/python/elsewhere/unconfigured.py"
        ),
    }


_CASES = {
    "files": _files,
    "path": _path,
    "site": _site,
}


def handler(event, context):
    return _CASES[event["case"]](event)
"#;
