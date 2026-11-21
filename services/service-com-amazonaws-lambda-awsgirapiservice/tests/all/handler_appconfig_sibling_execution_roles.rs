// SPDX-License-Identifier: CC0-1.0

//! Two functions running the SAME handler code over the SAME deployed configuration, under
//! sibling execution roles that differ only in whether they grant AppConfig's actions — end to
//! end over the wire: AWS Lambda, the AppConfig control plane, the AppConfig data plane, IAM
//! and the token service on ONE engine, with `python3.x` handlers that `import boto3` and open
//! a configuration session.
//!
//! RULE: a call a running handler makes is made by ITS OWN function's execution role, so two
//! functions whose code and identifiers are identical answer differently at the same instant
//! when their roles differ — the one whose role grants the data plane's actions is served the
//! deployed document byte for byte, and the one whose role grants none of them is refused
//! `AccessDeniedException` (HTTP 403) and never `ResourceNotFoundException`, which is what an
//! identifier that resolves to nothing gets. That refusal is the PRINCIPAL's and not the
//! resource's: the configuration stays servable, so a caller the policies do allow — here a
//! client outside Lambda — is served the same bytes straight after the refusal.
//!
//! The rows differ along what the rule generalises over: the bytes deployed into the
//! configuration each row reads, the FORM of the grant the permitted role carries (the three
//! actions AppConfig publishes for a reader on this configuration's own ARN, two of them on
//! IAM's resource wildcard, the action wildcard), and WHICH of the two sibling functions
//! carries that grant — so the answer follows the role rather than the function.
//!
//! The sibling suite `handler_appconfig_execution_role.rs` pins the same decision taken for one
//! function as the policy in force on its role changes, and `handler_appconfig_sdk.rs` pins
//! what an authorized handler reads over both of AppConfig's APIs; what this suite adds is the
//! two roles side by side and the configuration still being served to another caller once one
//! of them has been refused.
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

/// The Region every request below is signed for, and the one the functions' invocations — and
/// therefore the clients their handlers build — run in.
const REGION: &str = "us-east-1";

/// The module and attribute the package below carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The access-key id standing for the account's own published credential, which is what every
/// request this suite makes on its own behalf presents — including the control read that must
/// still be served after a function has been refused.
const ACCOUNT_KEY: &str = "AKIDEMULATOR";

/// The name of the inline identity policy each row writes on both roles.
const INLINE_POLICY: &str = "inline";

/// The error code and status AWS states an authorization failure with.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: i64 = 403;

/// The actions AppConfig publishes for reading a configuration, as a policy statement spells
/// them.
const START_SESSION_ACTION: &str = "appconfig:StartConfigurationSession";
const GET_LATEST_ACTION: &str = "appconfig:GetLatestConfiguration";
const GET_CONFIGURATION_ACTION: &str = "appconfig:GetConfiguration";

/// The predefined deployment strategy that reaches every target at once, so a deployment's
/// rollout has begun the instant `StartDeployment` answers.
const ALL_AT_ONCE: &str = "AppConfig.AllAtOnce";

/// The `LocationUri` that selects AppConfig's own hosted configuration store.
const HOSTED: &str = "hosted";

/// How long a condition the CLOCK owes is waited for before the suite gives up — generous,
/// because nothing here is a measurement of how fast the host runs.
const PATIENCE: Duration = Duration::from_secs(60);

/// How long the suite waits between two looks at a condition it is waiting for.
const BETWEEN_LOOKS: Duration = Duration::from_millis(50);

