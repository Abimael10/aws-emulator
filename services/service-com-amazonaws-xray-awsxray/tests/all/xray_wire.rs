// SPDX-License-Identifier: CC0-1.0

//! The wire harness this crate's suites share: one emulator with an empty
//! store, addressed through the SAME front door the production router is.
//!
//! Every case in every suite drives a `MultiServiceRouter` with this service
//! mounted, addressed by the `xray` endpoint host and speaking restJson1, so the
//! assertions are about HTTP responses rather than internal state.

#![allow(dead_code)]

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_cloudwatchlogs_logs_20140328::state as logs;
use service_com_amazonaws_xray_awsxray::state::destination_switch::{
    SPANS_DELIVERY_ACTION, XRAY_SERVICE_PRINCIPAL,
};
use service_com_amazonaws_xray_awsxray::state::span_delivery::SPANS_LOG_GROUP;
use service_com_amazonaws_xray_awsxray::state::TRACE_PAGE_SECONDS;
use smithy_server_runtime::MultiServiceRouter;

/// One emulator with an empty store.
pub struct Xray {
    router: MultiServiceRouter,
}

/// The region every call lands in unless a case names another, which is also
/// the region the service falls back to when a request names none.
pub const DEFAULT_REGION: &str = "us-east-1";

/// The access key id every call is signed with unless a case names another: a key
/// neither STS nor IAM holds a record for, which is the position the account's own
/// published credential is in.
pub const DEFAULT_ACCESS_KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// One step of a trace query's window, in the epoch seconds every instant on
/// this wire travels as, read from the service's own statement of it so no
/// suite can drift from the implementation.
#[allow(clippy::cast_precision_loss)]
pub const HOUR: f64 = TRACE_PAGE_SECONDS as f64;

/// An instant well in the past that falls exactly on the absolute hour grid
/// (1_700_002_800 = 472_223 * 3600), so a case can place a window's bounds on a
/// boundary of that grid or deliberately off it. Far enough behind the clock
/// that no window a suite builds from it reaches the skew allowance the window
/// check applies.
///
/// A window built from this instant is read from NO clock, so a query over it is
/// the same query however long the host takes between two calls — which is what
/// a walk of its pages needs, a pagination token being a cursor into the query
/// that issued it, the window included.
pub const ALIGNED: f64 = 1_700_002_800.0;

/// What one call answered.
#[derive(Debug)]
pub struct Answer {
    pub status: u16,
    /// restJson1 resolves an error shape from `x-amzn-errortype`.
    pub error_type: Option<String>,
    pub body: Value,
}

impl Answer {
    /// A member of the response body, or `Value::Null`.
    pub fn get(&self, member: &str) -> &Value {
        self.body.get(member).unwrap_or(&Value::Null)
    }

    /// The refusal message, whichever casing rendered it: a framework refusal
    /// renders `message`, a modelled error shape its member's own `Message`.
    pub fn message(&self) -> Option<&str> {
        self.get("message")
            .as_str()
            .or_else(|| self.get("Message").as_str())
    }
}

impl Xray {
    pub fn new() -> Self {
        Self::with_engine().0
    }

    /// One emulator and the engine it was mounted on, for a test that has to
    /// seed state no operation in the API writes.
    pub fn with_engine() -> (Self, Arc<engine::InMemoryEngine>) {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.register_service(service_com_amazonaws_xray_awsxray::EmulatedService::new(
            Arc::clone(&engine),
        ));
        // These tests exercise handlers, not SigV4; the signing name still has to
        // route, which the `Host` header below does.
        router.set_request_authentication(false);
        (Self { router }, engine)
    }

    /// One emulator with this service and the service `mount` builds on the SAME
    /// engine, for a case whose rule reaches another service of this emulator —
    /// which is how the production router assembles them too.
    pub fn beside<S: smithy_server_runtime::Service>(
        mount: impl FnOnce(Arc<engine::InMemoryEngine>) -> S,
    ) -> Self {
        let (mut emulator, engine) = Self::with_engine();
        emulator.router.register_service(mount(engine));
        emulator
    }

