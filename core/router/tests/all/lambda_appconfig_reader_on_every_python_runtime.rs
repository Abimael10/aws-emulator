// SPDX-License-Identifier: CC0-1.0

//! A `python3.x` Lambda handler that reads its own AppConfig configuration through the AWS SDK
//! its runtime supplies, driven over the ASSEMBLED router: CloudFormation deploys the execution
//! roles and the functions, IAM holds the roles, the token service mints the session the
//! handler's own call is signed with, Lambda runs the handler, and both of AppConfig's APIs
//! answer it.
//!
//! RULE: a managed `python3.x` runtime carries the AWS SDK on the function's import path, so a
//! module-scope `import boto3` resolves during initialization whatever the point release of the
//! runtime, and the invocation is answered HTTP 200 with the handler's own payload and NO
//! `X-Amz-Function-Error` (`Invoke` binds that member to the `X-Amz-Function-Error` header, so
//! a module that could not be imported is reported there, before a single line of handler code
//! has run). A service that refuses the handler's call does not change that: the refusal
//! reaches the guest as `botocore.exceptions.ClientError` carrying that service's OWN error
//! code, status and sentence, so a handler written to catch its own configuration failure
//! answers with its fallback document — which it can only do if the SDK it imports is there to
//! raise.
//!
//! Only a cross-service suite can state that: the configuration is AppConfig's record, the
//! deployed roles are CloudFormation's and IAM's, the session a handler's call is signed as is
//! the token service's, and the import path and the run are Lambda's. What the neighbours pin
//! and this one does not repeat: the Lambda crate's `python_runtime_bundled_sdk` drives the
//! runtime and import-form axes against Amazon DynamoDB, its `handler_appconfig_sdk` drives
//! both of AppConfig's APIs on a hand-picked three-service router, and
//! `appconfig_signing_name_routing` pins which service claims each endpoint without running a
//! handler.
//!
//! The rows differ along what the rule generalises over: WHICH point release of the runtime
//! runs the module (four `python3.x` identifiers of the model's `Runtime` value set), WHAT the
//! handler's configuration read meets (the deployed configuration, an identifier naming
//! nothing at every level, an identifier naming nothing at one level only, an execution role
//! granted none of AppConfig's actions), and WHICH resource the role's grant names (the
//! configuration's own ARN, every resource, nothing) — and therefore which of the two faults
//! the handler's own `except` reports. Every row asserts the same two invariants — HTTP 200 and
//! no function error — and the served rows are compared against the bytes a CLIENT polls out of
//! the data plane itself, so a document the runtime composed rather than read would fail here.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::query_wire::{elements, form_encode, opt_element, physical_ids_of};

/// The Region every request here is signed for, and so the Region the hierarchy, the roles and
/// the functions belong to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which the configuration's ARN and every session's ARN
/// carry.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The names the AppConfig hierarchy is created under.
const APPLICATION_NAME: &str = "xsvc-appconfig-orders-flags";
const ENVIRONMENT_NAME: &str = "production";
const PROFILE_NAME: &str = "backend-rollout";

/// The `LocationUri` that selects AppConfig's own hosted configuration store.
const HOSTED: &str = "hosted";

/// The predefined deployment strategy that reaches every target at once, so the rollout has
/// begun the instant `StartDeployment` answers.
const ALL_AT_ONCE: &str = "AppConfig.AllAtOnce";

/// The flag document the deployment puts in force, which is what a served read answers with.
const DEPLOYED: &str = r#"{"useNewBackend": true}"#;

/// The document [`READER_HANDLER`] falls back on, written here so a row's expectation is the
/// same text the handler carries as its `DEFAULT_CONFIG`.
const HANDLER_DEFAULT: &str = r#"{"useNewBackend": false}"#;

/// The media type the document above is written and served with.
const JSON_CONTENT_TYPE: &str = "application/json";

