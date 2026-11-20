// SPDX-License-Identifier: CC0-1.0

//! What an environment's CloudWatch `Monitors` decide about a deployment, followed through to
//! what AppConfig's DATA PLANE serves afterwards — over the WHOLE mounted service set, the one
//! `core/router/src/lib.rs` assembles for the binary.
//!
//! RULE: an environment's `Monitors` are "Amazon CloudWatch alarms to monitor during the
//! deployment process" (`smithy_query(query="shape", shape="Monitor")`), watched over the window
//! `FinalBakeTimeInMinutes` declares — "the amount of time that AppConfig monitors for Amazon
//! CloudWatch alarms before considering the deployment to be complete and no longer eligible for
//! automatic rollback" — so while a deployment into such an environment is in flight AppConfig
//! itself ends it, with no `StopDeployment` from the caller, when a monitored alarm is in its
//! alarming state or when the `AlarmRoleArn` it was to read that alarm under names a role whose
//! trust policy does not let AppConfig assume it: the deployment terminates `ROLLED_BACK`, its
//! event log records the rollback and no completion, the environment settles on `RolledBack`,
//! and what is IN SERVICE is whatever stood there before — so the data plane serves the
//! preceding version's bytes again, or refuses a session outright when no deployment of that
//! profile ever stood in that environment. A deployment whose monitors show neither, over the
//! same declared window, completes and its own bytes are what the data plane serves.
//!
//! Only a cross-service suite can state that end to end: the monitors name resources of two
//! OTHER services — a CloudWatch alarm and an IAM role, both created here through their own
//! operations — and the aftermath is read where a CLIENT reads it, out of the AppConfigData data
//! plane (`StartConfigurationSession` + `GetLatestConfiguration`), never out of the control-plane
//! answer being checked. The AppConfig crate's own `monitored_deployment_rollback` suite tables
//! the rollback DECISION per alarm state, monitor count and role, and reads the aftermath through
//! the control plane's own `GetConfiguration`; what only this suite reaches is the same path with
//! every service mounted and the aftermath taken from the data-plane API, including the session
//! refusal a condemned environment with nothing in service answers.
//!
//! The rows differ along what the rule generalises over: what the monitor shows (an alarm that
//! goes alarming while the deployment bakes, an alarm already alarming, a monitor role whose
//! trust policy withholds the grant, and monitors that show nothing at all), whether the monitor
//! names a role at all, whether a version of the profile was already in service in that
//! environment, and the media type and bytes of the configurations deployed.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_appconfig_amazonappconfig::state as appconfig_state;

use crate::backup_event_rule_target_runs_its_backup_task::role;
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::cw;
use crate::query_wire::form_encode;
use crate::rds_docdb_neptune_signing_name_routing::authorization;

/// The Region every request here is signed for, and so the Region the alarm and the AppConfig
/// hierarchy belong to.
const REGION: &str = "us-east-1";

/// The media type a control-plane JSON body travels with.
const JSON_CONTENT_TYPE: &str = "application/json";

/// The `LocationUri` that selects AppConfig's own hosted configuration store.
const HOSTED: &str = "hosted";

/// The service principal a monitor role has to let assume it for AppConfig to read the alarm it
/// names, taken from the crate under test so the two cannot disagree about how it is spelled.
const APPCONFIG_PRINCIPAL: &str = appconfig_state::alarm_monitors::SERVICE_PRINCIPAL;

/// A service principal that is NOT AppConfig, for the twin role whose trust policy withholds the
/// grant. Any other service principal does: what matters is that AppConfig's is absent.
const OTHER_PRINCIPAL: &str = "ec2.amazonaws.com";

/// The action a monitor role's inline policy grants, which is the read AppConfig performs on the
/// alarm it is handed.
const DESCRIBE_ALARMS: &str = "cloudwatch:DescribeAlarms";

/// CloudWatch's `StateValue` values this suite drives an alarm between
/// (`smithy_query(query="shape", shape="StateValue")`: OK | ALARM | INSUFFICIENT_DATA).
const STATE_OK: &str = "OK";
/// See [`STATE_OK`].
const STATE_ALARM: &str = "ALARM";

