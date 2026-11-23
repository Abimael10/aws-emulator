// SPDX-License-Identifier: CC0-1.0

//! An HTTP api whose route fronts AppConfig through the first-class `AWS_PROXY`
//! `AppConfig-GetConfiguration` subtype, configured and then driven on the ASSEMBLED emulator —
//! every service the call passes through mounted together, as the binary serves them.
//!
//! Two rules, both of which hold for every first-class subtype and not only for the one a
//! recording happened to exercise:
//!
//! RULE 1 (the WRITE): **an `AWS_PROXY` integration naming an `IntegrationSubtype` is created,
//! and patched, only when its `RequestParameters` DEFINE every parameter the action that
//! subtype names cannot be called without; a write that leaves any of them undefined is refused
//! `400 BadRequestException`, naming the subtype and its whole required list in that subtype's
//! own published order, and stores nothing.** `Integration$IntegrationSubtype` is "supported
//! only for HTTP API AWS_PROXY integrations. Specifies the AWS service action to invoke" and
//! `Integration$RequestParameters` is "a key-value map specifying parameters that are passed to
//! AWS_PROXY integrations" — the ARGUMENTS of that one action — so a mapping that leaves a
//! required argument undefined configures a call that could never be made, and API Gateway
//! settles that at the write rather than at invoke time.
//!
//! RULE 2 (the ACTION): **a route whose integration names that subtype performs the action in
//! the service that OWNS it, as a session of the role the integration's `CredentialsArn` names,
//! and the caller is answered that action's own answer in the form the interface it was
//! performed over states one — which for `appconfig GetConfiguration` binds the result to the
//! response's own payload: the configuration's bytes as the whole body, the `ContentType`
//! AppConfig stated for them as `Content-Type`, and the version they belong to as
//! `Configuration-Version` (`smithy_query(query="operation", operation="GetConfiguration")` on
//! the AppConfig model: `Content` is `@httpPayload`, `ContentType` is bound onto `Content-Type`
//! and `ConfigurationVersion` onto `Configuration-Version`).**
//!
//! Only the assembled estate can state either rule end to end: API Gateway v2 admits the
//! mapping, resolves the request to the route and performs the subtype, IAM holds the role,
//! the token service mints the session the call is made as, and AppConfig holds the deployed
//! configuration and decides which version is in force. Nothing asserted here is a value this
//! gateway fills in: the bytes, the media type and the version are read back out of AppConfig's
//! own answer to the SAME call made directly.
//!
//! The rows span what the rules generalise over: for rule 1 the subtype and the SHAPE of its
//! required list (one parameter and four, in that subtype's own order) and which required
//! parameter is the one left out; for rule 2 the media type and the bytes of the deployed
//! configuration (a JSON document, plain text, a YAML document and bytes no text encoding
//! describes), the address the control-plane client was published (a loopback endpoint and one
//! of AWS's own service hosts), the stage and the form of the invoke URL built from the
//! published value, and where each mapped argument comes from (a static value and an expression
//! over the request). The recorded case — a create with an empty mapping, then a JSON
//! configuration read through the route at a loopback address — is two of those rows.
//!
//! What the owning crates' own suites pin and this one does not restate: the whole first-class
//! catalogue against the create and the patch
//! (`services/service-com-amazonaws-apigatewayv2-apigatewayv2/tests/all/integration_validation.rs`),
//! the subtype reached at the api's own documented host
//! (`.../tests/all/http_api_appconfig_subtype.rs`), and AppConfig's own rules for which version
//! is in force (`services/service-com-amazonaws-appconfig-amazonappconfig/tests/all/`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigatewayv2_published_endpoint_runs_its_lambda_route::announced_parts;
use crate::query_wire::form_encode;

/// The statuses the calls below answer: the create's, every read's and the refusal's.
const CREATED: u16 = 201;
const SERVED: u16 = 200;
const BAD_REQUEST: u16 = 400;

/// The shape a refused write names, and the member its sentence travels under.
const BAD_REQUEST_SHAPE: &str = "BadRequestException";
const MESSAGE_MEMBER: &str = "message";

