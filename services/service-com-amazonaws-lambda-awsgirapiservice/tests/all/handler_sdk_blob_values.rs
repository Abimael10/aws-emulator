// SPDX-License-Identifier: CC0-1.0

//! The FORMS a running handler may put on a blob member of an SDK call, and the MANAGED
//! TRANSFERS the SDK adds to the Amazon S3 client: Lambda, Amazon S3, IAM and the token
//! service on ONE engine, a `python3.x` handler that writes and reads object bytes through
//! each of them, and the bytes read back where a client reads them.
//!
//! RULE: the AWS SDK a managed runtime bundles presents the surface that SDK documents — a
//! blob input member takes the value in any of the forms documented for it (the bytes
//! themselves, text, or a file-like object, read from the position the caller left it at),
//! whichever operation of whichever service carries that member, and the Amazon S3 client
//! carries the managed-transfer methods the SDK adds to that client and to no other
//! (`upload_fileobj`, `download_fileobj`, `upload_file`, `download_file`) — so a handler
//! written the documented way makes its call, and only a value that is none of those forms
//! is refused before the request is built.
//!
//! Three tables, each over what the rule generalises over:
//!
//! * the FORM table drives `PutObject$Body` — the member the client encodes before the call
//!   — one row per form of the value, including a buffer read to its end and rewound and one
//!   left standing part-way through its bytes (which carries what is LEFT of it), plus two
//!   values that are no blob at all, refused with nothing written — one of them a value whose
//!   own rendering carries braces, so the whole refusal sentence is read back as the value
//!   wrote it;
//! * the OTHER-MEMBER table drives the same forms through `Invoke$Payload`, a blob member
//!   encoded wherever the call's own document carries it rather than from a per-service list,
//!   so the rule is pinned on both seams a blob travels through;
//! * the TRANSFER table round-trips every managed transfer — through a buffer and through the
//!   function's own ephemeral storage — asserts each is present on the Amazon S3 client and
//!   absent on a client of another service, asserts a transfer handed a value it cannot read
//!   is refused rather than served, and asserts that a transfer named a path OUTSIDE the
//!   function's own storage is refused with nothing read and nothing written: the path a
//!   transfer opens is the handler's own value, and which paths a run may open at all is the
//!   runtime's file layer's to decide.
//!
//! Every row that writes an object reads it back through Amazon S3's own API, so a call that
//! was accepted and carried the wrong bytes fails here.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::given_role;
use crate::event_source_mappings::{await_function_active, call, router_on};
use crate::invoke_request_id::one_invocation_stream_at_a_time;
use crate::synchronous_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The bucket every object below is written into. Each test builds an emulator of its own, so
/// the name is shared and the state is not.
const BUCKET: &str = "handler-sdk-blob-values";

/// The `Timeout` every function here carries, in seconds. Sized the way the sibling suites
/// that RUN handlers size theirs: it has to cover this test binary's own scheduling, not the
/// handlers' work.
const FUNCTION_TIMEOUT_SECONDS: i32 = 25;

/// The content type the transfer table names on its upload, which a managed transfer carries
/// through `ExtraArgs`.
const TRANSFER_TYPE: &str = "text/csv";

/// The managed transfers the SDK adds to the Amazon S3 client, by the names it documents them
/// under, sorted the way the handler reports them.
const TRANSFERS: [&str; 4] = [
    "download_file",
    "download_fileobj",
    "upload_file",
    "upload_fileobj",
];

/// The handler every table runs: it puts the value the row names on a blob member of a call
/// made with the SDK the runtime bundles, or drives one of that SDK's managed transfers, and
/// reports what it saw — the SDK's own refusal included, so a row that must be refused is
/// asserted on the refusal rather than on a missing object alone.
const BLOB_SOURCE: &str = r#"
import io
import json

import boto3

S3 = boto3.client("s3")
LAMBDA = boto3.client("lambda")

TRANSFER_NAMES = (
    "download_file",
    "download_fileobj",
    "upload_file",
    "upload_fileobj",
)


