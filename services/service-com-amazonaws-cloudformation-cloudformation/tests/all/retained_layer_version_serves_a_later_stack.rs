// SPDX-License-Identifier: CC0-1.0

//! A layer version a stack published, in the execution environment of the functions that
//! reference it — the one its own stack declared, and the one a LATER stack declares once the
//! publishing stack is gone.
//!
//! The rule every row here holds the emulator to: **every layer version a function's `Layers`
//! names is extracted into that function's layer root (`/opt`) before the handler loads, so a
//! module the archive ships under one of the root's import directories is importable from the
//! handler and its file is readable there; and a published layer version is a resource of AWS
//! Lambda in its own right, so a version the publishing stack retained goes on resolving — and
//! goes on serving functions — after that stack has been deleted.** The two halves are one
//! rule about one resource: the archive's bytes belong to the layer version rather than to the
//! stack that published it, which is why another stack's function can mount them.
//!
//! The rows differ along what the rule generalises over: which import directory of the layer
//! root the archive ships its module in (the version-independent `python/`, and the
//! site-packages directory of the function's own runtime version), which `python3.x` runtime
//! the function and the layer declare, and HOW MANY layers the later function mounts — the
//! retained version alone, and the retained version beside one the later stack publishes
//! itself. Each row is a test of its own, so one row's deploy-invoke-delete-deploy chain is
//! not another's wait.
//!
//! What this suite reaches that its siblings do not: the layer declared by a HAND-WRITTEN
//! `AWS::Lambda::LayerVersion` serving a run (`crate::lambda_layer_version_reference` pins
//! only that the function carries the ARN the publish answered), and a retained version
//! serving a function of ANOTHER stack after its own stack is gone
//! (`crate::sam_layer_version_serves_its_function` pins the macro-expanded declaration and the
//! version's survival, never a second stack mounting it). A version published without the
//! staged bytes behind it, or a function that carries the ARN without the mount, passes both
//! of those and fails here.
//!
//! Every expected value is computed here from what the row staged and declared; nothing is
//! read back out of the answer being checked.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cdk_asset_function_writes_its_granted_table::wait_until_active;
use crate::dynamodb_table_resources::failure_reasons;
use crate::hotswapped_function_code::{lambda, package};
use crate::sam_layer_version_serves_its_function::settled;
use crate::sam_layer_version_references::{get_layer_version, layer_version_arn};
use crate::stack_lifecycle::{
    call, create_stack, member, output_value, router_with_engine, xml_text,
};
use crate::stack_resource_provisioning::s3;

/// The bucket a client stages the archives in before it submits a template, the way
/// `aws cloudformation package` uploads them.
const STAGING_BUCKET: &str = "retained-layer-packages";

/// The logical ids every template below declares, and the output keys it publishes the
/// deployed identifiers under.
const LAYER_LOGICAL_ID: &str = "Layer";
const BESIDE_LOGICAL_ID: &str = "Beside";
const FUNCTION_LOGICAL_ID: &str = "Worker";
const LAYER_OUTPUT: &str = "LayerVersionArn";
const FUNCTION_OUTPUT: &str = "FunctionName";

/// The statuses the rule is stated about: the one a completed create reports, and the one a
/// completed delete reports.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const DELETE_COMPLETE: &str = "DELETE_COMPLETE";

/// The `State` AWS Lambda takes calls on: "you cannot invoke … while the function is in the
/// Pending state".
const STATE_ACTIVE: &str = "Active";

/// The one source file each function's own deployment package carries, and the handler in it
/// every declaration names — the layers are where the modules it imports come from.
const SOURCE_FILE: &str = "index.py";
const HANDLER: &str = "index.handler";

/// How long one invocation is given, as the function's own `Timeout`, in seconds: far past
/// what a run of these handlers needs on any host, because nothing here is stated about how
/// fast an interpreter gets through one.
const FUNCTION_TIMEOUT: u64 = 30;

