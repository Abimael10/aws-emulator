// SPDX-License-Identifier: CC0-1.0

//! A `python3.x` handler that reads its own AppConfig configuration on every invocation, driven
//! over a SUCCESSION of deployments into the one environment — the lifecycle an operator walks
//! when a serverless client is reported to be serving a stale flag: Lambda, the AppConfig
//! control plane, the AppConfig data plane, IAM and the token service on ONE engine.
//!
//! RULE: the data plane "retrieves the latest DEPLOYED configuration"
//! (`smithy_query(query="doc",shape="GetLatestConfiguration")`), so what a reader is served is
//! the configuration version the environment has in force at the instant it polls — never the
//! newest version AUTHORED — and a `RequestResponse` invocation runs the deployment package and
//! answers HTTP 200 with the handler's own payload and no `X-Amz-Function-Error`, which is what
//! makes each invocation an answer about the configuration in force right then. Together those
//! give the lifecycle its three distinguishable moments: writing a hosted configuration version
//! changes nothing a reader sees, deploying it makes it what every reader is served, and
//! deploying an earlier version again takes the reader back to that one — with the function,
//! its code and its identifiers untouched throughout.
//!
//! The rows differ along what the rule generalises over: WHAT the step does to the configuration
//! (author a version and deploy it, author one and leave it undeployed, deploy a version an
//! earlier step authored, deploy an earlier version again), the CONTENT and shape of the
//! document that step puts in play (a flag that is off, the same flag on, a nested document with
//! an array beside it), and therefore WHICH generation is in force when the invocation that
//! follows runs. Every row asserts the same invariants — HTTP 200, no function error, and the
//! bytes the handler read being exactly the bytes a CLIENT outside Lambda polls out of the data
//! plane — so a document the runtime composed rather than read, or a reader served the newest
//! authored version, would fail here.
//!
//! What the neighbours pin and this one does not repeat: `handler_appconfig_sdk` drives both of
//! AppConfig's APIs for one deployed document, `handler_appconfig_execution_role` and
//! `handler_appconfig_sibling_execution_roles` drive the grants the execution role carries (and
//! the latter owns the five-service fixture this suite deploys through),
//! `handler_appconfig_identifier_repair` drives the identifiers the function carries, and
//! `handler_appconfig_deployment_strategy_forms` drives which strategy paced the ONE deployment
//! a row makes. What this suite adds is the SECOND and THIRD deployment into the same
//! environment: that an authored-but-undeployed version is exactly the stale value an operator
//! sees, and that the invocation's answer follows the deployment generation.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]): this emulator serves at most
//! `max_concurrent_runs` invocations at once for the whole process.

use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};

// The reader of the bytes a handler reports, exported by the suite that first needed it.
use crate::handler_appconfig_identifier_repair::reported_bytes;
// The five services on one engine, the execution role, the policy writer and the client-side
// poll the sibling suite exports — driven here rather than copied.
use crate::handler_appconfig_sibling_execution_roles::{allow, Emulator, Hierarchy};
use crate::invoke_request_id::one_invocation_stream_at_a_time;
// The one .zip writer this crate's suites package a handler with.
use crate::node_invoke_run::package;

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The module and attribute the package below carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The point release of the runtime the function runs under. WHICH point release runs the module
/// is `python_runtime_bundled_sdk`'s axis, not this suite's.
const RUNTIME: &str = "python3.12";

/// The timeout the function is created with, in seconds.
const FUNCTION_TIMEOUT: i64 = 25;

/// The role the function runs as, and the function's own name — one of each, created once and
/// never touched again, so every row is about the deployment generation and nothing else.
const ROLE_NAME: &str = "lambda-appconfig-redeploy-reader";
const FUNCTION_NAME: &str = "appconfig-redeploy-reader";

/// The names the hierarchy every row deploys into is created under.
const APPLICATION_NAME: &str = "orders-redeploy-flags";
const ENVIRONMENT_NAME: &str = "production";
const PROFILE_NAME: &str = "backend-rollout";

/// The `LocationUri` that selects AppConfig's own hosted configuration store, and the profile
/// type a freeform document is written under.
const HOSTED: &str = "hosted";
const FREEFORM: &str = "AWS.Freeform";