def _blob(form, text, offset):
    """The value this row puts on the call's blob member."""
    data = text.encode("utf-8")
    if form == "bytes":
        return data
    if form == "bytearray":
        return bytearray(data)
    if form == "text":
        return text
    if form == "buffer":
        return io.BytesIO(data)
    if form == "buffer-rewound":
        held = io.BytesIO(data)
        held.read()
        held.seek(0)
        return held
    if form == "buffer-advanced":
        held = io.BytesIO(data)
        held.read(offset)
        return held
    if form == "unsendable":
        return object()
    if form == "mapping":
        return {"rows": 1}
    raise ValueError("no such form: %s" % (form,))


def _refusal(error):
    return {"ok": False, "refused": "%s: %s" % (type(error).__name__, error)}


def lambda_handler(event, context):
    action = event["action"]
    if action == "put_object":
        try:
            S3.put_object(
                Bucket=event["bucket"],
                Key=event["key"],
                Body=_blob(event["form"], event["text"], event.get("offset", 0)),
            )
        except Exception as error:
            return _refusal(error)
        return {"ok": True}
    if action == "invoke":
        try:
            answered = LAMBDA.invoke(
                FunctionName=event["target"],
                Payload=_blob(
                    event["form"],
                    json.dumps({"echoed": event["text"]}),
                    event.get("offset", 0),
                ),
            )
        except Exception as error:
            return _refusal(error)
        return {
            "ok": True,
            "answered": json.loads(answered["Payload"].read().decode("utf-8")),
        }
    if action == "upload_fileobj":
        S3.upload_fileobj(
            io.BytesIO(event["text"].encode("utf-8")),
            event["bucket"],
            event["key"],
            ExtraArgs={"ContentType": event["content_type"]},
        )
        return {"ok": True}
    if action == "upload_fileobj_not_file_like":
        try:
            S3.upload_fileobj(
                event["text"].encode("utf-8"), event["bucket"], event["key"]
            )
        except Exception as error:
            return _refusal(error)
        return {"ok": True}
    if action == "download_fileobj":
        held = io.BytesIO()
        S3.download_fileobj(event["bucket"], event["key"], held)
        return {"ok": True, "read": held.getvalue().decode("utf-8")}
    if action == "upload_file":
        path = event["path"]
        try:
            with open(path, "wb") as writing:
                writing.write(event["text"].encode("utf-8"))
            S3.upload_file(path, event["bucket"], event["key"])
        except Exception as error:
            return _refusal(error)
        return {"ok": True}
    if action == "download_file":
        path = event["path"]
        try:
            S3.download_file(event["bucket"], event["key"], path)
            with open(path, "rb") as reading:
                return {"ok": True, "read": reading.read().decode("utf-8")}
        except Exception as error:
            return _refusal(error)
    if action == "transfers":
        other = boto3.client("sqs")
        return {
            "ok": True,
            "on_s3": sorted(name for name in TRANSFER_NAMES if hasattr(S3, name)),
            "on_other": sorted(name for name in TRANSFER_NAMES if hasattr(other, name)),
        }
    raise ValueError("no such action: %s" % (action,))
"#;

/// The function the other-member table's handler calls: it answers the event it was handed, so
/// the payload the caller's blob member carried is what comes back.
const ECHO_SOURCE: &str = r#"
def lambda_handler(event, context):
    return event
"#;

/// Lambda, Amazon S3, IAM and the token service over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the handler's own SDK calls are dispatched back into
/// this same router, so the bucket it writes is the bucket this suite reads and the function
/// it invokes is the function this suite created.
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

/// The identity policy the execution role carries: the object actions the handler makes, and
/// the invocation the other-member table's handler makes.
fn handler_access() -> Value {
    json!([{
        "Effect": "Allow",
        "Action": ["s3:GetObject", "s3:PutObject", "lambda:InvokeFunction"],
        "Resource": "*",
    }])
}

/// One Amazon S3 request, path style, as the account's own credential.
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: &[u8],
) -> (u16, http::HeaderMap, Vec<u8>) {
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
    let (parts, body) = response.into_parts();
    (parts.status.as_u16(), parts.headers, body.to_vec())
}

