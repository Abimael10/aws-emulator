// SPDX-License-Identifier: CC0-1.0

//! What a managed `python3.x` runtime PREINSTALLS beside the AWS SDK it ships, end to end
//! over the wire: Lambda, IAM, the token service and Amazon S3 on one engine, functions whose
//! handlers import the SDK's own dependency distributions and run a managed transfer, and the
//! objects read back where a client reads them.
//!
//! RULE: a managed Python runtime ships the AWS SDK for Python as an INSTALLATION, so every
//! distribution that installation requires — not only the SDK's two top-level packages — sits
//! on the handler's import path in the runtime's own directory, behind the deployment package
//! and behind the function's layers. A deployment package that bundles part of the SDK
//! therefore still imports: its own copy wins over the runtime's, while a dependency it did
//! not bundle resolves from the runtime behind it.
//!
//! The rows differ along what the rule generalises over: WHICH distribution of the
//! installation is imported and whether it is the package or one of its modules, whether the
//! deployment package bundles a copy of the SDK itself, WHICH managed transfer is run
//! (upload, download, copy, delete), the FORM the object is given in (a file of the
//! environment or a stream in memory), the SIZE class of the object (under the configured
//! threshold, so one request, or over it, so several parts) and whether the call is served or
//! refused. The last assertions read every object back through Amazon S3's OWN API, so a
//! transfer that answered without moving any bytes fails here.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::given_role;
use crate::event_source_mappings::{await_function_active, call, router_on};

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The bucket the managed transfers of this suite run against.
const BUCKET: &str = "runtime-sdk-dependency-transfers";

/// The directory the runtime's own modules are resolved from, which is what a module of the
/// installation reports as its file.
const RUNTIME_ROOT: &str = "/var/runtime";

/// The directory the deployment package is mounted at, which is what a module the package
/// bundles itself reports.
const TASK_ROOT: &str = "/var/task";

/// Lambda, IAM, the token service and Amazon S3 over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: a handler's own calls are dispatched back into this
/// same router, so the objects it transfers are the objects this suite reads back.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router
}

/// One request to Amazon S3, made the way a client makes one: its protocol is `restXml`, so
/// the request is a method and a path and the answer is a document.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/s3/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(bytes::Bytes::from(body.to_vec()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// The modules of the installation a handler imports, one row each: each distribution's own
/// package, and the modules of the managed transfer library the SDK's S3 customizations
/// import by name.
///
/// Source: the imports `boto3.s3.transfer` of the published SDK makes
/// (`s3transfer.manager`, `s3transfer.futures`, `s3transfer.subscribers`,
/// `s3transfer.utils`, `s3transfer.exceptions`), beside the packages the SDK itself is.
const INSTALLED_MODULES: &[(&str, &str)] = &[
    ("boto3", "boto3/__init__.py"),
    ("botocore", "botocore/__init__.py"),
    ("urllib3", "urllib3/__init__.py"),
    ("s3transfer", "s3transfer/__init__.py"),
    ("s3transfer.manager", "s3transfer/manager.py"),
    ("s3transfer.futures", "s3transfer/futures.py"),
    ("s3transfer.subscribers", "s3transfer/subscribers.py"),
    ("s3transfer.utils", "s3transfer/utils.py"),
    ("s3transfer.exceptions", "s3transfer/exceptions.py"),
    ("s3transfer.compat", "s3transfer/compat.py"),
    ("s3transfer.constants", "s3transfer/constants.py"),
    ("jmespath", "jmespath/__init__.py"),
    ("jmespath.parser", "jmespath/parser.py"),
    ("jmespath.lexer", "jmespath/lexer.py"),
    ("jmespath.visitor", "jmespath/visitor.py"),
    ("jmespath.functions", "jmespath/functions.py"),
    ("jmespath.ast", "jmespath/ast.py"),
    ("jmespath.exceptions", "jmespath/exceptions.py"),
    ("dateutil", "dateutil/__init__.py"),
    ("dateutil.tz", "dateutil/tz/__init__.py"),
    ("dateutil.tz.tz", "dateutil/tz/tz.py"),
    ("dateutil.parser", "dateutil/parser/__init__.py"),
    ("dateutil.parser._parser", "dateutil/parser/_parser.py"),
    ("dateutil.relativedelta", "dateutil/relativedelta.py"),
    ("dateutil.utils", "dateutil/utils.py"),
];

