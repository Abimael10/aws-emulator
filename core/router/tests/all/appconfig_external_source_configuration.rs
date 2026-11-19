// SPDX-License-Identifier: CC0-1.0

//! A configuration profile whose configuration lives OUTSIDE AppConfig's hosted store is
//! served by the data plane from that store.
//!
//! RULE: a configuration profile's `LocationUri` says which store its configuration lives in
//! — `hosted` for AppConfig's own one, `ssm-parameter://<parameter name>` or a parameter ARN
//! for a Systems Manager Parameter Store parameter (`smithy_query(query="doc",
//! shape="CreateConfigurationProfileRequest", member="LocationUri")`) — and a deployment of
//! such a profile names a version of THAT store ("for all other configurations, you must
//! specify the version number", `smithy_query(query="doc", shape="StartDeploymentRequest",
//! member="ConfigurationVersion")`). So every reader of the deployed configuration is served
//! the content that store holds for the version the deployment named, described by that
//! store's own media type, and the deployed version stays PINNED: a later write at the source
//! mints a new version and changes nothing a session is served until a deployment names the
//! new version.
//!
//! Only a cross-service suite can state that end to end: Parameter Store owns the parameter
//! and its versions, AppConfig's control plane owns the profile and the deployment, and what
//! a running application receives is read where that application reads it — out of the
//! AppConfig DATA plane, through a configuration session. The last suite below covers the
//! control plane's own reader of a configuration version, `ValidateConfiguration`, which
//! judges the same bytes out of the same store.
//!
//! The rows differ along what the rule generalises over: which store the profile points at
//! (the hosted store as the control row, a parameter by `ssm-parameter://` and a parameter by
//! ARN), the shape of the parameter's name (top level and hierarchical), the parameter's
//! `Type` (`String` and `StringList`), the bytes and media type of the configuration, and the
//! Region every call of the row is signed for — which is the endpoint the source is resolved
//! through.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::form_encode;

/// The Region most rows are signed for; one row uses [`OTHER_REGION`] instead, so nothing
/// here can pass by resolving a source at a Region the request never named.
const REGION: &str = "us-east-1";

/// See [`REGION`].
const OTHER_REGION: &str = "eu-west-2";

/// The `LocationUri` that selects AppConfig's own hosted configuration store.
const HOSTED: &str = "hosted";

/// The `LocationUri` prefix that selects a Parameter Store parameter by name.
const PARAMETER_URI_PREFIX: &str = "ssm-parameter://";

/// The media type a JSON body travels with.
const JSON_CONTENT_TYPE: &str = "application/json";

/// The media type a Parameter Store parameter's value is described by: a parameter's value is
/// text whatever its `ParameterType` is.
const TEXT_CONTENT_TYPE: &str = "text/plain";

/// The `DeploymentState` values a deployment can no longer leave
/// (`smithy_query(query="shape", shape="DeploymentState")`).
const TERMINAL_STATES: &[&str] = &["COMPLETE", "ROLLED_BACK", "REVERTED"];

/// How often work the clock owes — a deployment settling, a rollout reaching this session —
/// is looked for again, and how long the looking goes on.
///
/// Those spans belong to the services that own them, so nothing here sleeps on their behalf:
/// every wait is on the CONDITION, and each deadline is a ceiling on the WAIT and nothing
/// else, generous enough that a loaded host cannot decide any outcome. No assertion here is
/// about how long anything took.
const LOOK: Duration = Duration::from_millis(50);
/// See [`LOOK`].
const SETTLED_BY: Duration = Duration::from_secs(120);
/// See [`LOOK`].
const OBSERVED_BY: Duration = Duration::from_secs(60);

/// How many FRESH sessions are opened to check the pinning: a deployment pinned to one source
/// version must serve that version to every session started after the source was overwritten,
/// not merely to the one that was already polling.
const PINNED_SESSIONS: usize = 3;

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

