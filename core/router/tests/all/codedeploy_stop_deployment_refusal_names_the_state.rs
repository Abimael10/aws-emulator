// SPDX-License-Identifier: CC0-1.0

//! The SENTENCE a CodeDeploy `StopDeployment` is REFUSED with once its deployment has
//! already reached an outcome, driven over the assembled `MultiServiceRouter` with
//! CodeDeploy and AWS Lambda mounted on one engine.
//!
//! The RULE asserted here: **a `StopDeployment` of a deployment that has already reached
//! its own outcome is refused with HTTP 400 and `DeploymentAlreadyCompletedException`
//! whose message is `The deployment <deploymentId> is already in state <STATE>` — the
//! fixed text `The deployment `, then ` is already in state `, then nothing at all after
//! the state token — where `<deploymentId>` is the id the REQUEST carried and `<STATE>`
//! is the internal (upper-snake) name of the `DeploymentStatus` the service itself
//! reports for that deployment, and the refusal changes nothing a later caller can see:
//! neither the deployment record nor the Lambda alias the deployment moved.** The model
//! puts no text on `DeploymentAlreadyCompletedException`
//! (`smithy_query(query="shape",shape="DeploymentAlreadyCompletedException")` carries only
//! `message`, with `error: client` and the defaulted HTTP 400), so the sentence itself is
//! recorded behavior, while the state token is computed here from what `GetDeployment`
//! reports rather than written down per row.
//!
//! The rows differ along what the rule generalises over: the outcome the deployment
//! settled in and so the state token the sentence reports, whether the deployment took
//! its deployment configuration from its group or named one of this account's own, the
//! spelling the AppSpec revision arrives in, which version the shift moves the alias
//! from and to, the alias's name, and how the refused stop spells (or omits)
//! `autoRollbackEnabled` — a refusal names the state whatever the caller asked of the
//! stop, and rolls nothing back.
//!
//! Two crates have to agree for these rows to hold: CodeDeploy decides the outcome and
//! words the refusal, and AWS Lambda owns the alias the deployment moved and the one a
//! rollback would move back — so a row proves the state in the sentence is an outcome
//! real work reached, not a status written into a response. That is why the suite lives
//! here rather than beside the CodeDeploy crate's own `tests/all/stop_deployment.rs`,
//! which holds the same rule for the deployments that need no second service.
//!
//! Every wait here polls for the CONDITION it is waiting on — a function Lambda reports
//! `Active`, a deployment that has reached a terminal status — so no outcome depends on
//! how fast the host runs.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call here is signed for, which is what routes it to a service.
const REGION: &str = "us-east-1";

/// The error code CodeDeploy refuses a stop of a settled deployment with.
const ALREADY_COMPLETED: &str = "DeploymentAlreadyCompletedException";

/// The FIXED phrase between the deployment id the request named and the state that
/// deployment is already in — the part a client splits the sentence on.
const ALREADY_IN_STATE_PHRASE: &str = " is already in state ";

/// The FIXED phrase the operations over a deployment's PROGRESS word the same refusal
/// with, which a stop's sentence is not.
const ALREADY_COMPLETED_PHRASE: &str = " has already completed, current state ";

/// The built-in deployment configuration that shifts all of a Lambda alias's traffic in
/// one step, which a `Lambda` deployment group takes when it names none.
const ALL_AT_ONCE: &str = "CodeDeployDefault.LambdaAllAtOnce";

/// The service role every deployment group here is created with. CodeDeploy stores it; no
/// IAM is mounted here, so nothing resolves it.
const SERVICE_ROLE_ARN: &str = "arn:aws:iam::123456789012:role/CodeDeployServiceRole";

/// The execution role every function here is created with.
const EXECUTION_ROLE: &str = "arn:aws:iam::123456789012:role/LambdaExecution";

/// The deployment package uploaded inline: nothing here invokes the function, so what the
/// archive holds is immaterial — only the versions it is published as are.
const ZIP_FILE: &str = "cGFja2FnZQ==";

