// SPDX-License-Identifier: CC0-1.0

//! `GetProvisionedProductOutputs` over the wire, driven as awsJson1_1 requests through a
//! `MultiServiceRouter` carrying Service Catalog beside the services a launch is made out
//! of.
//!
//! The rules under test:
//!
//! * "provisioning a product that's based on a CloudFormation template launches a
//!   CloudFormation stack and its underlying resources": the launch stands up the stack
//!   AWS names `SC-<account id>-<provisioned product id>`, submitting the product
//!   version's template with the provisioning parameters the request carried;
//! * what the provisioning request passes on to CloudFormation is what that stack is
//!   created with: one stack parameter per key its `ProvisioningParameters` name (the last
//!   value winning for a key named more than once, the template's own `Default` in force
//!   for a key the request does not name) and the SNS topics its `NotificationArns` name,
//!   "passed to CloudFormation ... to which to publish stack-related events";
//! * the outputs of a provisioned product ARE that stack's outputs — every key, value and
//!   description is the one CloudFormation holds, and a launch whose template declares
//!   none reports none;
//! * `OutputKeys` selects among them ("if none are provided, the API will return all
//!   outputs of the provisioned product") and `PageSize`/`PageToken` page over the
//!   selection without dropping or repeating one;
//! * a provisioned product no provisioning of which has SUCCEEDED yet has no outputs to
//!   report, and the question is refused rather than answered with an empty list;
//! * terminating a provisioned product tears that stack down, unless the request asks for
//!   the underlying resources to be retained.
//!
//! Every assertion is about state another service owns or an earlier call persisted: the
//! outputs are checked against what the staged TEMPLATE declares and what the launch
//! request supplied, and the stack behind them is read through CloudFormation's own wire.
//!
//! The rows differ along what the rules generalise over: how many outputs the template
//! declares (none, one, several), where an output's value comes from (a literal, a
//! parameter the request supplied, a resource the stack stood up), whether an output
//! carries a description, how the provisioned product is addressed (id, name), how many
//! keys are selected and how large a page is, and — for what a launch submits — how many
//! topics and parameters the request carries and whether it names one key twice.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    seed_launch_path, call, create_portfolio, create_product_with_info,
    create_template_bucket, emulator_with_template_services, error_code,
    first_provisioning_artifact_id, message, s3, stage_template, staged_template_url, ACCOUNT_ID,
    DEFAULT_REGION,
};

/// The `ProductType` of a CloudFormation-backed product, which is the only kind whose
/// launch stands a stack up (`smithy_query query="shape" shape="ProductType"`).
const CFN_PRODUCT: &str = "CLOUD_FORMATION_TEMPLATE";

/// `ProvisionedProductStatus`'s stable value
/// (`smithy_query query="shape" shape="ProvisionedProductStatus"`: AVAILABLE,
/// UNDER_CHANGE, TAINTED, ERROR, PLAN_IN_PROGRESS).
const AVAILABLE: &str = "AVAILABLE";

/// CloudFormation's `StackStatus` for a stack that finished being created.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// CloudFormation's `StackStatus` for a stack that finished being deleted.
const DELETE_COMPLETE: &str = "DELETE_COMPLETE";

/// The `OutputKey` Service Catalog reports the stack of a CloudFormation-backed
/// provisioned product under, beside the outputs that stack's own template declares.
const STACK_ARN_OUTPUT: &str = "CloudformationStackARN";

/// AWS's sentence for a provisioned product no provisioning of which has succeeded yet.
/// All fixed text: it carries no value from the request.
const NO_SUCCESSFUL_RECORD: &str = "Last Successful Provisioning Record doesn't exist.";

/// How long a poll keeps looking for a condition the emulator's clock owes before it
/// gives up. Far longer than any span this emulator settles on, so a loaded or slow host
/// cannot fail a case a fast one passes: the loop waits for the CONDITION, never for a
/// fixed time.
const POLL_DEADLINE: Duration = Duration::from_secs(90);