/// The role every integration below is performed as, the service principal it trusts (the one
/// API Gateway assumes a `CredentialsArn` role as), the inline policy the grant is written on,
/// and the permission the action needs as AppConfig publishes it
/// (`<arnNamespace>:<OperationName>`).
const ROLE_NAME: &str = "apigatewayv2-reads-the-configuration";
const GATEWAY_PRINCIPAL: &str = "apigateway.amazonaws.com";
const INLINE_POLICY: &str = "may-read-the-configuration";
const GET_CONFIGURATION_PERMISSION: &str = "appconfig:GetConfiguration";

/// The `LocationUri` that selects AppConfig's own hosted configuration store.
const HOSTED: &str = "hosted";

/// The headers AppConfig's own api binds the two non-payload members of this answer onto.
const CONTENT_TYPE_HEADER: &str = "content-type";
const CONFIGURATION_VERSION_HEADER: &str = "configuration-version";

/// The state AppConfig reports for a deployment that has finished rolling out.
const COMPLETE: &str = "COMPLETE";

/// How long a condition is looked for again, and how long the looking goes on. Generous on
/// purpose: what is waited for is the CONDITION, re-read until it holds, so a loaded host only
/// takes more turns of the loop and the budget is never what a row asserts.
const LOOK: Duration = Duration::from_millis(20);
const SETTLED_BY: Duration = Duration::from_secs(120);

/// The emulator as it is ASSEMBLED, on one engine (`aws_emulator_router::build_router`): the
/// action a route's integration performs is dispatched back into this same router, so the
/// configuration it reads is the one this suite deployed through AppConfig's own operations.
struct Estate {
    router: MultiServiceRouter,
    /// The Region every call is signed for.
    region: String,
    /// The endpoint the CONTROL-PLANE client is configured with: the `Host` its calls carry,
    /// which is what decides the address a minted api announces.
    control_host: String,
}

/// What one dispatched request answered: the status, every header, and the body as it stands.
struct Answered {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Answered {
    /// The body as the document a client parses, or the text itself when it is not JSON.
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&self.body).to_string()))
    }

    /// The value of one header, or `None` when the answer carries none.
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(field, _value)| field == name)
            .map(|(_field, value)| value.as_str())
    }

    /// Everything a failure message needs to name what came back.
    fn summary(&self) -> String {
        format!(
            "{} headers={:?} body={:?}",
            self.status,
            self.headers,
            String::from_utf8_lossy(&self.body)
        )
    }
}

