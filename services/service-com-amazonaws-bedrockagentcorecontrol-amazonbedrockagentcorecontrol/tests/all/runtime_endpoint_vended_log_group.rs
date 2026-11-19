// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch Logs group this service vends for an agent runtime ENDPOINT, and what a
//! delete does to a vended group's streams and events.
//!
//! The rule under test: an invocation is addressed to an endpoint, so the service vends one
//! managed log group per endpoint whose invocations can be read — named from the runtime's
//! own `agentRuntimeId` and the endpoint's name, under the runtimes prefix — with NO
//! retention configured, so its events never expire; and the group belongs to the ACCOUNT,
//! so no delete in this service removes it, its streams or its events.
//! `CreateAgentRuntime` establishes the default endpoint and vends that endpoint's group
//! (pinned in `runtime_vended_log_group.rs`); every endpoint a caller adds afterwards is
//! vended its own.
//!
//! Nothing in `CreateAgentRuntimeEndpoint`'s modelled output names the group
//! (`smithy_query(query="operation", operation="CreateAgentRuntimeEndpoint")` reports
//! `agentRuntimeArn`, `agentRuntimeEndpointArn`, `agentRuntimeId`, `createdAt`,
//! `endpointName`, `status` and `targetVersion`), which is why the name has to be derivable:
//! the client rebuilds it from the runtime's id and the endpoint's name.
//!
//! What the suites below do NOT claim: that an invocation writes those events. Nothing here
//! invokes a runtime — `InvokeAgentRuntime` is the data plane's own operation and is its own
//! open stub issue. The events in the retention suite are written through CloudWatch Logs'
//! own front door, which is where they come from whoever delivers a runtime's logs; what the
//! suite pins is the part this service owns, that a delete here removes none of it.
//!
//! The rows span what the rule generalises over: the endpoint's name (the shortest and the
//! longest `EndpointName` admits, and the other characters its pattern admits), how many
//! endpoints one runtime holds, the Region the request is signed for, and how many streams
//! and events a group holds when the resources are deleted.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol as agentcore;

/// The Region the rows are signed for unless one names another.
const REGION: &str = "us-east-1";

/// The prefix CloudWatch Logs holds an AgentCore Runtime's vended logs under, and the name
/// of the endpoint a runtime's own create establishes. Spelled out here rather than read off
/// the implementation, so a drift in either is a failing test.
const RUNTIME_LOG_GROUP_PREFIX: &str = "/aws/bedrock-agentcore/runtimes/";
const DEFAULT_ENDPOINT: &str = "DEFAULT";

/// The longest name `EndpointName` admits: its pattern is `^[a-zA-Z][a-zA-Z0-9_]{0,47}$`
/// (`smithy_query(query="operation", operation="CreateAgentRuntimeEndpoint")`), so 48
/// characters.
const MAX_ENDPOINT_NAME_LEN: usize = 48;

/// The bounds `LogGroupName` places on the name a group is created under: length 1..512,
/// pattern `^[\.\-_/#A-Za-z0-9]+$`. Source: `smithy_query(query="shape",
/// shape="LogGroupName",
/// model_path="services/service-com-amazonaws-cloudwatchlogs-logs-20140328/smithy-model/\
/// com-amazonaws-cloudwatchlogs-logs-20140328.json")`.
const MAX_LOG_GROUP_NAME_LEN: usize = 512;

/// A `clientToken` within `ClientToken`'s modelled 33..256 length and its
/// `^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}$` pattern.
const CLIENT_TOKEN: &str = "vended-endpoint-group-create-token01";

/// The execution role every row names. No IAM is mounted, so nothing is known about the role
/// and the role gate refuses nothing: what each row turns on is the log group.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/AgentRuntimeRole";

