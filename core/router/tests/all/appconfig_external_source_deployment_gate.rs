// SPDX-License-Identifier: CC0-1.0

//! The deployment-time validator gate judges the version being deployed out of the store the
//! configuration profile's `LocationUri` names.
//!
//! RULE: the candidate configuration a configuration profile's validators are run against
//! when a deployment of a version is started is the content THAT version has in the store the
//! profile's `LocationUri` names — `hosted` for AppConfig's own configuration store,
//! `ssm-parameter://<parameter name>` or a parameter ARN for a Systems Manager Parameter Store
//! parameter (`smithy_query(query="doc", shape="CreateConfigurationProfileRequest",
//! member="LocationUri")`), where a deployment names a version of that source ("for all other
//! configurations, you must specify the version number", `smithy_query(query="doc",
//! shape="StartDeploymentRequest", member="ConfigurationVersion")`). "The configuration
//! deployment or update can only proceed when the configuration data is valid"
//! (`smithy_query(query="doc", shape="Validator")`), so a deployment whose version's content
//! at that store the validators reject is refused with `BadRequestException` / HTTP 400
//! (`smithy_query(query="operation", operation="StartDeployment")`) and writes no deployment
//! record, and one they admit completes and is what the data plane serves.
//!
//! Only a cross-service suite can state that: Parameter Store owns the parameter and its
//! versions, AppConfig's control plane owns the profile, its validators and the deployment,
//! and what a running application receives is read out of the AppConfig DATA plane through a
//! configuration session.
//!
//! The rows differ along what the rule generalises over: which store the profile points at
//! (the hosted store as the control row, a parameter by `ssm-parameter://` and a parameter by
//! ARN), the shape of the parameter's name (top level and hierarchical), the parameter's
//! `Type` (`String` and `StringList`), whether the first version's content satisfies the
//! profile's validator, and the Region every call of the row is signed for — which is the
//! endpoint the source is resolved through.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::form_encode;

/// The Region most rows are signed for; one row uses [`OTHER_REGION`] instead, so nothing here
/// can pass by resolving a source at a Region the request never named.
const REGION: &str = "us-east-1";

/// See [`REGION`].
const OTHER_REGION: &str = "eu-west-2";

/// The `LocationUri` that selects AppConfig's own hosted configuration store.
const HOSTED: &str = "hosted";

/// The `LocationUri` prefix that selects a Parameter Store parameter by name.
const PARAMETER_URI_PREFIX: &str = "ssm-parameter://";

/// The media type a JSON body travels with.
const JSON_CONTENT_TYPE: &str = "application/json";

/// The `DeploymentState` values a deployment can no longer leave
/// (`smithy_query(query="shape", shape="DeploymentState")`).
const TERMINAL_STATES: &[&str] = &["COMPLETE", "ROLLED_BACK", "REVERTED"];

/// How often work the clock owes — a deployment settling, a rollout reaching this session — is
/// looked for again, and how long the looking goes on.
///
/// Those spans belong to the services that own them, so nothing here sleeps on their behalf:
/// every wait is on the CONDITION, and each deadline is a ceiling on the WAIT and nothing else,
/// generous enough that a loaded host cannot decide any outcome. No assertion here is about how
/// long anything took.
const LOOK: Duration = Duration::from_millis(50);
/// See [`LOOK`].
const SETTLED_BY: Duration = Duration::from_secs(120);
/// See [`LOOK`].
const OBSERVED_BY: Duration = Duration::from_secs(60);

/// The schema every row's profile carries, as the `Content` of a `JSON_SCHEMA` validator: "a
/// JSON Schema document" (`smithy_query(query="doc", shape="Validator", member="Content")`).
const VALIDATION_SCHEMA: &str = r#"{
  "type": "object",
  "required": ["limit"],
  "properties": {"limit": {"type": "integer"}}
}"#;

/// Which store a row's configuration profile points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Store {
    /// `hosted`: AppConfig's own configuration store, the control row.
    Hosted,
    /// A Parameter Store parameter, addressed as `ssm-parameter://<parameter name>`.
    ParameterByUri,
    /// The same parameter, addressed by its ARN — the other form the member documents.
    ParameterByArn,
}