impl Estate {
    fn new(control_host: &str, region: &str) -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = aws_emulator_router::build_router(engine);
        // The suite exercises the services, not SigV4: each call carries a credential scope so
        // the router can name the target service, without a computed signature. The
        // INTEGRATION's own call is signed by the emulator, with the session it minted.
        router.set_request_authentication(false);
        Self {
            router,
            region: region.to_string(),
            control_host: control_host.to_string(),
        }
    }

    /// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
    /// request. The signature is never checked here.
    fn authorization(&self, service: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{}/{service}/aws4_request, \
             SignedHeaders=host, Signature=not-verified",
            self.region
        )
    }

    /// Dispatch one request into the assembled router.
    async fn dispatch(
        &self,
        method: &str,
        url: &str,
        host: &str,
        signing_name: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> Answered {
        let mut builder = ::http::Request::builder()
            .method(method)
            .uri(url)
            .header("host", host)
            .header("authorization", self.authorization(signing_name));
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(Bytes::from(body))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let (parts, body) = response.into_parts();
        Answered {
            status: parts.status.as_u16(),
            headers: parts
                .headers
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().to_ascii_lowercase(),
                        String::from_utf8_lossy(value.as_bytes()).to_string(),
                    )
                })
                .collect(),
            body: body.to_vec(),
        }
    }

    /// One AppConfig call, addressed at that service's own endpoint.
    async fn appconfig(
        &self,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> Answered {
        let host = format!("appconfig.{}.amazonaws.com", self.region);
        self.dispatch(
            method,
            &format!("https://{host}{uri}"),
            &host,
            "appconfig",
            headers,
            body,
        )
        .await
    }

    /// One AppConfig call carrying a JSON document, which must be served; answers the document
    /// it answered.
    async fn appconfig_ok(&self, case: &str, method: &str, uri: &str, body: Option<Value>) -> Value {
        let payload = body
            .as_ref()
            .map(|value| value.to_string().into_bytes())
            .unwrap_or_default();
        let answered = self
            .appconfig(method, uri, &[("content-type", "application/json")], payload)
            .await;
        assert!(
            (200..300).contains(&answered.status),
            "case {case:?}: expected AppConfig {method} {uri} to succeed, got {}",
            answered.summary()
        );
        answered.json()
    }

    /// One awsQuery call to IAM that must be served.
    async fn iam(&self, case: &str, action: &str, params: &[(&str, String)]) {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in params {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let answered = self
            .dispatch(
                "POST",
                "https://iam.amazonaws.com/",
                "iam.amazonaws.com",
                "iam",
                &[("content-type", "application/x-www-form-urlencoded")],
                body.into_bytes(),
            )
            .await;
        assert_eq!(
            answered.status,
            SERVED,
            "case {case:?}: IAM {action} must succeed, got {}",
            answered.summary()
        );
    }

    /// One API Gateway v2 control-plane call (`restJson1` under `/v2`), addressed at the
    /// endpoint the CLIENT was configured with, as `(status, parsed body)`.
    async fn control(&self, method: &str, uri: &str, body: Option<Value>) -> (u16, Value) {
        let payload = body
            .as_ref()
            .map(|value| value.to_string().into_bytes())
            .unwrap_or_default();
        let host = self.control_host.clone();
        let answered = self
            .dispatch(
                method,
                &format!("https://{host}{uri}"),
                &host,
                "apigateway",
                &[("content-type", "application/json")],
                payload,
            )
            .await;
        (answered.status, answered.json())
    }

    /// One client request to a route of the deployed stage — the invoke the integration is
    /// performed for. `endpoint` is the address the api ANNOUNCED and `path` what the client
    /// appends to it; every route below declares `AWS_IAM`, so the request carries a credential
    /// scope naming the data plane's own signing name.
    async fn invoke(&self, endpoint: &str, path: &str, headers: &[(&str, &str)]) -> Answered {
        let (authority, base_path) = announced_parts(endpoint);
        self.dispatch(
            "GET",
            &format!("https://{authority}{base_path}{path}"),
            &authority,
            "execute-api",
            headers,
            Vec::new(),
        )
        .await
    }

    /// The role an integration's `CredentialsArn` names: trusting API Gateway and carrying the
    /// grant for the action the subtype names, written with IAM's own operations. Answers its
    /// ARN.
    async fn role(&self, case: &str) -> String {
        let trust = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": { "Service": GATEWAY_PRINCIPAL },
                "Action": "sts:AssumeRole",
            }],
        })
        .to_string();
        self.iam(
            case,
            "CreateRole",
            &[
                ("RoleName", ROLE_NAME.to_string()),
                ("AssumeRolePolicyDocument", trust),
            ],
        )
        .await;
        let document = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": GET_CONFIGURATION_PERMISSION,
                "Resource": "*",
            }],
        })
        .to_string();
        self.iam(
            case,
            "PutRolePolicy",
            &[
                ("RoleName", ROLE_NAME.to_string()),
                ("PolicyName", INLINE_POLICY.to_string()),
                ("PolicyDocument", document),
            ],
        )
        .await;
        format!(
            "arn:aws:iam::{}:role/{ROLE_NAME}",
            engine::emulator_account_id()
        )
    }

    /// Deploy `content` under `content_type` into a fresh AppConfig hierarchy, each step through
    /// that service's own operation, and wait for the rollout to finish.
    async fn deploy(&self, case: &str, content: &[u8], content_type: &str) -> Deployed {
        let application = self
            .appconfig_ok(
                case,
                "POST",
                "/applications",
                Some(json!({ "Name": "configuration-fleet" })),
            )
            .await;
        let application_id = text(case, &application, "Id");
        let strategy = self
            .appconfig_ok(
                case,
                "POST",
                "/deploymentstrategies",
                Some(json!({
                    "Name": "all-at-once",
                    "DeploymentDurationInMinutes": 0,
                    "GrowthFactor": 100,
                    "FinalBakeTimeInMinutes": 0,
                    "ReplicateTo": "NONE",
                })),
            )
            .await;
        let strategy_id = text(case, &strategy, "Id");
        let environment = self
            .appconfig_ok(
                case,
                "POST",
                &format!("/applications/{application_id}/environments"),
                Some(json!({ "Name": "production" })),
            )
            .await;
        let environment_id = text(case, &environment, "Id");
        let profile = self
            .appconfig_ok(
                case,
                "POST",
                &format!("/applications/{application_id}/configurationprofiles"),
                Some(json!({ "Name": "features", "LocationUri": HOSTED })),
            )
            .await;
        let profile_id = text(case, &profile, "Id");
        // `Content` is this operation's `@httpPayload` and `ContentType` its `Content-Type`
        // header, so the bytes travel as the body a client posts and the media type as the
        // header describing them.
        let hosted = self
            .appconfig(
                "POST",
                &format!(
                    "/applications/{application_id}/configurationprofiles/{profile_id}\
                     /hostedconfigurationversions"
                ),
                &[("content-type", content_type)],
                content.to_vec(),
            )
            .await;
        assert!(
            (200..300).contains(&hosted.status),
            "case {case:?}: CreateHostedConfigurationVersion answered {}",
            hosted.summary()
        );
        let version = hosted
            .header("version-number")
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected a Version-Number header, got {}",
                    hosted.summary()
                )
            })
            .to_string();
        let deployment = self
            .appconfig_ok(
                case,
                "POST",
                &format!(
                    "/applications/{application_id}/environments/{environment_id}/deployments"
                ),
                Some(json!({
                    "DeploymentStrategyId": strategy_id,
                    "ConfigurationProfileId": profile_id,
                    "ConfigurationVersion": version,
                })),
            )
            .await;
        let number = deployment["DeploymentNumber"]
            .as_i64()
            .unwrap_or_else(|| panic!("case {case:?}: no DeploymentNumber in {deployment}"));
        let deployed = Deployed {
            application_id,
            environment_id,
            profile_id,
            version,
        };
        // The wait is on the CONDITION: the deployment's own `State`, re-read until it is the
        // terminal one, or until a deadline far beyond any span this emulator owes it.
        let uri = format!(
            "/applications/{}/environments/{}/deployments/{number}",
            deployed.application_id, deployed.environment_id
        );
        let deadline = Instant::now() + SETTLED_BY;
        loop {
            let read = self.appconfig_ok(case, "GET", &uri, None).await;
            if read["State"] == json!(COMPLETE) {
                return deployed;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the deployment to reach {COMPLETE}, got {read}"
            );
            tokio::time::sleep(LOOK).await;
        }
    }

    /// The SAME read made DIRECTLY against AppConfig, as a client holding the triple would make
    /// it: the independent answer a relayed one is compared with.
    async fn read_directly(&self, deployed: &Deployed, client_id: &str) -> Answered {
        self.appconfig(
            "GET",
            &format!(
                "/applications/{}/environments/{}/configurations/{}?client_id={client_id}",
                deployed.application_id, deployed.environment_id, deployed.profile_id
            ),
            &[],
            Vec::new(),
        )
        .await
    }

    /// A bare HTTP api; answers `(ApiId, the address its ApiEndpoint announces)`.
    async fn create_api(&self, case: &str) -> (String, String) {
        let (status, api) = self
            .control(
                "POST",
                "/v2/apis",
                Some(json!({ "name": "configuration-api", "protocolType": "HTTP" })),
            )
            .await;
        assert_eq!(
            status, CREATED,
            "case {case:?}: CreateApi must answer {CREATED}, got {status} {api}"
        );
        (text(case, &api, "apiId"), text(case, &api, "apiEndpoint"))
    }

    /// One CreateIntegration of a first-class subtype, as `(status, body)`.
    async fn create_integration(
        &self,
        api_id: &str,
        subtype: &str,
        role_arn: &str,
        request_parameters: Value,
    ) -> (u16, Value) {
        self.control(
            "POST",
            &format!("/v2/apis/{api_id}/integrations"),
            Some(json!({
                "integrationType": "AWS_PROXY",
                "integrationSubtype": subtype,
                "payloadFormatVersion": "1.0",
                "credentialsArn": role_arn,
                "requestParameters": request_parameters,
            })),
        )
        .await
    }

    /// One UpdateIntegration carrying only a new mapping, as `(status, body)`.
    async fn patch_mapping(
        &self,
        api_id: &str,
        integration_id: &str,
        request_parameters: Value,
    ) -> (u16, Value) {
        self.control(
            "PATCH",
            &format!("/v2/apis/{api_id}/integrations/{integration_id}"),
            Some(json!({ "requestParameters": request_parameters })),
        )
        .await
    }

    /// The integration of `integration_id`, read back through this service's own reader.
    async fn read_integration(&self, case: &str, api_id: &str, integration_id: &str) -> Value {
        let (status, read) = self
            .control(
                "GET",
                &format!("/v2/apis/{api_id}/integrations/{integration_id}"),
                None,
            )
            .await;
        assert_eq!(
            status, SERVED,
            "case {case:?}: GetIntegration must answer {SERVED}, got {status} {read}"
        );
        read
    }

    /// How many integrations the api holds, read through its own collection reader — so a
    /// refused write is shown to have stored NOTHING rather than merely to have answered 400.
    async fn integration_count(&self, case: &str, api_id: &str) -> usize {
        let (status, listed) = self
            .control("GET", &format!("/v2/apis/{api_id}/integrations"), None)
            .await;
        assert_eq!(
            status, SERVED,
            "case {case:?}: GetIntegrations must answer {SERVED}, got {status} {listed}"
        );
        listed["items"].as_array().map_or(0, Vec::len)
    }

    /// One route over `integration_id`, gated the way the recorded api gates it, and the stage
    /// that serves it with auto-deploy on.
    async fn deploy_route(
        &self,
        case: &str,
        api_id: &str,
        integration_id: &str,
        route_key: &str,
        stage: &str,
    ) {
        let (status, route) = self
            .control(
                "POST",
                &format!("/v2/apis/{api_id}/routes"),
                Some(json!({
                    "routeKey": route_key,
                    "target": format!("integrations/{integration_id}"),
                    "authorizationType": "AWS_IAM",
                })),
            )
            .await;
        assert_eq!(
            status, CREATED,
            "case {case:?}: CreateRoute must answer {CREATED}, got {status} {route}"
        );
        let (status, created) = self
            .control(
                "POST",
                &format!("/v2/apis/{api_id}/stages"),
                Some(json!({ "stageName": stage, "autoDeploy": true })),
            )
            .await;
        assert_eq!(
            status, CREATED,
            "case {case:?}: CreateStage must answer {CREATED}, got {status} {created}"
        );
    }
}

