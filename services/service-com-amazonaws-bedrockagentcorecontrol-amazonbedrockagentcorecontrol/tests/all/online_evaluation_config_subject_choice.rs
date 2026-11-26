// SPDX-License-Identifier: CC0-1.0

//! What an online evaluation configuration must be told to evaluate.
//!
//! RULE under test: a `CreateOnlineEvaluationConfig` names what the evaluation works on in
//! EXACTLY ONE of `evaluators` and `insights` — a request naming neither, or both, is
//! refused `ValidationException` (HTTP 400) with the fixed sentence `Exactly one of
//! evaluators or insights must be provided`, and nothing is persisted: no id is minted and
//! the name the request asked for is still free for a later create (`ListOnlineEvaluation
//! Configs` is still scaffolded, so the name being free again is how an absent record is
//! observed).
//!
//! A list counts as naming a subject when it carries at least one entry, because an AWS SDK
//! sends a list a caller never set as an empty one, so `[]` and an omitted member are the
//! same request on the wire.
//!
//! Sources: `smithy_query(query="operation",operation="CreateOnlineEvaluationConfig")` for
//! both members being optional lists (`EvaluatorList` 0..25, `InsightList` 0..10), for
//! `ValidationException` at HTTP 400, and for the required members every row below carries;
//! `smithy_query(query="shape",shape="EvaluatorReference,Insight")` for the entries.
//!
//! The rows span what the rule generalises over: which of the two lists is named, how many
//! entries it carries (one, and each list's published maximum), and the shape the ABSENT
//! side takes (omitted, or present and empty) — so a refusal cannot pass by keying on the
//! members being absent from the body, and an acceptance cannot pass by ignoring the lists.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region the rows sign for.
const REGION: &str = "us-east-1";

/// An IAM role ARN inside `RoleArn`'s pattern
/// `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$`. No IAM is mounted here, so the role is
/// resolved against nothing and cannot be what refuses a row: `bedrock_agentcore_online_
/// evaluation_execution_role` in the router's own suites is where that gate is pinned.
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/AgentCoreEvaluation";

/// The whole refusal sentence a create that does not name exactly one evaluation subject
/// carries — fixed text, with no part of the request in it.
const SUBJECT_MESSAGE: &str = "Exactly one of evaluators or insights must be provided";

fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(engine));
    router
}

fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock-agentcore/\
         aws4_request, SignedHeaders=host, Signature=abc"
    )
}

async fn send(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(Bytes::new, |value| {
        Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
    });
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", authorization())
        .header("host", "bedrock-agentcore-control.us-east-1.amazonaws.com");
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

async fn create_config(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    send(router, "POST", "/online-evaluation-configs/create", Some(body)).await
}

async fn get_config(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    send(
        router,
        "GET",
        &format!("/online-evaluation-configs/{id}"),
        None,
    )
    .await
}

fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The error code `restJson1` puts in `__type`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// A create carrying every required member, under `name`, with whatever the row says about
/// the two subject lists added to it.
fn create_body(name: &str, evaluators: Option<Value>, insights: Option<Value>) -> Value {
    let mut body = json!({
        "onlineEvaluationConfigName": name,
        "rule": {"samplingConfig": {"samplingPercentage": 10.0}},
        "dataSourceConfig": {"cloudWatchLogs": {
            "serviceNames": ["agentservice"],
            "logGroupNames": ["/aws/agentcore/subject-choice"]
        }},
        "evaluationExecutionRoleArn": EXECUTION_ROLE,
        "enableOnCreate": false
    });
    if let Some(evaluators) = evaluators {
        body["evaluators"] = evaluators;
    }
    if let Some(insights) = insights {
        body["insights"] = insights;
    }
    body
}

/// `EvaluatorList` is @length 0..25 and its member's `evaluatorId` admits
/// `Builtin.<name>` (`smithy_query(query="shape",shape="EvaluatorList,EvaluatorReference")`).
fn evaluators(count: usize) -> Value {
    json!((0..count)
        .map(|index| json!({"evaluatorId": format!("Builtin.Evaluator{index}")}))
        .collect::<Vec<_>>())
}

/// `InsightList` is @length 0..10 and its member's `insightId` admits `Builtin.<name>`
/// (`smithy_query(query="shape",shape="InsightList,Insight")`).
fn insights(count: usize) -> Value {
    json!((0..count)
        .map(|index| json!({"insightId": format!("Builtin.Insight{index}")}))
        .collect::<Vec<_>>())
}

/// One create this suite drives: what it says about each list, and whether the rule admits
/// it.
struct Row {
    case: &'static str,
    /// The name this row creates under (`EvaluationConfigName` is
    /// `^[a-zA-Z][a-zA-Z0-9_]{0,47}$`), so a row that is accepted cannot collide with
    /// another.
    name: &'static str,
    evaluators: Option<Value>,
    insights: Option<Value>,
    accepted: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one evaluator and no insights member",
            name: "oneEvaluator",
            evaluators: Some(evaluators(1)),
            insights: None,
            accepted: true,
        },
        Row {
            case: "the evaluator list at its published maximum",
            name: "maxEvaluators",
            evaluators: Some(evaluators(25)),
            insights: None,
            accepted: true,
        },
        Row {
            case: "one insight and no evaluators member",
            name: "oneInsight",
            evaluators: None,
            insights: Some(insights(1)),
            accepted: true,
        },
        Row {
            case: "the insight list at its published maximum",
            name: "maxInsights",
            evaluators: None,
            insights: Some(insights(10)),
            accepted: true,
        },
        Row {
            case: "one evaluator beside an empty insight list",
            name: "evaluatorsEmptyInsights",
            evaluators: Some(evaluators(1)),
            insights: Some(insights(0)),
            accepted: true,
        },
        Row {
            case: "one insight beside an empty evaluator list",
            name: "insightsEmptyEvaluators",
            evaluators: Some(evaluators(0)),
            insights: Some(insights(1)),
            accepted: true,
        },
        Row {
            case: "neither member",
            name: "neitherNamed",
            evaluators: None,
            insights: None,
            accepted: false,
        },
        Row {
            case: "both lists empty",
            name: "bothEmpty",
            evaluators: Some(evaluators(0)),
            insights: Some(insights(0)),
            accepted: false,
        },
        Row {
            case: "an empty evaluator list and no insights member",
            name: "emptyEvaluatorsOnly",
            evaluators: Some(evaluators(0)),
            insights: None,
            accepted: false,
        },
        Row {
            case: "an empty insight list and no evaluators member",
            name: "emptyInsightsOnly",
            evaluators: None,
            insights: Some(insights(0)),
            accepted: false,
        },
        Row {
            case: "both subjects named with one entry each",
            name: "bothNamedOnce",
            evaluators: Some(evaluators(1)),
            insights: Some(insights(1)),
            accepted: false,
        },
        Row {
            case: "both subjects named at each list's published maximum",
            name: "bothNamedFull",
            evaluators: Some(evaluators(25)),
            insights: Some(insights(10)),
            accepted: false,
        },
    ]
}

