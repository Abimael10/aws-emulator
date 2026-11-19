// SPDX-License-Identifier: CC0-1.0

//! The window an accepted update of a function opens, from the two sides a client watching
//! `LastUpdateStatus` sees it from: which deployment package an invocation made inside the
//! window RUNS, and what the VPC attachment such an update installs READS BACK as.
//!
//! RULE 1: a function's deployment package takes over only when the update that deployed it
//! LEAVES the window — "When you update a function, Lambda provisions an instance of the
//! function and its supporting resources. [...] During this time, you can't modify the
//! function, but you can still invoke it" (`UpdateFunctionConfiguration`). So while
//! `LastUpdateStatus` is `InProgress`, whatever opened the window: an invocation is SERVED, and
//! what runs is the package the last settled update installed; a competing modification is
//! refused with the modelled `ResourceConflictException` (HTTP 409) naming the function's own
//! ARN, and that refusal changes neither the configuration nor the package that is serving;
//! and the package an accepted `UpdateFunctionCode` uploaded serves from the instant the clock
//! closes the window, not before.
//!
//! RULE 2: `VpcConfigResponse$VpcId` ("The ID of the VPC") is the VPC the configured subnets
//! belong to, so an update that applies a `VpcConfig` — whose request shape carries no `VpcId`
//! member at all — reports the owning VPC on EVERY read of the configuration it wrote: the
//! update's own answer, `GetFunctionConfiguration` and `GetFunction`, inside the window and
//! once it has settled. A function attached to nothing reports no such id.
//!
//! The rows differ along what the rules generalise over: which operation opened the window
//! (`UpdateFunctionCode`, and `UpdateFunctionConfiguration` applying each of three unrelated
//! settings), and, for the attachment, how many subnets the configuration names, which VPC
//! they belong to and whether it attaches the function at all.
//!
//! What this suite adds beside its neighbours: `in_flight_code_update_serving_package.rs`
//! tables the qualifier and the invoking operation of a window opened by `UpdateFunctionCode`,
//! and `function_update_window_conflict.rs` tables the modifications refused inside a window;
//! here the two halves meet in ONE window — the invocation, the refusal and the read — over
//! windows a CONFIGURATION change opened, and the attachment such a change installs is read
//! INSIDE its own window as well as after it, which is the span
//! `vpc_attachment_interfaces.rs` settles before reading. The networks Amazon EC2 mints, the
//! request walk and the clock helpers are the ones `vpc_attachment_interfaces.rs` and
//! `function_lifecycle.rs` already publish; what is written out here is written out because
//! those suites keep it private to themselves, and adding to one suite's surface from another
//! is what makes two concurrent fixes collide in one file.
//!
//! Every VPC, subnet and security group below is MINTED by Amazon EC2 itself over the
//! in-process callback, so the id `VpcId` must report is one the account holds and never one
//! this suite wrote into a request. Every invocation is a real HTTP request whose answer is
//! what the handler RETURNED, so a package this emulator reported but never ran fails here.
//! The clock-owed `Pending` -> `Active` and `InProgress` -> `Successful` moves are exercised by
//! moving the stored due instants into the past, so nothing here sleeps.

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::function_lifecycle::{
    function_arn, json_call, make_activation_due, make_update_due, Answer, ROLE,
};
use crate::synchronous_invoke_run::package;
use crate::vpc_attachment_interfaces::{ec2_client, network};

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The module the handler of every package below lives in.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The `LastUpdateStatus` of an update still being applied, from the model's
/// `LastUpdateStatus` enum (`Failed | InProgress | Successful`).
const IN_PROGRESS: &str = "InProgress";

/// The `LastUpdateStatus` of a settled update, from the same enum.
const SUCCESSFUL: &str = "Successful";

/// The marker of the package a function below is created with — the one that is serving when
/// a window opens.
const SERVING: &str = "settled-v1";

