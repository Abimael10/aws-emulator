// SPDX-License-Identifier: CC0-1.0

//! A synchronous `Invoke` of a `python3.x` function whose handler reads its own AppConfig
//! configuration, over the deployment strategies a CLIENT declares for itself — end to end
//! over the wire: AWS Lambda, the AppConfig control plane, the AppConfig data plane, IAM and
//! the token service on ONE engine.
//!
//! RULE: a `RequestResponse` invocation RUNS the resolved version's deployment package and
//! answers HTTP 200 with the handler's own payload and no `X-Amz-Function-Error`; every AWS
//! call that running handler makes is decided against the identity-based policies in force on
//! ITS OWN function's execution role; and the configuration version a deployment put in force
//! is the one every reader is served once the rollout has reached its targets — whichever
//! strategy paced it, one the client created or one AppConfig publishes, and a strategy that
//! paces no stage at all (no rollout window and no final bake time) has reached them when
//! `StartDeployment` answers. So the function whose role grants the data plane's actions
//! answers with the deployed document byte for byte, and its sibling, same code and same
//! identifiers, whose role grants no AppConfig action is refused `AccessDeniedException`
//! (HTTP 403) carrying IAM's own sentence and answers with the fallback it caught that refusal
//! into.
//!
//! The rows differ along what the rule generalises over: the FORM of the strategy the
//! deployment is paced by — a client-created `LINEAR` one that paces nothing, a client-created
//! `EXPONENTIAL` one, a client-created one that leaves `GrowthType` and `ReplicateTo` unstated,
//! and the predefined strategy AppConfig publishes, whose id is the other form the member's own
//! pattern allows — and the document each row deploys. Every row asserts the deployment is
//! governed by the pacing its own strategy reports
//! (`smithy_query(query="operation",operation="CreateDeploymentStrategy")` names those four
//! members), and holds the handler's answer against the bytes a CLIENT polls out of the data
//! plane, so a document the runtime composed rather than read would fail here.
//!
//! What the neighbours pin and this one does not repeat: `handler_appconfig_sdk.rs` drives both
//! of AppConfig's APIs for an authorized handler, `handler_appconfig_execution_role.rs` the
//! same decision taken for one function as the policy in force on its role changes, and
//! `handler_appconfig_sibling_execution_roles.rs` the two sibling roles over the predefined
//! strategy; what this suite adds is that neither the run nor the authorization depends on
//! which strategy put the configuration in force.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]): this emulator serves at
//! most `max_concurrent_runs` invocations at once for the whole process.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::invoke_request_id::one_invocation_stream_at_a_time;
// The one .zip writer this crate's suites package a handler with, and the one awsQuery value
// encoder they reach for, each exported by the suite that first needed it rather than written
// again here.
use crate::handler_s3_object_access::form_encode;
use crate::node_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The Region every request here is signed for, and so the Region the hierarchy, the roles and
/// the functions belong to.
const REGION: &str = "us-east-1";

/// The module and attribute the package below carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The point release of the runtime both functions run under.
const RUNTIME: &str = "python3.12";

/// The access-key id standing for the account's own published credential, which is what every
/// request this suite makes on its own behalf presents.
const ACCOUNT_KEY: &str = "AKIDEMULATOR";

/// The name of the inline identity policy this suite writes on both roles.
const INLINE_POLICY: &str = "inline";

/// The error code and status AWS states an authorization failure with. The data plane's model
/// declares no authorization error, so this is the AWS-common refusal every service answers a
/// request its policies do not permit.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: i64 = 403;

/// The two actions AppConfig publishes the data plane's operations under, which is how a policy
/// statement spells them.
const START_SESSION_ACTION: &str = "appconfig:StartConfigurationSession";
const GET_LATEST_ACTION: &str = "appconfig:GetLatestConfiguration";

/// The document the DENIED sibling's role carries throughout: a function's ordinary logging
/// grant and not one AppConfig action, which is the only way the two roles differ.
const NO_APPCONFIG_ACTION: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["logs:CreateLogGroup","logs:CreateLogStream","logs:PutLogEvents"],"Resource":"*"}]}"#;

/// The `LocationUri` that selects AppConfig's own hosted configuration store.
const HOSTED: &str = "hosted";