/// One deployed configuration, as the triple a route's mapping addresses it by and the version
/// AppConfig holds it under.
struct Deployed {
    application_id: String,
    environment_id: String,
    profile_id: String,
    version: String,
}

/// One string member of a document that must carry it.
fn text(case: &str, document: &Value, member: &str) -> String {
    document[member]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} in {document}"))
        .to_string()
}

/// How API Gateway words the refusal of a subtype integration whose mapping leaves a parameter
/// its action requires undefined: fixed text, with the subtype the request named and that
/// subtype's whole required list, in the order it publishes them.
///
/// Written out here ON PURPOSE, rather than imported from the handler's own renderer: a test
/// that formats the sentence with the very function under test asserts only that the function
/// agrees with itself.
fn unmapped_required_parameters_message(subtype: &str, required: &[&str]) -> String {
    format!(
        "Operation: {subtype} requires enabling passthrough, or defining all of the following \
         parameters in the parameter mapping: [{}]",
        required.join(", ")
    )
}

/// Assert one write was refused as this service's modelled invalid request, with the sentence
/// `expected`.
fn assert_refused(case: &str, expected: &str, status: u16, body: &Value) {
    assert_eq!(
        status, BAD_REQUEST,
        "case {case:?}: expected {BAD_REQUEST} from the write, got {status} {body}"
    );
    let shape = body["__type"].as_str().unwrap_or_default();
    assert!(
        shape.rsplit(['#', '/']).next() == Some(BAD_REQUEST_SHAPE),
        "case {case:?}: expected {BAD_REQUEST_SHAPE}, got {body}"
    );
    assert_eq!(
        body[MESSAGE_MEMBER].as_str().unwrap_or_default(),
        expected,
        "case {case:?}: wrong refusal sentence, got {body}"
    );
}