/// One Amazon S3 request that must be served, answering the headers and bytes it came with.
async fn s3_ok(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: &[u8],
) -> (http::HeaderMap, Vec<u8>) {
    let (status, headers, answered) = s3(router, method, path, body).await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: expected S3 {method} {path} to be served, got {status} with {}",
        String::from_utf8_lossy(&answered)
    );
    (headers, answered)
}

/// Create the function `name` out of `source`, running as `role_arn`, and wait until it serves
/// invocations.
async fn given_function(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    role_arn: &str,
    source: &str,
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
            "Timeout": FUNCTION_TIMEOUT_SECONDS,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(source)),
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

/// `Invoke` `function` with `event`, which must run the handler to completion, and answer the
/// payload it returned.
async fn invoked(router: &MultiServiceRouter, case: &str, function: &str, event: &Value) -> Value {
    let answered = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/invocations"),
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
    assert_eq!(
        answered.header("x-amz-function-error"),
        None,
        "case {case:?}: the handler must run to completion, got the function error {:?} with {}",
        answered.header("x-amz-function-error"),
        answered.text()
    );
    answered.json()
}

/// The text every row carries: wide enough that a buffer left part-way through it has a
/// distinct tail, and outside ASCII so the UTF-8 a text value is encoded as is exercised.
const TEXT: &str = "order,total\nA-1,42\nA-2,7\nparçal,9\n";

/// How many bytes the part-read row consumes before the value is handed to the call, so what
/// the call carries is the TAIL of the buffer. A whole number of characters of [`TEXT`], so
/// the tail is text in its own right.
const CONSUMED: usize = 12;

