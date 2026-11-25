// SPDX-License-Identifier: CC0-1.0

//! WHICH OPERATIONS the AWS SDK a running Python handler imports can carry: a client the
//! runtime's catalog answers to is a client bound to the named service's OWN MODEL, method by
//! method, end to end over the wire.
//!
//! RULE: a managed `python3.x` runtime ships the AWS SDK for Python on the function's import
//! path with a client for every service the runtime's call seam binds, so a client built for
//! such a service resolves against THAT service's own model — a method naming an operation the
//! model declares is carried to the service, and a method naming no operation of it is an
//! attribute the client does not have, which is what the SDK reports rather than a call under a
//! name the service never named. An invocation whose handler then RETURNS is a successful
//! `RequestResponse` invocation: HTTP 200 with NO `X-Amz-Function-Error`, because
//! `InvocationResponse$FunctionError` is in-band and set only where the function's own code
//! raised.
//!
//! Where this sits among its neighbours. `crate::handler_sdk_catalog_is_complete` probes every
//! token the catalog lists at the TOKEN level: each builds a client that resolves to its own
//! entry. `crate::handler_sdk_service_inventory` spans hand-picked token rows and asserts the
//! catalog the runtime reports. `crate::handler_aws_sdk` drives ONE service's operations against
//! that service mounted beside Lambda. What none of them says is that the catalog is bound at
//! the OPERATION level for every service in it: that each token resolves to a service whose
//! model was really consulted for the method called, and that the method a wrapper handler
//! actually calls is one its service's model declares. A service listed and token-resolved but
//! whose operations no model backs is a service whose every call comes back as a method the
//! client does not have — which, from outside, is the same unhandled handler failure as a
//! missing client: a thin wrapper whose whole job is one call to another service reports a
//! botocore error document instead of its own.
//!
//! The rows differ along what the rule generalises over: the SWEEP covers every token the seam
//! binds — whichever service crate contributed it and whichever protocol it speaks — probed with
//! a method no model declares; and the modelled rows name an operation each of those services'
//! models does declare, across the AWS JSON protocols (`sqs`, `secretsmanager`, `ecr`, `ecs`),
//! `restJson1` (`batch`, `apigateway`), the query protocols (`sns`, `ses`) and the other XML one
//! (`cloudfront`), including the job submission and the job read a container-workload wrapper
//! is built around.
//!
//! No row needs the CALLED service mounted. A sweep row is decided from the seam's own tables
//! and the called service's model, before any request is built: the token resolves and the
//! method names no operation of it. A modelled row asserts only that the client BOUND the
//! method — the call itself is carried as far as this emulator's router, where nothing serves
//! it, so what comes back is a refusal of the call rather than an answer, and what a served
//! call answers belongs to the service that owns the state, which the cross-service suites
//! drive.
//!
//! The suite RUNS a handler, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::aws_api::reachable_services;
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{await_function_active, call, router_on};
// The one .zip writer this crate's suites package a handler with, exported by the suite that
// first needed it rather than copied here.
use crate::node_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// A client method naming no operation any service's model declares, which is what every token
/// of the sweep is probed with: the answer tells a token whose service's model was consulted
/// (so the method is simply not on the client) from one the SDK does not answer to at all.
const UNMODELLED_METHOD: &str = "no_such_operation_exists";

/// The class the SDK raises for a token no service of the seam answers to, which no token the
/// seam binds may be raised as.
const UNKNOWN_SERVICE: &str = "UnknownServiceError";

/// The class an SDK reports a method its client does not carry as.
const MISSING_ATTRIBUTE: &str = "AttributeError";

/// One probe: the token the handler builds a client with, the method it calls on it, and what
/// the SDK must do about it.
struct Probe {
    case: String,
    token: String,
    method: String,
    expected: Expected,
}

/// What the SDK must do with one probe.
#[derive(PartialEq, Eq, Debug)]
enum Expected {
    /// The method names no operation the named service's model declares: the attribute is
    /// missing, and the sentence names the method asked for.
    MissingMethod,
    /// The method names an operation that model does declare: the client bound it, so neither
    /// an unknown service nor a missing attribute is what comes back — what the call itself
    /// answers belongs to the service that owns the state.
    BoundMethod,
}

/// The operations a handler calls those services by, one per protocol the seam binds. Each name
/// is an operation that service's own model declares, read from the model the seam embeds for it
/// — e.g. `smithy_query(query="operation", operation="SubmitJob", model_path="services/\
/// service-com-amazonaws-batch-awsbatchv20160810/smithy-model/\
/// com-amazonaws-batch-awsbatchv20160810.json")`.
const MODELLED: &[(&str, &str)] = &[
    // `restJson1`: the job service a thin wrapper hands long-running work to, whose `SubmitJob`
    // answers the id every later read of that work is made against, the read of that id, and the
    // write that mints a definition revision.
    ("batch", "submit_job"),
    ("batch", "describe_jobs"),
    ("batch", "register_job_definition"),
    // `restJson1` again, for a service of another crate: a listing.
    ("apigateway", "get_rest_apis"),
    // The AWS JSON protocols: a queue write, a secret read, the registry an image is pulled
    // from and the container service a placed job runs as.
    ("sqs", "send_message"),
    ("secretsmanager", "get_secret_value"),
    ("ecr", "describe_repositories"),
    ("ecs", "describe_clusters"),
    // The query protocols: a notification and a message.
    ("sns", "publish"),
    ("ses", "send_email"),
    // The other XML protocol: a listing of the content delivery service's distributions.
    ("cloudfront", "list_distributions"),
];

