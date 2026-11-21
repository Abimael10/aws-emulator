// SPDX-License-Identifier: CC0-1.0

//! What a client POLLING a function for an answer gets, end to end over the wire: two
//! `python3.x` functions that read the same deployed configuration out of their own environment
//! under sibling execution roles that differ only in whether they grant AppConfig's read
//! actions, invoked with every shape of payload an invocation may carry.
//!
//! RULE: a synchronous invocation carries ONE run's answer, so a handler whose configuration read
//! is answered either way settles inside that invocation — HTTP 200 with the handler's own
//! payload and no `X-Amz-Function-Error` — and the read's verdict is decided by the function's
//! execution role and the identifiers the call carried, never by what the invocation's payload
//! holds: the deployed document for a role the policies allow, `AccessDeniedException` (HTTP 403)
//! with IAM's own sentence for a role granted none of the actions. Because a configuration read
//! opens its own session and consumes nothing, invoking the same function again answers the same
//! verdict, which is what makes a client that retries an invocation until it has an answer stop
//! on the first one rather than keep looking.
//!
//! The rows differ along what the rule generalises over: the FORM and content of the payload the
//! invocation carries (the object a readiness probe sends, an empty object, a JSON array, a JSON
//! string, JSON null — the handler reports the type it was handed, so a row knows its payload
//! arrived), which of the two sibling execution roles the function runs as, and whether this is
//! the FIRST invocation of that pair or a repeat of it.
//!
//! What the neighbours pin and this one does not repeat: `handler_appconfig_execution_role`
//! drives the grant forms one role carries, `handler_appconfig_sibling_execution_roles` drives
//! two roles over several deployed documents (and owns the five-service fixture this suite
//! deploys through), `handler_appconfig_identifier_repair` drives the identifiers a running
//! function reads out of its environment, and `handler_appconfig_sdk` pins what an authorized
//! handler reads over both of AppConfig's APIs. What this suite adds is that the verdict is the
//! SAME for every payload an invocation may carry and the same again when the invocation is
//! repeated.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]): this emulator serves at most
//! `max_concurrent_runs` invocations at once for the whole process.

use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};

// The fixture the sibling suite exports — the five services on one engine, the execution roles,
// the deployed hierarchy and the sentences a refusal is compared against — reused here rather
// than copied.
use crate::handler_appconfig_sibling_execution_roles::{
    allow, configuration_arn, denial, deploy, handler_default, session_principal, Emulator,
    Hierarchy, NO_APPCONFIG_ACTION,
};
use crate::invoke_request_id::one_invocation_stream_at_a_time;
// The one .zip writer this crate's suites package a handler with.
use crate::node_invoke_run::package;

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The module and attribute the package below carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The runtime both functions run on. WHICH point release runs the module is
/// `python_runtime_bundled_sdk`'s axis, not this suite's.
const RUNTIME: &str = "python3.12";

/// The code and status AWS states an authorization failure with.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: i64 = 403;

/// The two actions AppConfig publishes the data plane's operations under, which is how a policy
/// statement spells them.
const START_SESSION_ACTION: &str = "appconfig:StartConfigurationSession";
const GET_CONFIGURATION_ACTION: &str = "appconfig:GetLatestConfiguration";

/// The names the hierarchy both functions read is created under, and the document the deployment
/// puts in force.
const NAMES: (&str, &str, &str) = ("orders-probe-flags", "production", "backend-rollout");
const DEPLOYED: &str = r#"{"useNewBackend": true}"#;

/// The three environment variables the handler reads its identifiers out of, so nothing it needs
/// comes from the invocation's payload.
const APPLICATION_VARIABLE: &str = "APP_CONFIG_APPLICATION";
const ENVIRONMENT_VARIABLE: &str = "APP_CONFIG_ENVIRONMENT";
const CONFIGURATION_VARIABLE: &str = "APP_CONFIG_CONFIGURATION";

/// The two functions, and the roles they run as.
const PERMITTED_FUNCTION: &str = "appconfig-probe-permitted";
const PERMITTED_ROLE: &str = "lambda-appconfig-probe-permitted";
const UNPRIVILEGED_FUNCTION: &str = "appconfig-probe-unprivileged";
const UNPRIVILEGED_ROLE: &str = "lambda-appconfig-probe-unprivileged";

