// SPDX-License-Identifier: CC0-1.0

//! Repairing a running function's AppConfig identifiers IN PLACE, one at a time, the way an
//! operator fixes a misconfigured serverless client: Lambda, the AppConfig control plane and
//! the AppConfig data plane over ONE engine, a `python3.12` handler that reads the three
//! identifiers out of its own environment variables, and `UpdateFunctionConfiguration`
//! replacing that map between invocations.
//!
//! RULE: a configuration session is over ONE resolved triple, so `StartConfigurationSession`
//! is refused `ResourceNotFoundException` (HTTP 404) whenever ANY of the application, the
//! environment or the configuration profile resolves to nothing — resolution is total and
//! per-identifier, with no fallback to a default or latest configuration for the identifiers
//! that did resolve — and the deployed document is served, byte for byte, only once all three
//! resolve, each given as the ID or as the NAME of its level. A handler reading those
//! identifiers out of its environment is therefore answered afresh on every invocation from
//! the map the function currently carries: `UpdateFunctionConfiguration` REPLACES
//! `Environment.Variables` wholesale, the next invocation's SDK call is made with the
//! replacement, and the invocation itself is answered HTTP 200 with the handler's own payload
//! and no `X-Amz-Function-Error` whichever way AppConfig answered it, because a refusal
//! reaches the guest as `botocore.exceptions.ClientError` for the handler's own `except` to
//! report.
//!
//! The rows differ along what the rule generalises over: WHICH of the three identifiers
//! resolve and which do not (none, each one broken on its own, all three), the FORM a repaired
//! identifier is written in (the id the control plane minted, the name it was created under),
//! and the ORDER the repair is applied in — including a half-repaired triple, which must still
//! be refused rather than served, and a triple broken again AFTER a served read, which must go
//! back to being refused because nothing about the served read is remembered.
//!
//! What the neighbours pin and this one does not repeat: `handler_appconfig_sdk` drives both of
//! AppConfig's APIs with the identifiers carried in the EVENT (and owns the fixture this suite
//! deploys its hierarchy with), `handler_appconfig_execution_role` drives the execution role's
//! grants, `python_runtime_bundled_sdk` pins that a repointed variable is what the next
//! invocation addresses, and AppConfigData's own `unresolvable_tier_precedence` pins WHICH tier
//! a refusal names when several are unresolvable.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]): this emulator serves at
//! most `max_concurrent_runs` invocations at once for the whole process.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::handler_appconfig_sdk::{
    client_poll, data_plane, deployed_hierarchy, emulator, lambda, Hierarchy, APPLICATION_NAME,
    DEPLOYED, ENVIRONMENT_NAME, NOT_FOUND, PROFILE_NAME,
};
use crate::node_invoke_run::package;

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern. Which GRANTS a
/// role carries is the neighbouring `handler_appconfig_execution_role`'s axis, not this one's.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The module and attribute the package below carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The runtime every row runs on. WHICH point release runs the module is
/// `python_runtime_bundled_sdk`'s axis.
const RUNTIME: &str = "python3.12";

/// The `@httpError` status AppConfigData's `ResourceNotFoundException` carries, which is the
/// status the handler's SDK reports the refusal under.
const NOT_FOUND_STATUS: i64 = 404;

/// The three environment variables the handler reads its identifiers out of.
const APPLICATION_VARIABLE: &str = "APP_CONFIG_APPLICATION";
const ENVIRONMENT_VARIABLE: &str = "APP_CONFIG_ENVIRONMENT";
const CONFIGURATION_VARIABLE: &str = "APP_CONFIG_CONFIGURATION";

/// Identifiers in the shape a CloudFormation template's `Ref` hands a misconfigured function:
/// a logical id with a hash suffix, which is the id and the name of no application,
/// environment or configuration profile, so every level given one is unresolvable.
const BROKEN_APPLICATION: &str = "OrdersFlagsApplication1D4E6F80";
const BROKEN_ENVIRONMENT: &str = "OrdersFlagsEnvironment4F1C0A2B";
const BROKEN_CONFIGURATION: &str = "OrdersFlagsProfile7AB3584E";

/// How long a clock-owed transition is given to come due before a row fails. Generous on
/// purpose: the spans themselves are the emulator's, and this suite waits for the CONDITION
/// (the function settled, nothing in flight) rather than for a span.
const SETTLE_BUDGET: Duration = Duration::from_secs(120);

/// How long to wait between two looks at a condition that is not true yet.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The document the handler falls back on when it cannot read its configuration, which is the
/// `DEFAULT_CONFIG` the source below carries.
fn fallback() -> Value {
    json!({ "useNewBackend": false })
}

