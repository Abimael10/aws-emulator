// SPDX-License-Identifier: CC0-1.0

//! `DeleteOnlineEvaluationConfig` end to end over HTTP: what an accepted deletion reports,
//! what a read of the configuration says while the removal the clock owes is pending, what
//! happens to the evaluation the configuration describes, and what a later read, a later
//! change and a later create of the same name see once the record is gone.
//!
//! Every row drives the operations as a client does — one shared engine behind one router —
//! so the create, the delete and the reads around them are asserted against the same store
//! rather than against each other's envelopes.
//!
//! Sources: `smithy_query(query="operation",
//! operation="CreateOnlineEvaluationConfig,DeleteOnlineEvaluationConfig,GetOnlineEvaluationConfig,UpdateOnlineEvaluationConfig")`
//! for every member, route, status code and enum value asserted here, and
//! `DeleteOnlineEvaluationConfig`'s own documentation for the deletion stopping "any ongoing
//! evaluation processes associated with it".

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region the rows sign for, which the ARNs they assert carry.
const REGION: &str = "us-east-1";

/// The account id every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// How long a poll for a clock-owed transition keeps looking, and how long it waits between
/// looks. Generous: the condition is what the test waits for, not a duration.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);
const SETTLE_POLL: Duration = Duration::from_millis(50);

/// An IAM role ARN inside `RoleArn`'s pattern
/// `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$`.
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/AgentCoreEvaluation";

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

async fn delete_config(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    send(
        router,
        "DELETE",
        &format!("/online-evaluation-configs/{id}"),
        None,
    )
    .await
}

async fn update_config(router: &MultiServiceRouter, id: &str, body: &Value) -> (u16, Value) {
    send(
        router,
        "PUT",
        &format!("/online-evaluation-configs/{id}"),
        Some(body),
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

/// The CloudWatch Logs data source a configuration reads agent traces from, as the one
/// `DataSourceConfig` union member the model declares.
fn cloud_watch_logs(inner: Value) -> Value {
    json!({"cloudWatchLogs": inner})
}

/// Read the configuration until it reports `wanted`, or until the deadline passes. Waits for
/// the CONDITION rather than for a duration.
async fn poll_until_status(router: &MultiServiceRouter, id: &str, wanted: &str) -> Value {
    let started = Instant::now();
    let (_, mut last) = get_config(router, id).await;
    while started.elapsed() < SETTLE_DEADLINE && text(&last, "status") != wanted {
        tokio::time::sleep(SETTLE_POLL).await;
        let (_, fresh) = get_config(router, id).await;
        last = fresh;
    }
    last
}

/// Read the configuration until the read no longer serves it, or until the deadline passes.
async fn poll_until_gone(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    let started = Instant::now();
    let (mut status, mut last) = get_config(router, id).await;
    while started.elapsed() < SETTLE_DEADLINE && status == 200 {
        tokio::time::sleep(SETTLE_POLL).await;
        let (fresh_status, fresh) = get_config(router, id).await;
        status = fresh_status;
        last = fresh;
    }
    (status, last)
}

/// One deletion this suite drives: the configuration it is asked of, and whether the request
/// waits for the create's own transition to come due first.
struct Row {
    case: &'static str,
    /// Whether the delete is asked of a configuration that has reached `ACTIVE`, or of one
    /// whose create is still settling.
    settled_first: bool,
    body: Value,
}

/// The rows: configurations that differ in the members they carry, in whether their
/// evaluation was running, in the length of their name, and in what the configuration was
/// doing when the deletion was asked of it.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a configuration with the required members alone, deleted while still \
                   settling",
            settled_first: false,
            body: json!({
                "onlineEvaluationConfigName": "minimalDeleted",
                "rule": {"samplingConfig": {"samplingPercentage": 0.01}},
                "dataSourceConfig": cloud_watch_logs(json!({
                    "serviceNames": ["agentservice"],
                    "logGroupNames": ["/aws/agentcore/minimal"]
                })),
                "evaluationExecutionRoleArn": EXECUTION_ROLE,
                "enableOnCreate": false
            }),
        },
        Row {
            case: "a configuration whose evaluation is running, deleted once it is ACTIVE",
            settled_first: true,
            body: json!({
                "onlineEvaluationConfigName": "runningDeleted",
                "description": "the configuration whose evaluation the deletion stops",
                "rule": {
                    "samplingConfig": {"samplingPercentage": 100.0},
                    "filters": [{"key": "agent.name", "operator": "Equals",
                                 "value": {"stringValue": "checkout"}}],
                    "sessionConfig": {"sessionTimeoutMinutes": 1440}
                },
                "dataSourceConfig": cloud_watch_logs(json!({
                    "serviceNames": ["checkoutagent"],
                    "logGroupNames": ["/aws/agentcore/one", "/aws/agentcore/two"]
                })),
                "evaluators": [{"evaluatorId": "Builtin.Correctness"}],
                "insights": [{"insightId": "Builtin.TopicClusters"}],
                "clusteringConfig": {"frequencies": ["DAILY"]},
                "outputConfig": {"cloudWatchConfig": {
                    "logGroupName": "/aws/agentcore/evaluations",
                    "metricsNamespace": "AgentCore/Evaluations",
                    "resultDestination": "SOURCE_LOG_GROUP"
                }},
                "evaluationExecutionRoleArn": EXECUTION_ROLE,
                "enableOnCreate": true,
                "tags": {"owner": "agentcore"}
            }),
        },
        Row {
            case: "a configuration named at the member's maximum length, deleted while still \
                   settling",
            settled_first: false,
            body: json!({
                // `EvaluationConfigName` is `^[a-zA-Z][a-zA-Z0-9_]{0,47}$`.
                "onlineEvaluationConfigName": format!("d{}", "x".repeat(47)),
                "rule": {"samplingConfig": {"samplingPercentage": 12.5}},
                "dataSourceConfig": cloud_watch_logs(json!({
                    "serviceNames": ["prefixedagent"],
                    "logGroupNames": [],
                    "logGroupNamePrefixes": ["/aws/agentcore/"]
                })),
                "evaluators": [],
                "evaluationExecutionRoleArn": EXECUTION_ROLE,
                "enableOnCreate": true
            }),
        },
    ]
}