/// The two actions AppConfig publishes the data plane's operations under, which is how a policy
/// statement spells them.
const START_SESSION_ACTION: &str = "appconfig:StartConfigurationSession";
const GET_CONFIGURATION_ACTION: &str = "appconfig:GetLatestConfiguration";

/// The modelled refusal AppConfig's data plane answers an identifier that resolves to nothing
/// with (`ResourceNotFoundException`, HTTP 404 — both of its operations declare it).
const NOT_FOUND: &str = "ResourceNotFoundException";
const NOT_FOUND_STATUS: i64 = 404;

/// The code and status an authorization refusal carries. The data plane's model declares no
/// authorization error, so this is the AWS-common refusal every service answers a request its
/// policies do not permit.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: i64 = 403;

/// The `StackStatus` the functions' stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is what the handler reads and not how fast an
/// interpreter gets through it.
const FUNCTION_TIMEOUT: u64 = 120;

/// How often to look while waiting for work the clock owes — a stack reaching
/// `CREATE_COMPLETE`, a function reaching `Active`, a deployment's rollout reaching its targets
/// — and how many looks to take. Those spans are the owning services' own, so nothing here
/// sleeps a fixed span on their behalf, and the deadline is generous enough that a loaded host
/// cannot decide the outcome: this suite runs beside every other test of the workspace, each
/// holding the same host.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_800;

/// The statuses Lambda answers an invocation it has NOT served with, each of which says "not
/// now" and carries no answer from the handler: the concurrency the process can hold at once was
/// full (`TooManyRequestsException`, HTTP 429) or the function was not `Active` yet
/// (`ResourceConflictException`, HTTP 409). Whether one arrives is decided by what the rest of
/// this process is running at that instant and by nothing a row varies, so a row LOOKS AGAIN on
/// one rather than reading a configuration out of a run that never happened — and a refusal that
/// still stands at the deadline fails the row, carrying what it answered with.
const UNSERVED: &[u16] = &[409, 429];

/// How long a row waits for Lambda to serve its invocation, read as a DEADLINE because one look
/// of this wait is a whole run rather than a cheap read.
const SERVED_DEADLINE: Duration = Duration::from_secs(180);

/// The handler every function below runs: the SDK is imported and the client built at MODULE
/// scope, so a runtime that did not carry the SDK could not initialize the module at all; the
/// read is wrapped in the handler's own `except`, which is what a feature-flagged function does
/// so that a configuration it cannot reach leaves it serving its compiled-in default.
const READER_HANDLER: &str = r#"
import json
import os

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
            "ran_under": os.environ["AWS_EXECUTION_ENV"],
        }
    read = answered["Configuration"].read()
    return {
        "source": "appconfig",
        "config": json.loads(read.decode("utf-8")),
        "ran_under": os.environ["AWS_EXECUTION_ENV"],
    }
"#;

/// The emulator as it is ASSEMBLED, on one engine: the handler's own SDK call is dispatched
/// back into this same router, so the configuration it reads is the one this suite deployed and
/// the role its credentials are minted of is the record IAM holds.
fn build_router() -> MultiServiceRouter {
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: each setup call carries a credential scope
    // so the router can name the target service, without a computed signature. The handler's
    // OWN call is signed by the runtime, with the credential it was given.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request. The signature itself is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/{signing_name}/aws4_request, \
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

    /// One response header, as text.
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }
}

