// SPDX-License-Identifier: CC0-1.0

//! The queue a stack declared, across the stack's NEXT deployment.
//!
//! RULE: a physical resource id its OWNING SERVICE answered with is the identity in force, so a
//! deployment that cannot recompute it carries the recorded one forward and reads the
//! declaration as the resource the stack already has — the value the naming rules compute is
//! only the stand-in a reference was resolved against until that service answered. An
//! `AWS::SQS::Queue` is in that position: "Ref returns the queue URL", and a queue url is a
//! string a client sends straight back, so Amazon SQS reports the queue at the endpoint the
//! create REACHED rather than at that service's published regional host — an address of
//! wherever this emulator is served, which no template states. A later deployment of the same
//! declaration therefore leaves the queue exactly where it is, rather than creating a second
//! one and taking down the queue every reference resolves to.
//!
//! The rows differ along what the rule generalises over: whether the queue was NAMED by the
//! template or named by CloudFormation, and whether the second deployment changes another
//! resource's property or nothing about any resource at all. Each is judged where a client sees
//! it — Amazon SQS's own answer for the url the stack published, and AWS Lambda's own answer for
//! the property the update did change.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

use crate::harness::{
    call, create_stack, member, settled_stack, settled_stack_status, stack_event_reasons,
    xml_blocks, xml_text, REGION,
};
use crate::stack_lifecycle::acknowledging_iam;

/// One address this emulator can be SERVED at, which this suite's router announces as its own
/// endpoint.
///
/// A served emulator is the shape every address it hands a client is read in: the url Amazon SQS
/// reports for a queue names the endpoint the call reached, so the identifier the stack records
/// for a declared queue is a value no deployment recomputes from the template. An emulator
/// served on no port at all reports that service's published host instead, which the naming
/// rules do compute — so the rule under test is only observable with a base set.
const SERVED_AT: &str = "https://127.0.0.1:9297";

/// A router carrying CloudFormation, AWS Lambda, Amazon SQS and IAM on ONE engine — the shape
/// `core/router/src/lib.rs` mounts every service in.
///
/// Its own router rather than the shared harness's, for the reason
/// `lambda_event_source_mapping_batch_item_failures.rs` records: mounting Amazon SQS for every
/// suite of this target would change what the accounts those suites build hold.
fn router_with_a_queue_service() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router.set_url_base(EndpointUrl::parse(SERVED_AT).expect("the suite's base URL must parse"));
    router
}

/// The inline source the declared function carries: what is under test is the estate a
/// deployment leaves, not what a handler computes.
const HANDLER_SOURCE: &str = "def handler(event, context):\n    return {}\n";

/// The environment value the first deployment states, and the one the row that changes a
/// property states instead.
const FIRST_VERSION: &str = "v1";
const SECOND_VERSION: &str = "v2";

/// One Amazon SQS call (`awsJson1_0`), answered with its status and its document.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://sqs.{REGION}.amazonaws.com/"))
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/sqs/aws4_request, \
                 SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(body.to_string().into_bytes()))
        .expect("a well-formed Amazon SQS request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// The ARN Amazon SQS reports for the queue `url` addresses, or the refusal it answered.
async fn queue_arn(router: &MultiServiceRouter, url: &str) -> (u16, Value) {
    sqs(
        router,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await
}

/// What `GetFunctionConfiguration` reports for one function.
async fn function_configuration(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!(
            "https://lambda.{REGION}.amazonaws.com/2015-03-31/functions/{name}/configuration"
        ))
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/lambda/aws4_request, \
                 SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::new())
        .expect("a well-formed AWS Lambda request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// The value one `Outputs` entry of a `DescribeStacks` body reports.
fn output(body: &str, key: &str) -> Option<String> {
    xml_blocks(body, "member")
        .into_iter()
        .find(|block| xml_text(block, "OutputKey").as_deref() == Some(key))
        .and_then(|block| xml_text(&block, "OutputValue"))
}

/// The two queues every template of this suite declares: one the template NAMES, and one it
/// leaves CloudFormation to name.
const QUEUE_OUTPUTS: &[(&str, &str)] = &[
    ("NamedUrl", "a queue the template names"),
    ("GeneratedUrl", "a queue CloudFormation names"),
];

/// The template a row deploys: the two queues, the function's execution role, and the function
/// whose `Environment` the property row changes.
///
/// `description` is the stack's own `Description`, which is what the row that revises NOTHING
/// about any resource changes, and `version` the environment value the function states.
fn template(named_queue: &str, description: &str, version: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": description,
        "Resources": {
            "Named": {
                "Type": "AWS::SQS::Queue",
                "Properties": { "QueueName": named_queue },
            },
            "Generated": { "Type": "AWS::SQS::Queue" },
            "Role": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "lambda.amazonaws.com" },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    "Policies": [{
                        "PolicyName": "forward-to-the-sinks",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": "sqs:SendMessage",
                                "Resource": "*",
                            }],
                        },
                    }],
                },
            },
            "Forwarder": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": ["Role", "Arn"] },
                    // The function names the queue by the identity in force, so a deployment
                    // that read the queue as a different resource would also hand the function
                    // another address.
                    "Environment": { "Variables": {
                        "QUEUE_URL": { "Ref": "Generated" },
                        "VERSION": version,
                    }},
                    "Code": { "ZipFile": HANDLER_SOURCE },
                },
            },
        },
        "Outputs": {
            "NamedUrl": { "Value": { "Ref": "Named" } },
            "GeneratedUrl": { "Value": { "Ref": "Generated" } },
            "FunctionName": { "Value": { "Ref": "Forwarder" } },
        },
    })
    .to_string()
}

