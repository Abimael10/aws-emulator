// SPDX-License-Identifier: CC0-1.0

//! WHICH runtime version a function may be pinned to, and which BUILD an execution
//! environment reports initialising on.
//!
//! RULE: a runtime version is a BUILD of ONE managed runtime, so the two readings of it agree
//! on which runtime it belongs to. `PutRuntimeManagementConfig` with `UpdateRuntimeOn`
//! `Manual` resolves the `RuntimeVersionArn` it is offered to the runtime that version is a
//! build of and refuses — HTTP 400 `InvalidParameterValueException`, persisting nothing — one
//! that is not the runtime the addressed version is configured with; and the record that opens
//! a new execution environment's log names the BUILD of the function's runtime
//! (`<family>:<line>.v<build>`) beside the ARN of that same version, rather than the runtime
//! identifier.
//!
//! The rows differ along what the rule generalises over: which runtime the function is
//! configured with (python, node and java functions, so no row is keyed to one language),
//! which runtime the offered version belongs to (the function's own, another one, and no
//! runtime at all), which of the two forms the model's `RuntimeVersionArn` pattern admits the
//! ARN is written in, and — for the environment's own record — which engine ran it.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{
    await_function_active, call, refusal_message, router_on, Answer,
};
use crate::invocation_log_delivery::{
    create_function_logging, emulator, invoke, Created, NODE_RUNTIME, PYTHON_RUNTIME,
};

/// Where `CreateFunction` and the function reads are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The only `UpdateRuntimeOn` mode that pins a runtime version: "This is only required if
/// you're using the Manual runtime update mode."
const MANUAL: &str = "Manual";

/// The mode a version carries before anyone pins one, which `UpdateRuntimeOn`'s own
/// documentation states: "Auto (default)".
const AUTO: &str = "Auto";

/// The error the model declares for a request member that names no configuration the function
/// can be in (`smithy_query(query="operation", operation="PutRuntimeManagementConfig")`:
/// `InvalidParameterValueException`, `httpError` 400).
const INVALID_PARAMETER: &str = "InvalidParameterValueException";

/// Runtimes of three different languages, from the model's own `Runtime` enum
/// (`smithy_query(query="shape", shape="Runtime")`): the rows pin between them, so the rule is
/// asserted on runtimes no check of the issue cited as well as on the two engines that run.
const JAVA_RUNTIME: &str = "java21";

/// The record that opens the plain-text log of a new execution environment.
const INIT_START_PREFIX: &str = "INIT_START Runtime Version: ";

/// What separates the build the environment initialised on from the ARN of that same version,
/// on that record.
const INIT_START_ARN_FIELD: &str = "\tRuntime Version ARN: ";

/// Where both runtime-management operations are bound, for the unpublished version of `name`.
fn runtime_management_uri(name: &str) -> String {
    format!("/2021-07-20/functions/{name}/runtime-management-config")
}

/// Create the function `name` on `runtime` with a package nothing runs — these rows read and
/// write its configuration and never invoke it — and wait until it reports `Active`.
async fn given_function(router: &MultiServiceRouter, case: &str, name: &str, runtime: &str) {
    let package: Vec<u8> = (0..64u16)
        .map(|byte| u8::try_from(byte % 251).unwrap_or(0))
        .collect();
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": runtime,
            "Handler": "lambda_function.lambda_handler",
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(&package),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: expected CreateFunction {name:?} to answer 201, got {} {}",
        created.status,
        created.text()
    );
    await_function_active(router, case, name).await;
}

/// The runtime version ARN a read of the function `name` reports for it, which is the version
/// its own runtime is a build of.
async fn own_runtime_version_arn(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let read = call(
        router,
        "GET",
        &format!("{FUNCTIONS_URI}/{name}/configuration"),
        &Value::Null,
    )
    .await;
    read.json()["RuntimeVersionConfig"]["RuntimeVersionArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected {name:?} to report a runtime version ARN, got {}",
                read.text()
            )
        })
        .to_string()
}

/// `PutRuntimeManagementConfig` on the unpublished version of `name`, in `mode` and with the
/// runtime version ARN `arn` when one is sent.
async fn put_runtime_management(
    router: &MultiServiceRouter,
    name: &str,
    mode: &str,
    arn: Option<&str>,
) -> Answer {
    let mut body = json!({ "UpdateRuntimeOn": mode });
    if let Some(arn) = arn {
        body["RuntimeVersionArn"] = json!(arn);
    }
    call(router, "PUT", &runtime_management_uri(name), &body).await
}

/// `GetRuntimeManagementConfig` on the unpublished version of `name`.
async fn get_runtime_management(router: &MultiServiceRouter, name: &str) -> Answer {
    call(router, "GET", &runtime_management_uri(name), &Value::Null).await
}