/// The `DeploymentEventType` values this suite reads off a deployment's log
/// (`smithy_query(query="shape", shape="DeploymentEventType")`).
const ROLLBACK_STARTED: &str = "ROLLBACK_STARTED";
/// See [`ROLLBACK_STARTED`].
const ROLLBACK_COMPLETED: &str = "ROLLBACK_COMPLETED";
/// See [`ROLLBACK_STARTED`].
const DEPLOYMENT_COMPLETED: &str = "DEPLOYMENT_COMPLETED";

/// The `TriggeredBy` a rollback an ALARM asked for is attributed to, and so the one a rollback
/// no alarm asked for must NOT carry (`smithy_query(query="shape", shape="TriggeredBy")`).
const TRIGGERED_BY_CLOUDWATCH_ALARM: &str = "CLOUDWATCH_ALARM";

/// The `DeploymentState` values a deployment can no longer leave
/// (`smithy_query(query="shape", shape="DeploymentState")`).
const TERMINAL_STATES: &[&str] = &["COMPLETE", "ROLLED_BACK", "REVERTED"];

/// The `EnvironmentState` values this suite reads off `GetEnvironment`: the member names, which
/// is the form they go on the wire in.
const READY_FOR_DEPLOYMENT: &str = "ReadyForDeployment";
/// See [`READY_FOR_DEPLOYMENT`].
const ROLLED_BACK_ENVIRONMENT: &str = "RolledBack";

/// The modelled `ResourceType` the data plane names when nothing of a profile stands in an
/// environment, and the whole fixed `Message` that refusal carries.
const RESOURCE_TYPE_DEPLOYMENT: &str = "Deployment";
/// See [`RESOURCE_TYPE_DEPLOYMENT`].
const DEPLOYMENT_NOT_FOUND: &str = "Deployment not found";

/// The final bake time every row's monitored deployment declares: the window AppConfig watches
/// the environment's monitors over, and the shortest non-zero one the model's
/// `MinutesBetween0And24Hours` (`@range 0..1440`) lets a caller ask for.
const BAKE_IN_MINUTES: i32 = 1;

/// How often work the clock owes is looked for again, and how long the looking goes on.
///
/// Each deadline is a ceiling on the WAIT and nothing else — generous enough that a loaded host
/// cannot decide any outcome, because this suite runs beside every other test of the workspace
/// on one machine. Nothing here sleeps a fixed span on a service's behalf: every wait is on the
/// CONDITION.
const LOOK: Duration = Duration::from_millis(50);
/// See [`LOOK`].
const SETTLED_BY: Duration = Duration::from_secs(120);
/// See [`LOOK`].
const OBSERVED_BY: Duration = Duration::from_secs(60);

/// The whole emulator over one engine, exactly as the binary assembles it, with the engine
/// handed back beside it: one row has to drive the clock the end of the declared bake window is
/// owed by ([`bring_the_window_forward`]).
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(Arc::clone(&engine));
    // The suite exercises the services, not SigV4: each call carries a credential scope so the
    // router can name the target service, without a computed signature.
    router.set_request_authentication(false);
    (router, engine)
}

/// What one dispatched request answered.
struct Answered {
    status: u16,
    headers: ::http::HeaderMap,
    body: Bytes,
}

impl Answered {
    /// The body as JSON, or `Value::Null` for an empty body.
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&self.body).to_string()))
    }

    /// The body as text, for a failure message.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// One string member of a JSON body.
    fn string(&self, member: &str) -> Option<String> {
        self.json()
            .get(member)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// One response header, as text.
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    /// The wire error code of an error response.
    fn error_code(&self) -> String {
        self.string("__type")
            .or_else(|| self.header("x-amzn-errortype"))
            .unwrap_or_else(|| self.text())
    }
}

/// Dispatch one `restJson1` call at the endpoint host of `service`. Both of AppConfig's APIs sign
/// as `appconfig`, whichever endpoint a client addresses.
async fn dispatch(
    router: &MultiServiceRouter,
    service: &str,
    method: &str,
    uri: &str,
    content_type: &str,
    body: Vec<u8>,
) -> Answered {
    let host = format!("{service}.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host.clone())
        .header("authorization", authorization("appconfig"))
        .header("content-type", content_type)
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// One AppConfig CONTROL-plane call carrying a JSON body.
async fn appconfig(router: &MultiServiceRouter, method: &str, uri: &str, body: &Value) -> Answered {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        body.to_string().into_bytes()
    };
    dispatch(router, "appconfig", method, uri, JSON_CONTENT_TYPE, payload).await
}

/// One AppConfig DATA-plane call, as a polling client makes one.
async fn appconfigdata(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &Value,
) -> Answered {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        body.to_string().into_bytes()
    };
    dispatch(
        router,
        "appconfigdata",
        method,
        uri,
        JSON_CONTENT_TYPE,
        payload,
    )
    .await
}