/// The names the published SDK's own S3 customizations import out of the transfer library,
/// one row each.
///
/// Source: the `from s3transfer... import ...` lines of `boto3/s3/transfer.py`.
const IMPORTED_NAMES: &[&str] = &[
    "s3transfer.exceptions:CancelledError",
    "s3transfer.exceptions:RetriesExceededError",
    "s3transfer.exceptions:S3UploadFailedError",
    "s3transfer.futures:NonThreadedExecutor",
    "s3transfer.manager:TransferConfig",
    "s3transfer.manager:TransferManager",
    "s3transfer.subscribers:BaseSubscriber",
    "s3transfer.utils:OSUtils",
    // The names the SDK's own paginators, waiters and resource references reach the query
    // language through.
    //
    // Source: the `jmespath.compile`, `jmespath.search` and `jmespath.exceptions` uses of
    // `botocore/paginate.py`, `botocore/waiter.py` and `boto3/resources/response.py`.
    "jmespath:compile",
    "jmespath:search",
    "jmespath:Options",
    "jmespath.exceptions:JMESPathError",
    "jmespath.exceptions:JMESPathTypeError",
    "jmespath.exceptions:UnknownFunctionError",
    "jmespath.exceptions:EmptyExpressionError",
    "jmespath.functions:Functions",
    "jmespath.functions:signature",
    // The names every timestamp of the wire is read and carried with.
    //
    // Source: the `from dateutil.parser import parse` and `from dateutil.tz import tzlocal,
    // tzutc` lines of `botocore/utils.py`.
    "dateutil.parser:parse",
    "dateutil.parser:isoparse",
    "dateutil.parser:ParserError",
    "dateutil.tz:tzutc",
    "dateutil.tz:tzlocal",
    "dateutil.tz:tzoffset",
    "dateutil.tz:gettz",
    "dateutil.relativedelta:relativedelta",
];

/// One row per FORM a timestamp of the wire is written in, and the datetime it states.
///
/// Source: the ISO 8601 forms AWS's protocols carry a timestamp in, the HTTP date of RFC 9110
/// and the two older forms it names (the RFC 850 date and the ASCII `asctime` one), the
/// date-only form, and a text that is no timestamp at all.
const TIMESTAMPS: &[(&str, &str)] = &[
    ("2024-01-02T03:04:05Z", "2024-01-02T03:04:05+00:00"),
    (
        "2024-01-02T03:04:05.123456+02:00",
        "2024-01-02T03:04:05.123456+02:00",
    ),
    ("2024-01-02T03:04:05-05:00", "2024-01-02T03:04:05-05:00"),
    ("2024-01-02T03:04:05", "2024-01-02T03:04:05"),
    (
        "Tue, 02 Jan 2024 03:04:05 GMT",
        "2024-01-02T03:04:05+00:00",
    ),
    (
        "Tuesday, 02-Jan-24 03:04:05 GMT",
        "2024-01-02T03:04:05+00:00",
    ),
    ("Tue Jan 02 03:04:05 2024", "2024-01-02T03:04:05"),
    ("2024-01-02", "2024-01-02T00:00:00"),
    ("20240102T030405Z", "2024-01-02T03:04:05+00:00"),
];

/// The document the query rows below are read against: a paginated answer of the shape the
/// SDK's own pagination and waiter data is written for.
fn queried_document() -> Value {
    json!({
        "Contents": [
            { "Key": "a.txt", "Size": 3, "Owner": { "ID": "one" } },
            { "Key": "b/c.txt", "Size": 10, "Owner": { "ID": "two" } },
            { "Key": "d.txt", "Size": 7, "Owner": { "ID": "one" } },
        ],
        "Reservations": [
            { "Instances": [
                { "InstanceId": "i-1", "State": { "Name": "running" } },
                { "InstanceId": "i-2", "State": { "Name": "stopped" } },
            ] },
            { "Instances": [{ "InstanceId": "i-3", "State": { "Name": "running" } }] },
        ],
        "NextToken": "second-page",
        "IsTruncated": true,
        "KeyCount": 3,
        "weird key": "quoted",
        "Empty": [],
    })
}