/// How a row writes the runtime version ARN it offers.
#[derive(Clone, Copy, Debug)]
enum ArnForm {
    /// `arn:<partition>:lambda:<region>::runtime:<id>`, the version as this account reports it
    /// for a function on that runtime — the first branch of the member's pattern.
    Reported,
    /// `arn:<partition>:lambda:<region>:<account>:runtime/<name>/version/<version>`, the second
    /// branch, which names the runtime in the path itself.
    Path,
}

/// One pin: the runtime the function is configured with, the runtime the offered version
/// belongs to, how the ARN is written, and whether the pin holds.
struct Pin {
    case: &'static str,
    /// The runtime the function under test is created with.
    runtime: &'static str,
    /// The runtime whose version is offered, or `None` for an ARN that is no runtime's version.
    offered: Option<&'static str>,
    form: ArnForm,
    /// Whether the pin is stored, rather than refused.
    accepted: bool,
}

/// The rows. Between them they cover the function's own version and another runtime's, three
/// configured runtimes, both forms of the ARN, and an ARN naming no runtime at all.
fn pins() -> Vec<Pin> {
    vec![
        Pin {
            case: "a python function pinned to its own runtime version",
            runtime: PYTHON_RUNTIME,
            offered: Some(PYTHON_RUNTIME),
            form: ArnForm::Reported,
            accepted: true,
        },
        Pin {
            case: "a python function pinned to a node runtime's version",
            runtime: PYTHON_RUNTIME,
            offered: Some(NODE_RUNTIME),
            form: ArnForm::Reported,
            accepted: false,
        },
        Pin {
            case: "a node function pinned to a java runtime's version",
            runtime: NODE_RUNTIME,
            offered: Some(JAVA_RUNTIME),
            form: ArnForm::Reported,
            accepted: false,
        },
        Pin {
            case: "a java function pinned to its own runtime version in the path form",
            runtime: JAVA_RUNTIME,
            offered: Some(JAVA_RUNTIME),
            form: ArnForm::Path,
            accepted: true,
        },
        Pin {
            case: "a java function pinned to a python runtime's version in the path form",
            runtime: JAVA_RUNTIME,
            offered: Some(PYTHON_RUNTIME),
            form: ArnForm::Path,
            accepted: false,
        },
        Pin {
            case: "a python function pinned to a version that is no runtime's",
            runtime: PYTHON_RUNTIME,
            offered: None,
            form: ArnForm::Reported,
            accepted: true,
        },
    ]
}

/// RULE: a `Manual` pin is refused unless the runtime version it names is a build of the
/// runtime the addressed version is configured with, and a refused pin stores nothing.
#[tokio::test]
async fn a_manual_pin_is_refused_unless_the_version_is_a_build_of_the_functions_runtime() {
    for (index, row) in pins().iter().enumerate() {
        let case = row.case;
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let router = router_on(&engine);
        let name = format!("runtime-pin-{index}");
        given_function(&router, case, &name, row.runtime).await;
        // The version OFFERED, written in the form the row asks for: the reported form is the
        // ARN this account reports for a function on that runtime, read back off a function of
        // its own rather than spelled here, and the path form names the runtime itself.
        let offered = match (row.offered, row.form) {
            (Some(runtime), ArnForm::Reported) => {
                let holder = format!("runtime-holder-{index}");
                given_function(&router, case, &holder, runtime).await;
                own_runtime_version_arn(&router, case, &holder).await
            }
            (Some(runtime), ArnForm::Path) => format!(
                "arn:aws:lambda:us-east-1:000000000000:runtime/{runtime}/version/a1b2c3d4e5"
            ),
            // An ARN of the first form whose id is no runtime's digest: nothing shows which
            // runtime it is a build of, so there is nothing to refuse the pin on.
            (None, _) => "arn:aws:lambda:us-east-1::runtime:8eeff65f6809a3ce81507fe733fe09b8\
                          35899b99481ba22fd75b5a7338290ec1"
                .to_string(),
        };

        let written = put_runtime_management(&router, &name, MANUAL, Some(&offered)).await;
        if row.accepted {
            assert_eq!(
                written.status,
                200,
                "case {case:?}: expected the pin to be stored, got {} {}",
                written.status,
                written.text()
            );
            let read = get_runtime_management(&router, &name).await;
            assert_eq!(
                (
                    read.string("UpdateRuntimeOn"),
                    read.string("RuntimeVersionArn")
                ),
                (Some(MANUAL.to_string()), Some(offered.clone())),
                "case {case:?}: expected the stored pin to be reported, got {}",
                read.text()
            );
            continue;
        }
        assert_eq!(
            written.status,
            400,
            "case {case:?}: expected the pin to be refused 400, got {} {}",
            written.status,
            written.text()
        );
        assert_eq!(
            written.error_code(),
            INVALID_PARAMETER,
            "case {case:?}: expected {INVALID_PARAMETER}, got {}",
            written.text()
        );
        let expected = format!(
            "Runtime version ARN ({offered}) is not compatible with your function's runtime \
             ({}).",
            row.runtime
        );
        assert_eq!(
            refusal_message(&written),
            Some(expected.clone()),
            "case {case:?}: expected the message {expected:?}, got {}",
            written.text()
        );
        // The refusal changes nothing: the version is still on the documented default and
        // carries no pin.
        let read = get_runtime_management(&router, &name).await;
        assert_eq!(
            (
                read.string("UpdateRuntimeOn"),
                read.string("RuntimeVersionArn")
            ),
            (Some(AUTO.to_string()), None),
            "case {case:?}: expected the refusal to store nothing, got {}",
            read.text()
        );
    }
}