/// The root a function's layers are extracted to: "Lambda extracts the layer contents into the
/// `/opt` directory", which is where the handler reports resolving each module from.
const LAYER_ROOT: &str = "/opt";

/// One module a layer archive ships: the name the handler imports, the path inside the archive,
/// and the value the module defines.
struct Shipped {
    module: &'static str,
    /// A directory of the layer root that is on the runtime's own import path.
    entry: &'static str,
    value: &'static str,
}

impl Shipped {
    /// The source the archive ships for this module, which is both what the handler imports
    /// and what it reads back through the filesystem of the execution environment.
    fn source(&self) -> String {
        format!("VALUE = \"{}\"\n", self.value)
    }

    /// Where the handler resolves the module from once the layer is mounted.
    fn mounted_at(&self) -> String {
        format!("{LAYER_ROOT}/{}", self.entry)
    }
}

/// One row: the layer one stack publishes and retains, the layer a later stack publishes of its
/// own, and the `python3.x` runtime both stacks' functions declare.
struct Row {
    case: &'static str,
    /// The stack that publishes the retained version, and the one that mounts it afterwards.
    publisher: &'static str,
    consumer: &'static str,
    /// The `LayerName` the publishing declaration states, and so the layer `GetLayerVersion`
    /// addresses.
    layer: &'static str,
    runtime: &'static str,
    /// What the retained layer's archive ships.
    retained: Shipped,
    /// The layer the CONSUMING stack publishes of its own, mounted beside the retained one, or
    /// `None` where the later function mounts the retained version alone.
    beside: Option<(&'static str, Shipped)>,
}

/// The capabilities a template declaring an IAM role has to be submitted with.
fn capabilities() -> Vec<(String, String)> {
    vec![member("Capabilities.member.1", "CAPABILITY_IAM")]
}

/// The execution role every template here declares, and what the function's `Role` resolves to.
const ROLE: &str = r#""Role":{"Type":"AWS::IAM::Role","Properties":{
     "AssumeRolePolicyDocument":{"Version":"2012-10-17","Statement":[{"Effect":"Allow",
       "Principal":{"Service":"lambda.amazonaws.com"},"Action":"sts:AssumeRole"}]}}}"#;

/// The keys one row's archives are staged under: one layer archive and one deployment package
/// per stack, because the two stacks' handlers import different module sets.
fn layer_key(stack: &str) -> String {
    format!("{stack}/layer.zip")
}

fn code_key(stack: &str) -> String {
    format!("{stack}/function.zip")
}

/// Put one object into the staging bucket, creating the bucket first: AWS Lambda READS the
/// archive out of the object a `Content`/`Code` names, so the object has to be there before the
/// template is submitted.
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

/// The handler every function here runs: it imports each module its LAYERS ship at module scope
/// — the idiom a layer exists for, and the one that fails outright when a layer is not mounted —
/// and reports what each resolved to, where from, and the file's own text read back through the
/// filesystem of the execution environment.
fn handler_source(shipped: &[&Shipped]) -> String {
    let imports: String = shipped
        .iter()
        .map(|module| format!("import {}\n", module.module))
        .collect();
    let reports: String = shipped
        .iter()
        .map(|module| {
            let name = module.module;
            format!(
                "    found[\"{name}\"] = {{\"value\": {name}.VALUE, \"file\": {name}.__file__, \
                 \"text\": _read({name}.__file__)}}\n"
            )
        })
        .collect();
    format!(
        "{imports}\n\ndef _read(path):\n    with open(path) as reading:\n        return \
         reading.read()\n\n\ndef handler(event, context):\n    found = {{}}\n{reports}    return \
         found\n"
    )
}

/// What the handler must report for the modules its layers ship, computed from what the row
/// staged.
fn expected_report(shipped: &[&Shipped]) -> Value {
    let mut found = serde_json::Map::new();
    for module in shipped {
        found.insert(
            module.module.to_string(),
            json!({
                "value": module.value,
                "file": module.mounted_at(),
                "text": module.source(),
            }),
        );
    }
    Value::Object(found)
}

