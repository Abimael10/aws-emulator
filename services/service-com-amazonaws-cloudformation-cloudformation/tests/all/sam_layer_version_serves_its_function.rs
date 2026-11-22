// SPDX-License-Identifier: CC0-1.0

//! The layer an `AWS::Serverless::LayerVersion` declares, from the archive a client staged to
//! the handler that imports it — and what the stack's deletion leaves of it.
//!
//! The rule every row here holds the emulator to: **a declared layer version is published in
//! AWS Lambda out of the archive its content names, and a function that references it carries
//! that version in its execution environment, so the handler resolves what the archive ships
//! from the import directories of the layer root (`/opt`) and reads its files there; and the
//! generated version's own `DeletionPolicy` decides what the stack's deletion does with it,
//! so a RETAINED version is still one AWS Lambda holds — `GetLayerVersion` resolves it —
//! after the stack that published it is gone, while the function goes with the stack either
//! way.** The halves are one rule about one resource: a layer exists to serve the function
//! that references it, and `DeletionPolicy` is the attribute that decides whether a resource
//! is preserved or deleted when its stack is deleted, which for a layer version is what lets
//! another stack keep referencing it.
//!
//! The rows differ along what the rule generalises over: WHICH import directory of the layer
//! root the archive ships its module in (the version-independent `python/`, and the
//! site-packages directory of the function's own runtime version), which `python3.x` runtime
//! the declaration states, the module the handler imports and the content it ships, and what
//! the declaration says about retaining the version (nothing at all, `Retain`, `Delete`) —
//! the last being what decides whether the version outlives the stack. Each row is driven by
//! a test of its own, so one row's deploy-invoke-delete chain is not another's wait.
//!
//! Siblings reach one side each, and this suite is maintained with them:
//! [`crate::sam_layer_version_references`] pins the logical id the expansion gives the
//! declaration, the references it re-points at it, and the `DeletionPolicy` the processed
//! template states — and owns the `GetLayerVersion` call and the version-ARN derivation both
//! suites read a published version through. [`crate::lambda_layer_version_reference`] pins
//! what a reference to a HAND-WRITTEN `AWS::Lambda::LayerVersion` resolves to. What only this
//! suite reaches is the archive's CONTENT travelling all the way into a run of the function: a
//! version published with none of the staged bytes behind it, or a function that carries the
//! ARN without the mount, passes both of those and fails here.
//!
//! Every expected value is computed here from what the row staged and declared; nothing is
//! read back out of the answer being checked.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::settle_the_store;
use crate::dynamodb_table_resources::failure_reasons;
use crate::hotswapped_function_code::{lambda, package};
// The three readings a suite of this target takes of a function and a layer a stack deployed,
// each owned by the suite that first needed it: there is one spelling of each call here.
use crate::cdk_asset_function_writes_its_granted_table::wait_until_active;
use crate::sam_layer_version_references::{get_layer_version, layer_version_arn};
use crate::stack_lifecycle::{
    call, create_stack, member, output_value, router_with_engine, xml_text,
};
use crate::stack_resource_provisioning::s3;

/// The logical ids every template below declares, and the output keys it publishes the
/// deployed identifiers under.
const LAYER_LOGICAL_ID: &str = "Shared";
const FUNCTION_LOGICAL_ID: &str = "Worker";
const LAYER_OUTPUT: &str = "LayerVersionArn";
const FUNCTION_OUTPUT: &str = "FunctionName";

/// The bucket a client stages the archives in before it submits the template, the way
/// `sam package` uploads them.
const STAGING_BUCKET: &str = "sam-layer-packages";

/// The statuses the rule is stated about: the one a completed create reports, and the one a
/// completed delete reports.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const DELETE_COMPLETE: &str = "DELETE_COMPLETE";

/// The `State` AWS Lambda takes calls on: "you cannot invoke … while the function is in the
/// Pending state".
const STATE_ACTIVE: &str = "Active";

/// The module the function's own deployment package carries, and the handler in it the
/// declaration names — the layer is what the module it imports comes from.
const SOURCE_FILE: &str = "index.py";
const HANDLER: &str = "index.handler";

/// How long one invocation is given, as the function's own `Timeout`, in seconds: far past
/// what a run of this handler needs on any host, because nothing here is stated about how
/// fast an interpreter gets through one.
const FUNCTION_TIMEOUT: u64 = 30;

/// How long between two readings of a stack that is settling, and how many readings before
/// the row is a failure. Every transition this store owes is pulled into the past before each
/// reading, so the loop waits for the CONDITION; the budget is generous so a loaded host
/// cannot turn the wait itself into the thing under test.
const LOOK_AGAIN: Duration = Duration::from_millis(25);
const LOOKS: usize = 2_400;