/// The emulator the rows run against: the control plane, and CloudWatch Logs beside it on
/// the one shared engine — what the assembled emulator is (`core/router`'s `build_router`
/// registers every service crate).
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        // `default()` registers no authenticator, so the suite exercises routing and the
        // handlers rather than this process's SigV4 keys.
        let mut router = MultiServiceRouter::default();
        router.register_service(agentcore::EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        Self { router }
    }

    /// One `restJson1` request to the control plane, signed for `region` — which is what the
    /// router routes on and what the handlers read the Region from.
    async fn control(&self, method: &str, uri: &str, region: &str, body: Option<&Value>) -> Answer {
        let payload = body.map_or_else(Bytes::new, |value| {
            Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
        });
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240101/{region}/bedrock-agentcore/\
             aws4_request, SignedHeaders=host, Signature=abc"
        );
        let mut request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", authorization)
            .header("host", "bedrock-agentcore-control.amazonaws.com");
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        let request = request.body(payload).expect("a well-formed request");
        Answer::of(self.router.dispatch(request).await)
    }

    /// `PUT /runtimes/` — the route the model binds `CreateAgentRuntime` to.
    async fn create_runtime(&self, region: &str, name: &str) -> Answer {
        let body = json!({
            "agentRuntimeName": name,
            "agentRuntimeArtifact": {"containerConfiguration": {
                "containerUri": "public.ecr.aws/agentcore/demo:v1"
            }},
            "roleArn": ROLE_ARN,
            "networkConfiguration": {"networkMode": "PUBLIC"},
            "protocolConfiguration": {"serverProtocol": "HTTP"},
        });
        self.control("PUT", "/runtimes/", region, Some(&body)).await
    }

    /// `PUT /runtimes/{agentRuntimeId}/runtime-endpoints/` — the route the model binds
    /// `CreateAgentRuntimeEndpoint` to.
    async fn create_endpoint(&self, region: &str, runtime_id: &str, body: &Value) -> Answer {
        self.control(
            "PUT",
            &format!("/runtimes/{runtime_id}/runtime-endpoints/"),
            region,
            Some(body),
        )
        .await
    }

    async fn get_endpoint(&self, region: &str, runtime_id: &str, name: &str) -> Answer {
        self.control(
            "GET",
            &format!("/runtimes/{runtime_id}/runtime-endpoints/{name}/"),
            region,
            None,
        )
        .await
    }

    async fn get_runtime(&self, region: &str, runtime_id: &str) -> Answer {
        self.control("GET", &format!("/runtimes/{runtime_id}/"), region, None)
            .await
    }

    async fn delete_runtime(&self, region: &str, runtime_id: &str) -> Answer {
        self.control("DELETE", &format!("/runtimes/{runtime_id}/"), region, None)
            .await
    }

    /// Read the runtime until it is GONE — the modelled 404 — or fail once the generous
    /// deadline passes.
    ///
    /// `DeleteAgentRuntime` ACCEPTS the removal (202) and the runtime reports `DELETING`
    /// until the span the clock owes it passes, so this looks again for the CONDITION
    /// rather than waiting a fixed time: nothing here depends on how fast the host runs.
    async fn get_runtime_until_gone(&self, case: &str, region: &str, runtime_id: &str) -> Answer {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let answer = self.get_runtime(region, runtime_id).await;
            if answer.status == 404 {
                return answer;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "case {case:?}: the deleted runtime never went away, last answer {} and body {}",
                answer.status,
                answer.body
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    /// `DELETE /runtimes/{agentRuntimeId}/runtime-endpoints/{endpointName}/` — the route
    /// the model binds `DeleteAgentRuntimeEndpoint` to.
    async fn delete_endpoint(&self, region: &str, runtime_id: &str, name: &str) -> Answer {
        self.control(
            "DELETE",
            &format!("/runtimes/{runtime_id}/runtime-endpoints/{name}/"),
            region,
            None,
        )
        .await
    }

    /// One CloudWatch Logs call (`awsJson1_1`, `Logs_20140328.<Operation>`) signed for
    /// `region`. The credential scope names `logs`, which is what the router claims on.
    async fn logs(&self, region: &str, operation: &str, body: &Value) -> Answer {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240101/{region}/logs/aws4_request, \
             SignedHeaders=host, Signature=abc"
        );
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://logs.{region}.amazonaws.com/"))
            .header("authorization", authorization)
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("Logs_20140328.{operation}"))
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        Answer::of(self.router.dispatch(request).await)
    }

    /// Every log group CloudWatch Logs reports under `prefix` in `region`, as the client
    /// reads them.
    async fn log_groups(&self, case: &str, region: &str, prefix: &str) -> Vec<Value> {
        let answer = self
            .logs(
                region,
                "DescribeLogGroups",
                &json!({"logGroupNamePrefix": prefix}),
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200 from DescribeLogGroups, got {} and body {}",
            answer.status, answer.body
        );
        answer.body["logGroups"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// The `logGroupName` of every group reported under `prefix`, in the order reported.
    async fn log_group_names(&self, case: &str, region: &str, prefix: &str) -> Vec<String> {
        self.log_groups(case, region, prefix)
            .await
            .iter()
            .filter_map(|group| group["logGroupName"].as_str().map(str::to_string))
            .collect()
    }

    /// The `logStreamName` of every stream reported in `group`.
    async fn log_stream_names(&self, case: &str, region: &str, group: &str) -> Vec<String> {
        let answer = self
            .logs(
                region,
                "DescribeLogStreams",
                &json!({"logGroupName": group}),
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200 from DescribeLogStreams for {group:?}, got {} and \
             body {}",
            answer.status, answer.body
        );
        answer.body["logStreams"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|stream| stream["logStreamName"].as_str().map(str::to_string))
            .collect()
    }

    /// Write `messages` into `stream` of `group`, creating the stream first — what whoever
    /// delivers a runtime's logs does through this same front door.
    async fn write_events(
        &self,
        case: &str,
        region: &str,
        group: &str,
        stream: &str,
        messages: &[String],
    ) {
        let created = self
            .logs(
                region,
                "CreateLogStream",
                &json!({"logGroupName": group, "logStreamName": stream}),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected HTTP 200 from CreateLogStream in {group:?}, got {} and body \
             {}",
            created.status, created.body
        );
        let now_ms = i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("the clock is after the epoch")
                .as_millis(),
        )
        .expect("the epoch milliseconds fit an i64");
        let events: Vec<Value> = messages
            .iter()
            .enumerate()
            .map(|(index, message)| {
                json!({"timestamp": now_ms + i64::try_from(index).unwrap_or_default(),
                       "message": message})
            })
            .collect();
        let put = self
            .logs(
                region,
                "PutLogEvents",
                &json!({"logGroupName": group, "logStreamName": stream, "logEvents": events}),
            )
            .await;
        assert_eq!(
            put.status, 200,
            "case {case:?}: expected HTTP 200 from PutLogEvents into {group:?}, got {} and body {}",
            put.status, put.body
        );
    }

    /// The messages `FilterLogEvents` reports in `group` for `pattern`, each paired with the
    /// stream it was read from.
    async fn filtered_events(
        &self,
        case: &str,
        region: &str,
        group: &str,
        pattern: &str,
    ) -> Vec<(String, String)> {
        let answer = self
            .logs(
                region,
                "FilterLogEvents",
                &json!({"logGroupName": group, "filterPattern": pattern}),
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200 from FilterLogEvents in {group:?}, got {} and body \
             {}",
            answer.status, answer.body
        );
        answer.body["events"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|event| {
                (
                    event["logStreamName"].as_str().unwrap_or_default().to_string(),
                    event["message"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect()
    }
}

/// One answer from the emulator: its status and its JSON body.
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    fn of(response: http::Response<Bytes>) -> Self {
        let status = response.status().as_u16();
        let body = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        Self { status, body }
    }

    /// The error code `restJson1` and `awsJson1_1` both carry in `__type`.
    fn error_code(&self) -> String {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or("<no __type>")
            .rsplit('#')
            .next()
            .unwrap_or_default()
            .to_string()
    }

    fn runtime_id(&self, case: &str) -> String {
        self.body["agentRuntimeId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: a served create reports an agentRuntimeId, got body {}",
                    self.body
                )
            })
            .to_string()
    }
}

/// One endpoint a row adds: what it is called and where its runtime lives.
#[derive(Debug, Clone)]
struct Row {
    case: &'static str,
    runtime_name: &'static str,
    endpoint_name: String,
    region: &'static str,
}

/// The endpoints the rule is asserted over. One row per thing it generalises over: the
/// shortest name `EndpointName` admits, a name carrying the digits and underscores its
/// pattern admits, the longest name it admits, and a Region that is not this emulator's
/// default.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the shortest endpoint name the model admits",
            runtime_name: "short_endpoint_agent",
            endpoint_name: "a".to_string(),
            region: REGION,
        },
        Row {
            case: "the digits and underscores the endpoint name pattern admits",
            runtime_name: "pattern_endpoint_agent",
            endpoint_name: "prod_v2_2026".to_string(),
            region: REGION,
        },
        Row {
            case: "the longest endpoint name the model admits",
            runtime_name: "long_endpoint_agent",
            endpoint_name: format!("l{}", "o".repeat(MAX_ENDPOINT_NAME_LEN - 1)),
            region: REGION,
        },
        Row {
            case: "a Region that is not the emulator's default",
            runtime_name: "west_endpoint_agent",
            endpoint_name: "canary".to_string(),
            region: "eu-west-2",
        },
    ]
}