/// The marker of the package an accepted `UpdateFunctionCode` uploads, which may only serve
/// once the window that update opened has closed.
const DEPLOYED: &str = "deployed-v2";

/// The marker of the package a REFUSED upload inside the window carries, which must never
/// serve at all.
const REFUSED: &str = "refused-v3";

/// Lambda and the Amazon EC2 service that owns VPCs, subnets and security groups, on ONE
/// shared engine — what `core/router` assembles — plus an EC2 client over the router's own
/// in-process callback.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
    ec2: aws_sdk_ec2::Client,
}

fn emulator() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Pinned off so the suite does not depend on how the emulator process was started; these
    // tests assert the update window and the attachment, not the emulator's SigV4 check.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    let callback = EmbeddedEmulator::from_router(router.clone());
    Emulator {
        ec2: ec2_client(&callback),
        engine,
        router,
    }
}

/// A handler whose answer NAMES the deployment it was shipped in, so what an invocation
/// returns says which package ran.
fn source_naming(deployment: &str) -> String {
    format!("def lambda_handler(event, context):\n    return {{'deployment': '{deployment}'}}\n")
}

/// A `ZipFile` member holding a deployment package whose handler names `deployment`.
fn zip_naming(deployment: &str) -> Value {
    Value::String(
        base64::engine::general_purpose::STANDARD.encode(package(&source_naming(deployment))),
    )
}

/// Create one function whose handler names `deployment` and settle the provisioning window
/// the create opened, so what follows acts on an `Active` function.
async fn create_invokable(emulator: &Emulator, name: &str, deployment: &str) {
    let created = json_call(
        &emulator.router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": 5,
            "Code": { "ZipFile": zip_naming(deployment) },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateFunction {name:?} must succeed, got {} {}",
        created.status,
        text(&created)
    );
    make_activation_due(&emulator.engine);
}

/// `UpdateFunctionCode` shipping a handler that names `deployment`.
async fn update_code(router: &MultiServiceRouter, name: &str, deployment: &str) -> Answer {
    json_call(
        router,
        "PUT",
        &format!("{FUNCTIONS_URI}/{name}/code"),
        &json!({ "ZipFile": zip_naming(deployment) }),
    )
    .await
}

/// `UpdateFunctionConfiguration` on `name`, carrying `patch`.
async fn update_configuration(router: &MultiServiceRouter, name: &str, patch: &Value) -> Answer {
    json_call(
        router,
        "PUT",
        &format!("{FUNCTIONS_URI}/{name}/configuration"),
        patch,
    )
    .await
}

/// `GetFunctionConfiguration` on the unpublished version of `name`.
async fn configuration(router: &MultiServiceRouter, name: &str) -> Answer {
    json_call(
        router,
        "GET",
        &format!("{FUNCTIONS_URI}/{name}/configuration"),
        &Value::Null,
    )
    .await
}

/// `GetFunction` on `name`, whose `Configuration` member is the same shape.
async fn whole_function(router: &MultiServiceRouter, name: &str) -> Answer {
    json_call(router, "GET", &format!("{FUNCTIONS_URI}/{name}"), &Value::Null).await
}

/// The body text of an answer, for a failure message.
fn text(answer: &Answer) -> String {
    String::from_utf8_lossy(&answer.body).to_string()
}

/// The error message of a refusal, whichever casing the wire carries the member under.
fn message(answer: &Answer) -> String {
    answer
        .string("message")
        .or_else(|| answer.string("Message"))
        .unwrap_or_default()
}

/// Lambda's sentence for a modification refused because an update of the function is still
/// being applied: fixed text plus the function's own ARN, which the case builds from the
/// function it set up rather than from the answer it is checking.
fn in_progress_sentence(name: &str) -> String {
    format!(
        "The operation cannot be performed at this time. An update is in progress for \
         resource: {}",
        function_arn(name)
    )
}