/// One minted id out of a 201 answer.
fn minted_id(case: &str, what: &str, answered: &Answered) -> String {
    assert_eq!(
        answered.status,
        201,
        "case {case:?}: {what} must be created, got {} with {}",
        answered.status,
        answered.text()
    );
    answered
        .string("Id")
        .unwrap_or_else(|| panic!("case {case:?}: {what} answered no Id: {}", answered.text()))
}

/// Create one CloudWatch metric alarm and force it into `state_value`, answering the ARN
/// CloudWatch itself persisted for it.
///
/// The alarm watches a metric nothing publishes into, so its state is whatever `SetAlarmState`
/// last forced — which is how a monitor is driven without depending on any metric data.
async fn given_alarm(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    state_value: &str,
) -> String {
    cw(
        router,
        case,
        REGION,
        "PutMetricAlarm",
        &json!({
            "AlarmName": name,
            "Namespace": "Sim/AppConfigMonitoredRollback",
            "MetricName": "Errors",
            "Statistic": "Sum",
            "Period": 60,
            "EvaluationPeriods": 1,
            "Threshold": 1.0,
            "ComparisonOperator": "GreaterThanOrEqualToThreshold",
            "TreatMissingData": "notBreaching",
        }),
    )
    .await;
    hold_alarm(router, case, name, state_value).await;
    let described = cw(
        router,
        case,
        REGION,
        "DescribeAlarms",
        &json!({ "AlarmNames": [name], "AlarmTypes": ["MetricAlarm"] }),
    )
    .await;
    assert_eq!(
        described["MetricAlarms"][0]["StateValue"].as_str(),
        Some(state_value),
        "case {case:?}: the alarm {name:?} must report the state the suite forced on it, got \
         {described}"
    );
    described["MetricAlarms"][0]["AlarmArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: DescribeAlarms must report {name:?}'s ARN, got {described}")
        })
        .to_string()
}

/// Force one alarm into the state a row needs it in, through `SetAlarmState`.
///
/// Re-applied at every look rather than once, because that force stands only for its own span —
/// "metric alarms return to their actual state quickly" (`SetAlarmState`) — and every row is
/// about an alarm that IS in its state while the deployment runs.
async fn hold_alarm(router: &MultiServiceRouter, case: &str, name: &str, state_value: &str) {
    cw(
        router,
        case,
        REGION,
        "SetAlarmState",
        &json!({
            "AlarmName": name,
            "StateValue": state_value,
            "StateReason": "driven by the suite",
        }),
    )
    .await;
}

/// Create an environment declaring `monitors`, and answer its minted id.
///
/// The environment has to REPORT the monitors it was created with, or there is nothing for the
/// deployment state machine to watch: that round trip is what the rest of the suite rests on.
async fn given_environment(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    name: &str,
    monitors: &[Value],
) -> String {
    let created = appconfig(
        router,
        "POST",
        &format!("/applications/{application}/environments"),
        &json!({ "Name": name, "Monitors": monitors.to_vec() }),
    )
    .await;
    let id = minted_id(case, "the environment", &created);
    let echoed = created.json()["Monitors"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        echoed.len(),
        monitors.len(),
        "case {case:?}: CreateEnvironment must report the {} monitors it was given, got {}",
        monitors.len(),
        created.text()
    );
    id
}

/// A configuration profile over AppConfig's own hosted store, answering its minted id.
async fn given_profile(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    name: &str,
) -> String {
    let created = appconfig(
        router,
        "POST",
        &format!("/applications/{application}/configurationprofiles"),
        &json!({ "Name": name, "LocationUri": HOSTED }),
    )
    .await;
    minted_id(case, "the configuration profile", &created)
}

