// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack provisions the Amazon SageMaker AI ENDPOINT its template declares —
//! the model, the endpoint configuration that hosts it and the endpoint deployed from that
//! configuration — in SageMaker itself, and deleting the stack takes each of them away again.
//!
//! The rule under test: **a stack reports a resource created only once the service that owns
//! the type created it, so every identifier the stack publishes for one resolves in that
//! service, and a resource whose owning service answers a non-terminal status holds the
//! stack's operation open until that service reports it ready.** An
//! `AWS::SageMaker::EndpointConfig` and an `AWS::SageMaker::Endpoint` are addressed by their
//! NAME in every SageMaker call that reaches them — `CreateEndpoint` takes an
//! `EndpointConfigName`, and `DescribeEndpoint`, `DeleteEndpoint` and
//! `CreateInferenceComponent` an `EndpointName` — and `CreateEndpoint` answers the endpoint
//! `Creating`, so a stack that completed must leave a configuration
//! `DescribeEndpointConfig` resolves and an endpoint `DescribeEndpoint` reports `InService`
//! and an inference component can attach to.
//!
//! Three crates have to agree for that to hold (CloudFormation makes the calls, SageMaker
//! serves them, IAM holds the role the model runs as), so the suite lives here rather than in
//! any one of them.
//!
//! The rows differ along what the rule generalises over: whether each resource's name is a
//! literal or left for CloudFormation to generate, whether the endpoint names its
//! configuration through a `Ref` or through the same literal, and whether the declaration
//! spells the configuration's boolean property as JSON's own value or as the text a parameter
//! reference resolves to. Every expected value is computed here from what the row declares,
//! never read back out of the answer being checked.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery encoder and the XML/Outputs readers this suite needs already exist in the
// test binary's shared modules; they are shared rather than copied.
use crate::cloudformation_provisions_stack_resources::{outputs_of, xml_text};
use crate::query_wire::form_encode;

/// The region every request here is signed for, and so the region the stack's resources and
/// their ARNs belong to.
const REGION: &str = "us-west-2";

/// The account id every ARN this emulator mints carries.
const ACCOUNT_ID: &str = "000000000000";

/// How long a case waits for a stack to reach the status the rule says it reaches.
///
/// GENEROUS on purpose: the only thing under test is WHICH status the stack reaches, never how
/// fast it gets there, so the deadline leaves room for a loaded host and the case polls for the
/// condition rather than sleeping for a span.
const STATUS_DEADLINE: Duration = Duration::from_secs(60);

/// How long the poll below waits between two reads of the stack.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The image a declared model container is pulled from.
const CONTAINER_IMAGE: &str = "000000000000.dkr.ecr.us-west-2.amazonaws.com/inference:1";

/// The variant each declared configuration hosts its one model on.
const VARIANT: &str = "AllTraffic";

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sagemaker_sagemaker::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: this router runs with request authentication
/// off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// Call one SageMaker operation over its awsJson1_1 wire.
async fn sagemaker(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("api.sagemaker.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("SageMaker.{operation}"))
        .header("authorization", authorization("sagemaker"))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// Create a stack from `template` and answer its id, failing the case with the response body
/// when the create is refused.
async fn create_stack(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
    parameters: &[(&str, &str)],
) -> String {
    let mut members: Vec<(&str, &str)> = vec![
        ("StackName", stack_name),
        ("TemplateBody", template),
        // Every row declares an IAM role for the model to run as.
        ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
    ];
    let flattened: Vec<(String, String)> = parameters
        .iter()
        .enumerate()
        .flat_map(|(index, (key, value))| {
            let position = index + 1;
            [
                (
                    format!("Parameters.member.{position}.ParameterKey"),
                    (*key).to_string(),
                ),
                (
                    format!("Parameters.member.{position}.ParameterValue"),
                    (*value).to_string(),
                ),
            ]
        })
        .collect();
    for (key, value) in &flattened {
        members.push((key.as_str(), value.as_str()));
    }
    let (status, body) = cloudformation(router, "CreateStack", &members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    xml_text(&body, "StackId").unwrap_or_else(|| panic!("case {case:?}: no <StackId> in {body}"))
}

/// Read the stack until it reports `wanted`, and answer the body that did.
///
/// The CONDITION is what is waited for, never a span: a stack whose endpoint is still coming up
/// is read again until it rests in the status the rule says it reaches or [`STATUS_DEADLINE`]
/// passes.
async fn stack_resting_in(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    wanted: &str,
) -> String {
    let deadline = Instant::now() + STATUS_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks for {stack:?}, got {status} \
             with body {body}"
        );
        let reached = xml_text(&body, "StackStatus").unwrap_or_default();
        if reached == wanted {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the stack to reach {wanted:?}, got {reached:?} with body \
             {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The ARN an endpoint of `name` carries in this emulator, as the modelled `EndpointArn` form
/// spells it.
fn endpoint_arn(name: &str) -> String {
    format!("arn:aws:sagemaker:{REGION}:{ACCOUNT_ID}:endpoint/{name}")
}

/// The ARN an endpoint configuration of `name` carries.
fn endpoint_config_arn(name: &str) -> String {
    format!("arn:aws:sagemaker:{REGION}:{ACCOUNT_ID}:endpoint-config/{name}")
}

/// The trust document the role a model runs as carries: SageMaker assumes the role, so its own
/// service principal is the one admitted.
fn sagemaker_trust_document() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "sagemaker.amazonaws.com" },
            "Action": "sts:AssumeRole"
        }]
    })
}