/// Lambda, both of AppConfig's APIs, IAM and the token service over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: a handler's SDK call is dispatched back into this same
/// router and signed as the session of its own execution role, so the policies that decide it
/// are the ones this suite wrote with IAM's own operations.
///
/// `pub(crate)` so a sibling suite that needs the same five services, roles and hierarchy drives
/// this fixture rather than a second copy of it.
pub(crate) struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    pub(crate) fn new() -> Self {
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
        content_type: &str,
        body: &[u8],
    ) -> (u16, http::HeaderMap, Vec<u8>) {
        let host = format!("{host_prefix}.{REGION}.amazonaws.com");
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("authorization", Self::authorization(signing_name))
            .header("content-type", content_type)
            .body(bytes::Bytes::copy_from_slice(body))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let (parts, body) = response.into_parts();
        (parts.status.as_u16(), parts.headers, body.to_vec())
    }

    /// The same call, with the answer read as the JSON document it carries.
    pub(crate) async fn rest(
        &self,
        signing_name: &str,
        host_prefix: &str,
        method: &str,
        path: &str,
        content_type: &str,
        body: &[u8],
    ) -> (u16, http::HeaderMap, Value) {
        let (status, headers, body) = self
            .raw(signing_name, host_prefix, method, path, content_type, body)
            .await;
        let parsed: Value = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
        };
        (status, headers, parsed)
    }

    /// One AppConfig control-plane create that must be served, answering its body.
    async fn appconfig(&self, what: &str, path: &str, content_type: &str, body: &[u8]) -> Value {
        let (status, _headers, parsed) = self
            .rest("appconfig", "appconfig", "POST", path, content_type, body)
            .await;
        assert_eq!(
            status, 201,
            "setup {what}: expected HTTP 201, got {status} ({parsed})"
        );
        parsed
    }

    /// The `Id` a control-plane create minted.
    async fn created_id(&self, what: &str, path: &str, body: &Value) -> String {
        let created = self
            .appconfig(
                what,
                path,
                "application/json",
                &serde_json::to_vec(body).expect("the request body serializes"),
            )
            .await;
        created["Id"]
            .as_str()
            .unwrap_or_else(|| panic!("setup {what}: expected a minted Id, got {created}"))
            .to_string()
    }

    /// Read one deployment through AppConfig's control plane until its rollout has reached every
    /// target.
    ///
    /// Every stage of a deployment after the one `StartDeployment` answers is owed by the CLOCK,
    /// so this looks again until the condition holds instead of sleeping for a fixed span.
    /// Reaching every target is the condition a handler's read depends on: from then on the
    /// version the deployment put in service is the one AppConfig serves every caller.
    async fn await_rollout_reaching_every_target(
        &self,
        application: &str,
        environment: &str,
        number: i64,
    ) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let (status, _headers, parsed) = self
                .rest(
                    "appconfig",
                    "appconfig",
                    "GET",
                    &format!(
                        "/applications/{application}/environments/{environment}\
                         /deployments/{number}"
                    ),
                    "application/json",
                    &[],
                )
                .await;
            assert_eq!(
                status, 200,
                "setup GetDeployment: expected HTTP 200, got {status} ({parsed})"
            );
            if parsed["PercentageComplete"].as_f64().unwrap_or_default() >= 100.0 {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "setup GetDeployment: deployment {number} never reached every target, last \
                 read {parsed}"
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

    /// Create the execution role `role_name`, trusted by Lambda — written with IAM's own
    /// operations, so the role a session is minted of is the record `iam:CreateRole` wrote.
    pub(crate) async fn execution_role(&self, role_name: &str) -> String {
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
    pub(crate) async fn role_policy(&self, what: &str, role_name: &str, document: &str) {
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

    /// What `iam:GetRolePolicy` reports the inline document of `role_name` to be, URL-decoded:
    /// the suite reads the two roles back the way the operator does, so a row's claim that they
    /// differ in exactly the AppConfig statements rests on the store rather than on what was
    /// sent.
    async fn role_policy_document(&self, role_name: &str) -> String {
        let body = format!(
            "Action=GetRolePolicy&Version=2010-05-08&RoleName={}&PolicyName={INLINE_POLICY}",
            form_encode(role_name)
        );
        let request = http::Request::builder()
            .method("POST")
            .uri("https://iam.amazonaws.com/")
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", Self::authorization("iam"))
            .body(bytes::Bytes::from(body))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let answered = String::from_utf8_lossy(response.body()).to_string();
        assert_eq!(
            status, 200,
            "setup GetRolePolicy({role_name}): expected HTTP 200, got {status} {answered}"
        );
        let document = answered
            .split_once("<PolicyDocument>")
            .and_then(|(_before, rest)| rest.split_once("</PolicyDocument>"))
            .map(|(held, _after)| held.to_string())
            .unwrap_or_else(|| {
                panic!("setup GetRolePolicy({role_name}): expected a document, got {answered}")
            });
        form_decode(&document)
    }

    /// Create a function whose code is `source`, running as `role_arn`.
    async fn function(&self, name: &str, role_arn: &str, source: &str) {
        let (status, _headers, created) = self
            .rest(
                "lambda",
                "lambda",
                "POST",
                FUNCTIONS_URI,
                "application/json",
                &serde_json::to_vec(&json!({
                    "FunctionName": name,
                    "Role": role_arn,
                    "Runtime": "python3.12",
                    "Handler": HANDLER,
                    "Timeout": 25,
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
            "setup: expected HTTP 201 from CreateFunction, got {status} ({created})"
        );
    }

    /// Read the function's configuration until Lambda reports it `Active`: provisioning is a
    /// transition the CLOCK owes after the create, so this looks again until it has happened
    /// instead of sleeping for a fixed span.
    pub(crate) async fn await_function_active(&self, name: &str) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let (status, _headers, parsed) = self
                .rest(
                    "lambda",
                    "lambda",
                    "GET",
                    &format!("{FUNCTIONS_URI}/{name}/configuration"),
                    "application/json",
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
                "setup GetFunctionConfiguration({name}): the function never became Active, \
                 last read {parsed}"
            );
            tokio::time::sleep(BETWEEN_LOOKS).await;
        }
    }

    /// Invoke the function once with the triple it is to read, and answer what the handler
    /// reported — asserting the invocation itself was served and the handler ran to completion,
    /// which is what a refusal the handler CAUGHT looks like from outside.
    async fn read(&self, case: &str, function: &str, event: &Value) -> Value {
        let (status, headers, payload) = self
            .rest(
                "lambda",
                "lambda",
                "POST",
                &format!("{FUNCTIONS_URI}/{function}/invocations"),
                "application/json",
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
            "case {case:?}: {function} must run to completion, got the function error \
             {signal:?} with {payload}"
        );
        payload
    }

    /// What the data plane serves a caller OUTSIDE Lambda for this triple: a session of its own,
    /// polled once, answering the bytes the poll carried as its payload.
    pub(crate) async fn served_bytes(&self, case: &str, triple: &Hierarchy) -> Vec<u8> {
        let (status, _headers, started) = self
            .rest(
                "appconfig",
                "appconfigdata",
                "POST",
                "/configurationsessions",
                "application/json",
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
                "application/json",
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

/// The inverse of the encoder above, for the document `iam:GetRolePolicy` answers: IAM reports
/// an inline policy percent-encoded, as its own API documents. Decoded through the crate this
/// service already percent-decodes a bound URI with.
fn form_decode(value: &str) -> String {
    ::percent_encoding::percent_decode_str(value)
        .decode_utf8_lossy()
        .to_string()
}

/// One identity policy document granting `actions` on `resource`.
pub(crate) fn allow(actions: &[&str], resource: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{"Effect": "Allow", "Action": actions, "Resource": resource}],
    })
    .to_string()
}

/// The document the DENIED sibling's role carries in every row: a function's ordinary logging
/// grant and not one AppConfig action, which is the only way the two roles differ.
pub(crate) const NO_APPCONFIG_ACTION: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["logs:CreateLogGroup","logs:CreateLogStream","logs:PutLogEvents"],"Resource":"*"}]}"#;

