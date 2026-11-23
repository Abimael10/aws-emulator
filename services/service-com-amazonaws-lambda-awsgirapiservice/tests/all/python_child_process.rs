// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the CHILD PROCESS a `python3.x` handler starts:
//! `PublishLayerVersion` (`POST /2018-10-31/layers/{LayerName}/versions`) then
//! `CreateFunction` with that layer and `Invoke` (`POST
//! /2015-03-31/functions/{FunctionName}/invocations`) over the router.
//!
//! The rule under test: a managed runtime's execution environment is an ordinary POSIX sandbox
//! — it has a shell and the usual utilities, and `/opt/bin` of a layer is on its `PATH` — so a
//! handler that starts a child process RUNS the program it named, resolved against that
//! environment's own `PATH` and mounts and performed over that environment's filesystem, and
//! reads back the exit status and the two streams the program produced; a program the
//! environment does not have is the `FileNotFoundError` the command search ending with nothing
//! raises, and a program this emulator has no engine for is reported as that rather than as
//! work that was done.
//!
//! The cases differ along what the rule generalises over: how the program is named (a bare name
//! resolved on `PATH`, an absolute path, a command line through the shell, a stage of a
//! pipeline, the process pipe, a sibling program a layer's own program runs), whether it
//! carries
//! operands, whether it is a program of the layer or a utility of the sandbox, which status it
//! exits with, which stream it writes, whether it is handed standard input, whether it writes
//! the writable storage or a read-only mount, and whether it exists and is a program this
//! environment can run at all.

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

/// How long past a due instant this suite waits before calling. The transition is owed by the
/// CLOCK, so nothing here sleeps on the emulator's behalf.
const SLACK: Duration = Duration::from_millis(150);

/// The `Timeout` the function this suite creates carries, in seconds. Sized the way the sibling
/// suites that RUN handlers size theirs: it has to cover this test binary's own scheduling, not
/// the handler's work, which is immediate.
const FUNCTION_TIMEOUT_SECONDS: i32 = 25;

/// How many times a row re-drives its invocation while the answer reports one of this test
/// binary's own host bounds rather than a run.
const HOST_BOUND_ATTEMPTS: usize = 4;

/// The program the layer ships under `bin/`, which `/opt/bin` being on the environment's `PATH`
/// makes reachable by its bare name.
const TOOL: &str = "#!/bin/sh\necho ran\n";

/// A second one, which reports the OPERAND it was given — so a case reads a program's arguments
/// and not only its name.
const GREET: &str = "#!/bin/sh\necho \"hello $1\"\n";

/// A program of the layer that runs its SIBLING: the command search is the same one inside a
/// program as at the top level, so this reports what `greet` wrote for it.
const CALLER: &str = "#!/bin/sh\ngreet \"from the sibling\"\n";

/// A third, whose own commands report a failure on standard error and which then exits with a
/// status of its own.
const FAILS: &str = "#!/bin/sh\ncat /opt/python/absent.csv\nexit 3\n";

/// What that program's failing command reports, which is the diagnostic the utility writes for
/// a file that is not there.
const FAILING_DIAGNOSTIC: &str = "cat: /opt/python/absent.csv: No such file or directory\n";

/// The status that one exits with.
const FAILING_STATUS: i32 = 3;

/// A fourth, written for an interpreter this emulator has no engine for: nothing runs it, and
/// the call says so.
const NATIVE: &str = "#!/usr/bin/env python3\nprint('hi')\n";

/// The number a call that names a program the environment does not have raises with (`ENOENT`),
/// and the number one naming a program this emulator cannot run raises with (`ENOSYS`).
const NOT_FOUND_ERRNO: i32 = 2;

/// See [`NOT_FOUND_ERRNO`].
const UNSUPPORTED_ERRNO: i32 = 38;

/// A data file the layer ships, which a utility of the sandbox reads.
const TABLE: &str = "id,total\n1,42\n";

/// A file the layer ships whose bytes are NO text at all — a packed asset, which is how a
/// layer ships a table, an image or an archive — and the hexadecimal a handler reads it back
/// as.
const ASSET: &[u8] = &[0x00, 0xff, 0x10, 0xfe, 0x7f];

/// See [`ASSET`].
const ASSET_HEX: &str = "00ff10fe7f";

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

/// A .zip archive holding `entries` as the BYTES each of them carries, written in memory: how
/// a caller uploads a layer that ships a file which is no text at all.
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