/// One row: the store the profile points at, the content of version 1 of the configuration in
/// that store and whether the validator admits it, and the content of version 2, which every
/// row makes compliant.
#[derive(Debug)]
struct Row {
    /// The row's name, which every assertion of it reports.
    case: &'static str,
    /// The Region every call of this row is signed for, and so the Region its parameter lives
    /// in and its session is polled at.
    region: &'static str,
    store: Store,
    /// The name Parameter Store holds the parameter under (ignored by the hosted rows).
    parameter_name: &'static str,
    /// A `ParameterType` value (`smithy_query(query="shape", shape="ParameterType")`,
    /// Systems Manager's model), ignored by the hosted rows.
    parameter_type: &'static str,
    /// The content version 1 holds in that store.
    first: &'static str,
    /// Whether [`Row::first`] satisfies [`VALIDATION_SCHEMA`].
    first_is_compliant: bool,
    /// The content version 2 holds, which every row makes compliant.
    second: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a top-level parameter addressed by ssm-parameter://, a value the schema admits",
        region: REGION,
        store: Store::ParameterByUri,
        parameter_name: "gate-limits",
        parameter_type: "String",
        first: r#"{"limit":5}"#,
        first_is_compliant: true,
        second: r#"{"limit":9}"#,
    },
    Row {
        case: "a hierarchical parameter addressed by ssm-parameter://, a value of the wrong \
               member type",
        region: REGION,
        store: Store::ParameterByUri,
        parameter_name: "/fleet/eu/gate-limits",
        parameter_type: "String",
        first: r#"{"limit":"five"}"#,
        first_is_compliant: false,
        second: r#"{"limit":11}"#,
    },
    Row {
        case: "a parameter addressed by its ARN in another Region, missing the required member",
        region: OTHER_REGION,
        store: Store::ParameterByArn,
        parameter_name: "gate-endpoints",
        parameter_type: "String",
        first: r#"{"mode":"canary"}"#,
        first_is_compliant: false,
        second: r#"{"limit":2}"#,
    },
    Row {
        case: "a StringList parameter addressed by its ARN, a value that is not a document",
        region: REGION,
        store: Store::ParameterByArn,
        parameter_name: "gate-hosts",
        parameter_type: "StringList",
        first: "alpha,beta",
        first_is_compliant: false,
        second: r#"{"limit":3}"#,
    },
    Row {
        case: "the AppConfig hosted configuration store, a value the schema admits",
        region: REGION,
        store: Store::Hosted,
        parameter_name: "",
        parameter_type: "",
        first: r#"{"limit":1}"#,
        first_is_compliant: true,
        second: r#"{"limit":4}"#,
    },
    Row {
        case: "the AppConfig hosted configuration store, a value the schema rejects",
        region: REGION,
        store: Store::Hosted,
        parameter_name: "",
        parameter_type: "",
        first: r#"{"limit":[1]}"#,
        first_is_compliant: false,
        second: r#"{"limit":7}"#,
    },
];

/// The emulator as it is ASSEMBLED, on one engine: the parameter a profile points at is a
/// record of this emulator's own Systems Manager, and the configuration a deployment is gated
/// on is the one AppConfig reads against it.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: each call carries a credential scope so the
    // router can name the target service, without a computed signature.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and `region`,
/// which is what routes the request and what each handler reads its Region from. The signature
/// is never checked: this router runs with request authentication off.
fn authorization(signing_name: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
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

    /// One top-level string member of a JSON body.
    fn string(&self, member: &str) -> Option<String> {
        self.json()
            .get(member)
            .and_then(Value::as_str)
            .map(ToString::to_string)
    }

    /// One response header, as text.
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    /// The wire error code of an error response, without its namespace.
    fn error_code(&self) -> String {
        let code = self
            .string("__type")
            .or_else(|| self.header("x-amzn-errortype"))
            .unwrap_or_else(|| self.text());
        code.split('#').next_back().unwrap_or(&code).to_string()
    }
}

