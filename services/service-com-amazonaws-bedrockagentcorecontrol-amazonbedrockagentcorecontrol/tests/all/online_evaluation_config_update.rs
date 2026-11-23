// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `UpdateOnlineEvaluationConfig`: the PATCH of one online
//! evaluation configuration's settings, what a read of it reports afterwards, the status the
//! clock owes it, the replayed `clientToken`, the id that names nothing and the status that
//! blocks a further change.
//!
//! Every test drives the operations as a client does — one shared engine behind one router —
//! so what an update writes is what the next `GetOnlineEvaluationConfig` reads, rather than
//! one envelope being asserted against another.
//!
//! Sources: `smithy_query(query="operation",
//! operation="UpdateOnlineEvaluationConfig,GetOnlineEvaluationConfig,CreateOnlineEvaluationConfig")`
//! for every member, id, ARN form, enum value and bound asserted here, and the operation's own
//! documentation for the replayed `clientToken` ("If this token matches a previous request,
//! the service ignores the request, but doesn't return an error").
//!
//! The clock-owed transitions are waited for by POLLING the configuration until it reports
//! the status the test needs, never by assuming how long the host takes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region the tests sign for, which the ARNs they assert carry.
const REGION: &str = "us-east-1";

/// The account id every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// How long a poll for a clock-owed status keeps looking, and how long it waits between
/// looks. Generous: the condition is what the test waits for, not a duration.
const POLL_DEADLINE: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// IAM role ARNs inside `RoleArn`'s pattern `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$`.
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/AgentCoreEvaluation";
const REPLACEMENT_ROLE: &str = "arn:aws:iam::000000000000:role/AgentCoreEvaluationUpdated";

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

/// `UpdateOnlineEvaluationConfig`: `PUT /online-evaluation-configs/{onlineEvaluationConfigId}`.
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

/// One instant a response reports, as the seconds since the epoch `restJson1` renders a
/// `smithy.api#Timestamp` as.
fn instant(body: &Value, member: &str, case: &str) -> f64 {
    body.get(member)
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("case {case:?}: {member} is an epoch-seconds number, got {body}"))
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

/// The sentence this plane refuses a change with while the configuration's own asynchronous
/// work is still under way. The text is fixed; its variable parts are the id the path carried
/// and the status the configuration is in right now.
fn status_conflict_message(id: &str, status: &str) -> String {
    format!(
        "Online evaluation configuration with ID {id} is in {status} status and cannot be \
         modified. Retry the request once its status is ACTIVE."
    )
}

/// The configuration a create carries, which every row of the patch table starts from: every
/// member an update can replace is set here, so an omitted member has a stored value to be
/// left at.
fn base_body(name: &str, enable_on_create: bool) -> Value {
    json!({
        "onlineEvaluationConfigName": name,
        "description": "the configuration as created",
        "rule": {
            "samplingConfig": {"samplingPercentage": 10.0},
            "filters": [
                {"key": "agent.name", "operator": "Equals",
                 "value": {"stringValue": "asCreated"}}
            ],
            "sessionConfig": {"sessionTimeoutMinutes": 30}
        },
        "dataSourceConfig": cloud_watch_logs(json!({
            "serviceNames": ["agentservice"],
            "logGroupNames": ["/aws/agentcore/as-created"]
        })),
        "evaluators": [{"evaluatorId": "Builtin.Correctness"}],
        "insights": [{"insightId": "Builtin.TopicClusters"}],
        "clusteringConfig": {"frequencies": ["DAILY"]},
        "outputConfig": {"cloudWatchConfig": {
            "logGroupName": "/aws/agentcore/as-created",
            "metricsNamespace": "AgentCore/AsCreated",
            "resultDestination": "SOURCE_LOG_GROUP"
        }},
        "evaluationExecutionRoleArn": EXECUTION_ROLE,
        "enableOnCreate": enable_on_create
    })
}

