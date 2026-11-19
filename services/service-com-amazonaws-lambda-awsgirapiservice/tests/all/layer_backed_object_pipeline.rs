// SPDX-License-Identifier: CC0-1.0

//! A layer-backed `python3.x` object pipeline, end to end over the wire: `PublishLayerVersion`
//! from an Amazon S3 object, `CreateFunction` carrying that layer, and `Invoke` — Lambda and
//! Amazon S3 mounted on ONE engine, so every object the handler reads and writes is the object
//! this suite reads through Amazon S3's own API.
//!
//! Two rules, each asserted over a table rather than over one input:
//!
//! * **A managed `python3.x` execution environment presents the handler's WHOLE import path —
//!   the layers the function carries, the AWS SDK the runtime bundles (`boto3`/`botocore`) and
//!   the complete standard library, including the `os` module's `stat_result` type that
//!   `shutil` reads as it is imported and `pathlib` names in its own signatures — so a handler
//!   module importing any of them loads, and the invocation reports the handler's OWN outcome:
//!   HTTP 200 with no `X-Amz-Function-Error` and its return value as the payload, or, when the
//!   handler raises, `X-Amz-Function-Error: Unhandled` with the raised Python exception's own
//!   class as the error document's `errorType` and its own `str` as the `errorMessage`.**
//! * **A layer version's `Content` reports the archive Lambda holds — its `Location`, its
//!   `CodeSize` and the `CodeSha256` of the stored bytes — on the publish's own answer and on
//!   every read of that version**, which is what lets a deploy verify the bytes it published
//!   before it attaches them to a function. `layer_content_identity.rs` owns that rule over
//!   the table of content SOURCES (inline, an Amazon S3 object, a referenced object, a deleted
//!   source) on the publish and on `GetLayerVersion`; what is pinned here is the one read
//!   surface it does not reach, `GetLayerVersionByArn`, and the publish a deploy reads the
//!   digest off before it creates the function below.
//!
//! The rows differ along what the rules generalise over: for the pipeline, how deeply the
//! source key is nested, how many rows and cells the source carries, whether a cell's text
//! holds the delimiters XML reserves or characters outside ASCII, whether the cells are quoted
//! or ragged, and whether the key is one the handler's own guard admits; for the failures,
//! which Python class the run ends on and whether the interpreter raised it or the handler
//! did; for the layer read, how the archive arrived and how large it is. A further test drives
//! the SAME work asynchronously, where nothing waits for the run, and converges on the same
//! object.
//!
//! What this suite does NOT re-assert, because a sibling suite owns it: which directory of the
//! layer root an entry is importable from (`python_layer_import_path.rs`), which modules the
//! bundled SDK and the standard library expose (`python_runtime_bundled_sdk.rs`,
//! `python_runtime_os_surface.rs`), that a publish from an Amazon S3 location is refused when
//! the object is not there (`s3_package_source.rs`, whose Lambda-beside-S3 harness this suite
//! reuses), and which members a layer version's `Content` carries for each content source
//! (`layer_content_identity.rs`).
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{
    create_function, json_call, make_activation_due, send, Answer, ACCOUNT, REGION, ROLE,
};
use crate::function_lifecycle::zip_file;
use crate::invoke_request_id::{is_host_bound, one_invocation_stream_at_a_time};
use crate::node_invoke_run::package;
use crate::s3_package_source::{
    digest_of, emulator, publish_layer_version, s3_ok, s3_request,
};

/// The bucket every test of this suite stages its objects in. Each test builds an emulator of
/// its own, so the name is shared and the state is not.
const BUCKET: &str = "layer-backed-pipeline";

/// The prefix the handler's own guard admits a source key under.
const RAW_PREFIX: &str = "raw/";

/// The prefix the handler writes its report under, which no source key carries.
const OUTPUT_PREFIX: &str = "processed/";

/// The `Runtime` every function here is configured with, and the one the layers declare.
const RUNTIME: &str = "python3.12";

/// The `Timeout` every function here carries, in seconds. Sized the way the sibling suites
/// that RUN handlers size theirs: far past what any of these handlers needs on any host,
/// because what is under test is WHAT a run answered and not how fast a loaded host gets
/// through it.
const FUNCTION_TIMEOUT_SECONDS: i64 = 60;

