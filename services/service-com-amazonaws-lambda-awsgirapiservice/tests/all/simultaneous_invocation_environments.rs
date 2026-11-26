// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for HOW MANY execution environments a burst of simultaneous invocations
//! is served by.
//!
//! The rule under test: an execution environment serves ONE invocation at a time — "Lambda
//! maintains the execution environment for some time in anticipation of another invocation",
//! and an invocation that finds no environment of its own free runs in a new one — so
//! invocations that are IN FLIGHT TOGETHER are each served in an environment of their own:
//! N invocations overlapping within the function's and the account's concurrency limits are
//! served by N distinct environments, each with its own interpreter state and its own log
//! stream, and none of them waits for another's environment.
//!
//! The rows differ along what the rule generalises over: how many invocations overlap.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ACTIVATION_DELAY;
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::invoke_request_id::one_invocation_stream_at_a_time;
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

/// The module every package below holds its handler in.
const HANDLER: &str = "lambda_function.lambda_handler";

/// How long past a due instant this suite waits before calling. The transition is owed by the
/// CLOCK, so nothing here sleeps on the emulator's behalf.
const SLACK: Duration = Duration::from_millis(150);

/// The `Timeout` the function carries, in seconds: far above what its handler does, so what a
/// row asserts is the environments and never a run its own deadline ended.
const FUNCTION_TIMEOUT_SECONDS: u32 = 30;

/// How long each handler of a burst holds its environment, in seconds: long enough that every
/// caller of the row is really in flight while the others arrive, and short because the runs
/// of this process share one memory envelope.
const HELD_SECONDS: f64 = 0.5;

/// A handler that reports WHICH environment served the invocation — a value its module makes
/// once, while it is being initialized, so two invocations report the same one exactly when
/// one environment served both — and the log stream that environment writes into, and holds
/// that environment for as long as its event asks.
const REPORTS_ENVIRONMENT: &str = "import os\n\
     import time\n\
     import uuid\n\
     \n\
     ENVIRONMENT = uuid.uuid4().hex\n\
     \n\
     \n\
     def lambda_handler(event, context):\n\
     \x20   time.sleep(event['seconds'])\n\
     \x20   return {\n\
     \x20       'environment': ENVIRONMENT,\n\
     \x20       'stream': os.environ.get('AWS_LAMBDA_LOG_STREAM_NAME', ''),\n\
     \x20       'caller': event['caller'],\n\
     \x20   }\n";

/// What a dispatched request answered.
struct Answer {
    status: u16,
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

    /// A member of the document the handler returned, as text, and the empty string where the
    /// handler answered none.
    fn member(&self, name: &str) -> String {
        self.json()
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
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
        body,
    }
}

/// Create a function whose code is `source` and wait out the provisioning window the create
/// documents.
async fn create_function(router: &MultiServiceRouter, name: &str, source: &str) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT_SECONDS,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(source)),
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
    tokio::time::sleep(ACTIVATION_DELAY + SLACK).await;
}

/// `count` invocations of `name` that are all in flight at once, each holding the environment
/// that serves it for [`HELD_SECONDS`], answered in caller order.
///
/// A CLONE of the router per caller: they share the one engine and the one service instance,
/// so on this multi-thread runtime the invocations really do overlap.
async fn simultaneously(router: &MultiServiceRouter, name: &str, count: u32) -> Vec<Answer> {
    let mut callers = tokio::task::JoinSet::new();
    for caller in 0..count {
        let router = router.clone();
        let uri = format!("{FUNCTIONS_URI}/{name}/invocations");
        let event = json!({ "seconds": HELD_SECONDS, "caller": caller });
        callers.spawn(async move {
            let body = serde_json::to_vec(&event).expect("the event serializes");
            (caller, call(&router, "POST", &uri, &body).await)
        });
    }
    let mut answers: Vec<(u32, Answer)> = Vec::with_capacity(count as usize);
    while let Some(joined) = callers.join_next().await {
        answers.push(joined.expect("an invoking task must not panic"));
    }
    answers.sort_by_key(|(caller, _)| *caller);
    answers.into_iter().map(|(_, answer)| answer).collect()
}

/// Invocations in flight together are served by as many execution environments as there are of
/// them, and each of those environments has a log stream of its own.
///
/// One row per size of the overlapping burst.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_invocations_are_each_served_in_their_own_environment() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let name = "burst-environments";
    create_function(&router, name, REPORTS_ENVIRONMENT).await;

    for count in [2_u32, 3, 6] {
        let case = format!("{count} simultaneous invocations");
        let answers = simultaneously(&router, name, count).await;
        let mut environments: BTreeSet<String> = BTreeSet::new();
        let mut streams: BTreeSet<String> = BTreeSet::new();
        for (caller, answered) in answers.iter().enumerate() {
            assert_eq!(
                answered.status,
                200,
                "case {case}: caller {caller} must be served, got {} {}",
                answered.status,
                answered.text()
            );
            let environment = answered.member("environment");
            assert!(
                !environment.is_empty(),
                "case {case}: caller {caller} must report the environment that served it, \
                 got {}",
                answered.text()
            );
            environments.insert(environment);
            let stream = answered.member("stream");
            assert!(
                !stream.is_empty(),
                "case {case}: caller {caller} must report its environment's log stream, got {}",
                answered.text()
            );
            streams.insert(stream);
        }
        assert_eq!(
            environments.len(),
            count as usize,
            "case {case}: expected {count} distinct execution environments, got \
             {environments:?}"
        );
        assert_eq!(
            streams.len(),
            count as usize,
            "case {case}: expected one log stream per environment, so {count} of them, got \
             {streams:?}"
        );
    }
}