/// How long a condition the CLOCK owes — the function becoming `Active`, Lambda having a slot to
/// serve an invocation in — is waited for. Generous on purpose: nothing here measures how fast
/// the host runs.
const PATIENCE: Duration = Duration::from_secs(120);

/// How long ONE dispatch is given to answer at all before the suite fails with its own message
/// instead of stalling, which is what rules out an invocation that never settles — the symptom
/// this suite is about. Deliberately far longer than any answer takes, so only a dispatch that
/// never answers reaches it; a run that blocks rather than awaits is bounded by the function's
/// own configured timeout instead.
const ANSWER_BUDGET: Duration = Duration::from_secs(240);

/// How long to wait between two looks at a condition that is not true yet.
const BETWEEN_LOOKS: Duration = Duration::from_millis(50);

/// The statuses Lambda answers an invocation it has NOT served with — the concurrency this
/// process can hold at once was full (`TooManyRequestsException`, HTTP 429) or the function was
/// not `Active` yet (`ResourceConflictException`, HTTP 409). Neither carries an answer from a
/// handler, so a row looks again on one instead of reading a verdict out of a run that never
/// happened.
const UNSERVED: &[u16] = &[409, 429];

/// The handler both functions run: it reads the three identifiers out of its OWN environment — so
/// nothing it needs comes from the payload — opens a configuration session over them, polls it
/// once, and reports the document it was served; on any refusal it reports that refusal's own
/// code, status and sentence and falls back to the default compiled into it, the shape a
/// feature-flagged client has. It also reports the payload it was handed and that payload's type,
/// so a row can tell that its own invocation ran.
const HANDLER_SOURCE: &str = r#"
import json
import os

import boto3
import botocore.exceptions

# The document a handler that cannot reach its configuration has to fall back on.
DEFAULT_CONFIG = {"useNewBackend": False}


def lambda_handler(event, context):
    reported = {"event": event, "event_type": type(event).__name__}
    client = boto3.client("appconfigdata")
    try:
        session = client.start_configuration_session(
            ApplicationIdentifier=os.environ["APP_CONFIG_APPLICATION"],
            EnvironmentIdentifier=os.environ["APP_CONFIG_ENVIRONMENT"],
            ConfigurationProfileIdentifier=os.environ["APP_CONFIG_CONFIGURATION"],
        )
        answered = client.get_latest_configuration(
            ConfigurationToken=session["InitialConfigurationToken"]
        )
        document = answered["Configuration"].read()
    except botocore.exceptions.ClientError as error:
        reported["source"] = "default"
        reported["code"] = error.response["Error"]["Code"]
        reported["message"] = error.response["Error"]["Message"]
        reported["status"] = error.response["ResponseMetadata"]["HTTPStatusCode"]
        reported["config"] = DEFAULT_CONFIG
        return reported
    reported["source"] = "appconfig"
    reported["config"] = json.loads(document.decode("utf-8"))
    return reported
"#;

/// The `Environment.Variables` map a function carries, which is where its handler reads the three
/// identifiers from — the deployed hierarchy's own ids, so the payload names nothing it needs.
fn variables(hierarchy: &Hierarchy) -> Value {
    json!({
        APPLICATION_VARIABLE: hierarchy.application,
        ENVIRONMENT_VARIABLE: hierarchy.environment,
        CONFIGURATION_VARIABLE: hierarchy.profile,
    })
}

