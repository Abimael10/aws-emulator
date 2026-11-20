// SPDX-License-Identifier: CC0-1.0

//! A CodeDeploy deployment of the `Lambda` compute platform really MOVES the alias
//! its revision's AppSpec names, in the service that owns that alias.
//!
//! The rule under test: **a deployment whose application runs on the `Lambda`
//! compute platform shifts traffic as the AppSpec its revision carries declares —
//! so a deployment that reaches `Succeeded` has repointed the named alias of the
//! named function at the AppSpec's `TargetVersion` with no weighted routing left
//! behind, and a deployment an alarm of its effective `alarmConfiguration` stops
//! mid-shift (`Stopped`, `errorInformation.code` `ALARM_ACTIVE`) has the alias put
//! back on the `CurrentVersion` it was shifting from by the rollback its
//! `DEPLOYMENT_STOP_ON_ALARM` configuration asks for.**
//!
//! Three crates have to agree for that to hold — CodeDeploy decides when each
//! increment falls due and whether an alarm stops the deployment, CloudWatch owns
//! the alarm's state, and AWS Lambda owns the alias the shift writes to and
//! `GetAlias` reads — so the suite lives here, with the three mounted on one engine
//! the way `core/router/src/lib.rs` mounts them, and drives each through its own
//! front door: the function, its versions and its aliases through Lambda's
//! `restJson1` API, the alarm through CloudWatch, the deployment through
//! CodeDeploy's JSON envelope. Nothing reaches into any service's store.
//!
//! The rows of the first table differ along what the rule generalises over: which
//! version is targeted, which direction the shift goes, how many increments the
//! effective `trafficRoutingConfig` shifts in (a single cutover, a canary, a linear
//! ladder), what the alias is called, and which of the two formats the model says
//! the AppSpec content is written in. They share ONE function, each row moving its
//! own alias of it, so the table costs one function's transitions rather than one
//! per row. Every expected value is computed from what the row declares.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for; the credential scope is what routes
/// a request to a service and what each handler reads its Region from.
const REGION: &str = "us-east-1";

/// A service-role ARN of the form `CreateDeploymentGroup.serviceRoleArn` takes.
/// CodeDeploy stores it; no IAM is mounted here, so nothing resolves it.
const SERVICE_ROLE_ARN: &str = "arn:aws:iam::123456789012:role/CodeDeployServiceRole";

/// The built-in deployment configuration that shifts all of a Lambda alias's
/// traffic in one step.
const ALL_AT_ONCE: &str = "CodeDeployDefault.LambdaAllAtOnce";

/// How long anything this suite waits for is given, and how often it looks again.
///
/// Generous: every wait here is for a CONDITION (a function reported `Active`, an
/// increment on the alias, a deployment terminal), never for a span, so the
/// deadline only has to sit far above the spans the emulator chooses on any host —
/// and above the one configured interval a row below walks out, since
/// `canaryInterval` is minutes the caller asked for.
const DEADLINE: Duration = Duration::from_secs(240);
/// See [`DEADLINE`].
const POLL: Duration = Duration::from_millis(25);

/// The `ErrorCode` a deployment its alarms stopped reports
/// (`smithy_query(query="shape",shape="ErrorCode")` carries it).
const ALARM_ACTIVE: &str = "ALARM_ACTIVE";

/// The `DeploymentCreator` of a deployment CodeDeploy created to roll another one
/// back (`smithy_query(query="shape",shape="DeploymentCreator")`).
const CREATOR_ROLLBACK: &str = "codeDeployRollback";

/// The `canaryPercentage` the alarm test's deployment group shifts its first
/// increment of traffic in, which is also the weight that increment leaves on the
/// alias.
const CANARY_PERCENT: i32 = 10;

/// What one alias reports at an instant: the version it invokes, and the weighted
/// routing it splits its invocations by, sorted by version.
type AliasReading = (String, Vec<(String, f64)>);