/// The template the publishing stack submits: the layer it retains past its own deletion, and a
/// function of its own that mounts it.
fn publisher_template(row: &Row) -> String {
    format!(
        r#"{{"Resources":{{
             "{LAYER_LOGICAL_ID}":{{"Type":"AWS::Lambda::LayerVersion","DeletionPolicy":"Retain",
               "Properties":{{
               "LayerName":"{layer}",
               "Content":{{"S3Bucket":"{STAGING_BUCKET}","S3Key":"{layer_key}"}},
               "CompatibleRuntimes":["{runtime}"]}}}},
             {ROLE},
             "{FUNCTION_LOGICAL_ID}":{{"Type":"AWS::Lambda::Function","Properties":{{
               "FunctionName":"{publisher}-worker","Handler":"{HANDLER}",
               "Runtime":"{runtime}","Timeout":{FUNCTION_TIMEOUT},
               "Role":{{"Fn::GetAtt":["Role","Arn"]}},
               "Code":{{"S3Bucket":"{STAGING_BUCKET}","S3Key":"{code_key}"}},
               "Layers":[{{"Ref":"{LAYER_LOGICAL_ID}"}}]}}}}}},
           "Outputs":{{
             "{LAYER_OUTPUT}":{{"Value":{{"Ref":"{LAYER_LOGICAL_ID}"}}}},
             "{FUNCTION_OUTPUT}":{{"Value":{{"Ref":"{FUNCTION_LOGICAL_ID}"}}}}}}}}"#,
        layer = row.layer,
        runtime = row.runtime,
        publisher = row.publisher,
        layer_key = layer_key(row.publisher),
        code_key = code_key(row.publisher),
    )
}

/// The template the later stack submits: a function whose `Layers` names the RETAINED version by
/// the ARN AWS Lambda published it under — a version no resource of this stack created — beside
/// the layer this stack publishes of its own, where the row declares one.
fn consumer_template(row: &Row, retained_arn: &str) -> String {
    let (beside_resource, beside_entry) = match &row.beside {
        Some((layer, _)) => (
            format!(
                r#""{BESIDE_LOGICAL_ID}":{{"Type":"AWS::Lambda::LayerVersion","Properties":{{
                   "LayerName":"{layer}",
                   "Content":{{"S3Bucket":"{STAGING_BUCKET}","S3Key":"{key}"}},
                   "CompatibleRuntimes":["{runtime}"]}}}},"#,
                key = layer_key(row.consumer),
                runtime = row.runtime,
            ),
            format!(r#",{{"Ref":"{BESIDE_LOGICAL_ID}"}}"#),
        ),
        None => (String::new(), String::new()),
    };
    format!(
        r#"{{"Resources":{{
             {beside_resource}
             {ROLE},
             "{FUNCTION_LOGICAL_ID}":{{"Type":"AWS::Lambda::Function","Properties":{{
               "FunctionName":"{consumer}-worker","Handler":"{HANDLER}",
               "Runtime":"{runtime}","Timeout":{FUNCTION_TIMEOUT},
               "Role":{{"Fn::GetAtt":["Role","Arn"]}},
               "Code":{{"S3Bucket":"{STAGING_BUCKET}","S3Key":"{code_key}"}},
               "Layers":["{retained_arn}"{beside_entry}]}}}}}},
           "Outputs":{{
             "{FUNCTION_OUTPUT}":{{"Value":{{"Ref":"{FUNCTION_LOGICAL_ID}"}}}}}}}}"#,
        consumer = row.consumer,
        runtime = row.runtime,
        code_key = code_key(row.consumer),
    )
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