/// Create the function `name`, running as `role_arn`, carrying `variables` as its environment.
///
/// The sibling fixture's own `function` creates a function whose handler takes its identifiers
/// from the EVENT; this suite's handler takes them from its environment, which is the one member
/// of the create that differs.
async fn reader_function(emulator: &Emulator, name: &str, role_arn: &str, variables: &Value) {
    let (status, _headers, created) = emulator
        .rest(
            "lambda",
            "lambda",
            "POST",
            FUNCTIONS_URI,
            "application/json",
            &serde_json::to_vec(&json!({
                "FunctionName": name,
                "Role": role_arn,
                "Runtime": RUNTIME,
                "Handler": HANDLER,
                "Timeout": 25,
                "Environment": { "Variables": variables },
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
        "setup: expected HTTP 201 from CreateFunction for {name}, got {status} ({created})"
    );
}

/// Invoke `function` with the payload text `payload` and answer the handler's own report,
/// asserting on the way that the invocation was served HTTP 200 and the handler ran to completion
/// — which is what a refusal the handler CAUGHT looks like from outside.
///
/// Each dispatch is bounded by [`ANSWER_BUDGET`], so an invocation that never settles — the very
/// symptom this suite is about — fails the row with its own message instead of hanging until the
/// harness kills the target. A refusal that says only "not now" ([`UNSERVED`]) is looked at again,
/// so what this answers is always the answer of a handler that ran, on a loaded host as on an
/// idle one.
async fn invoked(emulator: &Emulator, case: &str, function: &str, payload: &str) -> Value {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let answered = tokio::time::timeout(
            ANSWER_BUDGET,
            emulator.rest(
                "lambda",
                "lambda",
                "POST",
                &format!("{FUNCTIONS_URI}/{function}/invocations"),
                "application/json",
                payload.as_bytes(),
            ),
        )
        .await;
        let (status, headers, payload_answered) = answered.unwrap_or_else(|_elapsed| {
            panic!(
                "case {case:?}: the invocation of {function} must settle with an answer, and none \
                 arrived at all"
            )
        });
        if UNSERVED.contains(&status) {
            assert!(
                Instant::now() < deadline,
                "case {case:?}: Lambda must serve this row's invocation, last answered {status} \
                 with {payload_answered}"
            );
            tokio::time::sleep(BETWEEN_LOOKS).await;
            continue;
        }
        assert_eq!(
            status, 200,
            "case {case:?}: the invocation must be served, got {status} ({payload_answered})"
        );
        let signal = headers
            .get("x-amz-function-error")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        assert_eq!(
            signal, None,
            "case {case:?}: the handler must run to completion, got the function error {signal:?} \
             with {payload_answered}"
        );
        return payload_answered;
    }
}

/// One payload an invocation may carry: the JSON text sent as the invocation's payload, and the
/// Python type a handler is handed it as — the two together say the payload reached the run
/// unchanged.
struct Payload {
    case: &'static str,
    body: &'static str,
    python_type: &'static str,
}

/// The payload forms, spanning the JSON shapes an invocation's payload may take: the object a
/// client's readiness probe sends, an object with nothing in it, an array, a string and null.
/// None of them names an identifier, which is the point: the handler's read is decided by its
/// role and its environment, not by what the caller sent.
const PAYLOADS: &[Payload] = &[
    Payload {
        case: "the object a readiness probe sends",
        body: r#"{"probe": "readiness"}"#,
        python_type: "dict",
    },
    Payload {
        case: "an object with nothing in it",
        body: "{}",
        python_type: "dict",
    },
    Payload {
        case: "a JSON array",
        body: "[1, 2, 3]",
        python_type: "list",
    },
    Payload {
        case: "a JSON string",
        body: r#""readiness""#,
        python_type: "str",
    },
    Payload {
        case: "JSON null",
        body: "null",
        python_type: "NoneType",
    },
];

/// One of the two sibling functions: its name, the role it runs as, and whether that role's
/// policies allow the data plane's actions.
struct Probe {
    function: &'static str,
    role: &'static str,
    permitted: bool,
}

const PROBES: &[Probe] = &[
    Probe {
        function: PERMITTED_FUNCTION,
        role: PERMITTED_ROLE,
        permitted: true,
    },
    Probe {
        function: UNPRIVILEGED_FUNCTION,
        role: UNPRIVILEGED_ROLE,
        permitted: false,
    },
];

/// RULE: a handler's configuration read settles inside the invocation that ran it and its verdict
/// comes from the function's execution role, so every payload an invocation may carry gets the
/// same answer — the deployed document under a role the policies allow, `AccessDeniedException`
/// (HTTP 403) with IAM's own sentence under a role granted none of AppConfig's actions — and
/// repeating the invocation answers it again, because the read opens its own session and consumes
/// nothing.
#[tokio::test]
async fn an_appconfig_reading_handler_answers_the_same_verdict_for_every_invocation_payload() {
    let _stream = one_invocation_stream_at_a_time().await;
    let emulator = Emulator::new();
    let hierarchy = deploy(&emulator, NAMES, DEPLOYED).await;
    let resource = configuration_arn(&hierarchy);

    // The suite's own control: a CLIENT's session and poll serve the deployed document, so a row
    // reporting those bytes read them out of the data plane.
    let control = "a client's own session and poll";
    let polled = emulator.served_bytes(control, &hierarchy).await;
    assert_eq!(
        polled,
        DEPLOYED.as_bytes(),
        "case {control:?}: expected the deployed document to be served to a client, got {:?}",
        String::from_utf8_lossy(&polled)
    );

    // Two sibling roles over the same configuration: one granted both of the data plane's
    // actions, one granted only the log actions every execution role carries and no AppConfig
    // action at all.
    let permitted = emulator.execution_role(PERMITTED_ROLE).await;
    emulator
        .role_policy(
            "the permitted role",
            PERMITTED_ROLE,
            &allow(&[START_SESSION_ACTION, GET_CONFIGURATION_ACTION], &resource),
        )
        .await;
    let unprivileged = emulator.execution_role(UNPRIVILEGED_ROLE).await;
    emulator
        .role_policy(
            "the role granted no AppConfig action",
            UNPRIVILEGED_ROLE,
            NO_APPCONFIG_ACTION,
        )
        .await;

    let variables = variables(&hierarchy);
    reader_function(&emulator, PERMITTED_FUNCTION, &permitted, &variables).await;
    reader_function(&emulator, UNPRIVILEGED_FUNCTION, &unprivileged, &variables).await;
    for probe in PROBES {
        emulator.await_function_active(probe.function).await;
    }

    for payload in PAYLOADS {
        for probe in PROBES {
            let case = format!(
                "{} invoked with {}",
                if probe.permitted {
                    "the permitted role"
                } else {
                    "the role granted no AppConfig action"
                },
                payload.case
            );
            let answer = invoked(&emulator, &case, probe.function, payload.body).await;

            // The payload reached the run as the caller sent it, whatever shape it has.
            assert_eq!(
                answer["event_type"],
                json!(payload.python_type),
                "case {case:?}: expected the handler to be handed a {} event, got {answer}",
                payload.python_type
            );
            assert_eq!(
                answer["event"],
                serde_json::from_str::<Value>(payload.body).expect("the row's payload is JSON"),
                "case {case:?}: expected the handler to be handed the payload the invocation \
                 carried, got {answer}"
            );

            if probe.permitted {
                assert_eq!(
                    answer["source"],
                    json!("appconfig"),
                    "case {case:?}: expected the authorized read to be served, got {answer}"
                );
                assert_eq!(
                    answer["config"],
                    serde_json::from_str::<Value>(DEPLOYED).expect("the deployed document is JSON"),
                    "case {case:?}: expected the deployed flag document, got {answer}"
                );
            } else {
                assert_eq!(
                    answer["source"],
                    json!("default"),
                    "case {case:?}: expected the handler to fall back to its own default, got \
                     {answer}"
                );
                assert_eq!(
                    answer["code"],
                    json!(ACCESS_DENIED),
                    "case {case:?}: expected the authorization refusal rather than a not-found \
                     one, got {answer}"
                );
                assert_eq!(
                    answer["status"],
                    json!(ACCESS_DENIED_STATUS),
                    "case {case:?}: expected HTTP {ACCESS_DENIED_STATUS} for an unauthorized \
                     caller, got {answer}"
                );
                assert_eq!(
                    answer["message"],
                    json!(denial(
                        &session_principal(probe.role, probe.function),
                        START_SESSION_ACTION,
                        &resource
                    )),
                    "case {case:?}: expected IAM's own denial sentence, got {answer}"
                );
                assert_eq!(
                    answer["config"],
                    handler_default(),
                    "case {case:?}: expected the handler's own default document, got {answer}"
                );
            }

            // The same invocation, again: a configuration read opens its own session and consumes
            // nothing, so the repeat answers exactly what the first one did — which is what makes
            // a client that retries an invocation stop on the answer it already has.
            let repeated = format!("{case}, invoked a second time");
            let again = invoked(&emulator, &repeated, probe.function, payload.body).await;
            assert_eq!(
                again, answer,
                "case {repeated:?}: expected the repeat to answer exactly what the first \
                 invocation did ({answer}), got {again}"
            );
        }
    }

    // And nothing the runs above did changed what AppConfig holds: a client polls the same
    // deployed bytes it polled before them, so no read consumed the configuration.
    let after = "a client's own poll after every invocation";
    let polled = emulator.served_bytes(after, &hierarchy).await;
    assert_eq!(
        polled,
        DEPLOYED.as_bytes(),
        "case {after:?}: expected the deployed document to still be what a client is served, got \
         {:?}",
        String::from_utf8_lossy(&polled)
    );
}