/// One row per construct of the query language, read against [`queried_document`].
///
/// Source: the JMESPath specification's own constructs — identifiers (quoted and unquoted),
/// sub-expressions, indices and slices, the projections and the flatten, filters,
/// multi-selects, pipes, the boolean and comparison operators, literals and raw strings, and
/// the functions it defines — beside the expression forms the SDK's own pagination and waiter
/// data is written in (`Contents[].Key`, `Reservations[].Instances[].InstanceId`,
/// `length(Contents[])`).
fn queries() -> Vec<(&'static str, Value)> {
    vec![
        ("NextToken", json!("second-page")),
        ("Contents[0].Key", json!("a.txt")),
        ("Contents[-1].Key", json!("d.txt")),
        ("Contents[].Key", json!(["a.txt", "b/c.txt", "d.txt"])),
        ("Contents[*].Key", json!(["a.txt", "b/c.txt", "d.txt"])),
        ("Contents[1:].Key", json!(["b/c.txt", "d.txt"])),
        ("Contents[0].Owner.ID", json!("one")),
        (
            "Reservations[].Instances[].InstanceId",
            json!(["i-1", "i-2", "i-3"]),
        ),
        ("Contents[?Size > `5`].Key", json!(["b/c.txt", "d.txt"])),
        ("Contents[?Owner.ID=='one'].Key", json!(["a.txt", "d.txt"])),
        ("length(Contents[])", json!(3)),
        ("max_by(Contents, &Size).Key", json!("b/c.txt")),
        ("min_by(Contents, &Size).Key", json!("a.txt")),
        (
            "sort_by(Contents, &Size)[].Key",
            json!(["a.txt", "d.txt", "b/c.txt"]),
        ),
        ("sort(keys(Contents[0]))", json!(["Key", "Owner", "Size"])),
        ("join(', ', Contents[].Key)", json!("a.txt, b/c.txt, d.txt")),
        ("map(&Key, Contents)", json!(["a.txt", "b/c.txt", "d.txt"])),
        ("Contents[].Size | sum(@)", json!(20)),
        ("Contents[].Owner.ID | sort(@)", json!(["one", "one", "two"])),
        ("reverse(Contents[].Key)", json!(["d.txt", "b/c.txt", "a.txt"])),
        ("contains(Contents[].Key, 'd.txt')", json!(true)),
        (
            "Contents[0].{name: Key, bytes: Size}",
            json!({ "name": "a.txt", "bytes": 3 }),
        ),
        ("[NextToken, KeyCount]", json!(["second-page", 3])),
        ("\"weird key\"", json!("quoted")),
        ("'a raw string'", json!("a raw string")),
        ("`{\"literal\": true}`.literal", json!(true)),
        ("NotThere", Value::Null),
        ("NotThere.deeper", Value::Null),
        ("Empty[0]", Value::Null),
        ("not_null(NotThere, NextToken)", json!("second-page")),
        ("IsTruncated && NextToken", json!("second-page")),
        ("NotThere || NextToken", json!("second-page")),
        ("!IsTruncated", json!(false)),
        ("KeyCount == `3`", json!(true)),
        ("KeyCount != `3`", json!(false)),
        ("KeyCount > `10`", json!(false)),
        ("KeyCount >= `3`", json!(true)),
        ("to_string(KeyCount)", json!("3")),
        ("to_number('42')", json!(42)),
        ("type(Contents)", json!("array")),
        ("abs(`-7`)", json!(7)),
        ("avg(Contents[].Size)", json!(6.666666666666667)),
        ("ceil(`1.2`)", json!(2)),
        ("starts_with(Contents[0].Key, 'a')", json!(true)),
        ("merge(Contents[0], {Size: `1`}).Size", json!(1)),
    ]
}

/// One row per expression the language REFUSES, named by the error it raises: an expression
/// that ends mid-way, a function nothing defines, and a function given a type it does not
/// take.
const REFUSED_QUERIES: &[(&str, &str)] = &[
    ("Contents[", "IncompleteExpressionError"),
    ("no_such_function(@)", "UnknownFunctionError"),
    ("length(KeyCount)", "JMESPathTypeError"),
    ("abs('text')", "JMESPathTypeError"),
    ("length(Contents, Contents)", "ArityError"),
    ("Contents..Key", "ParseError"),
    // An expression that ends in the middle of a quoted name, and one that states nothing at
    // all: malformed and truncated input is refused by the library's own error rather than
    // read as something else.
    ("\"unterminated", "LexerError"),
    ("", "EmptyExpressionError"),
];

/// The expressions the bounds rows drive: input far larger and far deeper than any the SDK's
/// own data carries, which an expression reader has to answer for without ending the run.
fn bounded_queries() -> Vec<(&'static str, String, usize)> {
    vec![
        // Deeply nested: the depth a parse recurses on is the caller's own, so what bounds it
        // is the interpreter's own recursion limit inside the bounded run — reported as an
        // error, never as a stopped run.
        (
            "deeply nested parentheses",
            format!("{}NextToken{}", "(".repeat(400), ")".repeat(400)),
            1,
        ),
        // Oversized: a multi-select of many expressions, which is read in full.
        (
            "a very long multi-select",
            format!("[{}NextToken]", "NextToken,".repeat(500)),
            501,
        ),
        // Deeply nested and oversized at once, in the projections the SDK's own data uses.
        (
            "a long chain of sub-expressions",
            format!("Contents{}", ".Key".repeat(500)),
            0,
        ),
    ]
}

/// The handler the runtime's own installation is read and driven through.
const HANDLER_SOURCE: &str = r#"
import importlib
import io
import json

import boto3
import botocore.exceptions
from s3transfer.manager import TransferConfig, TransferManager
from s3transfer.subscribers import BaseSubscriber


class _Recorder(BaseSubscriber):
    """What a transfer told its subscriber, in the order it was told."""

    def __init__(self):
        self.events = []

    def on_queued(self, future, **kwargs):
        self.events.append("queued")

    def on_progress(self, future, bytes_transferred, **kwargs):
        self.events.append("progress:%d" % bytes_transferred)

    def on_done(self, future, **kwargs):
        self.events.append("done")