/// The emulator with the three services this suite exercises, on ONE engine.
///
/// Request authentication is off, as in every other suite here; the in-process
/// calls CodeDeploy makes to the other two are signed either way, and the
/// credential scope is what routes them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_codedeploy_codedeploy_20141006::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in
/// [`REGION`]. The signature is never checked: this router runs with request
/// authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// Call one CodeDeploy operation over the awsJson1_1 wire its model declares.
async fn codedeploy(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("codedeploy.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("CodeDeploy_20141006.{operation}"))
        .header("authorization", authorization("codedeploy"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// The same, requiring success and answering the response body.
async fn codedeploy_ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, payload) = codedeploy(router, operation, body).await;
    assert_eq!(
        status, 200,
        "codedeploy {operation} must succeed, got {status} with {payload:?}"
    );
    payload
}

/// Call one AWS Lambda operation over the `restJson1` wire its model declares,
/// addressed by method and URI.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &Value,
) -> (u16, Value) {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, payload)
}

/// The same, requiring a success status and answering the response body.
async fn lambda_ok(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    uri: &str,
    body: &Value,
) -> Value {
    let (status, payload) = lambda(router, method, uri, body).await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: expected success from {method} {uri}, got {status} with {payload:?}"
    );
    payload
}

/// Call one CloudWatch operation and require success.
///
/// CloudWatch's model here declares `awsJson1_0`
/// (`smithy_query(query="service_info")`), so the envelope and the target prefix
/// are that service's own.
async fn cloudwatch_ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("monitoring.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("GraniteServiceVersion20100801.{operation}"),
        )
        .header("authorization", authorization("monitoring"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "cloudwatch {operation} must succeed, got {status} with body {answered}"
    );
    serde_json::from_slice(response.body()).unwrap_or(Value::Null)
}

/// Create `function` and publish `versions` versions of it.
///
/// Each publish carries its own `Description`: Lambda does not publish a version
/// whose configuration and code match the last one, so a distinct description is
/// what makes each call its own snapshot with its own number. Nothing here invokes
/// the function, so what the package holds is immaterial — only the VERSIONS an
/// AppSpec may name are.
async fn given_function(router: &MultiServiceRouter, case: &str, function: &str, versions: usize) {
    lambda_ok(
        router,
        case,
        "POST",
        "/2015-03-31/functions",
        &json!({
            "FunctionName": function,
            "Role": format!("arn:aws:iam::{}:role/handler", engine::emulator_account_id()),
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Code": {"ZipFile": "cGFja2FnZQ=="},
        }),
    )
    .await;
    // A freshly created function is owed the transition to `Active` before a
    // version of it can be published, so this waits for that report.
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = lambda_ok(
            router,
            case,
            "GET",
            &format!("/2015-03-31/functions/{function}/configuration"),
            &Value::Null,
        )
        .await;
        if reported["State"].as_str() == Some("Active") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: Lambda never reported {function:?} Active, last {reported:?}"
        );
        tokio::time::sleep(POLL).await;
    }
    for version in 1..=versions {
        let published = lambda_ok(
            router,
            case,
            "POST",
            &format!("/2015-03-31/functions/{function}/versions"),
            &json!({"Description": format!("snapshot {version}")}),
        )
        .await;
        assert_eq!(
            published["Version"].as_str(),
            Some(version.to_string().as_str()),
            "case {case:?}: PublishVersion must publish version {version}, got {published:?}"
        );
    }
}

/// Create the alias the deployment moves, invoking `version`.
async fn given_alias(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    alias: &str,
    version: &str,
) {
    lambda_ok(
        router,
        case,
        "POST",
        &format!("/2015-03-31/functions/{function}/aliases"),
        &json!({"Name": alias, "FunctionVersion": version}),
    )
    .await;
}