/// An accepted deletion reports the configuration it was accepted for in `DELETING`, stops the
/// evaluation that configuration describes, is answered the same way when it is repeated, and
/// the record goes when the removal the clock owes comes due — after which the id is the
/// modelled 404 and the name the configuration held is free again.
#[tokio::test]
async fn an_accepted_deletion_removes_the_config_the_clock_owes() {
    let router = router();

    for row in rows() {
        let case = row.case;
        let (status, created) = create_config(&router, &row.body).await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected HTTP 202 from CreateOnlineEvaluationConfig, got {status} \
             ({created})"
        );
        let name = text(&row.body, "onlineEvaluationConfigName");
        let id = text(&created, "onlineEvaluationConfigId");
        let arn =
            format!("arn:aws:bedrock-agentcore:{REGION}:{ACCOUNT}:online-evaluation-config/{id}");

        if row.settled_first {
            let settled = poll_until_status(&router, &id, "ACTIVE").await;
            assert_eq!(
                text(&settled, "status"),
                "ACTIVE",
                "case {case:?}: the configuration reaches ACTIVE before the delete, got {settled}"
            );
        }

        let (status, deleted) = delete_config(&router, &id).await;
        assert_eq!(
            (
                status,
                text(&deleted, "onlineEvaluationConfigId"),
                text(&deleted, "onlineEvaluationConfigArn"),
                text(&deleted, "status")
            ),
            (
                202,
                id.clone(),
                arn.clone(),
                "DELETING".to_string()
            ),
            "case {case:?}: the delete is accepted for the configuration the create minted and \
             reports it DELETING, got {status} ({deleted})"
        );

        // The record is still readable while the removal the clock owes is pending, and the
        // deletion stopped the evaluation the configuration describes.
        let (status, pending) = get_config(&router, &id).await;
        assert_eq!(
            (status, text(&pending, "status"), text(&pending, "executionStatus")),
            (200, "DELETING".to_string(), "DISABLED".to_string()),
            "case {case:?}: a read while the removal is pending reports the configuration \
             DELETING with its evaluation stopped, got {status} ({pending})"
        );

        // The operation is @idempotent: a repeat while the deletion is under way reports the
        // same acceptance rather than a second one.
        let (status, again) = delete_config(&router, &id).await;
        assert_eq!(
            (status, text(&again, "status"), text(&again, "onlineEvaluationConfigArn")),
            (202, "DELETING".to_string(), arn.clone()),
            "case {case:?}: a repeated delete reports the acceptance the first one got, got \
             {status} ({again})"
        );

        let (status, gone) = poll_until_gone(&router, &id).await;
        assert_eq!(
            (status, error_code(&gone).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: past the removal the clock owes, the id names no configuration, got \
             {status} {} ({gone})",
            error_code(&gone)
        );
        assert_eq!(
            text(&gone, "message"),
            "Online evaluation configuration not found",
            "case {case:?}: got {gone}"
        );

        // A removed configuration releases the name it reserved, so the same name is creatable
        // again — and the create mints a configuration of its own.
        let (status, recreated) = create_config(&router, &row.body).await;
        assert_eq!(
            status, 202,
            "case {case:?}: the removed configuration's name is free again, got {status} \
             ({recreated})"
        );
        assert_ne!(
            text(&recreated, "onlineEvaluationConfigId"),
            id,
            "case {case:?}: the create after the removal mints a configuration of its own, got \
             {recreated}"
        );
        assert_eq!(
            text(&recreated, "onlineEvaluationConfigId")
                .strip_prefix(&format!("{name}-"))
                .map(str::len),
            Some(10),
            "case {case:?}: the re-created configuration carries the same name, got {recreated}"
        );
    }
}

