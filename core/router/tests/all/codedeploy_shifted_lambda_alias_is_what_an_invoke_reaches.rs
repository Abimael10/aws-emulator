// SPDX-License-Identifier: CC0-1.0

//! What an alias-qualified `Invoke` REACHES once a CodeDeploy deployment has shifted that
//! alias: the control plane's reading of the alias and the data plane's resolution of it.
//!
//! The rule under test: **an alias is a qualifier `Invoke` resolves per invocation, so the
//! version an alias-qualified invocation reaches is the version that alias names at the
//! instant of the call** — `GetAlias(...).FunctionVersion` and the `X-Amz-Executed-Version`
//! an invocation of `"<function>:<alias>"` reports are one version, however many times a
//! deployment has moved that alias since it was created ("use aliases to provide clients with
//! a function identifier that you can update to invoke a different version", `CreateAlias`;
//! "The version of the function that executed. When you invoke a function with an alias, this
//! indicates which version the alias resolved to",
//! `smithy_query(query="operation",operation="Invoke")`). A completed traffic shift is
//! therefore readable from BOTH sides: the alias reports the AppSpec's `TargetVersion` and
//! every live invocation of that alias reaches it.
//!
//! Two crates have to agree for that to hold — CodeDeploy decides when each increment of a
//! deployment's shift falls due and writes it, and AWS Lambda owns the alias both `GetAlias`
//! and `Invoke` read — so the suite lives here, with the two mounted on one engine the way
//! `core/router/src/lib.rs` mounts them, each driven through its own front door: the function,
//! its versions, its alias and the invocation through Lambda's `restJson1` API, the deployment
//! through CodeDeploy's JSON envelope. Nothing reaches into any service's store.
//!
//! The invocations are DRY RUNS — "Validate parameter values and verify that the user or role
//! has permission to invoke the function" — because the QUALIFIER RESOLUTION is what this
//! suite is about: a dry run reports the version the qualifier resolved to and runs no code,
//! so no row depends on a deployment package. That the code of the run is the code of the
//! version thus reported is pinned on Lambda's own data plane, in
//! `services/service-com-amazonaws-lambda-awsgirapiservice/tests/all/alias_qualified_invoke_reaches_the_version_read.rs`.
//!
//! The rows differ along what the rule generalises over: which version the deployment targets
//! and in which direction, how many increments its effective `trafficRoutingConfig` shifts in,
//! and which form the invocation addresses the alias by. They run one after another on the
//! SAME alias, so each row's reading follows a deployment that has already shifted it — which
//! is the sequence a second release makes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for; the credential scope is what routes a request
/// to a service and what each handler reads its Region from.
const REGION: &str = "us-east-1";

/// A service-role ARN of the form `CreateDeploymentGroup.serviceRoleArn` takes. CodeDeploy
/// stores it; no IAM is mounted here, so nothing resolves it.
const SERVICE_ROLE_ARN: &str = "arn:aws:iam::123456789012:role/CodeDeployServiceRole";

/// The built-in deployment configuration that shifts all of a Lambda alias's traffic in one
/// step, which is what the deployments of a plain release use.
const ALL_AT_ONCE: &str = "CodeDeployDefault.LambdaAllAtOnce";

/// How long anything this suite waits for is given, and how often it looks again.
///
/// Generous: every wait here is for a CONDITION (a function reported `Active`, a deployment
/// terminal), never for a span, so the deadline only has to sit far above the spans the
/// emulator chooses on any host.
const DEADLINE: Duration = Duration::from_secs(240);
/// See [`DEADLINE`].
const POLL: Duration = Duration::from_millis(25);

/// "For the DryRun invocation type, the status code is 204" (`Invoke$StatusCode`).
const DRY_RUN_STATUS: u16 = 204;

/// The alias every deployment below moves, which is the one a release shifts traffic on.
const ALIAS: &str = "live";

/// What a dispatched request answered.
struct Answer {
    status: u16,
    headers: http::HeaderMap,
    body: Value,
}

impl Answer {
    /// One response header, as text.
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }
}