/// One first-class subtype of rule 1: the subtype a request names, and the parameters the action
/// it names cannot be called without — in the ORDER API Gateway lists them for that subtype,
/// which is per-subtype and neither alphabetical nor the serving model's declaration order.
struct Subtype {
    case: &'static str,
    name: &'static str,
    required: &'static [&'static str],
}

/// The subtypes rule 1 is read over. Each required list is the members the serving service's own
/// input marks REQUIRED, spelled as a mapping spells a parameter (upper camel case):
///
/// * `smithy_query(query="operation", operation="GetConfiguration")` on the AppConfig model —
///   `Application`, `Environment`, `Configuration`, `ClientId`;
/// * `smithy_query(query="operation", operation="StartExecution")` on the Step Functions model
///   — `stateMachineArn`, a subtype whose required list is one parameter long;
/// * `smithy_query(query="operation", operation="PutRecord")` on the Kinesis model — `Data`,
///   beside the `StreamName` and `PartitionKey` nothing else in that call names.
const SUBTYPES: &[Subtype] = &[
    Subtype {
        case: "the recorded subtype, whose four arguments address a configuration and its reader",
        name: "AppConfig-GetConfiguration",
        required: &["Application", "Environment", "Configuration", "ClientId"],
    },
    Subtype {
        case: "an action of another service whose one argument is the machine",
        name: "StepFunctions-StartExecution",
        required: &["StateMachineArn"],
    },
    Subtype {
        case: "an action of another service whose three arguments are listed stream first",
        name: "Kinesis-PutRecord",
        required: &["StreamName", "Data", "PartitionKey"],
    },
];

