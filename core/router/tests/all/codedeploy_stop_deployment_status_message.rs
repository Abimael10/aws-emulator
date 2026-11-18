// SPDX-License-Identifier: CC0-1.0

//! The `statusMessage` an ACCEPTED CodeDeploy `StopDeployment` answers with, driven over the
//! assembled `MultiServiceRouter` with CodeDeploy and AWS Lambda mounted on one engine.
//!
//! The RULE asserted here: **every accepted `StopDeployment` answers HTTP 200 with the
//! `StopStatus` for a stop that has taken effect and CodeDeploy's one FIXED accepted-stop
//! sentence as `statusMessage` — the same text whatever that stop did, interpolating nothing
//! from the request or from the service's state.** A rollback the stop performed is reported
//! where a rollback is reported, on the deployment's own `rollbackInfo.rollbackMessage`, and
//! never in place of that sentence. The model documents `statusMessage` only as "An
//! accompanying status message"
//! (`smithy_query(query="doc",shape="StopDeploymentOutput",member="statusMessage")`), so the
//! sentence itself is recorded behavior; `StopStatus` is `Pending | Succeeded`
//! (`smithy_query(query="shape",shape="StopStatus")`), of which a stop that has already taken
//! effect is the second.
//!
//! The rows differ along what the rule generalises over: the compute platform the stopped
//! deployment runs on, how the request spells (or omits) `autoRollbackEnabled`, whether the
//! deployment group has a revision to roll back TO — the condition that decides which
//! rollback wording the deployment's `rollbackInfo` then carries — whether the deployment's
//! own `autoRollbackConfiguration` asks to be rolled back when a caller stops it, and whether
//! the stop is the first one or a repeat of one that already ended the deployment. The last
//! test adds the deployment that is still WALKING a time-based traffic shift and is rolled
//! back by the stop: two crates have to agree for that row to hold (CodeDeploy decides the
//! rollback, AWS Lambda holds the alias it moves), which is why it is driven here.
//!
//! These rows live in this crate rather than beside the CodeDeploy crate's own
//! `tests/all/stop_deployment.rs` because that crate still roots its integration tests at a
//! hand-declared `tests/all.rs`, which a fix may not edit; the router's `tests/all/` picks a
//! new suite up with nothing to declare.
//!
//! Every wait here polls for the CONDITION it is waiting on — a deployment group free to take
//! a deployment, a function Lambda reports `Active`, a traffic increment on the alias — so no
//! outcome depends on how fast the host runs.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The `StopStatus` an accepted stop that has taken effect answers with.
const STOP_STATUS: &str = "Succeeded";

/// CodeDeploy's accepted-stop sentence: entirely fixed text, no part of which is taken from
/// the request or from state.
const STOP_STATUS_MESSAGE: &str =
    "No more commands will be scheduled for execution in the deployment instances";

/// The `AutoRollbackEvent` that asks for a rollback when a caller STOPS the deployment
/// (`smithy_query(query="shape",shape="AutoRollbackEvent")`: DEPLOYMENT_FAILURE |
/// DEPLOYMENT_STOP_ON_ALARM | DEPLOYMENT_STOP_ON_REQUEST).
const STOP_ON_REQUEST: &str = "DEPLOYMENT_STOP_ON_REQUEST";

/// The service role every deployment group here is created with.
const SERVICE_ROLE_ARN: &str = "arn:aws:iam::123456789012:role/CodeDeployServiceRole";

/// The execution role every function here is created with.
const EXECUTION_ROLE: &str = "arn:aws:iam::123456789012:role/LambdaExecution";

/// The deployment package uploaded inline: nothing here invokes the function, so what the
/// archive holds is immaterial — only the versions it is published as are.
const ZIP_FILE: &str = "cGFja2FnZQ==";

/// The alias the traffic-shift row moves.
const ALIAS: &str = "live";