/// The group a client derives for the `endpoint_name` endpoint of `agent_runtime_id`.
fn vended_group(agent_runtime_id: &str, endpoint_name: &str) -> String {
    format!("{RUNTIME_LOG_GROUP_PREFIX}{agent_runtime_id}-{endpoint_name}")
}

/// Assert `name` is a name CloudWatch Logs admits for a group.
fn assert_log_group_name_is_admissible(case: &str, name: &str) {
    assert!(
        (1..=MAX_LOG_GROUP_NAME_LEN).contains(&name.chars().count()),
        "case {case:?}: expected a name inside LogGroupName's 1..{MAX_LOG_GROUP_NAME_LEN} length, \
         got {} characters ({name:?})",
        name.chars().count()
    );
    assert!(
        name.chars().all(|character| character.is_ascii_alphanumeric()
            || matches!(character, '.' | '-' | '_' | '/' | '#')),
        "case {case:?}: every character of a log group's name must satisfy LogGroupName's \
         pattern, got {name:?}"
    );
}

/// Assert exactly one group exists under the name a client derives for the `endpoint` of
/// `runtime_id`, that it is that name, and that it carries NO `retentionInDays` — the group
/// never expires.
async fn assert_vended_group(
    emulator: &Emulator,
    case: &str,
    region: &str,
    runtime_id: &str,
    endpoint: &str,
) {
    let expected = vended_group(runtime_id, endpoint);
    assert_log_group_name_is_admissible(case, &expected);
    let groups = emulator.log_groups(case, region, &expected).await;
    assert_eq!(
        groups.len(),
        1,
        "case {case:?}: expected exactly the endpoint's vended group {expected:?} under that \
         prefix, got {groups:?}"
    );
    assert_eq!(
        groups[0]["logGroupName"],
        json!(expected),
        "case {case:?}: expected the vended group {expected:?}, got {}",
        groups[0]
    );
    assert!(
        groups[0].get("retentionInDays").is_none(),
        "case {case:?}: a vended log group carries no retention, so its events never expire, got \
         {}",
        groups[0]
    );
}