/// Assert that `answer` is the modelled 409 for an update of `name` still in flight.
fn assert_in_flight_conflict(case: &str, answer: &Answer, name: &str) {
    assert_eq!(
        answer.status,
        409,
        "case {case:?}: expected 409 from the competing UpdateFunctionCode, got {} {}",
        answer.status,
        text(answer)
    );
    assert!(
        answer.error_code().contains("ResourceConflictException"),
        "case {case:?}: expected ResourceConflictException from the competing \
         UpdateFunctionCode, got {:?}",
        answer.error_code()
    );
    assert_eq!(
        message(answer),
        in_progress_sentence(name),
        "case {case:?}: expected the in-flight sentence naming the function's ARN, got {:?}",
        message(answer)
    );
}

/// Assert that `name` reports `expected` as its `LastUpdateStatus`.
async fn assert_last_update_status(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    expected: &str,
) {
    let answer = configuration(router, name).await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: expected 200 from GetFunctionConfiguration, got {} {}",
        answer.status,
        text(&answer)
    );
    assert_eq!(
        answer.string("LastUpdateStatus").as_deref(),
        Some(expected),
        "case {case:?}: expected LastUpdateStatus {expected:?}, got {:?}",
        answer.string("LastUpdateStatus")
    );
}

/// Invoke `name` and answer the `deployment` member of what the handler RETURNED, asserting
/// the invocation was served rather than refused — "you can still invoke it".
async fn invoked_deployment(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let answer = json_call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        &json!({}),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: the invocation of {name:?} must be served, got {} {}",
        answer.status,
        text(&answer)
    );
    assert_eq!(
        answer.header("x-amz-function-error"),
        None,
        "case {case:?}: the invocation of {name:?} must not report a function error, got {}",
        text(&answer)
    );
    let returned = answer.json();
    returned
        .get("deployment")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the handler to name its deployment, got {returned}")
        })
        .to_string()
}

/// Which operation opens the window a row watches. Each is an accepted update of the
/// function, so each opens the same window — what differs is the setting it installs.
#[derive(Debug, Clone, Copy)]
enum Opener {
    /// `UpdateFunctionCode` replacing the deployment package.
    Code,
    /// `UpdateFunctionConfiguration` attaching the function to a VPC.
    VpcAttach,
    /// `UpdateFunctionConfiguration` replacing the function's environment.
    Environment,
    /// `UpdateFunctionConfiguration` replacing the memory it runs with.
    Memory,
}

/// Open the window on `name` through `opener`, attaching to `subnet_ids` with `group_ids`
/// where the opener is the attachment.
async fn open_window(
    router: &MultiServiceRouter,
    opener: Opener,
    name: &str,
    subnet_ids: &[String],
    group_ids: &[String],
) -> Answer {
    match opener {
        Opener::Code => update_code(router, name, DEPLOYED).await,
        Opener::VpcAttach => {
            update_configuration(
                router,
                name,
                &json!({
                    "VpcConfig": {
                        "SubnetIds": subnet_ids,
                        "SecurityGroupIds": group_ids,
                    },
                }),
            )
            .await
        }
        Opener::Environment => {
            update_configuration(
                router,
                name,
                &json!({ "Environment": { "Variables": { "STAGE": "canary" } } }),
            )
            .await
        }
        Opener::Memory => update_configuration(router, name, &json!({ "MemorySize": 256 })).await,
    }
}