/// How long any one condition here is given before the suite gives up: far above every
/// clock-owed span on this path, so a loaded host cannot decide an outcome. Nothing is
/// measured against it.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll waits between looks.
const POLL: Duration = Duration::from_millis(25);

/// What one alias reports at an instant: the version it invokes, and the weighted routing
/// it splits its invocations by, sorted by version.
type AliasReading = (String, Vec<(String, f64)>);

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly
/// as `core/router/src/lib.rs` mounts them.
///
/// Request authentication is off, as in the other suites here; the in-process calls
/// CodeDeploy makes to AWS Lambda are signed either way, and the credential scope is what
/// routes them.
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

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`].
/// The signature is never checked: this router runs with request authentication off.
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
        "setup: CodeDeploy {operation} must succeed, got {status} with {answered}"
    );
    answered
}

/// Call one AWS Lambda operation over the `restJson1` wire its model declares, addressed
/// by method and URI.
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

/// Call one AWS Lambda operation, requiring `expected`.
async fn lambda_ok(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &Value,
    expected: u16,
) -> Value {
    let (status, answered) = lambda(router, method, uri, body).await;
    assert_eq!(
        status, expected,
        "setup: Lambda {method} {uri} must answer {expected}, got {status} with {answered}"
    );
    answered
}

/// The error code an awsJson1_1 refusal carries, as its `__type` names it: the bare code,
/// which a `__type` may qualify with the shape's namespace.
fn error_code(body: &Value) -> &str {
    body["__type"]
        .as_str()
        .unwrap_or("<no __type>")
        .rsplit(['#', '.'])
        .next()
        .unwrap_or("<no __type>")
}

/// The `$.message` of a refusal body.
fn message(body: &Value, case: &str) -> String {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or_else(|| panic!("case {case:?}: expected a string $.message, got {body}"))
        .to_string()
}

/// Create `function`, wait until Lambda reports it `Active`, and publish `versions`
/// versions of it.
async fn given_function(router: &MultiServiceRouter, function: &str, versions: usize) {
    lambda_ok(
        router,
        "POST",
        "/2015-03-31/functions",
        &json!({
            "FunctionName": function,
            "Role": EXECUTION_ROLE,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Code": {"ZipFile": ZIP_FILE},
        }),
        201,
    )
    .await;

    let deadline = Instant::now() + DEADLINE;
    loop {
        let (_, reported) = lambda(
            router,
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
            "setup: Lambda never reported {function:?} Active within {DEADLINE:?}, last \
             {reported}"
        );
        tokio::time::sleep(POLL).await;
    }

    for version in 1..=versions {
        // A description per version: Lambda publishes no version when the function's
        // configuration and code have not changed since the last one, and the description
        // is part of that configuration.
        let published = lambda_ok(
            router,
            "POST",
            &format!("/2015-03-31/functions/{function}/versions"),
            &json!({"Description": format!("version {version}")}),
            201,
        )
        .await;
        assert_eq!(
            published["Version"].as_str(),
            Some(version.to_string().as_str()),
            "setup: PublishVersion({function:?}) must publish version {version}, got {published}"
        );
    }
}

/// Create `alias` on `function`, invoking `version`.
async fn given_alias(router: &MultiServiceRouter, function: &str, alias: &str, version: &str) {
    lambda_ok(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/aliases"),
        &json!({"Name": alias, "FunctionVersion": version}),
        201,
    )
    .await;
}

/// What `alias` of `function` reports right now, as a client reads it.
async fn alias_reading(router: &MultiServiceRouter, function: &str, alias: &str) -> AliasReading {
    let reported = lambda_ok(
        router,
        "GET",
        &format!("/2015-03-31/functions/{function}/aliases/{alias}"),
        &Value::Null,
        200,
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

/// An AppSpec declaring the shift of `alias` on `function`, in the spelling the AWS
/// CodeDeploy User Guide writes one in.
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

/// The same AppSpec in its other spelling: "the content is formatted as JSON or YAML
/// stored as a RawString"
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

/// Create a `Lambda` application and one deployment group on it, answering the group's
/// name.
async fn given_group(router: &MultiServiceRouter, application: &str) -> String {
    codedeploy_ok(
        router,
        "CreateApplication",
        &json!({"applicationName": application, "computePlatform": "Lambda"}),
    )
    .await;
    let group = format!("{application}-dg");
    codedeploy_ok(
        router,
        "CreateDeploymentGroup",
        &json!({
            "applicationName": application,
            "deploymentGroupName": group,
            "serviceRoleArn": SERVICE_ROLE_ARN,
            "deploymentConfigName": ALL_AT_ONCE,
        }),
    )
    .await;
    group
}

/// Create this account's own deployment configuration shifting all of the traffic in one
/// step, answering its name.
async fn given_own_all_at_once_config(router: &MultiServiceRouter, name: &str) -> String {
    codedeploy_ok(
        router,
        "CreateDeploymentConfig",
        &json!({
            "deploymentConfigName": name,
            "computePlatform": "Lambda",
            "trafficRoutingConfig": {"type": "AllAtOnce"},
        }),
    )
    .await;
    name.to_string()
}

/// Create a deployment of `revision` on `application`/`group`, under `config` where the
/// row names one of its own rather than taking the group's, waiting out the refusal a
/// group already running a deployment answers with, and answer the id CodeDeploy minted.
///
/// A deployment group runs ONE deployment at a time, so a second `CreateDeployment`
/// against it is refused with `DeploymentLimitExceededException` until the first is
/// terminal — which a client waits out, and so does this.
async fn deployment_on_free_group(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    group: &str,
    config: Option<&str>,
    revision: &Value,
) -> String {
    let mut request = json!({
        "applicationName": application,
        "deploymentGroupName": group,
        "revision": revision,
    });
    if let Some(config) = config {
        request["deploymentConfigName"] = json!(config);
    }
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, answered) = codedeploy(router, "CreateDeployment", &request).await;
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

/// Poll `GetDeployment` until `deployment_id` holds a terminal status, answering that
/// `deploymentInfo`.
///
/// The terminal statuses are the members of the model's `DeploymentStatus` enum that no
/// further transition leaves (`smithy_query(query="shape",shape="DeploymentStatus")`:
/// Created | Queued | InProgress | Baking | Succeeded | Failed | Stopped | Ready).
async fn terminal_deployment(
    router: &MultiServiceRouter,
    case: &str,
    deployment_id: &str,
) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let info = deployment_info(router, deployment_id).await;
        if matches!(
            info["status"].as_str(),
            Some("Succeeded" | "Failed" | "Stopped")
        ) {
            return info;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the deployment never reached a terminal status within {DEADLINE:?}, \
             last {info}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Stop `deployment_id`, requiring the refusal a settled deployment earns, and answer its
/// message.
async fn refused_stop(
    router: &MultiServiceRouter,
    case: &str,
    deployment_id: &str,
    auto_rollback_enabled: Option<bool>,
) -> String {
    let mut request = json!({"deploymentId": deployment_id});
    if let Some(enabled) = auto_rollback_enabled {
        request["autoRollbackEnabled"] = json!(enabled);
    }
    let (status, body) = codedeploy(router, "StopDeployment", &request).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 from StopDeployment, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        ALREADY_COMPLETED,
        "case {case:?}: expected {ALREADY_COMPLETED}, got {body}"
    );
    message(&body, case)
}

/// One row: a Lambda deployment driven to an outcome across the two services, and the
/// stop that is then refused on it.
struct Settled {
    /// What the row is about.
    case: &'static str,
    /// The alias this row's deployment moves, created on [`Settled::from_version`].
    alias: &'static str,
    /// The version the alias starts on, which the AppSpec names as its `CurrentVersion`.
    from_version: &'static str,
    /// The version the AppSpec names as its `TargetVersion`.
    to_version: &'static str,
    /// Whether the AppSpec names the function AWS Lambda HOLDS; a row that names one it
    /// does not hold is the deployment that cannot carry its shift out.
    names_a_held_function: bool,
    /// Whether the deployment names this account's own all-at-once configuration rather
    /// than taking the built-in one its group carries.
    names_its_own_config: bool,
    /// Whether the AppSpec travels as JSON rather than as the User Guide's YAML.
    app_spec_as_json: bool,
    /// `autoRollbackEnabled` on the REFUSED stop, absent when `None`.
    auto_rollback: Option<bool>,
    /// The `DeploymentStatus` the clock settles the deployment in.
    status: &'static str,
    /// The internal state name the refusal must report for that status.
    state: &'static str,
}

/// RULE: a stop of a deployment that has already reached its outcome is refused with
/// `The deployment <deploymentId> is already in state <STATE>` — the id the request
/// carried, the state the service itself reports for that deployment, and nothing after
/// it — and the refusal leaves the deployment record and the alias the deployment moved
/// exactly as they stood.
#[tokio::test]
async fn a_stop_of_a_settled_lambda_deployment_reports_the_state_it_is_already_in() {
    let rows: &[Settled] = &[
        Settled {
            case: "succeeded under the group's built-in configuration",
            alias: "live",
            from_version: "1",
            to_version: "2",
            names_a_held_function: true,
            names_its_own_config: false,
            app_spec_as_json: false,
            auto_rollback: None,
            status: "Succeeded",
            state: "SUCCEEDED",
        },
        Settled {
            case: "succeeded under this account's own configuration, rollback asked for",
            alias: "prod",
            from_version: "2",
            to_version: "1",
            names_a_held_function: true,
            names_its_own_config: true,
            app_spec_as_json: true,
            auto_rollback: Some(true),
            status: "Succeeded",
            state: "SUCCEEDED",
        },
        Settled {
            // A shift AWS Lambda cannot carry out — the AppSpec names a function it does
            // not hold — is how a Lambda deployment reaches the other outcome.
            case: "failed on a function Lambda does not hold, rollback declined",
            alias: "canary",
            from_version: "1",
            to_version: "2",
            names_a_held_function: false,
            names_its_own_config: false,
            app_spec_as_json: false,
            auto_rollback: Some(false),
            status: "Failed",
            state: "FAILED",
        },
    ];

    let router = build_router();
    let function = "stop-refusal-handler";
    // One function with the two versions every row shifts between, and one alias per row
    // so the rows do not move each other's traffic.
    given_function(&router, function, 2).await;

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        given_alias(&router, function, row.alias, row.from_version).await;
        let application = format!("stop-refusal-{index:02}");
        let group = given_group(&router, &application).await;
        let config = if row.names_its_own_config {
            Some(given_own_all_at_once_config(&router, &format!("{application}-config")).await)
        } else {
            None
        };
        // The function the AppSpec names: the one Lambda holds, or a name it does not.
        let named_function = if row.names_a_held_function {
            function.to_string()
        } else {
            format!("{function}-absent")
        };
        let content = if row.app_spec_as_json {
            app_spec_json(
                &named_function,
                row.alias,
                row.from_version,
                row.to_version,
            )
        } else {
            app_spec_yaml(
                &named_function,
                row.alias,
                row.from_version,
                row.to_version,
            )
        };
        let deployment_id = deployment_on_free_group(
            &router,
            case,
            &application,
            &group,
            config.as_deref(),
            &app_spec_revision(&content),
        )
        .await;

        let settled = terminal_deployment(&router, case, &deployment_id).await;
        let reported = settled["status"].as_str().unwrap_or_default().to_string();
        assert_eq!(
            reported, row.status,
            "case {case:?}: expected the clock to settle the deployment as {}, got {settled}",
            row.status
        );
        // What the deployment really did to the alias, which is what the state the
        // refusal reports stands for: a succeeded shift left the alias on the target
        // version with no weighted routing behind it, and a deployment whose shift could
        // not be carried out left the alias where it found it.
        let before_alias = alias_reading(&router, function, row.alias).await;
        let expected_alias: AliasReading = if row.names_a_held_function {
            (row.to_version.to_string(), Vec::new())
        } else {
            (row.from_version.to_string(), Vec::new())
        };
        assert_eq!(
            before_alias, expected_alias,
            "case {case:?}: expected the settled deployment to have left the alias at \
             {expected_alias:?}, got {before_alias:?}"
        );

        let actual = refused_stop(&router, case, &deployment_id, row.auto_rollback).await;
        // The sentence is computed from the state the SERVICE reports, not from the row:
        // the id the request carried, and the internal name of that status.
        let expected = format!(
            "The deployment {deployment_id}{ALREADY_IN_STATE_PHRASE}{}",
            reported.to_uppercase()
        );
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
        assert_eq!(
            reported.to_uppercase(),
            row.state,
            "case {case:?}: expected the internal state {} for the status {reported}",
            row.state
        );
        // Nothing follows the state token: the sentence ends on the state itself, and it
        // is not the sentence the operations over a deployment's progress use.
        assert!(
            actual.ends_with(row.state) && !actual.contains(ALREADY_COMPLETED_PHRASE),
            "case {case:?}: expected the sentence to end on {:?} and not to be worded as the \
             progress refusal, got {actual:?}",
            row.state
        );

        // The refusal changed nothing: not the record, and not the alias — not even where
        // the refused stop asked for a rollback.
        let after = deployment_info(&router, &deployment_id).await;
        assert_eq!(
            after, settled,
            "case {case:?}: expected the refusal to leave the record untouched, got {after}"
        );
        let after_alias = alias_reading(&router, function, row.alias).await;
        assert_eq!(
            after_alias, before_alias,
            "case {case:?}: expected the refusal to move no traffic, got {after_alias:?}"
        );
    }
}

/// RULE: the deployment id in the sentence is the one the REQUEST named — on an account
/// holding several settled deployments each refusal names its own, never the newest or
/// the first.
#[tokio::test]
async fn each_refusal_names_the_deployment_the_request_asked_about() {
    let case = "two settled deployments of one group";
    let router = build_router();
    let function = "stop-refusal-pair";
    let alias = "live";
    given_function(&router, function, 2).await;
    given_alias(&router, function, alias, "1").await;
    let application = "stop-refusal-pair";
    let group = given_group(&router, application).await;

    // Two deployments of the same group, one after the other: the second shifts the alias
    // back, so both reach an outcome of their own.
    let mut deployments = Vec::new();
    for (current, target) in [("1", "2"), ("2", "1")] {
        let deployment_id = deployment_on_free_group(
            &router,
            case,
            application,
            &group,
            None,
            &app_spec_revision(&app_spec_yaml(function, alias, current, target)),
        )
        .await;
        let settled = terminal_deployment(&router, case, &deployment_id).await;
        assert_eq!(
            settled["status"].as_str(),
            Some("Succeeded"),
            "case {case:?}: expected the deployment of version {target} to succeed, got {settled}"
        );
        deployments.push(deployment_id);
    }
    assert_ne!(
        deployments[0], deployments[1],
        "case {case:?}: expected two deployments, got {deployments:?}"
    );

    for deployment_id in &deployments {
        let actual = refused_stop(&router, case, deployment_id, None).await;
        assert_eq!(
            actual,
            format!("The deployment {deployment_id}{ALREADY_IN_STATE_PHRASE}SUCCEEDED"),
            "case {case:?}: expected the refusal to name {deployment_id}, got {actual:?}"
        );
        let other = deployments
            .iter()
            .find(|other| *other != deployment_id)
            .expect("the pair holds the other deployment");
        assert!(
            !actual.contains(other.as_str()),
            "case {case:?}: expected the refusal not to name the other deployment {other}, got \
             {actual:?}"
        );
    }
}