def _where(event):
    """The FILE each module of the installation reports, or the error importing it raised."""
    read = {}
    for name in event["modules"]:
        try:
            read[name] = getattr(importlib.import_module(name), "__file__", None)
        except BaseException as error:
            read[name] = "%s: %s" % (type(error).__name__, error)
    return read


def _names(event):
    """Whether each name the SDK's own S3 customizations import is there."""
    read = {}
    for asked in event["names"]:
        module, _, member = asked.partition(":")
        try:
            read[asked] = hasattr(importlib.import_module(module), member)
        except BaseException as error:
            read[asked] = "%s: %s" % (type(error).__name__, error)
    return read


def _transfers(event):
    """Every managed transfer, run against the bucket the event names."""
    bucket = event["bucket"]
    content = event["content"]
    client = boto3.client("s3")
    steps = {}
    with TransferManager(client) as manager:
        # An upload whose source is a FILE of this environment, with a subscriber watching.
        path = "/tmp/payload.txt"
        with open(path, "w") as writing:
            writing.write(content)
        recorder = _Recorder()
        future = manager.upload(
            path, bucket, "from-file.txt", {"ContentType": "text/plain"}, [recorder]
        )
        future.result()
        steps["upload_file"] = {
            "done": future.done(),
            "size": future.meta.size,
            "events": recorder.events,
            "transfer_id": future.meta.transfer_id,
        }

        # A download whose destination is a FILE of this environment.
        read_back = "/tmp/read-back.txt"
        manager.download(bucket, "from-file.txt", read_back).result()
        with open(read_back) as reading:
            steps["download_file"] = reading.read()

        # An upload whose source is a STREAM in memory, and a download into one.
        manager.upload(io.BytesIO(content.encode("utf-8")), bucket, "from-stream.txt").result()
        held = io.BytesIO()
        downloaded = manager.download(bucket, "from-stream.txt", held)
        downloaded.result()
        steps["download_stream"] = {
            "body": held.getvalue().decode("utf-8"),
            "size": downloaded.meta.size,
        }

        # A copy, which the service makes for itself.
        manager.copy(
            {"Bucket": bucket, "Key": "from-file.txt"}, bucket, "copied.txt"
        ).result()

        # A delete.
        manager.delete(bucket, "from-stream.txt").result()

        # A member no managed transfer carries is refused before any call is made.
        try:
            manager.upload(path, bucket, "never.txt", {"Nonsense": "1"})
            steps["unknown_member"] = "accepted"
        except ValueError as error:
            steps["unknown_member"] = str(error)

        # A read of an object that is not there is the SERVICE's own refusal, raised by the
        # future rather than reported as a transfer that moved bytes.
        missing = manager.download(bucket, "not-there.txt", io.BytesIO())
        try:
            missing.result()
            steps["missing"] = "served"
        except botocore.exceptions.ClientError as error:
            steps["missing"] = error.response["Error"]["Code"]
    return steps


def _queries(event):
    """What each expression of the event reaches in the document it names."""
    import jmespath

    data = event["data"]
    read = {}
    for expression in event["expressions"]:
        try:
            read[expression] = jmespath.search(expression, data)
        except BaseException as error:
            read[expression] = "%s: %s" % (type(error).__name__, error)
    return read


def _compiled(event):
    """An expression PARSED once and searched more than once, as the SDK holds one."""
    import jmespath

    parsed = jmespath.compile(event["expression"])
    return {
        "expression": parsed.expression,
        "first": parsed.search(event["data"]),
        "second": parsed.search(event["data"]),
        "other": parsed.search(event["other"]),
    }


def _bounds(event):
    """What an expression far larger or deeper than any of the SDK's own comes to.

    Each is answered or raises; what must not happen is the run ending, which is why the
    outcome is reported either way.
    """
    import jmespath

    data = event["data"]
    read = {}
    for name in event["expressions"]:
        expression = event["expressions"][name]
        try:
            answered = jmespath.search(expression, data)
            read[name] = {"length": 1 if isinstance(answered, str) else len(answered or [])}
        except BaseException as error:
            read[name] = {"error": type(error).__name__}
    return read