/// One call to make: the endpoint host it is addressed at, the credential scope that routes it
/// to the service claiming it and the Region it is signed for, and how that service's protocol
/// carries it.
struct Call<'a> {
    service: &'a str,
    signing_name: &'a str,
    region: &'a str,
    method: &'a str,
    uri: &'a str,
    content_type: &'a str,
    /// The `X-Amz-Target` an RPC protocol names its operation in; `None` for a REST protocol,
    /// which names it by method and path.
    target: Option<&'a str>,
}

/// Dispatch one call with `body` as its request body.
async fn dispatch(router: &MultiServiceRouter, call: &Call<'_>, body: &[u8]) -> Answered {
    let host = format!("{}.{}.amazonaws.com", call.service, call.region);
    let mut request = ::http::Request::builder()
        .method(call.method)
        .uri(format!("https://{host}{}", call.uri))
        .header("host", host.clone())
        .header("authorization", authorization(call.signing_name, call.region))
        .header("content-type", call.content_type);
    if let Some(target) = call.target {
        request = request.header("x-amz-target", target);
    }
    let request = request
        .body(Bytes::from(body.to_vec()))
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
async fn appconfig(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    body: &Value,
) -> Answered {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        body.to_string().into_bytes()
    };
    appconfig_payload(router, region, method, uri, JSON_CONTENT_TYPE, &payload).await
}

/// One AppConfig CONTROL-plane call whose body is the configuration's own bytes, of its own
/// media type — the form `CreateHostedConfigurationVersion` binds its `@httpPayload` content
/// and its `@required` `Content-Type` header in.
async fn appconfig_payload(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    content_type: &str,
    body: &[u8],
) -> Answered {
    dispatch(
        router,
        &Call {
            service: "appconfig",
            signing_name: "appconfig",
            region,
            method,
            uri,
            content_type,
            target: None,
        },
        body,
    )
    .await
}

/// One AppConfig DATA-plane call, as a polling client makes one. Both of AppConfig's APIs sign
/// as `appconfig`, whichever endpoint a client addresses.
async fn appconfigdata(
    router: &MultiServiceRouter,
    region: &str,
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
        &Call {
            service: "appconfigdata",
            signing_name: "appconfig",
            region,
            method,
            uri,
            content_type: JSON_CONTENT_TYPE,
            target: None,
        },
        &payload,
    )
    .await
}