    /// Mount one more service, built from the engine this emulator already shares,
    /// for a case whose rule reaches SEVERAL other services of this emulator.
    pub fn mount<S: smithy_server_runtime::Service>(&mut self, service: S) {
        self.router.register_service(service);
    }

    /// The in-process callback into this emulator, which is what an AWS SDK
    /// client is configured over to reach a service mounted beside this one.
    pub fn callback(&self) -> smithy_server_runtime::EmbeddedEmulator {
        smithy_server_runtime::EmbeddedEmulator::from_router(self.router.clone())
    }

    pub async fn call(&self, path: &str, body: &Value) -> Answer {
        self.call_in_region(DEFAULT_REGION, path, body).await
    }

    /// The same call with the body sent as the BYTES given, for a case whose rule
    /// is about something a document cannot carry — the order a JSON object
    /// writes its members in, which a `Value` holds in an order of its own.
    pub async fn call_raw(&self, path: &str, body: &str) -> Answer {
        self.dispatch(DEFAULT_REGION, DEFAULT_ACCESS_KEY_ID, path, body)
            .await
    }

    /// The same call, signed for `region`.
    ///
    /// The region a handler works in comes from the request's SigV4 credential
    /// scope, so this is how a case addresses another region: the
    /// `Authorization` header carries the scope even though signature
    /// verification is off.
    pub async fn call_in_region(&self, region: &str, path: &str, body: &Value) -> Answer {
        self.dispatch(region, DEFAULT_ACCESS_KEY_ID, path, &body.to_string())
            .await
    }

    /// The same call signed with `access_key_id` — what names the principal the
    /// request speaks for, which is what an authorization decision is taken on.
    pub async fn call_as(&self, access_key_id: &str, path: &str, body: &Value) -> Answer {
        self.dispatch(
            DEFAULT_REGION,
            access_key_id,
            path,
            &body.to_string(),
        )
        .await
    }