/// One request dispatched at the endpoint host of `service`, with `signing_name` in the
/// credential scope.
async fn dispatch(
    router: &MultiServiceRouter,
    service: &str,
    signing_name: &str,
    method: &str,
    uri: &str,
    content_type: &str,
    body: &[u8],
) -> Answered {
    let host = format!("{service}.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host.clone())
        .header("authorization", authorization(signing_name))
        .header("content-type", content_type)
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

/// One AppConfig CONTROL-plane call, as a client makes one.
async fn control_plane(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    content_type: &str,
    body: &[u8],
) -> Answered {
    // Both of AppConfig's APIs sign as `appconfig`, whichever endpoint a client addresses.
    dispatch(
        router,
        "appconfig",
        "appconfig",
        method,
        uri,
        content_type,
        body,
    )
    .await
}

/// One AppConfig DATA-plane call, as a client makes one.
async fn data_plane(router: &MultiServiceRouter, method: &str, uri: &str, body: &[u8]) -> Answered {
    dispatch(
        router,
        "appconfigdata",
        "appconfig",
        method,
        uri,
        JSON_CONTENT_TYPE,
        body,
    )
    .await
}

/// One Lambda `restJson1` call.
async fn lambda(router: &MultiServiceRouter, method: &str, uri: &str, body: &[u8]) -> Answered {
    dispatch(
        router,
        "lambda",
        "lambda",
        method,
        uri,
        JSON_CONTENT_TYPE,
        body,
    )
    .await
}

/// One awsQuery call of `service`, answered with the HTTP status and the XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let answered = dispatch(
        router,
        service,
        service,
        "POST",
        "/",
        "application/x-www-form-urlencoded",
        body.as_bytes(),
    )
    .await;
    (answered.status, answered.text())
}

/// A control-plane call that must answer `expected`, with its parsed body.
async fn served(
    router: &MultiServiceRouter,
    case: &str,
    expected: u16,
    method: &str,
    uri: &str,
    body: &[u8],
) -> Value {
    let answered = control_plane(router, method, uri, JSON_CONTENT_TYPE, body).await;
    assert_eq!(
        answered.status,
        expected,
        "case {case:?}: expected HTTP {expected} from {method} {uri}, got {} ({})",
        answered.status,
        answered.text()
    );
    answered.json()
}

/// The `Id` a control-plane create minted.
fn minted_id(case: &str, created: &Value) -> String {
    created["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a minted Id, got {created}"))
        .to_string()
}

/// The AppConfig hierarchy this suite deploys into, as the control plane minted it.
struct Hierarchy {
    application: String,
    environment: String,
    profile: String,
}

impl Hierarchy {
    /// The ARN of the configuration a session over this triple is served from, as AppConfig
    /// addresses one — built here rather than read off the crates under test, so a grant and a
    /// denial sentence have something to be wrong about.
    fn configuration_arn(&self) -> String {
        format!(
            "arn:aws:appconfig:{REGION}:{ACCOUNT}:application/{}/environment/{}/configuration/{}",
            self.application, self.environment, self.profile
        )
    }
}

/// Create the application, the environment and the hosted configuration profile, write the
/// document and deploy it — all through AppConfig's own operations, exactly as a client sets a
/// configuration up.
async fn deployed_hierarchy(router: &MultiServiceRouter) -> Hierarchy {
    let case = "the hierarchy the handlers read";
    let application = minted_id(
        case,
        &served(
            router,
            case,
            201,
            "POST",
            "/applications",
            json!({"Name": APPLICATION_NAME}).to_string().as_bytes(),
        )
        .await,
    );
    let environment = minted_id(
        case,
        &served(
            router,
            case,
            201,
            "POST",
            &format!("/applications/{application}/environments"),
            json!({"Name": ENVIRONMENT_NAME}).to_string().as_bytes(),
        )
        .await,
    );
    let profile = minted_id(
        case,
        &served(
            router,
            case,
            201,
            "POST",
            &format!("/applications/{application}/configurationprofiles"),
            json!({"Name": PROFILE_NAME, "LocationUri": HOSTED, "Type": "AWS.Freeform"})
                .to_string()
                .as_bytes(),
        )
        .await,
    );

    let written = control_plane(
        router,
        "POST",
        &format!(
            "/applications/{application}/configurationprofiles/{profile}\
             /hostedconfigurationversions"
        ),
        JSON_CONTENT_TYPE,
        DEPLOYED.as_bytes(),
    )
    .await;
    assert_eq!(
        written.status,
        201,
        "case {case:?}: expected HTTP 201 from CreateHostedConfigurationVersion, got {} ({})",
        written.status,
        written.text()
    );
    // `CreateHostedConfigurationVersion` binds `VersionNumber` to the `Version-Number` response
    // header, not to the body: the body IS the stored document
    // (`smithy_query(query="operation", operation="CreateHostedConfigurationVersion")`).
    let version = written
        .header("version-number")
        .unwrap_or_else(|| panic!("case {case:?}: expected a minted Version-Number header"));

    let started = served(
        router,
        case,
        201,
        "POST",
        &format!("/applications/{application}/environments/{environment}/deployments"),
        json!({
            "DeploymentStrategyId": ALL_AT_ONCE,
            "ConfigurationProfileId": profile,
            "ConfigurationVersion": version,
        })
        .to_string()
        .as_bytes(),
    )
    .await;
    let number = started["DeploymentNumber"].as_i64().unwrap_or_else(|| {
        panic!("case {case:?}: expected StartDeployment to mint a deployment, got {started}")
    });

    await_rollout_reaching_every_target(router, case, &application, &environment, number).await;

    Hierarchy {
        application,
        environment,
        profile,
    }
}