/// Create a runtime and report the id it minted.
async fn created_runtime(emulator: &Emulator, case: &str, region: &str, name: &str) -> String {
    let created = emulator.create_runtime(region, name).await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: expected HTTP 202 from CreateAgentRuntime, got {} and body {}",
        created.status, created.body
    );
    created.runtime_id(case)
}

/// A served `CreateAgentRuntimeEndpoint` vends that endpoint's log group: CloudWatch Logs
/// reports exactly the group a client derives from the runtime's id and the endpoint's name,
/// with no retention, beside the group the runtime's own create vended for its default
/// endpoint.
#[tokio::test]
async fn a_created_endpoint_has_its_own_vended_log_group() {
    for row in rows() {
        let case = row.case;
        let emulator = Emulator::new();
        let runtime_id = created_runtime(&emulator, case, row.region, row.runtime_name).await;
        // The runtime's create vended the default endpoint's group, and nothing else.
        let before = emulator
            .log_group_names(case, row.region, RUNTIME_LOG_GROUP_PREFIX)
            .await;
        assert_eq!(
            before,
            vec![vended_group(&runtime_id, DEFAULT_ENDPOINT)],
            "case {case:?}: only the default endpoint's group exists before the endpoint is \
             added, got {before:?}"
        );

        let created = emulator
            .create_endpoint(
                row.region,
                &runtime_id,
                &json!({"name": row.endpoint_name, "description": "the endpoint under test"}),
            )
            .await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: expected HTTP 202 from CreateAgentRuntimeEndpoint, got {} and body {}",
            created.status, created.body
        );
        assert_eq!(
            created.body["endpointName"],
            json!(row.endpoint_name),
            "case {case:?}: the create reports the endpoint it added, got body {}",
            created.body
        );

        assert_vended_group(&emulator, case, row.region, &runtime_id, &row.endpoint_name).await;

        // The round trip: the endpoint the group was vended for is the one the control
        // plane reports, under the very name the group's name is derived from.
        let read = emulator
            .get_endpoint(row.region, &runtime_id, &row.endpoint_name)
            .await;
        assert_eq!(
            (read.status, read.body["name"].clone()),
            (200, json!(row.endpoint_name)),
            "case {case:?}: GetAgentRuntimeEndpoint must report the created endpoint, got {} and \
             body {}",
            read.status,
            read.body
        );

        // One group per endpoint whose logs can be read: the default endpoint's and this
        // one's, and nothing else.
        let mut want = vec![
            vended_group(&runtime_id, DEFAULT_ENDPOINT),
            vended_group(&runtime_id, &row.endpoint_name),
        ];
        want.sort();
        let groups = emulator
            .log_group_names(case, row.region, RUNTIME_LOG_GROUP_PREFIX)
            .await;
        assert_eq!(
            groups, want,
            "case {case:?}: expected the default endpoint's group and the added endpoint's, got \
             {groups:?}"
        );
    }
}

