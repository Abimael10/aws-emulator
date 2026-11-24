// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the FILES an attached layer version puts in the execution
//! environment: `PublishLayerVersion` (`POST /2018-10-31/layers/{LayerName}/versions`) then
//! `CreateFunction` with those layer versions and `Invoke` (`POST
//! /2015-03-31/functions/{FunctionName}/invocations`) over the router.
//!
//! The rule under test: every FILE an attached layer version's archive carries is in the
//! execution environment's layer root at the same relative path before the handler's
//! initialization runs, byte for byte — the same number of bytes and the same SHA-256 as the
//! archive carried — and that root is READ-ONLY, so a write into it is refused for the
//! filesystem being read-only rather than for the path being absent. Where two attached
//! versions carry one path, the later entry of the function's layer list is the one the root
//! holds, and nothing the archives do not carry is in the root at all.
//!
//! The rows differ along what the rule generalises over: how deep inside the archive the file
//! sits (the archive's root, one directory, four directories), what it holds (a module's
//! source, comma-separated text, non-ASCII text, a shell script, bytes that are no text at
//! all), how large it is (no bytes, one byte, a few, 4 KiB), how many layer versions are
//! attached, and whether a path is carried by one version or by both. One row is the shape the
//! issue behind this suite recorded: a module under the import directory, a packed data file, a
//! file four directories deep and an executable script, inventoried from the handler.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ACTIVATION_DELAY;
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use sha2::Digest as _;
use smithy_server_runtime::MultiServiceRouter;

use crate::invoke_request_id::{is_host_bound, one_invocation_stream_at_a_time};

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

/// The directory a layer version's content is extracted into, which is what a handler reads its
/// layers' files from.
const LAYER_ROOT: &str = "/opt";

/// How long past a due instant this suite waits before calling. The transition is owed by the
/// CLOCK, so nothing here sleeps on the emulator's behalf.
const SLACK: Duration = Duration::from_millis(150);

/// The `Timeout` the function this suite creates carries, in seconds. Sized the way the sibling
/// suites that RUN handlers size theirs: it has to cover this test binary's own scheduling, not
/// the handler's work, which is immediate.
const FUNCTION_TIMEOUT_SECONDS: i32 = 25;

/// How many times the invocation is re-driven while the answer reports one of this test
/// binary's own host bounds rather than a run.
const HOST_BOUND_ATTEMPTS: usize = 6;

/// How many bytes the largest row of the table holds: a packed file of a layer, large enough
/// that its size is nothing another row's is.
const BLOB_BYTES: usize = 4096;

/// What the file BOTH attached versions carry holds in the EARLIER one of them, which is the
/// entry the later version's own overwrites.
const OVERLAID_EARLIER: &str = "carried by the layer named first\n";

/// What the LATER version carries at that same path, which is therefore what the layer root
/// holds. See [`OVERLAID_EARLIER`].
const OVERLAID_LATER: &str = "carried by the layer named after it\n";

/// One file a published layer version's archive carries.
struct LayerFile {
    /// What this row varies, named in every assertion it drives.
    case: &'static str,
    /// The path the archive stores it under, which is its path under the layer root.
    path: &'static str,
    /// The bytes the archive carries at that path.
    bytes: Vec<u8>,
}

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

/// A .zip archive holding `entries` as the BYTES each of them carries, written in memory: how a
/// caller uploads a layer that ships text and files that are no text at all in one archive.
fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        for (name, bytes) in entries {
            writer.start_file(*name, options).expect("the entry starts");
            std::io::Write::write_all(&mut writer, bytes).expect("the entry's bytes");
        }
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// The base64 an archive holding `entries` travels as, which is what a `ZipFile` member carries.
fn zip_file(entries: &[(&str, &[u8])]) -> String {
    base64::engine::general_purpose::STANDARD.encode(archive(entries))
}