/// How many times an invocation is re-driven while the answer reports one of this test
/// binary's own HOST BOUNDS instead of a run of the handler, and how long one of those waits
/// before the next. The handlers here convert a few CSV rows, so no row of any table can ask
/// for an answer like that: it belongs to the run slots and the process memory bound every
/// suite of this target shares ([`is_host_bound`]).
const HOST_BOUND_ATTEMPTS: usize = 6;
const BEFORE_RETRYING: Duration = Duration::from_millis(250);

/// The header `Invoke`'s `InvocationType` is bound to (`httpHeader: X-Amz-Invocation-Type`),
/// and the two values this suite drives: the one that RUNS the function and answers it, and
/// the one that takes the event and answers before the run.
const INVOCATION_TYPE_HEADER: &str = "x-amz-invocation-type";
const REQUEST_RESPONSE: &str = "RequestResponse";
const EVENT: &str = "Event";

/// The failure signal a synchronous invocation reports a faulted run under
/// (`Invoke$FunctionError`, `httpHeader: X-Amz-Function-Error`), and its value for a run that
/// ended on an error the function did not handle.
const FUNCTION_ERROR_HEADER: &str = "x-amz-function-error";
const UNHANDLED: &str = "Unhandled";

/// The `errorType` a runtime reports when it cannot import the module the configured `Handler`
/// names. Named here only so a row can state that a handler's own failure is NEVER reported as
/// one: a module that imports the SDK or the standard library loads.
const IMPORT_MODULE_ERROR: &str = "Runtime.ImportModuleError";

/// How many fields a `stat` structure carries as a SEQUENCE, which is what the standard
/// library's `os.stat_result` reports as `n_sequence_fields`: the ten fields `st_mode`,
/// `st_ino`, `st_dev`, `st_nlink`, `st_uid`, `st_gid`, `st_size`, `st_atime`, `st_mtime` and
/// `st_ctime`.
const STAT_SEQUENCE_FIELDS: i64 = 10;

/// Where a layer's version-independent import directory is mounted in the execution
/// environment, which is the path a module shipped under the archive's `python/` holds.
const LAYER_IMPORT_ROOT: &str = "/opt/python";

/// How long the asynchronous leg waits for the object its run writes to appear, and how long
/// one look waits before the next. The window is generous on purpose: what is under test is
/// that the run CONVERGES on the object, not how fast this host gets there, and the queued run
/// waits for a run slot every suite of this target shares.
const CONVERGES_WITHIN: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The module the LAYER ships, built on the standard library alone: its imports reach the text,
/// path, archive and file-metadata machinery a conversion needs, so a runtime missing any of
/// them cannot even load it.
const SUPPORT_SOURCE: &str = r#"
import csv
import glob
import io
import os
import pathlib
import shutil
import zipfile
import xml.sax.saxutils as saxutils

# Read at IMPORT time, the way `shutil` reads the same type as it is imported: a core type of
# the standard library's own `os` module.
STAT_FIELDS = os.stat_result.n_sequence_fields

# Two more names the standard library's file machinery reaches through, so a module missing
# either one fails here rather than silently.
HAS_COPYSTAT = hasattr(shutil, "copystat")
READ_ONLY = os.O_RDONLY
HAS_GLOB = hasattr(glob, "glob")


def output_key(prefix, source_key, extension):
    """Where the report for `source_key` goes: `prefix`, the source's BASE NAME, `extension`."""
    base = os.path.basename(str(pathlib.PurePosixPath(source_key)))
    return prefix + base.rsplit(".", 1)[0] + extension


def rows_of(text):
    """The CSV `text` as rows, read with the standard library's own reader."""
    return [row for row in csv.reader(io.StringIO(text)) if row]


def report(rows):
    """`rows` as an XML document, every cell escaped as the character data it is."""
    body = "".join(
        "<row>%s</row>" % "".join("<c>%s</c>" % saxutils.escape(cell) for cell in row)
        for row in rows
    )
    return '<?xml version="1.0" encoding="UTF-8"?><report>%s</report>' % body


def packaged(document):
    """`document` written into an in-memory archive and read back: the entries it holds."""
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED) as archive:
        archive.writestr("report.xml", document)
    with zipfile.ZipFile(io.BytesIO(buffer.getvalue())) as written:
        return sorted(written.namelist())
"#;