/// Every endpoint of one runtime is vended its OWN group, named for its own name — including
/// two endpoints whose names are a prefix of one another.
#[tokio::test]
async fn every_endpoint_of_a_runtime_is_vended_its_own_group() {
    let case = "several endpoints on one runtime";
    let emulator = Emulator::new();
    let runtime_id = created_runtime(&emulator, case, REGION, "many_endpoint_agent").await;
    let mut want = vec![vended_group(&runtime_id, DEFAULT_ENDPOINT)];

    // `prod` and `prod2`: the group is named for the whole endpoint name, so two names one
    // of which is a prefix of the other cannot collide.
    for endpoint in ["prod", "prod2", "staging_2"] {
        let created = emulator
            .create_endpoint(REGION, &runtime_id, &json!({"name": endpoint}))
            .await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: expected HTTP 202 from CreateAgentRuntimeEndpoint for {endpoint:?}, \
             got {} and body {}",
            created.status, created.body
        );
        assert_vended_group(&emulator, case, REGION, &runtime_id, endpoint).await;
        want.push(vended_group(&runtime_id, endpoint));
    }

    want.sort();
    let groups = emulator
        .log_group_names(case, REGION, RUNTIME_LOG_GROUP_PREFIX)
        .await;
    assert_eq!(
        groups, want,
        "case {case:?}: expected one group per endpoint of the runtime, got {groups:?}"
    );
}

