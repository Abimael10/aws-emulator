// SPDX-License-Identifier: CC0-1.0

//! Which deployment package serves an invocation while a code update is still being
//! applied, end to end over HTTP.
//!
//! The rule under test: **a function's deployment package takes over only when the update
//! that deployed it COMPLETES** — `UpdateFunctionConfiguration` documents the window as
//! "When you update a function, Lambda provisions an instance of the function and its
//! supporting resources. [...] During this time, you can't modify the function, but you can
//! still invoke it", so an invocation that arrives while `LastUpdateStatus` is `InProgress`
//! is served, and what runs is the package the function was already serving. What the
//! function REPORTS (`CodeSha256`, `CodeSize`) is the new package from the instant the
//! update is accepted, so the two readings differ for exactly the span of the window.
//!
//! The rows differ along what the rule generalises over: the qualifier the invocation
//! addresses (the bare name, an explicit `$LATEST`, a published version, an alias — the last
//! two froze their own package, so no update of the unpublished version changes what they
//! run), the operation the invocation is made through (`Invoke`,
//! `InvokeWithResponseStream`), and how many updates have been applied in succession, so the
//! package that keeps serving is the one the previous update installed rather than the one
//! the function was created with.
//!
//! Every row drives real HTTP requests through the router and reads what the handler
//! RETURNED, so a package this emulator reported but never ran fails here. The clock-owed
//! `Pending` -> `Active` and `InProgress` -> `Successful` moves are exercised by moving the
//! stored due instants into the past, so nothing here sleeps.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aws_smithy_eventstream::frame::{DecodedFrame, MessageFrameDecoder};
use aws_smithy_types::event_stream::HeaderValue;
use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ComAmazonawsLambdaAwsgirapiserviceState;
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::synchronous_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A SigV4 credential scope naming this service's signing name (`lambda`) in `us-east-1`.
/// Nothing in it is verified: the harness turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=emulator-test-key/20260101/us-east-1/lambda/aws4_request, \
     SignedHeaders=host, Signature=not-verified";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The module the handler of every package below lives in.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The label the unpublished version is stored under.
const LATEST: &str = "$LATEST";

/// The alias every function below carries, pointing at published version `1`.
const ALIAS: &str = "live";

/// The `LastUpdateStatus` of an update still being applied, from the model's
/// `LastUpdateStatus` enum (`Failed | InProgress | Successful`).
const IN_PROGRESS: &str = "InProgress";

/// The `LastUpdateStatus` of a settled update, from the same enum.
const SUCCESSFUL: &str = "Successful";

/// What a dispatched request answered.
struct Answer {
    status: u16,
    headers: http::HeaderMap,
    body: bytes::Bytes,
}

impl Answer {
    /// The body as JSON, or `Value::Null` for an empty body.
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|error| panic!("expected a JSON body, got {error}: {}", self.text()))
    }

    /// A `String` member of the JSON body.
    fn string(&self, member: &str) -> Option<String> {
        self.json()
            .get(member)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// The body text, for a failure message.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// One response header.
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    /// The bytes the `PayloadChunk` frames of this event-stream body carry, in order — the
    /// handler's own response, as the client reassembles it.
    fn streamed_payload(&self, case: &str) -> Vec<u8> {
        let mut decoder = MessageFrameDecoder::new();
        let mut remaining: &[u8] = &self.body;
        let mut payload = Vec::new();
        while !remaining.is_empty() {
            match decoder.decode_frame(&mut remaining) {
                Ok(DecodedFrame::Complete(message)) => {
                    let event_type = message
                        .headers()
                        .iter()
                        .find(|header| header.name().as_str() == ":event-type")
                        .and_then(|header| match header.value() {
                            HeaderValue::String(value) => Some(value.as_str().to_string()),
                            _ => None,
                        });
                    if event_type.as_deref() == Some("PayloadChunk") {
                        payload.extend_from_slice(message.payload());
                    }
                }
                Ok(DecodedFrame::Incomplete) => {
                    panic!("case {case}: the event stream ends mid-frame")
                }
                Err(error) => panic!(
                    "case {case}: the event stream is not decodable: {error}: {:?}",
                    self.text()
                ),
            }
        }
        payload
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

/// A handler whose answer NAMES the deployment it was shipped in, so what an invocation
/// returns says which package ran.
fn source_naming(deployment: &str) -> String {
    format!("def lambda_handler(event, context):\n    return {{'deployment': '{deployment}'}}\n")
}

/// Create one function whose handler names `deployment`, publish version `1`, point the
/// alias at it, and settle the provisioning window the create opened.
async fn create_function(
    engine: &Arc<engine::InMemoryEngine>,
    router: &MultiServiceRouter,
    name: &str,
    deployment: &str,
) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Publish": true,
            "Timeout": 5,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&source_naming(deployment))),
            },
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
    let alias = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/aliases"),
        &serde_json::to_vec(&json!({ "Name": ALIAS, "FunctionVersion": "1" }))
            .expect("the alias body serializes"),
    )
    .await;
    assert_eq!(
        alias.status,
        201,
        "CreateAlias on {name:?} must succeed, got {} {}",
        alias.status,
        alias.text()
    );
    make_activation_due(engine);
}

