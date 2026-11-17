// SPDX-License-Identifier: CC0-1.0

//! The online evaluation configuration end to end over HTTP: what
//! `CreateOnlineEvaluationConfig` persists, how `GetOnlineEvaluationConfig` reports it,
//! whether its evaluation is running, and the status the clock owes it.
//!
//! Every row drives the operations as a client does — one shared engine behind one router —
//! so a create and the read that follows it are asserted against the same store rather than
//! against each other's envelopes.
//!
//! Sources: `smithy_query(query="closure", operation="CreateOnlineEvaluationConfig")` and
//! `smithy_query(query="operation", operation="GetOnlineEvaluationConfig")` for every member,
//! id, ARN form and enum value asserted here, and the operation's own documentation for the
//! name being unique within the account, for `enableOnCreate` ("Whether to enable the online
//! evaluation configuration immediately upon creation") and for the replayed `clientToken`.

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

/// How long a poll for the clock-owed `ACTIVE` keeps looking, and how long it waits between
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

/// One create this suite drives, and what a read of the configuration must report back.
struct Row {
    case: &'static str,
    body: Value,
}

/// The rows: creates that differ in the members they carry, the sizes of the lists in them,
/// the form of the data source, the sampling percentage (each published bound of
/// `SamplingPercentage`'s 0.01..100 range) and the name's length.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the required members alone, at the sampling range's minimum",
            body: json!({
                "onlineEvaluationConfigName": "minimalConfig",
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
            case: "every optional member beside them, at the sampling range's maximum",
            body: json!({
                "onlineEvaluationConfigName": "fullConfig",
                "description": "the online evaluation config the suite reads back",
                "rule": {
                    "samplingConfig": {"samplingPercentage": 100.0},
                    "filters": [
                        {"key": "agent.name", "operator": "Equals",
                         "value": {"stringValue": "checkout"}},
                        {"key": "latency_ms", "operator": "GreaterThanOrEqual",
                         "value": {"doubleValue": 250.0}}
                    ],
                    "sessionConfig": {"sessionTimeoutMinutes": 1440}
                },
                "dataSourceConfig": cloud_watch_logs(json!({
                    "serviceNames": ["checkoutagent"],
                    "logGroupNames": ["/aws/agentcore/one", "/aws/agentcore/two"]
                })),
                "evaluators": [
                    {"evaluatorId": "Builtin.Correctness"},
                    {"evaluatorId": "ThirdParty.vendor.quality"}
                ],
                "insights": [{"insightId": "Builtin.TopicClusters"}],
                "clusteringConfig": {"frequencies": ["DAILY", "WEEKLY", "MONTHLY"]},
                "outputConfig": {"cloudWatchConfig": {
                    "logGroupName": "/aws/agentcore/evaluations",
                    "metricsNamespace": "AgentCore/Evaluations",
                    "resultDestination": "SOURCE_LOG_GROUP"
                }},
                "evaluationExecutionRoleArn": EXECUTION_ROLE,
                "enableOnCreate": true,
                "tags": {"owner": "agentcore", "tier": "test"}
            }),
        },
        Row {
            case: "a name at the member's maximum length, the prefix form of the data source \
                   and the lists at their minimum size",
            body: json!({
                // `EvaluationConfigName` is `^[a-zA-Z][a-zA-Z0-9_]{0,47}$`.
                "onlineEvaluationConfigName": format!("c{}", "x".repeat(47)),
                "rule": {
                    "samplingConfig": {"samplingPercentage": 12.5},
                    "filters": [{"key": "session.ok", "operator": "NotEquals",
                                 "value": {"booleanValue": false}}],
                    "sessionConfig": {"sessionTimeoutMinutes": 1}
                },
                "dataSourceConfig": cloud_watch_logs(json!({
                    "serviceNames": ["prefixedagent"],
                    "logGroupNames": [],
                    "logGroupNamePrefixes": ["/aws/agentcore/", "/aws/lambda/"]
                })),
                "evaluators": [],
                "insights": [],
                "clusteringConfig": {"frequencies": []},
                "evaluationExecutionRoleArn": EXECUTION_ROLE,
                "enableOnCreate": true
            }),
        },
    ]
}