/// One row: what the second deployment of the stack revises.
struct Row {
    case: &'static str,
    stack: &'static str,
    named_queue: &'static str,
    /// The `Description` the second deployment states.
    description: &'static str,
    /// The environment value it states, which the function has to report once it has settled.
    version: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "an update that changes another resource's property",
        stack: "redeployed-queue-changed-function",
        named_queue: "redeployed-queue-changed-sink",
        description: "the environment value raised",
        version: SECOND_VERSION,
    },
    Row {
        case: "an update that revises nothing about any resource",
        stack: "redeployed-queue-untouched",
        named_queue: "redeployed-queue-untouched-sink",
        description: "the same resources, redeployed",
        version: FIRST_VERSION,
    },
];

#[tokio::test]
async fn a_redeployed_stack_keeps_the_queue_amazon_sqs_reported() {
    for row in ROWS {
        let case = row.case;
        let router = router_with_a_queue_service();
        let (status, created) = create_stack(
            &router,
            row.stack,
            &template(row.named_queue, "the deployment the stack was created from", FIRST_VERSION),
            &acknowledging_iam(&[]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {created}"
        );
        let reached = settled_stack_status(case, &router, REGION, row.stack).await;
        let reasons = stack_event_reasons(&router, REGION, row.stack).await;
        assert_eq!(
            reached, "CREATE_COMPLETE",
            "case {case:?}: expected the create to complete, got {reached} with {reasons:?}"
        );

        // The urls the stack published, each an address of the endpoint this emulator is served
        // at — the identity Amazon SQS answered with, which the stand-in the naming rules
        // computed is not.
        let described = settled_stack(case, &router, REGION, row.stack).await;
        let urls: Vec<(&str, &str, String)> = QUEUE_OUTPUTS
            .iter()
            .map(|(key, queue)| {
                let url = output(&described, key).unwrap_or_else(|| {
                    panic!("case {case:?}: expected the stack to publish {key}, got {described}")
                });
                assert!(
                    url.starts_with(&format!("{SERVED_AT}/")),
                    "case {case:?}: expected {queue} to be published at the served endpoint, got \
                     {url:?}"
                );
                (*key, *queue, url)
            })
            .collect();
        let function = output(&described, "FunctionName").unwrap_or_else(|| {
            panic!("case {case:?}: expected the stack to publish FunctionName, got {described}")
        });
        for (_, queue, url) in &urls {
            let (status, answered) = queue_arn(&router, url).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected Amazon SQS to hold {queue} at the url the stack \
                 published, got {status} with {answered}"
            );
        }

        // The SECOND deployment of the same declarations.
        let (status, answered) = call(
            &router,
            "UpdateStack",
            &acknowledging_iam(&[
                member("StackName", row.stack),
                member(
                    "TemplateBody",
                    &template(row.named_queue, row.description, row.version),
                ),
            ]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with {answered}"
        );
        let reached = settled_stack_status(case, &router, REGION, row.stack).await;
        let reasons = stack_event_reasons(&router, REGION, row.stack).await;
        assert_eq!(
            reached, "UPDATE_COMPLETE",
            "case {case:?}: expected the update to complete, got {reached} with {reasons:?}"
        );

        // Each queue is the one the stack already had: the same url, and Amazon SQS still holds
        // a queue at it.
        let updated = settled_stack(case, &router, REGION, row.stack).await;
        for (key, queue, url) in &urls {
            let republished = output(&updated, key);
            assert_eq!(
                republished.as_deref(),
                Some(url.as_str()),
                "case {case:?}: expected {queue} to keep the url {url:?} across the second \
                 deployment, got {republished:?}"
            );
            let (status, answered) = queue_arn(&router, url).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected Amazon SQS to still hold {queue} at {url:?} after the \
                 second deployment, got {status} with {answered}"
            );
        }

        // ...and the function the deployment really did revise carries the declared value, read
        // back through AWS Lambda's own wire.
        let (status, held) = function_configuration(&router, &function).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected AWS Lambda to report the function the stack deployed, got \
             {status} with {held}"
        );
        let reported = (
            held.pointer("/Environment/Variables/VERSION").cloned(),
            held.pointer("/Environment/Variables/QUEUE_URL").cloned(),
        );
        let expected_url = urls
            .iter()
            .find(|(key, _, _)| *key == "GeneratedUrl")
            .map(|(_, _, url)| json!(url))
            .expect("the suite publishes the generated queue's url");
        assert_eq!(
            reported,
            (Some(json!(row.version)), Some(expected_url.clone())),
            "case {case:?}: expected the function's environment to carry {:?} and {expected_url}, \
             got {reported:?} in {held}",
            row.version
        );
    }
}
