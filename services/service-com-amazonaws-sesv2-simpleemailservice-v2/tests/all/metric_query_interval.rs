// SPDX-License-Identifier: CC0-1.0

//! The interval one `BatchGetMetricData` query may ask about, end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with this
//! service registered — so routing, deserialization, the handler and serialization
//! are all exercised.
//!
//! The rule these cases pin: a query asks for one metric's series over the window its
//! two bounds describe, and SES serves a window no longer than
//! [`MAX_QUERY_INTERVAL_DAYS`] days. A query whose window is longer is refused 400
//! `BadRequestException` with AWS's one sentence for it — which tells the caller to
//! partition the interval instead of reporting a partial series for it — before any
//! record is read, and the limit is each QUERY's, so one query of the batch breaking
//! it refuses the batch.
//!
//! The table spans the lengths a window can take around the limit: no time at all, a
//! second, a day, one day short of the limit, the limit itself, a second past it, and
//! well past it.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_sesv2_simpleemailservice_v2::ops::batch_get_metric_data::MAX_QUERY_INTERVAL_DAYS;
use service_com_amazonaws_sesv2_simpleemailservice_v2::state::FEATURE_ENABLED;
use service_com_amazonaws_sesv2_simpleemailservice_v2::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// `BatchGetMetricData`'s route, from the operation's HTTP binding
/// (`smithy_query(query="operation",operation="BatchGetMetricData")`:
/// `POST /v2/email/metrics/batch`).
const METRICS_PATH: &str = "/v2/email/metrics/batch";

/// The only namespace `MetricNamespace` publishes.
const VDM: &str = "VDM";

/// The metric every query here asks for; which metric is asked for does not come into
/// this rule.
const SEND: &str = "SEND";

/// Seconds in a day.
const DAY: i64 = 24 * 60 * 60;

/// A fixed UTC midnight every window here ends at: `2026-01-01T00:00:00Z`, so a window
/// of any length is written as the instant that many seconds before it.
const WINDOW_END: i64 = 1_767_225_600;

/// AWS's sentence for a window longer than SES serves one query over. The limit it
/// states is the one the service enforces; the rest is fixed text.
const INTERVAL_SENTENCE: &str =
    "Invalid interval, can request at most 60 days. Please partition your requested interval.";

/// One answered request: the status, the modelled error type and the parsed body.
struct Reply {
    status: u16,
    error_type: Option<String>,
    body: Value,
}

impl Reply {
    /// The modelled error code the response names, or `""` on success.
    fn error_code(&self) -> String {
        let raw = self
            .error_type
            .as_deref()
            .or_else(|| self.body.get("__type").and_then(Value::as_str))
            .unwrap_or_default();
        let name = raw.split('#').next_back().unwrap_or(raw);
        name.split(':').next().unwrap_or(name).to_string()
    }