    /// The one request every call above makes: the signing-name host, the
    /// credential scope of `region` naming `access_key_id`, and `body` as the
    /// bytes sent.
    async fn dispatch(
        &self,
        region: &str,
        access_key_id: &str,
        path: &str,
        body: &str,
    ) -> Answer {
        let request = ::http::Request::builder()
            .method("POST")
            .uri(format!("https://xray.{region}.amazonaws.com{path}"))
            .header("host", format!("xray.{region}.amazonaws.com"))
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={access_key_id}/20240101/{region}/xray/aws4_request, \
                     SignedHeaders=host, Signature=not-verified"
                ),
            )
            .body(::bytes::Bytes::from(body.to_string()))
            .expect("the request builds");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_type = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        Answer {
            status,
            error_type,
            body,
        }
    }

    /// One emulator with CloudWatch Logs mounted beside this service on ONE
    /// engine — the shape `core/router/src/lib.rs` mounts them in — for a case
    /// whose rule reaches the state that service's own API owns.
    pub fn with_cloud_watch_logs() -> Self {
        Self::beside(service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new)
    }

    /// Grant X-Ray the delivery a `CloudWatchLogs` trace segment destination
    /// needs, through CloudWatch Logs' OWN `PutResourcePolicy` — the API AWS's
    /// Transaction Search setup flow uses.
    ///
    /// The spans are written into the spans log group BY X-Ray under its own
    /// service principal, so the switch to that destination is refused until the
    /// account's resource policy lets that principal put log events there. The
    /// grant is account-scoped because the group is vended by the first delivery
    /// and so need not exist when the policy is put.
    ///
    /// Mount CloudWatch Logs first ([`Self::with_cloud_watch_logs`]).
    pub async fn grant_spans_delivery(&self, case: &str) {
        let document = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Sid": "TransactionSearchXRayAccess",
                "Effect": "Allow",
                "Principal": { "Service": XRAY_SERVICE_PRINCIPAL },
                "Action": SPANS_DELIVERY_ACTION,
                "Resource": logs::log_group_arn(DEFAULT_REGION, SPANS_LOG_GROUP, true),
            }],
        });
        let put = self
            .logs_call(
                "PutResourcePolicy",
                &json!({
                    "policyName": "XRayTransactionSearchAccess",
                    "policyDocument": document.to_string(),
                }),
            )
            .await;
        assert_eq!(
            put.status, 200,
            "case {case:?}: the spans delivery grant must be accepted, got {put:?}"
        );
    }

    /// One call to the CloudWatch Logs service of this emulator, for a case whose
    /// rule is about state that service's own API owns (a log group, a resource
    /// policy). `operation` is the operation name its `x-amz-target` names; the
    /// service speaks awsJson1_1 at its own endpoint host, and the call is signed
    /// for the same Region as the X-Ray calls above, so both see one account and
    /// Region.
    ///
    /// Mount it beside this service first — `Xray::beside(
    /// service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new)`
    /// — or the request reaches nothing.
    pub async fn logs_call(&self, operation: &str, body: &Value) -> Answer {
        let region = DEFAULT_REGION;
        let request = ::http::Request::builder()
            .method("POST")
            .uri(format!("https://logs.{region}.amazonaws.com/"))
            .header("host", format!("logs.{region}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("Logs_20140328.{operation}"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={DEFAULT_ACCESS_KEY_ID}/20240101/{region}/logs/\
                     aws4_request, SignedHeaders=host, Signature=not-verified"
                ),
            )
            .body(::bytes::Bytes::from(body.to_string()))
            .expect("the request builds");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_type = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        Answer {
            status,
            error_type,
            body,
        }
    }

    pub async fn put_segments(&self, documents: &[String]) -> Answer {
        self.call("/TraceSegments", &json!({ "TraceSegmentDocuments": documents }))
            .await
    }

    pub async fn summaries(&self, start: f64, end: f64, token: Option<&str>) -> Answer {
        self.query_summaries(start, end, None, token).await
    }

    /// `GetTraceSummaries` over `[start, end)` with a filter expression.
    pub async fn filtered(&self, start: f64, end: f64, filter: &str) -> Answer {
        self.query_summaries(start, end, Some(filter), None).await
    }

    /// `GetTraceSummaries` over `[start, end)`, with an optional filter
    /// expression and an optional page token: the ONE place the request shape of
    /// a paged, filtered query lives, so a suite walking pages does not rebuild
    /// it.
    pub async fn query_summaries(
        &self,
        start: f64,
        end: f64,
        filter: Option<&str>,
        token: Option<&str>,
    ) -> Answer {
        let mut body = summaries_body(start, end, filter);
        if let Some(token) = token {
            body["NextToken"] = json!(token);
        }
        self.call("/TraceSummaries", &body).await
    }

    /// Every `TraceSummary` the window `[start, end)` holds, walked across the
    /// pages the query serves it in: a query's pages are STEPS of its own
    /// window, so a reader that wants a window longer than one step follows
    /// `NextToken` until the service stops issuing one. `filter` is the optional
    /// filter expression every page is queried with.
    pub async fn paged_summaries(&self, start: f64, end: f64, filter: Option<&str>) -> Vec<Value> {
        let body = summaries_body(start, end, filter);
        let mut summaries: Vec<Value> = Vec::new();
        for page in self.trace_summary_pages(&body).await {
            summaries.extend(array(&page.body, "TraceSummaries").iter().cloned());
        }
        summaries
    }

    /// Every page of the `GetTraceSummaries` query `query`, in the order the
    /// service serves them.
    ///
    /// The pages of one query are STEPS of its own window measured back from
    /// the window's own end, so which page a trace falls on follows from the
    /// window the request asks about — a reader asking what a WINDOW holds
    /// follows `NextToken` until the service stops issuing one, which for a
    /// window no longer than one step is after the first page. `query` is the
    /// request body every page is asked with, carrying whatever else the query
    /// states (a `TimeRangeType`, a `SamplingStrategy`, a filter expression);
    /// the token is the only member this walk sets.
    pub async fn trace_summary_pages(&self, query: &Value) -> Vec<Answer> {
        /// More pages than any window this harness asks about is cut into, so a
        /// walk that does not terminate fails the case instead of hanging.
        const MAX_PAGES: usize = 200;

        let mut pages: Vec<Answer> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut body = query.clone();
            let object = body
                .as_object_mut()
                .expect("a GetTraceSummaries query is a JSON object");
            match token.as_deref() {
                Some(token) => object.insert("NextToken".to_string(), json!(token)),
                None => object.remove("NextToken"),
            };
            let page = self.call("/TraceSummaries", &body).await;
            assert_eq!(
                page.status, 200,
                "walking the pages of {query} must succeed, got {page:?}"
            );
            token = page.get("NextToken").as_str().map(str::to_string);
            pages.push(page);
            assert!(
                pages.len() <= MAX_PAGES,
                "walking the pages of {query} must terminate, stopped after {} pages",
                pages.len()
            );
            if token.is_none() {
                break;
            }
        }
        pages
    }

    pub async fn batch_get(&self, ids: &[String], token: Option<&str>) -> Answer {
        let mut body = json!({ "TraceIds": ids });
        if let Some(token) = token {
            body["NextToken"] = json!(token);
        }
        self.call("/Traces", &body).await
    }

    pub async fn get_groups(&self, token: Option<&str>) -> Answer {
        let mut body = json!({});
        if let Some(token) = token {
            body["NextToken"] = json!(token);
        }
        self.call("/Groups", &body).await
    }

    /// `GetIndexingRules`, with `NextToken` sent only when `token` is `Some`.
    pub async fn get_rules(&self, token: Option<&str>) -> Answer {
        let mut body = json!({});
        if let Some(token) = token {
            body["NextToken"] = json!(token);
        }
        self.call("/GetIndexingRules", &body).await
    }

    /// `UpdateIndexingRule` with a probabilistic configuration.
    pub async fn update_rule(&self, name: &str, percentage: f64) -> Answer {
        self.call(
            "/UpdateIndexingRule",
            &json!({
                "Name": name,
                "Rule": { "Probabilistic": { "DesiredSamplingPercentage": percentage } },
            }),
        )
        .await
    }
}