/// Look again until the configuration reports `want`, which is what a caller polling the
/// status field does. The deadline only catches a configuration that never settles at all.
async fn poll_until_status(router: &MultiServiceRouter, id: &str, want: &str, case: &str) -> Value {
    let started = Instant::now();
    loop {
        let (status, body) = get_config(router, id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read of {id} is served while polling for {want}, got {status} \
             ({body})"
        );
        if text(&body, "status") == want {
            return body;
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case {case:?}: {id} never reported {want}, last read {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Create a configuration and wait for the `ACTIVE` the create owes it, so an update of it is
/// not refused for work still under way.
async fn active_config(router: &MultiServiceRouter, body: &Value, case: &str) -> String {
    let (status, created) = create_config(router, body).await;
    assert_eq!(
        status, 202,
        "case {case:?}: expected HTTP 202 from CreateOnlineEvaluationConfig, got {status} \
         ({created})"
    );
    let id = text(&created, "onlineEvaluationConfigId");
    poll_until_status(router, &id, "ACTIVE", case).await;
    id
}

/// One patch this suite drives.
struct Row {
    case: &'static str,
    /// The name the configuration this row patches is created under
    /// (`EvaluationConfigName` is `^[a-zA-Z][a-zA-Z0-9_]{0,47}$`).
    name: &'static str,
    /// Whether the configuration is created with its evaluation running.
    enabled_on_create: bool,
    /// The body of the update — the members it carries are the ones that change.
    patch: Value,
}

/// The rows: patches that differ in which members they carry, how many, the type of each
/// (string, structure, union, list, enum), the size of the lists in them (each published bound
/// of `EvaluatorList`'s 0..25 and `InsightList`'s 0..10) and the value of the enum.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the description alone",
            name: "patchDescription",
            enabled_on_create: false,
            patch: json!({"description": "the configuration as updated"}),
        },
        Row {
            case: "the execution status alone, enabling the evaluation",
            name: "patchEnable",
            enabled_on_create: false,
            patch: json!({"executionStatus": "ENABLED"}),
        },
        Row {
            case: "the execution status alone, disabling the evaluation",
            name: "patchDisable",
            enabled_on_create: true,
            patch: json!({"executionStatus": "DISABLED"}),
        },
        Row {
            case: "the rule and the data source together, in the prefix form",
            name: "patchRuleAndSource",
            enabled_on_create: true,
            patch: json!({
                "rule": {
                    "samplingConfig": {"samplingPercentage": 100.0},
                    "filters": [
                        {"key": "latency_ms", "operator": "GreaterThanOrEqual",
                         "value": {"doubleValue": 250.0}},
                        {"key": "session.ok", "operator": "NotEquals",
                         "value": {"booleanValue": false}}
                    ],
                    "sessionConfig": {"sessionTimeoutMinutes": 1440}
                },
                "dataSourceConfig": cloud_watch_logs(json!({
                    "serviceNames": ["checkoutagent"],
                    "logGroupNames": [],
                    "logGroupNamePrefixes": ["/aws/agentcore/", "/aws/lambda/"]
                }))
            }),
        },
        Row {
            case: "the lists emptied, at each list member's minimum size",
            name: "patchEmptyLists",
            enabled_on_create: false,
            patch: json!({"evaluators": [], "insights": [], "clusteringConfig": {"frequencies": []}}),
        },
        Row {
            case: "the lists at each list member's maximum size",
            name: "patchFullLists",
            enabled_on_create: false,
            patch: json!({
                // `EvaluatorList` is @length 0..25 and `InsightList` 0..10.
                "evaluators": (0..25)
                    .map(|index| json!({"evaluatorId": format!("Builtin.Evaluator{index}")}))
                    .collect::<Vec<_>>(),
                "insights": (0..10)
                    .map(|index| json!({"insightId": format!("Builtin.Insight{index}")}))
                    .collect::<Vec<_>>()
            }),
        },
        Row {
            case: "every member an update can replace at once",
            name: "patchEverything",
            enabled_on_create: false,
            patch: json!({
                "description": "x".repeat(200),
                "rule": {
                    "samplingConfig": {"samplingPercentage": 0.01},
                    "filters": [{"key": "agent.name", "operator": "Equals",
                                 "value": {"stringValue": "asUpdated"}}],
                    "sessionConfig": {"sessionTimeoutMinutes": 1}
                },
                "dataSourceConfig": cloud_watch_logs(json!({
                    "serviceNames": ["updatedagent"],
                    "logGroupNames": ["/aws/agentcore/as-updated", "/aws/agentcore/second"]
                })),
                "evaluators": [{"evaluatorId": "ThirdParty.vendor.quality"}],
                "insights": [{"insightId": "Builtin.ErrorClusters"}],
                "clusteringConfig": {"frequencies": ["WEEKLY", "MONTHLY"]},
                "outputConfig": {"cloudWatchConfig": {
                    "logGroupName": "/aws/agentcore/as-updated",
                    "metricsNamespace": "AgentCore/AsUpdated",
                    "resultDestination": "SOURCE_LOG_GROUP"
                }},
                "evaluationExecutionRoleArn": REPLACEMENT_ROLE,
                "executionStatus": "ENABLED"
            }),
        },
        Row {
            case: "no member beside the id the path carries",
            name: "patchNothing",
            enabled_on_create: true,
            patch: json!({}),
        },
    ]
}

/// Every member an update carries replaces the stored one and every member it omits is left
/// as it was: the read of the configuration afterwards reports the patch over what the create
/// persisted.
#[tokio::test]
async fn an_update_replaces_the_members_it_carries_and_leaves_the_rest() {
    let router = router();

    for row in &rows() {
        let case = row.case;
        let created_body = base_body(row.name, row.enabled_on_create);
        let id = active_config(&router, &created_body, case).await;
        let (_, before) = get_config(&router, &id).await;

        let (status, updated) = update_config(&router, &id, &row.patch).await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected HTTP 202 from UpdateOnlineEvaluationConfig, got {status} \
             ({updated})"
        );
        let expected_arn =
            format!("arn:aws:bedrock-agentcore:{REGION}:{ACCOUNT}:online-evaluation-config/{id}");
        // The execution status the update reports is the one it asked for, or the stored one
        // when it asked for none.
        let expected_execution_status = row
            .patch
            .get("executionStatus")
            .and_then(Value::as_str)
            .map_or_else(|| text(&before, "executionStatus"), ToString::to_string);
        assert_eq!(
            (
                text(&updated, "onlineEvaluationConfigId"),
                text(&updated, "onlineEvaluationConfigArn"),
                text(&updated, "status"),
                text(&updated, "executionStatus")
            ),
            (
                id.clone(),
                expected_arn,
                "UPDATING".to_string(),
                expected_execution_status.clone()
            ),
            "case {case:?}: the update reports the configuration it wrote, the status it \
             enters and the execution status it asked for, got {updated}"
        );
        assert!(
            updated.get("updatedAt").is_some() && updated.get("failureReason").is_none(),
            "case {case:?}: the update reports updatedAt and, with nothing failed, no \
             failureReason, got {updated}"
        );

        let (status, after) = get_config(&router, &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetOnlineEvaluationConfig, got {status} \
             ({after})"
        );
        // A member the patch carried reads back as the patch's value; a member it omitted
        // reads back as the value the create persisted.
        for member in [
            "description",
            "rule",
            "evaluators",
            "insights",
            "clusteringConfig",
            "outputConfig",
            "evaluationExecutionRoleArn",
            "executionStatus",
        ] {
            let expected = row.patch.get(member).or_else(|| before.get(member));
            assert_eq!(
                after.get(member),
                expected,
                "case {case:?}: the read reports {member} as {expected:?}, got {after}"
            );
        }
        // The data source is compared member by member: `logGroupNames` carries a modelled
        // default, so a read reports it whether or not the request named it.
        let expected_source = row
            .patch
            .get("dataSourceConfig")
            .unwrap_or_else(|| &before["dataSourceConfig"])["cloudWatchLogs"]
            .clone();
        let read_source = &after["dataSourceConfig"]["cloudWatchLogs"];
        for member in ["serviceNames", "logGroupNames", "logGroupNamePrefixes"] {
            if let Some(expected) = expected_source.get(member) {
                assert_eq!(
                    read_source.get(member),
                    Some(expected),
                    "case {case:?}: the read reports the data source's {member} as \
                     {expected:?}, got {after}"
                );
            }
        }
        // The update is a write of the configuration, not of its identity: the name, the id,
        // the ARN and the instant of the create are the create's.
        assert_eq!(
            (
                after.get("onlineEvaluationConfigName"),
                after.get("onlineEvaluationConfigId"),
                after.get("onlineEvaluationConfigArn"),
                after.get("createdAt")
            ),
            (
                before.get("onlineEvaluationConfigName"),
                before.get("onlineEvaluationConfigId"),
                before.get("onlineEvaluationConfigArn"),
                before.get("createdAt")
            ),
            "case {case:?}: the update left the configuration's identity and creation \
             instant alone, got {after}"
        );
        let created_at = instant(&after, "createdAt", case);
        let updated_at = instant(&after, "updatedAt", case);
        assert!(
            updated_at >= created_at,
            "case {case:?}: the write instant is not before the creation instant, got \
             {updated_at} and {created_at} ({after})"
        );
        assert_eq!(
            after.get("updatedAt"),
            updated.get("updatedAt"),
            "case {case:?}: the read reports the instant the update wrote, got {after}"
        );
    }
}

