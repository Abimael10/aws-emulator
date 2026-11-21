// SPDX-License-Identifier: CC0-1.0

//! What the AgentCore built-in tools publish about the sessions a caller holds open,
//! driven end to end with the AgentCore control plane, its data plane and Amazon
//! CloudWatch on one engine.
//!
//! RULE under test: THE NUMBER OF SESSIONS OF ONE BUILT-IN TOOL A CALLER CURRENTLY HOLDS
//! OPEN IS PUBLISHED AS THE `AWS/Bedrock-AgentCore` `ActiveSessionCount` GAUGE,
//! DIMENSIONED ON `Service` BY THE TOOL THOSE SESSIONS BELONG TO — it rises as sessions
//! are started, falls back as each one is stopped or its time-to-live runs out, and a tool
//! the account has started no session on has no series at all. So a client that watches
//! concurrency, or alarms on it, gets an answer from the sessions it really holds instead
//! of reading a flat zero for ever.
//!
//! The dimension values are the two built-in tools this data plane serves sessions for, as
//! AgentCore names them in that namespace: `AgentCore.CodeInterpreter` and
//! `AgentCore.Browser` (the metric is documented for the built-in tools, not modelled in
//! Smithy, so the two names are stated here beside the rows that read them).
//!
//! This suite lives here rather than in either service's own crate because three services
//! are involved: a browser and a code interpreter are the CONTROL plane's resources, the
//! sessions are the DATA plane's, and the series they publish lives in CloudWatch's own
//! metric store — the one `ListMetrics` and `GetMetricStatistics` read.
//!
//! The rows span what the rule generalises over: which built-in tool holds the sessions,
//! how many are open at once, whether they are spread over one resource of that tool or
//! several, and how each one ends — an explicit stop, or a time-to-live that runs out with
//! nobody calling.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_cloudwatch_graniteserviceversion20100801 as cloudwatch;
use smithy_server_runtime::MultiServiceRouter;

// The AgentCore caller, its label escaping and the Region it signs for, from the sibling
// cross-plane suite that owns them: `tests/all/main.rs` is one crate root, so this suite
// borrows that wiring rather than restating it, and the two cannot drift in how they address
// either plane.
use crate::bedrock_agentcore_data_plane_parent_resources::{call, label, REGION};

/// The namespace Amazon Bedrock AgentCore publishes its built-in tools' metrics in, as a
/// client names it.
const NAMESPACE: &str = "AWS/Bedrock-AgentCore";

/// The metric this suite reads: how many sessions of a tool are open.
const ACTIVE_SESSION_COUNT: &str = "ActiveSessionCount";

/// The dimension that names the built-in tool a series belongs to.
const SERVICE_DIMENSION: &str = "Service";

/// The unit a count of things carries (`StandardUnit` holds `Count`).
const COUNT_UNIT: &str = "Count";

/// The period a read lays over the series, in seconds: the minute a regular-resolution
/// metric is stored at (`MetricDatum$StorageResolution`).
const PERIOD_SECONDS: i64 = 60;

/// How long a case looks for a reading, far longer than the spans involved — so a loaded
/// machine fails nothing, while a gauge that never moves still ends the test instead of
/// hanging.
const READ_DEADLINE: Duration = Duration::from_secs(240);

/// How often it looks again, as a client's own waiter does.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The `X-Amz-Target` prefix CloudWatch's JSON wire carries.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";

/// CloudWatch's SigV4 signing name, which is what routes a call to it.
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// One built-in tool of AgentCore: the resource the control plane creates, the session
/// routes the data plane serves, and the `Service` dimension value its gauge is published
/// under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    CodeInterpreter,
    Browser,
}

impl Tool {
    /// Both tools, so a case can assert that a series exists for the one in use and for no
    /// other.
    const ALL: [Self; 2] = [Self::CodeInterpreter, Self::Browser];

