// SPDX-License-Identifier: CC0-1.0

//! The FORM an answer's byte-carrying member reaches a handler's SDK in.
//!
//! RULE: a member an answer binds to the WHOLE response body whose shape is a `blob` is that
//! body's own bytes, and WHICH form the SDK hands them over in is decided by the one trait on
//! that blob. A `@streaming` blob is handed over UNREAD, as a stream the caller reads; a blob
//! the model does not mark streaming is a body the SDK COLLECTS, which the JavaScript SDK
//! hands the caller as the bytes themselves — a byte array, which `Buffer.from` takes and
//! which carries a length and an index — while the Python SDK wraps the payload of every
//! output whose payload shape is a blob in a streaming body.
//!
//! AWS Lambda's own `Invoke` is the first: `InvocationResponse$Payload` binds the whole body
//! (`@httpPayload`) and targets `Blob`, which carries no `@streaming` trait, so a JavaScript
//! caller reads the answer's bytes themselves. Amazon S3's `GetObject` is the second: its
//! `Body` targets a `@streaming` blob, so it stays the stream a caller reads. A handler that
//! is handed a stream where the SDK it was written against hands over bytes fails on the first
//! thing it does with them.
//!
//! The rows differ along what the rule generalises over: which trait the payload blob carries
//! (collected or streaming), and which SDK the runtime supplies read it (the JavaScript one
//! and the Python one, whose forms differ). A member of the same answers that is TEXT is read
//! beside each, so what the rule decides is the byte-carrying member and not the answer.
//!
//! The suites RUN handlers, so they take the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]): this emulator serves at
//! most `max_concurrent_runs` invocations at once for the whole process.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{create_function, send, zip_file, Answer, ROLE};
use crate::node_loader_errors::create_node_function_within;

/// Where `Invoke` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The bucket the handler writes and reads.
const BUCKET: &str = "payload-form-archive";

/// The key it writes, whose `/` makes the object's key a path rather than one segment.
const KEY: &str = "orders/one.txt";

/// The object's own bytes, which the handler reads back off the streaming payload.
const OBJECT_BODY: &str = "first order";

/// What the caller asks the target, which comes back inside the collected payload.
const ASKED: &str = "the question";

/// The target of the invocation: it answers the event it was handed, so what a caller reads
/// out of the payload is this handler's own answer.
const TARGET_SOURCE: &str = "exports.handler = async (event) => ({ echoed: event.asked });\n";

/// Lambda and Amazon S3 over ONE shared engine, as `core/router/src/lib.rs` mounts them: the
/// handler's own SDK calls are dispatched back into this same router.
fn emulator(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = crate::function_lifecycle::router_on(engine);
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(engine),
    ));
    router
}

/// `Invoke` on `name`, carrying `event`.
async fn invoke(router: &MultiServiceRouter, name: &str, event: &Value) -> Answer {
    send(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        "application/octet-stream",
        serde_json::to_vec(event).expect("the event serializes"),
        &[],
    )
    .await
}

/// Create the bucket the handler writes into, through Amazon S3's OWN `CreateBucket` — the
/// SDK this runtime supplies binds the object calls and not a bucket's create, so the bucket
/// is made the way a client makes one.
async fn given_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("http://s3.us-east-1.amazonaws.com/{bucket}"))
        .header("host", "s3.us-east-1.amazonaws.com")
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/us-east-1/s3/aws4_request, \
             SignedHeaders=host, Signature=not-verified",
        )
        .body(bytes::Bytes::new())
        .expect("the bucket request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    assert_eq!(
        status,
        200,
        "case {case:?}: expected CreateBucket to answer 200, got {status} {}",
        String::from_utf8_lossy(&response.into_body())
    );
}

/// The node handler: it makes one call of each binding class and reports the FORM the SDK gave
/// each byte-carrying member, beside what reading it answers.
const NODE_SOURCE: &str = r#"
'use strict';

const { LambdaClient, InvokeCommand } = require('@aws-sdk/client-lambda');
const { S3Client, PutObjectCommand, GetObjectCommand } = require('@aws-sdk/client-s3');

// Which form the SDK handed a value over in: a byte array, a stream the caller reads, or text.
function form(value) {
    if (typeof value === 'string') {
        return 'string';
    }
    if (value instanceof Uint8Array) {
        return 'bytes';
    }
    if (value !== null && typeof value === 'object'
        && typeof value.transformToString === 'function') {
        return 'stream';
    }
    return typeof value;
}

exports.handler = async (event) => {
    const steps = {};
    const lambda = new LambdaClient({ region: process.env.AWS_REGION });
    const answered = await lambda.send(new InvokeCommand({
        FunctionName: event.target,
        Payload: JSON.stringify({ asked: event.asked })
    }));
    const payload = answered.Payload;
    steps.collected = {
        form: form(payload),
        text: Buffer.from(payload).toString('utf8'),
        viaTransform: payload.transformToString(),
        transformedLength: payload.transformToByteArray().length,
        length: payload.length,
        statusCode: answered.StatusCode,
        executedVersion: form(answered.ExecutedVersion)
    };
    const s3 = new S3Client({ region: process.env.AWS_REGION });
    await s3.send(new PutObjectCommand({
        Bucket: event.bucket,
        Key: event.key,
        Body: Buffer.from(event.body, 'utf8'),
        ContentType: 'text/plain'
    }));
    const read = await s3.send(new GetObjectCommand({
        Bucket: event.bucket,
        Key: event.key
    }));
    steps.streaming = {
        form: form(read.Body),
        text: await read.Body.transformToString(),
        contentType: form(read.ContentType)
    };
    return steps;
};
"#;