/// One row: the store the profile points at, and the two versions of the configuration that
/// store holds.
#[derive(Debug)]
struct Row {
    /// The row's name, which every assertion of it reports.
    case: &'static str,
    /// The Region every call of this row is signed for, and so the Region its parameter lives
    /// in and its session is polled at.
    region: &'static str,
    store: Store,
    /// The name Parameter Store holds the parameter under (ignored by the hosted row).
    parameter_name: &'static str,
    /// A `ParameterType` value (`smithy_query(query="shape", shape="ParameterType")` of the
    /// Systems Manager model): `String`, `StringList` or `SecureString`.
    parameter_type: &'static str,
    /// The media type the served configuration must be described by.
    content_type: &'static str,
    /// The content of version 1 of the source, the one the first deployment names.
    deployed: &'static str,
    /// The content of version 2, written while version 1 is deployed and then deployed in
    /// turn.
    redeployed: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a top-level parameter addressed by ssm-parameter://",
        region: REGION,
        store: Store::ParameterByUri,
        parameter_name: "fleet-tier",
        parameter_type: "String",
        content_type: TEXT_CONTENT_TYPE,
        deployed: "tier-one",
        redeployed: "tier-two",
    },
    Row {
        case: "a hierarchical parameter addressed by ssm-parameter://, a JSON document",
        region: REGION,
        store: Store::ParameterByUri,
        parameter_name: "/fleet/eu/limits",
        parameter_type: "String",
        content_type: TEXT_CONTENT_TYPE,
        deployed: r#"{"limit":5,"mode":"canary"}"#,
        redeployed: r#"{"limit":9,"mode":"wide"}"#,
    },
    Row {
        case: "a StringList parameter addressed by its ARN, in another Region",
        region: OTHER_REGION,
        store: Store::ParameterByArn,
        parameter_name: "fleet-endpoints",
        parameter_type: "StringList",
        content_type: TEXT_CONTENT_TYPE,
        deployed: "alpha,beta",
        redeployed: "alpha,beta,gamma",
    },
    Row {
        case: "the AppConfig hosted configuration store",
        region: REGION,
        store: Store::Hosted,
        parameter_name: "",
        parameter_type: "",
        content_type: JSON_CONTENT_TYPE,
        deployed: r#"{"flag":"off"}"#,
        redeployed: r#"{"flag":"on"}"#,
    },
];

/// The emulator as it is ASSEMBLED, on one engine: the parameter a profile points at is a
/// record of this emulator's own Systems Manager, and the configuration a client polls is the
/// one AppConfig's data plane resolves against it.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: each call carries a credential scope so
    // the router can name the target service, without a computed signature.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and `region`,
/// which is what routes the request and what each handler reads its Region from. The
/// signature is never checked: this router runs with request authentication off.
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

/// One call to make: the endpoint host it is addressed at, the credential scope that routes
/// it to the service claiming it and the Region it is signed for, and how that service's
/// protocol carries it.
struct Call<'a> {
    service: &'a str,
    signing_name: &'a str,
    region: &'a str,
    method: &'a str,
    uri: &'a str,
    content_type: &'a str,
    /// The `X-Amz-Target` an RPC protocol names its operation in; `None` for a REST
    /// protocol, which names it by method and path.
    target: Option<&'a str>,
}

/// Dispatch one call with `body` as its request body.
async fn dispatch(router: &MultiServiceRouter, call: &Call<'_>, body: &[u8]) -> Answered {
    let host = format!("{}.{}.amazonaws.com", call.service, call.region);
    let mut request = ::http::Request::builder()
        .method(call.method)
        .uri(format!("https://{host}{}", call.uri))
        .header("host", host.clone())
        .header(
            "authorization",
            authorization(call.signing_name, call.region),
        )
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

/// One AppConfig DATA-plane call, as a polling client makes one. Both of AppConfig's APIs
/// sign as `appconfig`, whichever endpoint a client addresses.
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

/// Write one version of a parameter through Parameter Store's own wire, and answer the
/// version number it minted.
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

/// The value Parameter Store itself reports for the LATEST version of `name` — the source of
/// truth a row's seeding is confirmed against, so nothing below can pass against a parameter
/// that was never written.
async fn parameter_value(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    name: &str,
) -> String {
    let answered = ssm(router, region, "GetParameter", &json!({"Name": name})).await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: GetParameter({name:?}) must succeed, got {} with {}",
        answered.status,
        answered.text()
    );
    answered.json()["Parameter"]["Value"]
        .as_str()
        .unwrap_or_default()
        .to_string()
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

/// The `LocationUri` a row's profile is created with, and the parameter's own ARN for the row
/// that addresses it that way.
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
                row.parameter_name.strip_prefix('/').unwrap_or(row.parameter_name)
            ),
        ),
    }
}