/// Read one deployment through the control plane until AppConfig reports its rollout has reached
/// every target, which is when the version it deployed is the one the data plane serves every
/// reader. Every stage after the call that started it is owed by the CLOCK, so this looks again
/// until the condition holds rather than sleeping for a span.
async fn await_rollout_reaching_every_target(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    number: i64,
) {
    let uri =
        format!("/applications/{application}/environments/{environment}/deployments/{number}");
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let read = control_plane(router, "GET", &uri, JSON_CONTENT_TYPE, &[]).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: expected HTTP 200 from GetDeployment, got {} ({})",
            read.status,
            read.text()
        );
        last = read.json();
        if last["PercentageComplete"].as_f64().unwrap_or_default() >= 100.0 {
            return;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: deployment {number} never reached every target, last read {last}");
}

/// The bytes a CLIENT's own session and poll serve for the deployed configuration — the control
/// a served row is compared against, so a document the runtime composed rather than read out of
/// the data plane fails the comparison.
async fn client_poll(router: &MultiServiceRouter, case: &str, hierarchy: &Hierarchy) -> Value {
    let session = data_plane(
        router,
        "POST",
        "/configurationsessions",
        json!({
            "ApplicationIdentifier": hierarchy.application,
            "EnvironmentIdentifier": hierarchy.environment,
            "ConfigurationProfileIdentifier": hierarchy.profile,
        })
        .to_string()
        .as_bytes(),
    )
    .await;
    assert_eq!(
        session.status,
        201,
        "case {case:?}: expected HTTP 201 from StartConfigurationSession, got {} ({})",
        session.status,
        session.text()
    );
    let answered = session.json();
    let token = answered["InitialConfigurationToken"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected an initial token, got {answered}"))
        .to_string();
    let polled = data_plane(
        router,
        "GET",
        &format!("/configuration?configuration_token={}", form_encode(&token)),
        &[],
    )
    .await;
    assert_eq!(
        polled.status,
        200,
        "case {case:?}: expected HTTP 200 from GetLatestConfiguration, got {} ({})",
        polled.status,
        polled.text()
    );
    serde_json::from_slice(&polled.body).unwrap_or_else(|_| {
        panic!(
            "case {case:?}: expected the deployed JSON document, got {}",
            polled.text()
        )
    })
}

/// The trust policy a Lambda execution role needs.
fn trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// The log actions every execution role carries, which no row is about.
fn logs_statement() -> Value {
    json!({
        "Effect": "Allow",
        "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
        "Resource": "*",
    })
}

/// What a role's inline document grants of AppConfig, which is the other half of what decides
/// a handler's read: an action judged against a resource the statement does not name is refused
/// for the GRANT, before the identifiers are resolved at all.
#[derive(Clone, Copy)]
enum Grant {
    /// The data plane's two actions on the deployed configuration's own ARN.
    ThisConfiguration,
    /// The data plane's two actions on every resource, so a refusal can only be the service's
    /// own.
    EveryConfiguration,
    /// No AppConfig action at all.
    None,
}

/// The inline document one role carries: the log actions above, plus whatever its grant allows.
fn role_document(configuration_arn: &str, grant: Grant) -> String {
    let mut statements = vec![logs_statement()];
    let resource = match grant {
        Grant::ThisConfiguration => Some(configuration_arn.to_string()),
        Grant::EveryConfiguration => Some("*".to_string()),
        Grant::None => None,
    };
    if let Some(resource) = resource {
        statements.push(json!({
            "Effect": "Allow",
            "Action": [START_SESSION_ACTION, GET_CONFIGURATION_ACTION],
            "Resource": resource,
        }));
    }
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// Create the role `name` with that document through IAM's own operations, and answer the ARN
/// IAM minted for it.
async fn create_role(
    router: &MultiServiceRouter,
    name: &str,
    configuration_arn: &str,
    grant: Grant,
) -> String {
    let (status, created) = query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", &trust_policy()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole({name:?}) must succeed, got {status} with {created}"
    );
    let arn = opt_element(&created, "Arn").unwrap_or_else(|| {
        panic!("setup: CreateRole({name:?}) must answer the role's ARN, got {created}")
    });
    let (status, embedded) = query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "configuration-grants"),
            ("PolicyDocument", &role_document(configuration_arn, grant)),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: PutRolePolicy({name:?}) must succeed, got {status} with {embedded}"
    );
    arn
}