/// Host one hosted configuration version of `profile`, answering the version number AppConfig
/// stored it as.
async fn host(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    profile: &str,
    content_type: &str,
    content: &[u8],
) -> String {
    let created = dispatch(
        router,
        "appconfig",
        "POST",
        &format!(
            "/applications/{application}/configurationprofiles/{profile}\
             /hostedconfigurationversions"
        ),
        content_type,
        content.to_vec(),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateHostedConfigurationVersion must answer 201, got {} with {}",
        created.status,
        created.text()
    );
    created.header("version-number").unwrap_or_else(|| {
        panic!(
            "case {case:?}: the hosted version must report its number, got {}",
            created.text()
        )
    })
}

/// One deployment strategy, answering its minted id: every target at once, no rollout window,
/// and `bake` minutes of final bake time.
async fn given_strategy(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    bake: i32,
) -> String {
    let created = appconfig(
        router,
        "POST",
        "/deploymentstrategies",
        &json!({
            "Name": name,
            "DeploymentDurationInMinutes": 0,
            "GrowthFactor": 100.0,
            "GrowthType": "LINEAR",
            "FinalBakeTimeInMinutes": bake,
            "ReplicateTo": "NONE",
        }),
    )
    .await;
    minted_id(case, "the deployment strategy", &created)
}

/// `StartDeployment` of `version` into `environment`, answering the deployment number.
async fn start(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    profile: &str,
    strategy: &str,
    version: &str,
) -> i64 {
    let started = appconfig(
        router,
        "POST",
        &format!("/applications/{application}/environments/{environment}/deployments"),
        &json!({
            "ConfigurationProfileId": profile,
            "ConfigurationVersion": version,
            "DeploymentStrategyId": strategy,
        }),
    )
    .await;
    assert_eq!(
        started.status,
        201,
        "case {case:?}: StartDeployment must answer 201, got {} with {}",
        started.status,
        started.text()
    );
    started.json()["DeploymentNumber"]
        .as_i64()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: StartDeployment answered no DeploymentNumber: {}",
                started.text()
            )
        })
}