/// How long any one condition here is given before the suite gives up: far longer than any
/// clock-owed span on this path, so a loaded host cannot decide the outcome.
const DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between looks.
const POLL: Duration = Duration::from_millis(25);

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
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
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`], which
/// is what routes the request. The signature is never checked: this router runs with request
/// authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
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
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// Call one CodeDeploy operation, requiring HTTP 200.
async fn codedeploy_ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, answered) = codedeploy(router, operation, body).await;
    assert_eq!(
        status, 200,
        "setup: {operation} must succeed, got {status} with {answered}"
    );
    answered
}

/// Call one AWS Lambda operation over its restJson1 wire.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &Value,
) -> (u16, Value) {
    let payload = match body {
        Value::Null => Bytes::new(),
        body => Bytes::from(serde_json::to_vec(body).expect("the request body serializes")),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// Create an application and one deployment group on it, answering the group's name.
async fn given_group(
    router: &MultiServiceRouter,
    application: &str,
    platform: &str,
    deployment_config: Option<&str>,
) -> String {
    codedeploy_ok(
        router,
        "CreateApplication",
        &json!({"applicationName": application, "computePlatform": platform}),
    )
    .await;
    let group = format!("{application}-dg");
    let mut request = json!({
        "applicationName": application,
        "deploymentGroupName": group,
        "serviceRoleArn": SERVICE_ROLE_ARN,
    });
    if let Some(config) = deployment_config {
        request["deploymentConfigName"] = json!(config);
    }
    codedeploy_ok(router, "CreateDeploymentGroup", &request).await;
    group
}

/// Create a deployment, waiting out the refusal a deployment group that is already running
/// one answers with, and answer the id CodeDeploy minted.
///
/// A deployment group runs ONE deployment at a time, so a second `CreateDeployment` against
/// it is refused with `DeploymentLimitExceededException` until the first is terminal — which a
/// client waits out, and so does this.
async fn deployment_on_free_group(
    router: &MultiServiceRouter,
    request: &Value,
    case: &str,
) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, answered) = codedeploy(router, "CreateDeployment", request).await;
        if status == 200 {
            return answered["deploymentId"]
                .as_str()
                .unwrap_or_default()
                .to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the deployment group to take a deployment within \
             {DEADLINE:?}, last {status} with {answered}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The stored `deploymentInfo` of `deployment_id`.
async fn deployment_info(router: &MultiServiceRouter, deployment_id: &str) -> Value {
    codedeploy_ok(
        router,
        "GetDeployment",
        &json!({"deploymentId": deployment_id}),
    )
    .await["deploymentInfo"]
        .clone()
}

/// Assert that `response` is an accepted stop: HTTP 200, the stop status, the fixed sentence,
/// and no part of `tokens` — the values the request and the state could have been
/// interpolated into it — anywhere inside that sentence.
fn assert_accepted_stop(case: &str, status: u16, response: &Value, tokens: &[&str]) {
    assert_eq!(
        (
            status,
            response["status"].as_str(),
            response["statusMessage"].as_str(),
        ),
        (200, Some(STOP_STATUS), Some(STOP_STATUS_MESSAGE)),
        "case {case:?}: expected HTTP 200 with the accepted-stop status and sentence, got \
         {status} with {response}"
    );
    let message = response["statusMessage"].as_str().unwrap_or_default();
    for token in tokens {
        assert!(
            !message.contains(token),
            "case {case:?}: expected the accepted-stop sentence to interpolate nothing, but it \
             carries {token:?}: {message:?}"
        );
    }
}

/// An `s3Location` revision, the form the AWS CLI's `--s3-location` sends.
fn s3_revision(key: &str) -> Value {
    json!({
        "revisionType": "S3",
        "s3Location": {"bucket": "stop-message", "key": key, "bundleType": "zip"},
    })
}

/// One row of the accept table.
struct Accept {
    /// What the row is about.
    case: &'static str,
    /// The compute platform the stopped deployment runs on.
    platform: &'static str,
    /// `autoRollbackEnabled` on the stop, absent when `None`.
    auto_rollback_enabled: Option<bool>,
    /// Whether the deployment group already held a deployment, which is what decides whether
    /// there is a revision to roll back to.
    previous_deployment: bool,
    /// Whether the stopped deployment carries an `autoRollbackConfiguration` asking for a
    /// rollback when a caller stops it.
    rolls_back_on_request: bool,
}

/// RULE: an accepted `StopDeployment` answers the fixed accepted-stop sentence whatever the
/// stop did — whether a rollback was asked for, whether one could be created, and whether the
/// deployment itself asked to be rolled back on a stop — and the rollback is reported in its
/// own words on the deployment's `rollbackInfo` instead.
#[tokio::test]
async fn every_accepted_stop_answers_the_fixed_status_message() {
    let cases: &[Accept] = &[
        Accept {
            case: "a stop that asks for no rollback",
            platform: "Server",
            auto_rollback_enabled: None,
            previous_deployment: false,
            rolls_back_on_request: false,
        },
        Accept {
            case: "a stop that declines a rollback",
            platform: "ECS",
            auto_rollback_enabled: Some(false),
            previous_deployment: true,
            rolls_back_on_request: false,
        },
        Accept {
            case: "a stop whose rollback has a revision to go back to",
            platform: "Server",
            auto_rollback_enabled: Some(true),
            previous_deployment: true,
            rolls_back_on_request: false,
        },
        Accept {
            case: "a stop whose rollback has no revision to go back to",
            platform: "Lambda",
            auto_rollback_enabled: Some(true),
            previous_deployment: false,
            rolls_back_on_request: false,
        },
        Accept {
            case: "a deployment configured to roll back on a stop request",
            platform: "Lambda",
            auto_rollback_enabled: Some(true),
            previous_deployment: false,
            rolls_back_on_request: true,
        },
    ];

    let router = build_router();
    for (index, case) in cases.iter().enumerate() {
        let name = case.case;
        let application = format!("stop-message-{index:02}");
        let group = given_group(&router, &application, case.platform, None).await;
        if case.previous_deployment {
            deployment_on_free_group(
                &router,
                &json!({
                    "applicationName": application,
                    "deploymentGroupName": group,
                    "revision": s3_revision("first.zip"),
                }),
                name,
            )
            .await;
        }
        let mut create = json!({
            "applicationName": application,
            "deploymentGroupName": group,
            "revision": s3_revision("second.zip"),
        });
        if case.rolls_back_on_request {
            create["autoRollbackConfiguration"] =
                json!({"enabled": true, "events": [STOP_ON_REQUEST]});
        }
        let deployment_id = deployment_on_free_group(&router, &create, name).await;

        let mut stop = json!({"deploymentId": deployment_id});
        if let Some(enabled) = case.auto_rollback_enabled {
            stop["autoRollbackEnabled"] = json!(enabled);
        }
        let (status, stopped) = codedeploy(&router, "StopDeployment", &stop).await;
        // The values a response that interpolated its state would carry: the deployment this
        // stop named, and the application and the group it ran on.
        let tokens = [deployment_id.as_str(), application.as_str(), group.as_str()];
        assert_accepted_stop(name, status, &stopped, &tokens);

        // The rollback the stop performed is reported on the deployment, which is the record
        // that carries a rollback's own wording — never in place of the accepted-stop
        // sentence.
        let info = deployment_info(&router, &deployment_id).await;
        if let Some(rollback_message) = info["rollbackInfo"]["rollbackMessage"].as_str() {
            assert_ne!(
                rollback_message, STOP_STATUS_MESSAGE,
                "case {name:?}: expected the rollback to be reported in its own words, got \
                 {info}"
            );
            assert!(
                rollback_message.contains(deployment_id.as_str()),
                "case {name:?}: expected the rollback wording to name the deployment it is \
                 about, got {rollback_message:?}"
            );
        }

        // A deployment a stop already ended is in the state the caller is asking for, so a
        // repeat stop is answered — with the same sentence.
        let (status, again) = codedeploy(&router, "StopDeployment", &stop).await;
        assert_accepted_stop(&format!("{name} (repeated)"), status, &again, &tokens);
    }
}

/// Create a function, wait until Lambda reports it `Active`, publish `versions` versions of
/// it and point the [`ALIAS`] alias at the first one.
async fn given_function(router: &MultiServiceRouter, name: &str, versions: usize) {
    let (status, created) = lambda(
        router,
        "POST",
        "/2015-03-31/functions",
        &json!({
            "FunctionName": name,
            "Role": EXECUTION_ROLE,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Code": {"ZipFile": ZIP_FILE},
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup: CreateFunction({name:?}) must succeed, got {status} with {created}"
    );

    let deadline = Instant::now() + DEADLINE;
    loop {
        let (_, reported) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            &Value::Null,
        )
        .await;
        if reported["State"].as_str() == Some("Active") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "setup: Lambda never reported {name:?} Active within {DEADLINE:?}, last {reported}"
        );
        tokio::time::sleep(POLL).await;
    }

    for version in 1..=versions {
        // A description per version: "Lambda doesn't publish a version if the function's
        // configuration and code haven't changed since the last version", and the description
        // is part of that configuration.
        let (status, published) = lambda(
            router,
            "POST",
            &format!("/2015-03-31/functions/{name}/versions"),
            &json!({"Description": format!("version {version}")}),
        )
        .await;
        assert_eq!(
            (status, published["Version"].as_str()),
            (201, Some(version.to_string().as_str())),
            "setup: PublishVersion({name:?}) must publish version {version}, got {status} with \
             {published}"
        );
    }
    let (status, alias) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{name}/aliases"),
        &json!({"Name": ALIAS, "FunctionVersion": "1"}),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup: CreateAlias({name:?}) must succeed, got {status} with {alias}"
    );
}

/// The weight the alias of `function` carries on `version` right now, if any: what a client
/// reads of a traffic shift in progress.
async fn alias_weight(router: &MultiServiceRouter, function: &str, version: &str) -> Option<f64> {
    let (status, read) = lambda(
        router,
        "GET",
        &format!("/2015-03-31/functions/{function}/aliases/{ALIAS}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: GetAlias({function:?}) must succeed, got {status} with {read}"
    );
    read["RoutingConfig"]["AdditionalVersionWeights"][version].as_f64()
}

/// An `appSpecContent` revision shifting the alias of `function` from version 1 to version 2,
/// in the AppSpec reference's own spelling.
fn appspec_revision(function: &str) -> Value {
    let content = format!(
        "version: 0.0\nResources:\n  - {function}:\n      Type: AWS::Lambda::Function\n      \
         Properties:\n        Name: {function}\n        Alias: {ALIAS}\n        \
         CurrentVersion: \"1\"\n        TargetVersion: \"2\"\n"
    );
    json!({"revisionType": "AppSpecContent", "appSpecContent": {"content": content}})
}

/// RULE: the same sentence answers a stop of a deployment that is still WALKING a time-based
/// traffic shift and is rolled back by that stop — the path on which a rollback is decided,
/// and so the path on which a rollback's own wording could be substituted for the
/// accepted-stop sentence.
#[tokio::test]
async fn stopping_a_deployment_mid_traffic_shift_answers_the_fixed_status_message() {
    let case = "a canary deployment stopped with a rollback asked for";
    let router = build_router();
    let function = "stop-message-handler";
    let application = "stop-message-canary";
    given_function(&router, function, 2).await;

    // A canary holds its first increment for the window its interval describes, which is what
    // keeps the deployment non-terminal while the stop is made; nothing here measures that
    // window.
    let config = format!("{application}-config");
    codedeploy_ok(
        &router,
        "CreateDeploymentConfig",
        &json!({
            "deploymentConfigName": config,
            "computePlatform": "Lambda",
            "trafficRoutingConfig": {
                "type": "TimeBasedCanary",
                "timeBasedCanary": {"canaryPercentage": 10, "canaryInterval": 5},
            },
        }),
    )
    .await;
    let group = given_group(&router, application, "Lambda", Some(&config)).await;
    let deployment_id = deployment_on_free_group(
        &router,
        &json!({
            "applicationName": application,
            "deploymentGroupName": group,
            "revision": appspec_revision(function),
            "autoRollbackConfiguration": {"enabled": true, "events": [STOP_ON_REQUEST]},
        }),
        case,
    )
    .await;

    // Wait for the deployment to have actually moved traffic: the canary increment on the
    // alias is what the stop then has to roll back.
    let deadline = Instant::now() + DEADLINE;
    loop {
        if alias_weight(&router, function, "2").await.is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the canary increment never reached the alias within {DEADLINE:?}"
        );
        tokio::time::sleep(POLL).await;
    }

    let (status, stopped) = codedeploy(
        &router,
        "StopDeployment",
        &json!({"deploymentId": deployment_id, "autoRollbackEnabled": true}),
    )
    .await;
    assert_accepted_stop(
        case,
        status,
        &stopped,
        &[deployment_id.as_str(), application, group.as_str(), function],
    );

    let info = deployment_info(&router, &deployment_id).await;
    assert_eq!(
        info["status"].as_str(),
        Some("Stopped"),
        "case {case:?}: expected the stop to have ended the deployment, got {info}"
    );
    assert_ne!(
        info["rollbackInfo"]["rollbackMessage"].as_str(),
        Some(STOP_STATUS_MESSAGE),
        "case {case:?}: expected the rollback to be reported in its own words, got {info}"
    );
}