/// The ARN of the configuration a session over this triple is served from, as AppConfig
/// addresses one — stated here independently of the crates under test, so the sentence a refusal
/// carries is compared against an ARN this suite built.
pub(crate) fn configuration_arn(triple: &Hierarchy) -> String {
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
pub(crate) fn session_principal(role_name: &str, function_name: &str) -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{role_name}/{function_name}",
        engine::emulator_account_id()
    )
}

/// The sentence IAM refuses a request with when no document in force allows the action, built
/// here from the principal, the action and the resource rather than read off the crates under
/// test.
pub(crate) fn denial(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         because no identity-based policy allows the {action} action"
    )
}

/// The handler BOTH functions run: it opens a configuration session over the three identifiers
/// the event names, polls it once, and on ANY refusal reports the error code and falls back to
/// the default compiled into it — the shape a function that reads its own feature flags has.
const HANDLER_SOURCE: &str = r#"
import json

import boto3
import botocore.exceptions

CLIENT = boto3.client("appconfigdata")


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
            "config": {"useNewBackend": False},
        }
    read = answered["Configuration"].read()
    return {"source": "appconfig", "config": json.loads(read.decode("utf-8"))}
"#;

/// The document every handler above falls back to when its own read is refused.
pub(crate) fn handler_default() -> Value {
    json!({"useNewBackend": false})
}