/// The three actions AppConfig publishes a configuration reader's operations under, as a policy
/// statement spells them. WHICH form the grant takes is the sibling suites' axis; this one
/// grants all three so every row's read is permitted.
const START_SESSION_ACTION: &str = "appconfig:StartConfigurationSession";
const GET_LATEST_ACTION: &str = "appconfig:GetLatestConfiguration";
const GET_CONFIGURATION_ACTION: &str = "appconfig:GetConfiguration";

/// The strategy every deployment here is paced by: a client-created one that paces no stage at
/// all, so a deployment has reached its targets and stopped baking shortly after
/// `StartDeployment` answers and the next one is not refused the environment.
const STRATEGY_NAME: &str = "redeploy-at-once";

/// The `DeploymentState` a deployment that has finished reports
/// (`smithy_query(query="shape",shape="DeploymentState")` lists `BAKING | VALIDATING |
/// DEPLOYING | COMPLETE | ROLLING_BACK | ROLLED_BACK | REVERTED`).
const COMPLETE: &str = "COMPLETE";

/// The media type every document below is written and served with.
const JSON_CONTENT_TYPE: &str = "application/json";

/// The three environment variables the handler reads its identifiers out of, written once at
/// create time and never replaced.
const APPLICATION_VARIABLE: &str = "APP_CONFIG_APPLICATION";
const ENVIRONMENT_VARIABLE: &str = "APP_CONFIG_ENVIRONMENT";
const CONFIGURATION_VARIABLE: &str = "APP_CONFIG_CONFIGURATION";

/// The documents the steps below author. They differ in shape as well as in content, so a row
/// cannot pass on a document that merely has the right length.
const FLAG_OFF: &str = r#"{"useNewBackend": false}"#;
const FLAG_ON: &str = r#"{"useNewBackend": true}"#;
const NESTED: &str =
    r#"{"useNewBackend": true, "flags": {"beta": {"enabled": true}}, "ratios": [0.25, 0.5]}"#;

/// How long a condition the CLOCK owes is waited for before the suite gives up — generous,
/// because nothing here is a measurement of how fast the host runs.
const PATIENCE: Duration = Duration::from_secs(120);

/// How long the suite waits between two looks at a condition it is waiting for.
const BETWEEN_LOOKS: Duration = Duration::from_millis(50);

/// The handler the function runs for every row: it reads the three identifiers out of its own
/// environment, opens a configuration session over them, polls it once and reports the bytes it
/// was served — or, when AppConfig refuses the call, falls back to its own default document and
/// reports the refusal's error code, which is what a feature-flagged handler does. No row here
/// expects that fallback: it is kept so a row that lost its configuration says WHY rather than
/// failing as an unhandled error.
const HANDLER_SOURCE: &str = r#"
import base64
import json
import os

import boto3
import botocore.exceptions

# The document a handler that cannot reach its configuration has to fall back on.
DEFAULT_CONFIG = {"useNewBackend": False}

CLIENT = boto3.client("appconfigdata")


def lambda_handler(event, context):
    try:
        session = CLIENT.start_configuration_session(
            ApplicationIdentifier=os.environ["APP_CONFIG_APPLICATION"],
            EnvironmentIdentifier=os.environ["APP_CONFIG_ENVIRONMENT"],
            ConfigurationProfileIdentifier=os.environ["APP_CONFIG_CONFIGURATION"],
        )
        answered = CLIENT.get_latest_configuration(
            ConfigurationToken=session["InitialConfigurationToken"]
        )
    except botocore.exceptions.ClientError as error:
        return {
            "source": "default",
            "code": error.response["Error"]["Code"],
            "message": error.response["Error"]["Message"],
            "config": DEFAULT_CONFIG,
        }
    read = answered["Configuration"].read()
    return {
        "source": "appconfig",
        "bytes": base64.b64encode(read).decode("ascii"),
        "config": json.loads(read.decode("utf-8")),
    }
"#;

