// SPDX-License-Identifier: CC0-1.0

//! A stack update that changes a property the owning service FIXES when it creates the
//! resource is carried out by REPLACING that resource, not refused — driven end to end over
//! the real wire path (CloudFormation, Amazon SageMaker AI, Amazon S3 and IAM on ONE shared
//! engine, as the production router mounts every service).
//!
//! The rule under test: **a resource of a type whose owning service publishes no call that
//! changes it has every property create-only, so a declaration restating one asks for a
//! DIFFERENT resource — CloudFormation creates the resource the new declaration asks for,
//! publishes its identifier through `Ref` and the stack's outputs, deletes the one it
//! superseded, and the stack reaches `UPDATE_COMPLETE`.** SageMaker publishes no `UpdateModel`
//! and no `UpdateEndpointConfig` (`smithy_query(query="list_operations")` on its model lists
//! neither), so re-pointing a model at a newly trained artifact and re-sizing an endpoint
//! configuration's variant are both replacements; afterwards the superseded resource is gone,
//! which that service answers `ValidationException` for.
//!
//! Two crates have to agree for that to hold (CloudFormation plans and makes the calls,
//! SageMaker serves them), so the suite lives here rather than in either of them.
//!
//! The rows span what the rule generalises over: which TYPE is replaced and which of its
//! create-only properties the revision restates — a model's container artifact, a model's
//! execution role, and an endpoint configuration's production variant.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_provisions_stack_resources::{outputs_of, xml_text};
use crate::query_wire::form_encode;

/// The region every request here is signed for.
const REGION: &str = "us-west-2";

/// The account id every ARN this emulator mints carries.
const ACCOUNT_ID: &str = "000000000000";

/// The bucket the model artifacts live in, and the two artifacts a revision points at.
const ARTIFACT_BUCKET: &str = "model-artifacts-bucket";
const FIRST_ARTIFACT_KEY: &str = "model-artifacts/v1/model.tar.gz";
const SECOND_ARTIFACT_KEY: &str = "model-artifacts/v2/model.tar.gz";

/// How long a case waits for a stack to reach the status the rule says it reaches.
///
/// GENEROUS on purpose: what is under test is WHICH status the stack reaches, never how fast
/// it gets there, so the case polls for the condition rather than sleeping for a span.
const STATUS_DEADLINE: Duration = Duration::from_secs(60);

/// How long the poll waits between two reads of the stack.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The emulator with the four services this suite exercises, on ONE engine.
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
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: this router runs with authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// One awsQuery call to the service `service` names.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    host: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One CloudFormation operation.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(
        router,
        "cloudformation",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        "2010-05-15",
        action,
        members,
    )
    .await
}

/// One IAM operation.
async fn iam(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> (u16, String) {
    query(
        router,
        "iam",
        "iam.amazonaws.com",
        "2010-05-08",
        action,
        members,
    )
    .await
}

/// One SageMaker operation over its awsJson1_1 wire.
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

/// One raw path-style request to the emulator's own Amazon S3 front door.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://s3.{REGION}.amazonaws.com{path}"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3"))
        .body(Bytes::copy_from_slice(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// Hold the bucket and both artifacts in Amazon S3, so each revision's `ModelDataUrl` names an
/// object the role may read — which is what `CreateModel` settles before it registers a model.
async fn given_artifacts(router: &MultiServiceRouter) {
    // A bucket created through a Region's own endpoint states that Region as its location
    // constraint ("If you don't specify a Region, the bucket is created in the US East (N.
    // Virginia) Region"), which is what `CreateBucketConfiguration` carries.
    let configuration = format!(
        "<CreateBucketConfiguration><LocationConstraint>{REGION}</LocationConstraint>\
         </CreateBucketConfiguration>"
    );
    let (status, body) = s3(
        router,
        "PUT",
        &format!("/{ARTIFACT_BUCKET}"),
        configuration.as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "the artifact bucket must be created, got {status} {body}"
    );
    for key in [FIRST_ARTIFACT_KEY, SECOND_ARTIFACT_KEY] {
        let (status, body) = s3(
            router,
            "PUT",
            &format!("/{ARTIFACT_BUCKET}/{key}"),
            b"a model artifact",
        )
        .await;
        assert_eq!(
            status, 200,
            "the artifact {key} must be stored, got {status} {body}"
        );
    }
}

/// Hold `name` in IAM, trusting SageMaker and granted the artifact reads a model's deployment
/// makes, and answer its ARN.
async fn given_role(router: &MultiServiceRouter, name: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "sagemaker.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let (status, body) = iam(
        router,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    assert_eq!(status, 200, "the role {name} must be held, got {status} {body}");
    let grant = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["s3:GetObject", "s3:ListBucket"],
            "Resource": [
                format!("arn:aws:s3:::{ARTIFACT_BUCKET}"),
                format!("arn:aws:s3:::{ARTIFACT_BUCKET}/*"),
            ],
        }],
    })
    .to_string();
    let (status, body) = iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "artifact-read"),
            ("PolicyDocument", &grant),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the role {name} must carry its grant, got {status} {body}"
    );
    format!("arn:aws:iam::{ACCOUNT_ID}:role/{name}")
}