/// What `CreateOnlineEvaluationConfig` persists is what a read of the configuration reports:
/// the whole configuration the create carried, the id and ARN it minted, the status it
/// enters and the execution status `enableOnCreate` asked for.
#[tokio::test]
async fn a_created_online_evaluation_config_reads_back() {
    let router = router();

    for row in &rows() {
        let case = row.case;
        let (status, created) = create_config(&router, &row.body).await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected HTTP 202 from CreateOnlineEvaluationConfig, got {status} \
             ({created})"
        );
        let name = text(&row.body, "onlineEvaluationConfigName");
        let id = text(&created, "onlineEvaluationConfigId");
        let expected_arn =
            format!("arn:aws:bedrock-agentcore:{REGION}:{ACCOUNT}:online-evaluation-config/{id}");
        let enabled = row.body["enableOnCreate"].as_bool().unwrap_or_default();
        let expected_execution_status = if enabled { "ENABLED" } else { "DISABLED" };
        assert_eq!(
            (
                text(&created, "onlineEvaluationConfigArn"),
                text(&created, "status"),
                text(&created, "executionStatus")
            ),
            (
                expected_arn.clone(),
                "CREATING".to_string(),
                expected_execution_status.to_string()
            ),
            "case {case:?}: the create reports the configuration's ARN, the status it enters \
             and the execution status enableOnCreate asked for, got {created}"
        );
        assert_eq!(
            created.get("outputConfig"),
            row.body.get("outputConfig"),
            "case {case:?}: the create reports the output configuration it carried, got {created}"
        );
        assert!(
            created.get("createdAt").is_some() && created.get("failureReason").is_none(),
            "case {case:?}: the create reports createdAt and, with nothing failed, no \
             failureReason, got {created}"
        );
        // `OnlineEvaluationConfigId` is `^[a-zA-Z][a-zA-Z0-9-_]{0,99}-[a-zA-Z0-9]{10}$`: the
        // name, then a 10-character suffix this service minted.
        let suffix = id
            .strip_prefix(&format!("{name}-"))
            .unwrap_or_else(|| panic!("case {case:?}: {id:?} is not {name:?} plus a suffix"));
        assert!(
            suffix.len() == 10 && suffix.chars().all(|character| character.is_ascii_alphanumeric()),
            "case {case:?}: the minted id's suffix is 10 alphanumerics, got {suffix:?}"
        );

        let (status, fetched) = get_config(&router, &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetOnlineEvaluationConfig, got {status} \
             ({fetched})"
        );
        assert_eq!(
            (
                text(&fetched, "onlineEvaluationConfigId"),
                text(&fetched, "onlineEvaluationConfigArn"),
                text(&fetched, "onlineEvaluationConfigName"),
                text(&fetched, "executionStatus"),
                text(&fetched, "evaluationExecutionRoleArn")
            ),
            (
                id.clone(),
                expected_arn,
                name.clone(),
                expected_execution_status.to_string(),
                EXECUTION_ROLE.to_string()
            ),
            "case {case:?}: the read reports the configuration the create minted, got {fetched}"
        );
        // The read reports the status the one record carries at the instant it is read: the
        // transient one until the transition the clock owes comes due, the settled one
        // afterwards, and nothing else.
        // `a_created_config_settles_from_creating_to_active` is where that transition itself
        // is asserted, by polling for it.
        let read_status = text(&fetched, "status");
        assert!(
            read_status == "CREATING" || read_status == "ACTIVE",
            "case {case:?}: the read reports the status the record carries, got {read_status:?} \
             ({fetched})"
        );
        for member in [
            "description",
            "rule",
            "insights",
            "clusteringConfig",
            "outputConfig",
        ] {
            assert_eq!(
                fetched.get(member),
                row.body.get(member),
                "case {case:?}: the read reports the {member} the create persisted, got {fetched}"
            );
        }
        // `evaluators` is optional on the create and carries the modelled default `[]` on a
        // read, so a create that named none reads back as the empty list.
        assert_eq!(
            fetched.get("evaluators"),
            Some(row.body.get("evaluators").unwrap_or(&json!([]))),
            "case {case:?}: the read reports the evaluators the create persisted, got {fetched}"
        );
        // The data source is compared member by member: `logGroupNames` carries a modelled
        // default, so a read reports it whether or not the create named it.
        let requested_source = &row.body["dataSourceConfig"]["cloudWatchLogs"];
        let read_source = &fetched["dataSourceConfig"]["cloudWatchLogs"];
        for member in ["serviceNames", "logGroupNames", "logGroupNamePrefixes"] {
            if let Some(expected) = requested_source.get(member) {
                assert_eq!(
                    read_source.get(member),
                    Some(expected),
                    "case {case:?}: the read reports the data source's {member}, got {fetched}"
                );
            }
        }
        for instant in ["createdAt", "updatedAt"] {
            assert!(
                fetched.get(instant).is_some(),
                "case {case:?}: the read reports {instant}, got {fetched}"
            );
        }
    }
}