/// Floating-point seconds compare: the values travel as JSON doubles.
pub fn close(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() < 1e-6
}

/// A 35-character X-Ray trace id (version, 8 hex, 24 hex), distinct per `n`.
///
/// The widths are fixed, so the id stays inside `TraceId`'s modelled `@length`
/// 1..35 for every `n`, and sorting the ids orders them by `n` — which is what
/// the pagination cases rely on.
pub fn trace_id(n: u32) -> String {
    format!("1-{n:08x}-{n:024x}")
}

/// The JSON array member of `value`, or an empty slice.
pub fn array<'a>(value: &'a Value, member: &str) -> &'a [Value] {
    value
        .get(member)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// The request body of a `GetTraceSummaries` query over `[start, end)` with an
/// optional filter expression: the one shape every caller of that read builds,
/// stated once so a walk of its pages and a single call cannot disagree.
pub fn summaries_body(start: f64, end: f64, filter: Option<&str>) -> Value {
    let mut body = json!({ "StartTime": start, "EndTime": end });
    if let Some(filter) = filter {
        body["FilterExpression"] = json!(filter);
    }
    body
}

/// The trace ids one `GetTraceSummaries` answer reports, in the order it
/// reports them.
pub fn reported(answer: &Answer) -> Vec<String> {
    array(&answer.body, "TraceSummaries")
        .iter()
        .filter_map(|summary| summary.get("Id").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// The trace ids the pages of one `GetTraceSummaries` query report, in the order
/// the pages report them — what a reader of the WHOLE window sees, as opposed to
/// what one page of it carries.
pub fn reported_trace_ids(pages: &[Answer]) -> Vec<String> {
    pages.iter().flat_map(reported).collect()
}

/// A root segment document with the given times, in progress when `end` is
/// absent.
pub fn root_document(id: &str, trace: &str, name: &str, start: f64, end: Option<f64>) -> Value {
    let mut document = json!({
        "trace_id": trace,
        "id": id,
        "name": name,
        "start_time": start,
    });
    match end {
        Some(end) => document["end_time"] = json!(end),
        None => document["in_progress"] = json!(true),
    }
    document
}