/// `UpdateFunctionCode` shipping a handler that names `deployment`.
async fn update_code(router: &MultiServiceRouter, name: &str, deployment: &str) -> Answer {
    call(
        router,
        "PUT",
        &format!("{FUNCTIONS_URI}/{name}/code"),
        &serde_json::to_vec(&json!({
            "ZipFile": base64::engine::general_purpose::STANDARD
                .encode(package(&source_naming(deployment))),
        }))
        .expect("the update body serializes"),
    )
    .await
}

/// `GetFunctionConfiguration` on the unpublished version of `name`.
async fn configuration(router: &MultiServiceRouter, name: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("{FUNCTIONS_URI}/{name}/configuration"),
        &[],
    )
    .await
}

/// Which invoking operation a row drives the invocation through. Both run the handler of
/// the version the request resolves, so both read the package that is serving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    /// `POST /2015-03-31/functions/{FunctionName}/invocations`.
    Invoke,
    /// `POST /2021-11-15/functions/{FunctionName}/response-streaming-invocations`.
    ResponseStream,
}

/// Invoke the function `reference` names through `entry`, answering the `deployment` member
/// of what the handler returned.
///
/// A response-streaming invocation carries its answer as the `PayloadChunk` frames of an
/// event stream, so what the handler returned is the bytes those frames carry.
async fn invoked_deployment(
    router: &MultiServiceRouter,
    entry: Entry,
    case: &str,
    reference: &str,
) -> String {
    let answer = match entry {
        Entry::Invoke => {
            call(
                router,
                "POST",
                &format!("{FUNCTIONS_URI}/{reference}/invocations"),
                b"{}",
            )
            .await
        }
        Entry::ResponseStream => {
            call(
                router,
                "POST",
                &format!("/2021-11-15/functions/{reference}/response-streaming-invocations"),
                b"{}",
            )
            .await
        }
    };
    assert_eq!(
        answer.status,
        200,
        "case {case}: the invocation of {reference:?} through {entry:?} must be served, got {} {}",
        answer.status,
        answer.text()
    );
    assert_eq!(
        answer.header("x-amz-function-error"),
        None,
        "case {case}: the invocation of {reference:?} through {entry:?} must not report a \
         function error, got {}",
        answer.text()
    );
    let document = match entry {
        Entry::Invoke => answer.body.to_vec(),
        Entry::ResponseStream => answer.streamed_payload(case),
    };
    let value: Value = serde_json::from_slice(&document).unwrap_or_else(|error| {
        panic!(
            "case {case}: expected the handler's JSON answer, got {error}: {}",
            String::from_utf8_lossy(&document)
        )
    });
    value
        .get("deployment")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case}: expected the handler to name its deployment, got {value}")
        })
        .to_string()
}

/// Move every stored function's activation instant into the past, so the next read
/// materialises the `Pending` -> `Active` move the clock owes. This is what waiting would
/// do, without waiting.
fn make_activation_due(engine: &Arc<engine::InMemoryEngine>) {
    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let mut store = state.lock();
    let past = SystemTime::now() - Duration::from_secs(1);
    for function in store.functions.values_mut() {
        function.active_due = past;
    }
}

/// Move every in-flight update's settle instant into the past, so the next read
/// materialises the `InProgress` -> `Successful` move the clock owes.
fn make_update_settled(engine: &Arc<engine::InMemoryEngine>) {
    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let mut store = state.lock();
    let past = SystemTime::now() - Duration::from_secs(1);
    for function in store.functions.values_mut() {
        if function.last_update_due.is_some() {
            function.last_update_due = Some(past);
        }
    }
}

/// Assert that `name` reports `expected` as its `LastUpdateStatus`.
async fn assert_last_update_status(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    expected: &str,
) {
    let answer = configuration(router, name).await;
    assert_eq!(
        answer.status,
        200,
        "case {case}: expected 200 from GetFunctionConfiguration, got {} {}",
        answer.status,
        answer.text()
    );
    assert_eq!(
        answer.string("LastUpdateStatus").as_deref(),
        Some(expected),
        "case {case}: expected LastUpdateStatus {expected:?}, got {:?}",
        answer.string("LastUpdateStatus")
    );
}