/// `enableOnCreate` is what the execution status is read off, and the create and the read
/// agree about it: `true` begins the evaluation, `false` leaves it stopped.
#[tokio::test]
async fn the_execution_status_follows_enable_on_create() {
    let router = router();
    // (case, enableOnCreate, the `OnlineEvaluationExecutionStatus` it means)
    let rows: [(&str, bool, &str); 2] = [
        ("enabled on create", true, "ENABLED"),
        ("not enabled on create", false, "DISABLED"),
    ];

    for (case, enable_on_create, expected) in rows {
        let body = json!({
            "onlineEvaluationConfigName": if enable_on_create { "runningConfig" }
                                          else { "stoppedConfig" },
            "rule": {"samplingConfig": {"samplingPercentage": 5.0}},
            "dataSourceConfig": cloud_watch_logs(json!({
                "serviceNames": ["agentservice"],
                "logGroupNames": ["/aws/agentcore/enablement"]
            })),
            "evaluationExecutionRoleArn": EXECUTION_ROLE,
            "enableOnCreate": enable_on_create
        });
        let (status, created) = create_config(&router, &body).await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected HTTP 202, got {status} ({created})"
        );
        let id = text(&created, "onlineEvaluationConfigId");
        let (_, fetched) = get_config(&router, &id).await;
        assert_eq!(
            (
                text(&created, "executionStatus"),
                text(&fetched, "executionStatus")
            ),
            (expected.to_string(), expected.to_string()),
            "case {case:?}: the create and the read agree the evaluation is {expected}, got \
             {created} and {fetched}"
        );
    }
}

/// The name "Must be unique within your account": a second create carrying a name this
/// account already holds is the modelled conflict, and leaves the first configuration as the
/// first create wrote it.
#[tokio::test]
async fn a_name_this_account_already_holds_is_a_conflict() {
    let router = router();
    let body = json!({
        "onlineEvaluationConfigName": "uniqueConfig",
        "rule": {"samplingConfig": {"samplingPercentage": 10.0}},
        "dataSourceConfig": cloud_watch_logs(json!({
            "serviceNames": ["agentservice"],
            "logGroupNames": ["/aws/agentcore/first"]
        })),
        "evaluationExecutionRoleArn": EXECUTION_ROLE,
        "enableOnCreate": false
    });
    let (status, created) = create_config(&router, &body).await;
    assert_eq!(
        status, 202,
        "case \"the first create\": expected HTTP 202, got {status} ({created})"
    );
    let first_id = text(&created, "onlineEvaluationConfigId");

    // A second create of the same name, with a different rule, is still the name that
    // collides.
    let mut again = body.clone();
    again["rule"]["samplingConfig"]["samplingPercentage"] = json!(99.0);
    let (status, refused) = create_config(&router, &again).await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (409, "ConflictException"),
        "case \"a repeated name\": expected HTTP 409 ConflictException, got {status} {} \
         ({refused})",
        error_code(&refused)
    );
    assert_eq!(
        text(&refused, "message"),
        "Online evaluation configuration with same name already exists",
        "case \"a repeated name\": got {refused}"
    );

    let (_, fetched) = get_config(&router, &first_id).await;
    assert_eq!(
        fetched.get("rule"),
        body.get("rule"),
        "case \"a repeated name\": the refused create left the first configuration as it was, \
         got {fetched}"
    );
}