/// A create that adds no endpoint vends no group: a replayed `clientToken` answers with the
/// endpoint it already added, a name the runtime already holds is refused, and a runtime this
/// account does not hold is not found — none of the three adds a second group.
#[tokio::test]
async fn a_create_that_adds_no_endpoint_vends_no_group() {
    let case = "a replayed token, a name conflict and an unknown runtime";
    let emulator = Emulator::new();
    let runtime_id = created_runtime(&emulator, case, REGION, "replayed_endpoint_agent").await;
    let request = json!({"name": "prod", "clientToken": CLIENT_TOKEN});

    let first = emulator
        .create_endpoint(REGION, &runtime_id, &request)
        .await;
    assert_eq!(
        first.status, 202,
        "case {case:?}: expected HTTP 202 from CreateAgentRuntimeEndpoint, got {} and body {}",
        first.status, first.body
    );

    // The replay answers with the endpoint the token already added...
    let replayed = emulator
        .create_endpoint(REGION, &runtime_id, &request)
        .await;
    assert_eq!(
        (replayed.status, replayed.body["endpointName"].clone()),
        (202, json!("prod")),
        "case {case:?}: a replayed clientToken answers with the endpoint it first added, got {} \
         and body {}",
        replayed.status,
        replayed.body
    );

    // ...a name the runtime already holds an endpoint under is a conflict...
    let conflicting = emulator
        .create_endpoint(REGION, &runtime_id, &json!({"name": "prod"}))
        .await;
    assert_eq!(
        (conflicting.status, conflicting.error_code()),
        (409, "ConflictException".to_string()),
        "case {case:?}: a repeated endpoint name is a conflict, got {} and body {}",
        conflicting.status,
        conflicting.body
    );

    // ...and a runtime this account does not hold is not found. Its id satisfies
    // `AgentRuntimeId`'s pattern, so what refuses it is the lookup and not validation.
    let absent = emulator
        .create_endpoint(REGION, "notThere-0123456789", &json!({"name": "prod"}))
        .await;
    assert_eq!(
        (absent.status, absent.error_code()),
        (404, "ResourceNotFoundException".to_string()),
        "case {case:?}: an endpoint on an unknown runtime is not found, got {} and body {}",
        absent.status,
        absent.body
    );

    // Through all of it: the default endpoint's group and the one endpoint that was added.
    let mut want = vec![
        vended_group(&runtime_id, DEFAULT_ENDPOINT),
        vended_group(&runtime_id, "prod"),
    ];
    want.sort();
    let groups = emulator
        .log_group_names(case, REGION, RUNTIME_LOG_GROUP_PREFIX)
        .await;
    assert_eq!(
        groups, want,
        "case {case:?}: only an endpoint that was really added may have a group, got {groups:?}"
    );
}

/// How many streams and events a vended group holds when the resources are deleted — one
/// stream with one event, and several streams with several events each.
#[derive(Debug, Clone, Copy)]
struct Delivery {
    case: &'static str,
    streams: usize,
    events_per_stream: usize,
}