/// How long a poll waits between two looks.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Drive one CloudFormation request through the same router, as a client's own SDK sends
/// it: the query protocol that service speaks.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    pairs: &[(&str, &str)],
) -> (u16, String) {
    let mut form = ::form_urlencoded::Serializer::new(String::new());
    form.append_pair("Action", action);
    form.append_pair("Version", "2010-05-15");
    for (key, value) in pairs {
        form.append_pair(key, value);
    }
    let request = ::http::Request::builder()
        .method("POST")
        .uri(format!(
            "https://cloudformation.{DEFAULT_REGION}.amazonaws.com/"
        ))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTHARNESS/20260101/{DEFAULT_REGION}/\
                 cloudformation/aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .body(bytes::Bytes::from(form.finish()))
        .expect("a well-formed CloudFormation request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let read = String::from_utf8_lossy(&response.into_body()).to_string();
    (status, read)
}

/// The text of the first `<tag>` of an XML body, or `None` when it carries none.
fn xml_value(body: &str, tag: &str) -> Option<String> {
    body.split(&format!("<{tag}>"))
        .nth(1)
        .and_then(|rest| rest.split(&format!("</{tag}>")).next())
        .map(str::to_string)
}

/// The `<member>` entries of the first `tag` list of an XML body, each as the XML it holds,
/// in wire order — the list form of [`xml_value`], which reads a leaf.
fn list_members(body: &str, tag: &str) -> Vec<String> {
    let Some(block) = body
        .split(&format!("<{tag}>"))
        .nth(1)
        .and_then(|rest| rest.split(&format!("</{tag}>")).next())
    else {
        return Vec::new();
    };
    block
        .split("<member")
        .skip(1)
        // The first entry of a list carries the envelope's `xmlns`, so the element is
        // matched by name and what it holds starts after its opening tag.
        .filter_map(|entry| entry.split_once('>'))
        .filter_map(|(_, held)| held.split("</member>").next())
        .map(str::to_string)
        .collect()
}

/// The parameters one stack carries, as `key -> value`, read out of the `<Parameters>`
/// block of a `DescribeStacks` answer
/// (`<Parameters><member><ParameterKey/><ParameterValue/></member></Parameters>`).
fn stack_parameters(case: &str, described: &str) -> BTreeMap<String, String> {
    list_members(described, "Parameters")
        .iter()
        .map(|member| {
            let key = xml_value(member, "ParameterKey").unwrap_or_else(|| {
                panic!("case {case:?}: a stack parameter has no ParameterKey: {member}")
            });
            let value = xml_value(member, "ParameterValue").unwrap_or_else(|| {
                panic!("case {case:?}: the stack parameter {key} has no ParameterValue: {member}")
            });
            (key, value)
        })
        .collect()
}

/// The ARN of one SNS topic of this account, which is the form `NotificationArns` takes:
/// its member is an ARN (`smithy_query query="shape" shape="NotificationArns"`, pattern
/// `^arn:[a-z0-9-\.]{1,63}:...`), so a topic is named by ARN rather than by a bare name.
fn topic_arn(topic: &str) -> String {
    format!("arn:aws:sns:{DEFAULT_REGION}:{ACCOUNT_ID}:{topic}")
}

/// The `StackStatus` CloudFormation reports for `stack`, or `None` when it holds no such
/// stack at all.
async fn stack_status(router: &MultiServiceRouter, stack: &str) -> Option<String> {
    let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
    match status {
        200 => xml_value(&body, "StackStatus"),
        _ => None,
    }
}

/// Look at the stack `stack` until CloudFormation reports `wanted`, or the deadline
/// passes.
async fn stack_settles_to(router: &MultiServiceRouter, case: &str, stack: &str, wanted: &str) {
    let started = std::time::Instant::now();
    loop {
        let reported = stack_status(router, stack).await;
        if reported.as_deref() == Some(wanted) {
            return;
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case {case:?}: expected stack {stack} to reach {wanted}, last reported {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look at the provisioned product `name` until it reports `wanted`, or the deadline
/// passes.
async fn provisioned_product_settles_to(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    wanted: &str,
) {
    let started = std::time::Instant::now();
    loop {
        let (status, body) = call(router, "DescribeProvisionedProduct", &json!({"Name": name})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected provisioned product {name} to be described, got {status} \
             with {body}"
        );
        let reported = body["ProvisionedProductDetail"]["Status"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no provisioned product Status in {body}"))
            .to_string();
        if reported == wanted {
            return;
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case {case:?}: expected provisioned product {name} to reach {wanted}, last reported \
             {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// What one launch this suite drives is made of.
struct Launch<'a> {
    case: &'a str,
    /// The provisioned product's name, which also names the staged template object and
    /// the product it is created from.
    name: &'a str,
    /// The template the product version names, staged in the object store.
    template: String,
    /// The provisioning parameters the launch request carries.
    parameters: &'a [(&'a str, &'a str)],
    /// The SNS topic ARNs the launch request carries under `NotificationArns`, in request
    /// order: "passed to CloudFormation. The SNS topic ARNs to which to publish
    /// stack-related events."
    notification_arns: &'a [String],
}

/// What a launch left behind: the provisioned product's id and the stack it stood up.
struct Launched {
    provisioned_product_id: String,
    /// The stack's name, as AWS names the stack of a provisioned product.
    stack: String,
    /// The id CloudFormation minted for that stack, which addresses it even once it has
    /// been deleted — a deleted stack no longer resolves by name.
    stack_id: String,
}

/// Stage `launch`'s template, create the product and the portfolio that gives it a launch
/// path, and provision it — the whole administrator-then-end-user path a launch takes.
///
/// Answers the provisioned product's id and the stack the launch stands up, with the
/// provisioned product settled and that stack created: a caller then asserts on what the
/// launch produced.
async fn launch(router: &MultiServiceRouter, launch: &Launch<'_>) -> Launched {
    let Launch {
        case,
        name,
        template,
        parameters,
        notification_arns,
    } = launch;
    let key = format!("{name}.template.json");
    stage_template(router, &key, template).await;
    let portfolio = create_portfolio(router, &format!("{name}-portfolio"), "acme", &[], name).await;
    let product = create_product_with_info(
        router,
        name,
        CFN_PRODUCT,
        &json!({"LoadTemplateFromURL": staged_template_url(&key)}),
        name,
    )
    .await;
    let version = first_provisioning_artifact_id(router, &product).await;
    seed_launch_path(router, &product, &portfolio).await;

    let mut request = json!({
        "ProductId": product,
        "ProvisioningArtifactId": version,
        "ProvisionedProductName": name,
        "ProvisionToken": format!("{name}-provision"),
    });
    if !parameters.is_empty() {
        request["ProvisioningParameters"] = Value::Array(
            parameters
                .iter()
                .map(|(key, value)| json!({"Key": key, "Value": value}))
                .collect(),
        );
    }
    if !notification_arns.is_empty() {
        request["NotificationArns"] = json!(notification_arns);
    }
    let (status, provisioned) = call(router, "ProvisionProduct", &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the launch to be served, got {status} with {provisioned}"
    );
    let provisioned_product_id = provisioned["RecordDetail"]["ProvisionedProductId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no ProvisionedProductId in {provisioned}"))
        .to_string();

    // AWS names the stack a launch stands up `SC-<account id>-<provisioned product id>`,
    // so the stack is addressed by the id the launch minted rather than by a value this
    // file was handed.
    let stack = format!("SC-{ACCOUNT_ID}-{provisioned_product_id}");
    stack_settles_to(router, case, &stack, CREATE_COMPLETE).await;
    provisioned_product_settles_to(router, case, name, AVAILABLE).await;

    let (status, described) =
        cloudformation(router, "DescribeStacks", &[("StackName", &stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CloudFormation to hold the stack {stack}, got {status} with \
         {described}"
    );
    let stack_id = xml_value(&described, "StackId")
        .unwrap_or_else(|| panic!("case {case:?}: no StackId in {described}"));

    Launched {
        provisioned_product_id,
        stack,
        stack_id,
    }
}

/// One `GetProvisionedProductOutputs` call.
async fn get_outputs(router: &MultiServiceRouter, request: &Value) -> (u16, Value) {
    call(router, "GetProvisionedProductOutputs", request).await
}

/// The reported outputs as `key -> (value, description)`, which is the shape a template's
/// `Outputs` declarations are compared against.
fn outputs_of(case: &str, body: &Value) -> BTreeMap<String, (String, Option<String>)> {
    body["Outputs"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected an Outputs list, got {body}"))
        .iter()
        .map(|output| {
            let key = output["OutputKey"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: an output has no OutputKey: {output}"))
                .to_string();
            let value = output["OutputValue"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: output {key} has no OutputValue: {output}"))
                .to_string();
            let description = output["Description"].as_str().map(str::to_string);
            (key, (value, description))
        })
        .collect()
}

/// The keys the reported outputs carry, in the order they were reported.
fn output_keys(case: &str, body: &Value) -> Vec<String> {
    body["Outputs"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected an Outputs list, got {body}"))
        .iter()
        .map(|output| {
            output["OutputKey"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: an output has no OutputKey: {output}"))
                .to_string()
        })
        .collect()
}

/// A template declaring one bucket under the name `bucket` and the `Outputs` block
/// `outputs` — the shape of every template this suite stages, so the rows differ only in
/// what they declare as outputs.
fn template_with_outputs(bucket: &str, parameters: Value, outputs: Value) -> String {
    let mut document = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "A staged product template",
        "Resources": {
            "Data": {
                "Type": "AWS::S3::Bucket",
                "Properties": {"BucketName": bucket}
            }
        }
    });
    if !parameters.is_null() {
        document["Parameters"] = parameters;
    }
    if !outputs.is_null() {
        document["Outputs"] = outputs;
    }
    document.to_string()
}

/// One row of the outputs table: the template a launch is made from, the parameters the
/// request supplies and the outputs that launch must then report.
struct OutputsCase {
    case: &'static str,
    name: &'static str,
    /// The bucket the template's one resource is created under, which an output that
    /// `Ref`s that resource reports and which the object store must then hold.
    bucket: &'static str,
    parameters: &'static [(&'static str, &'static str)],
    /// The template's `Parameters` declarations.
    declared_parameters: Value,
    /// The template's `Outputs` declarations.
    declared_outputs: Value,
    /// The outputs the launch must report: key -> (value, description).
    expected: &'static [(&'static str, &'static str, Option<&'static str>)],
}

/// The outputs a launch reports are the outputs of the stack it stood up: each key, value
/// and description is the one the product version's template declared, evaluated against
/// the parameters the launch request supplied and the resources the stack created.
#[tokio::test]
async fn the_outputs_reported_are_the_outputs_of_the_stack_the_launch_stood_up() {
    // Rows differ along how many outputs the template declares, where each value comes
    // from (a literal, a parameter the request supplied, a resource the stack stood up)
    // and whether the declaration carries a description.
    let cases = [
        OutputsCase {
            case: "a template declaring no outputs",
            name: "launch-without-outputs",
            bucket: "launched-without-outputs",
            parameters: &[],
            declared_parameters: Value::Null,
            declared_outputs: Value::Null,
            expected: &[],
        },
        OutputsCase {
            case: "one output, carrying a description, reading the resource the stack created",
            name: "launch-with-one-output",
            bucket: "launched-with-one-output",
            parameters: &[],
            declared_parameters: Value::Null,
            declared_outputs: json!({
                "BucketName": {
                    "Value": {"Ref": "Data"},
                    "Description": "the bucket this launch stood up"
                }
            }),
            expected: &[(
                "BucketName",
                "launched-with-one-output",
                Some("the bucket this launch stood up"),
            )],
        },
        OutputsCase {
            case: "several outputs: a literal, a supplied parameter and a created resource",
            name: "launch-with-several-outputs",
            bucket: "launched-with-several-outputs",
            // The supplied value differs from the declared default, so an output that
            // echoes it can only report it if the launch really submitted it.
            parameters: &[("Environment", "prod")],
            declared_parameters: json!({
                "Environment": {"Type": "String", "Default": "dev"}
            }),
            declared_outputs: json!({
                "EnvironmentEcho": {
                    "Value": {"Ref": "Environment"},
                    "Description": "the environment the launch was asked for"
                },
                "BucketName": {"Value": {"Ref": "Data"}},
                "Partition": {"Value": "aws"}
            }),
            expected: &[
                (
                    "EnvironmentEcho",
                    "prod",
                    Some("the environment the launch was asked for"),
                ),
                ("BucketName", "launched-with-several-outputs", None),
                ("Partition", "aws", None),
            ],
        },
    ];

    for row in cases {
        let router = emulator_with_template_services();
        create_template_bucket(&router).await;
        let launched = launch(
            &router,
            &Launch {
                case: row.case,
                name: row.name,
                template: template_with_outputs(
                    row.bucket,
                    row.declared_parameters.clone(),
                    row.declared_outputs.clone(),
                ),
                parameters: row.parameters,
                notification_arns: &[],
            },
        )
        .await;

        // The resource the stack declared really exists in the service that owns it, so
        // an output reading it reports a bucket a caller can reach.
        let (status, body) = s3(&router, "HEAD", &format!("/{}", row.bucket), b"").await;
        assert_eq!(
            status, 200,
            "case {:?}: expected the launch to have created the bucket {}, got {status} with \
             {body}",
            row.case, row.bucket
        );

        let wanted: BTreeMap<String, (String, Option<String>)> = row
            .expected
            .iter()
            .map(|(key, value, description)| {
                (
                    key.to_string(),
                    (value.to_string(), description.map(str::to_string)),
                )
            })
            // Beside the template's own outputs, the launch reports the stack it stood
            // up: the ARN CloudFormation addresses that stack by, which is the id its
            // create was answered with.
            .chain([(
                STACK_ARN_OUTPUT.to_string(),
                (launched.stack_id.clone(), None),
            )])
            .collect();

        // Addressed by id and by name: "this API takes either a ProvisonedProductId or a
        // ProvisionedProductName", and both address the same launch, so both report the
        // same outputs.
        for addressed in [
            json!({"ProvisionedProductId": launched.provisioned_product_id}),
            json!({"ProvisionedProductName": row.name}),
        ] {
            let (status, body) = get_outputs(&router, &addressed).await;
            assert_eq!(
                status, 200,
                "case {:?}: expected {addressed} to be served, got {status} with {body}",
                row.case
            );
            let reported = outputs_of(row.case, &body);
            assert_eq!(
                reported, wanted,
                "case {:?}: expected the template's outputs {wanted:?} for {addressed}, got \
                 {reported:?}",
                row.case
            );
            assert!(
                body.get("NextPageToken").is_none(),
                "case {:?}: a page that reports every output has no next page, got {body}",
                row.case
            );
        }
    }
}

/// `OutputKeys` selects among the launch's outputs and `PageSize`/`PageToken` page over
/// that selection, reporting each selected output exactly once and in one order.
#[tokio::test]
async fn output_keys_select_and_the_page_size_bounds_what_is_reported() {
    let case = "four declared outputs";
    let router = emulator_with_template_services();
    create_template_bucket(&router).await;
    let launched = launch(
        &router,
        &Launch {
            case,
            name: "launch-with-four-outputs",
            template: template_with_outputs(
                "launched-with-four-outputs",
                Value::Null,
                json!({
                    "Alpha": {"Value": "first"},
                    "Beta": {"Value": "second"},
                    "Gamma": {"Value": "third"},
                    "Delta": {"Value": {"Ref": "Data"}}
                }),
            ),
            parameters: &[],
            notification_arns: &[],
        },
    )
    .await;
    let addressed = json!({"ProvisionedProductId": launched.provisioned_product_id});

    // The whole set, in the order the service reports it: every later selection and page
    // is compared against THIS order rather than against one this file assumes.
    let (status, body) = get_outputs(&router, &addressed).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the whole set to be served, got {status} with {body}"
    );
    let all = output_keys(case, &body);
    assert_eq!(
        all.len(),
        5,
        "case {case:?}: expected the four declared outputs beside the launch's stack, got {all:?}"
    );

    // One key, two keys, the key the launch's own stack is reported under, and a key the
    // stack declares no output for: the keys SELECT, so a key that names nothing selects
    // nothing.
    let selections: [(&str, Vec<&str>, Vec<&str>); 5] = [
        ("one key", vec!["Beta"], vec!["Beta"]),
        ("two keys", vec!["Alpha", "Delta"], vec!["Alpha", "Delta"]),
        (
            "the launch's stack",
            vec![STACK_ARN_OUTPUT],
            vec![STACK_ARN_OUTPUT],
        ),
        (
            "a key no output carries",
            vec!["Gamma", "Epsilon"],
            vec!["Gamma"],
        ),
        ("only keys no output carries", vec!["Epsilon"], vec![]),
    ];
    for (selection, keys, wanted) in selections {
        let mut request = addressed.clone();
        request["OutputKeys"] = json!(keys);
        let (status, body) = get_outputs(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}/{selection}: expected {request} to be served, got {status} with {body}"
        );
        let reported = output_keys(case, &body);
        let wanted: Vec<String> = all
            .iter()
            .filter(|key| wanted.contains(&key.as_str()))
            .cloned()
            .collect();
        assert_eq!(
            reported, wanted,
            "case {case:?}/{selection}: expected {wanted:?} for keys {keys:?}, got {reported:?}"
        );
    }

    // Paged two at a time: each page reports at most the page size, carries a next page
    // token while anything is left, and the pages concatenated are the whole set in the
    // same order — nothing dropped and nothing repeated.
    let mut paged: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0;
    loop {
        let mut request = addressed.clone();
        request["PageSize"] = json!(2);
        if let Some(token) = &token {
            request["PageToken"] = json!(token);
        }
        let (status, body) = get_outputs(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}/paged: expected {request} to be served, got {status} with {body}"
        );
        let page = output_keys(case, &body);
        assert!(
            page.len() <= 2,
            "case {case:?}/paged: expected at most the page size of 2, got {page:?}"
        );
        paged.extend(page);
        pages += 1;
        token = body["NextPageToken"].as_str().map(str::to_string);
        if token.is_none() {
            break;
        }
        assert!(
            pages <= all.len(),
            "case {case:?}/paged: paging over {} outputs must end, got {paged:?} after {pages} \
             pages",
            all.len()
        );
    }
    assert_eq!(
        paged, all,
        "case {case:?}/paged: expected the pages to be the whole set {all:?}, got {paged:?}"
    );
    assert_eq!(
        pages, 3,
        "case {case:?}/paged: expected five outputs to take three pages of two, got {pages}"
    );

    // A page over a selection is bounded the same way.
    let mut request = addressed.clone();
    request["OutputKeys"] = json!(["Alpha", "Beta", "Gamma"]);
    request["PageSize"] = json!(1);
    let (status, body) = get_outputs(&router, &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}/selected page: expected {request} to be served, got {status} with {body}"
    );
    assert_eq!(
        output_keys(case, &body).len(),
        1,
        "case {case:?}/selected page: expected one output per page, got {body}"
    );
    assert!(
        body["NextPageToken"].as_str().is_some(),
        "case {case:?}/selected page: three selected outputs leave a next page, got {body}"
    );

    // A page token this service did not mint names no position in that list.
    let mut request = addressed.clone();
    request["PageToken"] = json!("not-a-token");
    let (status, body) = get_outputs(&router, &request).await;
    assert_eq!(
        status, 400,
        "case {case:?}/bad token: expected a refusal, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidParametersException",
        "case {case:?}/bad token: expected the parameter error, got {body}"
    );
}

/// A provisioned product no provisioning of which has succeeded yet has no outputs to
/// report, and a request that addresses no provisioned product at all is refused rather
/// than answered.
#[tokio::test]
async fn a_launch_with_no_successful_provisioning_record_is_refused() {
    let router = emulator_with_template_services();
    create_template_bucket(&router).await;
    let name = "launch-still-in-flight";
    let key = format!("{name}.template.json");
    stage_template(
        &router,
        &key,
        &template_with_outputs(
            "launched-still-in-flight",
            Value::Null,
            json!({"Alpha": {"Value": "first"}}),
        ),
    )
    .await;
    let portfolio = create_portfolio(&router, "in-flight-portfolio", "acme", &[], "in-flight").await;
    let product = create_product_with_info(
        &router,
        name,
        CFN_PRODUCT,
        &json!({"LoadTemplateFromURL": staged_template_url(&key)}),
        name,
    )
    .await;
    let version = first_provisioning_artifact_id(&router, &product).await;
    seed_launch_path(&router, &product, &portfolio).await;
    let (status, provisioned) = call(
        &router,
        "ProvisionProduct",
        &json!({
            "ProductId": product,
            "ProvisioningArtifactId": version,
            "ProvisionedProductName": name,
            "ProvisionToken": "in-flight-provision",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected the launch to be served, got {status} with {provisioned}"
    );
    let provisioned_product_id = provisioned["RecordDetail"]["ProvisionedProductId"]
        .as_str()
        .unwrap_or_else(|| panic!("no ProvisionedProductId in {provisioned}"))
        .to_string();

    // The provisioning is still in flight — the record it minted has not succeeded — so
    // there is no successful provisioning whose outputs could be reported.
    let (status, body) = get_outputs(
        &router,
        &json!({"ProvisionedProductId": provisioned_product_id}),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"still in flight\": expected a refusal, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidParametersException",
        "case \"still in flight\": expected the parameter error, got {body}"
    );
    assert_eq!(
        message(&body),
        NO_SUCCESSFUL_RECORD,
        "case \"still in flight\": expected AWS's sentence, got {body}"
    );

    // Once the clock owes the provisioning's completion the same read is served, so the
    // refusal above is about the launch's progress and nothing else.
    provisioned_product_settles_to(&router, "still in flight", name, AVAILABLE).await;
    let (status, body) = get_outputs(
        &router,
        &json!({"ProvisionedProductId": provisioned_product_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"settled\": expected the outputs to be served, got {status} with {body}"
    );
    let settled = output_keys("settled", &body);
    assert_eq!(
        settled,
        vec!["Alpha".to_string(), STACK_ARN_OUTPUT.to_string()],
        "case \"settled\": expected the template's one output beside the launch's stack, got \
         {body}"
    );

    // How a request can fail to address one provisioned product: both members, neither
    // member, an id nothing stored carries and a name nothing stored carries.
    let refusals: [(&str, Value, &str); 4] = [
        (
            "both members",
            json!({"ProvisionedProductId": provisioned_product_id, "ProvisionedProductName": name}),
            "InvalidParametersException",
        ),
        ("neither member", json!({}), "InvalidParametersException"),
        (
            "an id nothing stored carries",
            json!({"ProvisionedProductId": "pp-000000000000"}),
            "ResourceNotFoundException",
        ),
        (
            "a name nothing stored carries",
            json!({"ProvisionedProductName": "no-such-provisioned-product"}),
            "ResourceNotFoundException",
        ),
    ];
    for (case, request, wanted) in refusals {
        let (status, body) = get_outputs(&router, &request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a refusal, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            wanted,
            "case {case:?}: expected {wanted}, got {body}"
        );
    }
}

/// Terminating a provisioned product tears the stack behind it down — unless the request
/// asks for the underlying resources to be retained, which leaves that stack standing.
#[tokio::test]
async fn terminating_a_provisioned_product_tears_its_stack_down() {
    // Rows differ along `RetainPhysicalResources`: "when true, Service Catalog stops
    // managing the specified provisioned product without deleting the underlying
    // resources".
    let cases: [(&str, bool); 2] = [
        ("the launch's resources are deleted with it", false),
        ("the launch's resources are retained", true),
    ];

    for (case, retain) in cases {
        let router = emulator_with_template_services();
        create_template_bucket(&router).await;
        let name = match retain {
            true => "launch-retained-on-terminate",
            false => "launch-deleted-on-terminate",
        };
        let bucket = match retain {
            true => "launched-retained-on-terminate",
            false => "launched-deleted-on-terminate",
        };
        let launched = launch(
            &router,
            &Launch {
                case,
                name,
                template: template_with_outputs(
                    bucket,
                    Value::Null,
                    json!({"BucketName": {"Value": {"Ref": "Data"}}}),
                ),
                parameters: &[],
                notification_arns: &[],
            },
        )
        .await;

        let (status, terminated) = call(
            &router,
            "TerminateProvisionedProduct",
            &json!({
                "ProvisionedProductId": launched.provisioned_product_id,
                "TerminateToken": format!("{name}-terminate"),
                "RetainPhysicalResources": retain,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the termination to be served, got {status} with {terminated}"
        );

        if retain {
            // The provisioned product stops existing, and the stack it stood up is left
            // exactly where it was.
            let started = std::time::Instant::now();
            loop {
                let (status, body) =
                    call(&router, "DescribeProvisionedProduct", &json!({"Name": name})).await;
                if status == 400 {
                    assert_eq!(
                        error_code(&body),
                        "ResourceNotFoundException",
                        "case {case:?}: expected the terminated provisioned product to be gone, \
                         got {body}"
                    );
                    break;
                }
                assert!(
                    started.elapsed() < POLL_DEADLINE,
                    "case {case:?}: expected {name} to stop existing, last answered {status} with \
                     {body}"
                );
                tokio::time::sleep(POLL_INTERVAL).await;
            }
            let reported = stack_status(&router, &launched.stack).await;
            assert_eq!(
                reported.as_deref(),
                Some(CREATE_COMPLETE),
                "case {case:?}: expected the retained stack {} to stand, got {reported:?}",
                launched.stack
            );
        } else {
            // The stack is torn down by the service that owns it, so CloudFormation is
            // what reports it deleted. A deleted stack no longer resolves by name, so it
            // is read back by the id its create answered with.
            stack_settles_to(&router, case, &launched.stack_id, DELETE_COMPLETE).await;
            let by_name = stack_status(&router, &launched.stack).await;
            assert!(
                by_name.is_none(),
                "case {case:?}: expected the deleted stack {} to stop resolving by name, got \
                 {by_name:?}",
                launched.stack
            );
        }
    }
}

/// One row of the submitted-launch table: what the provisioning request passes on to
/// CloudFormation, and what the stack it stands up must then report.
struct SubmittedCase {
    case: &'static str,
    /// The provisioned product's name, which also names the staged template object and the
    /// product it is created from.
    name: &'static str,
    /// The bucket the template's one resource is created under, so two rows collide over
    /// nothing.
    bucket: &'static str,
    /// The topics the request names under `NotificationArns`, in request order.
    topics: &'static [&'static str],
    /// The `ProvisioningParameters` the request carries, in request order.
    parameters: &'static [(&'static str, &'static str)],
    /// The parameters the stack must then report: one entry per key the template declares,
    /// carrying the value in force for it.
    expected: &'static [(&'static str, &'static str)],
}

/// What the provisioning request passes on to CloudFormation is what the stack the launch
/// stands up is created with: one stack parameter per key the request's
/// `ProvisioningParameters` name — the last value winning for a key named more than once,
/// the template's own `Default` in force for a key the request does not name — and, in
/// request order, the SNS topics its `NotificationArns` name, which are "passed to
/// CloudFormation" as the topics that stack's events are published to.
#[tokio::test]
async fn the_launch_submits_the_requests_parameters_and_topics_to_the_stack() {
    // The `Parameters` every row's template declares, each with the `Default` — "a value
    // of the appropriate type for the template to use if no value is specified" — a
    // request that names it nothing leaves in force.
    let declared = json!({
        "Environment": {"Type": "String", "Default": "dev"},
        "Retention": {"Type": "String", "Default": "7"}
    });

    // Rows differ along what the rule generalises over: how many topics the request names
    // (none, one, several, the five `NotificationArns` admits at most), how many of the
    // declared parameters it supplies, in which order it supplies them, and whether it
    // names one key twice.
    let cases = [
        SubmittedCase {
            case: "neither topics nor parameters supplied",
            name: "launch-submitting-nothing",
            bucket: "launched-submitting-nothing",
            topics: &[],
            parameters: &[],
            expected: &[("Environment", "dev"), ("Retention", "7")],
        },
        SubmittedCase {
            case: "one topic and one of the two parameters supplied",
            name: "launch-submitting-one-of-each",
            bucket: "launched-submitting-one-of-each",
            topics: &["launch-events"],
            parameters: &[("Environment", "prod")],
            expected: &[("Environment", "prod"), ("Retention", "7")],
        },
        SubmittedCase {
            case: "three topics and both parameters, supplied in the other order",
            name: "launch-submitting-three-topics",
            bucket: "launched-submitting-three-topics",
            topics: &["launch-events", "audit-events", "ops-events"],
            parameters: &[("Retention", "30"), ("Environment", "stage")],
            expected: &[("Environment", "stage"), ("Retention", "30")],
        },
        SubmittedCase {
            case: "five topics — the modelled maximum — and a parameter key named twice",
            name: "launch-submitting-five-topics",
            bucket: "launched-submitting-five-topics",
            topics: &[
                "launch-events",
                "audit-events",
                "ops-events",
                "finance-events",
                "security-events",
            ],
            parameters: &[
                ("Environment", "first"),
                ("Retention", "14"),
                ("Environment", "last"),
            ],
            expected: &[("Environment", "last"), ("Retention", "14")],
        },
    ];

    for row in cases {
        let router = emulator_with_template_services();
        create_template_bucket(&router).await;
        let topics: Vec<String> = row.topics.iter().map(|topic| topic_arn(topic)).collect();
        let launched = launch(
            &router,
            &Launch {
                case: row.case,
                name: row.name,
                template: template_with_outputs(row.bucket, declared.clone(), Value::Null),
                parameters: row.parameters,
                notification_arns: &topics,
            },
        )
        .await;

        // The stack is read through CloudFormation's own wire: what it reports is what the
        // launch submitted to it, not anything Service Catalog echoed back.
        let (status, described) = cloudformation(
            &router,
            "DescribeStacks",
            &[("StackName", &launched.stack)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected CloudFormation to hold the stack {}, got {status} with \
             {described}",
            row.case, launched.stack
        );

        let reported_topics: Vec<String> = list_members(&described, "NotificationARNs")
            .iter()
            .map(|entry| entry.trim().to_string())
            .collect();
        assert_eq!(
            reported_topics, topics,
            "case {:?}: expected the stack to be created with the topics {topics:?}, got \
             {reported_topics:?}",
            row.case
        );

        let wanted: BTreeMap<String, String> = row
            .expected
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        let reported = stack_parameters(row.case, &described);
        assert_eq!(
            reported, wanted,
            "case {:?}: expected the stack to carry the parameters {wanted:?}, got {reported:?}",
            row.case
        );
        // A stack holds each parameter key ONCE, so a key the request named twice reaches
        // it as one parameter rather than as two entries disagreeing about its value.
        let submitted = list_members(&described, "Parameters").len();
        assert_eq!(
            submitted,
            wanted.len(),
            "case {:?}: expected one stack parameter per declared key ({}), got {submitted}",
            row.case,
            wanted.len()
        );
    }
}

/// `NotificationArns` is modelled as a list of at most five entries
/// (`smithy_query query="shape" shape="NotificationArns"`: `@length(min: 0, max: 5)`), so a
/// provisioning request naming more is refused with HTTP 400 `ValidationException` and
/// nothing is provisioned — the launch above having shown that a request at the bound is
/// served.
#[tokio::test]
async fn a_launch_naming_more_topics_than_the_model_admits_is_refused() {
    let router = emulator_with_template_services();
    create_template_bucket(&router).await;
    let name = "launch-with-too-many-topics";
    let key = format!("{name}.template.json");
    stage_template(
        &router,
        &key,
        &template_with_outputs("launched-with-too-many-topics", Value::Null, Value::Null),
    )
    .await;
    let portfolio = create_portfolio(&router, "too-many-topics", "acme", &[], "too-many").await;
    let product = create_product_with_info(
        &router,
        name,
        CFN_PRODUCT,
        &json!({"LoadTemplateFromURL": staged_template_url(&key)}),
        name,
    )
    .await;
    let version = first_provisioning_artifact_id(&router, &product).await;
    seed_launch_path(&router, &product, &portfolio).await;

    // Rows: one past the bound, and well past it. Everything else about the request
    // resolves, so the refusal can only be about how many topics it names.
    for (case, count) in [("one past the maximum", 6), ("well past the maximum", 9)] {
        let topics: Vec<String> = (0..count)
            .map(|index| topic_arn(&format!("launch-events-{index}")))
            .collect();
        let (status, refused) = call(
            &router,
            "ProvisionProduct",
            &json!({
                "ProductId": product,
                "ProvisioningArtifactId": version,
                "ProvisionedProductName": name,
                "ProvisionToken": format!("{name}-{count}"),
                "NotificationArns": topics,
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected {count} topics to be refused, got {status} with {refused}"
        );
        assert_eq!(
            error_code(&refused),
            "ValidationException",
            "case {case:?}: expected the modelled bound to refuse the request, got {refused}"
        );

        // A refused request provisions nothing: the name it asked for is held by no
        // provisioned product at all.
        let (status, described) = call(&router, "DescribeProvisionedProduct", &json!({"Name": name}))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the refusal to have provisioned nothing, got {status} with \
             {described}"
        );
        assert_eq!(
            error_code(&described),
            "ResourceNotFoundException",
            "case {case:?}: expected no provisioned product named {name}, got {described}"
        );
    }
}