def _timestamps(event):
    """The datetime each text states, and what the zones and the deltas beside it answer."""
    import datetime

    from dateutil import tz
    from dateutil.parser import isoparse, parse
    from dateutil.relativedelta import relativedelta

    read = {}
    for text in event["texts"]:
        try:
            read[text] = parse(text).isoformat()
        except BaseException as error:
            read[text] = "%s: %s" % (type(error).__name__, error)
    read["isoparse"] = isoparse("2024-01-02T03:04:05.123456Z").isoformat()
    read["utc_offset"] = tz.tzutc().utcoffset(None).total_seconds()
    read["utc_is_the_standard_utc"] = tz.tzutc() == datetime.timezone.utc
    read["in_an_offset_zone"] = datetime.datetime(
        2024, 1, 2, 3, 4, 5, tzinfo=tz.tzoffset("X", -3600)
    ).isoformat()
    read["named_zone_offset"] = str(
        datetime.datetime(2024, 1, 2, tzinfo=tz.gettz("UTC")).utcoffset()
    )
    read["unknown_zone"] = tz.gettz("Nowhere/Nothing") is None
    read["local_zone_is_a_zone"] = isinstance(tz.tzlocal(), datetime.tzinfo)
    read["a_month_on"] = (datetime.date(2024, 1, 31) + relativedelta(months=1)).isoformat()
    read["a_year_back"] = (datetime.date(2024, 2, 29) - relativedelta(years=1)).isoformat()
    return read


def _parts(event):
    """An upload of an object OVER the configured threshold, which is sent as parts."""
    bucket = event["bucket"]
    client = boto3.client("s3")
    piece = event["piece"]
    body = ("a" * piece + "b" * piece + "c").encode("utf-8")
    config = TransferConfig(multipart_threshold=piece, multipart_chunksize=piece)
    recorder = _Recorder()
    with TransferManager(client, config) as manager:
        future = manager.upload(
            io.BytesIO(body), bucket, "in-parts.txt", None, [recorder]
        )
        read = {"size": future.meta.size, "events": recorder.events}
        try:
            future.result()
            read["result"] = "completed"
        except botocore.exceptions.ClientError as error:
            read["result"] = error.response["Error"]["Code"]
    return read


_CASES = {
    "where": _where,
    "names": _names,
    "transfers": _transfers,
    "parts": _parts,
    "queries": _queries,
    "compiled": _compiled,
    "bounds": _bounds,
    "timestamps": _timestamps,
}


def lambda_handler(event, context):
    return _CASES[event["case"]](event)
"#;

/// The handler of the function whose deployment package BUNDLES the SDK itself: it imports the
/// package's own copy and reports where each module came from.
const BUNDLED_HANDLER_SOURCE: &str = r#"
import boto3
import botocore
import jmespath
import s3transfer


def lambda_handler(event, context):
    return {
        "boto3": boto3.__file__,
        "boto3_marker": getattr(boto3, "MARKER", None),
        "botocore": botocore.__file__,
        "s3transfer": s3transfer.__file__,
        "jmespath": jmespath.__file__,
        "manager": boto3.MANAGER,
        "queried": boto3.QUERIED,
    }
"#;

/// The copy of the SDK's upper half such a package bundles: it imports the dependencies it did
/// NOT bundle by name and USES them, exactly as the published SDK's own S3 customizations and
/// its data-driven interfaces do.
const BUNDLED_BOTO3_SOURCE: &str = r#"
import jmespath
from s3transfer.manager import TransferManager

import botocore

MARKER = "the deployment package"
MANAGER = TransferManager.__name__
QUERIED = jmespath.search("Contents[].Key", {"Contents": [{"Key": "bundled.txt"}]})
"#;

/// The copy of the SDK's lower half beside it.
const BUNDLED_BOTOCORE_SOURCE: &str = "MARKER = \"the deployment package\"\n";

/// The policy a role that may read and write the bucket carries.
fn object_access() -> Value {
    json!([{ "Effect": "Allow", "Action": "s3:*", "Resource": "*" }])
}

/// Create the function `name`, running as `role_arn`, out of the entries `entries` holds.
async fn given_function(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    role_arn: &str,
    entries: &[(&str, &str)],
) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": role_arn,
            "Runtime": "python3.12",
            "Handler": "lambda_function.lambda_handler",
            "Timeout": 60,
            "MemorySize": 512,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(crate::node_invoke_run::package(entries)),
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
    await_function_active(router, case, name).await;
}

/// Invoke `name` with `event`, and answer what the handler returned.
async fn invoked(router: &MultiServiceRouter, case: &str, name: &str, event: &Value) -> Value {
    let answered = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        event,
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
        answered.status,
        answered.text()
    );
    let failure = answered.header("x-amz-function-error");
    assert_eq!(
        failure, None,
        "case {case:?}: the handler reports each outcome itself, so the invocation carries no \
         function error, got {failure:?} with {}",
        answered.text()
    );
    answered.json()
}