/// The python handler: the same collected payload, through the other SDK the runtime supplies.
const PYTHON_SOURCE: &str = r#"
import json

import boto3


def lambda_handler(event, context):
    client = boto3.client("lambda")
    answered = client.invoke(
        FunctionName=event["target"],
        Payload=json.dumps({"asked": event["asked"]}).encode(),
    )
    payload = answered["Payload"]
    return {
        "reads_as_stream": hasattr(payload, "read"),
        "text": payload.read().decode(),
        "status": answered["StatusCode"],
    }
"#;

/// How long a function this suite creates may RUN for, in seconds.
///
/// Generous on purpose: the timeout is a limit of the function and not one this suite asserts,
/// so a run slowed by a loaded host is answered by the handler rather than by it. Far below the
/// span a suite of this target may take.
const RUN_TIMEOUT_SECONDS: i64 = 120;

/// Create the `nodejs20.x` function `name` running `source` as its `index.js`, through the one
/// creator of a node function in this target.
async fn create_node_caller(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    name: &str,
    source: &str,
) {
    create_node_function_within(
        router,
        engine,
        name,
        &[("index.js", source)],
        "index.handler",
        RUN_TIMEOUT_SECONDS,
    )
    .await;
}

#[tokio::test]
async fn a_node_handler_is_handed_a_collected_payload_as_bytes_and_a_streaming_one_as_a_stream() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "a node handler reading each payload class";
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator(&engine);
    let target = "payload-form-target";
    let caller = "payload-form-node-caller";
    create_node_caller(&router, &engine, target, TARGET_SOURCE).await;
    create_node_caller(&router, &engine, caller, NODE_SOURCE).await;
    given_bucket(&router, case, BUCKET).await;

    let answered = invoke(
        &router,
        caller,
        &json!({
            "target": target,
            "asked": ASKED,
            "bucket": BUCKET,
            "key": KEY,
            "body": OBJECT_BODY,
        }),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
        answered.status,
        String::from_utf8_lossy(&answered.body)
    );
    assert_eq!(
        answered.header("x-amz-function-error"),
        None,
        "case {case:?}: expected the run to answer rather than fail, got {}",
        String::from_utf8_lossy(&answered.body)
    );
    let steps = answered.json();

    // What the target answered, as the runtime serialises a handler's return value: the text
    // the collected payload carries.
    let echoed = json!({ "echoed": ASKED }).to_string();
    let rows: &[(&str, &str, Value)] = &[
        (
            "a blob bound to the whole body that the model does not mark @streaming",
            "collected",
            json!({
                "form": "bytes",
                "text": echoed,
                "viaTransform": echoed,
                "transformedLength": echoed.len(),
                "length": echoed.len(),
                "statusCode": 200,
                "executedVersion": "string",
            }),
        ),
        (
            "a @streaming blob bound to the whole body",
            "streaming",
            json!({
                "form": "stream",
                "text": OBJECT_BODY,
                "contentType": "string",
            }),
        ),
    ];
    for (row, step, expected) in rows {
        let actual = steps.get(step).cloned().unwrap_or(Value::Null);
        assert_eq!(
            &actual, expected,
            "case {row:?}: expected {expected}, got {actual} (the whole run: {steps})"
        );
    }
}

#[tokio::test]
async fn a_python_handler_is_handed_the_same_collected_payload_as_a_streaming_body() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "a python handler reading the collected payload";
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator(&engine);
    let target = "payload-form-target";
    let caller = "payload-form-python-caller";
    create_node_caller(&router, &engine, target, TARGET_SOURCE).await;
    let created = create_function(
        &router,
        &json!({
            "FunctionName": caller,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "lambda_function.lambda_handler",
            "Timeout": RUN_TIMEOUT_SECONDS,
            "MemorySize": 256,
            "Code": {
                "ZipFile": zip_file(&crate::synchronous_invoke_run::package(PYTHON_SOURCE)),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: expected CreateFunction to answer 201, got {} {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    crate::function_lifecycle::make_activation_due(&engine);

    let answered = invoke(
        &router,
        caller,
        &json!({ "target": target, "asked": ASKED }),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
        answered.status,
        String::from_utf8_lossy(&answered.body)
    );
    let actual = answered.json();
    let expected = json!({
        "reads_as_stream": true,
        "text": json!({ "echoed": ASKED }).to_string(),
        "status": 200,
    });
    assert_eq!(
        actual, expected,
        "case {case:?}: expected {expected}, got {actual}"
    );
}