/// One row's AppConfig hierarchy as the control plane minted it, with the Region its calls
/// are signed for: what a deployment is started against, and what a session polls.
struct Hierarchy {
    region: &'static str,
    application: String,
    environment: String,
    profile: String,
    strategy: String,
}

/// Read one deployment until it reaches a state it can no longer leave, and answer the
/// document it was last reported with. The wait is on the CONDITION.
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

/// Start a deployment of `version` and wait for it to settle, asserting it completed.
async fn deploy(
    router: &MultiServiceRouter,
    case: &str,
    hierarchy: &Hierarchy,
    number: i64,
    version: &str,
) {
    let started = appconfig(
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
    .await;
    assert_eq!(
        started.status,
        201,
        "case {case:?}: StartDeployment({version:?}) must be accepted, got {} with {}",
        started.status,
        started.text()
    );
    let settled = settled_deployment(router, case, hierarchy, number).await;
    assert_eq!(
        settled["State"],
        json!("COMPLETE"),
        "case {case:?}: the deployment of version {version:?} must complete, got {settled}"
    );
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
    let token = session
        .string("InitialConfigurationToken")
        .unwrap_or_else(|| {
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
/// through a NEW session until the bytes are `expected` or the deadline passes, then answer
/// the last read, which is what the caller asserts on.
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

/// RULE: the configuration a deployment puts in service is the content the store the
/// profile's `LocationUri` names holds for the version the deployment named — and it stays
/// that version's content until a deployment names another one.
#[tokio::test]
async fn a_profile_is_served_the_deployed_version_of_the_store_it_points_at() {
    let router = build_router();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let suffix = format!("src{index}");

        // Version 1 of the source, written where that source's own clients write it. The
        // hosted store's versions are AppConfig's own, so they are written below, once the
        // profile they belong to exists.
        if row.store != Store::Hosted {
            let version = put_parameter(
                &router,
                case,
                row.region,
                row.parameter_name,
                row.parameter_type,
                row.deployed,
                false,
            )
            .await;
            assert_eq!(
                version, 1,
                "case {case:?}: the first write of {:?} must mint version 1, got {version}",
                row.parameter_name
            );
            let held = parameter_value(&router, case, row.region, row.parameter_name).await;
            assert_eq!(
                held, row.deployed,
                "case {case:?}: Parameter Store must hold the seeded value, expected {:?}, got \
                 {held:?}",
                row.deployed
            );
        }

        let application = minted_id(
            case,
            "the application",
            &appconfig(
                &router,
                row.region,
                "POST",
                "/applications",
                &json!({"Name": format!("sourced-app-{suffix}")}),
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
                &json!({"Name": format!("sourced-env-{suffix}")}),
            )
            .await,
        );
        let mut profile_request = json!({
            "Name": format!("sourced-profile-{suffix}"),
            "LocationUri": location_uri(row),
        });
        if row.store != Store::Hosted {
            profile_request["RetrievalRoleArn"] = json!(retrieval_role_arn(&suffix));
        }
        let profile = minted_id(
            case,
            "the configuration profile",
            &appconfig(
                &router,
                row.region,
                "POST",
                &format!("/applications/{application}/configurationprofiles"),
                &profile_request,
            )
            .await,
        );
        let strategy = given_strategy(&router, row.region, &format!("sourced-fast-{suffix}")).await;
        let hierarchy = Hierarchy {
            region: row.region,
            application: application.clone(),
            environment,
            profile: profile.clone(),
            strategy,
        };

        // The hosted row's two versions are written here, through the hosted store's own
        // operation; every other row's live in Parameter Store.
        if row.store == Store::Hosted {
            let written = appconfig_payload(
                &router,
                row.region,
                "POST",
                &format!(
                    "/applications/{application}/configurationprofiles/{profile}\
                     /hostedconfigurationversions"
                ),
                row.content_type,
                row.deployed.as_bytes(),
            )
            .await;
            assert_eq!(
                written.status,
                201,
                "case {case:?}: writing hosted version 1 must succeed, got {} with {}",
                written.status,
                written.text()
            );
        }

        // Version 1 of the source into service.
        deploy(&router, case, &hierarchy, 1, "1").await;
        let read = serving(&router, case, &hierarchy, row.deployed).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: GetLatestConfiguration must serve the deployed configuration, got \
             {} with {}",
            read.status,
            read.text()
        );
        assert_eq!(
            read.text(),
            row.deployed,
            "case {case:?}: expected the deployed version's content {:?}, got {:?}",
            row.deployed,
            read.text()
        );
        let content_type = read.header("content-type").unwrap_or_default();
        assert_eq!(
            content_type, row.content_type,
            "case {case:?}: expected ContentType {:?}, got {content_type:?}",
            row.content_type
        );

        // Version 2 written at the source, out of band: nothing a session is served may
        // change until a deployment names it.
        if row.store == Store::Hosted {
            let written = appconfig_payload(
                &router,
                row.region,
                "POST",
                &format!(
                    "/applications/{application}/configurationprofiles/{profile}\
                     /hostedconfigurationversions"
                ),
                row.content_type,
                row.redeployed.as_bytes(),
            )
            .await;
            assert_eq!(
                written.status,
                201,
                "case {case:?}: writing hosted version 2 must succeed, got {} with {}",
                written.status,
                written.text()
            );
        } else {
            let version = put_parameter(
                &router,
                case,
                row.region,
                row.parameter_name,
                row.parameter_type,
                row.redeployed,
                true,
            )
            .await;
            assert_eq!(
                version, 2,
                "case {case:?}: overwriting {:?} must mint version 2, got {version}",
                row.parameter_name
            );
            let held = parameter_value(&router, case, row.region, row.parameter_name).await;
            assert_eq!(
                held, row.redeployed,
                "case {case:?}: Parameter Store must hold the overwritten value, expected {:?}, \
                 got {held:?}",
                row.redeployed
            );
        }

        for look in 1..=PINNED_SESSIONS {
            let pinned = served(&router, case, &hierarchy).await;
            assert_eq!(
                pinned.text(),
                row.deployed,
                "case {case:?}: session {look} started after the source was overwritten must \
                 still be served the DEPLOYED version {:?}, got {:?}",
                row.deployed,
                pinned.text()
            );
        }

        // And the new version only once a deployment names it.
        deploy(&router, case, &hierarchy, 2, "2").await;
        let after = serving(&router, case, &hierarchy, row.redeployed).await;
        assert_eq!(
            after.status,
            200,
            "case {case:?}: GetLatestConfiguration must serve the redeployed configuration, got \
             {} with {}",
            after.status,
            after.text()
        );
        assert_eq!(
            after.text(),
            row.redeployed,
            "case {case:?}: expected the newly deployed version's content {:?}, got {:?}",
            row.redeployed,
            after.text()
        );
    }
}

/// What a row of the refusal table points its profile at, and the version its deployment
/// names.
#[derive(Debug)]
struct MissingRow {
    case: &'static str,
    /// The parameter name the profile's `LocationUri` points at.
    parameter_name: &'static str,
    /// Whether that parameter is written at all before the deployment.
    parameter_written: bool,
    /// The `ConfigurationVersion` the deployment names.
    deployed_version: &'static str,
}

const MISSING_ROWS: &[MissingRow] = &[
    MissingRow {
        case: "a version the parameter does not carry",
        parameter_name: "absent-version",
        parameter_written: true,
        deployed_version: "7",
    },
    MissingRow {
        case: "a parameter Parameter Store does not hold",
        parameter_name: "/never/written",
        parameter_written: false,
        deployed_version: "1",
    },
];

/// RULE (the same one, from the other side): the hosted store answers only for a profile that
/// USES it, so a poll for a parameter-backed profile whose source has no such version is
/// refused as the configuration that could not be retrieved from THAT source
/// (`ResourceNotFoundException` / HTTP 404 with `ResourceType` `Configuration`,
/// `smithy_query(query="operation", operation="GetLatestConfiguration")`) and never as bytes
/// the AppConfig hosted configuration store does not hold.
#[tokio::test]
async fn a_source_without_the_deployed_version_is_not_refused_as_a_hosted_version() {
    let router = build_router();
    let strategy = given_strategy(&router, REGION, "sourced-missing-fast").await;
    for (index, row) in MISSING_ROWS.iter().enumerate() {
        let case = row.case;
        let suffix = format!("miss{index}");
        if row.parameter_written {
            put_parameter(
                &router,
                case,
                REGION,
                row.parameter_name,
                "String",
                "only-version-one",
                false,
            )
            .await;
        }
        let application = minted_id(
            case,
            "the application",
            &appconfig(
                &router,
                REGION,
                "POST",
                "/applications",
                &json!({"Name": format!("sourced-app-{suffix}")}),
            )
            .await,
        );
        let environment = minted_id(
            case,
            "the environment",
            &appconfig(
                &router,
                REGION,
                "POST",
                &format!("/applications/{application}/environments"),
                &json!({"Name": format!("sourced-env-{suffix}")}),
            )
            .await,
        );
        let location = format!("{PARAMETER_URI_PREFIX}{}", row.parameter_name);
        let profile = minted_id(
            case,
            "the configuration profile",
            &appconfig(
                &router,
                REGION,
                "POST",
                &format!("/applications/{application}/configurationprofiles"),
                &json!({
                    "Name": format!("sourced-profile-{suffix}"),
                    "LocationUri": location,
                    "RetrievalRoleArn": retrieval_role_arn(&suffix),
                }),
            )
            .await,
        );
        let hierarchy = Hierarchy {
            region: REGION,
            application,
            environment,
            profile,
            strategy: strategy.clone(),
        };
        let started = appconfig(
            &router,
            REGION,
            "POST",
            &format!(
                "/applications/{}/environments/{}/deployments",
                hierarchy.application, hierarchy.environment
            ),
            &json!({
                "DeploymentStrategyId": hierarchy.strategy,
                "ConfigurationProfileId": hierarchy.profile,
                "ConfigurationVersion": row.deployed_version,
            }),
        )
        .await;
        assert_eq!(
            started.status,
            201,
            "case {case:?}: StartDeployment must be accepted for an external source, got {} with \
             {}",
            started.status,
            started.text()
        );
        settled_deployment(&router, case, &hierarchy, 1).await;

        let read = served(&router, case, &hierarchy).await;
        assert_eq!(
            read.status,
            404,
            "case {case:?}: a poll whose source holds no such version must be refused with 404, \
             got {} with {}",
            read.status,
            read.text()
        );
        assert_eq!(
            read.error_code(),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {}",
            read.error_code()
        );
        let resource_type = read.string("ResourceType").unwrap_or_default();
        assert_eq!(
            resource_type, "Configuration",
            "case {case:?}: expected ResourceType \"Configuration\", got {resource_type:?}"
        );
        let message = read.string("Message").unwrap_or_default();
        assert!(
            message.contains(&location),
            "case {case:?}: expected the refusal to name the source {location:?}, got {message:?}"
        );
        assert!(
            !message.contains("hosted configuration store"),
            "case {case:?}: a profile that does not use the hosted store must not be refused as \
             a hosted version, got {message:?}"
        );
    }
}

/// One row of the validation table: a parameter value, and whether the profile's validator
/// admits it.
#[derive(Debug)]
struct ValidationRow {
    case: &'static str,
    /// The value the parameter version holds, which is the document the validator judges.
    value: &'static str,
    /// Whether that document satisfies the schema below.
    compliant: bool,
}

/// The schema a validated profile carries, as the `Content` of a `JSON_SCHEMA` validator:
/// "a JSON Schema document" (`smithy_query(query="doc", shape="Validator",
/// member="Content")`).
const VALIDATION_SCHEMA: &str = r#"{
  "type": "object",
  "required": ["limit"],
  "properties": {"limit": {"type": "integer"}}
}"#;

const VALIDATION_ROWS: &[ValidationRow] = &[
    ValidationRow {
        case: "a parameter value the schema admits",
        value: r#"{"limit":5}"#,
        compliant: true,
    },
    ValidationRow {
        case: "a parameter value of the wrong member type",
        value: r#"{"limit":"five"}"#,
        compliant: false,
    },
    ValidationRow {
        case: "a parameter value missing the required member",
        value: r#"{"mode":"canary"}"#,
        compliant: false,
    },
];

/// RULE (the same one, for the OTHER reader of a configuration version): the document a
/// profile's validators are run against is the content the store its `LocationUri` names
/// holds for the version — so a parameter-backed profile is validated against that
/// parameter version's value, and `ValidateConfiguration` answers its modelled 204 when the
/// validators admit it and `BadRequestException` / HTTP 400 when they do not
/// (`smithy_query(query="operation", operation="ValidateConfiguration")`).
#[tokio::test]
async fn a_parameter_backed_version_is_validated_against_the_parameters_value() {
    let router = build_router();
    for (index, row) in VALIDATION_ROWS.iter().enumerate() {
        let case = row.case;
        let suffix = format!("val{index}");
        let parameter_name = format!("/validated/{suffix}");
        put_parameter(
            &router,
            case,
            REGION,
            &parameter_name,
            "String",
            row.value,
            false,
        )
        .await;
        let application = minted_id(
            case,
            "the application",
            &appconfig(
                &router,
                REGION,
                "POST",
                "/applications",
                &json!({"Name": format!("sourced-app-{suffix}")}),
            )
            .await,
        );
        let profile = minted_id(
            case,
            "the configuration profile",
            &appconfig(
                &router,
                REGION,
                "POST",
                &format!("/applications/{application}/configurationprofiles"),
                &json!({
                    "Name": format!("sourced-profile-{suffix}"),
                    "LocationUri": format!("{PARAMETER_URI_PREFIX}{parameter_name}"),
                    "RetrievalRoleArn": retrieval_role_arn(&suffix),
                    "Validators": [{"Type": "JSON_SCHEMA", "Content": VALIDATION_SCHEMA}],
                }),
            )
            .await,
        );
        let validated = appconfig(
            &router,
            REGION,
            "POST",
            &format!(
                "/applications/{application}/configurationprofiles/{profile}\
                 /validators?configuration_version=1"
            ),
            &Value::Null,
        )
        .await;
        if row.compliant {
            assert_eq!(
                validated.status,
                204,
                "case {case:?}: the parameter version's value satisfies the validator, so \
                 ValidateConfiguration must answer 204, got {} with {}",
                validated.status,
                validated.text()
            );
        } else {
            assert_eq!(
                validated.status,
                400,
                "case {case:?}: the parameter version's value fails the validator, so \
                 ValidateConfiguration must be refused with 400, got {} with {}",
                validated.status,
                validated.text()
            );
            assert_eq!(
                validated.error_code(),
                "BadRequestException",
                "case {case:?}: expected BadRequestException, got {}",
                validated.error_code()
            );
        }
    }
}