/// The handler the pipeline tables run: its module imports the SDK the runtime bundles and the
/// module its LAYER ships, and its body reads the source object, converts it and writes the
/// report back — reporting what it wrote, what the layer resolved to, and the refusal its own
/// read of an absent key came to.
const PIPELINE_SOURCE: &str = r#"
import boto3
import botocore.exceptions

import pipeline_support

CLIENT = boto3.client("s3")


def lambda_handler(event, context):
    bucket = event["bucket"]
    source = event["source_key"]
    # The function's own guard, which is the handler's answer rather than a failed import.
    if not source.startswith(event["raw_prefix"]):
        return {"refused": source}

    read = CLIENT.get_object(Bucket=bucket, Key=source)
    text = read["Body"].read().decode("utf-8")
    rows = pipeline_support.rows_of(text)
    document = pipeline_support.report(rows)
    key = pipeline_support.output_key(event["output_prefix"], source, ".xml")
    CLIENT.put_object(
        Bucket=bucket,
        Key=key,
        Body=document.encode("utf-8"),
        ContentType="application/xml",
    )
    try:
        CLIENT.get_object(Bucket=bucket, Key=source + ".absent")
    except botocore.exceptions.ClientError as error:
        absent = error.response["Error"]["Code"]
    else:
        absent = None
    return {
        "written": [key],
        "rows": len(rows),
        "support": pipeline_support.__file__,
        "stat_fields": pipeline_support.STAT_FIELDS,
        "copystat": pipeline_support.HAS_COPYSTAT,
        "glob": pipeline_support.HAS_GLOB,
        "entries": pipeline_support.packaged(document),
        "absent_source": absent,
    }
"#;

/// The handler the failure table runs: its module imports the SDK and the standard library, so
/// a run that ends in an error ends in the handler's OWN error — the class the interpreter
/// raised for it, or the one the handler raised itself.
const FAILING_SOURCE: &str = r#"
import boto3
import shutil


class ReportMissing(Exception):
    """A class this module itself defines, raised like any other."""


def lambda_handler(event, context):
    # The module-scope imports are reached before anything else, so a run can only get here
    # on a runtime that has both.
    reached = {"client": boto3.client("s3") is not None, "copy": hasattr(shutil, "copystat")}
    asked = event["raise"]
    if asked == "KeyError":
        # Raised by the INTERPRETER, not by this handler: the class is the lookup's own.
        return reached[event["member"]]
    if asked == "ValueError":
        raise ValueError(event["detail"])
    if asked == "TypeError":
        raise TypeError(event["detail"])
    if asked == "ReportMissing":
        raise ReportMissing(event["detail"])
    return {"reached": reached}
"#;

/// The characters an ARN carries that a query string would otherwise read as structure, as
/// the set to percent-encode: everything but the unreserved characters, through the crate this
/// service already percent-codes a bound URI with.
const ARN_RESERVED: &::percent_encoding::AsciiSet = &::percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// `GetLayerVersionByArn`, whose `Arn` member is bound to the `Arn` query parameter of
/// `GET /2018-10-31/layers?find=LayerVersion`.
async fn get_layer_version_by_arn(router: &MultiServiceRouter, arn: &str) -> Answer {
    let encoded = ::percent_encoding::utf8_percent_encode(arn, ARN_RESERVED);
    let uri = format!("/2018-10-31/layers?find=LayerVersion&Arn={encoded}");
    json_call(router, "GET", &uri, &Value::Null).await
}

/// The ARN version `version` of a layer of `name` carries.
fn layer_version_arn(name: &str, version: i64) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:layer:{name}:{version}")
}

/// The body text of `answer`, for a failure message.
fn text(answer: &Answer) -> String {
    String::from_utf8_lossy(&answer.body).to_string()
}

/// Create the suite's bucket through Amazon S3's own `CreateBucket`.
async fn create_bucket(router: &MultiServiceRouter, case: &str) {
    s3_ok(router, case, "PUT", &format!("/{BUCKET}"), &[]).await;
}