/// The whole rule, driven END TO END as one test: every table below is real `CreateRole`,
/// `CreateFunction`, `Invoke` and Amazon S3 requests over one router.
#[tokio::test]
async fn a_python_runtime_preinstalls_every_distribution_its_sdk_installation_needs() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "the SDK installation a managed python runtime preinstalls";
    let router = emulator();
    let (created, body) = s3(&router, "PUT", &format!("/{BUCKET}"), b"").await;
    assert!(
        created == 200,
        "case {case:?}: expected CreateBucket to answer 200, got {created}: {body}"
    );
    let role = given_role(&router, case, "runtime-sdk-dependency-role", Some(object_access())).await;
    let function = "runtime-sdk-dependency-reader";
    given_function(
        &router,
        case,
        function,
        &role,
        &[("lambda_function.py", HANDLER_SOURCE)],
    )
    .await;

    every_module_of_the_installation_is_the_runtimes_own(&router, function).await;
    every_name_the_sdk_imports_is_there(&router, function).await;
    a_bundled_sdk_resolves_its_dependency_from_the_runtime(&router, case, &role).await;
    every_construct_of_the_query_language_is_evaluated(&router, function).await;
    a_parsed_expression_is_searched_more_than_once(&router, function).await;
    an_expression_larger_than_any_of_the_sdks_is_answered_or_refused(&router, function).await;
    every_form_a_timestamp_is_written_in_is_read(&router, function).await;
    every_managed_transfer_moves_the_bytes(&router, function).await;
    an_object_over_the_threshold_is_sent_as_parts(&router, function).await;
}

/// One row per construct of the query language, and one per expression it refuses: every
/// expression is tokenized, parsed and evaluated, so what a row answers is what the language
/// states — nothing is matched as text.
async fn every_construct_of_the_query_language_is_evaluated(
    router: &MultiServiceRouter,
    function: &str,
) {
    let rows = queries();
    let expressions: Vec<&str> = rows
        .iter()
        .map(|(expression, _)| *expression)
        .chain(REFUSED_QUERIES.iter().map(|(expression, _)| *expression))
        .collect();
    let read = invoked(
        router,
        "the query language",
        function,
        &json!({
            "case": "queries",
            "data": queried_document(),
            "expressions": expressions,
        }),
    )
    .await;
    for (expression, expected) in &rows {
        assert_eq!(
            read[*expression], *expected,
            "case {expression:?}: expected the expression to reach {expected}, got {}",
            read[*expression]
        );
    }
    for (expression, error) in REFUSED_QUERIES {
        let answered = read[*expression].as_str().unwrap_or_default();
        assert!(
            answered.starts_with(&format!("{error}: ")),
            "case {expression:?}: expected the expression refused as {error}, got {answered:?}"
        );
    }
}

/// One row per FORM a timestamp of the wire is written in: each is read into the datetime it
/// states, with the offset the text itself states — and the zones and the deltas beside the
/// parser answer what the library's own do.
async fn every_form_a_timestamp_is_written_in_is_read(
    router: &MultiServiceRouter,
    function: &str,
) {
    let case = "every form a timestamp is written in";
    let texts: Vec<&str> = TIMESTAMPS
        .iter()
        .map(|(text, _)| *text)
        .chain(["not a timestamp at all"])
        .collect();
    let read = invoked(
        router,
        case,
        function,
        &json!({ "case": "timestamps", "texts": texts }),
    )
    .await;
    for (text, expected) in TIMESTAMPS {
        assert_eq!(
            read[*text],
            json!(expected),
            "case {text:?}: expected the datetime {expected:?}, got {}",
            read[*text]
        );
    }
    let refused = read["not a timestamp at all"].as_str().unwrap_or_default();
    assert!(
        refused.starts_with("ParserError: "),
        "case {case:?}: expected a text that is no timestamp to be refused by the library's \
         own error, got {refused:?}"
    );
    let expected = json!({
        "isoparse": "2024-01-02T03:04:05.123456+00:00",
        "utc_offset": 0.0,
        "utc_is_the_standard_utc": true,
        "in_an_offset_zone": "2024-01-02T03:04:05-01:00",
        "named_zone_offset": "0:00:00",
        "unknown_zone": true,
        "local_zone_is_a_zone": true,
        // A month added to the 31st lands on the last day of a month that has no 31st, and a
        // year taken off the 29th of February lands inside a year that has no 29th.
        "a_month_on": "2024-02-29",
        "a_year_back": "2023-02-28",
    });
    for (name, wanted) in expected.as_object().expect("the expected document") {
        assert_eq!(
            read[name.as_str()], *wanted,
            "case {name:?}: expected {wanted}, got {}",
            read[name.as_str()]
        );
    }
}

/// Input far larger and far deeper than any expression the SDK's own data carries is either
/// answered or refused by name — never left to end the run.
///
/// The depth a parse recurses on is the caller's own, so what bounds it is the interpreter's
/// own recursion limit inside a run the host already bounds in time, steps and memory: a row
/// that trips it reports the error, and one that does not reports what the expression reached.
async fn an_expression_larger_than_any_of_the_sdks_is_answered_or_refused(
    router: &MultiServiceRouter,
    function: &str,
) {
    let rows = bounded_queries();
    let expressions: serde_json::Map<String, Value> = rows
        .iter()
        .map(|(case, expression, _)| ((*case).to_string(), json!(expression)))
        .collect();
    let read = invoked(
        router,
        "an expression larger than any of the SDK's own",
        function,
        &json!({ "case": "bounds", "data": queried_document(), "expressions": expressions }),
    )
    .await;
    for (case, _, length) in &rows {
        let answered = &read[*case];
        let reached = answered.get("length").and_then(Value::as_u64);
        let refused = answered.get("error").and_then(Value::as_str);
        assert!(
            reached == Some(*length as u64) || refused.is_some(),
            "case {case:?}: expected the expression to reach {length} values or to be refused \
             by name, got {answered}"
        );
    }
}