/// The mapping that defines `parameters`, each with a value a mapping may carry.
fn mapping(parameters: &[&str]) -> Value {
    let defined: serde_json::Map<String, Value> = parameters
        .iter()
        .map(|parameter| {
            (
                (*parameter).to_string(),
                json!(format!("$request.header.{parameter}")),
            )
        })
        .collect();
    Value::Object(defined)
}

/// RULE 1: an `AWS_PROXY` integration naming a first-class subtype is written — created and
/// patched alike — only when its `RequestParameters` define every parameter that subtype's
/// action requires; a write that leaves any of them undefined is refused `400`
/// `BadRequestException` naming the subtype and its whole required list, and stores nothing.
#[tokio::test]
async fn a_subtype_integration_is_written_only_with_the_parameters_its_action_requires() {
    // The sentence this suite holds every refusal below to, pinned against API Gateway's own
    // wording once per SHAPE of the bracketed list — one required parameter and several. These
    // two assertions are about THIS FILE's spelling of the sentence; what holds the handler to
    // it is `assert_refused` on every row.
    assert_eq!(
        unmapped_required_parameters_message("StepFunctions-StartExecution", &["StateMachineArn"]),
        "Operation: StepFunctions-StartExecution requires enabling passthrough, or defining all \
         of the following parameters in the parameter mapping: [StateMachineArn]",
        "the refusal must be worded as API Gateway words it"
    );
    assert_eq!(
        unmapped_required_parameters_message(
            "AppConfig-GetConfiguration",
            &["Application", "Environment", "Configuration", "ClientId"]
        ),
        "Operation: AppConfig-GetConfiguration requires enabling passthrough, or defining all of \
         the following parameters in the parameter mapping: [Application, Environment, \
         Configuration, ClientId]",
        "a required list of more than one must be comma-separated inside the brackets, in that \
         subtype's own order"
    );

    let estate = Estate::new("127.0.0.1:45923", "us-west-2");
    let role_arn = estate.role("the role every integration below names").await;
    for subtype in SUBTYPES {
        let case = subtype.case;
        let expected = unmapped_required_parameters_message(subtype.name, subtype.required);
        let (api_id, _announced) = estate.create_api(case).await;

        // The empty mapping — the recorded request — then each ONE required parameter left out,
        // then every required name present with nothing behind it: refused with the sentence
        // naming the whole list, and nothing stored.
        let mut incomplete: Vec<(String, Value)> =
            vec![("with no mapping at all".to_string(), json!({}))];
        for dropped in subtype.required {
            let kept: Vec<&str> = subtype
                .required
                .iter()
                .copied()
                .filter(|parameter| parameter != dropped)
                .collect();
            incomplete.push((format!("without {dropped}"), mapping(&kept)));
        }
        let blank: serde_json::Map<String, Value> = subtype
            .required
            .iter()
            .map(|parameter| ((*parameter).to_string(), json!("")))
            .collect();
        incomplete.push((
            "with every required name left blank".to_string(),
            Value::Object(blank),
        ));

        for (variant, request_parameters) in &incomplete {
            let case = format!("{case} {variant}");
            let (status, refused) = estate
                .create_integration(&api_id, subtype.name, &role_arn, request_parameters.clone())
                .await;
            assert_refused(&case, &expected, status, &refused);
            assert_eq!(
                estate.integration_count(&case, &api_id).await,
                0,
                "case {case:?}: a refused create must store no integration"
            );
        }

        // Every required parameter defined: created, and reported back by the reader as it was
        // configured.
        let complete = mapping(subtype.required);
        let (status, created) = estate
            .create_integration(&api_id, subtype.name, &role_arn, complete.clone())
            .await;
        assert_eq!(
            status, CREATED,
            "case {case:?}: a complete mapping must be created, got {status} {created}"
        );
        let integration_id = text(case, &created, "integrationId");
        let read = estate.read_integration(case, &api_id, &integration_id).await;
        assert_eq!(
            read["requestParameters"], complete,
            "case {case:?}: GetIntegration must report the mapping that was sent, got {read}"
        );
        assert_eq!(
            read["integrationSubtype"],
            json!(subtype.name),
            "case {case:?}: GetIntegration must report the subtype, got {read}"
        );

        // The PATCH is held to the same rule: a mapping that would leave a required parameter
        // undefined is refused, and the integration keeps the mapping it had.
        for (variant, request_parameters) in &incomplete {
            let case = format!("{case} patched {variant}");
            let (status, refused) = estate
                .patch_mapping(&api_id, &integration_id, request_parameters.clone())
                .await;
            assert_refused(&case, &expected, status, &refused);
            let read = estate.read_integration(&case, &api_id, &integration_id).await;
            assert_eq!(
                read["requestParameters"], complete,
                "case {case:?}: a refused patch must leave the stored mapping alone, got {read}"
            );
        }
    }
}