/// The root a function's layers are extracted to: "Lambda extracts the layer contents into
/// the `/opt` directory", which is where the handler reports resolving the module from.
const LAYER_ROOT: &str = "/opt";

/// The handler every row runs: it imports the module the LAYER ships at module scope — the
/// idiom a layer exists for, and the one that fails outright when the layer is not mounted —
/// and reports what it resolved to, where from, and how much of it reads back through the
/// filesystem of the execution environment.
const HANDLER_SOURCE: &str = r#"
import LAYER_MODULE as shipped


def handler(event, context):
    with open(shipped.__file__) as reading:
        source = reading.read()
    return {
        "value": shipped.VALUE,
        "file": shipped.__file__,
        "bytes": len(source),
    }
"#;

/// One row: the layer one stack declares, the function that references it, and what the
/// declaration asks be left of the version once the stack is gone.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The `LayerName` the declaration states, and so the layer `GetLayerVersion` addresses.
    layer: &'static str,
    /// The `python3.x` runtime both the function and the layer declare.
    runtime: &'static str,
    /// The module the handler imports, and the path the archive ships it under — a directory
    /// of the layer root that is on the runtime's own import path.
    module: &'static str,
    entry: &'static str,
    /// What that module defines, which is what the handler must report.
    value: &'static str,
    /// The `RetentionPolicy` the declaration states, or `None` for the documented default.
    retention: Option<&'static str>,
    /// Whether AWS Lambda still holds the version once the stack has been deleted.
    outlives_the_stack: bool,
}

/// The source the row's layer archive ships: one module defining the value the handler
/// reports.
fn module_source(row: &Row) -> String {
    format!("VALUE = \"{}\"\n", row.value)
}

/// The capabilities a template declaring the SAM macro — and the execution role the macro
/// synthesizes for a function that declares none — has to be submitted with.
fn capabilities() -> Vec<(String, String)> {
    vec![
        member("Capabilities.member.1", "CAPABILITY_IAM"),
        member("Capabilities.member.2", "CAPABILITY_AUTO_EXPAND"),
    ]
}

/// The keys one row's archives are staged under.
fn layer_key(row: &Row) -> String {
    format!("{}/layer.zip", row.stack)
}

fn code_key(row: &Row) -> String {
    format!("{}/function.zip", row.stack)
}

/// Put one object into the staging bucket, creating the bucket first: AWS Lambda READS the
/// archive out of the object the expanded `Content`/`Code` names, so the object has to be
/// there before the template is submitted.
async fn stage(router: &MultiServiceRouter, case: &str, key: &str, bytes: &[u8]) {
    let created = s3(router, "PUT", &format!("/{STAGING_BUCKET}"), b"").await;
    assert!(
        (200..300).contains(&created.status),
        "case {case:?}: the staging bucket must be created first, got {} {}",
        created.status,
        created.body
    );
    let staged = s3(router, "PUT", &format!("/{STAGING_BUCKET}/{key}"), bytes).await;
    assert!(
        (200..300).contains(&staged.status),
        "case {case:?}: the archive {key} must be staged first, got {} {}",
        staged.status,
        staged.body
    );
}

/// The template one row submits: the layer, the function whose `Layers` references it, and
/// the outputs a client reads the two deployed identifiers from.
fn template(row: &Row) -> String {
    let mut layer_properties = json!({
        "LayerName": row.layer,
        "ContentUri": format!("s3://{STAGING_BUCKET}/{}", layer_key(row)),
        "CompatibleRuntimes": [row.runtime],
    });
    if let Some(policy) = row.retention {
        layer_properties["RetentionPolicy"] = json!(policy);
    }
    json!({
        "Transform": "AWS::Serverless-2016-10-31",
        "Resources": {
            LAYER_LOGICAL_ID: {
                "Type": "AWS::Serverless::LayerVersion",
                "Properties": layer_properties,
            },
            FUNCTION_LOGICAL_ID: {
                "Type": "AWS::Serverless::Function",
                "Properties": {
                    "Runtime": row.runtime,
                    "Handler": HANDLER,
                    "Timeout": FUNCTION_TIMEOUT,
                    "CodeUri": format!("s3://{STAGING_BUCKET}/{}", code_key(row)),
                    "Layers": [{ "Ref": LAYER_LOGICAL_ID }],
                },
            },
        },
        "Outputs": {
            LAYER_OUTPUT: { "Value": { "Ref": LAYER_LOGICAL_ID } },
            FUNCTION_OUTPUT: { "Value": { "Ref": FUNCTION_LOGICAL_ID } },
        },
    })
    .to_string()
}