/// Publish the support layer out of an Amazon S3 OBJECT — the route a deploy takes when the
/// archive is already staged — holding the publish to the digest this test computes itself,
/// and answer the published version's ARN.
async fn publish_support_layer(
    router: &MultiServiceRouter,
    case: &str,
    layer: &str,
    key: &str,
) -> String {
    let archive = package(&[("python/pipeline_support.py", SUPPORT_SOURCE)]);
    s3_ok(router, case, "PUT", &format!("/{BUCKET}/{key}"), &archive).await;
    let published = publish_layer_version(
        router,
        layer,
        &json!({
            "Content": { "S3Bucket": BUCKET, "S3Key": key },
            "CompatibleRuntimes": [RUNTIME],
        }),
    )
    .await;
    assert_eq!(
        published.status,
        201,
        "case {case:?}: expected PublishLayerVersion to answer 201, got {} {}",
        published.status,
        text(&published)
    );
    // The deploy phase reads the digest off the publish before it attaches the layer, so a
    // publish that reported no digest never gets as far as a function.
    let content = published.json()["Content"].clone();
    let expected = digest_of(&archive);
    assert_eq!(
        content.get("CodeSha256").and_then(Value::as_str),
        Some(expected.as_str()),
        "case {case:?}: expected the published archive's digest {expected:?} in {content}"
    );
    published.json()["LayerVersionArn"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| panic!("case {case:?}: expected a LayerVersionArn in {content}"))
}

/// `CreateFunction` for a `python3.12` function holding `entries`, carrying the layers
/// `layers` names, and bring its activation forward: the create answers it `Pending`, and the
/// transition is owed by the CLOCK rather than waited out.
async fn create_pipeline_function(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    name: &str,
    entries: &[(&str, &str)],
    layers: &[String],
) {
    let created = create_function(
        router,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": RUNTIME,
            "Handler": "lambda_function.lambda_handler",
            "Timeout": FUNCTION_TIMEOUT_SECONDS,
            "Layers": layers,
            "Code": { "ZipFile": zip_file(&package(entries)) },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: expected CreateFunction to answer 201, got {} {}",
        created.status,
        text(&created)
    );
    make_activation_due(engine);
}

/// `Invoke` on `name` with the invocation type `invocation_type`, re-driven while the answer
/// reports one of this test binary's own host bounds instead of a run of the handler: this
/// target runs its suites in parallel over the run slots and the process memory bound every
/// one of them shares, so a trivial run can be refused or stopped for what another suite
/// holds. The last answer is returned whatever it is, so a run that really failed still fails
/// the row.
async fn invoke(
    router: &MultiServiceRouter,
    name: &str,
    invocation_type: &str,
    event: &Value,
) -> Answer {
    let mut answered = invoke_once(router, name, invocation_type, event).await;
    for _ in 1..HOST_BOUND_ATTEMPTS {
        if !is_host_bound(answered.status, &text(&answered)) {
            break;
        }
        tokio::time::sleep(BEFORE_RETRYING).await;
        answered = invoke_once(router, name, invocation_type, event).await;
    }
    answered
}

/// One `Invoke` request, as it came back.
async fn invoke_once(
    router: &MultiServiceRouter,
    name: &str,
    invocation_type: &str,
    event: &Value,
) -> Answer {
    send(
        router,
        "POST",
        &format!("/2015-03-31/functions/{name}/invocations"),
        "application/json",
        serde_json::to_vec(event).expect("the event serializes"),
        &[(INVOCATION_TYPE_HEADER, invocation_type.to_string())],
    )
    .await
}