/// One Systems Manager `awsJson1_1` RPC (service shape `AmazonSSM`).
async fn ssm(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> Answered {
    let target = format!("AmazonSSM.{operation}");
    dispatch(
        router,
        &Call {
            service: "ssm",
            signing_name: "ssm",
            region,
            method: "POST",
            uri: "/",
            content_type: "application/x-amz-json-1.1",
            target: Some(&target),
        },
        body.to_string().as_bytes(),
    )
    .await
}

/// Write one version of a parameter through Parameter Store's own wire, and answer the version
/// number it minted.
async fn put_parameter(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    name: &str,
    parameter_type: &str,
    value: &str,
    overwrite: bool,
) -> i64 {
    let answered = ssm(
        router,
        region,
        "PutParameter",
        &json!({
            "Name": name,
            "Type": parameter_type,
            "Value": value,
            "Overwrite": overwrite,
        }),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: PutParameter({name:?}) must succeed, got {} with {}",
        answered.status,
        answered.text()
    );
    answered.json()["Version"].as_i64().unwrap_or_else(|| {
        panic!(
            "case {case:?}: PutParameter({name:?}) answered no Version: {}",
            answered.text()
        )
    })
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

/// A deployment strategy that reaches every target in one step and bakes for no time.
async fn given_strategy(router: &MultiServiceRouter, region: &str, name: &str) -> String {
    let created = appconfig(
        router,
        region,
        "POST",
        "/deploymentstrategies",
        &json!({
            "Name": name,
            "DeploymentDurationInMinutes": 0,
            "FinalBakeTimeInMinutes": 0,
            "GrowthFactor": 100,
            "GrowthType": "LINEAR",
            "ReplicateTo": "NONE",
        }),
    )
    .await;
    minted_id(name, "the deployment strategy", &created)
}

/// The ARN of the role AppConfig reads an external source through. A `RetrievalRoleArn` is
/// "required for all other sources that store your configuration"
/// (`smithy_query(query="doc", shape="CreateConfigurationProfileRequest",
/// member="RetrievalRoleArn")`), so every non-hosted row carries one.
fn retrieval_role_arn(suffix: &str) -> String {
    ::engine::emulator_arn("iam", "", &format!("role/appconfig-retrieval-{suffix}"))
}

/// The `LocationUri` a row's profile is created with, and the parameter's own ARN for the rows
/// that address it that way.
fn location_uri(row: &Row) -> String {
    match row.store {
        Store::Hosted => HOSTED.to_string(),
        Store::ParameterByUri => format!("{PARAMETER_URI_PREFIX}{}", row.parameter_name),
        // `arn:aws:ssm:<region>:<account>:parameter/<name>`, the ARN Parameter Store reports
        // for the parameter — a hierarchical name keeps its whole path after `parameter/`.
        Store::ParameterByArn => ::engine::emulator_arn(
            "ssm",
            row.region,
            &format!(
                "parameter/{}",
                row.parameter_name
                    .strip_prefix('/')
                    .unwrap_or(row.parameter_name)
            ),
        ),
    }
}

/// One row's AppConfig hierarchy as the control plane minted it, with the Region its calls are
/// signed for: what a deployment is started against, and what a session polls.
struct Hierarchy {
    region: &'static str,
    application: String,
    environment: String,
    profile: String,
    strategy: String,
}

/// Write one version of the row's configuration into the store its profile points at, and
/// assert that store minted the version number expected.
async fn given_version(
    router: &MultiServiceRouter,
    case: &str,
    row: &Row,
    hierarchy: &Hierarchy,
    content: &str,
    expected_version: i64,
) {
    if row.store == Store::Hosted {
        let written = appconfig_payload(
            router,
            row.region,
            "POST",
            &format!(
                "/applications/{}/configurationprofiles/{}/hostedconfigurationversions",
                hierarchy.application, hierarchy.profile
            ),
            JSON_CONTENT_TYPE,
            content.as_bytes(),
        )
        .await;
        assert_eq!(
            written.status,
            201,
            "case {case:?}: writing hosted version {expected_version} must succeed, got {} with \
             {}",
            written.status,
            written.text()
        );
        // `VersionNumber` is bound to the `Version-Number` HEADER of this operation's
        // response, whose `@httpPayload` is the configuration's own bytes
        // (`smithy_query(query="operation", operation="CreateHostedConfigurationVersion")`).
        let number = written
            .header("version-number")
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or_default();
        assert_eq!(
            number, expected_version,
            "case {case:?}: expected hosted version {expected_version}, got {number}"
        );
        return;
    }
    let version = put_parameter(
        router,
        case,
        row.region,
        row.parameter_name,
        row.parameter_type,
        content,
        expected_version > 1,
    )
    .await;
    assert_eq!(
        version, expected_version,
        "case {case:?}: writing {:?} must mint version {expected_version}, got {version}",
        row.parameter_name
    );
}

/// Read one deployment until it reaches a state it can no longer leave, and answer the document
/// it was last reported with. The wait is on the CONDITION.
async fn settled_deployment(
    router: &MultiServiceRouter,
    case: &str,
    hierarchy: &Hierarchy,
    number: i64,
) -> Value {
    let deadline = Instant::now() + SETTLED_BY;
    loop {
        let read = appconfig(
            router,
            hierarchy.region,
            "GET",
            &format!(
                "/applications/{}/environments/{}/deployments/{number}",
                hierarchy.application, hierarchy.environment
            ),
            &Value::Null,
        )
        .await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: GetDeployment({number}) must succeed, got {} with {}",
            read.status,
            read.text()
        );
        let document = read.json();
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

/// What AppConfig's data plane serves ONE fresh client right now: a session of its own, then
/// the configuration that session is handed, answered as it came back off the wire.
async fn served(router: &MultiServiceRouter, case: &str, hierarchy: &Hierarchy) -> Answered {
    let session = appconfigdata(
        router,
        hierarchy.region,
        "POST",
        "/configurationsessions",
        &json!({
            "ApplicationIdentifier": hierarchy.application,
            "EnvironmentIdentifier": hierarchy.environment,
            "ConfigurationProfileIdentifier": hierarchy.profile,
        }),
    )
    .await;
    assert_eq!(
        session.status,
        201,
        "case {case:?}: StartConfigurationSession must succeed, got {} with {}",
        session.status,
        session.text()
    );
    let token = session.string("InitialConfigurationToken").unwrap_or_else(|| {
        panic!(
            "case {case:?}: StartConfigurationSession answered no token: {}",
            session.text()
        )
    });
    appconfigdata(
        router,
        hierarchy.region,
        "GET",
        &format!("/configuration?configuration_token={}", form_encode(&token)),
        &Value::Null,
    )
    .await
}

/// What a fresh client is served once the deployment's rollout has reached it: read again
/// through a NEW session until the bytes are `expected` or the deadline passes, then answer the
/// last read, which is what the caller asserts on.
async fn serving(
    router: &MultiServiceRouter,
    case: &str,
    hierarchy: &Hierarchy,
    expected: &str,
) -> Answered {
    let deadline = Instant::now() + OBSERVED_BY;
    loop {
        let read = served(router, case, hierarchy).await;
        if read.body.as_ref() == expected.as_bytes() || Instant::now() >= deadline {
            return read;
        }
        tokio::time::sleep(LOOK).await;
    }
}

/// Start a deployment of `version` and answer what `StartDeployment` itself said.
async fn start_deployment(
    router: &MultiServiceRouter,
    hierarchy: &Hierarchy,
    version: &str,
) -> Answered {
    appconfig(
        router,
        hierarchy.region,
        "POST",
        &format!(
            "/applications/{}/environments/{}/deployments",
            hierarchy.application, hierarchy.environment
        ),
        &json!({
            "DeploymentStrategyId": hierarchy.strategy,
            "ConfigurationProfileId": hierarchy.profile,
            "ConfigurationVersion": version,
        }),
    )
    .await
}

/// RULE: the document a configuration profile's validators judge when a deployment is started
/// is the content the store the profile's `LocationUri` names holds for the version being
/// deployed — so a parameter-backed version its validators reject is refused with
/// `BadRequestException` / HTTP 400 and leaves no deployment behind, and one they admit is
/// deployed and served.
#[tokio::test]
async fn a_deployments_validators_judge_the_version_in_the_store_the_profile_points_at() {
    let router = build_router();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let suffix = format!("gate{index}");

        let application = minted_id(
            case,
            "the application",
            &appconfig(
                &router,
                row.region,
                "POST",
                "/applications",
                &json!({"Name": format!("gated-app-{suffix}")}),
            )
            .await,
        );
        let environment = minted_id(
            case,
            "the environment",
            &appconfig(
                &router,
                row.region,
                "POST",
                &format!("/applications/{application}/environments"),
                &json!({"Name": format!("gated-env-{suffix}")}),
            )
            .await,
        );
        let mut profile_request = json!({
            "Name": format!("gated-profile-{suffix}"),
            "LocationUri": location_uri(row),
            "Validators": [{"Type": "JSON_SCHEMA", "Content": VALIDATION_SCHEMA}],
        });
        if row.store != Store::Hosted {
            profile_request["RetrievalRoleArn"] = json!(retrieval_role_arn(&suffix));
        }
        let created_profile = appconfig(
            &router,
            row.region,
            "POST",
            &format!("/applications/{application}/configurationprofiles"),
            &profile_request,
        )
        .await;
        let profile = minted_id(case, "the configuration profile", &created_profile);
        // The profile is created over the store the row names, and says so: what the gate below
        // reads the candidate configuration out of is this member.
        let echoed = created_profile.string("LocationUri").unwrap_or_default();
        assert_eq!(
            echoed,
            location_uri(row),
            "case {case:?}: expected the profile to be over {:?}, got {echoed:?}",
            location_uri(row)
        );
        let hierarchy = Hierarchy {
            region: row.region,
            application,
            environment,
            profile,
            strategy: given_strategy(&router, row.region, &format!("gated-fast-{suffix}")).await,
        };

        // Version 1 of the configuration, written where the store the profile points at takes
        // it.
        given_version(&router, case, row, &hierarchy, row.first, 1).await;

        let started = start_deployment(&router, &hierarchy, "1").await;
        if row.first_is_compliant {
            assert_eq!(
                started.status,
                201,
                "case {case:?}: the version's content satisfies the validator, so StartDeployment \
                 must be accepted, got {} with {}",
                started.status,
                started.text()
            );
            let settled = settled_deployment(&router, case, &hierarchy, 1).await;
            assert_eq!(
                settled["State"],
                json!("COMPLETE"),
                "case {case:?}: the deployment of version 1 must complete, got {settled}"
            );
            let read = serving(&router, case, &hierarchy, row.first).await;
            assert_eq!(
                read.text(),
                row.first,
                "case {case:?}: expected the deployed version's content {:?}, got {:?}",
                row.first,
                read.text()
            );
        } else {
            assert_eq!(
                started.status,
                400,
                "case {case:?}: the version's content at {:?} fails the validator, so \
                 StartDeployment must be refused with 400, got {} with {}",
                location_uri(row),
                started.status,
                started.text()
            );
            assert_eq!(
                started.error_code(),
                "BadRequestException",
                "case {case:?}: expected BadRequestException, got {} with {}",
                started.error_code(),
                started.text()
            );
            let message = started.string("Message").unwrap_or_default();
            assert!(
                !message.is_empty(),
                "case {case:?}: the refusal must report the validator's verdict, got {}",
                started.text()
            );
            // "No deployment record to roll back": the refused call wrote nothing, so the
            // environment has no deployment 1 at all.
            let absent = appconfig(
                &router,
                row.region,
                "GET",
                &format!(
                    "/applications/{}/environments/{}/deployments/1",
                    hierarchy.application, hierarchy.environment
                ),
                &Value::Null,
            )
            .await;
            assert_eq!(
                absent.status,
                404,
                "case {case:?}: a refused deployment must leave no record, GetDeployment(1) got \
                 {} with {}",
                absent.status,
                absent.text()
            );
        }

        // Version 2, which every row makes compliant: the next deployment is accepted, and its
        // number shows the refusal above consumed none — a refused call leaves the
        // environment's deployment number untouched.
        given_version(&router, case, row, &hierarchy, row.second, 2).await;
        let next_number = if row.first_is_compliant { 2 } else { 1 };
        let redeployed = start_deployment(&router, &hierarchy, "2").await;
        assert_eq!(
            redeployed.status,
            201,
            "case {case:?}: the compliant version must be accepted, got {} with {}",
            redeployed.status,
            redeployed.text()
        );
        let number = redeployed.json()["DeploymentNumber"]
            .as_i64()
            .unwrap_or_default();
        assert_eq!(
            number, next_number,
            "case {case:?}: expected deployment number {next_number}, got {number}"
        );
        let settled = settled_deployment(&router, case, &hierarchy, next_number).await;
        assert_eq!(
            settled["State"],
            json!("COMPLETE"),
            "case {case:?}: the deployment of version 2 must complete, got {settled}"
        );
        let after = serving(&router, case, &hierarchy, row.second).await;
        assert_eq!(
            after.status,
            200,
            "case {case:?}: GetLatestConfiguration must serve the deployed configuration, got {} \
             with {}",
            after.status,
            after.text()
        );
        assert_eq!(
            after.text(),
            row.second,
            "case {case:?}: expected the newly deployed version's content {:?}, got {:?}",
            row.second,
            after.text()
        );
    }
}