/// One row: the case, how the invocation addresses the function, what it must run inside
/// the update's window, and what it must run once the window has closed.
type ServingRow = (&'static str, fn(&str) -> String, &'static str, &'static str);

/// RULE: while a code update is still being applied, the invocation is SERVED by the
/// package that was already serving; the new one takes over when the update completes.
///
/// One row per qualifier the invocation addresses, crossed with each invoking operation.
#[tokio::test]
async fn an_invocation_inside_the_update_window_runs_the_package_that_was_already_serving() {
    // A published version and an alias froze their own package, so an update of the
    // unpublished version never changes what they run — before, during or after the window.
    let rows: &[ServingRow] = &[
        ("the bare name", |name| name.to_string(), "v1", "v2"),
        (
            "an explicit $LATEST qualifier",
            |name| format!("{name}:{LATEST}"),
            "v1",
            "v2",
        ),
        (
            "the published version",
            |name| format!("{name}:1"),
            "v1",
            "v1",
        ),
        ("the alias", |name| format!("{name}:{ALIAS}"), "v1", "v1"),
    ];

    for (index, (case, reference, inside, after)) in rows.iter().enumerate() {
        for entry in [Entry::Invoke, Entry::ResponseStream] {
            // One function per row, so the window a row opens is the only one in flight.
            let engine = Arc::new(engine::InMemoryEngine::new());
            let router = router_on(&engine);
            let name = format!("serving-package-{index}-{}", usize::from(entry == Entry::Invoke));
            let case = format!("{case} through {entry:?}");
            create_function(&engine, &router, &name, "v1").await;

            let addressed = reference(&name);
            let before = invoked_deployment(&router, entry, &case, &addressed).await;
            assert_eq!(
                before, "v1",
                "case {case}: before any update the function must run its created package, got \
                 {before:?}"
            );
            let created_digest = configuration(&router, &name)
                .await
                .string("CodeSha256")
                .unwrap_or_default();

            let updated = update_code(&router, &name, "v2").await;
            assert_eq!(
                updated.status,
                200,
                "case {case}: UpdateFunctionCode must be accepted, got {} {}",
                updated.status,
                updated.text()
            );
            assert_last_update_status(&router, &case, &name, IN_PROGRESS).await;

            // What the function REPORTS is the new package from the instant the update was
            // accepted, which is what makes the two readings differ for the window's span.
            let reported = configuration(&router, &name)
                .await
                .string("CodeSha256")
                .unwrap_or_default();
            assert_eq!(
                reported,
                updated.string("CodeSha256").unwrap_or_default(),
                "case {case}: the function must report the digest the update answered, got \
                 {reported:?}"
            );
            assert_ne!(
                reported, created_digest,
                "case {case}: the reported digest must change with the accepted update, got \
                 {reported:?}"
            );

            // ... and what an invocation RUNS inside the window is the package that was
            // already serving.
            let served = invoked_deployment(&router, entry, &case, &addressed).await;
            assert_eq!(
                served, *inside,
                "case {case}: inside the update window the invocation must run {inside:?}, got \
                 {served:?}"
            );

            make_update_settled(&engine);
            assert_last_update_status(&router, &case, &name, SUCCESSFUL).await;
            let settled = invoked_deployment(&router, entry, &case, &addressed).await;
            assert_eq!(
                settled, *after,
                "case {case}: once the update has completed the invocation must run {after:?}, \
                 got {settled:?}"
            );
        }
    }
}

/// RULE, over a SUCCESSION of updates: what keeps serving inside a window is the package
/// the PREVIOUS update installed, not the one the function was created with.
#[tokio::test]
async fn each_update_window_is_served_by_the_package_the_previous_update_installed() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let name = "serving-package-chain";
    create_function(&engine, &router, name, "v1").await;

    // Each step deploys the next package: inside its window the one before it serves, and
    // once the window closes the deployed one does.
    for (deployed, previous) in [("v2", "v1"), ("v3", "v2"), ("v4", "v3")] {
        let case = format!("the update deploying {deployed}");
        let updated = update_code(&router, name, deployed).await;
        assert_eq!(
            updated.status,
            200,
            "case {case}: UpdateFunctionCode must be accepted, got {} {}",
            updated.status,
            updated.text()
        );
        assert_last_update_status(&router, &case, name, IN_PROGRESS).await;
        let served = invoked_deployment(&router, Entry::Invoke, &case, name).await;
        assert_eq!(
            served, previous,
            "case {case}: inside the window the invocation must run {previous:?}, got {served:?}"
        );

        make_update_settled(&engine);
        assert_last_update_status(&router, &case, name, SUCCESSFUL).await;
        let settled = invoked_deployment(&router, Entry::Invoke, &case, name).await;
        assert_eq!(
            settled, deployed,
            "case {case}: once the window has closed the invocation must run {deployed:?}, got \
             {settled:?}"
        );
    }
}