/// The cell text as XML character data: the three delimiters the format reserves, escaped the
/// way the standard library's own escaper escapes them. Computed here so the test never reads
/// the escaping it is checking out of the answer it is checking.
fn escaped(cell: &str) -> String {
    cell.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The report document `cells` must come to.
fn expected_report(cells: &[&[&str]]) -> String {
    let body: String = cells
        .iter()
        .map(|row| {
            let inner: String = row
                .iter()
                .map(|cell| format!("<c>{}</c>", escaped(cell)))
                .collect();
            format!("<row>{inner}</row>")
        })
        .collect();
    format!(r#"<?xml version="1.0" encoding="UTF-8"?><report>{body}</report>"#)
}

/// One source object the pipeline converts.
struct Row {
    case: &'static str,
    /// The key the source object is staged under.
    source_key: &'static str,
    /// The source object's own bytes, as the CSV text they are.
    csv: &'static str,
    /// The rows the standard library's reader must read out of that text, written out here so
    /// the expectation is independent of the reader under test.
    cells: &'static [&'static [&'static str]],
    /// The key the report must land under: the output prefix and the source's base name.
    output_key: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a source at the prefix root, two plain rows",
            source_key: "raw/orders.csv",
            csv: "id,note\n1,plain\n",
            cells: &[&["id", "note"], &["1", "plain"]],
            output_key: "processed/orders.xml",
        },
        Row {
            case: "a deeply nested source key whose cells hold the delimiters XML reserves",
            source_key: "raw/2026/07/15/daily-report.csv",
            csv: "id,note\n1,a & b < c > d\n2,<tag>\n",
            cells: &[
                &["id", "note"],
                &["1", "a & b < c > d"],
                &["2", "<tag>"],
            ],
            output_key: "processed/daily-report.xml",
        },
        Row {
            case: "a source key carrying dots, with a quoted cell and text outside ASCII",
            source_key: "raw/archive.v2.summary.csv",
            // The quoted cell holds the delimiter the format separates on, which is what
            // makes the reader's own quoting part of the row.
            csv: "id,note\n1,\"café, crème & more\"\n",
            cells: &[&["id", "note"], &["1", "café, crème & more"]],
            output_key: "processed/archive.v2.summary.xml",
        },
        Row {
            case: "a ragged source whose rows carry different cell counts",
            source_key: "raw/ragged.csv",
            csv: "a,b,c\n1\n2,3\n",
            cells: &[&["a", "b", "c"], &["1"], &["2", "3"]],
            output_key: "processed/ragged.xml",
        },
    ]
}

/// RULE: a `python3.x` handler whose module imports the layer it carries, the SDK the runtime
/// bundles and the standard library RUNS, and the invocation answers its own value — proved
/// against the objects Amazon S3 itself holds afterwards.
#[tokio::test]
async fn a_layer_backed_handler_converts_the_object_and_reports_its_own_value() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator(&engine);
    create_bucket(&router, "the pipeline's bucket").await;

    let layer = publish_support_layer(
        &router,
        "the pipeline's support layer",
        "pipeline-support",
        "layers/pipeline-support.zip",
    )
    .await;
    let function = "layer-backed-pipeline";
    create_pipeline_function(
        &router,
        &engine,
        "the pipeline's function",
        function,
        &[("lambda_function.py", PIPELINE_SOURCE)],
        &[layer],
    )
    .await;

    for row in rows() {
        let case = row.case;
        s3_ok(
            &router,
            case,
            "PUT",
            &format!("/{BUCKET}/{}", row.source_key),
            row.csv.as_bytes(),
        )
        .await;

        let answered = invoke(
            &router,
            function,
            REQUEST_RESPONSE,
            &json!({
                "bucket": BUCKET,
                "source_key": row.source_key,
                "raw_prefix": RAW_PREFIX,
                "output_prefix": OUTPUT_PREFIX,
            }),
        )
        .await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
            answered.status,
            text(&answered)
        );
        let signal = answered.header(FUNCTION_ERROR_HEADER);
        assert_eq!(
            signal, None,
            "case {case:?}: expected no function error, got {signal:?} with {}",
            text(&answered)
        );
        let payload = answered.json();
        let expected = json!({
            "written": [row.output_key],
            "rows": row.cells.len(),
            // The layer's module is resolved from the LAYER root, not from the function's own
            // package: the file it reports says which mount it came off.
            "support": format!("{LAYER_IMPORT_ROOT}/pipeline_support.py"),
            "stat_fields": STAT_SEQUENCE_FIELDS,
            "copystat": true,
            "glob": true,
            "entries": ["report.xml"],
            // The handler's own read of a key the bucket holds nothing under: the service's
            // own error code reaching the guest.
            "absent_source": "NoSuchKey",
        });
        assert_eq!(
            payload, expected,
            "case {case:?}: expected the handler's own report {expected}, got {payload}"
        );

        // ... and the object the run wrote is the object Amazon S3 holds, with the cells the
        // reader read and the escaping the document needs.
        let stored = s3_request(&router, "GET", &format!("/{BUCKET}/{}", row.output_key), &[]).await;
        assert_eq!(
            stored.status,
            200,
            "case {case:?}: expected Amazon S3 to serve {}, got {} {}",
            row.output_key,
            stored.status,
            text(&stored)
        );
        let document = String::from_utf8_lossy(&stored.body).to_string();
        let wanted = expected_report(row.cells);
        assert_eq!(
            document, wanted,
            "case {case:?}: expected the report {wanted:?}, got {document:?}"
        );
    }

    // A key the handler's own guard does not admit is the HANDLER's answer: the run completed
    // and refused the key, rather than failing before its body ever ran.
    let outside = "staging/elsewhere.csv";
    let answered = invoke(
        &router,
        function,
        REQUEST_RESPONSE,
        &json!({
            "bucket": BUCKET,
            "source_key": outside,
            "raw_prefix": RAW_PREFIX,
            "output_prefix": OUTPUT_PREFIX,
        }),
    )
    .await;
    let case = "a source key outside the prefix the guard admits";
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
        answered.status,
        text(&answered)
    );
    assert_eq!(
        answered.header(FUNCTION_ERROR_HEADER),
        None,
        "case {case:?}: expected no function error, got {}",
        text(&answered)
    );
    let payload = answered.json();
    assert_eq!(
        payload,
        json!({ "refused": outside }),
        "case {case:?}: expected the guard's own refusal, got {payload}"
    );
    // Nothing was written for a key the guard refused.
    let absent = s3_request(
        &router,
        "GET",
        &format!("/{BUCKET}/{OUTPUT_PREFIX}elsewhere.xml"),
        &[],
    )
    .await;
    assert_eq!(
        absent.status,
        404,
        "case {case:?}: expected no report for a refused key, got {} {}",
        absent.status,
        text(&absent)
    );
}