/// The parameters one deployment supplies, flattened the way an SDK flattens the list.
fn flattened(parameters: &[(&str, &str)]) -> Vec<(String, String)> {
    parameters
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
        .collect()
}

/// Deploy `template` as `stack` — a create or an update, as `action` names — with
/// `parameters`.
async fn deploy(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    stack: &str,
    template: &str,
    parameters: &[(&str, &str)],
) {
    let owned = flattened(parameters);
    let mut members: Vec<(&str, &str)> = vec![("StackName", stack), ("TemplateBody", template)];
    for (key, value) in &owned {
        members.push((key.as_str(), value.as_str()));
    }
    let (status, body) = cloudformation(router, action, &members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
}

/// Read the stack until it reports `wanted`, and answer the body that did.
async fn stack_resting_in(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    wanted: &str,
) -> String {
    let deadline = Instant::now() + STATUS_DEADLINE;
    loop {
        let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with {body}"
        );
        let reported = xml_text(&body, "StackStatus").unwrap_or_default();
        if reported == wanted {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the stack to reach {wanted:?}, got {reported:?} with body \
             {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The value of the stack output `name`.
fn output(case: &str, described: &str, name: &str) -> String {
    outputs_of(described)
        .get(name)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no output {name} in {described}"))
}

/// The image a declared model container is pulled from.
fn container_image() -> String {
    format!("{ACCOUNT_ID}.dkr.ecr.{REGION}.amazonaws.com/inference:1")
}

/// The `s3://` URI of one artifact key, as the template resolves it.
fn artifact_uri(key: &str) -> String {
    format!("s3://{ARTIFACT_BUCKET}/{key}")
}

/// The template a MODEL stack is deployed from: one `AWS::SageMaker::Model` whose artifact and
/// execution role come from parameters, and whose name CloudFormation generates — so a
/// replacement is created beside the model the stack has, under a name of its own.
fn model_template() -> String {
    json!({
        "Parameters": {
            "ModelDataKey": { "Type": "String" },
            "RoleArn": { "Type": "String" },
        },
        "Resources": {
            "Inference": {
                "Type": "AWS::SageMaker::Model",
                "Properties": {
                    "ExecutionRoleArn": { "Ref": "RoleArn" },
                    "PrimaryContainer": {
                        "Image": container_image(),
                        "ModelDataUrl": {
                            "Fn::Join": [
                                "",
                                [format!("s3://{ARTIFACT_BUCKET}/"), { "Ref": "ModelDataKey" }],
                            ],
                        },
                    },
                },
            },
        },
        "Outputs": {
            "ModelName": { "Value": { "Ref": "Inference" } },
        },
    })
    .to_string()
}

/// The `ModelDataUrl` SageMaker reports for the model `name`, and the refusal it answers for a
/// name it holds no model under.
async fn described_model(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    sagemaker(router, "DescribeModel", &json!({ "ModelName": name })).await
}

/// RULE: a revision that restates a create-only property of an `AWS::SageMaker::Model`
/// REPLACES the model — the stack reaches `UPDATE_COMPLETE`, publishes the new model's name,
/// that model reports the new declaration, and the model it superseded is gone.
#[tokio::test]
async fn a_model_stack_update_replaces_the_model() {
    /// One row: which create-only property the revision restates, the parameters each
    /// deployment supplies, and the `ModelDataUrl` the replacement must report.
    struct Row {
        case: &'static str,
        /// The parameters of the create and of the update.
        created_with: fn(&str) -> Vec<(String, String)>,
        updated_with: fn(&str) -> Vec<(String, String)>,
        /// What the replacement's `PrimaryContainer.ModelDataUrl` must be.
        replacement_artifact: &'static str,
    }

    let rows: &[Row] = &[
        Row {
            // The issue's own case: a model re-pointed at a newly trained artifact.
            case: "the artifact the container deploys",
            created_with: |role| {
                vec![
                    ("ModelDataKey".to_string(), FIRST_ARTIFACT_KEY.to_string()),
                    ("RoleArn".to_string(), role.to_string()),
                ]
            },
            updated_with: |role| {
                vec![
                    ("ModelDataKey".to_string(), SECOND_ARTIFACT_KEY.to_string()),
                    ("RoleArn".to_string(), role.to_string()),
                ]
            },
            replacement_artifact: SECOND_ARTIFACT_KEY,
        },
        Row {
            // The same rule over ANOTHER create-only property: the role the model's
            // containers run as, with the artifact left where it was.
            case: "the execution role the containers run as",
            created_with: |role| {
                vec![
                    ("ModelDataKey".to_string(), FIRST_ARTIFACT_KEY.to_string()),
                    ("RoleArn".to_string(), role.to_string()),
                ]
            },
            updated_with: |_| {
                vec![
                    ("ModelDataKey".to_string(), FIRST_ARTIFACT_KEY.to_string()),
                    (
                        "RoleArn".to_string(),
                        format!("arn:aws:iam::{ACCOUNT_ID}:role/second-model-role"),
                    ),
                ]
            },
            replacement_artifact: FIRST_ARTIFACT_KEY,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let router = build_router();
        given_artifacts(&router).await;
        let role = given_role(&router, "first-model-role").await;
        let second_role = given_role(&router, "second-model-role").await;
        assert_ne!(
            role, second_role,
            "case {:?}: the two roles must differ",
            row.case
        );
        let stack = format!("model-replacement-{index}");
        let template = model_template();

        let created = (row.created_with)(&role);
        let created: Vec<(&str, &str)> = created
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        deploy(
            &router,
            row.case,
            "CreateStack",
            &stack,
            &template,
            &created,
        )
        .await;
        let described = stack_resting_in(&router, row.case, &stack, "CREATE_COMPLETE").await;
        let first_model = output(row.case, &described, "ModelName");
        let (status, reported) = described_model(&router, &first_model).await;
        assert_eq!(
            status, 200,
            "case {:?}: the created model must be described, got {status} with {reported}",
            row.case
        );
        let reported_artifact = reported
            .pointer("/PrimaryContainer/ModelDataUrl")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            reported_artifact,
            artifact_uri(FIRST_ARTIFACT_KEY),
            "case {:?}: the created model must report the artifact it was created with, got \
             {reported}",
            row.case
        );

        let updated = (row.updated_with)(&role);
        let updated: Vec<(&str, &str)> = updated
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        deploy(
            &router,
            row.case,
            "UpdateStack",
            &stack,
            &template,
            &updated,
        )
        .await;
        let described = stack_resting_in(&router, row.case, &stack, "UPDATE_COMPLETE").await;

        // A REPLACEMENT: the stack publishes a model name of its own, which is the one
        // SageMaker now holds the new declaration under.
        let second_model = output(row.case, &described, "ModelName");
        assert_ne!(
            second_model, first_model,
            "case {:?}: the replacement must carry a name of its own, got {second_model} with \
             {described}",
            row.case
        );
        let (status, reported) = described_model(&router, &second_model).await;
        assert_eq!(
            status, 200,
            "case {:?}: the replacement must be described, got {status} with {reported}",
            row.case
        );
        let reported_artifact = reported
            .pointer("/PrimaryContainer/ModelDataUrl")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            reported_artifact,
            artifact_uri(row.replacement_artifact),
            "case {:?}: the replacement must report the declaration the update asked for, got \
             {reported}",
            row.case
        );

        // ...and the model it superseded is gone, which SageMaker answers for a name it holds
        // no model under.
        let (status, refused) = described_model(&router, &first_model).await;
        assert_eq!(
            status, 400,
            "case {:?}: the superseded model must be gone, got {status} with {refused}",
            row.case
        );
        let message = refused
            .get("message")
            .or_else(|| refused.get("Message"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            message,
            format!("Could not find model \"{first_model}\"."),
            "case {:?}: expected the refusal to name the superseded model, got {refused}",
            row.case
        );
    }
}

/// The template an ENDPOINT CONFIGURATION stack is deployed from: a named model and a
/// configuration whose variant's instance type comes from a parameter, with the
/// configuration's own name left for CloudFormation to generate.
fn endpoint_config_template() -> String {
    json!({
        "Parameters": {
            "InstanceType": { "Type": "String" },
            "RoleArn": { "Type": "String" },
        },
        "Resources": {
            "Inference": {
                "Type": "AWS::SageMaker::Model",
                "Properties": {
                    "ModelName": "hosted-model",
                    "ExecutionRoleArn": { "Ref": "RoleArn" },
                    "PrimaryContainer": {
                        "Image": container_image(),
                        "ModelDataUrl": artifact_uri(FIRST_ARTIFACT_KEY),
                    },
                },
            },
            "Hosting": {
                "Type": "AWS::SageMaker::EndpointConfig",
                "Properties": {
                    "ProductionVariants": [{
                        "VariantName": "AllTraffic",
                        "ModelName": { "Fn::GetAtt": ["Inference", "ModelName"] },
                        "InitialInstanceCount": 1,
                        "InitialVariantWeight": 1,
                        "InstanceType": { "Ref": "InstanceType" },
                    }],
                },
            },
        },
        "Outputs": {
            "ConfigName": { "Value": { "Ref": "Hosting" } },
        },
    })
    .to_string()
}

/// RULE: the same rule over the other SageMaker type whose owning service publishes no update
/// for it — a revision that re-sizes an `AWS::SageMaker::EndpointConfig`'s production variant
/// replaces the configuration, and the one it superseded is gone.
#[tokio::test]
async fn an_endpoint_config_stack_update_replaces_the_configuration() {
    let case = "the production variant's instance type";
    let router = build_router();
    given_artifacts(&router).await;
    let role = given_role(&router, "hosting-role").await;
    let stack = "endpoint-config-replacement";
    let template = endpoint_config_template();

    deploy(
        &router,
        case,
        "CreateStack",
        stack,
        &template,
        &[("InstanceType", "ml.m5.large"), ("RoleArn", &role)],
    )
    .await;
    let described = stack_resting_in(&router, case, stack, "CREATE_COMPLETE").await;
    let first_config = output(case, &described, "ConfigName");

    deploy(
        &router,
        case,
        "UpdateStack",
        stack,
        &template,
        &[("InstanceType", "ml.m5.xlarge"), ("RoleArn", &role)],
    )
    .await;
    let described = stack_resting_in(&router, case, stack, "UPDATE_COMPLETE").await;
    let second_config = output(case, &described, "ConfigName");
    assert_ne!(
        second_config, first_config,
        "case {case:?}: the replacement must carry a name of its own, got {described}"
    );

    let (status, reported) = sagemaker(
        &router,
        "DescribeEndpointConfig",
        &json!({ "EndpointConfigName": second_config }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the replacement must be described, got {status} with {reported}"
    );
    let instance_type = reported
        .pointer("/ProductionVariants/0/InstanceType")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert_eq!(
        instance_type, "ml.m5.xlarge",
        "case {case:?}: the replacement must report the variant the update asked for, got \
         {reported}"
    );

    let (status, refused) = sagemaker(
        &router,
        "DescribeEndpointConfig",
        &json!({ "EndpointConfigName": first_config }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: the superseded configuration must be gone, got {status} with {refused}"
    );
    let message = refused
        .get("message")
        .or_else(|| refused.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert_eq!(
        message,
        format!("Could not find endpoint configuration \"{first_config}\"."),
        "case {case:?}: expected the refusal to name the superseded configuration, got {refused}"
    );
}