/// One row of rule 2: the configuration deployed, the client that created the api, the stage and
/// the invoke URL built from the published address.
struct ReadRow {
    case: &'static str,
    /// The `Host` every control-plane call of this row carries.
    control_host: &'static str,
    region: &'static str,
    stage: &'static str,
    /// Whether the invoke URL appends the stage name to the published endpoint.
    appends_the_stage_name: bool,
    route_key: &'static str,
    path: &'static str,
    /// The bytes deployed, and the media type AppConfig is told describes them.
    content: &'static [u8],
    content_type: &'static str,
    /// The client the read is made for, and whether the mapping takes it from the request rather
    /// than carrying it as a static value.
    client_id: &'static str,
    client_id_from_the_request: bool,
}

/// The rows: the recorded case, then the same rule over another media type, another address
/// form, a named stage, bytes no text encoding describes, and a client id the request carries.
const READ_ROWS: &[ReadRow] = &[
    ReadRow {
        case: "the recorded case: a JSON configuration read at a loopback address on the default \
               stage",
        control_host: "127.0.0.1:45923",
        region: "us-west-2",
        stage: "$default",
        appends_the_stage_name: false,
        route_key: "GET /config",
        path: "/config",
        content: br#"{"marker":"first","limits":{"retries":3}}"#,
        content_type: "application/json",
        client_id: "a-device",
        client_id_from_the_request: false,
    },
    ReadRow {
        case: "a plain-text configuration on a NAMED stage whose name the invoke URL appends, \
               published on the api's own documented host",
        control_host: "apigateway.eu-central-1.amazonaws.com",
        region: "eu-central-1",
        stage: "live",
        appends_the_stage_name: true,
        route_key: "GET /settings",
        path: "/settings",
        content: b"maintenance = false\nbanner = none\n",
        content_type: "text/plain",
        client_id: "a-fleet-member",
        client_id_from_the_request: true,
    },
    ReadRow {
        case: "bytes no text encoding describes, read at a loopback address",
        control_host: "127.0.0.1:8081",
        region: "us-east-2",
        stage: "$default",
        appends_the_stage_name: false,
        route_key: "GET /blob",
        path: "/blob",
        content: &[0x00, 0x01, 0xfe, 0xff, 0x10, 0x00, 0x7f],
        content_type: "application/octet-stream",
        client_id: "a-sensor",
        client_id_from_the_request: false,
    },
];