/// The transition an accepted update owes: the configuration reports `UPDATING` while the
/// write is under way and `ACTIVE` once the span has passed, with the execution status the
/// update asked for carried across it. The test waits for the CONDITION, polling the read.
#[tokio::test]
async fn an_updated_config_settles_from_updating_to_active() {
    let case = "the accepted update";
    let router = router();
    let id = active_config(&router, &base_body("settlingUpdate", false), case).await;

    let (status, updated) = update_config(&router, &id, &json!({"executionStatus": "ENABLED"})).await;
    assert_eq!(
        (status, text(&updated, "status")),
        (202, "UPDATING".to_string()),
        "case {case:?}: the update is accepted and leaves the configuration UPDATING, got \
         {status} ({updated})"
    );

    let settled = poll_until_status(&router, &id, "ACTIVE", case).await;
    // The transition is the status's alone: what the update asked of the evaluation is
    // unchanged by it.
    assert_eq!(
        text(&settled, "executionStatus"),
        "ENABLED",
        "case {case:?}: past the settle span the evaluation is still the one the update asked \
         for, got {settled}"
    );
}

/// A configuration whose own asynchronous work is still under way refuses a further change
/// the modelled conflict, names what to wait for, and keeps the configuration it had.
#[tokio::test]
async fn a_config_that_is_not_active_refuses_a_further_update() {
    // (case, the body of the update that is refused)
    let refused_updates = [
        (
            "a replacement description",
            json!({"description": "never applied"}),
        ),
        ("a replacement execution status", json!({"executionStatus": "ENABLED"})),
        ("no member at all", json!({})),
    ];

    for (case, patch) in &refused_updates {
        // While the CREATE is still under way: the configuration is `CREATING`, so a caller
        // acting straight after the create meets the conflict.
        let router = router();
        let (status, created) = create_config(&router, &base_body("stillCreating", false)).await;
        assert_eq!(
            (status, text(&created, "status")),
            (202, "CREATING".to_string()),
            "case {case:?}: the create reports CREATING, got {status} ({created})"
        );
        let id = text(&created, "onlineEvaluationConfigId");

        let (status, refused) = update_config(&router, &id, patch).await;
        assert_eq!(
            (status, error_code(&refused), text(&refused, "message")),
            (
                409,
                "ConflictException".to_string(),
                status_conflict_message(&id, "CREATING")
            ),
            "case {case:?}/CREATING: expected the modelled conflict naming CREATING, got \
             {status} ({refused})"
        );
        let (_, unchanged) = get_config(&router, &id).await;
        assert_eq!(
            unchanged.get("description"),
            Some(&json!("the configuration as created")),
            "case {case:?}/CREATING: the refused update changed nothing, got {unchanged}"
        );

        // And while an accepted UPDATE is still under way: the configuration is `UPDATING`,
        // which is the concurrent change the sentence is about.
        let settled_id = active_config(&router, &base_body("stillUpdating", false), case).await;
        let (status, accepted) = update_config(
            &router,
            &settled_id,
            &json!({"description": "the update that won"}),
        )
        .await;
        assert_eq!(
            (status, text(&accepted, "status")),
            (202, "UPDATING".to_string()),
            "case {case:?}: the first update is accepted and leaves the configuration \
             UPDATING, got {status} ({accepted})"
        );
        let (status, second) = update_config(&router, &settled_id, patch).await;
        assert_eq!(
            (status, error_code(&second), text(&second, "message")),
            (
                409,
                "ConflictException".to_string(),
                status_conflict_message(&settled_id, "UPDATING")
            ),
            "case {case:?}/UPDATING: expected the modelled conflict naming UPDATING, got \
             {status} ({second})"
        );
        let (_, unchanged) = get_config(&router, &settled_id).await;
        assert_eq!(
            unchanged.get("description"),
            Some(&json!("the update that won")),
            "case {case:?}/UPDATING: the refused update left the accepted one's write alone, \
             got {unchanged}"
        );
    }
}

