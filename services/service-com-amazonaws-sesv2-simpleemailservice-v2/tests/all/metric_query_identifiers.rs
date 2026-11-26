// SPDX-License-Identifier: CC0-1.0

//! `BatchGetMetricData`'s query identifiers, end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with this
//! service registered — so routing, deserialization, the handler and serialization
//! are all exercised.
//!
//! The rule these cases pin: `BatchGetMetricDataQuery$Id` is "the query identifier",
//! and every result the batch answers with carries the `Id` of the query it answers,
//! so a batch must identify each of its queries. One that spends the same identifier
//! on two queries is refused 400 `BadRequestException` with AWS's one sentence for it,
//! before any record is read; a batch whose identifiers are all distinct is answered,
//! one result per query, keyed back by the identifier it was sent under.
//!
//! The tables span what the rule generalises over: how many queries the batch carries
//! (up to the modelled maximum of ten), where the repeat sits and how often it
//! repeats, the form and length of the identifier (one character and the modelled
//! maximum of 255), whether the queries sharing an identifier are otherwise alike,
//! and — on the other side of the rule — the ways two identifiers can be close
//! without being the same string.

use std::sync::Arc;

use serde_json::{json, Value};
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

/// A fixed UTC midnight the windows are written around: `2026-01-01T00:00:00Z`.
const DAY_ONE: i64 = 1_767_225_600;

/// Seconds in a day.
const DAY: i64 = 24 * 60 * 60;

/// The modelled maximum of `BatchGetMetricDataQueries`
/// (`smithy_query(query="shape",shape="BatchGetMetricDataQueries")`: `@length` max 10).
const MAX_QUERIES: usize = 10;

/// The modelled maximum length of `BatchGetMetricDataQuery$Id`
/// (`smithy_query(query="shape",shape="BatchGetMetricDataQuery")`: `@length` 1..255).
const MAX_ID_CHARS: usize = 255;