    /// The message the error carried, or `""` on success.
    fn message(&self) -> String {
        self.body
            .get("message")
            .or_else(|| self.body.get("Message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// How many results the response carries.
    fn result_count(&self) -> usize {
        self.body
            .get("Results")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or_default()
    }
}

/// The emulated service behind its router.
struct Ses {
    router: MultiServiceRouter,
}

impl Ses {
    /// A service whose account has VDM switched on, which is what the namespace the
    /// queries name is served under.
    async fn with_vdm() -> Self {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These cases exercise handler behaviour, not SigV4.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        let ses = Self { router };
        let put = ses
            .call(
                "PUT",
                "/v2/email/account/vdm",
                Some(json!({"VdmAttributes": {"VdmEnabled": FEATURE_ENABLED}})),
            )
            .await;
        assert_eq!(
            put.status, 200,
            "case \"setup\": expected 200 from the VDM put, got {} {:?}",
            put.status, put.body
        );
        ses
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> Reply {
        let payload = match &body {
            Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("serialize body")),
            None => bytes::Bytes::new(),
        };
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://email.us-east-1.amazonaws.com{path}"))
            .header("host", "email.us-east-1.amazonaws.com")
            .header("content-type", "application/json")
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 Credential=AKIAEMULATOR0000000/20260101/us-east-1/ses/aws4_request, SignedHeaders=host, Signature=00",
            )
            .body(payload)
            .expect("build request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_type = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_type,
            body,
        }
    }

    async fn batch(&self, queries: Vec<Value>) -> Reply {
        self.call("POST", METRICS_PATH, Some(json!({"Queries": queries})))
            .await
    }
}

/// One query asking about a window `span` seconds long, ending at [`WINDOW_END`].
fn query(id: &str, span: i64) -> Value {
    json!({
        "Id": id,
        "Namespace": VDM,
        "Metric": SEND,
        "StartDate": WINDOW_END - span,
        "EndDate": WINDOW_END,
    })
}

/// The limit in seconds, the units a query's bounds carry.
fn limit_seconds() -> i64 {
    MAX_QUERY_INTERVAL_DAYS * DAY
}

/// One row: the window's length in seconds and whether SES serves it.
fn interval_cases() -> Vec<(&'static str, i64, bool)> {
    vec![
        ("no time at all", 0, true),
        ("one second", 1, true),
        ("one day", DAY, true),
        ("one day short of the limit", limit_seconds() - DAY, true),
        ("the limit itself", limit_seconds(), true),
        ("one second past the limit", limit_seconds() + 1, false),
        ("one day past the limit", limit_seconds() + DAY, false),
        ("twice the limit", 2 * limit_seconds(), false),
        ("a year", 365 * DAY, false),
    ]
}

#[tokio::test]
async fn the_limit_states_the_days_aws_names_in_its_refusal() {
    // The sentence AWS answers states the limit, so the limit the service enforces is
    // the one the sentence names.
    assert!(
        INTERVAL_SENTENCE.contains(&format!("at most {MAX_QUERY_INTERVAL_DAYS} days")),
        "expected the refusal to state the enforced limit of {MAX_QUERY_INTERVAL_DAYS} days, got {INTERVAL_SENTENCE:?}"
    );
}

#[tokio::test]
async fn a_window_longer_than_the_served_interval_is_refused() {
    for (case, span, served) in interval_cases() {
        let ses = Ses::with_vdm().await;
        let reply = ses.batch(vec![query("q1", span)]).await;
        if served {
            assert_eq!(
                reply.status,
                200,
                "case {case:?}: expected a window of {span}s to be served, got {} {:?}",
                reply.status,
                reply.message()
            );
            assert_eq!(
                reply.result_count(),
                1,
                "case {case:?}: expected the one query to be answered, got {:?}",
                reply.body
            );
        } else {
            assert_eq!(
                (reply.status, reply.error_code().as_str()),
                (400, "BadRequestException"),
                "case {case:?}: expected 400 BadRequestException for a window of {span}s, got {} {:?}",
                reply.status,
                reply.body
            );
            assert_eq!(
                reply.message(),
                INTERVAL_SENTENCE,
                "case {case:?}: expected AWS's interval sentence, got {:?}",
                reply.message()
            );
            assert_eq!(
                reply.result_count(),
                0,
                "case {case:?}: expected no results on a refused batch, got {:?}",
                reply.body
            );
        }
    }
}

#[tokio::test]
async fn the_limit_is_each_query_s_own() {
    // The limit applies to the window ONE query asks about, so a batch of windows
    // each within it is served however much they add up to — and a single query
    // breaking it refuses the batch, wherever in the batch it sits.
    let ses = Ses::with_vdm().await;
    let within: Vec<Value> = (0..3)
        .map(|position| {
            json!({
                "Id": format!("q{position}"),
                "Namespace": VDM,
                "Metric": SEND,
                "StartDate": WINDOW_END - (position + 1) * limit_seconds(),
                "EndDate": WINDOW_END - position * limit_seconds(),
            })
        })
        .collect();
    let reply = ses.batch(within).await;
    assert_eq!(
        reply.status,
        200,
        "case partitioned: expected three windows of the limit to be served, got {} {:?}",
        reply.status,
        reply.message()
    );
    assert_eq!(
        reply.result_count(),
        3,
        "case partitioned: expected one result per query, got {:?}",
        reply.body
    );

    for position in 0..3 {
        let ses = Ses::with_vdm().await;
        let queries: Vec<Value> = (0..3)
            .map(|at| {
                let span = if at == position {
                    limit_seconds() + 1
                } else {
                    DAY
                };
                query(&format!("q{at}"), span)
            })
            .collect();
        let reply = ses.batch(queries).await;
        assert_eq!(
            (reply.status, reply.error_code().as_str()),
            (400, "BadRequestException"),
            "case over-at-{position}: expected the batch to be refused, got {} {:?}",
            reply.status,
            reply.body
        );
        assert_eq!(
            reply.message(),
            INTERVAL_SENTENCE,
            "case over-at-{position}: expected AWS's interval sentence, got {:?}",
            reply.message()
        );
    }
}