/// One row of the table: a template, and what the rule says the stack must then have created
/// in SageMaker.
struct Row {
    case: &'static str,
    /// The template body, built from the stack name the row runs under.
    template: fn(&str) -> String,
    /// The parameters the create supplies.
    parameters: &'static [(&'static str, &'static str)],
    /// The endpoint's name where the row fixes it, and `None` where the template leaves it for
    /// CloudFormation to generate — in which case the stack's own output is the only way to
    /// learn it.
    expected_endpoint_name: fn(&str) -> Option<String>,
    /// Whether the configuration the row declares isolates its model containers, whichever way
    /// it spells the property.
    expected_isolated: bool,
}

/// The rows: one axis per thing the rule generalises over.
const ROWS: &[Row] = &[
    Row {
        case: "literal names, the endpoint naming its configuration by Ref, JSON's own boolean",
        template: |stack| {
            json!({
                "Resources": {
                    "ModelRole": {
                        "Type": "AWS::IAM::Role",
                        "Properties": {
                            "RoleName": format!("{stack}-role"),
                            "AssumeRolePolicyDocument": sagemaker_trust_document()
                        }
                    },
                    "Model": {
                        "Type": "AWS::SageMaker::Model",
                        "Properties": {
                            "ModelName": format!("{stack}-model"),
                            "PrimaryContainer": { "Image": CONTAINER_IMAGE },
                            "ExecutionRoleArn": { "Fn::GetAtt": ["ModelRole", "Arn"] }
                        }
                    },
                    "Config": {
                        "Type": "AWS::SageMaker::EndpointConfig",
                        "Properties": {
                            "EndpointConfigName": format!("{stack}-config"),
                            "EnableNetworkIsolation": true,
                            "ProductionVariants": [{
                                "VariantName": VARIANT,
                                "ModelName": { "Fn::GetAtt": ["Model", "ModelName"] },
                                "InstanceType": "ml.m5.large",
                                "InitialInstanceCount": 1,
                                "InitialVariantWeight": 1
                            }]
                        }
                    },
                    "Endpoint": {
                        "Type": "AWS::SageMaker::Endpoint",
                        "Properties": {
                            "EndpointName": format!("{stack}-endpoint"),
                            "EndpointConfigName": { "Ref": "Config" }
                        }
                    }
                },
                "Outputs": {
                    "ModelName": { "Value": { "Ref": "Model" } },
                    "ConfigName": { "Value": { "Ref": "Config" } },
                    "EndpointName": { "Value": { "Fn::GetAtt": ["Endpoint", "EndpointName"] } },
                    "RoleArn": { "Value": { "Fn::GetAtt": ["ModelRole", "Arn"] } }
                }
            })
            .to_string()
        },
        parameters: &[],
        expected_endpoint_name: |stack| Some(format!("{stack}-endpoint")),
        expected_isolated: true,
    },
    Row {
        case: "generated names, the isolation switch as the text a parameter resolves to",
        template: |_| {
            json!({
                "Parameters": { "Isolated": { "Type": "String", "Default": "false" } },
                "Resources": {
                    "ModelRole": {
                        "Type": "AWS::IAM::Role",
                        "Properties": {
                            "AssumeRolePolicyDocument": sagemaker_trust_document()
                        }
                    },
                    "Model": {
                        "Type": "AWS::SageMaker::Model",
                        "Properties": {
                            "PrimaryContainer": { "Image": CONTAINER_IMAGE },
                            "ExecutionRoleArn": { "Fn::GetAtt": ["ModelRole", "Arn"] }
                        }
                    },
                    "Config": {
                        "Type": "AWS::SageMaker::EndpointConfig",
                        "Properties": {
                            "EnableNetworkIsolation": { "Ref": "Isolated" },
                            "ProductionVariants": [{
                                "VariantName": VARIANT,
                                "ModelName": { "Ref": "Model" },
                                "InstanceType": "ml.c5.xlarge",
                                "InitialInstanceCount": 1
                            }]
                        }
                    },
                    "Endpoint": {
                        "Type": "AWS::SageMaker::Endpoint",
                        "Properties": {
                            "EndpointConfigName": { "Fn::GetAtt": ["Config", "EndpointConfigName"] }
                        }
                    }
                },
                "Outputs": {
                    "ModelName": { "Value": { "Ref": "Model" } },
                    "ConfigName": { "Value": { "Ref": "Config" } },
                    "EndpointName": { "Value": { "Ref": "Endpoint" } },
                    "RoleArn": { "Value": { "Fn::GetAtt": ["ModelRole", "Arn"] } }
                }
            })
            .to_string()
        },
        parameters: &[("Isolated", "false")],
        expected_endpoint_name: |_| None,
        expected_isolated: false,
    },
];