/// Publish one layer version holding `files`, answering its version ARN.
async fn publish_layer(router: &MultiServiceRouter, name: &str, files: &[LayerFile]) -> String {
    let entries: Vec<(&str, &[u8])> = files
        .iter()
        .map(|file| (file.path, file.bytes.as_slice()))
        .collect();
    let published = call(
        router,
        "POST",
        &format!("{LAYERS_URI}/{name}/versions"),
        &serde_json::to_vec(&json!({
            "Content": { "ZipFile": zip_file(&entries) },
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

/// Create a `python3.12` function with the layer versions `layers` names, and wait out the
/// provisioning window the create documents.
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
            "Code": { "ZipFile": zip_file(&[("index.py", PROBE_SOURCE.as_bytes())]) },
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

/// What the probe answered, re-driving the invocation while the answer reports a bound this
/// whole test binary shares rather than a run of the handler.
async fn answered(router: &MultiServiceRouter, name: &str, case: &str, event: &Value) -> Value {
    let mut answer = invoke_once(router, name, event).await;
    for _ in 1..HOST_BOUND_ATTEMPTS {
        if !is_host_bound(answer.status, &answer.text()) {
            break;
        }
        answer = invoke_once(router, name, event).await;
    }
    assert!(
        !is_host_bound(answer.status, &answer.text()),
        "case {case}: every invocation reported a bound this whole test binary shares, so \
         nothing of the rule was read: {} {}",
        answer.status,
        answer.text()
    );
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
    answer.json()
}

/// The files the version named FIRST in the function's layer list carries: one row per kind of
/// file a layer archive holds.
fn earlier_layer_files() -> Vec<LayerFile> {
    vec![
        LayerFile {
            case: "a module under the layer's import directory",
            path: "python/opt_layer_mod.py",
            bytes: b"WHERE = 'the earlier layer'\n".to_vec(),
        },
        LayerFile {
            case: "a text file at the archive's own root",
            path: "VERSION.txt",
            bytes: b"1.2.3\n".to_vec(),
        },
        LayerFile {
            case: "comma-separated text one directory down",
            path: "assets/table.csv",
            bytes: b"id,total\n1,42\n".to_vec(),
        },
        LayerFile {
            case: "a text file four directories down",
            path: "assets/deep/a/b/c.txt",
            bytes: b"as deep as the archive stored it\n".to_vec(),
        },
        LayerFile {
            case: "text whose characters are not ASCII",
            path: "assets/unicode.txt",
            bytes: "sj\u{00e4}lvst\u{00e4}ndig \u{2713}\n".as_bytes().to_vec(),
        },
        LayerFile {
            case: "a shell script a handler starts as a program",
            path: "assets/tool.sh",
            bytes: b"#!/bin/sh\necho ran\n".to_vec(),
        },
        LayerFile {
            case: "a file of no bytes at all",
            path: "assets/empty.dat",
            bytes: Vec::new(),
        },
        LayerFile {
            case: "one byte that is no text",
            path: "assets/one.bin",
            bytes: vec![0xff],
        },
        LayerFile {
            case: "a few bytes that are no text",
            path: "assets/data.bin",
            bytes: vec![0x00, 0xff, 0x10, 0xfe, 0x7f],
        },
        LayerFile {
            case: "4 KiB of bytes that are no text",
            path: "assets/blob.bin",
            bytes: (0..BLOB_BYTES).map(|at| (at % 256) as u8).collect(),
        },
        LayerFile {
            case: "a path both attached versions carry, in the earlier one",
            path: "shared/overlaid.txt",
            bytes: OVERLAID_EARLIER.as_bytes().to_vec(),
        },
    ]
}

/// The files the version named AFTER it carries: a second attached version contributes its own
/// files to the one root, and the path both of them carry is this version's.
fn later_layer_files() -> Vec<LayerFile> {
    vec![
        LayerFile {
            case: "a module of the second attached version",
            path: "python/second_layer_mod.py",
            bytes: b"WHERE = 'the later layer'\n".to_vec(),
        },
        LayerFile {
            case: "a file only the second attached version carries",
            path: "shared/second-only.txt",
            bytes: b"only the later layer ships this\n".to_vec(),
        },
        LayerFile {
            case: "a path both attached versions carry, in the later one",
            path: "shared/overlaid.txt",
            bytes: OVERLAID_LATER.as_bytes().to_vec(),
        },
    ]
}

/// The whole rule, driven END TO END: real `PublishLayerVersion`, `CreateFunction` and `Invoke`
/// requests over the router, with the handler inventorying the layer root itself.
///
/// One invocation for the whole table, the way the sibling runtime suites are built: the memory
/// bound a run is stopped on is read from the WHOLE process (`state::execution::python`) and
/// this binary runs its suites in parallel, so fewer interpreters in flight at once is fewer
/// runs stopped for memory belonging to another suite.
#[tokio::test]
async fn an_attached_layers_files_are_the_read_only_files_of_the_layer_root() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let earlier = earlier_layer_files();
    let later = later_layer_files();
    let earlier_arn = publish_layer(&router, "opt-materialized-earlier", &earlier).await;
    let later_arn = publish_layer(&router, "opt-materialized-later", &later).await;
    let name = "probe-opt-materialization";
    create_function(&router, name, &[earlier_arn, later_arn]).await;

    // What the layer root holds: every row of both versions, the later version's entry winning
    // at the one path both carry — which is what extracting one archive over the other leaves.
    let mut held: BTreeMap<&'static str, &LayerFile> = BTreeMap::new();
    for file in earlier.iter().chain(later.iter()) {
        held.insert(file.path, file);
    }

    let case = "the files of two attached layer versions";
    let document = answered(
        &router,
        name,
        case,
        &json!({
            "existing": format!("{LAYER_ROOT}/assets/table.csv"),
            "module": "opt_layer_mod",
            "second_module": "second_layer_mod",
            "overlaid": "shared/overlaid.txt",
        }),
    )
    .await;

    // The root holds EXACTLY the paths the archives carry: nothing of them is missing, and
    // nothing the archives do not carry is there.
    let inventoried: Vec<String> = held.keys().map(|path| (*path).to_string()).collect();
    assert_eq!(
        document["paths"],
        json!(inventoried),
        "case {case}: expected the layer root to hold exactly the packaged paths, got {}",
        document["paths"]
    );

    // And each of them is a readable, unwritable file whose bytes are the bytes the archive
    // carried: the same count and the same digest, read back through the handler.
    for (path, file) in &held {
        let reading = json!({
            "size": file.bytes.len(),
            "sha256": hex::encode(sha2::Sha256::digest(&file.bytes)),
            "stat_size": file.bytes.len(),
            "readable": true,
            "writable": false,
        });
        let actual = document["files"].get(*path);
        assert_eq!(
            actual,
            Some(&reading),
            "case {}: expected {LAYER_ROOT}/{path} to be the packaged file {reading}, got \
             {actual:?}",
            file.case
        );
    }

    // The root is READ-ONLY, whichever call writes it and whether the path it names is one the
    // archives carry or one they do not; a path under it that no version carries is absent
    // rather than refused for the filesystem.
    let refusals = json!({
        "write_existing": "EROFS",
        "write_new": "EROFS",
        "write_descriptor": "EROFS",
        "read_absent": "FileNotFoundError",
    });
    for (member, expected) in refusals.as_object().expect("the refusals are an object") {
        assert_eq!(
            &document[member], expected,
            "case {case}: expected {member} to answer {expected}, got {}",
            document[member]
        );
    }

    // A module is one of those files read another way: the import directory of each attached
    // version resolves the module it carries, and the overlaid path reads as the later
    // version's own text.
    let read = json!({
        "imported": "the earlier layer",
        "second_imported": "the later layer",
        "overlaid": OVERLAID_LATER,
    });
    for (member, expected) in read.as_object().expect("the readings are an object") {
        assert_eq!(
            &document[member], expected,
            "case {case}: expected {member} to read {expected}, got {}",
            document[member]
        );
    }
}

/// The probe the table drives: its handler walks the layer root and reports what it found and
/// what every write into it came to, so a refusal is a value of the answer rather than a failed
/// invocation.
const PROBE_SOURCE: &str = r#"
import errno
import hashlib
import os


_LAYER_ROOT = "/opt"


def _refusal(call):
    """The errno name `call` was refused with, or what it answered."""
    try:
        return call()
    except OSError as error:
        return errno.errorcode.get(error.errno, str(error.errno))


def _read(call):
    """What `call` answered, or the NAME of the error it raised."""
    try:
        return call()
    except BaseException as error:
        return type(error).__name__


def _wrote(path):
    with open(path, "w") as writing:
        writing.write("changed")
    return "written"


def _wrote_descriptor(path):
    descriptor = os.open(path, os.O_WRONLY)
    os.close(descriptor)
    return "opened for writing"


def _text(path):
    with open(path) as reading:
        return reading.read()


def _inventory():
    """Every regular file under the layer root, at its path inside it: how many bytes it
    holds, the digest of those bytes, the size a stat reports, and whether it can be read and
    written."""
    found = {}
    for base, _directories, names in os.walk(_LAYER_ROOT):
        for name in names:
            path = base + "/" + name
            with open(path, "rb") as reading:
                held = reading.read()
            found[path[len(_LAYER_ROOT) + 1:]] = {
                "size": len(held),
                "sha256": hashlib.sha256(held).hexdigest(),
                "stat_size": os.path.getsize(path),
                "readable": os.access(path, os.R_OK),
                "writable": os.access(path, os.W_OK),
            }
    return found


def _attribute(module, attribute):
    loaded = __import__(module)
    return getattr(loaded, attribute, None)


def handler(event, context):
    found = _inventory()
    return {
        "paths": sorted(found),
        "files": found,
        "write_existing": _refusal(lambda: _wrote(event["existing"])),
        "write_new": _refusal(lambda: _wrote(_LAYER_ROOT + "/written.txt")),
        "write_descriptor": _refusal(lambda: _wrote_descriptor(event["existing"])),
        "read_absent": _read(lambda: _text(_LAYER_ROOT + "/assets/absent.bin")),
        "imported": _read(lambda: _attribute(event["module"], "WHERE")),
        "second_imported": _read(lambda: _attribute(event["second_module"], "WHERE")),
        "overlaid": _read(lambda: _text(_LAYER_ROOT + "/" + event["overlaid"])),
    }
"#;