/// An id naming no configuration this account holds is the modelled 404 and persists nothing,
/// for ids that differ only in which part of a well-formed id they carry.
#[tokio::test]
async fn deleting_an_id_naming_no_config_is_not_found() {
    let router = router();
    let (status, created) = create_config(
        &router,
        &json!({
            "onlineEvaluationConfigName": "heldThroughDeletes",
            "rule": {"samplingConfig": {"samplingPercentage": 1.0}},
            "dataSourceConfig": cloud_watch_logs(json!({
                "serviceNames": ["agentservice"],
                "logGroupNames": ["/aws/agentcore/held"]
            })),
            "evaluationExecutionRoleArn": EXECUTION_ROLE,
            "enableOnCreate": false
        }),
    )
    .await;
    assert_eq!(
        status, 202,
        "case \"the create\": expected HTTP 202, got {status} ({created})"
    );
    let held = text(&created, "onlineEvaluationConfigId");

    // (case, the id the delete names) — each inside `OnlineEvaluationConfigId`'s own format
    // `^[a-zA-Z][a-zA-Z0-9-_]{0,99}-[a-zA-Z0-9]{10}$`.
    let rows: [(&str, String); 4] = [
        (
            "a name this account never created",
            "otherConfig-abcdefghij".to_string(),
        ),
        (
            "the held configuration's name with another suffix",
            format!("heldThroughDeletes-{}", "z".repeat(10)),
        ),
        (
            "an id at the member's minimum length",
            "a-bcdefghijk".to_string(),
        ),
        (
            "an id at the member's maximum length",
            format!("i{}-0123456789", "_".repeat(99)),
        ),
    ];

    for (case, id) in rows {
        let (status, refused) = delete_config(&router, &id).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?} ({id}): expected HTTP 404 ResourceNotFoundException, got {status} {} \
             ({refused})",
            error_code(&refused)
        );
        assert_eq!(
            text(&refused, "message"),
            "Online evaluation configuration not found",
            "case {case:?}: got {refused}"
        );
    }

    let (status, fetched) = get_config(&router, &held).await;
    assert_eq!(
        status, 200,
        "case \"the held configuration\": the refused deletes persisted nothing, got {status} \
         ({fetched})"
    );
}

/// A configuration on its way out cannot take a change: the update collides with the removal
/// the clock owes it, whatever the change asks for.
#[tokio::test]
async fn a_config_being_deleted_refuses_a_change() {
    let router = router();
    let (status, created) = create_config(
        &router,
        &json!({
            "onlineEvaluationConfigName": "changedWhileDeleting",
            "rule": {"samplingConfig": {"samplingPercentage": 5.0}},
            "dataSourceConfig": cloud_watch_logs(json!({
                "serviceNames": ["agentservice"],
                "logGroupNames": ["/aws/agentcore/deleting"]
            })),
            "evaluationExecutionRoleArn": EXECUTION_ROLE,
            "enableOnCreate": true
        }),
    )
    .await;
    assert_eq!(
        status, 202,
        "case \"the create\": expected HTTP 202, got {status} ({created})"
    );
    let id = text(&created, "onlineEvaluationConfigId");
    let settled = poll_until_status(&router, &id, "ACTIVE").await;
    assert_eq!(
        text(&settled, "status"),
        "ACTIVE",
        "case \"before the delete\": the configuration reaches ACTIVE, got {settled}"
    );

    let (status, deleted) = delete_config(&router, &id).await;
    assert_eq!(
        status, 202,
        "case \"the delete\": expected HTTP 202, got {status} ({deleted})"
    );

    // (case, the change the update asks for) — changes that differ in the member they carry.
    let changes: [(&str, Value); 3] = [
        (
            "a changed description",
            json!({"description": "changed while the removal is pending"}),
        ),
        (
            "a changed sampling rule",
            json!({"rule": {"samplingConfig": {"samplingPercentage": 99.0}}}),
        ),
        (
            "the evaluation asked to start again",
            json!({"executionStatus": "ENABLED"}),
        ),
    ];

    for (case, change) in changes {
        let (status, refused) = update_config(&router, &id, &change).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (409, "ConflictException"),
            "case {case:?}: expected HTTP 409 ConflictException, got {status} {} ({refused})",
            error_code(&refused)
        );
        assert_eq!(
            text(&refused, "message"),
            format!(
                "Online evaluation configuration with ID {id} is in DELETING status and cannot \
                 be modified. Retry the request once its status is ACTIVE."
            ),
            "case {case:?}: got {refused}"
        );
    }

    // The refused changes persisted nothing: the configuration is still on its way out with
    // its evaluation stopped.
    let (status, pending) = get_config(&router, &id).await;
    assert_eq!(
        (
            status,
            text(&pending, "status"),
            text(&pending, "executionStatus")
        ),
        (200, "DELETING".to_string(), "DISABLED".to_string()),
        "case \"after the refused changes\": the configuration is unchanged, got {status} \
         ({pending})"
    );
}