    /// The `Service` dimension value AgentCore publishes this tool's sessions under.
    fn service(self) -> &'static str {
        match self {
            Self::CodeInterpreter => "AgentCore.CodeInterpreter",
            Self::Browser => "AgentCore.Browser",
        }
    }

    /// The first URI segment both planes bind this tool's routes under.
    fn segment(self) -> &'static str {
        match self {
            Self::CodeInterpreter => "code-interpreters",
            Self::Browser => "browsers",
        }
    }

    /// The member the control plane's create answers this tool's minted id in.
    fn created_id_member(self) -> &'static str {
        match self {
            Self::CodeInterpreter => "codeInterpreterId",
            Self::Browser => "browserId",
        }
    }

    /// The `networkMode` the control plane's create takes for this tool: a code interpreter
    /// runs in a sandbox, a browser on the public network.
    fn network_mode(self) -> &'static str {
        match self {
            Self::CodeInterpreter => "SANDBOX",
            Self::Browser => "PUBLIC",
        }
    }
}

/// AgentCore Control, the AgentCore data plane and Amazon CloudWatch over ONE shared
/// engine, exactly as `router/src/lib.rs` mounts them.
///
/// Its own, rather than the sibling suite's, because this one needs CloudWatch mounted beside
/// the two AgentCore planes: the series the tools publish lives in CloudWatch's store.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signatures are not what this suite is about; the router still reads the SigV4
    // credential scope, which is where each call's service and Region come from.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_bedrockagentcore_amazonbedrockagentcore::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(cloudwatch::EmulatedService::new(engine));
    router
}

/// One CloudWatch call that has to be served, and the parsed body it answered.
async fn cloudwatch_ok(
    case: &str,
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("monitoring.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("{CLOUDWATCH_TARGET_PREFIX}.{operation}"),
        )
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{REGION}/\
                 {CLOUDWATCH_SIGNING_NAME}/aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let text = String::from_utf8_lossy(&payload).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch {operation}, got {status} with body \
         {text}"
    );
    serde_json::from_slice(&payload).unwrap_or(Value::Null)
}