/// RULE 1: inside the window an accepted update opens, an invocation is SERVED and runs the
/// package the last settled update installed; a competing modification is refused with the
/// modelled conflict and changes nothing; the uploaded package serves once the clock closes
/// the window.
///
/// One row per operation that opens the window, with the package that must serve afterwards.
#[tokio::test]
async fn an_invocation_inside_any_update_window_runs_the_package_that_was_already_serving() {
    // (case, the operation that opens the window, the package that serves once it closes)
    let rows: &[(&str, Opener, &str)] = &[
        ("a code update", Opener::Code, DEPLOYED),
        ("a VPC attachment", Opener::VpcAttach, SERVING),
        ("an environment change", Opener::Environment, SERVING),
        ("a memory change", Opener::Memory, SERVING),
    ];

    for (index, (case, opener, after)) in rows.iter().enumerate() {
        // One function per row, so the window a row opens is the only one in flight.
        let emulator = emulator();
        // A network is minted only for the row whose opener attaches the function to one.
        let held = match opener {
            Opener::VpcAttach => {
                let prefix = u8::try_from(40 + index).expect("one CIDR prefix per row");
                Some(network(&emulator.ec2, case, prefix, 2).await)
            }
            Opener::Code | Opener::Environment | Opener::Memory => None,
        };
        let (subnet_ids, group_ids) = match &held {
            Some(held) => (held.subnet_ids.clone(), vec![held.group_id.clone()]),
            None => (Vec::new(), Vec::new()),
        };
        let name = format!("update-window-{index}");
        create_invokable(&emulator, &name, SERVING).await;

        let before = invoked_deployment(&emulator.router, case, &name).await;
        assert_eq!(
            before, SERVING,
            "case {case:?}: before any update the function must run its created package, got \
             {before:?}"
        );

        let opened = open_window(&emulator.router, *opener, &name, &subnet_ids, &group_ids).await;
        assert_eq!(
            opened.status,
            200,
            "case {case:?}: the update that opens the window must be accepted, got {} {}",
            opened.status,
            text(&opened)
        );
        assert_last_update_status(&emulator.router, case, &name, IN_PROGRESS).await;

        let inside = invoked_deployment(&emulator.router, case, &name).await;
        assert_eq!(
            inside, SERVING,
            "case {case:?}: inside the window the invocation must run the package that was \
             already serving, got {inside:?}"
        );

        // "During this time, you can't modify the function": the competing upload is refused,
        // and what it carried never serves — before or after the window closes.
        let refused = update_code(&emulator.router, &name, REFUSED).await;
        assert_in_flight_conflict(case, &refused, &name);
        let still = invoked_deployment(&emulator.router, case, &name).await;
        assert_eq!(
            still, SERVING,
            "case {case:?}: the refused upload must leave the serving package alone, got \
             {still:?}"
        );

        make_update_due(&emulator.engine);
        assert_last_update_status(&emulator.router, case, &name, SUCCESSFUL).await;
        let settled = invoked_deployment(&emulator.router, case, &name).await;
        assert_eq!(
            settled, *after,
            "case {case:?}: once the window has closed the invocation must run {after:?}, got \
             {settled:?}"
        );
    }
}

/// The `VpcConfig` a `FunctionConfiguration` body reports, or `Value::Null` when it reports
/// none.
fn reported_vpc_config(body: &Value) -> Value {
    body.get("VpcConfig").cloned().unwrap_or(Value::Null)
}

/// Assert that the body `label` names reports the attachment the request asked for, the id of
/// the VPC its subnets belong to included.
fn assert_reports_attachment(
    case: &str,
    label: &str,
    body: &Value,
    subnet_ids: &[String],
    group_ids: &[String],
    expected_vpc: Option<&str>,
) {
    let reported = reported_vpc_config(body);
    assert_eq!(
        reported.get("VpcId").and_then(Value::as_str),
        expected_vpc,
        "case {case:?}: expected {label} to report VpcId {expected_vpc:?}, got {reported:?}"
    );
    assert_eq!(
        reported.get("SubnetIds").cloned(),
        Some(json!(subnet_ids)),
        "case {case:?}: expected {label} to report the subnets {subnet_ids:?}, got {reported:?}"
    );
    assert_eq!(
        reported.get("SecurityGroupIds").cloned(),
        Some(json!(group_ids)),
        "case {case:?}: expected {label} to report the groups {group_ids:?}, got {reported:?}"
    );
}