/// One function this suite deploys: the point release of the runtime it runs under, and what its
/// execution role grants of AppConfig.
struct Probe {
    /// The logical id the stack declares it under.
    logical: &'static str,
    /// The function's own name, which the session a refusal names carries.
    name: &'static str,
    /// A `python3.x` identifier of the model's `Runtime` value set
    /// (`smithy_query(query="shape", shape="Runtime")`).
    runtime: &'static str,
    /// The role this function runs as.
    role: &'static str,
    /// What that role's inline document grants of AppConfig.
    grant: Grant,
}

impl Probe {
    /// What `AWS_EXECUTION_ENV` states for this probe's runtime, which is how the handler
    /// reports which runtime really ran it.
    fn execution_env(&self) -> String {
        format!("AWS_Lambda_{}", self.runtime)
    }
}

/// The functions: three point releases reading under a grant that covers their read, and one
/// reading as a role granted none of AppConfig's actions.
const PROBES: &[Probe] = &[
    Probe {
        logical: "ReaderOnPython39",
        name: "xsvc-appconfig-reader-py39",
        runtime: "python3.9",
        role: "xsvc-appconfig-reader-scoped",
        grant: Grant::ThisConfiguration,
    },
    Probe {
        logical: "ReaderOnPython312",
        name: "xsvc-appconfig-reader-py312",
        runtime: "python3.12",
        role: "xsvc-appconfig-reader-wildcard",
        grant: Grant::EveryConfiguration,
    },
    Probe {
        logical: "ReaderOnPython313",
        name: "xsvc-appconfig-reader-py313",
        runtime: "python3.13",
        role: "xsvc-appconfig-reader-scoped",
        grant: Grant::ThisConfiguration,
    },
    Probe {
        logical: "ReaderOnPython311",
        name: "xsvc-appconfig-reader-py311",
        runtime: "python3.11",
        role: "xsvc-appconfig-reader-ungranted",
        grant: Grant::None,
    },
];

/// Which of the three identifier levels the event carries as the id AppConfig minted, and which
/// as a name that resolves to nothing — a template's logical id, which is what a handler is
/// handed when its environment was wired to the wrong template attribute.
#[derive(Clone, Copy)]
enum Identifiers {
    /// Every level as the id the control plane minted.
    Minted,
    /// Every level as a logical id of a template, so the first call resolves nothing.
    LogicalThroughout,
    /// The application and the profile as minted ids, the environment as a logical id.
    LogicalEnvironment,
}