/// The predefined deployment strategy AppConfig publishes for reaching every target at once,
/// whose id is the `AppConfig.<Name>` form `DeploymentStrategyId`'s own pattern allows beside a
/// minted one.
const ALL_AT_ONCE: &str = "AppConfig.AllAtOnce";

/// The media type the documents below are written and served with.
const JSON_CONTENT_TYPE: &str = "application/json";

/// How long a condition the CLOCK owes is waited for before the suite gives up — generous,
/// because nothing here is a measurement of how fast the host runs.
const PATIENCE: Duration = Duration::from_secs(120);

/// How long the suite waits between two looks at a condition it is waiting for.
const BETWEEN_LOOKS: Duration = Duration::from_millis(50);

/// The timeout the functions are created with, in seconds.
const FUNCTION_TIMEOUT: i64 = 25;

/// Lambda, both of AppConfig's APIs, IAM and the token service over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: a handler's SDK call is dispatched back into this same
/// router and signed as the session of its own execution role, so the policies that decide it
/// are the ones this suite wrote with IAM's own operations.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite signs nothing: the `Authorization` header carries only the credential
        // scope, which is where a service reads the Region and the caller's access key id.
        // The handler's OWN call is signed by the runtime, with its execution role's session.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_appconfig_amazonappconfig::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_appconfigdata_appconfigdata::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
                engine,
            ),
        );
        Self { router }
    }

    /// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
    /// request and where the Region comes from. The signature is never checked here.
    fn authorization(service: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}/{service}/\
             aws4_request, SignedHeaders=host, Signature=not-verified"
        )
    }

    /// One `restJson1` call to the service answering at the endpoint host `host_prefix`: its
    /// status, its headers and the bytes it answered, untouched — which is what a member bound
    /// to the whole response body is read from.
    async fn raw(
        &self,
        signing_name: &str,
        host_prefix: &str,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> (u16, http::HeaderMap, Vec<u8>) {
        let host = format!("{host_prefix}.{REGION}.amazonaws.com");
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("authorization", Self::authorization(signing_name))
            .header("content-type", JSON_CONTENT_TYPE)
            .body(bytes::Bytes::copy_from_slice(body))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let (parts, body) = response.into_parts();
        (parts.status.as_u16(), parts.headers, body.to_vec())
    }

    /// The same call, with the answer read as the JSON document it carries.
    async fn rest(
        &self,
        signing_name: &str,
        host_prefix: &str,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> (u16, http::HeaderMap, Value) {
        let (status, headers, body) = self.raw(signing_name, host_prefix, method, path, body).await;
        let parsed: Value = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
        };
        (status, headers, parsed)
    }

    /// One AppConfig control-plane call that must answer `expected`, with its parsed body.
    async fn appconfig(
        &self,
        what: &str,
        expected: u16,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> Value {
        let (status, _headers, parsed) = self
            .rest("appconfig", "appconfig", method, path, body)
            .await;
        assert_eq!(
            status, expected,
            "setup {what}: expected HTTP {expected} from {method} {path}, got {status} ({parsed})"
        );
        parsed
    }

    /// The `Id` a control-plane create minted.
    async fn created_id(&self, what: &str, path: &str, body: &Value) -> String {
        let created = self
            .appconfig(
                what,
                201,
                "POST",
                path,
                &serde_json::to_vec(body).expect("the request body serializes"),
            )
            .await;
        created["Id"]
            .as_str()
            .unwrap_or_else(|| panic!("setup {what}: expected a minted Id, got {created}"))
            .to_string()
    }

    /// Write `document` as a hosted configuration version of this profile and answer the
    /// version number the control plane minted for it.
    ///
    /// `CreateHostedConfigurationVersion` binds `Content` to the whole payload and
    /// `VersionNumber` to the `Version-Number` header
    /// (`smithy_query(query="operation",operation="CreateHostedConfigurationVersion")`), so the
    /// answer is read out of the header and the stored bytes out of the body.
    async fn hosted_version(
        &self,
        case: &str,
        application: &str,
        profile: &str,
        document: &str,
    ) -> i64 {
        let (status, headers, body) = self
            .raw(
                "appconfig",
                "appconfig",
                "POST",
                &format!(
                    "/applications/{application}/configurationprofiles/{profile}\
                     /hostedconfigurationversions"
                ),
                document.as_bytes(),
            )
            .await;
        assert_eq!(
            status,
            201,
            "setup {case:?}: expected HTTP 201 from CreateHostedConfigurationVersion, got \
             {status} ({})",
            String::from_utf8_lossy(&body)
        );
        assert_eq!(
            body,
            document.as_bytes(),
            "setup {case:?}: expected the stored version to carry the written bytes, got {:?}",
            String::from_utf8_lossy(&body)
        );
        headers
            .get("version-number")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or_else(|| {
                panic!("setup {case:?}: expected a minted version number, got {headers:?}")
            })
    }

    /// Read one deployment through AppConfig's control plane until its rollout has reached
    /// every target.
    ///
    /// Every stage of a deployment after the one `StartDeployment` answers is owed by the
    /// CLOCK, so this looks again until the condition holds instead of sleeping for a fixed
    /// span. Reaching every target is the condition a reader's answer depends on: from then on
    /// the version the deployment put in service is the one AppConfig serves every caller,
    /// whether the deployment is still monitoring a bake time or already complete.
    async fn await_rollout_reaching_every_target(
        &self,
        case: &str,
        application: &str,
        environment: &str,
        number: i64,
    ) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let read = self
                .appconfig(
                    case,
                    200,
                    "GET",
                    &format!(
                        "/applications/{application}/environments/{environment}\
                         /deployments/{number}"
                    ),
                    &[],
                )
                .await;
            if read["PercentageComplete"].as_f64().unwrap_or_default() >= 100.0 {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: deployment {number} never reached every target, last read {read}"
            );
            tokio::time::sleep(BETWEEN_LOOKS).await;
        }
    }

    /// One awsQuery call to IAM that must be served.
    async fn iam(&self, what: &str, action: &str, params: &[(&str, String)]) {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in params {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri("https://iam.amazonaws.com/")
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", Self::authorization("iam"))
            .body(bytes::Bytes::from(body))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        assert_eq!(
            status,
            200,
            "setup {what}: IAM {action} must succeed, got {status} {}",
            String::from_utf8_lossy(response.body())
        );
    }

    /// Create the execution role `role_name`, trusted by Lambda and carrying no AppConfig
    /// action yet — written with IAM's own operations, so the role a session is minted of is the
    /// record `iam:CreateRole` wrote.
    async fn execution_role(&self, role_name: &str) -> String {
        let trust = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": {"Service": "lambda.amazonaws.com"},
                "Action": "sts:AssumeRole",
            }],
        })
        .to_string();
        self.iam(
            "the execution role",
            "CreateRole",
            &[
                ("RoleName", role_name.to_string()),
                ("AssumeRolePolicyDocument", trust),
            ],
        )
        .await;
        self.role_policy("the execution role", role_name, NO_APPCONFIG_ACTION)
            .await;
        format!(
            "arn:aws:iam::{}:role/{role_name}",
            engine::emulator_account_id()
        )
    }

    /// The identity policy in force on `role_name` from now on, so a row is about the grant in
    /// force when the handler calls rather than the one in force when the role was made.
    async fn role_policy(&self, what: &str, role_name: &str, document: &str) {
        self.iam(
            what,
            "PutRolePolicy",
            &[
                ("RoleName", role_name.to_string()),
                ("PolicyName", INLINE_POLICY.to_string()),
                ("PolicyDocument", document.to_string()),
            ],
        )
        .await;
    }

    /// Create a function whose deployment package holds `source`, running as `role_arn`.
    async fn function(&self, name: &str, role_arn: &str, source: &str) {
        let (status, _headers, created) = self
            .rest(
                "lambda",
                "lambda",
                "POST",
                FUNCTIONS_URI,
                &serde_json::to_vec(&json!({
                    "FunctionName": name,
                    "Role": role_arn,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {
                        "ZipFile": base64::engine::general_purpose::STANDARD
                            .encode(package(&[("lambda_function.py", source)])),
                    },
                }))
                .expect("the create body serializes"),
            )
            .await;
        assert_eq!(
            status, 201,
            "setup: expected HTTP 201 from CreateFunction({name}), got {status} ({created})"
        );
    }

    /// Read the function's configuration until Lambda reports it `Active`: provisioning is a
    /// transition the CLOCK owes after the create, so this looks again until it has happened
    /// instead of sleeping for a fixed span.
    async fn await_function_active(&self, name: &str) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let (status, _headers, parsed) = self
                .rest(
                    "lambda",
                    "lambda",
                    "GET",
                    &format!("{FUNCTIONS_URI}/{name}/configuration"),
                    &[],
                )
                .await;
            assert_eq!(
                status, 200,
                "setup GetFunctionConfiguration({name}): expected HTTP 200, got {status} \
                 ({parsed})"
            );
            if parsed["State"] == json!("Active") {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "setup GetFunctionConfiguration({name}): the function never became Active, last \
                 read {parsed}"
            );
            tokio::time::sleep(BETWEEN_LOOKS).await;
        }
    }

    /// Invoke `function` synchronously with the triple it is to read, and answer what the
    /// handler reported — asserting the invocation itself was served HTTP 200 and that the
    /// handler ran to completion, which is what a refusal the handler CAUGHT looks like from
    /// outside.
    async fn read(&self, case: &str, function: &str, event: &Value) -> Value {
        let (status, headers, payload) = self
            .rest(
                "lambda",
                "lambda",
                "POST",
                &format!("{FUNCTIONS_URI}/{function}/invocations"),
                event.to_string().as_bytes(),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from Invoke of {function}, got {status} ({payload})"
        );
        let signal = headers
            .get("x-amz-function-error")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        assert_eq!(
            signal, None,
            "case {case:?}: {function} must run to completion, got the function error {signal:?} \
             with {payload}"
        );
        payload
    }

    /// What the data plane serves a caller OUTSIDE Lambda for this triple: a session of its
    /// own, polled once, answering the bytes the poll carried as its payload.
    async fn served_bytes(&self, case: &str, triple: &Hierarchy) -> Vec<u8> {
        let (status, _headers, started) = self
            .rest(
                "appconfig",
                "appconfigdata",
                "POST",
                "/configurationsessions",
                &serde_json::to_vec(&json!({
                    "ApplicationIdentifier": triple.application,
                    "EnvironmentIdentifier": triple.environment,
                    "ConfigurationProfileIdentifier": triple.profile,
                }))
                .expect("the request body serializes"),
            )
            .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected HTTP 201 from StartConfigurationSession, got {status} \
             ({started})"
        );
        let token = started["InitialConfigurationToken"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected an initial configuration token, got {started}")
            })
            .to_string();
        let (status, _headers, body) = self
            .raw(
                "appconfig",
                "appconfigdata",
                "GET",
                &format!("/configuration?configuration_token={token}"),
                &[],
            )
            .await;
        assert_eq!(
            status,
            200,
            "case {case:?}: expected HTTP 200 from GetLatestConfiguration, got {status} ({})",
            String::from_utf8_lossy(&body)
        );
        body
    }
}