/// One `GetDeployment`, which must be answered.
async fn deployment(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    number: i64,
) -> Value {
    let read = appconfig(
        router,
        "GET",
        &format!("/applications/{application}/environments/{environment}/deployments/{number}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetDeployment({number}) must answer 200, got {} with {}",
        read.status,
        read.text()
    );
    read.json()
}

/// Read one deployment until it reaches a state it can no longer leave, holding `held` — an
/// alarm name and the state the row needs it in — there while the wait runs; answer the document
/// that reported it.
async fn settled(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    number: i64,
    held: Option<(&str, &str)>,
) -> Value {
    let deadline = Instant::now() + SETTLED_BY;
    loop {
        if let Some((name, state_value)) = held {
            hold_alarm(router, case, name, state_value).await;
        }
        let document = deployment(router, case, application, environment, number).await;
        let state = document["State"].as_str().unwrap_or_default().to_string();
        if TERMINAL_STATES.contains(&state.as_str()) {
            return document;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: deployment {number} never reached a terminal state, it is still \
             {state:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The `EventType`s on a deployment document's log.
fn event_types(case: &str, document: &Value) -> Vec<String> {
    document
        .get("EventLog")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: a Deployment document carries an EventLog"))
        .iter()
        .map(|event| event["EventType"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The newest event of `event_type` on a deployment document's log.
fn event_of(case: &str, document: &Value, event_type: &str) -> Value {
    document
        .get("EventLog")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: a Deployment document carries an EventLog"))
        .iter()
        .find(|event| event["EventType"].as_str() == Some(event_type))
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the log must carry a {event_type} event, got {document}")
        })
}

/// The `State` `GetEnvironment` reports for `environment`.
async fn environment_state(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
) -> String {
    let read = appconfig(
        router,
        "GET",
        &format!("/applications/{application}/environments/{environment}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetEnvironment must answer 200, got {} with {}",
        read.status,
        read.text()
    );
    read.string("State")
        .unwrap_or_else(|| panic!("case {case:?}: GetEnvironment answered no State: {}", read.text()))
}

/// One `StartConfigurationSession` over the triple, whatever it answers: the data plane's own
/// view of whether anything of that profile stands in that environment.
async fn session(
    router: &MultiServiceRouter,
    application: &str,
    environment: &str,
    profile: &str,
) -> Answered {
    appconfigdata(
        router,
        "POST",
        "/configurationsessions",
        &json!({
            "ApplicationIdentifier": application,
            "EnvironmentIdentifier": environment,
            "ConfigurationProfileIdentifier": profile,
        }),
    )
    .await
}

/// The bytes AppConfig's data plane serves ONE client of this triple right now: a session of its
/// own, then the configuration that session is handed.
async fn served(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    profile: &str,
) -> Vec<u8> {
    let opened = session(router, application, environment, profile).await;
    assert_eq!(
        opened.status,
        201,
        "case {case:?}: StartConfigurationSession must answer 201, got {} with {}",
        opened.status,
        opened.text()
    );
    let token = opened.string("InitialConfigurationToken").unwrap_or_else(|| {
        panic!(
            "case {case:?}: StartConfigurationSession answered no token: {}",
            opened.text()
        )
    });
    let read = appconfigdata(
        router,
        "GET",
        &format!("/configuration?configuration_token={}", form_encode(&token)),
        &Value::Null,
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetLatestConfiguration must answer 200, got {} with {}",
        read.status,
        read.text()
    );
    read.body.to_vec()
}

/// What a client is served once what the deployment reported has reached the data plane: read
/// again until the bytes are `expected` or the deadline passes, then answer the last read, which
/// is what the caller asserts on.
async fn serving(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    profile: &str,
    expected: &[u8],
) -> Vec<u8> {
    let deadline = Instant::now() + OBSERVED_BY;
    loop {
        let bytes = served(router, case, application, environment, profile).await;
        if bytes == expected || Instant::now() >= deadline {
            return bytes;
        }
        tokio::time::sleep(LOOK).await;
    }
}

/// The refusal the data plane answers a triple nothing of the profile stands in: read again until
/// a session IS refused or the deadline passes, then answer the last reply, which is what the
/// caller asserts on.
async fn refused_session(
    router: &MultiServiceRouter,
    application: &str,
    environment: &str,
    profile: &str,
) -> Answered {
    let deadline = Instant::now() + OBSERVED_BY;
    loop {
        let opened = session(router, application, environment, profile).await;
        if opened.status != 201 || Instant::now() >= deadline {
            return opened;
        }
        tokio::time::sleep(LOOK).await;
    }
}

/// Bring the end of the declared bake window forward for every deployment the environment holds,
/// by moving the instant each one started back by that window.
///
/// The bake keeps the minutes the caller declared — it is the window they asked AppConfig to
/// monitor the deployment over — so the row about a deployment that must COMPLETE drives the
/// clock that transition is owed by instead of waiting the declared minute out. The transition
/// itself is still the service's: nothing here moves a state or writes an event.
fn bring_the_window_forward(
    engine: &Arc<engine::InMemoryEngine>,
    application: &str,
    environment: &str,
) {
    let held = engine.state::<appconfig_state::ComAmazonawsAppconfigAmazonappconfigState>();
    let mut store = held.lock();
    let Some(environment) = store
        .application_mut(application)
        .and_then(|application| application.environments.get_mut(environment))
    else {
        panic!("the environment the deployment was started into must be held");
    };
    for deployment in environment.deployments.values_mut() {
        deployment.started_at -= appconfig_state::bake_duration(BAKE_IN_MINUTES);
    }
}

/// Bytes as a failure message reports them: the text when they are text, so a row whose document
/// is readable fails readably, and the bytes themselves otherwise.
fn shown(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => format!("{text:?}"),
        Err(_) => format!("{bytes:?}"),
    }
}

/// What the environment's single monitor names: whether it carries an `AlarmRoleArn` at all, and
/// whether the role it names is one AppConfig may assume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MonitorRole {
    /// A role whose trust policy names AppConfig: the alarm can be read.
    Trusted,
    /// No `AlarmRoleArn` at all; AppConfig reads the alarm as itself.
    None,
    /// A role whose trust policy names another service: the alarm cannot be read at all.
    Untrusted,
}

/// What the monitors make of the deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// Nothing the monitors show ends it, so it completes over the declared window.
    Completes,
    /// A monitored alarm is alarming, so AppConfig rolls it back and attributes that to the
    /// alarm, describing it by the alarm's own ARN.
    RolledBackByTheAlarm,
    /// The alarm cannot be read at all, so AppConfig rolls the deployment back rather than ship
    /// it unmonitored, and names the role it could not assume.
    RolledBackByTheRole,
}

/// One row: what its environment monitors, what stands in that environment beforehand, and what
/// must become of the deployment and of what the data plane serves.
struct Row {
    case: &'static str,
    role: MonitorRole,
    /// The state the alarm is held in before the deployment is started.
    before: &'static str,
    /// The state the alarm is held in once the deployment is under way, which is what makes one
    /// row's alarm go alarming DURING the window it is monitored over.
    during: &'static str,
    /// The bytes already in service in that environment, hosted and deployed first; `None` for a
    /// row where nothing of the profile ever stood there.
    in_service: Option<&'static [u8]>,
    /// The bytes of the version the monitored deployment carries.
    candidate: &'static [u8],
    /// The media type both versions of this row are written with.
    content_type: &'static str,
    outcome: Outcome,
}

const ROWS: &[Row] = &[
    Row {
        case: "an alarm that goes into ALARM while the deployment bakes, over a version in \
               service",
        role: MonitorRole::Trusted,
        before: STATE_OK,
        during: STATE_ALARM,
        in_service: Some(br#"{"release":"first","flags":"off"}"#),
        candidate: br#"{"release":"second","flags":"on"}"#,
        content_type: JSON_CONTENT_TYPE,
        outcome: Outcome::RolledBackByTheAlarm,
    },
    Row {
        case: "an alarm already in ALARM, monitored under no role at all, nothing in service yet",
        role: MonitorRole::None,
        before: STATE_ALARM,
        during: STATE_ALARM,
        in_service: None,
        candidate: b"release: second\nflags: on\n",
        content_type: "application/x-yaml",
        outcome: Outcome::RolledBackByTheAlarm,
    },
    Row {
        case: "a monitor role whose trust policy withholds the grant, nothing in service yet",
        role: MonitorRole::Untrusted,
        before: STATE_OK,
        during: STATE_OK,
        in_service: None,
        candidate: &[0x00, 0x01, 0xff, 0xfe, 0x7f, 0x80],
        content_type: "application/octet-stream",
        outcome: Outcome::RolledBackByTheRole,
    },
    Row {
        case: "a monitor role whose trust policy withholds the grant, over a version in service",
        role: MonitorRole::Untrusted,
        before: STATE_OK,
        during: STATE_OK,
        in_service: Some(b"first release\n"),
        candidate: b"second release\n",
        content_type: "text/plain",
        outcome: Outcome::RolledBackByTheRole,
    },
    Row {
        case: "an alarm that stays OK under a role that trusts AppConfig, over a version in \
               service",
        role: MonitorRole::Trusted,
        before: STATE_OK,
        during: STATE_OK,
        in_service: Some(br#"{"release":"first"}"#),
        candidate: br#"{"release":"second"}"#,
        content_type: JSON_CONTENT_TYPE,
        outcome: Outcome::Completes,
    },
];

/// RULE: an environment's `Monitors` decide a deployment in flight into it — an alarming alarm or
/// a monitor role AppConfig cannot assume terminates it `ROLLED_BACK` with the rollback on its
/// log, no completion logged, the environment `RolledBack` and whatever preceded it back in
/// service, while monitors that show neither let it complete over the same declared window — and
/// what is in service is read where a client reads it, out of the data-plane API.
#[tokio::test]
async fn an_environments_monitors_decide_the_deployment_and_what_the_data_plane_serves_next() {
    let (router, engine) = emulator();
    let setup = "the suite's fixtures";
    let trusted = role(
        &router,
        "appconfig-monitor-trusted",
        APPCONFIG_PRINCIPAL,
        &[DESCRIBE_ALARMS],
    )
    .await;
    // The twin role differs ONLY in which service its trust policy names: both grant the same
    // read on the alarm, so nothing but the trust relationship can decide the rows apart.
    let untrusted = role(
        &router,
        "appconfig-monitor-untrusted",
        OTHER_PRINCIPAL,
        &[DESCRIBE_ALARMS],
    )
    .await;
    let application = minted_id(
        setup,
        "the application",
        &appconfig(
            &router,
            "POST",
            "/applications",
            &json!({ "Name": "monitored-rollback" }),
        )
        .await,
    );
    // Two strategies: the monitored one declares the bake window the rule is about, and the
    // other puts a version in service with no window at all, so a row's predecessor is in place
    // without waiting on anything.
    let monitored = given_strategy(&router, setup, "monitored-bake", BAKE_IN_MINUTES).await;
    let immediate = given_strategy(&router, setup, "no-bake", 0).await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let alarm_name = format!("monitored-rollback-{index}");
        let alarm = given_alarm(&router, case, &alarm_name, row.before).await;
        let monitor = match row.role {
            MonitorRole::Trusted => json!({ "AlarmArn": alarm, "AlarmRoleArn": trusted }),
            MonitorRole::None => json!({ "AlarmArn": alarm }),
            MonitorRole::Untrusted => json!({ "AlarmArn": alarm, "AlarmRoleArn": untrusted }),
        };
        // Its own environment and profile per row: an environment takes one deployment at a
        // time, and each row writes its versions in its own media type.
        let environment = given_environment(
            &router,
            case,
            &application,
            &format!("monitored-{index}"),
            &[monitor],
        )
        .await;
        let profile = given_profile(&router, case, &application, &format!("profile-{index}")).await;

        // What stands in the environment before the monitored deployment, where the row has one:
        // a deployment with no window of its own, so nothing is monitored over it.
        if let Some(bytes) = row.in_service {
            let version = host(&router, case, &application, &profile, row.content_type, bytes).await;
            let number = start(
                &router, case, &application, &environment, &profile, &immediate, &version,
            )
            .await;
            let document = settled(
                &router,
                case,
                &application,
                &environment,
                number,
                Some((&alarm_name, row.before)),
            )
            .await;
            assert_eq!(
                document["State"].as_str(),
                Some("COMPLETE"),
                "case {case:?}: the version that precedes the monitored deployment must be in \
                 service, got {document}"
            );
            let actual = serving(&router, case, &application, &environment, &profile, bytes).await;
            assert_eq!(
                actual,
                bytes.to_vec(),
                "case {case:?}: the data plane must serve the preceding version, expected {}, got \
                 {}",
                shown(bytes),
                shown(&actual)
            );
        }

        let candidate = host(
            &router,
            case,
            &application,
            &profile,
            row.content_type,
            row.candidate,
        )
        .await;
        let number = start(
            &router, case, &application, &environment, &profile, &monitored, &candidate,
        )
        .await;

        if row.outcome == Outcome::Completes {
            // The window the caller declared is a window AppConfig MONITORS the deployment over,
            // so the deployment is still in flight while it runs — a minute of wall clock, which
            // no read taken straight after the start can have passed — and only the clock
            // reaching the end of it completes the deployment.
            let in_flight =
                deployment(&router, case, &application, &environment, number).await;
            let state = in_flight["State"].as_str().unwrap_or_default().to_string();
            assert!(
                !TERMINAL_STATES.contains(&state.as_str()),
                "case {case:?}: a deployment declaring a bake window is monitored over it rather \
                 than finished at once, got {state:?}"
            );
            bring_the_window_forward(&engine, &application, &environment);
        }

        let document = settled(
            &router,
            case,
            &application,
            &environment,
            number,
            Some((&alarm_name, row.during)),
        )
        .await;
        let state = document["State"].as_str().unwrap_or_default().to_string();
        let log = event_types(case, &document);

        if row.outcome == Outcome::Completes {
            assert_eq!(
                state, "COMPLETE",
                "case {case:?}: nothing its monitors show ends this deployment, so it completes, \
                 got {state:?} with log {log:?}"
            );
            assert!(
                log.iter().any(|event| event == DEPLOYMENT_COMPLETED),
                "case {case:?}: a completed deployment logs its completion, got {log:?}"
            );
            assert!(
                !log.iter().any(|event| event == ROLLBACK_STARTED),
                "case {case:?}: nothing rolled this deployment back, got {log:?}"
            );
            let reported = environment_state(&router, case, &application, &environment).await;
            assert_eq!(
                reported, READY_FOR_DEPLOYMENT,
                "case {case:?}: an environment whose deployment completed takes another, got \
                 {reported:?}"
            );
            let actual = serving(
                &router,
                case,
                &application,
                &environment,
                &profile,
                row.candidate,
            )
            .await;
            assert_eq!(
                actual,
                row.candidate.to_vec(),
                "case {case:?}: the data plane must serve the version that completed, expected \
                 {}, got {}",
                shown(row.candidate),
                shown(&actual)
            );
            continue;
        }

        assert_eq!(
            state, "ROLLED_BACK",
            "case {case:?}: AppConfig ends a deployment its monitors condemn rolled back, got \
             {state:?} with log {log:?}"
        );
        assert!(
            !log.iter().any(|event| event == DEPLOYMENT_COMPLETED),
            "case {case:?}: a deployment AppConfig rolled back never completed, got {log:?}"
        );

        // Both events of the rollback name what caused it: a client reading the log has to be
        // able to tell an alarm's rollback from one AppConfig performed because it could not read
        // the alarm at all.
        let expected_description = match row.outcome {
            Outcome::RolledBackByTheRole => {
                format!("Unable to assume environment monitor role: {untrusted}")
            }
            _ => alarm.clone(),
        };
        for event_type in [ROLLBACK_STARTED, ROLLBACK_COMPLETED] {
            let event = event_of(case, &document, event_type);
            assert_eq!(
                event["Description"].as_str(),
                Some(expected_description.as_str()),
                "case {case:?}: the {event_type} Description must name what caused the rollback, \
                 expected {expected_description:?}, got {event}"
            );
        }
        let attributions: Vec<String> = document["EventLog"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|event| event["TriggeredBy"].as_str().unwrap_or_default().to_string())
            .collect();
        let by_the_alarm = attributions
            .iter()
            .any(|triggered_by| triggered_by == TRIGGERED_BY_CLOUDWATCH_ALARM);
        assert_eq!(
            by_the_alarm,
            row.outcome == Outcome::RolledBackByTheAlarm,
            "case {case:?}: only a rollback an alarm asked for is attributed to \
             {TRIGGERED_BY_CLOUDWATCH_ALARM}, got {attributions:?}"
        );
        if row.outcome == Outcome::RolledBackByTheRole {
            // The alarm said nothing here — it was never readable — so no event of this
            // rollback may be attributed to it, the finished one least of all: that is the only
            // place an operator can tell a broken trust policy from an alarm that fired.
            let finished = event_of(case, &document, ROLLBACK_COMPLETED);
            assert_ne!(
                finished["TriggeredBy"].as_str(),
                Some(TRIGGERED_BY_CLOUDWATCH_ALARM),
                "case {case:?}: a rollback no alarm asked for is attributed to something else, \
                 got {finished}"
            );
        } else {
            let begun = event_of(case, &document, ROLLBACK_STARTED);
            assert_eq!(
                begun["TriggeredBy"].as_str(),
                Some(TRIGGERED_BY_CLOUDWATCH_ALARM),
                "case {case:?}: a rollback the alarm asked for is attributed to it, got {begun}"
            );
        }

        let reported = environment_state(&router, case, &application, &environment).await;
        assert_eq!(
            reported, ROLLED_BACK_ENVIRONMENT,
            "case {case:?}: an environment whose deployment was rolled back reports it, got \
             {reported:?}"
        );

        // The aftermath a CLIENT sees: the rollback put back whatever stood there before, which
        // for a row that had nothing in that environment is nothing at all — so the data plane
        // has no deployed configuration to open a session over.
        match row.in_service {
            Some(bytes) => {
                let actual =
                    serving(&router, case, &application, &environment, &profile, bytes).await;
                assert_eq!(
                    actual,
                    bytes.to_vec(),
                    "case {case:?}: the rollback must put the preceding version back in service, \
                     expected {}, got {}",
                    shown(bytes),
                    shown(&actual)
                );
            }
            None => {
                let refused = refused_session(&router, &application, &environment, &profile).await;
                assert_eq!(
                    refused.status,
                    404,
                    "case {case:?}: a session over an environment nothing stands in is refused \
                     404, got {} with {}",
                    refused.status,
                    refused.text()
                );
                assert_eq!(
                    refused.error_code(),
                    "ResourceNotFoundException",
                    "case {case:?}: that refusal is the modelled ResourceNotFoundException, got {}",
                    refused.text()
                );
                assert_eq!(
                    refused.json()["ResourceType"].as_str(),
                    Some(RESOURCE_TYPE_DEPLOYMENT),
                    "case {case:?}: the missing resource is the deployment, got {}",
                    refused.text()
                );
                assert_eq!(
                    refused.json()["Message"].as_str(),
                    Some(DEPLOYMENT_NOT_FOUND),
                    "case {case:?}: the refusal carries its whole fixed message, got {}",
                    refused.text()
                );
                assert!(
                    refused.json()["InitialConfigurationToken"].is_null(),
                    "case {case:?}: a refused session mints no token, got {}",
                    refused.text()
                );
            }
        }
    }
}