/// A replayed `clientToken` answers with the configuration that token first produced rather
/// than minting a second one — whatever else the repeated request carries.
#[tokio::test]
async fn a_repeated_client_token_answers_with_the_first_config() {
    let router = router();
    // `ClientToken` is @length 33..256 with pattern `^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}$`.
    let token = "a".repeat(33);
    let body = json!({
        "onlineEvaluationConfigName": "idempotentConfig",
        "clientToken": token,
        "rule": {"samplingConfig": {"samplingPercentage": 25.0}},
        "dataSourceConfig": cloud_watch_logs(json!({
            "serviceNames": ["agentservice"],
            "logGroupNames": ["/aws/agentcore/idempotent"]
        })),
        "evaluationExecutionRoleArn": EXECUTION_ROLE,
        "enableOnCreate": false
    });
    let (status, first) = create_config(&router, &body).await;
    assert_eq!(
        status, 202,
        "case \"the first create\": expected HTTP 202, got {status} ({first})"
    );

    let mut again = body.clone();
    again["rule"]["samplingConfig"]["samplingPercentage"] = json!(75.0);
    let (status, replayed) = create_config(&router, &again).await;
    assert_eq!(
        (status, text(&replayed, "onlineEvaluationConfigId")),
        (202, text(&first, "onlineEvaluationConfigId")),
        "case \"the replayed token\": expected the configuration the token first produced, got \
         {status} ({replayed})"
    );

    // The replay persisted nothing of its own: the stored configuration is still the first
    // create's.
    let (_, fetched) = get_config(&router, &text(&first, "onlineEvaluationConfigId")).await;
    assert_eq!(
        fetched.get("rule"),
        body.get("rule"),
        "case \"the replayed token\": the replay left the first configuration as it was, got \
         {fetched}"
    );
}

/// An id naming no configuration this account holds is the modelled 404, for ids that differ
/// only in which part of a well-formed id they carry.
#[tokio::test]
async fn an_id_naming_no_config_is_not_found() {
    let router = router();
    let (status, created) = create_config(
        &router,
        &json!({
            "onlineEvaluationConfigName": "heldConfig",
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

    // (case, the id the read names) — each inside `OnlineEvaluationConfigId`'s own format.
    let rows: [(&str, String); 3] = [
        (
            "a name this account never created",
            "otherConfig-abcdefghij".to_string(),
        ),
        (
            "the held configuration's name with another suffix",
            format!("heldConfig-{}", "z".repeat(10)),
        ),
        (
            "an id at the member's minimum length",
            "a-bcdefghijk".to_string(),
        ),
    ];

    for (case, id) in rows {
        let (status, refused) = get_config(&router, &id).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?} ({id}): expected HTTP 404 ResourceNotFoundException, got {status} {} \
             ({refused})",
            error_code(&refused)
        );
        assert_eq!(
            text(&refused, "message"),
            format!("Online evaluation configuration with ID {id} does not exist"),
            "case {case:?}: got {refused}"
        );
    }

    let (status, fetched) = get_config(&router, &held).await;
    assert_eq!(
        status, 200,
        "case \"the held configuration\": the id the create minted is still served, got {status} \
         ({fetched})"
    );
}

/// The clock-owed transition: a configuration read straight after the create is still
/// `CREATING`, and reports `ACTIVE` once the span the create owes has passed. The test waits
/// for the CONDITION, polling the read until it holds.
#[tokio::test]
async fn a_created_config_settles_from_creating_to_active() {
    let router = router();
    let (status, created) = create_config(
        &router,
        &json!({
            "onlineEvaluationConfigName": "settlingConfig",
            "rule": {"samplingConfig": {"samplingPercentage": 50.0}},
            "dataSourceConfig": cloud_watch_logs(json!({
                "serviceNames": ["agentservice"],
                "logGroupNames": ["/aws/agentcore/settling"]
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
    assert_eq!(
        text(&created, "status"),
        "CREATING",
        "case \"straight after the create\": a new configuration enters CREATING, got {created}"
    );

    let (_, fetched) = get_config(&router, &id).await;
    // The read is served from the one record, so it reports either side of the transition the
    // clock owes and nothing else; which side it is depends on how long this process took to
    // get here, so the transition itself is asserted by polling for it below.
    let read_status = text(&fetched, "status");
    assert!(
        read_status == "CREATING" || read_status == "ACTIVE",
        "case \"a caller acting straight after the create\": the read reports the status the \
         record carries, got {read_status:?} ({fetched})"
    );

    let started = Instant::now();
    let mut last = fetched;
    while started.elapsed() < SETTLE_DEADLINE && text(&last, "status") != "ACTIVE" {
        tokio::time::sleep(SETTLE_POLL).await;
        let (_, fresh) = get_config(&router, &id).await;
        last = fresh;
    }
    assert_eq!(
        text(&last, "status"),
        "ACTIVE",
        "case \"past the settle span\": the CREATING -> ACTIVE transition is due, got {last}"
    );
    // The transition is the status's alone: the execution status the create was asked for is
    // unchanged by it.
    assert_eq!(
        text(&last, "executionStatus"),
        "ENABLED",
        "case \"past the settle span\": the execution status is still the one the create asked \
         for, got {last}"
    );
}