/// Create one resource of `tool` through the CONTROL plane, which owns it, and return the
/// id it minted. `CreateCodeInterpreter` is `PUT /code-interpreters` and `CreateBrowser`
/// `PUT /browsers`, both answering 202.
async fn create_resource(case: &str, router: &MultiServiceRouter, tool: Tool, name: &str) -> String {
    let (status, created) = call(
        router,
        "PUT",
        &format!("/{}", tool.segment()),
        Some(json!({
            "name": name,
            "networkConfiguration": {"networkMode": tool.network_mode()},
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "case {case:?}: creating the {:?} {name} must answer 202, got {status} with {created:?}",
        tool
    );
    created[tool.created_id_member()]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the create reported no id: {created:?}"))
        .to_string()
}

/// Start one session of `tool` on `resource`, with the time-to-live the case asked for, and
/// return the `sessionId` the data plane minted.
async fn start_session(
    case: &str,
    router: &MultiServiceRouter,
    tool: Tool,
    resource: &str,
    timeout_seconds: i64,
) -> String {
    let (status, started) = call(
        router,
        "PUT",
        &format!("/{}/{}/sessions/start", tool.segment(), label(resource)),
        Some(json!({"sessionTimeoutSeconds": timeout_seconds})),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: starting a {tool:?} session on {resource} must answer 200, got {status} \
         with {started:?}"
    );
    started["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the start reported no sessionId: {started:?}"))
        .to_string()
}

/// Stop one session of `tool`, as a client that is done with it does.
async fn stop_session(
    case: &str,
    router: &MultiServiceRouter,
    tool: Tool,
    resource: &str,
    session: &str,
) {
    let (status, stopped) = call(
        router,
        "PUT",
        &format!(
            "/{}/{}/sessions/stop?sessionId={}",
            tool.segment(),
            label(resource),
            label(session)
        ),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: stopping the {tool:?} session {session} must answer 200, got {status} \
         with {stopped:?}"
    );
}

/// Now, as the epoch seconds a CloudWatch read windows by.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// What a read of one tool's gauge says: the highest and the lowest reading the series
/// holds, and the unit its datapoints carry.
#[derive(Debug, Clone, Default)]
struct Gauge {
    /// The greatest `Maximum` of any datapoint, or `None` for a series with no datapoint.
    peak: Option<f64>,
    /// The smallest `Minimum` of any datapoint, or `None` for a series with no datapoint.
    trough: Option<f64>,
    /// Every unit the datapoints were published in.
    units: BTreeSet<String>,
}

/// Read the `ActiveSessionCount` series of one built-in tool exactly as a client reads one:
/// `GetMetricStatistics` over a window around now, at the minute a regular-resolution
/// metric is stored at.
async fn gauge(case: &str, router: &MultiServiceRouter, service: &str) -> Gauge {
    let now = now_secs();
    let answered = cloudwatch_ok(
        case,
        router,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": ACTIVE_SESSION_COUNT,
            "Dimensions": [{"Name": SERVICE_DIMENSION, "Value": service}],
            // An hour back and a minute on, so every reading taken by now is inside the
            // read whatever minute it was taken in.
            "StartTime": now - 3_600.0,
            "EndTime": now + 60.0,
            "Period": PERIOD_SECONDS,
            "Statistics": ["Maximum", "Minimum"],
        }),
    )
    .await;
    let mut gauge = Gauge::default();
    for point in answered["Datapoints"].as_array().cloned().unwrap_or_default() {
        if let Some(maximum) = point["Maximum"].as_f64() {
            gauge.peak = Some(gauge.peak.map_or(maximum, |held: f64| held.max(maximum)));
        }
        if let Some(minimum) = point["Minimum"].as_f64() {
            gauge.trough = Some(gauge.trough.map_or(minimum, |held: f64| held.min(minimum)));
        }
        if let Some(unit) = point["Unit"].as_str() {
            gauge.units.insert(unit.to_string());
        }
    }
    gauge
}