/// What the alias of `function` reports right now, as a client reads it.
async fn alias_reading(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    alias: &str,
) -> AliasReading {
    let reported = lambda_ok(
        router,
        case,
        "GET",
        &format!("/2015-03-31/functions/{function}/aliases/{alias}"),
        &Value::Null,
    )
    .await;
    let version = reported["FunctionVersion"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let mut weights: Vec<(String, f64)> = reported["RoutingConfig"]["AdditionalVersionWeights"]
        .as_object()
        .map(|weights| {
            weights
                .iter()
                .map(|(version, weight)| (version.clone(), weight.as_f64().unwrap_or_default()))
                .collect()
        })
        .unwrap_or_default();
    weights.sort_by(|left, right| left.0.cmp(&right.0));
    (version, weights)
}

/// An AppSpec declaring the shift of `alias` on `function`, in the spelling the
/// AWS CodeDeploy User Guide writes one in.
fn app_spec_yaml(function: &str, alias: &str, current: &str, target: &str) -> String {
    format!(
        "version: 0.0\n\
         Resources:\n\
         \x20 - {function}:\n\
         \x20     Type: AWS::Lambda::Function\n\
         \x20     Properties:\n\
         \x20       Name: {function}\n\
         \x20       Alias: {alias}\n\
         \x20       CurrentVersion: \"{current}\"\n\
         \x20       TargetVersion: \"{target}\"\n"
    )
}

/// The same AppSpec in its other spelling: "the content is formatted as JSON or
/// YAML stored as a RawString"
/// (`smithy_query(query="shape",shape="RevisionLocation",member="appSpecContent")`).
fn app_spec_json(function: &str, alias: &str, current: &str, target: &str) -> String {
    json!({
        "version": 0.0,
        "Resources": [{
            function: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Name": function,
                    "Alias": alias,
                    "CurrentVersion": current,
                    "TargetVersion": target,
                },
            },
        }],
    })
    .to_string()
}

/// An `appSpecContent` revision carrying `content`.
fn app_spec_revision(content: &str) -> Value {
    json!({"revisionType": "AppSpecContent", "appSpecContent": {"content": content}})
}

/// A `trafficRoutingConfig` of `kind` shifting `percentage` of the traffic every
/// `interval` minutes.
///
/// `kind` is one of the two `TrafficRoutingType` members that shift in increments
/// over time (`smithy_query(query="shape",shape="TrafficRoutingType"`: TimeBasedCanary
/// | TimeBasedLinear | AllAtOnce); anything else is a mistake in a row rather than a
/// configuration to send, so it panics instead of being read as one of them.
fn traffic_routing(kind: &str, percentage: i32, interval: i32) -> Value {
    match kind {
        "TimeBasedCanary" => json!({
            "type": kind,
            "timeBasedCanary": {"canaryPercentage": percentage, "canaryInterval": interval},
        }),
        "TimeBasedLinear" => json!({
            "type": kind,
            "timeBasedLinear": {"linearPercentage": percentage, "linearInterval": interval},
        }),
        other => panic!("a row named the traffic routing type {other:?}, which shifts no increments"),
    }
}

/// Create an `Lambda` application named `application`, so deployment groups can be
/// made under it.
async fn given_application(router: &MultiServiceRouter, application: &str) {
    codedeploy_ok(
        router,
        "CreateApplication",
        &json!({"applicationName": application, "computePlatform": "Lambda"}),
    )
    .await;
}

/// Create one deployment group named `group` of `application`, under a deployment
/// configuration carrying `routing` (or the built-in that shifts all of the traffic
/// at once) and with `extras` merged into the group's own request. Answers the
/// configuration's name, which a deployment may also name for itself.
async fn given_lambda_group(
    router: &MultiServiceRouter,
    application: &str,
    group: &str,
    routing: Option<Value>,
    extras: &Value,
) -> String {
    let config = match routing {
        Some(routing) => {
            let name = format!("{group}-config");
            codedeploy_ok(
                router,
                "CreateDeploymentConfig",
                &json!({
                    "deploymentConfigName": name,
                    "computePlatform": "Lambda",
                    "trafficRoutingConfig": routing,
                }),
            )
            .await;
            name
        }
        None => ALL_AT_ONCE.to_string(),
    };
    let mut request = json!({
        "applicationName": application,
        "deploymentGroupName": group,
        "serviceRoleArn": SERVICE_ROLE_ARN,
        "deploymentConfigName": config,
    });
    for (member, value) in extras.as_object().cloned().unwrap_or_default() {
        request[member] = value;
    }
    codedeploy_ok(router, "CreateDeploymentGroup", &request).await;
    config
}