/// One identity policy document granting `actions` on `resource`.
fn allow(actions: &[&str], resource: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{"Effect": "Allow", "Action": actions, "Resource": resource}],
    })
    .to_string()
}

/// The ARN of the configuration a session over this triple is served from, as AppConfig
/// addresses one — stated here independently of the crates under test, so the sentence a
/// refusal carries is compared against an ARN this suite built.
fn configuration_arn(triple: &Hierarchy) -> String {
    format!(
        "arn:aws:appconfig:{REGION}:{}:application/{}/environment/{}/configuration/{}",
        engine::emulator_account_id(),
        triple.application,
        triple.environment,
        triple.profile
    )
}

/// The assumed-role session a function's own calls are made as:
/// `arn:aws:sts::<account>:assumed-role/<role name>/<function name>`, the identity AWS reports
/// for a function's calls.
fn session_principal(role_name: &str, function_name: &str) -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{role_name}/{function_name}",
        engine::emulator_account_id()
    )
}

/// The sentence IAM refuses a request with when no document in force allows the action, built
/// here from the principal, the action and the resource rather than read off the crates under
/// test.
fn denial(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         because no identity-based policy allows the {action} action"
    )
}

/// The handler BOTH functions run: it opens a configuration session over the three identifiers
/// the event names, polls it once, and on ANY refusal reports the error code, its status and its
/// sentence and falls back to the default compiled into it — the shape a function that reads its
/// own feature flags has.
const HANDLER_SOURCE: &str = r#"
import json