/// What one step does to the configuration before the invocation that follows it.
#[derive(Debug)]
enum Change {
    /// Write this document as a hosted configuration version and deploy nothing — the write an
    /// operator makes and then wonders why their function has not picked up.
    Author(&'static str),
    /// Write this document as a hosted configuration version and deploy the version it was
    /// written as.
    AuthorAndDeploy(&'static str),
    /// Deploy the version an earlier step wrote this document as, without writing anything.
    Deploy(&'static str),
}

/// One step of the lifecycle: what it does, and the document that must be in force for the
/// invocation that follows it.
#[derive(Debug)]
struct Step {
    case: &'static str,
    change: Change,
    /// The document every reader must be served after this step — the one the environment has
    /// in force, which is not always the newest one authored.
    in_force: &'static str,
}

/// The lifecycle, step by step: a first deployment, a version authored and left undeployed (the
/// stale value), that version deployed, a third version authored and deployed, and the first
/// version deployed again.
const STEPS: &[Step] = &[
    Step {
        case: "the first version authored and deployed",
        change: Change::AuthorAndDeploy(FLAG_OFF),
        in_force: FLAG_OFF,
    },
    Step {
        case: "a second version authored and left undeployed",
        change: Change::Author(FLAG_ON),
        in_force: FLAG_OFF,
    },
    Step {
        case: "the authored second version deployed",
        change: Change::Deploy(FLAG_ON),
        in_force: FLAG_ON,
    },
    Step {
        case: "a nested third version authored and deployed",
        change: Change::AuthorAndDeploy(NESTED),
        in_force: NESTED,
    },
    Step {
        case: "the first version deployed again",
        change: Change::Deploy(FLAG_OFF),
        in_force: FLAG_OFF,
    },
];

/// One AppConfig control-plane call that must answer `expected`, as its headers and its parsed
/// body.
async fn appconfig(
    emulator: &Emulator,
    what: &str,
    expected: u16,
    method: &str,
    path: &str,
    body: &[u8],
) -> (http::HeaderMap, Value) {
    let (status, headers, parsed) = emulator
        .rest(
            "appconfig",
            "appconfig",
            method,
            path,
            JSON_CONTENT_TYPE,
            body,
        )
        .await;
    assert_eq!(
        status, expected,
        "case {what:?}: expected HTTP {expected} from {method} {path}, got {status} ({parsed})"
    );
    (headers, parsed)
}

/// The `Id` one control-plane create minted.
async fn created_id(emulator: &Emulator, what: &str, path: &str, body: &Value) -> String {
    let (_headers, created) = appconfig(
        emulator,
        what,
        201,
        "POST",
        path,
        &serde_json::to_vec(body).expect("the request body serializes"),
    )
    .await;
    created["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {what:?}: expected a minted Id, got {created}"))
        .to_string()
}

/// Write `document` as a hosted configuration version of this profile, read it back as a client
/// does, and answer the version number the control plane minted for it.
///
/// `CreateHostedConfigurationVersion` binds `Content` to the whole payload and `VersionNumber`
/// to the `Version-Number` header
/// (`smithy_query(query="operation",operation="CreateHostedConfigurationVersion")`), so the
/// number is read out of the header and the stored document out of the body.
async fn author(
    emulator: &Emulator,
    case: &str,
    triple: &Hierarchy,
    document: &'static str,
) -> i64 {
    let path = format!(
        "/applications/{}/configurationprofiles/{}/hostedconfigurationversions",
        triple.application, triple.profile
    );
    let (headers, written) =
        appconfig(emulator, case, 201, "POST", &path, document.as_bytes()).await;
    let version = headers
        .get("version-number")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a minted version number, got {headers:?}")
        });
    let expected = as_document(case, document);
    assert_eq!(
        written, expected,
        "case {case:?}: expected the write to answer the document it carried, got {written}"
    );
    // The authored version is held whether or not it is ever deployed: a reader being served an
    // older document is the deployment's doing and not a write that went missing.
    let (_headers, stored) = appconfig(
        emulator,
        case,
        200,
        "GET",
        &format!(
            "/applications/{}/configurationprofiles/{}/hostedconfigurationversions/{version}",
            triple.application, triple.profile
        ),
        &[],
    )
    .await;
    assert_eq!(
        stored, expected,
        "case {case:?}: expected version {version} to be held as written, got {stored}"
    );
    version
}