/// One run that ends in a failure the function did not handle.
struct Failure {
    case: &'static str,
    /// What the event asks the handler to do.
    asked: &'static str,
    /// The `errorType` the error document must carry: the Python class the run ended on.
    class: &'static str,
    /// The `errorMessage` it must carry: the raised exception's own `str`.
    message: &'static str,
}

fn failures() -> Vec<Failure> {
    vec![
        Failure {
            // The interpreter's own class for a mapping lookup that found nothing, which is
            // what a handler indexing a key it does not hold ends on.
            case: "a lookup the interpreter raised for",
            asked: "KeyError",
            class: "KeyError",
            message: "'absent'",
        },
        Failure {
            case: "a value the handler rejected itself",
            asked: "ValueError",
            class: "ValueError",
            message: "the report holds no rows",
        },
        Failure {
            case: "a type the handler rejected itself",
            asked: "TypeError",
            class: "TypeError",
            message: "the report is no mapping",
        },
        Failure {
            case: "a class the handler's own module defines",
            asked: "ReportMissing",
            class: "ReportMissing",
            message: "the report is not there",
        },
    ]
}

/// RULE (the same import-path rule, at the failing end): a handler whose module imports the
/// SDK and the standard library runs, so a run that ends in an error reports the HANDLER's own
/// Python class and message — never a runtime import error.
#[tokio::test]
async fn a_handler_failure_is_reported_as_its_own_python_class() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator(&engine);
    create_bucket(&router, "the failing function's bucket").await;

    let function = "python-failure-classes";
    create_pipeline_function(
        &router,
        &engine,
        "the failing function",
        function,
        &[("lambda_function.py", FAILING_SOURCE)],
        &[],
    )
    .await;

    // The same module, asked for nothing, returns: the imports its failures are reported
    // through are the imports a served run reaches.
    let served = invoke(
        &router,
        function,
        REQUEST_RESPONSE,
        &json!({ "raise": "nothing", "member": "client", "detail": "" }),
    )
    .await;
    let case = "the module's imports, reached by a run that returns";
    assert_eq!(
        served.status,
        200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
        served.status,
        text(&served)
    );
    assert_eq!(
        served.header(FUNCTION_ERROR_HEADER),
        None,
        "case {case:?}: expected no function error, got {}",
        text(&served)
    );
    let payload = served.json();
    assert_eq!(
        payload,
        json!({ "reached": { "client": true, "copy": true } }),
        "case {case:?}: expected the SDK client and the standard library's copier, got {payload}"
    );

    for failure in failures() {
        let case = failure.case;
        let answered = invoke(
            &router,
            function,
            REQUEST_RESPONSE,
            &json!({
                "raise": failure.asked,
                "member": "absent",
                "detail": failure.message,
            }),
        )
        .await;
        // A faulted run is still a terminal answer: the status is the operation's own.
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
            answered.status,
            text(&answered)
        );
        assert_eq!(
            answered.header(FUNCTION_ERROR_HEADER).as_deref(),
            Some(UNHANDLED),
            "case {case:?}: expected the function-error signal of a handler that raised, got \
             {:?} with {}",
            answered.header(FUNCTION_ERROR_HEADER),
            text(&answered)
        );
        let document = answered.json();
        let reported = document.get("errorType").and_then(Value::as_str);
        assert_ne!(
            reported,
            Some(IMPORT_MODULE_ERROR),
            "case {case:?}: the module imports the SDK and the standard library, so the run \
             must not report an import failure, got {document}"
        );
        assert_eq!(
            reported,
            Some(failure.class),
            "case {case:?}: expected the class the run ended on {:?}, got {document}",
            failure.class
        );
        assert_eq!(
            document.get("errorMessage").and_then(Value::as_str),
            Some(failure.message),
            "case {case:?}: expected the raised message {:?}, got {document}",
            failure.message
        );
        assert!(
            document
                .get("stackTrace")
                .is_some_and(|frames| frames.as_array().is_some_and(|frames| !frames.is_empty())),
            "case {case:?}: expected the run's own frames, got {document}"
        );
    }
}