import boto3
import botocore.exceptions

CLIENT = boto3.client("appconfigdata")

DEFAULT_CONFIG = {"useNewBackend": False}


def lambda_handler(event, context):
    try:
        session = CLIENT.start_configuration_session(
            ApplicationIdentifier=event["application"],
            EnvironmentIdentifier=event["environment"],
            ConfigurationProfileIdentifier=event["profile"],
        )
        answered = CLIENT.get_latest_configuration(
            ConfigurationToken=session["InitialConfigurationToken"]
        )
    except botocore.exceptions.ClientError as error:
        return {
            "source": "default",
            "code": error.response["Error"]["Code"],
            "message": error.response["Error"]["Message"],
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
            "config": DEFAULT_CONFIG,
        }
    read = answered["Configuration"].read()
    return {"source": "appconfig", "config": json.loads(read.decode("utf-8"))}
"#;

/// The document the handler above falls back to when its own read is refused, written here so a
/// row's expectation is the same text the handler carries as its `DEFAULT_CONFIG`.
fn handler_default() -> Value {
    json!({"useNewBackend": false})
}

/// One AppConfig hierarchy, as its control plane minted it.
struct Hierarchy {
    application: String,
    environment: String,
    profile: String,
}

impl Hierarchy {
    /// The event a handler is given the triple in.
    fn event(&self) -> Value {
        json!({
            "application": self.application,
            "environment": self.environment,
            "profile": self.profile,
        })
    }
}