fn probes() -> Vec<Probe> {
    let mut probes = Vec::new();
    // The sweep: every token the seam binds, probed with a method no model declares.
    for token in reachable_services() {
        probes.push(Probe {
            case: format!("a method no model declares, on the service token {token:?}"),
            token: token.to_string(),
            method: UNMODELLED_METHOD.to_string(),
            expected: Expected::MissingMethod,
        });
    }
    for (token, method) in MODELLED {
        probes.push(Probe {
            case: format!("{method:?}, an operation the model of {token:?} declares"),
            token: (*token).to_string(),
            method: (*method).to_string(),
            expected: Expected::BoundMethod,
        });
    }
    probes
}

/// The handler every probe runs: it builds a client for the token the event names, calls the
/// method it names, and reports WHAT the SDK did about it — the exception class and its
/// sentence, or the answer's own members — so what is asserted is what the SDK gave the guest.
const PROBE_HANDLER: &str = r#"
import boto3
import botocore.exceptions


def lambda_handler(event, context):
    reported = {}
    for probe in event["probes"]:
        at = probe["at"]
        client = boto3.client(probe["token"])
        try:
            answered = getattr(client, probe["method"])()
        except botocore.exceptions.UnknownServiceError as error:
            reported[at] = {"raised": "UnknownServiceError", "sentence": str(error)}
        except AttributeError as error:
            reported[at] = {"raised": "AttributeError", "sentence": str(error)}
        except botocore.exceptions.ClientError as error:
            reported[at] = {
                "raised": "ClientError",
                "code": error.response["Error"]["Code"],
            }
        except botocore.exceptions.BotoCoreError as error:
            reported[at] = {"raised": type(error).__name__, "sentence": str(error)}
        else:
            reported[at] = {"raised": None, "members": sorted(answered)}
    return {"reported": reported}
"#;

/// Lambda alone: no probe needs the named service mounted.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    router_on(&engine)
}

/// The part of a missing-attribute sentence that is the SDK's own wherever it spells the client's
/// own name: it names the METHOD the caller asked for, which is the fact under test here (the
/// suites over an SDK's own wording are the ones that drive it).
fn missing_method_tail(method: &str) -> String {
    format!("has no attribute '{method}'")
}

#[tokio::test]
async fn every_service_the_seam_binds_has_its_operations_bound_from_its_own_model() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "a handler calling a method on a client for each service the seam binds";
    let router = emulator();

    let caller = "sdk-catalog-binding-caller";
    let created = call(
        &router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": caller,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "lambda_function.lambda_handler",
            "Timeout": 60,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&[("lambda_function.py", PROBE_HANDLER)])),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: expected CreateFunction to answer 201, got {} {}",
        created.status,
        created.text()
    );
    await_function_active(&router, case, caller).await;

    let probes = probes();
    assert!(
        probes.len() > MODELLED.len(),
        "case {case:?}: expected the seam to report services to sweep, got {} probes",
        probes.len()
    );
    let asked: Vec<Value> = probes
        .iter()
        .enumerate()
        .map(|(at, probe)| {
            json!({ "at": at.to_string(), "token": probe.token, "method": probe.method })
        })
        .collect();

    let answered = call(
        &router,
        "POST",
        &format!("{FUNCTIONS_URI}/{caller}/invocations"),
        &json!({ "probes": asked }),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
        answered.status,
        answered.text()
    );
    // A handler that returns is a successful invocation: the signal is set only where the
    // handler's own code raised, so a run that reports every probe carries none.
    assert_eq!(
        answered.header("x-amz-function-error"),
        None,
        "case {case:?}: expected no X-Amz-Function-Error from a handler that returned, got {}",
        answered.text()
    );
    let reported = answered.json()["reported"].clone();

    for (at, probe) in probes.iter().enumerate() {
        let got = reported[at.to_string()].clone();
        assert!(
            got.is_object(),
            "case {:?}: expected the handler to report this probe, got {got:?}",
            probe.case
        );
        let raised = got["raised"].as_str();
        // Whatever the method, the token is one the seam binds: no probe here may be refused as
        // a service the SDK does not answer to.
        assert_ne!(
            raised,
            Some(UNKNOWN_SERVICE),
            "case {:?}: the seam binds the token {:?}, so the SDK answers to it; got {got:?}",
            probe.case,
            probe.token
        );
        match probe.expected {
            Expected::MissingMethod => {
                assert_eq!(
                    raised,
                    Some(MISSING_ATTRIBUTE),
                    "case {:?}: expected the model of {:?} to be consulted and the method \
                     reported as missing, got {got:?}",
                    probe.case,
                    probe.token
                );
                let sentence = got["sentence"].as_str().unwrap_or_default();
                assert!(
                    sentence.ends_with(&missing_method_tail(&probe.method)),
                    "case {:?}: expected the missing-attribute sentence to name the method \
                     {:?}, got {sentence:?}",
                    probe.case,
                    probe.method
                );
            }
            Expected::BoundMethod => {
                assert_ne!(
                    raised,
                    Some(MISSING_ATTRIBUTE),
                    "case {:?}: expected the client to carry the method {:?} its service's model \
                     declares, got {got:?}",
                    probe.case,
                    probe.method
                );
            }
        }
    }
}