/// What the call must carry for one row of the form table.
#[derive(Debug, PartialEq, Eq)]
enum Carries {
    /// Every byte the value stands for.
    Whole,
    /// What is LEFT of a file-like value the handler already read part of.
    Tail,
    /// Nothing: the SDK refuses the value before it builds the request. `Some(sentence)` is
    /// the WHOLE refusal the handler must report, for a value whose rendering is the same on
    /// every run; `None` is a value whose own rendering carries a per-run address, where the
    /// parts of the sentence are asserted instead.
    Nothing(Option<&'static str>),
}

/// One row of the form table: the form the handler puts on the blob member, and what the call
/// must then carry.
struct Form {
    case: &'static str,
    form: &'static str,
    carries: Carries,
}

/// The rows the form table spans: every form the SDK documents for a blob member, the two
/// positions a file-like value can stand at, and two values that are no blob at all — one
/// whose rendering carries braces, so the sentence the refusal is substituted into is read
/// back as the value wrote it.
fn forms() -> Vec<Form> {
    vec![
        Form {
            case: "the bytes themselves",
            form: "bytes",
            carries: Carries::Whole,
        },
        Form {
            case: "a mutable byte buffer",
            form: "bytearray",
            carries: Carries::Whole,
        },
        Form {
            case: "text, which the SDK encodes as UTF-8",
            form: "text",
            carries: Carries::Whole,
        },
        Form {
            case: "a file-like object standing at its start",
            form: "buffer",
            carries: Carries::Whole,
        },
        Form {
            case: "a file-like object read to its end and rewound",
            form: "buffer-rewound",
            carries: Carries::Whole,
        },
        Form {
            case: "a file-like object left part-way through its bytes",
            form: "buffer-advanced",
            carries: Carries::Tail,
        },
        Form {
            case: "a value that is no blob at all",
            form: "unsendable",
            carries: Carries::Nothing(None),
        },
        Form {
            case: "a mapping, whose own rendering carries braces",
            form: "mapping",
            carries: Carries::Nothing(Some(
                "ParamValidationError: Parameter validation failed:\n\
                 Invalid type for parameter Body, value: {'rows': 1}, type: <class 'dict'>, \
                 valid types: <class 'bytes'>, <class 'bytearray'>, file-like object",
            )),
        },
    ]
}

/// RULE: a blob input member takes the value in every form the SDK documents, and the call
/// carries exactly the bytes that value stands for.
///
/// Driven through `PutObject$Body` and asserted on the stored object, so a row the SDK accepted
/// and carried the wrong bytes for fails here; the refused row is asserted on the SDK's own
/// refusal AND on the object it must not have written.
#[tokio::test]
async fn a_blob_member_takes_every_form_the_sdk_documents() {
    let _stream = one_invocation_stream_at_a_time().await;
    let router = emulator();
    let role = given_role(&router, "setup", "blob-values-role", Some(handler_access())).await;
    let function = "blob-values";
    given_function(&router, "setup", function, &role, BLOB_SOURCE).await;
    s3_ok(&router, "setup", "PUT", &format!("/{BUCKET}"), &[]).await;

    for (index, row) in forms().iter().enumerate() {
        let case = row.case;
        let key = format!("written/body-{index}");
        let reported = invoked(
            &router,
            case,
            function,
            &json!({
                "action": "put_object",
                "bucket": BUCKET,
                "key": key,
                "form": row.form,
                "text": TEXT,
                "offset": CONSUMED,
            }),
        )
        .await;

        let expected = match row.carries {
            Carries::Whole => Some(TEXT),
            // What is LEFT of the buffer, which is how the SDK reads a file-like value.
            Carries::Tail => Some(&TEXT[CONSUMED..]),
            Carries::Nothing(_) => None,
        };
        match expected {
            Some(text) => {
                assert_eq!(
                    reported["ok"],
                    json!(true),
                    "case {case:?}: expected the SDK to carry the value, got {reported}"
                );
                let (_, stored) =
                    s3_ok(&router, case, "GET", &format!("/{BUCKET}/{key}"), &[]).await;
                assert_eq!(
                    String::from_utf8_lossy(&stored),
                    text,
                    "case {case:?}: expected the object to hold the bytes the value stands \
                     for, got {:?}",
                    String::from_utf8_lossy(&stored)
                );
            }
            None => {
                assert_eq!(
                    reported["ok"],
                    json!(false),
                    "case {case:?}: expected the SDK to refuse the value, got {reported}"
                );
                let refused = reported["refused"].as_str().unwrap_or_default();
                match row.carries {
                    // A value whose rendering is the same on every run: the WHOLE sentence,
                    // so a rendering that mangles the value's own text fails here.
                    Carries::Nothing(Some(sentence)) => assert_eq!(
                        refused, sentence,
                        "case {case:?}: expected the SDK's own parameter refusal, got \
                         {refused:?}"
                    ),
                    _ => {
                        for named in [
                            "ParamValidationError",
                            "Invalid type for parameter Body",
                            "file-like object",
                        ] {
                            assert!(
                                refused.contains(named),
                                "case {case:?}: expected the SDK's own parameter refusal to \
                                 name {named:?}, got {refused:?}"
                            );
                        }
                    }
                }
                let (status, _, answered) =
                    s3(&router, "GET", &format!("/{BUCKET}/{key}"), &[]).await;
                assert_eq!(
                    status,
                    404,
                    "case {case:?}: a refused call writes nothing, and {key:?} answered \
                     {status} with {}",
                    String::from_utf8_lossy(&answered)
                );
            }
        }
    }
}

/// RULE, on ANOTHER operation's blob member: `Invoke$Payload` takes the same forms, because
/// what the SDK does with a blob is decided by the member's form and not by the operation's
/// identity.
///
/// Asserted on what the invoked function answered, which is the payload the member carried.
#[tokio::test]
async fn another_operations_blob_member_takes_the_same_forms() {
    let _stream = one_invocation_stream_at_a_time().await;
    let router = emulator();
    let role = given_role(&router, "setup", "blob-payload-role", Some(handler_access())).await;
    let caller = "blob-payload-caller";
    let echo = "blob-payload-echo";
    given_function(&router, "setup", caller, &role, BLOB_SOURCE).await;
    given_function(&router, "setup", echo, &role, ECHO_SOURCE).await;

    // Text is left out: a `Payload` given as text is the document itself rather than a blob
    // this SDK encodes. The part-read position is left out too: a document read part-way
    // through is no longer a document, which is a question about JSON and not about the form
    // the member takes.
    for form in ["bytes", "bytearray", "buffer", "buffer-rewound"] {
        let case = form;
        let reported = invoked(
            &router,
            case,
            caller,
            &json!({
                "action": "invoke",
                "target": echo,
                "form": form,
                "text": TEXT,
            }),
        )
        .await;
        assert_eq!(
            reported["ok"],
            json!(true),
            "case {case:?}: expected the SDK to carry the payload, got {reported}"
        );
        assert_eq!(
            reported["answered"],
            json!({ "echoed": TEXT }),
            "case {case:?}: expected the invoked function to answer the payload the member \
             carried, got {reported}"
        );
    }
}

/// RULE: the Amazon S3 client carries the managed transfers the SDK adds to it, and a client
/// of another service carries none of them.
///
/// Every transfer is round-tripped — the upload asserted where a client reads the object, the
/// download asserted on what the handler read back — and the upload's `ExtraArgs` is asserted
/// on the stored object, because those arguments are members of the call the transfer makes.
#[tokio::test]
async fn the_s3_client_carries_the_managed_transfers_the_sdk_adds_to_it() {
    let _stream = one_invocation_stream_at_a_time().await;
    let router = emulator();
    let role = given_role(&router, "setup", "blob-transfer-role", Some(handler_access())).await;
    let function = "blob-transfers";
    given_function(&router, "setup", function, &role, BLOB_SOURCE).await;
    s3_ok(&router, "setup", "PUT", &format!("/{BUCKET}"), &[]).await;

    // Which transfers the Amazon S3 client has at all, and which a client of another service
    // has.
    let case = "the transfers the SDK adds to each client";
    let reported = invoked(&router, case, function, &json!({ "action": "transfers" })).await;
    assert_eq!(
        reported["on_s3"],
        json!(TRANSFERS),
        "case {case:?}: expected every managed transfer on the Amazon S3 client, got {reported}"
    );
    assert_eq!(
        reported["on_other"],
        json!([]),
        "case {case:?}: expected no managed transfer on another service's client, got {reported}"
    );

    // `upload_fileobj`: a buffer the handler built, written with the content type its
    // `ExtraArgs` names.
    let case = "upload_fileobj writes the buffer it is handed";
    let key = "written/by-fileobj.csv";
    invoked(
        &router,
        case,
        function,
        &json!({
            "action": "upload_fileobj",
            "bucket": BUCKET,
            "key": key,
            "text": TEXT,
            "content_type": TRANSFER_TYPE,
        }),
    )
    .await;
    let (headers, stored) = s3_ok(&router, case, "GET", &format!("/{BUCKET}/{key}"), &[]).await;
    assert_eq!(
        String::from_utf8_lossy(&stored),
        TEXT,
        "case {case:?}: expected the uploaded object to hold the buffer's bytes, got {:?}",
        String::from_utf8_lossy(&stored)
    );
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some(TRANSFER_TYPE),
        "case {case:?}: expected the content type the transfer's ExtraArgs named, got {headers:?}"
    );