/// Publish one layer version holding `entries`, answering its version ARN.
async fn publish_layer(
    router: &MultiServiceRouter,
    name: &str,
    entries: &[(&str, &[u8])],
) -> String {
    let published = call(
        router,
        "POST",
        &format!("{LAYERS_URI}/{name}/versions"),
        &serde_json::to_vec(&json!({
            "Content": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(archive(entries)),
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

/// Create a `python3.12` function holding `entries` with the layers `layers` names, and wait out
/// the provisioning window the create documents.
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

/// The whole rule, driven END TO END: real `PublishLayerVersion`, `CreateFunction` and `Invoke`
/// requests over the router, with the handler reporting what every child it started came to.
///
/// One invocation for the whole table, the way the sibling runtime suites are built: the memory
/// bound a run is stopped on is read from the WHOLE process (`state::execution::python`) and
/// this binary runs its suites in parallel, so fewer interpreters in flight at once is fewer
/// runs stopped for memory belonging to another suite. A run stopped by one of those shared
/// bounds says nothing about the rule, and this test fails rather than passing silently when no
/// run answered at all.
#[tokio::test]
async fn a_python_handler_runs_the_programs_of_its_execution_environment() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let layer = publish_layer(
        &router,
        "child-programs",
        &[
            ("bin/tool", TOOL.as_bytes()),
            ("bin/greet", GREET.as_bytes()),
            ("bin/caller", CALLER.as_bytes()),
            ("bin/fails", FAILS.as_bytes()),
            ("bin/native", NATIVE.as_bytes()),
            ("python/vendored/table.csv", TABLE.as_bytes()),
            ("asset/data.bin", ASSET),
        ],
    )
    .await;
    let name = "probe-child-process";
    create_function(&router, name, &[("index.py", PROBE_SOURCE)], &[layer]).await;
    let case = "the children of one invocation";
    let Some(document) = answered(&router, name, case, &json!({})).await else {
        panic!(
            "case {case}: every invocation reported a bound this whole test binary shares, so \
             nothing of the rule was read"
        );
    };
    let expected = json!({
        // A program the layer ships is reachable by its bare name, because `/opt/bin` is on
        // the environment's `PATH`, and by its own path — and it reports the operands it was
        // given.
        "bare": { "status": 0, "stdout": "ran\n", "quiet": true },
        "by_path": { "status": 0, "stdout": "hello world\n", "quiet": true },
        // A utility of the sandbox runs too, over the files of the environment's mounts.
        "utility": TABLE,
        // A command LINE goes through the environment's own shell.
        "line": "one\ntwo\n",
        // And a program of the layer is resolved INSIDE such a line exactly as it is at the
        // top level: by its bare name, by its path, through `os.popen`, through the call that
        // answers a status and an output together, and from inside another program of the
        // layer.
        "line_bare": "ran\n",
        "line_path": "hello shell\n",
        "piped": "hello piped\n",
        "popen": "hello popen\n",
        "sibling": { "status": 0, "stdout": "hello from the sibling\n", "quiet": true },
        // The program's own exit status and its own standard error are what the call reports,
        // and a checked call raises this module's own error for a status that is not success.
        "failing": {
            "status": FAILING_STATUS,
            "stdout": "",
            "quiet": false,
        },
        "failing_stderr": FAILING_DIAGNOSTIC,
        "checked": "CalledProcessError",
        // A name the environment has no program for is where the command search ends with
        // nothing, and a program this emulator has no engine for is reported as that.
        "absent": format!("FileNotFoundError: {NOT_FOUND_ERRNO}"),
        "native": format!("OSError: {UNSUPPORTED_ERRNO}"),
        // What a child wrote to the function's ephemeral storage is there for the handler
        // afterwards, and a child's write to a read-only mount fails exactly as the handler's
        // own does.
        "wrote_status": 0,
        "wrote": "kept\n",
        "write_layer_refused": true,
        "layer_unchanged": TOOL,
        // What the caller hands the child on standard input is what the program reads.
        "fed": "fed in\n",
        // The status and the output of a command line, as the call that answers both reports
        // them.
        "status_output": [0, "hi"],        // A child that ran has an id of its own, which is not the environment's own process.
        "own_pid": true,
        // A file of a layer whose bytes are NO text is a file of the environment like every
        // other: the directory lists it, its size is its own and its bytes read back.
        "asset_names": ["data.bin"],
        "asset_size": ASSET.len(),
        "asset_bytes": ASSET_HEX,
    });
    assert_eq!(
        document, expected,
        "case {case}: expected every child to report the program it ran, got {document}"
    );
}

/// The probe the table drives: its handler starts one child per case and reports what each came
/// to, so a refusal is a value of the answer rather than a failed invocation.
const PROBE_SOURCE: &str = r##"
import os
import subprocess


def _ran(argv, **keywords):
    """What one child came to: its status, what it wrote, and whether it was quiet."""
    done = subprocess.run(argv, capture_output=True, text=True, **keywords)
    return {
        "status": done.returncode,
        "stdout": done.stdout,
        "quiet": done.stderr == "",
    }


def _refused(call):
    """The CLASS and the number of the error `call` raised, or what it answered."""
    try:
        return call()
    except OSError as error:
        return "%s: %s" % (type(error).__name__, error.errno)
    except BaseException as error:
        return type(error).__name__


def handler(event, context):
    report = {}
    # A program the layer ships, by its bare name on the environment's PATH and by its own
    # path, with an operand.
    report["bare"] = _ran(["tool"])
    report["by_path"] = _ran(["/opt/bin/greet", "world"])
    # A utility of the sandbox, reading a file of a mount.
    report["utility"] = subprocess.check_output(
        ["cat", "/opt/python/vendored/table.csv"], text=True
    )
    # A command line, performed by the environment's own shell.
    report["line"] = subprocess.run(
        "echo one; echo two", shell=True, capture_output=True, text=True
    ).stdout
    # A program of the layer, named INSIDE such a line: by its bare name, by its own path, and
    # as a stage of a pipeline.
    report["line_bare"] = subprocess.run(
        "tool", shell=True, capture_output=True, text=True
    ).stdout
    report["line_path"] = subprocess.run(
        "/opt/bin/greet shell", shell=True, capture_output=True, text=True
    ).stdout
    report["piped"] = subprocess.run(
        "/opt/bin/greet piped | cat", shell=True, capture_output=True, text=True
    ).stdout
    # The same through the process pipe the standard library builds on this module.
    with os.popen("/opt/bin/greet popen") as reading:
        report["popen"] = reading.read()
    # A program of the layer that runs its SIBLING, which is the same search once more.
    report["sibling"] = _ran(["caller"])
    # A program's own exit status and its own standard error.
    failing = subprocess.run(
        ["/opt/bin/fails"], capture_output=True, text=True
    )
    report["failing"] = {
        "status": failing.returncode,
        "stdout": failing.stdout,
        "quiet": failing.stderr == "",
    }
    report["failing_stderr"] = failing.stderr
    report["checked"] = _refused(lambda: subprocess.check_call(["/opt/bin/fails"]))
    # A name this environment has no program for, and a program written for an interpreter
    # this runtime has none of.
    report["absent"] = _refused(lambda: subprocess.run(["absent-tool"]))
    report["native"] = _refused(lambda: subprocess.run(["/opt/bin/native"]))
    # What a child writes to the ephemeral storage is there for the handler afterwards.
    wrote = subprocess.run(
        ["sh", "-c", "echo kept > /tmp/child.txt"], capture_output=True, text=True
    )
    report["wrote_status"] = wrote.returncode
    with open("/tmp/child.txt") as reading:
        report["wrote"] = reading.read()
    # A write under a read-only mount fails for a child exactly as it does for the handler.
    refused = _ran(["sh", "-c", "echo changed > /opt/bin/tool"])
    report["write_layer_refused"] = refused["status"] != 0
    with open("/opt/bin/tool") as reading:
        report["layer_unchanged"] = reading.read()
    # What the caller hands the child on standard input is what the program reads.
    report["fed"] = subprocess.check_output(["cat"], input="fed in\n", text=True)
    # The status and the output of a command line together.
    report["status_output"] = list(subprocess.getstatusoutput("echo hi"))
    # A child that ran reports an id of its own.
    report["own_pid"] = subprocess.Popen(["tool"], stdout=subprocess.PIPE).pid != os.getpid()
    # A file of a layer whose bytes are no text at all is a file of this environment too.
    report["asset_names"] = sorted(os.listdir("/opt/asset"))
    report["asset_size"] = os.path.getsize("/opt/asset/data.bin")
    with open("/opt/asset/data.bin", "rb") as reading:
        report["asset_bytes"] = reading.read().hex()
    return report
"##;