/// The strategy a row's deployment is paced by.
enum StrategyForm {
    /// One AppConfig publishes, named by the `AppConfig.<Name>` id form.
    Predefined(&'static str),
    /// One the CLIENT creates, as `CreateDeploymentStrategy` takes it: the required
    /// `DeploymentDurationInMinutes` and `GrowthFactor`, and the `GrowthType`, `ReplicateTo` and
    /// `FinalBakeTimeInMinutes` a client may leave unstated
    /// (`smithy_query(query="operation",operation="CreateDeploymentStrategy")`).
    Created {
        name: &'static str,
        duration_in_minutes: i64,
        growth_factor: f64,
        growth_type: Option<&'static str>,
        final_bake_time_in_minutes: Option<i64>,
        replicate_to: Option<&'static str>,
    },
}

impl StrategyForm {
    /// The id this strategy is deployed through: the published one as it stands, and a
    /// client-created one as the control plane minted it.
    async fn id(&self, emulator: &Emulator, case: &str) -> String {
        match self {
            Self::Predefined(id) => (*id).to_string(),
            Self::Created {
                name,
                duration_in_minutes,
                growth_factor,
                growth_type,
                final_bake_time_in_minutes,
                replicate_to,
            } => {
                let mut body = serde_json::Map::new();
                body.insert("Name".to_string(), json!(name));
                body.insert(
                    "DeploymentDurationInMinutes".to_string(),
                    json!(duration_in_minutes),
                );
                body.insert("GrowthFactor".to_string(), json!(growth_factor));
                if let Some(growth_type) = growth_type {
                    body.insert("GrowthType".to_string(), json!(growth_type));
                }
                if let Some(minutes) = final_bake_time_in_minutes {
                    body.insert("FinalBakeTimeInMinutes".to_string(), json!(minutes));
                }
                if let Some(replicate_to) = replicate_to {
                    body.insert("ReplicateTo".to_string(), json!(replicate_to));
                }
                let created = emulator
                    .created_id(case, "/deploymentstrategies", &Value::Object(body))
                    .await;
                // `DeploymentStrategy$Id` is minted under the pattern `^[a-z0-9]{4,7}$`, which
                // is also the form `StartDeployment` accepts beside a published id.
                assert!(
                    (4..=7).contains(&created.len())
                        && created
                            .chars()
                            .all(|held| held.is_ascii_lowercase() || held.is_ascii_digit()),
                    "case {case:?}: expected a minted strategy id of four to seven lowercase \
                     alphanumerics, got {created:?}"
                );
                created
            }
        }
    }
}

/// One row: the strategy form its deployment is paced by, the names its own hierarchy is
/// created under, and the document it deploys into it.
struct Row {
    case: &'static str,
    strategy: StrategyForm,
    names: (&'static str, &'static str, &'static str),
    deployed: &'static str,
}

/// The rows: the four forms of a strategy that has reached every target by the time a reader
/// calls — three the client declares for itself, one AppConfig publishes — each over a document
/// of its own.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a client-created linear strategy that paces no stage at all",
            strategy: StrategyForm::Created {
                name: "paced-linear-at-once",
                duration_in_minutes: 0,
                growth_factor: 100.0,
                growth_type: Some("LINEAR"),
                final_bake_time_in_minutes: Some(0),
                replicate_to: Some("NONE"),
            },
            names: ("orders-paced-linear", "production", "backend-rollout"),
            deployed: r#"{"useNewBackend": true}"#,
        },
        Row {
            case: "a client-created exponential strategy over an unpaced window",
            strategy: StrategyForm::Created {
                name: "paced-exponential-at-once",
                duration_in_minutes: 0,
                growth_factor: 10.0,
                growth_type: Some("EXPONENTIAL"),
                final_bake_time_in_minutes: Some(0),
                replicate_to: Some("NONE"),
            },
            names: ("checkout-paced-exponential", "staging", "pricing-flags"),
            deployed: r#"{"useNewBackend": true, "rolloutRatio": 0.25}"#,
        },
        Row {
            case: "a client-created strategy leaving growth type, bake time and replication \
                   unstated",
            strategy: StrategyForm::Created {
                name: "paced-defaults",
                duration_in_minutes: 0,
                growth_factor: 25.0,
                growth_type: None,
                final_bake_time_in_minutes: None,
                replicate_to: None,
            },
            names: ("search-paced-defaults", "canary", "ranking-flags"),
            deployed: r#"{"flags": {"beta": {"enabled": true}}, "values": [1, 2, 3]}"#,
        },
        Row {
            case: "the strategy AppConfig publishes for reaching every target at once",
            strategy: StrategyForm::Predefined(ALL_AT_ONCE),
            names: ("billing-published-strategy", "production", "invoice-flags"),
            deployed: r#"{"useNewBackend": true, "tier": "gold"}"#,
        },
    ]
}