/// Deploy `version` of this profile into this environment and answer the deployment number
/// AppConfig minted.
async fn start_deployment(
    emulator: &Emulator,
    case: &str,
    triple: &Hierarchy,
    strategy: &str,
    version: i64,
) -> i64 {
    let (_headers, started) = appconfig(
        emulator,
        case,
        201,
        "POST",
        &format!(
            "/applications/{}/environments/{}/deployments",
            triple.application, triple.environment
        ),
        &serde_json::to_vec(&json!({
            "DeploymentStrategyId": strategy,
            "ConfigurationProfileId": triple.profile,
            "ConfigurationVersion": version.to_string(),
        }))
        .expect("the request body serializes"),
    )
    .await;
    assert_eq!(
        started["ConfigurationVersion"],
        json!(version.to_string()),
        "case {case:?}: expected the deployment to record version {version}, got {started}"
    );
    started["DeploymentNumber"].as_i64().unwrap_or_else(|| {
        panic!("case {case:?}: expected a minted deployment number, got {started}")
    })
}

/// Read one deployment through the control plane until AppConfig reports it finished.
///
/// Every stage after the call that started it — the rollout reaching its targets, the bake, the
/// completion — is owed by the CLOCK, so this looks again until the condition holds instead of
/// sleeping for a span. A finished deployment is what the NEXT step needs: an environment still
/// working on one refuses a second deployment.
async fn await_deployment_finished(
    emulator: &Emulator,
    case: &str,
    triple: &Hierarchy,
    number: i64,
) {
    let path = format!(
        "/applications/{}/environments/{}/deployments/{number}",
        triple.application, triple.environment
    );
    let deadline = Instant::now() + PATIENCE;
    loop {
        let (_headers, read) = appconfig(emulator, case, 200, "GET", &path, &[]).await;
        if read["State"] == json!(COMPLETE) {
            assert_eq!(
                read["PercentageComplete"],
                json!(100.0),
                "case {case:?}: a finished deployment has reached every target, got {read}"
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: deployment {number} never finished, last read {read}"
        );
        tokio::time::sleep(BETWEEN_LOOKS).await;
    }
}

/// Invoke the function once, synchronously, as `(the FunctionError signal, the payload)`.
async fn invoke(emulator: &Emulator, case: &str, function: &str) -> (Option<String>, Value) {
    let (status, headers, payload) = emulator
        .rest(
            "lambda",
            "lambda",
            "POST",
            &format!("{FUNCTIONS_URI}/{function}/invocations"),
            JSON_CONTENT_TYPE,
            json!({}).to_string().as_bytes(),
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
    (signal, payload)
}

/// One of this suite's documents as the JSON value it carries, so a round trip is compared as
/// the document it is rather than as the whitespace it was written with.
fn as_document(case: &str, document: &str) -> Value {
    serde_json::from_str(document)
        .unwrap_or_else(|error| panic!("case {case:?}: {document:?} is a JSON document ({error})"))
}

/// RULE: a reader is served the configuration version its environment has in FORCE, so writing a
/// hosted configuration version changes nothing a reader sees until a deployment puts it in
/// force, and a synchronous invocation of an untouched function answers from the generation in
/// force at the instant it runs — HTTP 200, no function error, and byte for byte what a client
/// outside Lambda polls out of the data plane.
#[tokio::test]
async fn a_redeployment_decides_what_the_next_invocation_reads() {
    let _stream = one_invocation_stream_at_a_time().await;
    let emulator = Emulator::new();

    // The execution role, granted the three actions a configuration reader is published under,
    // so every row's read is the principal's to make.
    let role_arn = emulator.execution_role(ROLE_NAME).await;
    emulator
        .role_policy(
            "the reader's grant",
            ROLE_NAME,
            &allow(
                &[
                    START_SESSION_ACTION,
                    GET_LATEST_ACTION,
                    GET_CONFIGURATION_ACTION,
                ],
                "*",
            ),
        )
        .await;

    // The hierarchy every step deploys into, created through AppConfig's own control plane.
    let application = created_id(
        &emulator,
        "CreateApplication",
        "/applications",
        &json!({ "Name": APPLICATION_NAME }),
    )
    .await;
    let environment = created_id(
        &emulator,
        "CreateEnvironment",
        &format!("/applications/{application}/environments"),
        &json!({ "Name": ENVIRONMENT_NAME }),
    )
    .await;
    let profile = created_id(
        &emulator,
        "CreateConfigurationProfile",
        &format!("/applications/{application}/configurationprofiles"),
        &json!({ "Name": PROFILE_NAME, "LocationUri": HOSTED, "Type": FREEFORM }),
    )
    .await;
    let triple = Hierarchy {
        application,
        environment,
        profile,
    };

    // The strategy every deployment is paced by: no rollout window, every target at once, no
    // bake time, nothing replicated — the pacing a client declares when it wants the version
    // servable as soon as the deployment reports itself finished.
    let strategy = created_id(
        &emulator,
        "CreateDeploymentStrategy",
        "/deploymentstrategies",
        &json!({
            "Name": STRATEGY_NAME,
            "DeploymentDurationInMinutes": 0,
            "GrowthFactor": 100.0,
            "GrowthType": "LINEAR",
            "FinalBakeTimeInMinutes": 0,
            "ReplicateTo": "NONE",
        }),
    )
    .await;

    // The ONE function, carrying the three identifiers in its own environment. Neither its code
    // nor its configuration is touched again, so every difference below is the deployment's.
    let (status, _headers, created) = emulator
        .rest(
            "lambda",
            "lambda",
            "POST",
            FUNCTIONS_URI,
            JSON_CONTENT_TYPE,
            &serde_json::to_vec(&json!({
                "FunctionName": FUNCTION_NAME,
                "Role": role_arn,
                "Runtime": RUNTIME,
                "Handler": HANDLER,
                "Timeout": FUNCTION_TIMEOUT,
                "Environment": { "Variables": {
                    APPLICATION_VARIABLE: triple.application,
                    ENVIRONMENT_VARIABLE: triple.environment,
                    CONFIGURATION_VARIABLE: triple.profile,
                } },
                "Code": {
                    "ZipFile": base64::engine::general_purpose::STANDARD
                        .encode(package(&[("lambda_function.py", HANDLER_SOURCE)])),
                },
            }))
            .expect("the create body serializes"),
        )
        .await;
    assert_eq!(
        status, 201,
        "setup: expected HTTP 201 from CreateFunction, got {status} ({created})"
    );
    emulator.await_function_active(FUNCTION_NAME).await;

    // The version number each authored document was written as, so a later step can deploy a
    // document an earlier one wrote.
    let mut authored: Vec<(&'static str, i64)> = Vec::new();

    for step in STEPS {
        let case = step.case;
        let version = match step.change {
            Change::Author(document) | Change::AuthorAndDeploy(document) => {
                let version = author(&emulator, case, &triple, document).await;
                authored.push((document, version));
                version
            }
            Change::Deploy(document) => authored
                .iter()
                .find(|(written, _version)| *written == document)
                .map(|(_written, version)| *version)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: no earlier step authored {document:?}")
                }),
        };
        if !matches!(step.change, Change::Author(_)) {
            let number = start_deployment(&emulator, case, &triple, &strategy, version).await;
            await_deployment_finished(&emulator, case, &triple, number).await;
        }

        // What a client OUTSIDE Lambda is served for this triple, which is the control the
        // handler's own answer is held against.
        let polled = emulator.served_bytes(case, &triple).await;
        assert_eq!(
            polled,
            step.in_force.as_bytes(),
            "case {case:?}: expected the data plane to serve the version in force ({:?}), got {:?}",
            step.in_force,
            String::from_utf8_lossy(&polled)
        );

        let (signal, payload) = invoke(&emulator, case, FUNCTION_NAME).await;
        assert_eq!(
            signal, None,
            "case {case:?}: the handler must run to completion, got the function error \
             {signal:?} with {payload}"
        );
        assert_eq!(
            payload["source"],
            json!("appconfig"),
            "case {case:?}: expected the handler to be served its configuration, got {payload}"
        );
        let read = reported_bytes(case, &payload);
        assert_eq!(
            read,
            step.in_force.as_bytes(),
            "case {case:?}: expected the handler to read the version in force ({:?}), got {:?}",
            step.in_force,
            String::from_utf8_lossy(&read)
        );
        assert_eq!(
            read, polled,
            "case {case:?}: expected the handler to read exactly what a client polls, got {:?} \
             against {:?}",
            String::from_utf8_lossy(&read),
            String::from_utf8_lossy(&polled)
        );
        assert_eq!(
            payload["config"],
            as_document(case, step.in_force),
            "case {case:?}: expected the document in force as the handler's own answer, got \
             {payload}"
        );
    }
}