/// Create a deployment of `revision`, under `config` where the row names one of its
/// own rather than taking the group's, and answer the id CodeDeploy minted.
async fn create_deployment(
    router: &MultiServiceRouter,
    group: (&str, &str),
    config: Option<&str>,
    revision: &Value,
) -> String {
    let (application, group) = group;
    let mut request = json!({
        "applicationName": application,
        "deploymentGroupName": group,
        "revision": revision,
    });
    if let Some(config) = config {
        request["deploymentConfigName"] = json!(config);
    }
    let created = codedeploy_ok(router, "CreateDeployment", &request).await;
    created["deploymentId"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Poll `GetDeployment` until `deployment` holds a terminal status, answering that
/// `deploymentInfo`.
///
/// The terminal statuses are the members of the model's `DeploymentStatus` enum
/// that no further transition leaves
/// (`smithy_query(query="shape",shape="DeploymentStatus")`).
async fn terminal_deployment(router: &MultiServiceRouter, deployment: &str, case: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let info = codedeploy_ok(router, "GetDeployment", &json!({"deploymentId": deployment}))
            .await["deploymentInfo"]
            .clone();
        if matches!(
            info["status"].as_str(),
            Some("Succeeded" | "Failed" | "Stopped")
        ) {
            return info;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: deployment {deployment} reached no terminal status, last {info:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Wait until the alias of `function` reads `expected`.
///
/// `while_running` names the deployment whose shift is expected to produce that
/// reading, for a reading only a deployment still in flight holds: a deployment
/// that reached an outcome owes the alias nothing further, so the wait says so at
/// once rather than running down the deadline.
async fn alias_reaches(
    router: &MultiServiceRouter,
    case: &str,
    function: (&str, &str),
    while_running: Option<&str>,
    expected: &AliasReading,
) {
    let (function, alias) = function;
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reading = alias_reading(router, case, function, alias).await;
        if reading == *expected {
            return;
        }
        if let Some(deployment) = while_running {
            let info =
                codedeploy_ok(router, "GetDeployment", &json!({"deploymentId": deployment}))
                    .await["deploymentInfo"]
                    .clone();
            assert!(
                !matches!(
                    info["status"].as_str(),
                    Some("Succeeded" | "Failed" | "Stopped")
                ),
                "case {case:?}: deployment {deployment} reached {:?} before the alias {alias} of \
                 {function} ever read {expected:?}, last {reading:?}",
                info["status"]
            );
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the alias {alias} of {function} to reach {expected:?}, \
             last {reading:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// One row of the shift table: the shift its AppSpec declares, and how it travels.
struct Shift {
    /// What the row is about.
    case: &'static str,
    /// The version its own alias invokes, and the AppSpec's `CurrentVersion`.
    current: &'static str,
    /// The AppSpec's `TargetVersion`, which is where that alias has to end up.
    target: &'static str,
    /// What the row's alias of the shared function is called.
    alias: &'static str,
    /// The `trafficRoutingConfig` the deployment's configuration carries, or
    /// `None` for the built-in that shifts all of the traffic at once.
    routing: Option<(&'static str, i32, i32)>,
    /// Whether the AppSpec is written as JSON rather than as YAML.
    json: bool,
}

/// RULE: a Lambda-platform deployment that reaches `Succeeded` has carried out the
/// traffic shift its revision's AppSpec declares — the named alias of the named
/// function invokes the AppSpec's `TargetVersion` and splits its invocations with
/// no other version — whatever the shift's direction, the increments it is walked
/// in, the alias's name or the format the AppSpec arrives in.
#[tokio::test]
async fn a_succeeded_lambda_deployment_leaves_the_alias_on_the_target_version() {
    let cases: &[Shift] = &[
        Shift {
            case: "the next version, shifted in one step",
            current: "1",
            target: "2",
            alias: "live",
            routing: None,
            json: false,
        },
        Shift {
            case: "two versions ahead, under a 10% canary",
            current: "1",
            target: "3",
            alias: "canary",
            routing: Some(("TimeBasedCanary", 10, 1)),
            json: false,
        },
        Shift {
            case: "back to an earlier version, under a 50% linear shift",
            current: "3",
            target: "1",
            alias: "prod",
            // No wait between the increments (`canaryInterval`/`linearInterval`
            // have a published minimum of 0 minutes): this row is about where the
            // alias ENDS once the deployment succeeded, and the window a row that
            // states an interval holds open is the CodeDeploy crate's own suite's
            // (`tests/all/lambda_traffic_shifting.rs`), so only the row above
            // walks one out here.
            routing: Some(("TimeBasedLinear", 50, 0)),
            json: false,
        },
        Shift {
            case: "a JSON AppSpec and a hyphenated alias",
            current: "1",
            target: "2",
            alias: "blue-green",
            routing: None,
            json: true,
        },
        Shift {
            case: "the newest of four versions, under a 75% canary",
            current: "2",
            target: "4",
            alias: "stage",
            routing: Some(("TimeBasedCanary", 75, 0)),
            json: false,
        },
    ];

    let router = build_router();
    let function = "shifted";
    let application = "shift-app";
    // One function, four published versions and one application for the whole
    // table: each row moves its OWN alias of that function, under its own
    // deployment group, so a row's shift is independent of every other row's.
    given_function(&router, "the table's function", function, 4).await;
    given_application(&router, application).await;

    for (index, row) in cases.iter().enumerate() {
        let case = row.case;
        let group = format!("shift-dg-{index}");
        given_alias(&router, case, function, row.alias, row.current).await;
        given_lambda_group(
            &router,
            application,
            &group,
            row.routing
                .map(|(kind, percentage, interval)| traffic_routing(kind, percentage, interval)),
            &Value::Null,
        )
        .await;

        let content = if row.json {
            app_spec_json(function, row.alias, row.current, row.target)
        } else {
            app_spec_yaml(function, row.alias, row.current, row.target)
        };
        let deployment = create_deployment(
            &router,
            (application, &group),
            None,
            &app_spec_revision(&content),
        )
        .await;
        let info = terminal_deployment(&router, &deployment, case).await;
        assert_eq!(
            info["status"].as_str(),
            Some("Succeeded"),
            "case {case:?}: a shift nothing stopped must succeed, got {info:?}"
        );

        let reading = alias_reading(&router, case, function, row.alias).await;
        assert_eq!(
            reading,
            (row.target.to_string(), Vec::new()),
            "case {case:?}: expected the alias on the AppSpec's TargetVersion {} with no \
             weighted routing left, got {reading:?}",
            row.target
        );
    }
}

/// RULE: an alarm of a deployment's effective `alarmConfiguration` that is sounding
/// while its traffic-shift window is open stops that deployment — `Stopped`, with
/// `errorInformation.code` `ALARM_ACTIVE` and a message naming the alarm — and the
/// rollback its `DEPLOYMENT_STOP_ON_ALARM` configuration asks for puts the alias
/// back on the version the stopped deployment was shifting FROM; an alarm that
/// stays quiet stops nothing, and that deployment leaves the alias on its target
/// version.
#[tokio::test]
async fn an_alarm_sounding_mid_shift_stops_the_deployment_and_rolls_the_alias_back() {
    let router = build_router();
    let function = "gated";
    let application = "gate-app";
    let group = "gate-dg";
    let alarm = "gate-alarm";
    let alias = "live";
    let quiet = "the alarm stays quiet";
    let breaching = "the alarm starts sounding mid-shift";

    given_function(&router, quiet, function, 3).await;
    given_alias(&router, quiet, function, alias, "1").await;
    cloudwatch_ok(
        &router,
        "PutMetricAlarm",
        &json!({
            "AlarmName": alarm,
            "Namespace": "Sim/CodeDeployShift",
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
    cloudwatch_ok(
        &router,
        "SetAlarmState",
        &json!({
            "AlarmName": alarm,
            "StateValue": "OK",
            "StateReason": "nothing has breached yet",
        }),
    )
    .await;
    given_application(&router, application).await;
    // The group's own configuration shifts in a canary increment held for a
    // window wide enough that the alarm is driven while it is open — what the
    // breaching leg is about is the increment the stop comes between, and nothing
    // here measures how long that window is.
    let canary = given_lambda_group(
        &router,
        application,
        group,
        Some(traffic_routing("TimeBasedCanary", CANARY_PERCENT, 10)),
        &json!({
            "alarmConfiguration": {"enabled": true, "alarms": [{"name": alarm}]},
            "autoRollbackConfiguration": {
                "enabled": true,
                "events": ["DEPLOYMENT_STOP_ON_ALARM"],
            },
        }),
    )
    .await;

    // The quiet leg: the group watches the alarm, the alarm is in `OK`, and the
    // deployment completes and leaves the alias on the version it targeted. It
    // names the built-in configuration for ITSELF — the effective configuration is
    // the deployment's where it states one — so the leg that is not about a window
    // does not wait for one.
    let control = create_deployment(
        &router,
        (application, group),
        Some(ALL_AT_ONCE),
        &app_spec_revision(&app_spec_yaml(function, alias, "1", "2")),
    )
    .await;
    let control_info = terminal_deployment(&router, &control, quiet).await;
    assert_eq!(
        control_info["status"].as_str(),
        Some("Succeeded"),
        "case {quiet:?}: a quiet alarm stops nothing, got {control_info:?}"
    );
    let reading = alias_reading(&router, quiet, function, alias).await;
    assert_eq!(
        reading,
        ("2".to_string(), Vec::new()),
        "case {quiet:?}: the completed shift must leave the alias on version 2, got {reading:?}"
    );

    // The breaching leg, 2 -> 3 under the group's canary: the alarm is driven once
    // that increment is on the alias, which is the window being open.
    let shifting = create_deployment(
        &router,
        (application, group),
        Some(&canary),
        &app_spec_revision(&app_spec_yaml(function, alias, "2", "3")),
    )
    .await;
    // `AdditionalVersionWeights` carries the share as a fraction between 0 and 1,
    // so the increment the group's canary percentage describes is that percentage
    // of the whole.
    let canary_weight = f64::from(CANARY_PERCENT) / 100.0;
    alias_reaches(
        &router,
        breaching,
        (function, alias),
        Some(&shifting),
        &("2".to_string(), vec![("3".to_string(), canary_weight)]),
    )
    .await;
    cloudwatch_ok(
        &router,
        "SetAlarmState",
        &json!({
            "AlarmName": alarm,
            "StateValue": "ALARM",
            "StateReason": "the breach under test",
        }),
    )
    .await;

    let info = terminal_deployment(&router, &shifting, breaching).await;
    assert_eq!(
        (
            info["status"].as_str(),
            info["errorInformation"]["code"].as_str()
        ),
        (Some("Stopped"), Some(ALARM_ACTIVE)),
        "case {breaching:?}: a sounding alarm must stop the deployment, got {info:?}"
    );
    assert!(
        info["errorInformation"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains(alarm),
        "case {breaching:?}: the stop must name the alarm that activated, got {:?}",
        info["errorInformation"]["message"]
    );
    let rollback = info["rollbackInfo"]["rollbackDeploymentId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !rollback.is_empty(),
        "case {breaching:?}: a stop that rolls back names its rollback deployment, got {info:?}"
    );
    let rollback_info = terminal_deployment(&router, &rollback, breaching).await;
    assert_eq!(
        (
            rollback_info["creator"].as_str(),
            rollback_info["rollbackInfo"]["rollbackTriggeringDeploymentId"].as_str()
        ),
        (Some(CREATOR_ROLLBACK), Some(shifting.as_str())),
        "case {breaching:?}: the rollback is CodeDeploy's own and points back at the stopped \
         deployment, got {rollback_info:?}"
    );

    // And the alias is back on the version the stopped deployment was shifting
    // from, with the weighted routing its increment had added cleared.
    alias_reaches(
        &router,
        breaching,
        (function, alias),
        None,
        &("2".to_string(), Vec::new()),
    )
    .await;
}