/// `DeleteAgentRuntime` does not cascade into CloudWatch Logs: the runtime and its endpoints
/// go, and the groups vended for them — with every stream and every event they hold, and
/// still no retention — stay. The logs of a runtime that is gone are what an operator
/// diagnosing its last session reads.
#[tokio::test]
async fn deleting_the_runtime_retains_the_vended_groups_and_their_events() {
    for delivery in [
        Delivery {
            case: "one stream of one event",
            streams: 1,
            events_per_stream: 1,
        },
        Delivery {
            case: "several streams of several events",
            streams: 3,
            events_per_stream: 4,
        },
    ] {
        let case = delivery.case;
        let emulator = Emulator::new();
        let runtime_id = created_runtime(&emulator, case, REGION, "retained_logs_agent").await;
        let endpoint = "prod";
        let created = emulator
            .create_endpoint(REGION, &runtime_id, &json!({"name": endpoint}))
            .await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: expected HTTP 202 from CreateAgentRuntimeEndpoint, got {} and body {}",
            created.status, created.body
        );

        // Fill both vended groups the way whoever delivers a runtime's logs does: a stream
        // per session, each carrying that session's lines. The stream names are inside
        // `LogStreamName`'s length and carry none of the characters it excludes.
        let mut written: Vec<(String, String, String)> = Vec::new();
        for group_endpoint in [DEFAULT_ENDPOINT, endpoint] {
            let group = vended_group(&runtime_id, group_endpoint);
            for stream_index in 0..delivery.streams {
                let stream = format!("session_{group_endpoint}_{stream_index}");
                let messages: Vec<String> = (0..delivery.events_per_stream)
                    .map(|event_index| {
                        format!("agent line marker_{group_endpoint}_{stream_index}_{event_index}")
                    })
                    .collect();
                emulator
                    .write_events(case, REGION, &group, &stream, &messages)
                    .await;
                for message in messages {
                    written.push((group.clone(), stream.clone(), message));
                }
            }
        }

        // A runtime is not destroyed underneath a live endpoint, so the endpoint the
        // caller created is torn down first and the runtime goes after it; the endpoint
        // the service manages for the runtime goes with the runtime.
        let deleted_endpoint = emulator.delete_endpoint(REGION, &runtime_id, endpoint).await;
        assert_eq!(
            deleted_endpoint.status, 202,
            "case {case:?}: expected HTTP 202 from DeleteAgentRuntimeEndpoint, got {} and body {}",
            deleted_endpoint.status, deleted_endpoint.body
        );
        let deleted_runtime = emulator.delete_runtime(REGION, &runtime_id).await;
        assert_eq!(
            deleted_runtime.status, 202,
            "case {case:?}: expected HTTP 202 from DeleteAgentRuntime, got {} and body {}",
            deleted_runtime.status, deleted_runtime.body
        );
        let read = emulator
            .get_runtime_until_gone(case, REGION, &runtime_id)
            .await;
        assert_eq!(
            (read.status, read.error_code()),
            (404, "ResourceNotFoundException".to_string()),
            "case {case:?}: a deleted runtime is not found, got {} and body {}",
            read.status,
            read.body
        );

        // Both groups are still there, still without retention...
        for group_endpoint in [DEFAULT_ENDPOINT, endpoint] {
            assert_vended_group(&emulator, case, REGION, &runtime_id, group_endpoint).await;
        }
        // ...with every stream that was written to...
        for group_endpoint in [DEFAULT_ENDPOINT, endpoint] {
            let group = vended_group(&runtime_id, group_endpoint);
            let mut want: Vec<String> = (0..delivery.streams)
                .map(|stream_index| format!("session_{group_endpoint}_{stream_index}"))
                .collect();
            want.sort();
            let streams = emulator.log_stream_names(case, REGION, &group).await;
            assert_eq!(
                streams, want,
                "case {case:?}: the streams of the vended group {group:?} outlive the runtime, \
                 got {streams:?}"
            );
        }
        // ...and every event still readable, in the stream it was written to. The pattern is
        // the marker as a quoted term, the unstructured form a client tailing the group uses.
        for (group, stream, message) in written {
            let marker = message
                .rsplit(' ')
                .next()
                .expect("the message carries a marker")
                .to_string();
            let events = emulator
                .filtered_events(case, REGION, &group, &format!("\"{marker}\""))
                .await;
            assert_eq!(
                events,
                vec![(stream.clone(), message.clone())],
                "case {case:?}: the event {message:?} of the vended group {group:?} outlives the \
                 runtime, got {events:?}"
            );
        }
    }
}
