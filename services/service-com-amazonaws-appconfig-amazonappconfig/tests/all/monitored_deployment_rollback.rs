// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch alarm MONITORS an environment declares, driven end to end with
//! CloudWatch and IAM mounted beside AppConfig on one engine.
//!
//! The rule under test: while AppConfig is working on a deployment into an environment that
//! declares `Monitors` — "Amazon CloudWatch alarms to monitor during the deployment
//! process", watched until the deployment is complete and "no longer eligible for automatic
//! rollback" (`FinalBakeTimeInMinutes`) — the service rolls that deployment back itself,
//! with no `StopDeployment` from the caller, when a monitored alarm is in its alarming state
//! or when the monitor role AppConfig was asked to read that alarm under does not let
//! `appconfig.amazonaws.com` assume it. Either way the deployment terminates `ROLLED_BACK`,
//! its log records the rollback, no completion is logged, the environment settles on
//! `RolledBack`, and the configuration version that preceded it is what the data plane
//! serves again.
//!
//! The table spans what the rule generalises over: the alarm's own state (all three
//! `StateValue`s), whether the monitor names a role at all, whether that role's trust policy
//! names AppConfig or another service, how many monitors the environment declares and which
//! of them is breached, an environment with no monitors at all, and a monitor naming an
//! alarm this account holds none of — which shows nothing and so blocks nothing.
//!
//! Every alarm here is created with `PutMetricAlarm` and driven with `SetAlarmState`, and
//! every role with IAM's own `CreateRole`: the operations a client uses, so nothing reaches
//! into either service's store.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_appconfig_amazonappconfig::state;

use crate::appconfig_workflow::{
    create_application, create_profile, json_call, send, TERMINAL_DEPLOYMENT_STATES,
};
use crate::deployment_wire_fidelity::member;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The service principal a monitor role has to trust for AppConfig to read the alarm it
/// names, as the crate under test spells it.
///
/// `pub(crate)` with the fixtures below: `tests/all/zzz_probe.rs` probes the same monitors
/// from the same fixtures, so one place says how a role, an alarm and a monitored
/// environment are created.
pub(crate) const APPCONFIG_PRINCIPAL: &str = state::alarm_monitors::SERVICE_PRINCIPAL;

/// A service principal that is NOT AppConfig, for the twin role whose trust policy withholds
/// the grant. Any other service principal does: what matters is that AppConfig's is absent.
pub(crate) const OTHER_PRINCIPAL: &str = "ec2.amazonaws.com";

/// CloudWatch's three `StateValue` values, from that service's own model
/// (`smithy_query(query="shape",shape="StateValue")`: OK | ALARM | INSUFFICIENT_DATA).
pub(crate) const STATE_OK: &str = "OK";
/// See [`STATE_OK`].
pub(crate) const STATE_ALARM: &str = "ALARM";
/// See [`STATE_OK`].
const STATE_INSUFFICIENT_DATA: &str = "INSUFFICIENT_DATA";

/// The `DeploymentEventType` values this suite reads off a deployment's log.
const ROLLBACK_STARTED: &str = "ROLLBACK_STARTED";
/// See [`ROLLBACK_STARTED`].
const ROLLBACK_COMPLETED: &str = "ROLLBACK_COMPLETED";
/// See [`ROLLBACK_STARTED`].
const DEPLOYMENT_COMPLETED: &str = "DEPLOYMENT_COMPLETED";

/// An ARN no alarm of this account carries, for the row whose monitor names an alarm the
/// CloudWatch store holds none of. Shaped like an alarm ARN so the monitor is well formed.
const UNHELD_ALARM: &str = "arn:aws:cloudwatch:us-east-1:000000000000:alarm:held-by-nobody";

/// AppConfig, CloudWatch and IAM on ONE engine: an environment's monitors name resources of
/// the other two services, and this suite creates them through their own operations.
///
/// The engine is answered beside the router because one row has to drive the clock the bake
/// window's end is owed by ([`bring_the_window_forward`]).
pub(crate) fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_appconfig_amazonappconfig::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    (router, engine)
}