    // `upload_fileobj` handed a value with nothing to read: "the file-like object must be in
    // binary mode", so the transfer checks for the method the value has to carry and names
    // it, rather than accepting more than the real transfer does.
    let case = "upload_fileobj refuses a value that is not file-like";
    let reported = invoked(
        &router,
        case,
        function,
        &json!({
            "action": "upload_fileobj_not_file_like",
            "bucket": BUCKET,
            "key": "written/never.csv",
            "text": TEXT,
        }),
    )
    .await;
    assert_eq!(
        reported["refused"],
        json!("ValueError: Fileobj must implement read"),
        "case {case:?}: expected the transfer to refuse a value it cannot read, got {reported}"
    );
    let (status, _, answered) =
        s3(&router, "GET", &format!("/{BUCKET}/written/never.csv"), &[]).await;
    assert_eq!(
        status,
        404,
        "case {case:?}: a refused transfer writes nothing, got {status} with {}",
        String::from_utf8_lossy(&answered)
    );

    // `download_fileobj`: an object a client wrote, read back into a buffer.
    let case = "download_fileobj reads into the buffer it is handed";
    let key = "staged/for-fileobj.csv";
    s3_ok(
        &router,
        case,
        "PUT",
        &format!("/{BUCKET}/{key}"),
        TEXT.as_bytes(),
    )
    .await;
    let reported = invoked(
        &router,
        case,
        function,
        &json!({
            "action": "download_fileobj",
            "bucket": BUCKET,
            "key": key,
        }),
    )
    .await;
    assert_eq!(
        reported["read"],
        json!(TEXT),
        "case {case:?}: expected the download to hand over the object's own bytes, got {reported}"
    );