/// An id naming no configuration this account holds is the modelled 404, for ids that differ
/// only in which part of a well-formed id they carry, and nothing is written for it.
#[tokio::test]
async fn an_update_of_an_id_naming_no_config_is_not_found() {
    let case = "the held configuration";
    let router = router();
    let held = active_config(&router, &base_body("heldForUpdate", false), case).await;

    // (case, the id the update names) — each inside `OnlineEvaluationConfigId`'s own format
    // `^[a-zA-Z][a-zA-Z0-9-_]{0,99}-[a-zA-Z0-9]{10}$`.
    let rows: [(&str, String); 3] = [
        (
            "a name this account never created",
            "otherConfig-abcdefghij".to_string(),
        ),
        (
            "the held configuration's name with another suffix",
            format!("heldForUpdate-{}", "z".repeat(10)),
        ),
        (
            "an id at the member's minimum length",
            "a-bcdefghijk".to_string(),
        ),
    ];

    for (case, id) in rows {
        let (status, refused) =
            update_config(&router, &id, &json!({"description": "never applied"})).await;
        assert_eq!(
            (status, error_code(&refused), text(&refused, "message")),
            (
                404,
                "ResourceNotFoundException".to_string(),
                format!("Online evaluation configuration with ID {id} does not exist")
            ),
            "case {case:?} ({id}): expected the modelled 404 naming the id, got {status} \
             ({refused})"
        );
        let (status, fetched) = get_config(&router, &id).await;
        assert_eq!(
            status, 404,
            "case {case:?}: the refused update minted no configuration, got {status} \
             ({fetched})"
        );
    }

    let (status, fetched) = get_config(&router, &held).await;
    assert_eq!(
        (status, text(&fetched, "description")),
        (200, "the configuration as created".to_string()),
        "case \"the held configuration\": the refusals left it as the create wrote it, got \
         {status} ({fetched})"
    );
}