/// A handler that returns at once: these rows read the ENVIRONMENT's own opening record, not
/// anything the handler wrote.
const PYTHON_QUIET: &str = "def lambda_handler(event, context):\n    return {\"ok\": True}\n";

/// The node handler of the same shape.
const NODE_QUIET: &str = "exports.handler = async () => ({ ok: true });\n";

/// One environment row: the runtime the function runs, the handler that runs on it, and the
/// `<family>:<line>.` the build of that runtime opens with — written here from the runtime
/// identifier, which names the family and the version line.
struct Environment {
    case: &'static str,
    runtime: &'static str,
    source: &'static str,
    opening: &'static str,
}

/// RULE: the record that opens a new execution environment's log names the BUILD of the
/// function's runtime — the family, the version line the identifier states and the build of
/// it — beside the ARN of that same runtime version.
#[tokio::test]
async fn a_cold_environment_names_the_runtime_build_it_initialised_on() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let rows = [
        Environment {
            case: "a python environment",
            runtime: PYTHON_RUNTIME,
            source: PYTHON_QUIET,
            opening: "python:3.12.",
        },
        Environment {
            case: "a node environment",
            runtime: NODE_RUNTIME,
            source: NODE_QUIET,
            opening: "nodejs:20.",
        },
    ];
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let name = format!("runtime-build-{index}");
        create_function_logging(
            &router,
            &Created {
                case,
                name: &name,
                source: row.source,
                runtime: row.runtime,
                role: ROLE,
                log_group: None,
            },
            None,
        )
        .await;
        let expected_arn = own_runtime_version_arn(&router, case, &name).await;
        // The environment's own record is written to the function's log group ahead of the
        // first invocation it serves, so it is read back out of that group.
        let (_tail, _payload) = invoke(&router, case, &name).await;
        let group = format!("/aws/lambda/{name}");
        let opened = initialization_record(&router, case, &group).await;

        let stated = opened
            .strip_prefix(INIT_START_PREFIX)
            .unwrap_or_else(|| panic!("case {case:?}: expected the opening record, got {opened:?}"));
        let (build, arn) = stated.split_once(INIT_START_ARN_FIELD).unwrap_or_else(|| {
            panic!("case {case:?}: expected the record to state the version's ARN, got {opened:?}")
        });
        assert!(
            build.starts_with(row.opening),
            "case {case:?}: expected a build of {:?} opening {:?}, got {build:?}",
            row.runtime,
            row.opening
        );
        let number = build.trim_start_matches(row.opening);
        assert!(
            number
                .strip_prefix('v')
                .is_some_and(|digits| !digits.is_empty()
                    && digits.chars().all(|digit| digit.is_ascii_digit())),
            "case {case:?}: expected the build of the line after {:?}, got {build:?}",
            row.opening
        );
        // The identifier itself is NOT what the record names: a client parsing the
        // `family:version` form could not read it.
        assert_ne!(
            build, row.runtime,
            "case {case:?}: expected the runtime build rather than the identifier, got {build:?}"
        );
        assert_eq!(
            arn, expected_arn,
            "case {case:?}: expected the ARN of the same version the function reports, got \
             {arn:?}"
        );
    }
}

/// The record that opens the one environment the group `group` holds a stream of, waited for:
/// the delivery of a run's log happens off the invocation's own answer.
async fn initialization_record(router: &MultiServiceRouter, case: &str, group: &str) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let streams = crate::invocation_log_delivery::streams(router, case, group).await;
        if let Some(stream) = streams.first() {
            let (records, _text) =
                crate::invocation_log_delivery::delivered(router, case, group, stream).await;
            if let Some(opened) = records
                .iter()
                .find(|record| record.starts_with(INIT_START_PREFIX))
            {
                return opened.clone();
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the environment's opening record never reached {group:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