/// RULE (the pipeline rule where nothing waits for the run): an asynchronous invocation is
/// taken for later and answers 202 with an empty payload, and the run it queued does the SAME
/// work — it converges on the object a synchronous invocation of the same source produces.
#[tokio::test]
async fn an_asynchronous_invocation_converges_on_the_same_object() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator(&engine);
    create_bucket(&router, "the asynchronous bucket").await;

    let layer = publish_support_layer(
        &router,
        "the asynchronous support layer",
        "pipeline-support-async",
        "layers/pipeline-support-async.zip",
    )
    .await;
    let function = "layer-backed-pipeline-async";
    create_pipeline_function(
        &router,
        &engine,
        "the asynchronous function",
        function,
        &[("lambda_function.py", PIPELINE_SOURCE)],
        &[layer],
    )
    .await;

    // One source text, staged twice: the two runs differ only in HOW they were asked for.
    let csv = "id,note\n1,a & b\n";
    let cells: &[&[&str]] = &[&["id", "note"], &["1", "a & b"]];
    for key in ["raw/sync-source.csv", "raw/async-source.csv"] {
        s3_ok(
            &router,
            "the converging sources",
            "PUT",
            &format!("/{BUCKET}/{key}"),
            csv.as_bytes(),
        )
        .await;
    }

    let event = |key: &str| {
        json!({
            "bucket": BUCKET,
            "source_key": key,
            "raw_prefix": RAW_PREFIX,
            "output_prefix": OUTPUT_PREFIX,
        })
    };

    let case = "a synchronous invocation of the same work";
    let answered = invoke(
        &router,
        function,
        REQUEST_RESPONSE,
        &event("raw/sync-source.csv"),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
        answered.status,
        text(&answered)
    );
    assert_eq!(
        answered.json()["written"],
        json!([format!("{OUTPUT_PREFIX}sync-source.xml")]),
        "case {case:?}: expected the report's key, got {}",
        text(&answered)
    );

    let case = "an asynchronous invocation";
    let accepted = invoke(&router, function, EVENT, &event("raw/async-source.csv")).await;
    assert_eq!(
        accepted.status,
        202,
        "case {case:?}: expected HTTP 202 from an Event invocation, got {} {}",
        accepted.status,
        text(&accepted)
    );
    assert!(
        accepted.body.is_empty(),
        "case {case:?}: expected an empty payload, got {}",
        text(&accepted)
    );

    // Nothing answers for a queued run, so the object it writes is waited FOR rather than
    // waited OUT: look again until it is there or the window closes.
    let key = format!("{OUTPUT_PREFIX}async-source.xml");
    let deadline = Instant::now() + CONVERGES_WITHIN;
    let document = loop {
        let stored = s3_request(&router, "GET", &format!("/{BUCKET}/{key}"), &[]).await;
        if stored.status == 200 {
            break String::from_utf8_lossy(&stored.body).to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the queued run to write {key}, last answer {} {}",
            stored.status,
            text(&stored)
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    };

    let wanted = expected_report(cells);
    assert_eq!(
        document, wanted,
        "case {case:?}: expected the queued run to converge on {wanted:?}, got {document:?}"
    );
    // The synchronous run's own object is the same document, so the two routes converge.
    let sync = s3_request(
        &router,
        "GET",
        &format!("/{BUCKET}/{OUTPUT_PREFIX}sync-source.xml"),
        &[],
    )
    .await;
    assert_eq!(
        String::from_utf8_lossy(&sync.body).to_string(),
        document,
        "case {case:?}: expected both routes to write one document"
    );
}