/// The handler every row runs: it reads the three identifiers out of its OWN environment on
/// every invocation, opens a configuration session over them and reports the bytes it was
/// served — or, when AppConfig refuses the session, falls back to its own default document and
/// reports the refusal's own error code and status, which is what a feature-flagged client
/// does. It also reports the identifiers it read, so a row can tell an invocation that ran
/// against the replaced map from one that ran against the old one.
const HANDLER_SOURCE: &str = r#"
import base64
import json
import os

import boto3
import botocore.exceptions

# The document a handler that cannot reach its configuration has to fall back on.
DEFAULT_CONFIG = {"useNewBackend": False}


def lambda_handler(event, context):
    application = os.environ["APP_CONFIG_APPLICATION"]
    environment = os.environ["APP_CONFIG_ENVIRONMENT"]
    profile = os.environ["APP_CONFIG_CONFIGURATION"]
    reported = {"identifiers": [application, environment, profile]}
    client = boto3.client("appconfigdata")
    try:
        session = client.start_configuration_session(
            ApplicationIdentifier=application,
            EnvironmentIdentifier=environment,
            ConfigurationProfileIdentifier=profile,
        )
        answered = client.get_latest_configuration(
            ConfigurationToken=session["InitialConfigurationToken"]
        )
        document = answered["Configuration"].read()
    except botocore.exceptions.ClientError as error:
        reported["source"] = "default"
        reported["code"] = error.response["Error"]["Code"]
        reported["status"] = error.response["ResponseMetadata"]["HTTPStatusCode"]
        reported["config"] = DEFAULT_CONFIG
        return reported
    reported["source"] = "appconfig"
    reported["bytes"] = base64.b64encode(document).decode("ascii")
    reported["config"] = json.loads(document.decode("utf-8"))
    return reported
"#;

/// What must come of one stage's invocation.
enum Expect {
    /// Every identifier resolved, so the deployed document is served byte for byte.
    Served,
    /// At least one identifier resolved to nothing, so the session is refused with this error
    /// code and status and the handler falls back.
    Refused { code: &'static str, status: i64 },
}

/// One stage of the repair: the environment map the function carries from here on, and what
/// the invocation that follows must answer.
struct Stage {
    case: &'static str,
    /// The identifiers, in the order (application, environment, configuration profile).
    identifiers: [String; 3],
    expect: Expect,
}

impl Stage {
    /// The `Environment.Variables` map this stage's identifiers make, which is both what is
    /// submitted and what the configuration must read back as.
    fn variables(&self) -> BTreeMap<&'static str, String> {
        BTreeMap::from([
            (APPLICATION_VARIABLE, self.identifiers[0].clone()),
            (ENVIRONMENT_VARIABLE, self.identifiers[1].clone()),
            (CONFIGURATION_VARIABLE, self.identifiers[2].clone()),
        ])
    }

    /// That map as the JSON member a write carries and a read answers with.
    fn variables_document(&self) -> Value {
        Value::Object(
            self.variables()
                .into_iter()
                .map(|(name, value)| (name.to_string(), Value::String(value)))
                .collect(),
        )
    }
}

/// A stage whose triple is refused, because at least one of its identifiers resolves to
/// nothing.
fn refused(case: &'static str, identifiers: [&str; 3]) -> Stage {
    Stage {
        case,
        identifiers: identifiers.map(str::to_string),
        expect: Expect::Refused {
            code: NOT_FOUND,
            status: NOT_FOUND_STATUS,
        },
    }
}

/// A stage whose triple resolves at every level, so the deployed document is served.
fn served(case: &'static str, identifiers: [&str; 3]) -> Stage {
    Stage {
        case,
        identifiers: identifiers.map(str::to_string),
        expect: Expect::Served,
    }
}

/// The repair, stage by stage, spanning which identifiers resolve and the form a repaired one
/// is written in. The first stage is the map the function is CREATED with; every later one is
/// an `UpdateFunctionConfiguration` that replaces it.
fn stages(hierarchy: &Hierarchy) -> Vec<Stage> {
    let application = hierarchy.application_id.as_str();
    let environment = hierarchy.environment_id.as_str();
    let profile = hierarchy.profile_id.as_str();
    vec![
        refused(
            "none of the three identifiers resolves",
            [BROKEN_APPLICATION, BROKEN_ENVIRONMENT, BROKEN_CONFIGURATION],
        ),
        refused(
            "the application and the configuration profile repaired, the environment still not \
             resolving",
            [application, BROKEN_ENVIRONMENT, profile],
        ),
        refused(
            "the environment and the configuration profile repaired, the application still not \
             resolving",
            [BROKEN_APPLICATION, environment, profile],
        ),
        refused(
            "the application and the environment repaired, the configuration profile still not \
             resolving",
            [application, environment, BROKEN_CONFIGURATION],
        ),
        served(
            "all three repaired, each given as the id its level was minted with",
            [application, environment, profile],
        ),
        served(
            "all three given as the NAME their level was created under",
            [APPLICATION_NAME, ENVIRONMENT_NAME, PROFILE_NAME],
        ),
        refused(
            "the environment broken again after a served read",
            [application, BROKEN_ENVIRONMENT, profile],
        ),
        served(
            "the environment repaired once more",
            [application, environment, profile],
        ),
    ]
}