/// RULE: a stack's SageMaker endpoint estate is created in SageMaker, so every name the stack
/// publishes resolves there, the endpoint the stack reports created is `InService` and takes an
/// inference component, and the stack's deletion takes all three away.
#[tokio::test]
async fn a_stack_provisions_the_endpoint_it_declares() {
    let router = build_router();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let stack_name = format!("endpoint-estate-{index}");
        let template = (row.template)(&stack_name);
        let stack_id = create_stack(&router, case, &stack_name, &template, row.parameters).await;

        // `CreateEndpoint` answers the endpoint `Creating`, so the stack's creation is still
        // open when that call answers: the case waits for the CONDITION the rule states.
        let body = stack_resting_in(&router, case, &stack_name, "CREATE_COMPLETE").await;
        let outputs = outputs_of(&body);
        let model = outputs
            .get("ModelName")
            .unwrap_or_else(|| panic!("case {case:?}: no ModelName output in {body}"))
            .clone();
        let config = outputs
            .get("ConfigName")
            .unwrap_or_else(|| panic!("case {case:?}: no ConfigName output in {body}"))
            .clone();
        let endpoint = outputs
            .get("EndpointName")
            .unwrap_or_else(|| panic!("case {case:?}: no EndpointName output in {body}"))
            .clone();
        match (row.expected_endpoint_name)(&stack_name) {
            Some(expected) => assert_eq!(
                endpoint, expected,
                "case {case:?}: expected the stack to name the endpoint {expected:?}, got \
                 {endpoint:?}"
            ),
            None => {
                let prefix = format!("{stack_name}-Endpoint-");
                assert!(
                    endpoint.starts_with(&prefix),
                    "case {case:?}: expected a generated name starting {prefix:?}, got \
                     {endpoint:?}"
                );
            }
        }

        // The endpoint configuration exists in SageMaker, under the name the stack published,
        // hosting the model of the same stack on the ML compute the declaration asked for.
        let (status, described_config) = sagemaker(
            &router,
            "DescribeEndpointConfig",
            &json!({ "EndpointConfigName": config }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeEndpointConfig for {config:?}, got \
             {status} with body {described_config}"
        );
        assert_eq!(
            described_config["EndpointConfigArn"].as_str(),
            Some(endpoint_config_arn(&config).as_str()),
            "case {case:?}: expected the configuration's own ARN, got {described_config}"
        );
        assert_eq!(
            described_config["ProductionVariants"][0]["ModelName"].as_str(),
            Some(model.as_str()),
            "case {case:?}: expected the variant to host the stack's own model, got \
             {described_config}"
        );
        assert_eq!(
            described_config["EnableNetworkIsolation"].as_bool(),
            Some(row.expected_isolated),
            "case {case:?}: expected the declared network isolation, got {described_config}"
        );

        // The endpoint exists too, deployed from that configuration and `InService` — which is
        // what the stack's own completion says about it.
        let (status, described) = sagemaker(
            &router,
            "DescribeEndpoint",
            &json!({ "EndpointName": endpoint }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeEndpoint for {endpoint:?}, got \
             {status} with body {described}"
        );
        assert_eq!(
            described["EndpointArn"].as_str(),
            Some(endpoint_arn(&endpoint).as_str()),
            "case {case:?}: expected the endpoint's own ARN, got {described}"
        );
        assert_eq!(
            described["EndpointStatus"].as_str(),
            Some("InService"),
            "case {case:?}: expected the completed stack to leave the endpoint InService, got \
             {described}"
        );
        assert_eq!(
            described["EndpointConfigName"].as_str(),
            Some(config.as_str()),
            "case {case:?}: expected the endpoint to report the stack's own configuration, got \
             {described}"
        );

        // And a client can USE the name the stack published: an inference component attaches to
        // the endpoint by that name.
        let component = format!("{stack_name}-component");
        let (status, attached) = sagemaker(
            &router,
            "CreateInferenceComponent",
            &json!({
                "InferenceComponentName": component,
                "EndpointName": endpoint,
                "VariantName": VARIANT,
                "Specification": {
                    "ModelName": model,
                    "ComputeResourceRequirements": { "MinMemoryRequiredInMb": 1024 }
                },
                "RuntimeConfig": { "CopyCount": 1 }
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected an inference component to attach to the stack's endpoint, \
             got {status} with body {attached}"
        );

        // Deleting the stack deletes what it created, in the order the dependency demands: the
        // endpoint cannot go while its configuration is still deployed, so the stack waits for
        // SageMaker to free it.
        let (status, deleted) =
            cloudformation(&router, "DeleteStack", &[("StackName", &stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {deleted}"
        );
        stack_resting_in(&router, case, &stack_id, "DELETE_COMPLETE").await;
        for (operation, request, subject) in [
            (
                "DescribeEndpoint",
                json!({ "EndpointName": endpoint }),
                "the endpoint",
            ),
            (
                "DescribeEndpointConfig",
                json!({ "EndpointConfigName": config }),
                "the endpoint configuration",
            ),
            (
                "DescribeModel",
                json!({ "ModelName": model }),
                "the model",
            ),
        ] {
            let (status, gone) = sagemaker(&router, operation, &request).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected {subject} to be gone after the stack's deletion, got \
                 {status} with body {gone}"
            );
        }
    }
}

/// RULE: a resource the owning service refuses, or a declaration carrying a property no call
/// this emulator makes applies, is never passed off as created — the stack fails and leaves
/// nothing behind in SageMaker.
///
/// The rows differ along WHY the resource cannot be created: an endpoint naming a configuration
/// SageMaker holds none of, and an endpoint declaring a property that belongs to
/// `UpdateEndpoint` rather than to the create.
#[tokio::test]
async fn an_endpoint_that_cannot_be_created_fails_the_stack() {
    let router = build_router();

    let rows: &[(&str, Value)] = &[
        (
            "a configuration SageMaker holds none of",
            json!({ "EndpointConfigName": "no-such-endpoint-config" }),
        ),
        (
            "a property no call this emulator makes applies",
            json!({
                "EndpointConfigName": "no-such-endpoint-config",
                "RetainAllVariantProperties": true
            }),
        ),
    ];

    for (index, (case, properties)) in rows.iter().enumerate() {
        let stack_name = format!("doomed-endpoint-{index}");
        let endpoint_name = format!("{stack_name}-endpoint");
        let mut declared = properties.clone();
        declared["EndpointName"] = json!(endpoint_name);
        let template = json!({
            "Resources": {
                "Endpoint": {
                    "Type": "AWS::SageMaker::Endpoint",
                    "Properties": declared
                }
            },
            "Outputs": { "EndpointName": { "Value": { "Ref": "Endpoint" } } }
        });
        create_stack(&router, case, &stack_name, &template.to_string(), &[]).await;
        stack_resting_in(&router, case, &stack_name, "ROLLBACK_COMPLETE").await;

        let (status, gone) = sagemaker(
            &router,
            "DescribeEndpoint",
            &json!({ "EndpointName": endpoint_name }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the refused endpoint never to exist, got {status} with \
             body {gone}"
        );
    }
}