/// A logical id of the kind a template declares its AppConfig resources under.
const LOGICAL_APPLICATION: &str = "OrdersFlagsApplication";
const LOGICAL_ENVIRONMENT: &str = "ProductionEnvironment";
const LOGICAL_PROFILE: &str = "BackendRolloutProfile";

impl Identifiers {
    /// The three identifiers the event carries.
    fn event_of(self, hierarchy: &Hierarchy) -> Value {
        let (application, environment, profile) = match self {
            Self::Minted => (
                hierarchy.application.clone(),
                hierarchy.environment.clone(),
                hierarchy.profile.clone(),
            ),
            Self::LogicalThroughout => (
                LOGICAL_APPLICATION.to_string(),
                LOGICAL_ENVIRONMENT.to_string(),
                LOGICAL_PROFILE.to_string(),
            ),
            Self::LogicalEnvironment => (
                hierarchy.application.clone(),
                LOGICAL_ENVIRONMENT.to_string(),
                hierarchy.profile.clone(),
            ),
        };
        json!({
            "application": application,
            "environment": environment,
            "profile": profile,
        })
    }
}

/// What the handler's read meets, and so what its own `except` reports.
#[derive(Clone, Copy)]
enum Outcome {
    /// The configuration the deployment put in force.
    Served,
    /// An identifier that resolves to nothing: the service's own not-found refusal.
    NotFound,
    /// A role granted none of AppConfig's actions: the authorization refusal, naming the action
    /// of the FIRST call the handler makes.
    Denied,
}

/// One row: which function (and so which runtime and role) runs, which identifiers it is handed,
/// and what its read has to meet.
struct Row {
    case: &'static str,
    /// The logical id of the [`PROBES`] entry this row invokes, so a row stays bound to its own
    /// function however that table is ordered.
    probe: &'static str,
    identifiers: Identifiers,
    outcome: Outcome,
}

impl Row {
    /// The probe this row runs on.
    fn probe(&self) -> &'static Probe {
        PROBES
            .iter()
            .find(|probe| probe.logical == self.probe)
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: no probe is declared under the logical id {:?}",
                    self.case, self.probe
                )
            })
    }
}

const ROWS: &[Row] = &[
    Row {
        case: "minted identifiers on python3.9, read under a grant naming this configuration",
        probe: "ReaderOnPython39",
        identifiers: Identifiers::Minted,
        outcome: Outcome::Served,
    },
    Row {
        case: "minted identifiers on python3.13, read under a grant naming this configuration",
        probe: "ReaderOnPython313",
        identifiers: Identifiers::Minted,
        outcome: Outcome::Served,
    },
    Row {
        case: "logical ids at every level on python3.12, under a grant over every resource",
        probe: "ReaderOnPython312",
        identifiers: Identifiers::LogicalThroughout,
        outcome: Outcome::NotFound,
    },
    Row {
        case: "a logical id for the environment only on python3.12, under that same grant",
        probe: "ReaderOnPython312",
        identifiers: Identifiers::LogicalEnvironment,
        outcome: Outcome::NotFound,
    },
    Row {
        case: "minted identifiers on python3.11, read by a role granted no AppConfig action",
        probe: "ReaderOnPython311",
        identifiers: Identifiers::Minted,
        outcome: Outcome::Denied,
    },
];

/// The stack declaring one function per probe: the inline source is archived into the deployment
/// package by CloudFormation, which is how a function whose handler is written here comes to
/// exist at all.
fn function_stack(role_arns: &BTreeMap<&str, String>) -> String {
    let mut resources = serde_json::Map::new();
    for probe in PROBES {
        let role_arn = role_arns
            .get(probe.role)
            .unwrap_or_else(|| panic!("setup: no ARN for the role {:?}", probe.role));
        resources.insert(
            probe.logical.to_string(),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": probe.name,
                    "Runtime": probe.runtime,
                    "Handler": "index.lambda_handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": READER_HANDLER},
                },
            }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
    })
    .to_string()
}