/// One layer version under test, and where its archive came from.
struct Content {
    case: &'static str,
    /// The layer the version is published under.
    layer: &'static str,
    /// The key the archive is staged under, or `None` for an archive uploaded inline.
    key: Option<&'static str>,
    /// How many bytes the archive's single entry holds, which is what makes each row its own
    /// byte string.
    length: usize,
}

fn contents() -> Vec<Content> {
    vec![
        Content {
            case: "an archive uploaded inline",
            layer: "content-inline",
            key: None,
            length: 128,
        },
        Content {
            case: "an Amazon S3 object",
            layer: "content-from-object",
            key: Some("layers/helpers.zip"),
            length: 1_024,
        },
        Content {
            case: "an Amazon S3 object of another size, under a nested key",
            layer: "content-from-nested-object",
            key: Some("layers/2026/07/helpers-v2.zip"),
            length: 8_192,
        },
    ]
}

/// RULE: a layer version's `Content` reports the archive Lambda holds — `Location`, `CodeSize`
/// and the `CodeSha256` of the stored bytes — on the publish's own answer and on EVERY read of
/// that version, so the read that addresses a version BY ITS ARN reports it too.
///
/// `layer_content_identity.rs` owns the same rule over the table of content sources on the
/// publish and on `GetLayerVersion`; this test is the `GetLayerVersionByArn` surface, which
/// reads the same stored version through the other binding.
#[tokio::test]
async fn a_layer_version_read_by_its_arn_reports_the_archives_digest() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator(&engine);
    create_bucket(&router, "the content table's bucket").await;

    for row in contents() {
        let case = row.case;
        // An entry of the row's own length, so no two rows share an archive.
        let source = format!("VALUE = \"{}\"\n", "x".repeat(row.length));
        let archive = package(&[("python/helpers.py", source.as_str())]);
        let content = match row.key {
            None => json!({ "ZipFile": zip_file(&archive) }),
            Some(key) => {
                s3_ok(&router, case, "PUT", &format!("/{BUCKET}/{key}"), &archive).await;
                json!({ "S3Bucket": BUCKET, "S3Key": key })
            }
        };

        let published = publish_layer_version(
            &router,
            row.layer,
            &json!({ "Content": content, "CompatibleRuntimes": [RUNTIME] }),
        )
        .await;
        assert_eq!(
            published.status,
            201,
            "case {case:?}: expected PublishLayerVersion to answer 201, got {} {}",
            published.status,
            text(&published)
        );
        let arn = layer_version_arn(row.layer, 1);
        let surfaces = [
            ("the publish's own answer", published),
            (
                "GetLayerVersionByArn",
                get_layer_version_by_arn(&router, &arn).await,
            ),
        ];

        let expected_digest = digest_of(&archive);
        let expected_size = i64::try_from(archive.len()).unwrap_or(i64::MAX);
        for (surface, answer) in &surfaces {
            assert!(
                (200..300).contains(&answer.status),
                "case {case:?} on {surface}: the read must succeed, got {} {}",
                answer.status,
                text(answer)
            );
            let content = answer.json()["Content"].clone();
            let digest = content.get("CodeSha256").and_then(Value::as_str);
            assert_eq!(
                digest,
                Some(expected_digest.as_str()),
                "case {case:?} on {surface}: expected CodeSha256 {expected_digest:?}, got \
                 {content}"
            );
            let size = content.get("CodeSize").and_then(Value::as_i64);
            assert_eq!(
                size,
                Some(expected_size),
                "case {case:?} on {surface}: expected CodeSize {expected_size}, got {content}"
            );
            let location = content.get("Location").and_then(Value::as_str);
            assert!(
                location.is_some_and(|location| !location.is_empty()),
                "case {case:?} on {surface}: expected an announced Location, got {content}"
            );
        }
    }
}