/// An expression PARSED once answers the same thing every time it is searched, and answers
/// about whatever document it is searched against — which is what the SDK relies on when it
/// compiles a paginator's expressions once and drives every page through them.
async fn a_parsed_expression_is_searched_more_than_once(
    router: &MultiServiceRouter,
    function: &str,
) {
    let case = "a parsed expression searched again";
    let read = invoked(
        router,
        case,
        function,
        &json!({
            "case": "compiled",
            "expression": "Contents[].Key",
            "data": queried_document(),
            "other": { "Contents": [{ "Key": "elsewhere.txt" }] },
        }),
    )
    .await;
    let expected = json!({
        "expression": "Contents[].Key",
        "first": ["a.txt", "b/c.txt", "d.txt"],
        "second": ["a.txt", "b/c.txt", "d.txt"],
        "other": ["elsewhere.txt"],
    });
    assert_eq!(
        read, expected,
        "case {case:?}: expected the parsed expression to answer about each document it is \
         searched against, got {read}"
    );
}

/// One row per module of the installation: each is importable and each reports its own file
/// under the RUNTIME's directory, which is where a preinstalled distribution lives.
async fn every_module_of_the_installation_is_the_runtimes_own(
    router: &MultiServiceRouter,
    function: &str,
) {
    let names: Vec<&str> = INSTALLED_MODULES.iter().map(|(name, _)| *name).collect();
    let read = invoked(
        router,
        "the installation's own modules",
        function,
        &json!({ "case": "where", "modules": names }),
    )
    .await;
    for (name, path) in INSTALLED_MODULES {
        let expected = json!(format!("{RUNTIME_ROOT}/{path}"));
        assert_eq!(
            read[*name], expected,
            "case {name:?}: expected the module preinstalled at {expected}, got {}",
            read[*name]
        );
    }
}

/// One row per name the published SDK's own S3 customizations import out of the transfer
/// library: every one of them is there, so a handler — or a bundled SDK — that imports them
/// by name loads.
async fn every_name_the_sdk_imports_is_there(router: &MultiServiceRouter, function: &str) {
    let read = invoked(
        router,
        "the names the SDK imports",
        function,
        &json!({ "case": "names", "names": IMPORTED_NAMES }),
    )
    .await;
    for asked in IMPORTED_NAMES {
        assert_eq!(
            read[*asked],
            json!(true),
            "case {asked:?}: expected the name to be there, got {}",
            read[*asked]
        );
    }
}

/// A deployment package that BUNDLES the SDK itself: its own copy of each package it bundled
/// wins, and the dependency it did not bundle resolves from the runtime behind it — which is
/// what lets such a package import at all.
async fn a_bundled_sdk_resolves_its_dependency_from_the_runtime(
    router: &MultiServiceRouter,
    case: &str,
    role: &str,
) {
    let function = "runtime-sdk-dependency-bundled";
    given_function(
        router,
        case,
        function,
        role,
        &[
            ("lambda_function.py", BUNDLED_HANDLER_SOURCE),
            ("boto3/__init__.py", BUNDLED_BOTO3_SOURCE),
            ("botocore/__init__.py", BUNDLED_BOTOCORE_SOURCE),
        ],
    )
    .await;
    let case = "a deployment package bundling the SDK";
    let read = invoked(router, case, function, &json!({})).await;
    let expected = json!({
        // What the package bundled is resolved from the package.
        "boto3": format!("{TASK_ROOT}/boto3/__init__.py"),
        "boto3_marker": "the deployment package",
        "botocore": format!("{TASK_ROOT}/botocore/__init__.py"),
        // What it did not bundle is resolved from the runtime, which is the whole point —
        // and it WORKS there: the query the bundled copy ran at its own module scope
        // answered.
        "s3transfer": format!("{RUNTIME_ROOT}/s3transfer/__init__.py"),
        "jmespath": format!("{RUNTIME_ROOT}/jmespath/__init__.py"),
        "manager": "TransferManager",
        "queried": ["bundled.txt"],
    });
    assert_eq!(
        read, expected,
        "case {case:?}: expected the bundled copies to win and the dependency to come from the \
         runtime, got {read}"
    );
}