/// One AppConfig hierarchy, as its control plane minted it.
pub(crate) struct Hierarchy {
    pub(crate) application: String,
    pub(crate) environment: String,
    pub(crate) profile: String,
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

/// Create an application, an environment and a hosted configuration profile under `names` and
/// deploy `document` into it, all through AppConfig's own control plane — so what a handler
/// reads is what a client deployed.
pub(crate) async fn deploy(
    emulator: &Emulator,
    names: (&str, &str, &str),
    document: &str,
) -> Hierarchy {
    let (application_name, environment_name, profile_name) = names;
    let application = emulator
        .created_id(
            "CreateApplication",
            "/applications",
            &json!({"Name": application_name}),
        )
        .await;
    let environment = emulator
        .created_id(
            "CreateEnvironment",
            &format!("/applications/{application}/environments"),
            &json!({"Name": environment_name}),
        )
        .await;
    let profile = emulator
        .created_id(
            "CreateConfigurationProfile",
            &format!("/applications/{application}/configurationprofiles"),
            &json!({
                "Name": profile_name,
                "LocationUri": HOSTED,
                "Type": "AWS.Freeform",
            }),
        )
        .await;
    emulator
        .appconfig(
            "CreateHostedConfigurationVersion",
            &format!(
                "/applications/{application}/configurationprofiles/{profile}\
                 /hostedconfigurationversions"
            ),
            "application/json",
            document.as_bytes(),
        )
        .await;
    let started = emulator
        .appconfig(
            "StartDeployment",
            &format!("/applications/{application}/environments/{environment}/deployments"),
            "application/json",
            &serde_json::to_vec(&json!({
                "DeploymentStrategyId": ALL_AT_ONCE,
                "ConfigurationProfileId": profile,
                "ConfigurationVersion": "1",
            }))
            .expect("the request body serializes"),
        )
        .await;
    emulator
        .await_rollout_reaching_every_target(
            &application,
            &environment,
            started["DeploymentNumber"].as_i64().unwrap_or_default(),
        )
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

/// Which sibling a row puts the AppConfig grant on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Granted {
    First,
    Second,
}

/// One row of the table: a configuration of its own, the grant the permitted role carries for
/// it, and which sibling that role belongs to.
struct Row {
    case: &'static str,
    /// The application, environment and profile names this row's hierarchy is created under, so
    /// every row reads a configuration of its own.
    names: (&'static str, &'static str, &'static str),
    /// The bytes this row deploys into that configuration.
    deployed: &'static str,
    /// The grant the permitted role carries, built from the ARN of this row's configuration.
    grant: fn(&str) -> String,
    /// Which of the two siblings carries it this row.
    granted: Granted,
}

/// The rows: the bytes deployed, the FORM of the permitted role's grant (the three actions a
/// reader is written on this configuration's own ARN, two of them on IAM's resource wildcard,
/// the action wildcard), and which sibling carries it.
const ROWS: &[Row] = &[
    Row {
        case: "the reader's three actions on this configuration, granted to the first sibling",
        names: ("orders-siblings", "production", "backend-rollout"),
        deployed: r#"{"useNewBackend": true}"#,
        grant: |resource| {
            allow(
                &[
                    START_SESSION_ACTION,
                    GET_LATEST_ACTION,
                    GET_CONFIGURATION_ACTION,
                ],
                resource,
            )
        },
        granted: Granted::First,
    },
    Row {
        case: "the two data-plane actions on every resource, granted to the second sibling",
        names: ("checkout-siblings", "staging", "pricing-flags"),
        deployed: r#"{"useNewBackend": true, "rolloutRatio": 0.25}"#,
        grant: |_resource| allow(&[START_SESSION_ACTION, GET_LATEST_ACTION], "*"),
        granted: Granted::Second,
    },
    Row {
        case: "AppConfig's action wildcard on this configuration, granted to the first sibling",
        names: ("search-siblings", "canary", "ranking-flags"),
        deployed: r#"{"flags": {"beta": {"enabled": true}}, "values": [1, 2, 3]}"#,
        grant: |resource| allow(&["appconfig:*"], resource),
        granted: Granted::First,
    },
];

/// RULE: a handler's configuration read is decided against the identity-based policies of ITS
/// OWN function's execution role, so two functions running the same code over the same
/// identifiers answer differently when their roles differ — the granted one is served the
/// deployed document byte for byte, the one granted no AppConfig action is refused
/// `AccessDeniedException` (HTTP 403) carrying IAM's own sentence rather than the
/// `ResourceNotFoundException` a wrong identifier gets, and falls back to its own default. The
/// refusal is the principal's: the configuration is still served, byte for byte, to a caller
/// outside Lambda straight afterwards.
#[tokio::test]
async fn sibling_execution_roles_decide_a_handlers_configuration_read() {
    let _stream = one_invocation_stream_at_a_time().await;
    let emulator = Emulator::new();
    let first = Sibling {
        role_name: "lambda-appconfig-sibling-a",
        function_name: "appconfig-sibling-reader-a",
        role_arn: emulator.execution_role("lambda-appconfig-sibling-a").await,
    };
    let second = Sibling {
        role_name: "lambda-appconfig-sibling-b",
        function_name: "appconfig-sibling-reader-b",
        role_arn: emulator.execution_role("lambda-appconfig-sibling-b").await,
    };
    for sibling in [&first, &second] {
        emulator
            .function(sibling.function_name, &sibling.role_arn, HANDLER_SOURCE)
            .await;
    }
    for sibling in [&first, &second] {
        emulator.await_function_active(sibling.function_name).await;
    }

    for row in ROWS {
        let case = row.case;
        let triple = deploy(&emulator, row.names, row.deployed).await;
        let resource = configuration_arn(&triple);
        let (permitted, denied) = match row.granted {
            Granted::First => (&first, &second),
            Granted::Second => (&second, &first),
        };
        emulator
            .role_policy(case, permitted.role_name, &(row.grant)(&resource))
            .await;
        emulator
            .role_policy(case, denied.role_name, NO_APPCONFIG_ACTION)
            .await;

        // The two roles differ in exactly the AppConfig statements, read back through IAM.
        let permitted_document = emulator.role_policy_document(permitted.role_name).await;
        let denied_document = emulator.role_policy_document(denied.role_name).await;
        assert!(
            permitted_document.contains("appconfig:"),
            "case {case:?}: expected the permitted role's document to carry AppConfig \
             actions, got {permitted_document:?}"
        );
        assert!(
            !denied_document.contains("appconfig:"),
            "case {case:?}: expected the denied role's document to carry no AppConfig action, \
             got {denied_document:?}"
        );

        let event = triple.event();
        let served = emulator.read(case, permitted.function_name, &event).await;
        let expected: Value = serde_json::from_str(row.deployed).expect("the row's document");
        assert_eq!(
            served["source"],
            json!("appconfig"),
            "case {case:?}: expected the granted sibling to read AppConfig, got {served}"
        );
        assert_eq!(
            served["config"], expected,
            "case {case:?}: expected the deployed document {expected}, got {served}"
        );

        let refused = emulator.read(case, denied.function_name, &event).await;
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
            "case {case:?}: expected HTTP 403 for an unauthorized principal, got {refused}"
        );
        assert_eq!(
            refused["message"],
            json!(denial(
                &session_principal(denied.role_name, denied.function_name),
                START_SESSION_ACTION,
                &resource
            )),
            "case {case:?}: expected IAM's own denial sentence for the denied role's session, \
             got {refused}"
        );
        assert_eq!(
            refused["config"],
            handler_default(),
            "case {case:?}: expected the handler's own default document, got {refused}"
        );
        assert_ne!(
            refused["config"], served["config"],
            "case {case:?}: expected the refused sibling to report something other than the \
             deployed document, got {refused}"
        );

        // The denial is the PRINCIPAL's, not the resource's: a caller the policies do allow is
        // served the same bytes straight after it.
        let bytes = emulator.served_bytes(case, &triple).await;
        assert_eq!(
            bytes,
            row.deployed.as_bytes(),
            "case {case:?}: expected the configuration to still be served byte for byte, got \
             {:?}",
            String::from_utf8_lossy(&bytes)
        );
    }
}
