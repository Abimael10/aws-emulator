// SPDX-License-Identifier: CC0-1.0

//! The request harness a suite that drives a CloudFormation stack over its CodePipeline and
//! AWS Lambda resources needs: the whole router on one engine, the SigV4 header those
//! requests carry, one driver per wire protocol in play (CloudFormation speaks awsQuery,
//! CodePipeline awsJson1_1, AWS Lambda restJson1), and the poll every wait for a clock-owed
//! stack transition is made of.
//!
//! It lives here, ONCE, for the reason `query_wire.rs` gives for the wire readers it owns: a
//! suite that writes its own drifts from the others, and two suites asserting the same rule
//! through two harnesses are no longer asserting the same thing. The percent-encoder and the
//! XML element readers come from `query_wire.rs` rather than being written again here.
//!
//! Each suite keeps what is its own: its tables, its templates and the assertions it makes
//! about an answer.

use std::sync::Arc;
use std::time::Duration;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::form_encode;

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
pub(crate) const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every request built here is signed for.
pub(crate) const REGION: &str = "us-east-1";

/// How long a wait for a clock-owed stack transition is given before the reading it last took
/// is the one asserted on. Generous, because what is waited for is the CONDITION and not a
/// span of the clock: a loaded host reaches the same state later than an idle one.
pub(crate) const STACK_DEADLINE: Duration = Duration::from_secs(120);

/// How long one look at a clock-owed condition waits before the next; see [`until`].
pub(crate) const STACK_POLL: Duration = Duration::from_millis(50);

/// CodePipeline's `X-Amz-Target` prefix, from that service's own model (service shape
/// `CodePipeline_20150709`).
pub(crate) const CODEPIPELINE_TARGET_PREFIX: &str = "CodePipeline_20150709";

/// The stack status a create that deployed everything settles in.
pub(crate) const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// ...and the one a create that deployed nothing settles in.
pub(crate) const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// Take `read` again until `settled` holds of what it answered, or until [`STACK_DEADLINE`]
/// has passed, and answer the last reading taken.
///
/// Every wait a suite built on this harness makes is for a state some clock-owed work of the
/// emulator reaches, so it is a poll of that state and never a sleep of a fixed length: the
/// caller asserts on the reading this answers, which is the settled one wherever the work ran
/// inside the deadline.
pub(crate) fn until<T>(mut read: impl FnMut() -> T, settled: impl Fn(&T) -> bool) -> T {
    let deadline = std::time::Instant::now() + STACK_DEADLINE;
    loop {
        let reading = read();
        if settled(&reading) || std::time::Instant::now() >= deadline {
            return reading;
        }
        std::thread::sleep(STACK_POLL);
    }
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` in [`REGION`],
/// which is what routes the request and what each handler reads its Region from. The
/// signature is never checked: this router runs with request authentication off.
pub(crate) fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=deadbeef"
    )
}

/// The emulator a suite drives: the whole router over one engine, and the runtime its
/// requests are blocked on.
pub(crate) struct Emulator {
    router: MultiServiceRouter,
    runtime: tokio::runtime::Runtime,
}

impl Emulator {
    pub(crate) fn new() -> Self {
        let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
        let mut router = build_router(engine);
        router.set_request_authentication(false);
        Self {
            router,
            runtime: tokio::runtime::Builder::new_current_thread()
                // The time driver, because the emulator's services run CLOCKS: a service
                // whose state owes a transition waits on a timer, and a timer on a runtime
                // built without this panics the task that waits on it.
                .enable_time()
                .build()
                .expect("a current-thread runtime"),
        }
    }

    /// Drive one request and answer its status and body.
    pub(crate) fn send(
        &self,
        signing_name: &str,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> (u16, Vec<u8>) {
        let host = format!("{signing_name}.{REGION}.amazonaws.com");
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{uri}"))
            .header("host", host)
            .header("authorization", authorization(signing_name));
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(bytes::Bytes::from(body))
            .expect("a well-formed request");
        let response = self.runtime.block_on(self.router.dispatch(request));
        let status = response.status().as_u16();
        (status, response.into_body().to_vec())
    }

    /// One CloudFormation action (awsQuery, API version 2010-05-15).
    pub(crate) fn cloudformation(&self, action: &str, members: &[(&str, &str)]) -> (u16, String) {
        let mut body = format!("Action={action}&Version=2010-05-15");
        for (key, value) in members {
            body.push('&');
            body.push_str(&form_encode(key));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let (status, payload) = self.send(
            "cloudformation",
            "POST",
            "/",
            &[("content-type", "application/x-www-form-urlencoded")],
            body.into_bytes(),
        );
        (status, String::from_utf8_lossy(&payload).into_owned())
    }

    /// One awsJson1_1 CodePipeline operation.
    pub(crate) fn codepipeline(&self, operation: &str, body: Value) -> (u16, Value) {
        let (status, payload) = self.send(
            "codepipeline",
            "POST",
            "/",
            &[
                ("content-type", "application/x-amz-json-1.1"),
                (
                    "x-amz-target",
                    &format!("{CODEPIPELINE_TARGET_PREFIX}.{operation}"),
                ),
            ],
            serde_json::to_vec(&body).expect("the request body serializes"),
        );
        let parsed = if payload.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&payload).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// AWS Lambda's read of one function by name (`GetFunction`, restJson1).
    pub(crate) fn get_function(&self, name: &str) -> (u16, String) {
        let (status, payload) = self.send(
            "lambda",
            "GET",
            &format!("/2015-03-31/functions/{name}"),
            &[],
            Vec::new(),
        );
        (status, String::from_utf8_lossy(&payload).into_owned())
    }
}