/// A create names exactly one evaluation subject or it is refused, and a refused create
/// persists nothing: no id is minted and the name it asked for is still free for a create
/// that does name one.
#[tokio::test]
async fn a_create_names_exactly_one_evaluation_subject() {
    let router = router();

    for row in rows() {
        let case = row.case;
        let body = create_body(row.name, row.evaluators.clone(), row.insights.clone());
        let (status, answered) = create_config(&router, &body).await;

        if row.accepted {
            assert_eq!(
                status, 202,
                "case {case:?}: a create naming exactly one evaluation subject is accepted, got \
                 {status} ({answered})"
            );
            let id = text(&answered, "onlineEvaluationConfigId");
            assert!(
                !id.is_empty(),
                "case {case:?}: an accepted create mints an id, got {answered}"
            );
            // The configuration it minted carries the lists the create named, read back
            // through the implemented reader rather than off the create's own envelope.
            let (status, fetched) = get_config(&router, &id).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the minted configuration reads back, got {status} ({fetched})"
            );
            assert_eq!(
                fetched.get("evaluators"),
                // `evaluators` carries the modelled default `[]` on a read, so a create
                // that named none reads back as the empty list.
                Some(body.get("evaluators").unwrap_or(&json!([]))),
                "case {case:?}: the read reports the evaluators the create named, got {fetched}"
            );
            assert_eq!(
                fetched.get("insights"),
                body.get("insights"),
                "case {case:?}: the read reports the insights the create named, got {fetched}"
            );
            continue;
        }

        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "ValidationException"),
            "case {case:?}: a create that does not name exactly one evaluation subject is \
             refused 400 ValidationException, got {status} {} ({answered})",
            error_code(&answered)
        );
        assert_eq!(
            text(&answered, "message"),
            SUBJECT_MESSAGE,
            "case {case:?}: got {answered}"
        );
        assert!(
            answered.get("onlineEvaluationConfigId").is_none()
                && answered.get("onlineEvaluationConfigArn").is_none(),
            "case {case:?}: a refused create mints nothing, got {answered}"
        );

        // The name the refused create asked for is free: the same name, with exactly one
        // subject named, is accepted and reads back.
        let repaired = create_body(row.name, Some(evaluators(1)), None);
        let (status, created) = create_config(&router, &repaired).await;
        assert_eq!(
            status, 202,
            "case {case:?}: the refused create reserved nothing, so the same name with one \
             evaluator is accepted, got {status} ({created})"
        );
        // What that create wrote is the repaired configuration, not the refused one: the
        // read reports the one evaluator it named.
        let id = text(&created, "onlineEvaluationConfigId");
        let (status, fetched) = get_config(&router, &id).await;
        assert_eq!(
            (status, fetched.get("evaluators"), fetched.get("insights")),
            (200, Some(&evaluators(1)), None),
            "case {case:?}: the configuration that WAS written carries one evaluator and no \
             insights, got {status} ({fetched})"
        );
    }
}