/// Read `GetFunctionConfiguration` for `function`.
async fn configuration(router: &MultiServiceRouter, function: &str) -> Value {
    let read = lambda(
        router,
        "GET",
        &format!("{FUNCTIONS_URI}/{function}/configuration"),
        &[],
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "expected HTTP 200 from GetFunctionConfiguration, got {} ({})",
        read.status,
        read.text()
    );
    read.json()
}

/// Wait until Lambda reports `function` `Active` with no update still in flight, which is when
/// an operator's next invocation runs against the configuration just written. Every stage of
/// the create and of each update is owed by the CLOCK, so this looks again until the condition
/// holds rather than sleeping for a span.
async fn await_settled(router: &MultiServiceRouter, case: &str, function: &str) {
    let deadline = Instant::now() + SETTLE_BUDGET;
    loop {
        let read = configuration(router, function).await;
        let settled = read["State"] == json!("Active")
            && read["LastUpdateStatus"] != json!("InProgress")
            && read["LastUpdateStatus"] != Value::Null;
        if settled {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the function never settled after the write, last read {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// `UpdateFunctionConfiguration` replacing the whole environment-variable map, then the read
/// back a caller makes: "the LastUpdateStatus [...] fields in the response from
/// GetFunctionConfiguration indicate when the update is complete", and once it is the
/// configuration reports exactly the map that was submitted.
async fn replace_environment(router: &MultiServiceRouter, function: &str, stage: &Stage) {
    let case = stage.case;
    let submitted = stage.variables_document();
    let updated = lambda(
        router,
        "PUT",
        &format!("{FUNCTIONS_URI}/{function}/configuration"),
        json!({ "Environment": { "Variables": submitted } })
            .to_string()
            .as_bytes(),
    )
    .await;
    assert_eq!(
        updated.status,
        200,
        "case {case:?}: expected HTTP 200 from UpdateFunctionConfiguration, got {} ({})",
        updated.status,
        updated.text()
    );
    await_settled(router, case, function).await;
    let read = configuration(router, function).await;
    assert_eq!(
        read["Environment"]["Variables"], submitted,
        "case {case:?}: expected the configuration to report exactly the submitted map \
         {submitted}, got {read}"
    );
    assert_eq!(
        read["Environment"]["Error"],
        Value::Null,
        "case {case:?}: expected the replaced variables to be readable, got {read}"
    );
}

/// Invoke `function` once, as `(the FunctionError signal, the payload)`.
async fn invoke(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
) -> (Option<String>, Value) {
    let answered = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/invocations"),
        json!({}).to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} ({})",
        answered.status,
        answered.text()
    );
    (answered.header("x-amz-function-error"), answered.json())
}

/// The bytes the handler reported being served, decoded from the base64 it carried them in.
///
/// `pub(crate)` so a sibling suite whose handler reports its bytes the same way reads them
/// through this one rather than through a second copy of it.
pub(crate) fn reported_bytes(case: &str, payload: &Value) -> Vec<u8> {
    let encoded = payload["bytes"].as_str().unwrap_or_else(|| {
        panic!("case {case:?}: expected the bytes the handler was served, got {payload}")
    });
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap_or_else(|error| {
            panic!("case {case:?}: expected base64 bytes, got {encoded:?} ({error})")
        })
}

/// A session a CLIENT opens over the same triple, as the error code and status it answered —
/// `None` when the triple was served one. The control every stage is compared against, so the
/// handler's own answer is AppConfig's answer and not something the runtime composed.
async fn client_session(
    router: &MultiServiceRouter,
    case: &str,
    identifiers: &[String; 3],
) -> Option<(String, i64)> {
    let answered = data_plane(
        router,
        "POST",
        "/configurationsessions",
        json!({
            "ApplicationIdentifier": identifiers[0],
            "EnvironmentIdentifier": identifiers[1],
            "ConfigurationProfileIdentifier": identifiers[2],
        })
        .to_string()
        .as_bytes(),
    )
    .await;
    if answered.status == 201 {
        return None;
    }
    let code = answered
        .json()
        .get("__type")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| answered.header("x-amzn-errortype"))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a modelled error code from StartConfigurationSession, \
                 got {}",
                answered.text()
            )
        });
    Some((code, i64::from(answered.status)))
}