/// One `awsJson1_0` RPC against the CloudWatch emulator, requiring success; the SigV4
/// credential scope is what routes it to the `monitoring` signing name.
async fn cloudwatch_ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("monitoring.{REGION}.amazonaws.com"))
        .header(
            "x-amz-target",
            format!("GraniteServiceVersion20100801.{operation}"),
        )
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTEST/20260101/{REGION}/monitoring/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    assert_eq!(
        status,
        200,
        "cloudwatch {operation} must answer 200, got {status} / {}",
        String::from_utf8_lossy(&payload)
    );
    serde_json::from_slice(&payload).unwrap_or(Value::Null)
}

/// One `awsQuery` request against the IAM emulator, answering its XML body.
async fn iam_query(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTEST/20260101/{REGION}/iam/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let answered = String::from_utf8_lossy(&payload).to_string();
    assert_eq!(
        status, 200,
        "iam {action} must answer 200, got {status} / {answered}"
    );
    answered
}

/// Percent-encode one form value the way an AWS SDK encodes an `awsQuery` body — through the
/// crate that owns percent-encoding, over the set that escapes everything outside the
/// unreserved characters.
fn form_encode(value: &str) -> String {
    ::percent_encoding::utf8_percent_encode(value, ::percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// The text of the first `<name>` element of an `awsQuery` XML answer.
fn xml_field(document: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = document.find(&open)? + open.len();
    let end = document[start..].find(&close)? + start;
    Some(document[start..end].to_string())
}

/// Create one IAM role whose trust policy lets `principal`, and only `principal`, assume it,
/// through IAM's own `CreateRole`; answer the ARN IAM persisted for it.
pub(crate) async fn given_role(router: &MultiServiceRouter, name: &str, principal: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": principal},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = iam_query(
        router,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    xml_field(&created, "Arn")
        .unwrap_or_else(|| panic!("CreateRole {name:?} answered no Arn: {created}"))
}

/// Create one CloudWatch metric alarm and force it into `state_value`, answering the ARN
/// CloudWatch persisted for it.
///
/// The alarm watches a metric nothing publishes into, so its state is whatever
/// `SetAlarmState` last forced — which is how a monitor is driven without depending on any
/// metric data. That force is TEMPORARY: "metric alarms return to their actual state
/// quickly, often within seconds" (`SetAlarmState`), so a row whose alarm has to BE in that
/// state while its deployment runs holds it there at every look ([`hold_alarms`]).
pub(crate) async fn given_alarm(
    router: &MultiServiceRouter,
    name: &str,
    state_value: &str,
) -> String {
    cloudwatch_ok(
        router,
        "PutMetricAlarm",
        &json!({
            "AlarmName": name,
            "Namespace": "Sim/AppConfigMonitors",
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
    hold_alarms(router, &[(name.to_string(), state_value)]).await;
    let described = cloudwatch_ok(
        router,
        "DescribeAlarms",
        &json!({ "AlarmNames": [name], "AlarmTypes": ["MetricAlarm"] }),
    )
    .await;
    let reported = described["MetricAlarms"][0]["StateValue"].as_str();
    assert_eq!(
        reported,
        Some(state_value),
        "the alarm {name:?} must report the state the suite forced on it, got {described}"
    );
    described["MetricAlarms"][0]["AlarmArn"]
        .as_str()
        .unwrap_or_else(|| panic!("DescribeAlarms must report {name:?}'s ARN, got {described}"))
        .to_string()
}

/// Force each of `held` into the state the row needs it in, through `SetAlarmState`.
///
/// Re-applied at every look rather than once, because the force stands only for its own span
/// and the row is about an alarm that IS in that state while the deployment runs.
async fn hold_alarms(router: &MultiServiceRouter, held: &[(String, &str)]) {
    for (name, state_value) in held {
        cloudwatch_ok(
            router,
            "SetAlarmState",
            &json!({
                "AlarmName": name,
                "StateValue": state_value,
                "StateReason": "driven by the suite",
            }),
        )
        .await;
    }
}

/// How long [`settled_with`] waits for a deployment to settle.
///
/// A generous ceiling on the WAIT rather than a statement about the span any stage takes: a
/// deployment that has not settled by here is one nothing is moving.
const SETTLES_BY: std::time::Duration = std::time::Duration::from_secs(60);

/// How often [`settled_with`] re-reads the deployment, and re-applies the row's forces.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// Poll `GetDeployment` until the deployment reports a terminal state, holding every alarm of
/// `held` in the state the row needs it in while the wait runs; answer the document that
/// reported it.
///
/// Waits for the CONDITION, never for a span: every stage after the one `StartDeployment`
/// answers is owed by the clock, so this re-reads until the deployment has settled.
pub(crate) async fn settled_with(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    number: i64,
    held: &[(String, &str)],
) -> Value {
    let deadline = std::time::Instant::now() + SETTLES_BY;
    loop {
        hold_alarms(router, held).await;
        let read = json_call(
            router,
            "GET",
            &format!(
                "/applications/{application}/environments/{environment}/deployments/{number}"
            ),
            &Value::Null,
        )
        .await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: GetDeployment must answer 200, got {} / {}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        let document = read.json();
        let state = read.string("State").unwrap_or_default();
        if TERMINAL_DEPLOYMENT_STATES.contains(&state.as_str()) {
            return document;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: deployment {number} did not settle within {}s; last state {state:?}",
            SETTLES_BY.as_secs()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Create an environment declaring `monitors`, and answer its minted id.
pub(crate) async fn environment_with_monitors(
    router: &MultiServiceRouter,
    application: &str,
    name: &str,
    monitors: &[Value],
) -> String {
    let mut body = json!({ "Name": name });
    if !monitors.is_empty() {
        body["Monitors"] = Value::from(monitors.to_vec());
    }
    let answer = json_call(
        router,
        "POST",
        &format!("/applications/{application}/environments"),
        &body,
    )
    .await;
    assert_eq!(
        answer.status,
        201,
        "case {name:?}: CreateEnvironment must answer 201, got {} / {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    // The environment has to REPORT the monitors it was created with, or there is nothing
    // for the deployment state machine to watch: this is the round trip the rest of the
    // suite rests on.
    let created = answer.json();
    let echoed = created
        .get("Monitors")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        echoed.len(),
        monitors.len(),
        "case {name:?}: CreateEnvironment must report the {} monitors it was given, got {created}",
        monitors.len()
    );
    answer
        .string("Id")
        .unwrap_or_else(|| panic!("case {name:?}: CreateEnvironment answered no Id"))
}

/// Host one hosted configuration version of `profile` carrying `content`, and answer the
/// version number it was stored as.
pub(crate) async fn host(
    router: &MultiServiceRouter,
    application: &str,
    profile: &str,
    content: &[u8],
) -> String {
    let answer = send(
        router,
        "POST",
        &format!(
            "/applications/{application}/configurationprofiles/{profile}\
             /hostedconfigurationversions"
        ),
        "application/json",
        content.to_vec(),
        &[],
    )
    .await;
    assert_eq!(
        answer.status,
        201,
        "CreateHostedConfigurationVersion must answer 201, got {} / {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    answer
        .header("version-number")
        .unwrap_or_else(|| panic!("the hosted version must report its number"))
}

/// The strategy every row's deployment is paced by: no rollout window, every target at
/// once, and ONE minute of final bake time — the window AppConfig watches the environment's
/// monitors over, and the shortest the model's `MinutesBetween0And24Hours` (`range 0..1440`)
/// allows a caller to ask for.
const BAKE_IN_MINUTES: i32 = 1;

/// `CreateDeploymentStrategy` with that strategy, answering the minted id.
pub(crate) async fn given_strategy(router: &MultiServiceRouter, name: &str) -> String {
    let answer = json_call(
        router,
        "POST",
        "/deploymentstrategies",
        &json!({
            "Name": name,
            "DeploymentDurationInMinutes": 0,
            "GrowthFactor": 100.0,
            "GrowthType": "LINEAR",
            "FinalBakeTimeInMinutes": BAKE_IN_MINUTES,
            "ReplicateTo": "NONE",
        }),
    )
    .await;
    assert_eq!(
        answer.status,
        201,
        "CreateDeploymentStrategy must answer 201, got {} / {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    answer
        .string("Id")
        .unwrap_or_else(|| panic!("CreateDeploymentStrategy answered no Id"))
}

/// `StartDeployment` of `version` into `environment`, answering the deployment number.
pub(crate) async fn start(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    profile: &str,
    strategy: &str,
    version: &str,
) -> i64 {
    let answer = json_call(
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
        answer.status,
        201,
        "case {case:?}: StartDeployment must answer 201, got {} / {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    member(&answer.json(), "DeploymentNumber")
        .as_i64()
        .unwrap_or_else(|| panic!("case {case:?}: StartDeployment answered no DeploymentNumber"))
}

/// Bring the end of the declared bake window forward for every deployment the environment
/// holds, by moving the instant each one started back by that window.
///
/// The bake keeps the minutes the caller declared — it is the window they asked AppConfig to
/// monitor the deployment for — so a row about a deployment that must COMPLETE drives the
/// clock that transition is owed by instead of waiting the declared minute out. The
/// transition itself is still the service's: nothing here moves a state or writes an event.
fn bring_the_window_forward(
    engine: &Arc<engine::InMemoryEngine>,
    application: &str,
    environment: &str,
) {
    let held = engine.state::<state::ComAmazonawsAppconfigAmazonappconfigState>();
    let mut store = held.lock();
    let Some(environment) = store
        .application_mut(application)
        .and_then(|application| application.environments.get_mut(environment))
    else {
        panic!("the environment the deployment was started into must be held");
    };
    for deployment in environment.deployments.values_mut() {
        deployment.started_at -= state::bake_duration(BAKE_IN_MINUTES);
    }
}

/// The `EventType`s of a deployment document's log, newest first.
fn event_types(case: &str, document: &Value) -> Vec<String> {
    document
        .get("EventLog")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: a Deployment document carries an EventLog"))
        .iter()
        .map(|event| {
            event["EventType"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// The newest event of `event_type` in a deployment document's log.
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
    let answer = json_call(
        router,
        "GET",
        &format!("/applications/{application}/environments/{environment}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: GetEnvironment must answer 200, got {} / {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    answer
        .string("State")
        .unwrap_or_else(|| panic!("case {case:?}: GetEnvironment answered no State"))
}

/// What a row's monitors name, which is what the rule generalises over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Monitors {
    /// No `Monitors` at all: nothing to watch.
    None,
    /// One monitor on an alarm forced into the named state, read under an assumable role.
    One(&'static str),
    /// One monitor on an alarm forced into the named state and NO `AlarmRoleArn`: AppConfig
    /// reads it as itself.
    OneWithoutARole(&'static str),
    /// One monitor on an alarm forced into the named state, read under a role whose trust
    /// policy does not name AppConfig.
    OneUnderAnUnassumableRole(&'static str),
    /// Two monitors under assumable roles, each on an alarm forced into the named state.
    Two(&'static str, &'static str),
    /// One monitor naming an alarm this account holds none of, under an assumable role.
    OneOnAnUnheldAlarm,
}

/// What a row expects of the deployment once it settles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The deployment completes: nothing the monitors show rolls it back.
    Completes,
    /// AppConfig rolls it back because a monitored alarm is alarming, attributed to that
    /// alarm and described by its ARN.
    RolledBackByTheAlarm,
    /// AppConfig rolls it back because it could not read the alarm at all, attributed to the
    /// error that stopped the monitoring and described by the role it could not assume.
    RolledBackByTheRole,
}

/// One row: the case, what its environment monitors, and what must become of the deployment.
struct Row {
    case: &'static str,
    monitors: Monitors,
    outcome: Outcome,
}

/// The rows. Each one deploys the same configuration under the same strategy into its own
/// environment, so the only thing that differs is what that environment monitors.
const ROWS: &[Row] = &[
    Row {
        case: "no monitors at all",
        monitors: Monitors::None,
        outcome: Outcome::Completes,
    },
    Row {
        case: "one monitor whose alarm is OK",
        monitors: Monitors::One(STATE_OK),
        outcome: Outcome::Completes,
    },
    Row {
        case: "one monitor whose alarm has insufficient data",
        monitors: Monitors::One(STATE_INSUFFICIENT_DATA),
        outcome: Outcome::Completes,
    },
    Row {
        case: "one monitor whose alarm is alarming",
        monitors: Monitors::One(STATE_ALARM),
        outcome: Outcome::RolledBackByTheAlarm,
    },
    Row {
        case: "one monitor with no role whose alarm is alarming",
        monitors: Monitors::OneWithoutARole(STATE_ALARM),
        outcome: Outcome::RolledBackByTheAlarm,
    },
    Row {
        case: "two monitors, the second of them alarming",
        monitors: Monitors::Two(STATE_OK, STATE_ALARM),
        outcome: Outcome::RolledBackByTheAlarm,
    },
    Row {
        case: "two monitors, neither of them alarming",
        monitors: Monitors::Two(STATE_OK, STATE_INSUFFICIENT_DATA),
        outcome: Outcome::Completes,
    },
    Row {
        case: "one monitor whose alarm is OK but whose role withholds the grant",
        monitors: Monitors::OneUnderAnUnassumableRole(STATE_OK),
        outcome: Outcome::RolledBackByTheRole,
    },
    Row {
        case: "one monitor whose alarm is alarming AND whose role withholds the grant",
        monitors: Monitors::OneUnderAnUnassumableRole(STATE_ALARM),
        // The alarm cannot be read at all under that role, so no state of it is evidence of
        // anything: the rollback is the unreadable monitor's, not the alarm's.
        outcome: Outcome::RolledBackByTheRole,
    },
    Row {
        case: "one monitor naming an alarm this account holds none of",
        monitors: Monitors::OneOnAnUnheldAlarm,
        // Nothing can be read about that alarm, so nothing is shown about it and the
        // deployment is not rolled back for a state nothing answered.
        outcome: Outcome::Completes,
    },
];

/// What a row's monitors are: the `Monitors` list its environment declares, the alarms this
/// suite has to hold in a state for the whole wait, and the role ARN its unreadable monitor
/// names (when it has one).
struct Declared {
    monitors: Vec<Value>,
    held: Vec<(String, &'static str)>,
    unreadable_role: Option<String>,
}

/// Build what a row declares.
async fn monitors_of(
    router: &MultiServiceRouter,
    index: usize,
    row: &Row,
    assumable: &str,
    unassumable: &str,
) -> Declared {
    match row.monitors {
        Monitors::None => Declared {
            monitors: Vec::new(),
            held: Vec::new(),
            unreadable_role: None,
        },
        Monitors::One(state_value) => {
            let name = format!("alarm-{index}");
            let arn = given_alarm(router, &name, state_value).await;
            Declared {
                monitors: vec![json!({ "AlarmArn": arn, "AlarmRoleArn": assumable })],
                held: vec![(name, state_value)],
                unreadable_role: None,
            }
        }
        Monitors::OneWithoutARole(state_value) => {
            let name = format!("alarm-{index}");
            let arn = given_alarm(router, &name, state_value).await;
            Declared {
                monitors: vec![json!({ "AlarmArn": arn })],
                held: vec![(name, state_value)],
                unreadable_role: None,
            }
        }
        Monitors::OneUnderAnUnassumableRole(state_value) => {
            let name = format!("alarm-{index}");
            let arn = given_alarm(router, &name, state_value).await;
            Declared {
                monitors: vec![json!({ "AlarmArn": arn, "AlarmRoleArn": unassumable })],
                held: vec![(name, state_value)],
                unreadable_role: Some(unassumable.to_string()),
            }
        }
        Monitors::Two(first, second) => {
            let one_name = format!("alarm-{index}-a");
            let two_name = format!("alarm-{index}-b");
            let one = given_alarm(router, &one_name, first).await;
            let two = given_alarm(router, &two_name, second).await;
            Declared {
                monitors: vec![
                    json!({ "AlarmArn": one, "AlarmRoleArn": assumable }),
                    json!({ "AlarmArn": two, "AlarmRoleArn": assumable }),
                ],
                held: vec![(one_name, first), (two_name, second)],
                unreadable_role: None,
            }
        }
        Monitors::OneOnAnUnheldAlarm => Declared {
            monitors: vec![json!({ "AlarmArn": UNHELD_ALARM, "AlarmRoleArn": assumable })],
            held: Vec::new(),
            unreadable_role: None,
        },
    }
}

/// The ARN of the alarm a row expects the rollback to be attributed to: the LAST monitor of
/// the row, which is the one its table forces into `ALARM`.
fn alarming_arn(monitors: &[Value]) -> String {
    monitors
        .last()
        .and_then(|monitor| monitor["AlarmArn"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// A monitored deployment settles on what its environment's monitors show: it completes when
/// they show nothing, and AppConfig rolls it back when one of them is alarming or cannot be
/// read at all.
#[tokio::test]
async fn a_monitored_deployment_settles_on_what_its_monitors_show() {
    let (router, engine) = emulator();
    let assumable = given_role(&router, "appconfig-monitor", APPCONFIG_PRINCIPAL).await;
    let unassumable = given_role(&router, "not-appconfig-monitor", OTHER_PRINCIPAL).await;
    let application = create_application(&router, "monitored-deployments").await;
    let profile = create_profile(&router, &application, "monitored").await;
    let version = host(&router, &application, &profile, br#"{"enabled":true}"#).await;
    let strategy = given_strategy(&router, "monitored-bake").await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let declared = monitors_of(&router, index, row, &assumable, &unassumable).await;
        // Its own environment per row: an environment takes one deployment at a time.
        let environment = environment_with_monitors(
            &router,
            &application,
            &format!("monitored-{index}"),
            &declared.monitors,
        )
        .await;
        let number = start(
            &router, case, &application, &environment, &profile, &strategy, &version,
        )
        .await;

        // A row that must COMPLETE has to get past the bake window the caller declared, and
        // that window keeps its declared minutes: the row drives the clock it is owed by.
        // A row that must roll back is owed its rollback INSIDE that window, so it waits for
        // the condition instead.
        if row.outcome == Outcome::Completes {
            bring_the_window_forward(&engine, &application, &environment);
        }
        let settled = settled_with(
            &router,
            case,
            &application,
            &environment,
            number,
            &declared.held,
        )
        .await;
        let state = member(&settled, "State").as_str().unwrap_or_default().to_string();
        let log = event_types(case, &settled);

        if row.outcome == Outcome::Completes {
            assert_eq!(
                state, "COMPLETE",
                "case {case:?}: nothing its monitors show rolls this deployment back, so it \
                 completes, got {state:?} with log {log:?}"
            );
            assert!(
                log.iter().any(|event| event == DEPLOYMENT_COMPLETED),
                "case {case:?}: a completed deployment logs its completion, got {log:?}"
            );
            assert!(
                !log.iter().any(|event| event == ROLLBACK_STARTED),
                "case {case:?}: nothing rolled this deployment back, got {log:?}"
            );
            let environment_now =
                environment_state(&router, case, &application, &environment).await;
            assert_eq!(
                environment_now, "ReadyForDeployment",
                "case {case:?}: an environment whose deployment completed takes another, got \
                 {environment_now:?}"
            );
            continue;
        }

        assert_eq!(
            state, "ROLLED_BACK",
            "case {case:?}: AppConfig rolls a deployment its monitors condemn back, got \
             {state:?} with log {log:?}"
        );
        assert!(
            !log.iter().any(|event| event == DEPLOYMENT_COMPLETED),
            "case {case:?}: a deployment AppConfig rolled back never completed, got {log:?}"
        );
        assert_eq!(
            member(&settled, "PercentageComplete").as_f64(),
            Some(0.0),
            "case {case:?}: a rolled-back deployment is available to no target, got {settled}"
        );
        assert!(
            member(&settled, "CompletedAt").is_null(),
            "case {case:?}: a deployment AppConfig rolled back reports no completion instant, \
             got {settled}"
        );

        // Both events of the rollback name what caused it: a client reading the log at either
        // end has to be able to tell an alarm's rollback from an unreadable monitor's.
        let (expected_triggered_by, expected_description) = match row.outcome {
            Outcome::RolledBackByTheRole => {
                let role = declared
                    .unreadable_role
                    .clone()
                    .unwrap_or_else(|| panic!("case {case:?}: the row names an unreadable role"));
                (
                    "INTERNAL_ERROR",
                    format!("Unable to assume environment monitor role: {role}"),
                )
            }
            _ => ("CLOUDWATCH_ALARM", alarming_arn(&declared.monitors)),
        };
        for event_type in [ROLLBACK_STARTED, ROLLBACK_COMPLETED] {
            let event = event_of(case, &settled, event_type);
            assert_eq!(
                event["Description"].as_str(),
                Some(expected_description.as_str()),
                "case {case:?}: the {event_type} Description must name what caused the \
                 rollback, got {event}"
            );
        }
        let started_event = event_of(case, &settled, ROLLBACK_STARTED);
        assert_eq!(
            started_event["TriggeredBy"].as_str(),
            Some(expected_triggered_by),
            "case {case:?}: the rollback is attributed to what asked for it, got {started_event}"
        );

        let environment_now = environment_state(&router, case, &application, &environment).await;
        assert_eq!(
            environment_now, "RolledBack",
            "case {case:?}: an environment whose deployment was rolled back reports it, got \
             {environment_now:?}"
        );
    }
}

/// The aftermath of an alarm-driven rollback: the version that preceded the condemned
/// deployment is what the data plane serves again, and the condemned environment takes a
/// further deployment.
#[tokio::test]
async fn an_alarm_driven_rollback_restores_the_previous_configuration_version() {
    let (router, engine) = emulator();
    let assumable = given_role(&router, "appconfig-monitor", APPCONFIG_PRINCIPAL).await;
    let application = create_application(&router, "monitor-rollback-aftermath").await;
    let profile = create_profile(&router, &application, "two-versions").await;
    let first = host(&router, &application, &profile, br#"{"enabled":false}"#).await;
    let second = host(&router, &application, &profile, br#"{"enabled":true}"#).await;
    let strategy = given_strategy(&router, "aftermath-bake").await;

    // The alarm starts OK, so the first deployment is the positive control: it completes and
    // its bytes go into service.
    let alarm = given_alarm(&router, "aftermath-alarm", STATE_OK).await;
    let environment = environment_with_monitors(
        &router,
        &application,
        "aftermath",
        &[json!({ "AlarmArn": alarm, "AlarmRoleArn": assumable })],
    )
    .await;

    let case = "the version that preceded the rollback";
    let one = start(
        &router, case, &application, &environment, &profile, &strategy, &first,
    )
    .await;
    bring_the_window_forward(&engine, &application, &environment);
    let held_ok = [("aftermath-alarm".to_string(), STATE_OK)];
    let completed = settled_with(
        &router,
        case,
        &application,
        &environment,
        one,
        &held_ok,
    )
    .await;
    assert_eq!(
        member(&completed, "State").as_str(),
        Some("COMPLETE"),
        "case {case:?}: the control deployment must complete, got {completed}"
    );
    assert_eq!(
        served(&router, case, &application, &environment, &profile).await,
        (br#"{"enabled":false}"#.to_vec(), first.clone()),
        "case {case:?}: the control deployment's bytes must be in service"
    );

    // The second deployment is then condemned by the same alarm going into ALARM.
    let two = start(
        &router, case, &application, &environment, &profile, &strategy, &second,
    )
    .await;
    let held_alarming = [("aftermath-alarm".to_string(), STATE_ALARM)];
    let rolled_back = settled_with(
        &router,
        case,
        &application,
        &environment,
        two,
        &held_alarming,
    )
    .await;
    assert_eq!(
        member(&rolled_back, "State").as_str(),
        Some("ROLLED_BACK"),
        "case {case:?}: the alarm must roll the second deployment back, got {rolled_back}"
    );
    assert_eq!(
        served(&router, case, &application, &environment, &profile).await,
        (br#"{"enabled":false}"#.to_vec(), first.clone()),
        "case {case:?}: the rollback must put the previous version back in service"
    );

    // And the environment is free again: a rolled-back deployment is one AppConfig has
    // finished with, so it blocks no further one. The alarm is put back to OK first, so the
    // further deployment is not condemned on arrival.
    hold_alarms(&router, &held_ok).await;
    let three = start(
        &router, case, &application, &environment, &profile, &strategy, &second,
    )
    .await;
    assert!(
        three > two,
        "case {case:?}: a further deployment must be accepted after the rollback, got {three}"
    );
}

/// The bytes and version the data plane serves one client of this configuration.
async fn served(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    profile: &str,
) -> (Vec<u8>, String) {
    let answer = json_call(
        router,
        "GET",
        &format!(
            "/applications/{application}/environments/{environment}/configurations/{profile}\
             ?client_id=monitor-suite"
        ),
        &Value::Null,
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: GetConfiguration must answer 200, got {} / {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    let version = answer
        .header("configuration-version")
        .unwrap_or_else(|| panic!("case {case:?}: GetConfiguration answered no version header"));
    (answer.body.to_vec(), version)
}