/// A replayed `clientToken` answers with the configuration that token first updated rather
/// than applying the change a second time — whatever else the repeated request carries.
#[tokio::test]
async fn a_repeated_client_token_applies_the_update_once() {
    let case = "the replayed token";
    let router = router();
    let id = active_config(&router, &base_body("idempotentUpdate", false), case).await;
    // `ClientToken` is @length 33..256 with pattern `^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}$`.
    let token = "b".repeat(33);

    let first_patch = json!({
        "clientToken": token,
        "description": "the update the token carried",
        "executionStatus": "ENABLED"
    });
    let (status, first) = update_config(&router, &id, &first_patch).await;
    assert_eq!(
        status, 202,
        "case {case:?}: the first update is accepted, got {status} ({first})"
    );

    let (status, replayed) = update_config(
        &router,
        &id,
        &json!({
            "clientToken": token,
            "description": "the update the replay carried",
            "executionStatus": "DISABLED"
        }),
    )
    .await;
    assert_eq!(
        (
            status,
            text(&replayed, "onlineEvaluationConfigId"),
            replayed.get("updatedAt"),
            text(&replayed, "executionStatus")
        ),
        (
            202,
            id.clone(),
            first.get("updatedAt"),
            "ENABLED".to_string()
        ),
        "case {case:?}: expected the configuration the token first updated, got {status} \
         ({replayed})"
    );

    // The replay persisted nothing of its own: the stored configuration is still the first
    // update's.
    let (_, fetched) = get_config(&router, &id).await;
    assert_eq!(
        (
            fetched.get("description"),
            text(&fetched, "executionStatus")
        ),
        (
            first_patch.get("description"),
            "ENABLED".to_string()
        ),
        "case {case:?}: the replay left the first update's write alone, got {fetched}"
    );

    // The record is what that token applied to THAT configuration: the same token against
    // another configuration is not a replay of this request, so it is applied to it.
    let other = active_config(&router, &base_body("otherForToken", false), case).await;
    let (status, applied) = update_config(
        &router,
        &other,
        &json!({"clientToken": token, "description": "the update of the other configuration"}),
    )
    .await;
    assert_eq!(
        (status, text(&applied, "onlineEvaluationConfigId")),
        (202, other.clone()),
        "case \"the token against another configuration\": the update is applied to the \
         configuration the path named, got {status} ({applied})"
    );
    let (_, fetched) = get_config(&router, &other).await;
    assert_eq!(
        fetched.get("description"),
        Some(&json!("the update of the other configuration")),
        "case \"the token against another configuration\": the write landed, got {fetched}"
    );

    // And the id the path carries is resolved first: a token this store remembers does not
    // answer for an id naming no configuration.
    let (status, refused) = update_config(
        &router,
        "neverCreated-abcdefghij",
        &json!({"clientToken": token, "description": "never applied"}),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused)),
        (404, "ResourceNotFoundException".to_string()),
        "case \"the token against an id naming nothing\": expected the modelled 404, got \
         {status} ({refused})"
    );
}

/// `executionStatus` is `OnlineEvaluationExecutionStatus`, whose value set the model fixes:
/// a value outside it is refused at the boundary rather than persisted, so no read can report
/// an execution status the service never has.
#[tokio::test]
async fn an_execution_status_outside_the_modelled_set_is_refused() {
    let case = "an off-enum execution status";
    let router = router();
    let id = active_config(&router, &base_body("enumBoundConfig", false), case).await;

    // `OnlineEvaluationExecutionStatus` is `DISABLED | ENABLED`
    // (`smithy_query shape=OnlineEvaluationExecutionStatus`).
    for value in ["enabled", "PAUSED", ""] {
        let (status, refused) = update_config(&router, &id, &json!({"executionStatus": value})).await;
        assert_eq!(
            (status, error_code(&refused)),
            (400, "ValidationException".to_string()),
            "case {case:?} ({value:?}): expected HTTP 400 ValidationException, got {status} \
             ({refused})"
        );
        let (_, fetched) = get_config(&router, &id).await;
        assert_eq!(
            text(&fetched, "executionStatus"),
            "DISABLED",
            "case {case:?} ({value:?}): the refused update left the stored execution status \
             alone, got {fetched}"
        );
    }
}