/// Every `Service` value CloudWatch lists an `ActiveSessionCount` series for, across every
/// page, read exactly as a client reads it.
async fn listed_services(case: &str, router: &MultiServiceRouter) -> BTreeSet<String> {
    let mut services: BTreeSet<String> = BTreeSet::new();
    let mut next_token: Option<String> = None;
    loop {
        let mut request = json!({"Namespace": NAMESPACE, "MetricName": ACTIVE_SESSION_COUNT});
        if let Some(token) = &next_token {
            request["NextToken"] = json!(token);
        }
        let answered = cloudwatch_ok(case, router, "ListMetrics", &request).await;
        for metric in answered["Metrics"].as_array().cloned().unwrap_or_default() {
            for dimension in metric["Dimensions"].as_array().cloned().unwrap_or_default() {
                if dimension["Name"].as_str() == Some(SERVICE_DIMENSION) {
                    services.insert(
                        dimension["Value"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    );
                }
            }
        }
        next_token = answered["NextToken"].as_str().map(ToString::to_string);
        if next_token.is_none() {
            return services;
        }
    }
}

/// Look at `service`'s gauge until its highest reading reaches `want`, up to a deadline far
/// longer than the spans involved — the condition is waited FOR, never slept through.
async fn wait_for_peak(case: &str, router: &MultiServiceRouter, service: &str, want: f64) -> Gauge {
    let deadline = Instant::now() + READ_DEADLINE;
    loop {
        let read = gauge(case, router, service).await;
        if read.peak.is_some_and(|peak| peak >= want) {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {NAMESPACE}/{ACTIVE_SESSION_COUNT} for \
             {SERVICE_DIMENSION}={service} to reach {want}, it reads {:?}",
            read.peak
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look at `service`'s gauge until its lowest reading falls to `want`, the same way.
async fn wait_for_trough(
    case: &str,
    router: &MultiServiceRouter,
    service: &str,
    want: f64,
) -> Gauge {
    let deadline = Instant::now() + READ_DEADLINE;
    loop {
        let read = gauge(case, router, service).await;
        if read.trough.is_some_and(|trough| trough <= want) {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {NAMESPACE}/{ACTIVE_SESSION_COUNT} for \
             {SERVICE_DIMENSION}={service} to fall to {want}, it reads {:?}",
            read.trough
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the table: a built-in tool, and how many sessions each resource of it holds.
#[derive(Debug, Clone)]
struct Row {
    case: &'static str,
    tool: Tool,
    /// How many sessions each resource of that tool gets, one entry per resource — so a row
    /// can spread the sessions of ONE tool over several of its resources.
    sessions_per_resource: &'static [usize],
}

/// The time-to-live every row that ends its sessions by STOPPING them asks for: long enough
/// that nothing expires while the case runs, well inside the modelled maximum of 28800.
const LONG_SESSION_TIMEOUT_SECONDS: i64 = 28_800;

/// The gauge of each built-in tool rises to the sessions the caller holds open on it, falls
/// back to zero as they are stopped, and no series is minted for a tool that holds none.
#[tokio::test]
async fn active_session_count_reports_the_sessions_each_built_in_tool_holds_open() {
    let rows = [
        Row {
            case: "one code interpreter session",
            tool: Tool::CodeInterpreter,
            sessions_per_resource: &[1],
        },
        Row {
            case: "two code interpreter sessions on one interpreter",
            tool: Tool::CodeInterpreter,
            sessions_per_resource: &[2],
        },
        Row {
            case: "three code interpreter sessions over two interpreters",
            tool: Tool::CodeInterpreter,
            sessions_per_resource: &[2, 1],
        },
        Row {
            case: "one browser session",
            tool: Tool::Browser,
            sessions_per_resource: &[1],
        },
        Row {
            case: "two browser sessions on one browser",
            tool: Tool::Browser,
            sessions_per_resource: &[2],
        },
        Row {
            case: "three browser sessions over two browsers",
            tool: Tool::Browser,
            sessions_per_resource: &[1, 2],
        },
    ];

    for row in rows {
        let case = row.case;
        let router = emulator();
        let service = row.tool.service();
        let open: usize = row.sessions_per_resource.iter().sum();
        let expected = open as f64;

        // Each resource of the tool, created through the plane that owns it, with the
        // sessions this row opens on it.
        let mut started: Vec<(String, String)> = Vec::new();
        for (index, count) in row.sessions_per_resource.iter().enumerate() {
            let resource = create_resource(
                case,
                &router,
                row.tool,
                &format!("gauge_resource_{index}"),
            )
            .await;
            for _ in 0..*count {
                let session = start_session(
                    case,
                    &router,
                    row.tool,
                    &resource,
                    LONG_SESSION_TIMEOUT_SECONDS,
                )
                .await;
                started.push((resource.clone(), session));
            }
        }

        // A resource of every OTHER built-in tool exists too, with no session on it: the
        // gauge is about the sessions a caller holds open, not about the resources the
        // control plane holds, so an unused tool must stay unpublished even though its
        // resource is there.
        let unused: Vec<Tool> = Tool::ALL
            .into_iter()
            .filter(|tool| *tool != row.tool)
            .collect();
        for (index, idle) in unused.iter().enumerate() {
            create_resource(case, &router, *idle, &format!("idle_resource_{index}")).await;
        }

        // The gauge reports the concurrency the caller really holds — and NOT more than
        // it: nothing counts a session twice, and nothing invents one.
        let read = wait_for_peak(case, &router, service, expected).await;
        assert_eq!(
            read.peak,
            Some(expected),
            "case {case:?}: expected a peak of exactly {expected} open {service} sessions, got \
             {:?}",
            read.peak
        );
        assert_eq!(
            read.units,
            BTreeSet::from([COUNT_UNIT.to_string()]),
            "case {case:?}: expected the readings in {COUNT_UNIT}, got {:?}",
            read.units
        );

        // The series belongs to the tool in use and to no other: a tool this account has
        // started no session on is not published at all, even where its resource exists.
        let listed = listed_services(case, &router).await;
        assert!(
            listed.contains(service),
            "case {case:?}: expected {service} among the listed series, got {listed:?}"
        );
        for idle in &unused {
            let idle_service = idle.service();
            assert!(
                !listed.contains(idle_service),
                "case {case:?}: expected no series for the unused tool {idle_service}, got \
                 {listed:?}"
            );
        }

        // Every session stopped, the gauge falls back to zero: the concurrency a client
        // reads follows the sessions it still holds.
        for (resource, session) in &started {
            stop_session(case, &router, row.tool, resource, session).await;
        }
        let read = wait_for_trough(case, &router, service, 0.0).await;
        assert_eq!(
            read.trough,
            Some(0.0),
            "case {case:?}: expected the gauge to read 0 once every session was stopped, got {:?}",
            read.trough
        );
    }
}

/// Sessions of DIFFERENT built-in tools are counted into different series: the `Service`
/// dimension is what keeps one tool's concurrency out of the other's.
#[tokio::test]
async fn each_built_in_tools_sessions_are_counted_into_its_own_series() {
    let case = "two code interpreter sessions beside one browser session";
    let router = emulator();
    let interpreter = create_resource(case, &router, Tool::CodeInterpreter, "mixed_interpreter").await;
    let browser = create_resource(case, &router, Tool::Browser, "mixed_browser").await;

    for _ in 0..2 {
        start_session(
            case,
            &router,
            Tool::CodeInterpreter,
            &interpreter,
            LONG_SESSION_TIMEOUT_SECONDS,
        )
        .await;
    }
    start_session(
        case,
        &router,
        Tool::Browser,
        &browser,
        LONG_SESSION_TIMEOUT_SECONDS,
    )
    .await;

    for (tool, expected) in [(Tool::CodeInterpreter, 2.0), (Tool::Browser, 1.0)] {
        let service = tool.service();
        let read = wait_for_peak(case, &router, service, expected).await;
        assert_eq!(
            read.peak,
            Some(expected),
            "case {case:?}: expected {service} to report exactly {expected} open sessions, got \
             {:?}",
            read.peak
        );
    }

    let listed = listed_services(case, &router).await;
    assert_eq!(
        listed,
        BTreeSet::from([
            Tool::CodeInterpreter.service().to_string(),
            Tool::Browser.service().to_string(),
        ]),
        "case {case:?}: expected one series per tool in use, got {listed:?}"
    );
}

/// A session nobody stops stops counting once its time-to-live runs out: the gauge falls
/// back to zero from the CLOCK alone, with no further AgentCore call.
///
/// The time-to-live is the smallest `sessionTimeoutSeconds` the model admits (range
/// 1..28800), which is the one bound this case is about; the wait for the fall is generous.
#[tokio::test]
async fn a_session_that_times_out_stops_counting() {
    let case = "a code interpreter session whose time-to-live runs out";
    let router = emulator();
    let interpreter = create_resource(case, &router, Tool::CodeInterpreter, "expiring_interpreter").await;
    let service = Tool::CodeInterpreter.service();

    start_session(case, &router, Tool::CodeInterpreter, &interpreter, 1).await;

    // It counted while it was usable …
    let read = wait_for_peak(case, &router, service, 1.0).await;
    assert_eq!(
        read.peak,
        Some(1.0),
        "case {case:?}: expected the open session to be counted once, got {:?}",
        read.peak
    );

    // … and stops counting once the clock has made its termination due, with nothing but
    // the clock running.
    let read = wait_for_trough(case, &router, service, 0.0).await;
    assert_eq!(
        read.trough,
        Some(0.0),
        "case {case:?}: expected the gauge to fall to 0 once the time-to-live lapsed, got {:?}",
        read.trough
    );
}