/// One row per managed transfer and per form the object is given in — and every object read
/// back through Amazon S3's own API, so a transfer that moved no bytes fails here.
async fn every_managed_transfer_moves_the_bytes(router: &MultiServiceRouter, function: &str) {
    let case = "every managed transfer";
    let content = "{\"report\": \"managed transfer\"}";
    let read = invoked(
        router,
        case,
        function,
        &json!({ "case": "transfers", "bucket": BUCKET, "content": content }),
    )
    .await;

    // The upload of a file: the size the future reports is the file's own, the subscriber was
    // told in the library's own order, and the transfer is done.
    let uploaded = &read["upload_file"];
    assert_eq!(
        uploaded["size"],
        json!(content.len()),
        "case {case:?}: expected the file's own size on the future, got {uploaded}"
    );
    assert_eq!(
        uploaded["done"],
        json!(true),
        "case {case:?}: expected the submitted transfer to be done, got {uploaded}"
    );
    assert_eq!(
        uploaded["events"],
        json!(["queued", format!("progress:{}", content.len()), "done"]),
        "case {case:?}: expected the subscriber told queued, the progress and done, got \
         {uploaded}"
    );

    // The download into a file, and the stream both ways.
    assert_eq!(
        read["download_file"],
        json!(content),
        "case {case:?}: expected the object's own bytes written to the file, got {read}"
    );
    assert_eq!(
        read["download_stream"]["body"],
        json!(content),
        "case {case:?}: expected the object's own bytes in the stream, got {}",
        read["download_stream"]
    );
    assert_eq!(
        read["download_stream"]["size"],
        json!(content.len()),
        "case {case:?}: expected the length the service reported on the future, got {}",
        read["download_stream"]
    );

    // A member no managed transfer carries is refused with the key that was not allowed.
    let refused = read["unknown_member"].as_str().unwrap_or_default();
    assert!(
        refused.starts_with("Invalid extra_args key 'Nonsense', must be one of: "),
        "case {case:?}: expected the unknown member named in the refusal, got {refused:?}"
    );

    // A read of an object that is not there is the service's own refusal.
    assert_eq!(
        read["missing"],
        json!("NoSuchKey"),
        "case {case:?}: expected the service's own refusal on the future, got {read}"
    );

    // Read every object back through Amazon S3 itself: the uploads are there with the bytes
    // the handler sent, the copy is the same object again, and the delete is gone.
    for (key, expected) in [("from-file.txt", content), ("copied.txt", content)] {
        let (status, body) = s3(router, "GET", &format!("/{BUCKET}/{key}"), b"").await;
        assert_eq!(
            status, 200,
            "case {key:?}: expected the transferred object to be readable, got {status}: {body}"
        );
        assert_eq!(
            body, expected,
            "case {key:?}: expected the bytes the handler transferred, got {body:?}"
        );
    }
    let (status, body) = s3(router, "GET", &format!("/{BUCKET}/from-stream.txt"), b"").await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected the deleted object to be gone, got {status}: {body}"
    );
}

/// An object LARGER than the configured threshold is sent as several parts, each of the
/// configured size: the parts are the service's own multipart upload, so a part below the
/// size the service documents is refused by the service and nothing is left behind.
async fn an_object_over_the_threshold_is_sent_as_parts(
    router: &MultiServiceRouter,
    function: &str,
) {
    let case = "an object over the threshold";
    // Two full parts and a remainder, at a part size BELOW the smallest the service admits:
    // what comes back is therefore the service's own refusal of the part, which only a
    // multipart upload that really happened can produce.
    let piece = 1024_usize;
    let read = invoked(
        router,
        case,
        function,
        &json!({ "case": "parts", "bucket": BUCKET, "piece": piece }),
    )
    .await;
    assert_eq!(
        read["size"],
        json!(piece * 2 + 1),
        "case {case:?}: expected the stream's own size on the future, got {read}"
    );
    assert_eq!(
        read["events"],
        json!([
            "queued",
            format!("progress:{piece}"),
            format!("progress:{piece}"),
            "progress:1",
            "done"
        ]),
        "case {case:?}: expected one progress report per part sent, got {read}"
    );
    assert_eq!(
        read["result"],
        json!("EntityTooSmall"),
        "case {case:?}: expected the service's own refusal of a part below its minimum, got \
         {read}"
    );
    // Nothing of the refused upload is left: the object was never completed, and the upload
    // the manager started was aborted.
    let (status, body) = s3(router, "GET", &format!("/{BUCKET}/in-parts.txt"), b"").await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected no object from an upload that was refused, got {status}: \
         {body}"
    );
    let (status, listed) = s3(router, "GET", &format!("/{BUCKET}?uploads"), b"").await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the multipart uploads to be listable, got {status}: {listed}"
    );
    assert!(
        !listed.contains("in-parts.txt"),
        "case {case:?}: expected the refused upload to have been aborted, got {listed}"
    );
}
