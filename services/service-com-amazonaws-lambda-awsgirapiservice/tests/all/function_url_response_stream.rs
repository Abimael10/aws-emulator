// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a function URL whose `InvokeMode` is `RESPONSE_STREAM`.
//!
//! RULE: "RESPONSE_STREAM – Your function streams payload results as they become available",
//! so such a URL answers the bytes the handler WROTE, in the order it wrote them, rather than
//! the response a value it returned describes. A handler that announced the response ahead of
//! its first byte — `awslambda.HttpResponseStream.from(stream, metadata)` — has that
//! announcement's `statusCode`, `headers` and `cookies` shape the answer, with the bytes after
//! it as the body; one that announced nothing has its bytes served whole. A stream that has
//! been begun cannot be withdrawn, so a handler that fails AFTER its first write still answers
//! those bytes, cut short where it stopped, while a stream that was never begun is no response
//! at all and is the gateway failure every unusable response is.
//!
//! The rows differ along what the rule generalises over: whether the handler announced the
//! response, what it announced, how many writes it made, and where in the stream it failed.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ACTIVATION_DELAY;
use smithy_server_runtime::MultiServiceRouter;

use crate::function_url_data_plane::{control, given_configured_url, router_on, send, Answer};
use crate::node_invoke_run::package;

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The runtime a response-streaming handler runs on.
const RUNTIME: &str = "nodejs20.x";

/// How long past a due instant this suite waits before calling. The transition is owed by the
/// CLOCK, so nothing here sleeps on the emulator's behalf.
const SLACK: Duration = Duration::from_millis(200);

/// The media type a streamed answer that announced none carries.
const STREAMED_CONTENT_TYPE: &str = "application/octet-stream";

/// The handler every row runs: it writes from the event's own `rawPath`, so one function
/// serves every row and each answer is the RUN's own bytes.
const HANDLER_SOURCE: &str = r#"
exports.handler = awslambda.streamifyResponse(async (event, responseStream, context) => {
    const path = event.rawPath;
    if (path === '/shaped') {
        const shaped = awslambda.HttpResponseStream.from(responseStream, {
            statusCode: 201,
            headers: { 'content-type': 'text/plain', 'x-written-by': 'the handler' },
            cookies: ['first=1', 'second=2']
        });
        shaped.write('shaped body');
        return;
    }
    if (path === '/truncated') {
        responseStream.write('part-0;');
        responseStream.write('part-1;');
        throw new Error('cut short');
    }
    if (path === '/never-written') {
        throw new Error('nothing was written');
    }
    responseStream.write('part-0;');
    responseStream.write('part-1;');
    responseStream.write('done');
});
"#;

/// Create the node function whose package RUNS the handler above, and wait out the
/// provisioning window.
async fn given_streaming_function(router: &MultiServiceRouter, name: &str) {
    let created = control(
        router,
        "POST",
        "/2015-03-31/functions",
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": RUNTIME,
            "Handler": "index.handler",
            "Timeout": 20,
            "MemorySize": 512,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&[("index.js", HANDLER_SOURCE)])),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateFunction {name:?} must succeed, got {}",
        created.summary()
    );
    tokio::time::sleep(ACTIVATION_DELAY + SLACK).await;
}

/// `UpdateFunctionUrlConfig` putting the URL into the response-streaming mode.
async fn stream_mode(router: &MultiServiceRouter, function: &str) {
    let updated = control(
        router,
        "PUT",
        &format!("/2021-10-31/functions/{function}/url"),
        &json!({ "InvokeMode": "RESPONSE_STREAM" }),
    )
    .await;
    assert_eq!(
        updated.status,
        200,
        "UpdateFunctionUrlConfig on {function:?} must succeed, got {}",
        updated.summary()
    );
}

/// RULE: a `RESPONSE_STREAM` URL answers the bytes the handler wrote, shaped by what it
/// announced ahead of them.
#[tokio::test]
async fn a_response_stream_url_answers_the_bytes_its_handler_wrote() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    given_streaming_function(&router, "url-streamed").await;
    let endpoint =
        given_configured_url(&router, "url-streamed", None, "AWS_IAM", &Value::Null).await;
    stream_mode(&router, "url-streamed").await;

    // (case, the path the handler writes from, the status, the body, the content type)
    let rows: &[(&str, &str, u16, &[u8], &str)] = &[
        (
            "a stream that announced nothing",
            "/plain",
            200,
            b"part-0;part-1;done",
            STREAMED_CONTENT_TYPE,
        ),
        (
            "a stream that announced its response",
            "/shaped",
            201,
            b"shaped body",
            "text/plain",
        ),
        (
            "a handler that failed after its first write",
            "/truncated",
            200,
            b"part-0;part-1;",
            STREAMED_CONTENT_TYPE,
        ),
        (
            // A stream that was never begun is no response: the handler wrote nothing, so
            // there are no bytes for a response to be made of.
            "a handler that wrote nothing",
            "/never-written",
            502,
            b"Internal Server Error",
            "application/json",
        ),
    ];
    for (case, path, status, body, content_type) in rows {
        let answer: Answer = send(&router, "GET", &endpoint.host, path, &[], &[]).await;
        assert_eq!(
            answer.status, *status,
            "case {case:?}: expected HTTP {status}, got {}",
            answer.summary()
        );
        assert_eq!(
            answer.body.as_ref(),
            *body,
            "case {case:?}: expected the body {:?}, got {:?}",
            String::from_utf8_lossy(body),
            answer.text()
        );
        assert_eq!(
            answer.header("content-type").as_deref(),
            Some(*content_type),
            "case {case:?}: expected the content type {content_type:?}, got {:?}",
            answer.header("content-type")
        );
    }

    // The announcement's own headers and cookies reach the client: one `set-cookie` per
    // cookie it named, beside the header it set.
    let shaped = send(&router, "GET", &endpoint.host, "/shaped", &[], &[]).await;
    assert_eq!(
        shaped.header("x-written-by").as_deref(),
        Some("the handler"),
        "case \"a stream that announced its response\": expected the announced header, got {:?}",
        shaped.header("x-written-by")
    );
    let cookies: Vec<String> = shaped
        .headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(str::to_string)
        .collect();
    assert_eq!(
        cookies,
        vec!["first=1".to_string(), "second=2".to_string()],
        "case \"a stream that announced its response\": expected one set-cookie per announced \
         cookie, got {cookies:?}"
    );
}

/// RULE: the mode is the URL's own. The SAME handler on a `BUFFERED` URL answers what a
/// buffered result describes, so a stream's bytes are not read as one.
#[tokio::test]
async fn the_invoke_mode_the_url_serves_in_decides_how_a_result_is_read() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    given_streaming_function(&router, "url-buffered").await;
    let endpoint =
        given_configured_url(&router, "url-buffered", None, "AWS_IAM", &Value::Null).await;

    // A `BUFFERED` URL (the documented default) reads the run's answer as a result document.
    // The handler above writes bytes that are no result document at all, so the response is
    // the gateway failure every unusable response is.
    let answer = send(&router, "GET", &endpoint.host, "/plain", &[], &[]).await;
    assert_eq!(
        answer.status,
        502,
        "case \"a stream's bytes on a BUFFERED url\": expected HTTP 502, got {}",
        answer.summary()
    );
}