/// Deploy the functions' stack, wait for `CREATE_COMPLETE` and answer the physical id of each
/// declared function.
async fn deploy_functions(
    router: &MultiServiceRouter,
    stack: &str,
    role_arns: &BTreeMap<&str, String>,
) -> BTreeMap<String, String> {
    let template = function_stack(role_arns);
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({stack:?}) must be accepted, got {status} with {answered}"
    );
    let mut reported = stack_status(router, stack).await;
    for _ in 0..LOOKS {
        if reported.0 == CREATE_COMPLETE {
            break;
        }
        tokio::time::sleep(LOOK).await;
        reported = stack_status(router, stack).await;
    }
    let failures = if reported.0 == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, events) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStackEvents",
            &[("StackName", stack)],
        )
        .await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        reported.0, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {} with resource failures \
         {failures:?}",
        reported.1
    );
    let (status, resources) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "DescribeStackResources",
        &[("StackName", stack)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: DescribeStackResources({stack:?}) must succeed, got {status} with {resources}"
    );
    physical_ids_of(&resources)
}

/// Wait until Lambda reports `name` `Active`, which is the state an invocation needs.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) {
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let read = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            &[],
        )
        .await;
        last = read.json();
        if last["State"].as_str() == Some("Active") {
            return;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda must report {name:?} Active, last reported {last}");
}

/// The assumed-role session a call an invocation of `function` makes speaks for, which is the
/// principal a refusal names.
fn session_principal(role: &str, function: &str) -> String {
    format!("arn:aws:sts::{ACCOUNT}:assumed-role/{role}/{function}")
}

/// The sentence IAM refuses a request with, built here from the principal, the action and the
/// resource rather than read off the crates under test.
fn denial(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         because no identity-based policy allows the {action} action"
    )
}

/// One JSON document written out in this suite, as the value it parses to.
fn document(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{text:?} is JSON: {error}"))
}

/// Invoke `function` with `event` and answer the handler's own payload — asserting on the way
/// that the invocation was served HTTP 200 and that it carries NO `X-Amz-Function-Error`, which
/// is where a runtime that could not import the SDK reports the initialization fault instead of
/// a result.
///
/// Looks again, every [`LOOK`] until [`SERVED_DEADLINE`] passes, for as long as Lambda has not
/// served the invocation at all ([`UNSERVED`]): what this answers is always the answer of a
/// handler that ran, on a loaded host as on an idle one.
async fn invoked(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    event: &Value,
) -> Value {
    let deadline = Instant::now() + SERVED_DEADLINE;
    loop {
        let answered = lambda(
            router,
            "POST",
            &format!("/2015-03-31/functions/{function}/invocations"),
            event.to_string().as_bytes(),
        )
        .await;
        let payload = answered.json();
        if UNSERVED.contains(&answered.status) {
            assert!(
                Instant::now() < deadline,
                "case {case:?}: Lambda must serve this row's invocation, last answered {} with \
                 {payload}",
                answered.status
            );
            tokio::time::sleep(LOOK).await;
            continue;
        }
        assert_eq!(
            answered.status, 200,
            "case {case:?}: the invocation must be served, got {} with {payload}",
            answered.status
        );
        // The SDK is imported at module scope, so a runtime that did not carry it would report
        // the initialization fault here instead of the handler's own answer.
        assert_eq!(
            answered.header("x-amz-function-error"),
            None,
            "case {case:?}: the handler must run to completion, got a function error with \
             {payload}"
        );
        return payload;
    }
}