/// The header a row whose mapping takes the client id out of the request sends it in.
const CLIENT_ID_HEADER: &str = "x-client-id";

/// RULE 2: a route whose integration names `AppConfig-GetConfiguration` performs that read in
/// AppConfig and answers the caller what that action answered, in the form AppConfig's own api
/// states it: the configuration's bytes as the whole body, its `ContentType` as `Content-Type`
/// and the version they belong to as `Configuration-Version`.
#[tokio::test]
async fn a_configuration_subtype_route_answers_the_action_s_own_payload_and_media_type() {
    for row in READ_ROWS {
        let case = row.case;
        let estate = Estate::new(row.control_host, row.region);
        let role_arn = estate.role(case).await;
        let deployed = estate.deploy(case, row.content, row.content_type).await;

        // The answer the SAME read gives a client that calls the action itself: every assertion
        // below is against this, so nothing is compared with a value written into this file.
        let directly = estate.read_directly(&deployed, row.client_id).await;
        assert_eq!(
            directly.status,
            SERVED,
            "case {case:?}: AppConfig must serve the deployed configuration, got {}",
            directly.summary()
        );
        assert_eq!(
            directly.body, row.content,
            "case {case:?}: AppConfig must answer the deployed bytes, got {}",
            directly.summary()
        );
        let expected_media_type = directly
            .header(CONTENT_TYPE_HEADER)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: AppConfig must state the media type it holds, got {}",
                    directly.summary()
                )
            })
            .to_string();
        let expected_version = directly
            .header(CONFIGURATION_VERSION_HEADER)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: AppConfig must state the version it answered, got {}",
                    directly.summary()
                )
            })
            .to_string();

        let (api_id, announced) = estate.create_api(case).await;
        let client_id = if row.client_id_from_the_request {
            format!("$request.header.{CLIENT_ID_HEADER}")
        } else {
            row.client_id.to_string()
        };
        let (status, created) = estate
            .create_integration(
                &api_id,
                "AppConfig-GetConfiguration",
                &role_arn,
                json!({
                    "Application": deployed.application_id,
                    "Environment": deployed.environment_id,
                    "Configuration": deployed.profile_id,
                    "ClientId": client_id,
                }),
            )
            .await;
        assert_eq!(
            status, CREATED,
            "case {case:?}: CreateIntegration must answer {CREATED}, got {status} {created}"
        );
        let integration_id = text(case, &created, "integrationId");
        estate
            .deploy_route(case, &api_id, &integration_id, row.route_key, row.stage)
            .await;

        let path = if row.appends_the_stage_name {
            format!("/{}{}", row.stage, row.path)
        } else {
            row.path.to_string()
        };
        let headers: Vec<(&str, &str)> = if row.client_id_from_the_request {
            vec![(CLIENT_ID_HEADER, row.client_id)]
        } else {
            Vec::new()
        };
        let answered = estate.invoke(&announced, &path, &headers).await;
        assert_eq!(
            answered.status,
            SERVED,
            "case {case:?}: {announced}{path} must answer {SERVED}, got {}",
            answered.summary()
        );
        assert_eq!(
            answered.body, row.content,
            "case {case:?}: expected the deployed configuration as the whole body, got {}",
            answered.summary()
        );
        assert_eq!(
            answered.header(CONTENT_TYPE_HEADER),
            Some(expected_media_type.as_str()),
            "case {case:?}: expected the media type AppConfig stated ({expected_media_type}), \
             got {}",
            answered.summary()
        );
        assert_eq!(
            answered.header(CONFIGURATION_VERSION_HEADER),
            Some(expected_version.as_str()),
            "case {case:?}: expected the version AppConfig answered ({expected_version}), got {}",
            answered.summary()
        );
        assert_eq!(
            expected_version, deployed.version,
            "case {case:?}: expected the deployed version to be the one in force, got {}",
            directly.summary()
        );
    }
}