/// Repairing the identifiers of a RUNNING function one at a time produces exactly three
/// answers: refused while any identifier resolves to nothing, whichever one it is, and the
/// deployed document once all three resolve — and it goes back to being refused the moment one
/// is broken again.
#[tokio::test]
async fn an_in_place_identifier_repair_reaches_the_configuration_only_once_all_three_resolve() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let router = emulator();
    let hierarchy = deployed_hierarchy(&router).await;

    // The suite's own control: a CLIENT's session and poll serve the deployed document, so a
    // stage that reports those bytes read them out of the data plane.
    let control = "a client's own session and poll";
    let polled = client_poll(&router, control, &hierarchy).await;
    assert_eq!(
        polled,
        DEPLOYED.to_vec(),
        "case {control:?}: expected the deployed document to be served to a client, got {polled:?}"
    );

    let plan = stages(&hierarchy);
    let function = "appconfig-identifier-repair";
    let first = &plan[0];
    let created = lambda(
        &router,
        "POST",
        FUNCTIONS_URI,
        json!({
            "FunctionName": function,
            "Role": ROLE,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": 25,
            "Environment": { "Variables": first.variables_document() },
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&[("lambda_function.py", HANDLER_SOURCE)])),
            },
        })
        .to_string()
        .as_bytes(),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "expected HTTP 201 from CreateFunction, got {} ({})",
        created.status,
        created.text()
    );

    for (index, stage) in plan.iter().enumerate() {
        let case = stage.case;
        if index == 0 {
            await_settled(&router, case, function).await;
            let read = configuration(&router, function).await;
            assert_eq!(
                read["Environment"]["Variables"],
                stage.variables_document(),
                "case {case:?}: expected the created function to carry the map it was given, \
                 got {read}"
            );
        } else {
            replace_environment(&router, function, stage).await;
        }

        let (signal, payload) = invoke(&router, case, function).await;
        assert_eq!(
            signal, None,
            "case {case:?}: the handler must run to completion, got the function error \
             {signal:?} with {payload}"
        );
        let read_identifiers = Value::Array(
            stage
                .identifiers
                .iter()
                .map(|identifier| Value::String(identifier.clone()))
                .collect(),
        );
        assert_eq!(
            payload["identifiers"], read_identifiers,
            "case {case:?}: expected the invocation to read the current map {read_identifiers}, \
             got {payload}"
        );

        // What a CLIENT's own session over the same triple answers, so each stage's verdict is
        // the data plane's and not the runtime's.
        let client = client_session(&router, case, &stage.identifiers).await;

        match stage.expect {
            Expect::Served => {
                assert_eq!(
                    payload["source"],
                    json!("appconfig"),
                    "case {case:?}: expected the handler to be served its configuration, got \
                     {payload}"
                );
                let served_bytes = reported_bytes(case, &payload);
                assert_eq!(
                    served_bytes,
                    DEPLOYED.to_vec(),
                    "case {case:?}: expected the deployed document's own bytes {:?}, got \
                     {served_bytes:?}",
                    DEPLOYED
                );
                assert_eq!(
                    payload["config"],
                    json!({ "useNewBackend": true }),
                    "case {case:?}: expected the deployed flag document, got {payload}"
                );
                assert_eq!(
                    client, None,
                    "case {case:?}: expected a client's own session over the same triple to be \
                     served too, got {client:?}"
                );
            }
            Expect::Refused { code, status } => {
                assert_eq!(
                    payload["source"],
                    json!("default"),
                    "case {case:?}: expected the handler to fall back on its own default, got \
                     {payload}"
                );
                assert_eq!(
                    payload["code"],
                    json!(code),
                    "case {case:?}: expected the service's own error code, got {payload}"
                );
                assert_eq!(
                    payload["status"],
                    json!(status),
                    "case {case:?}: expected the refusal's own status, got {payload}"
                );
                assert_eq!(
                    payload["config"],
                    fallback(),
                    "case {case:?}: expected the handler's own default document, got {payload}"
                );
                assert_eq!(
                    payload["bytes"],
                    Value::Null,
                    "case {case:?}: a refused session must serve no configuration at all, got \
                     {payload}"
                );
                assert_eq!(
                    client,
                    Some((code.to_string(), status)),
                    "case {case:?}: expected a client's own session over the same triple to be \
                     refused the same way, got {client:?}"
                );
            }
        }
    }

    // And the configuration AppConfig holds is unchanged by every read above: a client polls
    // the same deployed bytes it polled before the repair ran.
    let after = "the data plane's own view after the repair";
    let polled = client_poll(&router, after, &hierarchy).await;
    assert_eq!(
        polled,
        DEPLOYED.to_vec(),
        "case {after:?}: expected the deployed document to still be what a client is served, \
         got {polled:?}"
    );
}