/// Every row: the handler runs to completion on every point release of the runtime, and what it
/// answers is what AppConfig answered its own SDK call.
#[tokio::test]
async fn an_appconfig_reading_handler_runs_on_every_python_runtime() {
    let router = build_router();
    let hierarchy = deployed_hierarchy(&router).await;
    let configuration_arn = hierarchy.configuration_arn();

    let mut role_arns: BTreeMap<&str, String> = BTreeMap::new();
    for probe in PROBES {
        if role_arns.contains_key(probe.role) {
            continue;
        }
        let arn = create_role(&router, probe.role, &configuration_arn, probe.grant).await;
        role_arns.insert(probe.role, arn);
    }

    let physical = deploy_functions(&router, "xsvc-appconfig-readers", &role_arns).await;
    for probe in PROBES {
        let function = physical.get(probe.logical).cloned().unwrap_or_else(|| {
            panic!(
                "setup: the stack must report a physical id for {:?}",
                probe.logical
            )
        });
        assert_eq!(
            function, probe.name,
            "setup: the stack must name the function {:?}, got {function:?}",
            probe.name
        );
        wait_until_active(&router, &function).await;
    }

    // What a CLIENT reads out of the data plane itself: the control every served row is held
    // against.
    let as_a_client_reads_it =
        client_poll(&router, "a client's own session and poll", &hierarchy).await;
    assert_eq!(
        as_a_client_reads_it,
        document(DEPLOYED),
        "setup: expected the data plane to serve the deployed document, got \
         {as_a_client_reads_it}"
    );

    for row in ROWS {
        let case = row.case;
        let probe = row.probe();
        let payload = invoked(
            &router,
            case,
            probe.name,
            &row.identifiers.event_of(&hierarchy),
        )
        .await;
        assert_eq!(
            payload["ran_under"],
            json!(probe.execution_env()),
            "case {case:?}: expected the row's own runtime to have run it, got {payload}"
        );

        match row.outcome {
            Outcome::Served => {
                assert_eq!(
                    payload["source"],
                    json!("appconfig"),
                    "case {case:?}: expected the read to be served, got {payload}"
                );
                assert_eq!(
                    payload["config"], as_a_client_reads_it,
                    "case {case:?}: expected the document the data plane serves a client, got \
                     {payload}"
                );
            }
            Outcome::NotFound => {
                assert_eq!(
                    payload["source"],
                    json!("default"),
                    "case {case:?}: expected the handler to fall back to its default, got \
                     {payload}"
                );
                assert_eq!(
                    payload["code"],
                    json!(NOT_FOUND),
                    "case {case:?}: expected the service's own not-found refusal, got {payload}"
                );
                assert_eq!(
                    payload["status"],
                    json!(NOT_FOUND_STATUS),
                    "case {case:?}: expected HTTP {NOT_FOUND_STATUS} for an identifier that \
                     resolves to nothing, got {payload}"
                );
                assert_eq!(
                    payload["config"],
                    document(HANDLER_DEFAULT),
                    "case {case:?}: expected the handler's own default document, got {payload}"
                );
            }
            Outcome::Denied => {
                assert_eq!(
                    payload["source"],
                    json!("default"),
                    "case {case:?}: expected the handler to fall back to its default, got \
                     {payload}"
                );
                assert_eq!(
                    payload["code"],
                    json!(ACCESS_DENIED),
                    "case {case:?}: expected the authorization refusal rather than a not-found \
                     one, got {payload}"
                );
                assert_eq!(
                    payload["status"],
                    json!(ACCESS_DENIED_STATUS),
                    "case {case:?}: expected HTTP {ACCESS_DENIED_STATUS} for an unauthorized \
                     caller, got {payload}"
                );
                assert_eq!(
                    payload["message"],
                    json!(denial(
                        &session_principal(probe.role, probe.name),
                        START_SESSION_ACTION,
                        &configuration_arn
                    )),
                    "case {case:?}: expected IAM's own denial sentence, got {payload}"
                );
                assert_eq!(
                    payload["config"],
                    document(HANDLER_DEFAULT),
                    "case {case:?}: expected the handler's own default document, got {payload}"
                );
            }
        }
    }
}