/// The emulator with the two services this suite exercises, on ONE engine.
///
/// Request authentication is off, as in every other suite here; the in-process calls
/// CodeDeploy makes to Lambda are signed either way, and the credential scope is what routes
/// them.
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

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`]. The
/// signature is never checked: this router runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// Call one CodeDeploy operation over the awsJson1_1 wire its model declares, requiring
/// success and answering the response body.
async fn codedeploy_ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
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
    assert_eq!(
        status, 200,
        "codedeploy {operation} must succeed, got {status} with {payload:?}"
    );
    payload
}

/// Call one AWS Lambda operation over the `restJson1` wire its model declares, addressed by
/// method and URI and carrying `headers`.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &Value,
    headers: &[(&str, &str)],
) -> Answer {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"));
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    Answer {
        status,
        headers: response.headers().clone(),
        body,
    }
}

/// The same, requiring a success status and answering the response body.
async fn lambda_ok(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    uri: &str,
    body: &Value,
) -> Value {
    let answered = lambda(router, method, uri, body, &[]).await;
    assert!(
        (200..300).contains(&answered.status),
        "case {case:?}: expected success from {method} {uri}, got {} with {:?}",
        answered.status,
        answered.body
    );
    answered.body
}

/// Create `function` and publish `versions` versions of it.
///
/// Each publish carries its own `Description`: Lambda does not publish a version whose
/// configuration and code match the last one, so a distinct description is what makes each
/// call its own snapshot with its own number. The invocations below are dry runs, so what the
/// package holds is immaterial — only the VERSIONS an AppSpec may name are.
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
    // A freshly created function is owed the transition to `Active` before a version of it can
    // be published or an invocation of it admitted, so this waits for that report.
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

/// Create the alias the deployments move, invoking `version`.
async fn given_alias(router: &MultiServiceRouter, case: &str, function: &str, version: &str) {
    lambda_ok(
        router,
        case,
        "POST",
        &format!("/2015-03-31/functions/{function}/aliases"),
        &json!({"Name": ALIAS, "FunctionVersion": version}),
    )
    .await;
}

/// What the alias of `function` reports right now, as a client reads it: the version it
/// invokes, and the versions its weighted routing splits invocations with, sorted.
async fn alias_reading(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
) -> (String, Vec<String>) {
    let reported = lambda_ok(
        router,
        case,
        "GET",
        &format!("/2015-03-31/functions/{function}/aliases/{ALIAS}"),
        &Value::Null,
    )
    .await;
    let version = reported["FunctionVersion"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let mut weighted: Vec<String> = reported["RoutingConfig"]["AdditionalVersionWeights"]
        .as_object()
        .map(|weights| weights.keys().cloned().collect())
        .unwrap_or_default();
    weighted.sort();
    (version, weighted)
}

/// The version a DRY RUN of `reference` resolves to: "Validate parameter values and verify
/// that the user or role has permission to invoke the function" runs no code, and reports the
/// version the qualifier resolved to.
///
/// `qualifier` is the `Qualifier` parameter where the row addresses the alias through it
/// rather than through the `FunctionName`'s own `:`-suffix.
async fn invoked_version(
    router: &MultiServiceRouter,
    case: &str,
    reference: &str,
    qualifier: Option<&str>,
) -> String {
    let uri = match qualifier {
        Some(qualifier) => format!(
            "/2015-03-31/functions/{reference}/invocations?Qualifier={qualifier}"
        ),
        None => format!("/2015-03-31/functions/{reference}/invocations"),
    };
    let answered = lambda(
        router,
        "POST",
        &uri,
        &json!({}),
        &[("x-amz-invocation-type", "DryRun")],
    )
    .await;
    assert_eq!(
        answered.status, DRY_RUN_STATUS,
        "case {case:?}: a dry run of {reference:?} must answer {DRY_RUN_STATUS}, got {} with \
         {:?}",
        answered.status, answered.body
    );
    answered
        .header("x-amz-executed-version")
        .unwrap_or_else(|| {
            panic!("case {case:?}: a dry run of {reference:?} reported no ExecutedVersion")
        })
}

/// An AppSpec declaring the shift of [`ALIAS`] on `function`, in the spelling the AWS
/// CodeDeploy User Guide writes one in.
fn app_spec_yaml(function: &str, current: &str, target: &str) -> String {
    format!(
        "version: 0.0\n\
         Resources:\n\
         \x20 - {function}:\n\
         \x20     Type: AWS::Lambda::Function\n\
         \x20     Properties:\n\
         \x20       Name: {function}\n\
         \x20       Alias: {ALIAS}\n\
         \x20       CurrentVersion: \"{current}\"\n\
         \x20       TargetVersion: \"{target}\"\n"
    )
}

/// Create one deployment group named `group` of `application`, under a deployment
/// configuration carrying `routing` or the built-in that shifts all of the traffic at once.
async fn given_lambda_group(
    router: &MultiServiceRouter,
    application: &str,
    group: &str,
    routing: Option<Value>,
) {
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
    codedeploy_ok(
        router,
        "CreateDeploymentGroup",
        &json!({
            "applicationName": application,
            "deploymentGroupName": group,
            "serviceRoleArn": SERVICE_ROLE_ARN,
            "deploymentConfigName": config,
        }),
    )
    .await;
}

/// Create a deployment of the AppSpec `content` in `group`, and answer the id CodeDeploy
/// minted for it.
async fn create_deployment(
    router: &MultiServiceRouter,
    application: &str,
    group: &str,
    content: &str,
) -> String {
    let created = codedeploy_ok(
        router,
        "CreateDeployment",
        &json!({
            "applicationName": application,
            "deploymentGroupName": group,
            "revision": {
                "revisionType": "AppSpecContent",
                "appSpecContent": {"content": content},
            },
        }),
    )
    .await;
    created["deploymentId"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Poll `GetDeployment` until `deployment` holds a terminal status, answering that
/// `deploymentInfo`.
///
/// The terminal statuses are the members of the model's `DeploymentStatus` enum that no
/// further transition leaves (`smithy_query(query="shape",shape="DeploymentStatus")`).
async fn terminal_deployment(router: &MultiServiceRouter, case: &str, deployment: &str) -> Value {
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

/// One row: the deployment one release makes, and where it leaves the alias.
struct Release {
    /// What the row is about.
    case: &'static str,
    /// The AppSpec's `CurrentVersion`, which is the version the alias invokes when the row
    /// starts.
    current: &'static str,
    /// The AppSpec's `TargetVersion`, which is where the alias — and every live invocation of
    /// it — has to end up.
    target: &'static str,
    /// The `trafficRoutingConfig` the row's deployment configuration carries, or `None` for
    /// the built-in that shifts all of the traffic at once.
    routing: Option<Value>,
}

/// RULE: once a Lambda-platform deployment has shifted its alias, the alias and every
/// alias-qualified invocation name ONE version — the AppSpec's `TargetVersion` — whatever the
/// direction of the shift, the increments it was walked in, or the form the invocation
/// addresses the alias by.
#[tokio::test]
async fn an_invoke_of_a_shifted_alias_reaches_the_version_the_alias_reads() {
    let rows: Vec<Release> = vec![
        Release {
            case: "the first release, one version forward in a single cutover",
            current: "1",
            target: "2",
            routing: None,
        },
        Release {
            case: "the release after a succeeded one, on the alias it already moved",
            current: "2",
            target: "3",
            routing: None,
        },
        Release {
            case: "a canary whose last increment completes the move, back two versions",
            current: "3",
            target: "1",
            // No wait between the increments (`canaryInterval` has a published minimum of 0
            // minutes): this row is about where the alias and the invocations END once the
            // deployment succeeded, and the window an interval holds open is the CodeDeploy
            // crate's own suite's.
            routing: Some(json!({
                "type": "TimeBasedCanary",
                "timeBasedCanary": {"canaryPercentage": 10, "canaryInterval": 0},
            })),
        },
        Release {
            case: "three versions forward at once, onto the newest",
            current: "1",
            target: "4",
            routing: None,
        },
    ];

    let router = build_router();
    let function = "released";
    let application = "release-app";
    given_function(&router, "the suite's function", function, 4).await;
    given_alias(&router, "the suite's alias", function, "1").await;
    codedeploy_ok(
        &router,
        "CreateApplication",
        &json!({"applicationName": application, "computePlatform": "Lambda"}),
    )
    .await;
    // Before any deployment: the alias and an invocation of it already agree, which is the
    // state every row below starts from.
    let reached = invoked_version(
        &router,
        "before any release",
        &format!("{function}:{ALIAS}"),
        None,
    )
    .await;
    assert_eq!(
        reached, "1",
        "case \"before any release\": an invocation of the alias must reach the version it was \
         created on, got {reached:?}"
    );

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let group = format!("release-group-{index}");
        given_lambda_group(&router, application, &group, row.routing.clone()).await;
        let deployment = create_deployment(
            &router,
            application,
            &group,
            &app_spec_yaml(function, row.current, row.target),
        )
        .await;
        let info = terminal_deployment(&router, case, &deployment).await;
        assert_eq!(
            info["status"].as_str(),
            Some("Succeeded"),
            "case {case:?}: the deployment must succeed, got {info:?}"
        );

        // The control plane's reading of the alias: the target version, with nothing weighted
        // off it once the move is complete.
        let (read, weighted) = alias_reading(&router, case, function).await;
        assert_eq!(
            read, row.target,
            "case {case:?}: GetAlias must report FunctionVersion {:?}, got {read:?}",
            row.target
        );
        assert!(
            weighted.is_empty(),
            "case {case:?}: a completed shift leaves no weighted routing, got {weighted:?}"
        );

        // ... and the data plane's: every form of an alias-qualified invocation reaches the
        // version that reading names, and so does the version named directly.
        for (form, reference, qualifier) in [
            ("the :-suffixed FunctionName", format!("{function}:{ALIAS}"), None),
            ("the Qualifier parameter", function.to_string(), Some(ALIAS)),
            (
                "the version named directly",
                format!("{function}:{}", row.target),
                None,
            ),
        ] {
            let reached = invoked_version(&router, case, &reference, qualifier).await;
            assert_eq!(
                reached, read,
                "case {case:?}: an invocation through {form} must reach the version GetAlias \
                 reads ({read:?}), got {reached:?}"
            );
        }
    }
}