/// The ARN of every layer a function's configuration reports, in the order it reports them.
fn carried_layers(configuration: &Value) -> Vec<String> {
    configuration
        .get("Layers")
        .and_then(Value::as_array)
        .map(|layers| {
            layers
                .iter()
                .filter_map(|layer| layer.get("Arn").and_then(Value::as_str))
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Invoke `function` synchronously and assert it ran to completion reporting exactly `expected`
/// — the modules its layers ship, resolved under the layer root.
async fn assert_the_layers_served(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    expected: &Value,
) {
    let state = wait_until_active(router, case, function).await;
    assert_eq!(
        state, STATE_ACTIVE,
        "case {case:?}: expected AWS Lambda to report {function} {STATE_ACTIVE} before an \
         invocation, got {state:?}"
    );
    let answered = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/invocations"),
        b"{}",
        &[("x-amz-invocation-type", "RequestResponse")],
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from Invoke of {function}, got {} with payload {}",
        answered.status, answered.body
    );
    assert_eq!(
        answered.function_error(),
        None,
        "case {case:?}: expected the handler of {function} to run to completion, got the error \
         document {}",
        answered.body
    );
    assert_eq!(
        &answered.body, expected,
        "case {case:?}: expected the handler of {function} to report {expected}, got {}",
        answered.body
    );
}

/// One row, end to end: publish and mount, delete the publishing stack, then deploy a second
/// stack whose function mounts the version that outlived it.
async fn drive(row: &Row) {
    let case = row.case;
    let (router, engine) = router_with_engine();

    // What a client stages before it submits the publishing template: the layer's archive, and
    // the function's own package whose handler imports what that archive ships.
    let retained_archive = package(row.retained.entry, &row.retained.source());
    stage(&router, case, &layer_key(row.publisher), &retained_archive).await;
    stage(
        &router,
        case,
        &code_key(row.publisher),
        &package(SOURCE_FILE, &handler_source(&[&row.retained])),
    )
    .await;

    let (status, body) = create_stack(
        &router,
        row.publisher,
        &publisher_template(row),
        &capabilities(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} {body}"
    );
    // The stack's own id, which stays addressable once the stack has been deleted — the name
    // does not have to.
    let publisher_id = xml_text(&body, "StackId").unwrap_or_default();
    assert!(
        !publisher_id.is_empty(),
        "case {case:?}: expected CreateStack to answer the stack's id, got {body}"
    );
    let (reported, described) = settled(&router, &engine, case, row.publisher).await;
    assert_eq!(
        reported, CREATE_COMPLETE,
        "case {case:?}: expected the publishing stack to deploy, got {reported:?} because {:?}",
        failure_reasons(&router, row.publisher).await
    );

    let retained_arn = layer_version_arn(row.layer, 1);
    let published = output_value(&described, LAYER_OUTPUT).unwrap_or_default();
    assert_eq!(
        published, retained_arn,
        "case {case:?}: expected the stack to publish the version {retained_arn:?}, got \
         {published:?} in {described}"
    );
    let publisher_function = output_value(&described, FUNCTION_OUTPUT).unwrap_or_default();
    assert!(
        !publisher_function.is_empty(),
        "case {case:?}: expected the stack to publish the function it deployed, got {described}"
    );

    // The publishing stack's own function has the layer in its execution environment: the
    // handler imports what the archive ships and reads its file under the layer root.
    assert_eq!(
        carried_layers(&get_function_configuration(&router, &publisher_function).await.1),
        vec![retained_arn.clone()],
        "case {case:?}: expected the function to carry the published version {retained_arn:?}"
    );
    assert_the_layers_served(
        &router,
        case,
        &publisher_function,
        &expected_report(&[&row.retained]),
    )
    .await;

    // The publishing stack goes away. Its function goes with it; the version it retained is AWS
    // Lambda's, so that service still holds the archive it published.
    let (status, body) = call(&router, "DeleteStack", &[member("StackName", row.publisher)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} {body}"
    );
    let (reported, described) = settled(&router, &engine, case, &publisher_id).await;
    assert_eq!(
        reported, DELETE_COMPLETE,
        "case {case:?}: expected the publishing stack to be deleted, got {reported:?} in \
         {described}"
    );
    let (status, gone) = get_function_configuration(&router, &publisher_function).await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected the deleted stack's function to be gone, got {status} {gone}"
    );
    let (status, held) = get_layer_version(&router, row.layer, 1).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected AWS Lambda to still hold the retained version, got {status} \
         {held}"
    );
    assert_eq!(
        held.get("LayerVersionArn").and_then(Value::as_str),
        Some(retained_arn.as_str()),
        "case {case:?}: expected the retained version {retained_arn:?}, got {held}"
    );
    assert_eq!(
        held.pointer("/Content/CodeSize").and_then(Value::as_i64),
        Some(retained_archive.len() as i64),
        "case {case:?}: expected the retained content to be the staged archive's {} bytes, got \
         {held}",
        retained_archive.len()
    );

    // A LATER stack's function mounts that version: the archive the deleted stack published is
    // still the layer's content, so the module it ships is on this handler's import path too.
    let mut mounted: Vec<&Shipped> = vec![&row.retained];
    let mut expected_arns = vec![retained_arn.clone()];
    if let Some((layer, beside)) = &row.beside {
        let archive = package(beside.entry, &beside.source());
        stage(&router, case, &layer_key(row.consumer), &archive).await;
        mounted.push(beside);
        expected_arns.push(layer_version_arn(layer, 1));
    }
    stage(
        &router,
        case,
        &code_key(row.consumer),
        &package(SOURCE_FILE, &handler_source(&mounted)),
    )
    .await;

    let (status, body) = create_stack(
        &router,
        row.consumer,
        &consumer_template(row, &retained_arn),
        &capabilities(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack for the later stack, got {status} \
         {body}"
    );
    let (reported, described) = settled(&router, &engine, case, row.consumer).await;
    assert_eq!(
        reported, CREATE_COMPLETE,
        "case {case:?}: expected the later stack to deploy against the retained version, got \
         {reported:?} because {:?}",
        failure_reasons(&router, row.consumer).await
    );
    let consumer_function = output_value(&described, FUNCTION_OUTPUT).unwrap_or_default();
    assert!(
        !consumer_function.is_empty(),
        "case {case:?}: expected the later stack to publish the function it deployed, got \
         {described}"
    );
    assert_eq!(
        carried_layers(&get_function_configuration(&router, &consumer_function).await.1),
        expected_arns,
        "case {case:?}: expected the later function to carry {expected_arns:?}"
    );
    assert_the_layers_served(&router, case, &consumer_function, &expected_report(&mounted)).await;
}

/// RULE: a retained layer version shipping its module in the version-independent import
/// directory serves the function of the stack that published it, and then the function of a
/// later stack that mounts it alone.
#[tokio::test]
async fn a_retained_version_serves_a_later_stack_that_mounts_it_alone() {
    drive(&Row {
        case: "a module in the version-independent import directory, mounted alone",
        publisher: "retained-layer-publisher",
        consumer: "retained-layer-consumer",
        layer: "retained-toolkit",
        runtime: "python3.12",
        retained: Shipped {
            module: "toolkit",
            entry: "python/toolkit.py",
            value: "from the layer the deleted stack published",
        },
        beside: None,
    })
    .await;
}

/// The same rule where the archive ships its module in the site-packages directory of the
/// function's OWN runtime version, on another runtime, and the later stack mounts the retained
/// version BESIDE a layer it publishes itself.
#[tokio::test]
async fn a_retained_version_serves_a_later_stack_beside_its_own_layer() {
    drive(&Row {
        case: "a module in this runtime version's site-packages, mounted beside a second layer",
        publisher: "retained-site-packages-publisher",
        consumer: "retained-site-packages-consumer",
        layer: "retained-vendored",
        runtime: "python3.11",
        retained: Shipped {
            module: "vendored_kit",
            entry: "python/lib/python3.11/site-packages/vendored_kit.py",
            value: "vendored beside the runtime's own packages",
        },
        beside: Some((
            "consumer-companion",
            Shipped {
                module: "companion",
                entry: "python/companion.py",
                value: "published by the stack that mounts both layers",
            },
        )),
    })
    .await;
}