/// The four members a strategy paces a deployment by, which a deployment reports as its own.
const PACING_MEMBERS: [&str; 4] = [
    "DeploymentDurationInMinutes",
    "GrowthType",
    "GrowthFactor",
    "FinalBakeTimeInMinutes",
];

/// Create an application, an environment and a hosted configuration profile under `names`,
/// write `document` as a hosted configuration version and deploy it through `strategy` — all
/// through AppConfig's own control plane, so what a handler reads is what a client deployed.
///
/// On the way, the deployment is held against the strategy it names: a deployment reports the
/// pacing of its own strategy, which is what makes the row about THAT strategy.
async fn deploy(emulator: &Emulator, row: &Row) -> Hierarchy {
    let case = row.case;
    let (application_name, environment_name, profile_name) = row.names;
    let strategy_id = row.strategy.id(emulator, case).await;
    let strategy = emulator
        .appconfig(
            case,
            200,
            "GET",
            &format!("/deploymentstrategies/{strategy_id}"),
            &[],
        )
        .await;

    let application = emulator
        .created_id(case, "/applications", &json!({"Name": application_name}))
        .await;
    let environment = emulator
        .created_id(
            case,
            &format!("/applications/{application}/environments"),
            &json!({"Name": environment_name}),
        )
        .await;
    let profile = emulator
        .created_id(
            case,
            &format!("/applications/{application}/configurationprofiles"),
            &json!({
                "Name": profile_name,
                "LocationUri": HOSTED,
                "Type": "AWS.Freeform",
            }),
        )
        .await;
    let version_number = emulator
        .hosted_version(case, &application, &profile, row.deployed)
        .await;

    let started = emulator
        .appconfig(
            case,
            201,
            "POST",
            &format!("/applications/{application}/environments/{environment}/deployments"),
            &serde_json::to_vec(&json!({
                "DeploymentStrategyId": strategy_id,
                "ConfigurationProfileId": profile,
                "ConfigurationVersion": version_number.to_string(),
            }))
            .expect("the request body serializes"),
        )
        .await;
    assert_eq!(
        started["DeploymentStrategyId"],
        json!(strategy_id),
        "case {case:?}: expected the deployment to name the strategy it was started through, \
         got {started}"
    );
    for member in PACING_MEMBERS {
        assert_eq!(
            started[member], strategy[member],
            "case {case:?}: expected the deployment's {member} to be the strategy's own \
             {}, got {}",
            strategy[member], started[member]
        );
    }
    let number = started["DeploymentNumber"].as_i64().unwrap_or_else(|| {
        panic!("setup {case:?}: expected StartDeployment to mint a deployment, got {started}")
    });
    emulator
        .await_rollout_reaching_every_target(case, &application, &environment, number)
        .await;

    Hierarchy {
        application,
        environment,
        profile,
    }
}