/// One row of the attachment table: the case, the subnets the configuration names, the groups
/// it names, and the VPC every read must then report.
type AttachmentRow = (&'static str, Vec<String>, Vec<String>, Option<String>);

/// RULE 2: the VPC an attachment resolves to is reported as `VpcConfigResponse$VpcId` on every
/// read of the configuration the update wrote — its own answer, `GetFunctionConfiguration` and
/// `GetFunction` — inside the window it opened and once that window has closed, and a function
/// attached to nothing reports no such id.
#[tokio::test]
async fn the_vpc_an_attachment_resolves_to_is_reported_on_every_read_of_the_configuration() {
    let emulator = emulator();
    let first = network(&emulator.ec2, "the first VPC", 60, 2).await;
    let second = network(&emulator.ec2, "the second VPC", 61, 1).await;
    let name = "vpc-attached";
    create_invokable(&emulator, name, SERVING).await;

    // Before any attachment the configuration names no VPC at all, so no read reports one.
    let before = configuration(&emulator.router, name).await;
    assert_eq!(
        reported_vpc_config(&before.json()).get("VpcId"),
        None,
        "an unattached function must report no VpcId, got {}",
        text(&before)
    );

    let rows: Vec<AttachmentRow> = vec![
        (
            "one subnet of the first VPC",
            first.subnet_ids[..1].to_vec(),
            vec![first.group_id.clone()],
            Some(first.vpc_id.clone()),
        ),
        (
            "both subnets of the first VPC",
            first.subnet_ids.clone(),
            vec![first.group_id.clone()],
            Some(first.vpc_id.clone()),
        ),
        (
            "a subnet of another VPC, which moves the attachment",
            second.subnet_ids.clone(),
            vec![second.group_id.clone()],
            Some(second.vpc_id.clone()),
        ),
        (
            "empty lists, which remove the function's VPC configuration",
            Vec::new(),
            Vec::new(),
            None,
        ),
    ];

    for (case, subnet_ids, group_ids, expected_vpc) in rows {
        // Whatever the previous row left in flight is settled first: "you can't modify the
        // function" while an update is in progress.
        make_update_due(&emulator.engine);
        let updated = update_configuration(
            &emulator.router,
            name,
            &json!({
                "VpcConfig": { "SubnetIds": subnet_ids, "SecurityGroupIds": group_ids },
            }),
        )
        .await;
        assert_eq!(
            updated.status,
            200,
            "case {case:?}: expected 200 from UpdateFunctionConfiguration, got {} {}",
            updated.status,
            text(&updated)
        );

        // Inside the window the update opened: its own answer, and the reader beside it.
        assert_last_update_status(&emulator.router, case, name, IN_PROGRESS).await;
        assert_reports_attachment(
            case,
            "the UpdateFunctionConfiguration answer",
            &updated.json(),
            &subnet_ids,
            &group_ids,
            expected_vpc.as_deref(),
        );
        let reading = configuration(&emulator.router, name).await;
        assert_reports_attachment(
            case,
            "GetFunctionConfiguration inside the window",
            &reading.json(),
            &subnet_ids,
            &group_ids,
            expected_vpc.as_deref(),
        );
        let inside = invoked_deployment(&emulator.router, case, name).await;
        assert_eq!(
            inside, SERVING,
            "case {case:?}: the attach window must leave the function invokable, got {inside:?}"
        );

        // ... and once the clock has closed it, both readers still report the same attachment.
        make_update_due(&emulator.engine);
        assert_last_update_status(&emulator.router, case, name, SUCCESSFUL).await;
        let settled = configuration(&emulator.router, name).await;
        assert_reports_attachment(
            case,
            "GetFunctionConfiguration once the window has settled",
            &settled.json(),
            &subnet_ids,
            &group_ids,
            expected_vpc.as_deref(),
        );
        let whole = whole_function(&emulator.router, name).await;
        assert_reports_attachment(
            case,
            "GetFunction once the window has settled",
            whole.json().get("Configuration").unwrap_or(&Value::Null),
            &subnet_ids,
            &group_ids,
            expected_vpc.as_deref(),
        );
    }
}