/// AWS's sentence for a batch that does not identify each of its queries: entirely
/// fixed text, naming neither the repeated identifier nor where it was repeated.
const UNIQUE_IDENTIFIER_SENTENCE: &str = "All queries must have a unique identifier.";

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

    /// The `Id`s of `Results`, in the order the response lists them.
    fn result_ids(&self) -> Vec<String> {
        self.body
            .get("Results")
            .and_then(Value::as_array)
            .map(|results| {
                results
                    .iter()
                    .map(|result| {
                        result
                            .get("Id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The emulated service behind its router.
struct Ses {
    router: MultiServiceRouter,
}

impl Ses {
    fn new() -> Self {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These cases exercise handler behaviour, not SigV4.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    /// A service whose account has VDM switched on, which is what the namespace the
    /// queries name is served under.
    async fn with_vdm() -> Self {
        let ses = Self::new();
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
            // The SigV4 credential scope is what routes the request to the `ses`
            // signing name; the signature itself is not checked here.
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

/// One query of a batch, carrying the identifier the case is about.
fn query(id: &str) -> Value {
    json!({
        "Id": id,
        "Namespace": VDM,
        "Metric": SEND,
        "StartDate": DAY_ONE,
        "EndDate": DAY_ONE + DAY,
    })
}

/// A query whose metric, window and dimensions differ from [`query`]'s, so a case can
/// show that two queries sharing an identifier are refused however unalike they are.
fn other_query(id: &str) -> Value {
    json!({
        "Id": id,
        "Namespace": VDM,
        "Metric": "COMPLAINT",
        "Dimensions": {"EMAIL_IDENTITY": "sender@example.com"},
        "StartDate": DAY_ONE - 7 * DAY,
        "EndDate": DAY_ONE,
    })
}

#[tokio::test]
async fn a_batch_that_repeats_an_identifier_is_refused() {
    /// One row: the identifiers the batch is sent with, and whether the queries
    /// sharing one are otherwise alike.
    struct Case {
        case: &'static str,
        ids: Vec<String>,
        alike: bool,
    }

    let long = "x".repeat(MAX_ID_CHARS);
    let cases = vec![
        Case {
            case: "two identical queries",
            ids: vec!["q1".to_string(), "q1".to_string()],
            alike: true,
        },
        Case {
            case: "two queries that differ in everything but the identifier",
            ids: vec!["q1".to_string(), "q1".to_string()],
            alike: false,
        },
        Case {
            case: "the repeat at the far end of the batch",
            ids: vec!["q1".to_string(), "q2".to_string(), "q1".to_string()],
            alike: true,
        },
        Case {
            case: "the repeat in the middle of the batch",
            ids: vec![
                "a".to_string(),
                "b".to_string(),
                "b".to_string(),
                "c".to_string(),
            ],
            alike: true,
        },
        Case {
            case: "one identifier spent three times",
            ids: vec!["same".to_string(), "same".to_string(), "same".to_string()],
            alike: false,
        },
        Case {
            case: "a single-character identifier",
            ids: vec!["1".to_string(), "1".to_string()],
            alike: true,
        },
        Case {
            case: "an identifier of the modelled maximum length",
            ids: vec![long.clone(), long.clone()],
            alike: false,
        },
        Case {
            case: "a full batch whose last query repeats its first",
            ids: (0..MAX_QUERIES)
                .map(|position| {
                    if position + 1 == MAX_QUERIES {
                        "q0".to_string()
                    } else {
                        format!("q{position}")
                    }
                })
                .collect(),
            alike: true,
        },
    ];

    for case in cases {
        let ses = Ses::with_vdm().await;
        let mut seen: Vec<&String> = Vec::new();
        let queries: Vec<Value> = case
            .ids
            .iter()
            .map(|id| {
                let repeat = seen.contains(&id);
                seen.push(id);
                if case.alike || !repeat {
                    query(id)
                } else {
                    other_query(id)
                }
            })
            .collect();
        let reply = ses.batch(queries).await;
        assert_eq!(
            (reply.status, reply.error_code().as_str()),
            (400, "BadRequestException"),
            "case {:?}: expected 400 BadRequestException for {:?}, got {} {:?}",
            case.case,
            case.ids,
            reply.status,
            reply.body
        );
        assert_eq!(
            reply.message(),
            UNIQUE_IDENTIFIER_SENTENCE,
            "case {:?}: expected AWS's unique-identifier sentence, got {:?}",
            case.case,
            reply.message()
        );
        // Nothing of a batch that was refused is answered: no result document comes
        // back beside the refusal.
        assert_eq!(
            reply.result_ids(),
            Vec::<String>::new(),
            "case {:?}: expected no results on a refused batch, got {:?}",
            case.case,
            reply.body
        );
    }
}

#[tokio::test]
async fn a_batch_whose_identifiers_are_distinct_is_answered() {
    // The other side of the rule: two identifiers are the same identifier only when
    // they are the same string, so the rows below are all distinct and every one of
    // them is answered — one result per query, in the order the queries were sent.
    let long = "x".repeat(MAX_ID_CHARS);
    let cases: Vec<(&str, Vec<String>)> = vec![
        ("a batch of one", vec!["q1".to_string()]),
        (
            "two plain identifiers",
            vec!["q1".to_string(), "q2".to_string()],
        ),
        (
            "identifiers differing only in case",
            vec!["Q1".to_string(), "q1".to_string()],
        ),
        (
            "identifiers differing only in trailing space",
            vec!["q1".to_string(), "q1 ".to_string()],
        ),
        (
            "one identifier a prefix of the other",
            vec!["q".to_string(), "q1".to_string()],
        ),
        (
            "the modelled maximum length and one character less",
            vec![long.clone(), long[1..].to_string()],
        ),
        (
            "a full batch of distinct identifiers",
            (0..MAX_QUERIES).map(|n| format!("q{n}")).collect(),
        ),
    ];

    for (case, ids) in cases {
        let ses = Ses::with_vdm().await;
        let queries: Vec<Value> = ids.iter().map(|id| query(id)).collect();
        let reply = ses.batch(queries).await;
        assert_eq!(
            reply.status,
            200,
            "case {case:?}: expected the batch to be answered, got {} {:?}",
            reply.status,
            reply.message()
        );
        assert_eq!(
            reply.result_ids(),
            ids,
            "case {case:?}: expected one result per query keyed by its identifier, got {:?}",
            reply.body
        );
    }
}