/// One of the two sibling functions: the same code, under a role of its own.
struct Sibling {
    role_name: &'static str,
    function_name: &'static str,
    role_arn: String,
}

/// RULE: a synchronous `Invoke` runs the deployment package and answers the handler's own
/// payload, and the handler's own configuration read is decided against the identity-based
/// policies of ITS function's execution role — whichever deployment strategy put the
/// configuration in force, one the client created or one AppConfig publishes. So the granted
/// sibling is served the deployed document byte for byte and the sibling granted no AppConfig
/// action is refused `AccessDeniedException` (HTTP 403) carrying IAM's own sentence, and
/// answers with the fallback it caught that refusal into.
#[tokio::test]
async fn a_handlers_configuration_read_is_decided_whatever_strategy_deployed_it() {
    let _stream = one_invocation_stream_at_a_time().await;
    let emulator = Emulator::new();
    let granted = Sibling {
        role_name: "lambda-appconfig-paced-granted",
        function_name: "appconfig-paced-reader-granted",
        role_arn: emulator
            .execution_role("lambda-appconfig-paced-granted")
            .await,
    };
    let ungranted = Sibling {
        role_name: "lambda-appconfig-paced-ungranted",
        function_name: "appconfig-paced-reader-ungranted",
        role_arn: emulator
            .execution_role("lambda-appconfig-paced-ungranted")
            .await,
    };
    for sibling in [&granted, &ungranted] {
        emulator
            .function(sibling.function_name, &sibling.role_arn, HANDLER_SOURCE)
            .await;
    }
    for sibling in [&granted, &ungranted] {
        emulator.await_function_active(sibling.function_name).await;
    }

    for row in rows() {
        let case = row.case;
        let triple = deploy(&emulator, &row).await;
        let resource = configuration_arn(&triple);
        // The granted role reaches the data plane's two actions on THIS row's configuration,
        // and the other role keeps the logging-only document it was created with.
        emulator
            .role_policy(
                case,
                granted.role_name,
                &allow(&[START_SESSION_ACTION, GET_LATEST_ACTION], &resource),
            )
            .await;

        // What a CLIENT reads out of the data plane itself: the control the handler's answer
        // is held against, so a document the runtime composed rather than read would fail.
        let as_a_client_reads_it = emulator.served_bytes(case, &triple).await;
        assert_eq!(
            as_a_client_reads_it,
            row.deployed.as_bytes(),
            "case {case:?}: expected the data plane to serve the deployed document byte for \
             byte, got {:?}",
            String::from_utf8_lossy(&as_a_client_reads_it)
        );
        let expected: Value = serde_json::from_slice(&as_a_client_reads_it)
            .expect("the document the data plane served is JSON");

        let event = triple.event();
        let served = emulator.read(case, granted.function_name, &event).await;
        assert_eq!(
            served["source"],
            json!("appconfig"),
            "case {case:?}: expected the granted sibling to read AppConfig, got {served}"
        );
        assert_eq!(
            served["config"], expected,
            "case {case:?}: expected the document the data plane serves a client, got {served}"
        );

        let refused = emulator.read(case, ungranted.function_name, &event).await;
        assert_eq!(
            refused["source"],
            json!("default"),
            "case {case:?}: expected the sibling granted no AppConfig action to fall back to \
             its default, got {refused}"
        );
        assert_eq!(
            refused["code"],
            json!(ACCESS_DENIED),
            "case {case:?}: expected the authorization refusal rather than a not-found one, \
             got {refused}"
        );
        assert_eq!(
            refused["status"],
            json!(ACCESS_DENIED_STATUS),
            "case {case:?}: expected HTTP {ACCESS_DENIED_STATUS} for an unauthorized \
             principal, got {refused}"
        );
        assert_eq!(
            refused["message"],
            json!(denial(
                &session_principal(ungranted.role_name, ungranted.function_name),
                START_SESSION_ACTION,
                &resource
            )),
            "case {case:?}: expected IAM's own denial sentence for the ungranted role's \
             session, got {refused}"
        );
        assert_eq!(
            refused["config"],
            handler_default(),
            "case {case:?}: expected the handler's own default document, got {refused}"
        );
    }
}