/// The stack's body once the operation in flight on it is no longer in progress.
///
/// Every transition this store owes is pulled into the past and an ordinary read materialises
/// it, so the loop waits for the CONDITION — a settled status — rather than for a span of the
/// clock.
///
/// `pub(crate)` because the sibling suite that drives a RETAINED version into a later stack's
/// function (`retained_layer_version_serves_a_later_stack.rs`) waits for each of its stacks
/// through this one spelling of the wait rather than a copy of it.
pub(crate) async fn settled(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    stack: &str,
) -> (String, String) {
    let mut last = (String::new(), String::new());
    for _ in 0..LOOKS {
        settle_the_store(router, engine).await;
        let (status, body) = call(router, "DescribeStacks", &[member("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DescribeStacks({stack:?}) to answer 200, got {status} {body}"
        );
        let reported = xml_text(&body, "StackStatus").unwrap_or_default();
        let settled = !reported.ends_with("_IN_PROGRESS");
        last = (reported, body);
        if settled {
            return last;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    last
}

/// `GetFunctionConfiguration` for one function.
async fn get_function_configuration(router: &MultiServiceRouter, function: &str) -> (u16, Value) {
    let answered = lambda(
        router,
        "GET",
        &format!("/2015-03-31/functions/{function}/configuration"),
        &[],
        &[],
    )
    .await;
    (answered.status, answered.body)
}

/// The `(Arn, CodeSize)` of every layer a function's configuration reports.
fn carried_layers(configuration: &Value) -> Vec<(String, i64)> {
    configuration
        .get("Layers")
        .and_then(Value::as_array)
        .map(|layers| {
            layers
                .iter()
                .map(|layer| {
                    (
                        layer
                            .get("Arn")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        layer.get("CodeSize").and_then(Value::as_i64).unwrap_or(-1),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One row, end to end: stage, deploy, read the published version, invoke the function it
/// serves, then delete the stack and read what its retention left.
async fn drive(row: &Row) {
    let case = row.case;
    let (router, engine) = router_with_engine();

    // What a client stages before it submits the template: the layer's archive, and the
    // function's own package whose handler imports what that archive ships.
    let source = module_source(row);
    let archive = package(row.entry, &source);
    stage(&router, case, &layer_key(row), &archive).await;
    stage(
        &router,
        case,
        &code_key(row),
        &package(SOURCE_FILE, &HANDLER_SOURCE.replace("LAYER_MODULE", row.module)),
    )
    .await;

    let (status, body) = create_stack(&router, row.stack, &template(row), &capabilities()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} {body}"
    );
    // The stack's own id, which stays addressable once the stack has been deleted — the name
    // does not have to.
    let stack_id = xml_text(&body, "StackId").unwrap_or_default();
    assert!(
        !stack_id.is_empty(),
        "case {case:?}: expected CreateStack to answer the stack's id, got {body}"
    );
    let (reported, described) = settled(&router, &engine, case, row.stack).await;
    assert_eq!(
        reported, CREATE_COMPLETE,
        "case {case:?}: expected the stack to deploy the expanded template, got {reported:?} \
         because {:?}",
        failure_reasons(&router, row.stack).await
    );

    let expected_arn = layer_version_arn(row.layer, 1);
    let published_arn = output_value(&described, LAYER_OUTPUT).unwrap_or_default();
    assert_eq!(
        published_arn, expected_arn,
        "case {case:?}: expected the stack to publish the version {expected_arn:?}, got \
         {published_arn:?} in {described}"
    );
    let function = output_value(&described, FUNCTION_OUTPUT).unwrap_or_default();
    assert!(
        !function.is_empty(),
        "case {case:?}: expected the stack to publish the function it deployed, got {described}"
    );

    // The version AWS Lambda holds is the one the staged archive was published as: its size is
    // that object's own byte count, and it carries what the declaration stated.
    let (status, held) = get_layer_version(&router, row.layer, 1).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected AWS Lambda to hold the published version, got {status} {held}"
    );
    assert_eq!(
        held.get("LayerVersionArn").and_then(Value::as_str),
        Some(expected_arn.as_str()),
        "case {case:?}: expected the version {expected_arn:?}, got {held}"
    );
    assert_eq!(
        held.pointer("/Content/CodeSize").and_then(Value::as_i64),
        Some(archive.len() as i64),
        "case {case:?}: expected the published content to be the staged archive's {} bytes, got \
         {held}",
        archive.len()
    );
    assert_eq!(
        held.get("CompatibleRuntimes").and_then(Value::as_array),
        Some(&vec![json!(row.runtime)]),
        "case {case:?}: expected the declared compatible runtimes, got {held}"
    );

    // ...and the function the same template declares really carries it.
    let state = wait_until_active(&router, case, &function).await;
    assert_eq!(
        state, STATE_ACTIVE,
        "case {case:?}: expected AWS Lambda to report {function} {STATE_ACTIVE} before an \
         invocation, got {state:?}"
    );
    let (status, configuration) = get_function_configuration(&router, &function).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected AWS Lambda to hold the function {function:?}, got {status} \
         {configuration}"
    );
    assert_eq!(
        carried_layers(&configuration),
        vec![(expected_arn.clone(), archive.len() as i64)],
        "case {case:?}: expected the function to carry the published version, got {configuration}"
    );

    // The run itself: the handler resolves the module the archive ships, under the layer root,
    // and reads the file back out of the environment it was mounted into.
    let answered = lambda(
        &router,
        "POST",
        &format!("/2015-03-31/functions/{function}/invocations"),
        b"{}",
        &[("x-amz-invocation-type", "RequestResponse")],
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} with payload {}",
        answered.status, answered.body
    );
    assert_eq!(
        answered.function_error(),
        None,
        "case {case:?}: expected the handler to run to completion, got the error document {}",
        answered.body
    );
    let expected = json!({
        "value": row.value,
        "file": format!("{LAYER_ROOT}/{}", row.entry),
        "bytes": source.len(),
    });
    assert_eq!(
        answered.body, expected,
        "case {case:?}: expected the handler's own result {expected}, got {}",
        answered.body
    );

    // The deletion: the function goes with the stack, and the version's own deletion policy
    // decides whether AWS Lambda still holds it.
    let (status, body) = call(&router, "DeleteStack", &[member("StackName", row.stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} {body}"
    );
    let (reported, described) = settled(&router, &engine, case, &stack_id).await;
    assert_eq!(
        reported, DELETE_COMPLETE,
        "case {case:?}: expected the stack to be deleted, got {reported:?} in {described}"
    );

    let (status, gone) = get_function_configuration(&router, &function).await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected the deleted stack's function to be gone, got {status} {gone}"
    );

    let (status, after) = get_layer_version(&router, row.layer, 1).await;
    let expected_status = if row.outlives_the_stack { 200 } else { 404 };
    assert_eq!(
        status, expected_status,
        "case {case:?}: expected AWS Lambda to answer {expected_status} for the version after \
         the stack was deleted, got {status} {after}"
    );
    if row.outlives_the_stack {
        assert_eq!(
            after.get("LayerVersionArn").and_then(Value::as_str),
            Some(expected_arn.as_str()),
            "case {case:?}: expected the retained version {expected_arn:?} to read back \
             unchanged, got {after}"
        );
    }
}

/// The recorded shape: a layer shipping its module in the version-independent import
/// directory, with nothing said about retaining it — so the generated version is retained and
/// outlives the stack.
#[tokio::test]
async fn a_layer_retained_by_default_serves_its_function_and_outlives_the_stack() {
    drive(&Row {
        case: "a module in the version-independent import directory, retained by default",
        stack: "sam-layer-serves-default",
        layer: "serves-default-deps",
        runtime: "python3.12",
        module: "shared",
        entry: "python/shared.py",
        value: "from the layer the stack published",
        retention: None,
        outlives_the_stack: true,
    })
    .await;
}

/// The same rule where the archive ships the module in the site-packages directory of the
/// function's OWN runtime version, on another runtime, with the retention stated rather than
/// defaulted.
#[tokio::test]
async fn a_layer_retained_by_declaration_serves_its_function_from_its_site_packages() {
    drive(&Row {
        case: "a module in this runtime version's site-packages, retained by declaration",
        stack: "sam-layer-serves-site-packages",
        layer: "serves-site-packages-deps",
        runtime: "python3.11",
        module: "vendored",
        entry: "python/lib/python3.11/site-packages/vendored.py",
        value: "vendored beside the runtime's own packages",
        retention: Some("Retain"),
        outlives_the_stack: true,
    })
    .await;
}

/// The other value the retention policy takes: the version is the stack's to remove, so the
/// deletion takes it with the function it served.
#[tokio::test]
async fn a_layer_the_declaration_deletes_goes_with_its_stack() {
    drive(&Row {
        case: "a layer the declaration asks be deleted with the stack",
        stack: "sam-layer-serves-deleted",
        layer: "serves-deleted-deps",
        runtime: "python3.13",
        module: "helper",
        entry: "python/helper.py",
        value: "deleted with the stack that published it",
        retention: Some("Delete"),
        outlives_the_stack: false,
    })
    .await;
}