    // `upload_file` / `download_file`: the same two transfers through the function's own
    // ephemeral storage, which is the path form a handler names them with.
    let case = "upload_file writes the file at the path it is given";
    let key = "written/by-file.csv";
    invoked(
        &router,
        case,
        function,
        &json!({
            "action": "upload_file",
            "bucket": BUCKET,
            "key": key,
            "text": TEXT,
            "path": "/tmp/uploaded.csv",
        }),
    )
    .await;
    let (_, stored) = s3_ok(&router, case, "GET", &format!("/{BUCKET}/{key}"), &[]).await;
    assert_eq!(
        String::from_utf8_lossy(&stored),
        TEXT,
        "case {case:?}: expected the uploaded object to hold the file's bytes, got {:?}",
        String::from_utf8_lossy(&stored)
    );

    let case = "download_file writes the object to the path it is given";
    let key = "staged/for-file.csv";
    s3_ok(
        &router,
        case,
        "PUT",
        &format!("/{BUCKET}/{key}"),
        TEXT.as_bytes(),
    )
    .await;
    let reported = invoked(
        &router,
        case,
        function,
        &json!({
            "action": "download_file",
            "bucket": BUCKET,
            "key": key,
            "path": "/tmp/downloaded.csv",
        }),
    )
    .await;
    assert_eq!(
        reported["read"],
        json!(TEXT),
        "case {case:?}: expected the downloaded file to hold the object's own bytes, got \
         {reported}"
    );

    // A path OUTSIDE the function's own storage: the path a transfer opens is the handler's
    // own value, and which paths a run may open at all is the runtime's file layer's to
    // decide — so a transfer named a host path is refused there, with nothing read into an
    // object and nothing written outside the function's storage. One row per direction, since
    // one of them READS a path and the other WRITES one.
    for (case, action, key, path) in [
        (
            "upload_file named a path outside the function's storage",
            "upload_file",
            "written/from-outside.csv",
            "/etc/passwd",
        ),
        (
            "download_file named a path outside the function's storage",
            "download_file",
            "staged/for-file.csv",
            "/etc/emulator-escape.csv",
        ),
    ] {
        let reported = invoked(
            &router,
            case,
            function,
            &json!({
                "action": action,
                "bucket": BUCKET,
                "key": key,
                "text": TEXT,
                "path": path,
            }),
        )
        .await;
        assert_eq!(
            reported["ok"],
            json!(false),
            "case {case:?}: expected {path:?} to be refused, got {reported}"
        );
        assert!(
            reported["refused"]
                .as_str()
                .is_some_and(|refused| !refused.is_empty()),
            "case {case:?}: expected the refusal the runtime's file layer states, got {reported}"
        );
        assert_eq!(
            reported["read"],
            Value::Null,
            "case {case:?}: a refused transfer reads nothing, got {reported}"
        );
    }
    // And the upload row's object was never written: the refusal happened before any call.
    let case = "upload_file named a path outside the function's storage";
    let (status, _, answered) = s3(
        &router,
        "GET",
        &format!("/{BUCKET}/written/from-outside.csv"),
        &[],
    )
    .await;
    assert_eq!(
        status,
        404,
        "case {case:?}: a refused transfer writes no object, got {status} with {}",
        String::from_utf8_lossy(&answered)
    );
}
