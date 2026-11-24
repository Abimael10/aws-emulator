// SPDX-License-Identifier: CC0-1.0

//! The toolkit bootstrap stack a deploying tool installs under a qualifier: every resource its
//! template declares is REAL in the service that owns it once the stack completes.
//!
//! The rule under test: **a stack that reaches `CREATE_COMPLETE` has created each resource its
//! `Resources` section declares through the owning service's own create API, under the physical
//! name the template's intrinsics resolved to — so each one answers that service's own read
//! afterwards, while a name no deployment created is still answered as not found.** The
//! bootstrap template is the case that spans the most owners at once: a staging bucket in
//! Amazon S3, a container-assets repository in the private Amazon ECR registry, five roles in
//! IAM and the version parameter in AWS Systems Manager, every one of them named by an
//! `Fn::Sub` over a stack parameter and the account and Region pseudo parameters.
//!
//! Five crates have to agree for that to hold — CloudFormation makes the calls, and S3, Amazon
//! ECR, IAM and Systems Manager each own one of the resources and are the only side that can
//! say whether it exists — so the suite lives here: a CloudFormation-only test sees the
//! reported resource status and the published name, never whether anything is behind them.
//!
//! The rows differ along what the rule generalises over: the qualifier the bootstrap is
//! installed under (the toolkit's own default, a custom one, a short one), the Region the
//! stack deploys in — which every one of those names carries — and whether the repository's
//! name comes from the branch of the template's `Fn::If` that composes it or from the one that
//! takes it from a parameter of the deployment. Each row also reads a repository name no
//! bootstrap created, which is the only case Amazon ECR answers `RepositoryNotFoundException`
//! for.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode, opt_element, outputs_of, pairs_of, physical_ids_of};

/// The account id every resource this emulator holds belongs to, and so the registry id Amazon
/// ECR reports (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The `X-Amz-Target` prefixes the two awsJson1.1 services here fix (Amazon ECR's service
/// shape `AmazonEC2ContainerRegistry_V20150921`, Systems Manager's `AmazonSSM`).
const ECR_TARGET_PREFIX: &str = "AmazonEC2ContainerRegistry_V20150921";
const SSM_TARGET_PREFIX: &str = "AmazonSSM";

/// The API version IAM's awsQuery wire carries (`iam` 2010-05-08), beside CloudFormation's.
const IAM_VERSION: &str = "2010-05-08";
const CLOUDFORMATION_VERSION: &str = "2010-05-15";

/// How a stack read waits for the deployment's clock-owed span to come due: look again, with a
/// deadline generous enough that a loaded host still reaches it.
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const POLL_ATTEMPTS: usize = 160;

/// The version the bootstrap template writes into its parameter. A number of this template's
/// own, not of any service's: the value is checked back out of Systems Manager, so what matters
/// is that the deployment stored the one the template declared.
const BOOTSTRAP_VERSION: &str = "21";

/// The image tag mutability the bootstrap template gives its container-assets repository, so a
/// published asset tag cannot be overwritten.
const IMAGE_TAG_MUTABILITY: &str = "IMMUTABLE";

/// The lifecycle policy that template installs on the same repository, as one document.
const LIFECYCLE_POLICY_TEXT: &str = concat!(
    r#"{"rules":[{"rulePriority":1,"description":"Untagged images should not exist, "#,
    r#"but expire any older than one year","selection":{"tagStatus":"untagged","#,
    r#""countType":"sinceImagePushed","countUnit":"days","countNumber":365},"#,
    r#""action":{"type":"expire"}}]}"#,
);

/// The tag the bootstrap template puts on the container-assets repository, naming the role that
/// publishes to it.
const BOOTSTRAP_ROLE_TAG: (&str, &str) = ("aws-cdk:bootstrap-role", "image-publishing");

/// The five roles the bootstrap stack creates: the logical id each is declared under, against
/// the infix its name carries between the qualifier and the account and Region.
const BOOTSTRAP_ROLES: &[(&str, &str)] = &[
    ("FilePublishingRole", "file-publishing-role"),
    ("ImagePublishingRole", "image-publishing-role"),
    ("LookupRole", "lookup-role"),
    ("DeploymentActionRole", "deploy-role"),
    ("CloudFormationExecutionRole", "cfn-exec-role"),
];

/// The logical ids of the other three resources the template declares.
const BUCKET_LOGICAL_ID: &str = "StagingBucket";
const REPOSITORY_LOGICAL_ID: &str = "ContainerAssetsRepository";
const VERSION_LOGICAL_ID: &str = "CdkBootstrapVersion";

/// The terminal status a created stack and each of its created resources reports.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The object a file-publishing step would put in the staging bucket, which is read back to
/// show the bucket the stack created really serves data.
const ASSET_KEY: &str = "assets/0123456789abcdef.zip";
const ASSET_BODY: &[u8] = b"a published file asset";

/// The five services this rule spans, over ONE engine exactly as `core/router/src/lib.rs`
/// mounts them.
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
        service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` in `region`,
/// which is what routes the request and which Region it is made in. The signature is never
/// checked: this router runs with request authentication off.
fn authorization(signing_name: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Post one awsQuery form body to `signing_name` in `region`.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    region: &str,
    version: &str,
    action: &str,
    members: &[(&str, String)],
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
        .header("host", format!("{signing_name}.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name, region))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one CloudFormation operation over its awsQuery wire, in `region`.
async fn cloudformation(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    members: &[(&str, String)],
) -> (u16, String) {
    query(
        router,
        "cloudformation",
        region,
        CLOUDFORMATION_VERSION,
        action,
        members,
    )
    .await
}

/// Call one IAM operation over its awsQuery wire, in `region`.
async fn iam(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    members: &[(&str, String)],
) -> (u16, String) {
    query(router, "iam", region, IAM_VERSION, action, members).await
}

/// Call one awsJson1.1 operation of the service addressed by `signing_name`, in `region`.
async fn json_rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    region: &str,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name, region))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let payload: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, payload)
}

/// Call one Amazon ECR operation, in `region`.
async fn ecr(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    json_rpc(router, "ecr", ECR_TARGET_PREFIX, region, operation, request).await
}

/// Call one Systems Manager operation, in `region`.
async fn ssm(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    json_rpc(router, "ssm", SSM_TARGET_PREFIX, region, operation, request).await
}

/// One Amazon S3 request, addressed PATH-STYLE against the Region's endpoint.
async fn s3(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    path: &str,
    body: &[u8],
) -> (u16, Bytes) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("s3.{region}.amazonaws.com"))
        .header("content-type", "application/octet-stream")
        .header("authorization", authorization("s3", region))
        .body(Bytes::copy_from_slice(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, response.into_body())
}

/// The error code an awsJson answer carries, without the namespace a `__type` may qualify it
/// with.
fn error_code(body: &Value) -> &str {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or("<no __type>")
}

/// The message an awsJson answer carries, under either spelling of that member.
fn error_message(body: &Value) -> &str {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
}

/// One bootstrap installation: the qualifier it is installed under, the Region it is installed
/// in, and whether the deployment states the repository's name itself.
#[derive(Debug)]
struct Row {
    case: &'static str,
    /// The value the deployment gives the template's `Qualifier` parameter.
    qualifier: &'static str,
    region: &'static str,
    stack: &'static str,
    /// The name the deployment asks the container-assets repository to carry, which the
    /// template's `Fn::If` prefers over the name it composes; `None` leaves the composed name
    /// in force.
    stated_repository_name: Option<&'static str>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // The qualifier the toolkit uses when a client names none.
            case: "the toolkit's default qualifier, in us-east-1",
            qualifier: "hnb659fds",
            region: "us-east-1",
            stack: "CDKToolkit",
            stated_repository_name: None,
        },
        Row {
            // A custom qualifier, in another Region: every name below carries both, so a stack
            // that composed them from anything but the deployment's own parameter and pseudo
            // parameters would address resources nobody created.
            case: "a custom qualifier, in eu-west-1",
            qualifier: "zqx7bk3m1",
            region: "eu-west-1",
            stack: "CDKToolkit-custom-qualifier",
            stated_repository_name: None,
        },
        Row {
            // The shortest qualifier a client can pass, in a third Region, with the
            // repository's name stated by the deployment rather than composed.
            case: "a short qualifier and a stated repository name, in ap-southeast-2",
            qualifier: "q1",
            region: "ap-southeast-2",
            stack: "CDKToolkit-stated-repository",
            stated_repository_name: Some("stated-container-assets"),
        },
    ]
}

/// The name the staging bucket carries: the template's `Fn::Sub` over the qualifier and the
/// account and Region pseudo parameters.
fn staging_bucket(row: &Row) -> String {
    format!("cdk-{}-assets-{ACCOUNT}-{}", row.qualifier, row.region)
}

/// The name the container-assets repository carries: what the deployment stated, or the name
/// the template composes when it states none.
fn container_assets_repository(row: &Row) -> String {
    match row.stated_repository_name {
        Some(stated) => stated.to_string(),
        None => format!(
            "cdk-{}-container-assets-{ACCOUNT}-{}",
            row.qualifier, row.region
        ),
    }
}

/// The name one bootstrap role carries, for the infix that names its job.
fn bootstrap_role(row: &Row, infix: &str) -> String {
    format!("cdk-{}-{infix}-{ACCOUNT}-{}", row.qualifier, row.region)
}

/// The parameter the bootstrap version is written into.
fn version_parameter(row: &Row) -> String {
    format!("/cdk-bootstrap/{}/version", row.qualifier)
}

/// A repository name of a qualifier no deployment here installs, which is a repository the
/// registry holds none of however many bootstraps have run.
fn unbootstrapped_repository(row: &Row) -> String {
    format!("cdk-nobootstrap-container-assets-{ACCOUNT}-{}", row.region)
}

/// The trust policy each bootstrap role carries: the account itself may assume it, which is
/// what lets the deploying client take the role on.
fn assume_role_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Action": "sts:AssumeRole",
            "Effect": "Allow",
            "Principal": { "AWS": { "Ref": "AWS::AccountId" } },
        }],
    })
}

/// The resource-based policy the bootstrap template gives its container-assets repository: the
/// one that lets a function image be pulled from it. Written ONCE, so the document the
/// template declares and the document the assertion compares the repository's policy against
/// cannot drift apart.
fn repository_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "LambdaECRImageRetrievalPolicy",
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": ["ecr:BatchGetImage", "ecr:GetDownloadUrlForLayer"],
        }],
    })
}

/// The bootstrap template: the staging bucket, the container-assets repository, the five roles
/// and the version parameter, each named by an `Fn::Sub` over the `Qualifier` parameter and the
/// account and Region pseudo parameters — with the repository's and bucket's names taken from a
/// parameter instead where the deployment states one, as the toolkit's own template does.
fn bootstrap_template() -> String {
    let mut resources = json!({
        BUCKET_LOGICAL_ID: {
            "Type": "AWS::S3::Bucket",
            "Properties": {
                "BucketName": {
                    "Fn::If": [
                        "HasCustomFileAssetsBucketName",
                        { "Fn::Sub": "${FileAssetsBucketName}" },
                        { "Fn::Sub": "cdk-${Qualifier}-assets-${AWS::AccountId}-${AWS::Region}" },
                    ],
                },
                "VersioningConfiguration": { "Status": "Enabled" },
                "PublicAccessBlockConfiguration": {
                    "BlockPublicAcls": true,
                    "BlockPublicPolicy": true,
                    "IgnorePublicAcls": true,
                    "RestrictPublicBuckets": true,
                },
            },
        },
        REPOSITORY_LOGICAL_ID: {
            "Type": "AWS::ECR::Repository",
            "Properties": {
                "RepositoryName": {
                    "Fn::If": [
                        "HasCustomContainerAssetsRepositoryName",
                        { "Fn::Sub": "${ContainerAssetsRepositoryName}" },
                        {
                            "Fn::Sub":
                                "cdk-${Qualifier}-container-assets-${AWS::AccountId}-${AWS::Region}",
                        },
                    ],
                },
                "ImageTagMutability": IMAGE_TAG_MUTABILITY,
                "LifecyclePolicy": { "LifecyclePolicyText": LIFECYCLE_POLICY_TEXT },
                "RepositoryPolicyText": repository_policy(),
                "Tags": [{ "Key": BOOTSTRAP_ROLE_TAG.0, "Value": BOOTSTRAP_ROLE_TAG.1 }],
            },
        },
        VERSION_LOGICAL_ID: {
            "Type": "AWS::SSM::Parameter",
            "Properties": {
                "Type": "String",
                "Name": { "Fn::Sub": "/cdk-bootstrap/${Qualifier}/version" },
                "Value": BOOTSTRAP_VERSION,
            },
        },
    });
    for (logical_id, infix) in BOOTSTRAP_ROLES {
        resources[*logical_id] = json!({
            "Type": "AWS::IAM::Role",
            "Properties": {
                "AssumeRolePolicyDocument": assume_role_policy(),
                "RoleName": {
                    "Fn::Sub": format!(
                        "cdk-${{Qualifier}}-{infix}-${{AWS::AccountId}}-${{AWS::Region}}"
                    ),
                },
            },
        });
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "The toolkit stack this emulator is bootstrapped with",
        "Parameters": {
            "Qualifier": { "Type": "String", "Default": "hnb659fds" },
            "FileAssetsBucketName": { "Type": "String", "Default": "" },
            "ContainerAssetsRepositoryName": { "Type": "String", "Default": "" },
        },
        "Conditions": {
            "HasCustomFileAssetsBucketName": {
                "Fn::Not": [{ "Fn::Equals": [{ "Ref": "FileAssetsBucketName" }, ""] }],
            },
            "HasCustomContainerAssetsRepositoryName": {
                "Fn::Not": [{
                    "Fn::Equals": [{ "Ref": "ContainerAssetsRepositoryName" }, ""],
                }],
            },
        },
        "Resources": resources,
        "Outputs": {
            "BucketName": { "Value": { "Ref": BUCKET_LOGICAL_ID } },
            "ImageRepositoryName": { "Value": { "Ref": REPOSITORY_LOGICAL_ID } },
            "BootstrapVersion": { "Value": { "Fn::GetAtt": [VERSION_LOGICAL_ID, "Value"] } },
        },
    })
    .to_string()
}

/// The stack's own events, which is where the reason a failed deployment stopped is read.
async fn stack_events(router: &MultiServiceRouter, row: &Row) -> String {
    let (_, body) = cloudformation(
        router,
        row.region,
        "DescribeStackEvents",
        &[("StackName", row.stack.to_string())],
    )
    .await;
    body
}

/// Deploy the bootstrap stack the way the toolkit does — `CreateStack` with the qualifier (and
/// the repository's name, where the row states one) as stack parameters — and read it until it
/// reports a status that is no longer in progress. The answer is that read's body.
async fn bootstrap(router: &MultiServiceRouter, row: &Row) -> String {
    let case = row.case;
    let mut members = vec![
        ("StackName", row.stack.to_string()),
        ("TemplateBody", bootstrap_template()),
        // The template names the roles it creates, which is what this capability covers.
        ("Capabilities.member.1", "CAPABILITY_NAMED_IAM".to_string()),
        ("Parameters.member.1.ParameterKey", "Qualifier".to_string()),
        (
            "Parameters.member.1.ParameterValue",
            row.qualifier.to_string(),
        ),
    ];
    if let Some(stated) = row.stated_repository_name {
        members.push((
            "Parameters.member.2.ParameterKey",
            "ContainerAssetsRepositoryName".to_string(),
        ));
        members.push(("Parameters.member.2.ParameterValue", stated.to_string()));
    }
    let (status, body) = cloudformation(router, row.region, "CreateStack", &members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );

    let mut described = String::new();
    for _ in 0..POLL_ATTEMPTS {
        let (status, body) = cloudformation(
            router,
            row.region,
            "DescribeStacks",
            &[("StackName", row.stack.to_string())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        described = body;
        if !opt_element(&described, "StackStatus")
            .unwrap_or_default()
            .ends_with("_IN_PROGRESS")
        {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some(CREATE_COMPLETE),
        "case {case:?}: expected the bootstrap stack to reach {CREATE_COMPLETE}; reason {:?}; \
         events {}",
        opt_element(&described, "StackStatusReason"),
        stack_events(router, row).await,
    );
    described
}

/// The stack's logical-id -> physical-id map beside its logical-id -> resource-status map, which
/// is what a client reads the identifier and the outcome of each declared resource from.
async fn stack_resources(
    router: &MultiServiceRouter,
    row: &Row,
) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let case = row.case;
    let (status, body) = cloudformation(
        router,
        row.region,
        "DescribeStackResources",
        &[("StackName", row.stack.to_string())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with body \
         {body}"
    );
    (
        physical_ids_of(&body),
        pairs_of(&body, "LogicalResourceId", "ResourceStatus"),
    )
}

/// RULE: every resource the bootstrap template declares exists in the service that owns it,
/// under the name the template's intrinsics resolved to, once the stack completes — and a
/// repository name no bootstrap created is still not found.
#[tokio::test]
async fn every_bootstrap_resource_is_real_in_the_service_that_owns_it() {
    for row in rows() {
        // A router of its own per row: every row deploys into the one account its engine holds,
        // and what each case asserts is about resources nothing else has acted on.
        let router = build_router();
        let case = row.case;
        let described = bootstrap(&router, &row).await;
        let outputs = outputs_of(&described);
        let (physical_ids, statuses) = stack_resources(&router, &row).await;

        // ---- What the stack reports: each declared resource created, under its own name. ----
        let bucket = staging_bucket(&row);
        let repository = container_assets_repository(&row);
        let parameter = version_parameter(&row);
        let mut expected: Vec<(&str, String)> = vec![
            (BUCKET_LOGICAL_ID, bucket.clone()),
            (REPOSITORY_LOGICAL_ID, repository.clone()),
            (VERSION_LOGICAL_ID, parameter.clone()),
        ];
        for (logical_id, infix) in BOOTSTRAP_ROLES {
            expected.push((*logical_id, bootstrap_role(&row, infix)));
        }
        for (logical_id, name) in &expected {
            assert_eq!(
                physical_ids.get(*logical_id).map(String::as_str),
                Some(name.as_str()),
                "case {case:?}: expected the stack to report {logical_id:?} as {name:?}, got \
                 {physical_ids:?}"
            );
            assert_eq!(
                statuses.get(*logical_id).map(String::as_str),
                Some(CREATE_COMPLETE),
                "case {case:?}: expected {logical_id:?} to report {CREATE_COMPLETE}, got \
                 {statuses:?}"
            );
        }
        assert_eq!(
            outputs.get("ImageRepositoryName").map(String::as_str),
            Some(repository.as_str()),
            "case {case:?}: expected the stack to publish the repository name {repository:?}, got \
             {outputs:?}"
        );
        assert_eq!(
            outputs.get("BucketName").map(String::as_str),
            Some(bucket.as_str()),
            "case {case:?}: expected the stack to publish the bucket name {bucket:?}, got \
             {outputs:?}"
        );
        assert_eq!(
            outputs.get("BootstrapVersion").map(String::as_str),
            Some(BOOTSTRAP_VERSION),
            "case {case:?}: expected the stack to publish the bootstrap version, got {outputs:?}"
        );

        // ---- Amazon ECR: the container-assets repository the template declared. ----
        let (status, reported) = ecr(
            &router,
            row.region,
            "DescribeRepositories",
            &json!({ "repositoryNames": [repository.clone()] }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ecr DescribeRepositories on {repository:?} to answer 200, \
             got {status} with body {reported}"
        );
        let held = &reported["repositories"][0];
        assert_eq!(
            held["repositoryName"].as_str(),
            Some(repository.as_str()),
            "case {case:?}: expected the bootstrapped repository, got {reported}"
        );
        assert_eq!(
            held["registryId"].as_str(),
            Some(ACCOUNT),
            "case {case:?}: expected the repository in this account's registry, got {reported}"
        );
        assert_eq!(
            held["imageTagMutability"].as_str(),
            Some(IMAGE_TAG_MUTABILITY),
            "case {case:?}: expected the declared tag mutability on the repository, got {reported}"
        );
        assert_eq!(
            held["repositoryArn"].as_str().unwrap_or_default(),
            format!("arn:aws:ecr:{}:{ACCOUNT}:repository/{repository}", row.region),
            "case {case:?}: expected the repository ARN to name this account and Region, got \
             {reported}"
        );
        // A repository the registry really holds answers a read of its images too, with none
        // pushed yet — which a record of a repository nothing created could not.
        let (status, listed) = ecr(
            &router,
            row.region,
            "ListImages",
            &json!({ "repositoryName": repository.clone() }),
        )
        .await;
        assert_eq!(
            (status, listed["imageIds"].as_array().map(Vec::len)),
            (200, Some(0)),
            "case {case:?}: expected ecr ListImages on {repository:?} to answer an empty listing, \
             got {status} with body {listed}"
        );
        // The lifecycle policy the declaration carried is on that repository.
        let (status, policy) = ecr(
            &router,
            row.region,
            "GetLifecyclePolicy",
            &json!({ "repositoryName": repository.clone() }),
        )
        .await;
        assert_eq!(
            (status, policy["lifecyclePolicyText"].as_str()),
            (200, Some(LIFECYCLE_POLICY_TEXT)),
            "case {case:?}: expected the declared lifecycle policy on {repository:?}, got \
             {status} with body {policy}"
        );
        // So is the resource-based policy the declaration carried: the document reaches that
        // registry as its policy TEXT, so it is compared as the document it parses back to and
        // not as one spelling of it.
        let (status, resource_policy) = ecr(
            &router,
            row.region,
            "GetRepositoryPolicy",
            &json!({ "repositoryName": repository.clone() }),
        )
        .await;
        let stated: Option<Value> = resource_policy["policyText"]
            .as_str()
            .and_then(|text| serde_json::from_str(text).ok());
        assert_eq!(
            (status, stated.as_ref()),
            (200, Some(&repository_policy())),
            "case {case:?}: expected the declared repository policy on {repository:?}, got \
             {status} with body {resource_policy}"
        );
        // And the tag the declaration gave it, which that registry keys by the repository's ARN.
        let (status, tagged) = ecr(
            &router,
            row.region,
            "ListTagsForResource",
            &json!({ "resourceArn": held["repositoryArn"].as_str().unwrap_or_default() }),
        )
        .await;
        let carried: Vec<(&str, &str)> = tagged["tags"]
            .as_array()
            .map(|listed| {
                listed
                    .iter()
                    .map(|tag| {
                        (
                            tag["Key"].as_str().unwrap_or_default(),
                            tag["Value"].as_str().unwrap_or_default(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            status, 200,
            "case {case:?}: expected ecr ListTagsForResource on {repository:?} to answer 200, got \
             {status} with body {tagged}"
        );
        assert!(
            carried.contains(&BOOTSTRAP_ROLE_TAG),
            "case {case:?}: expected the declared tag {BOOTSTRAP_ROLE_TAG:?} on {repository:?}, \
             got {carried:?}"
        );

        // A repository name no bootstrap created is the one case that not-found answers.
        let missing = unbootstrapped_repository(&row);
        let (status, refused) = ecr(
            &router,
            row.region,
            "DescribeRepositories",
            &json!({ "repositoryNames": [missing.clone()] }),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused)),
            (400, "RepositoryNotFoundException"),
            "case {case:?}: expected ecr DescribeRepositories on the never-bootstrapped \
             {missing:?} to be refused as not found, got {status} with body {refused}"
        );
        assert_eq!(
            error_message(&refused),
            format!(
                "The repository with name '{missing}' does not exist in the registry with id \
                 '{ACCOUNT}'"
            ),
            "case {case:?}: expected the not-found wording naming {missing:?}, got {refused}"
        );

        // ---- Amazon S3: the staging bucket the template declared, serving data. ----
        let (status, body) = s3(&router, row.region, "HEAD", &format!("/{bucket}"), b"").await;
        assert_eq!(
            status,
            200,
            "case {case:?}: expected s3 HeadBucket on {bucket:?} to answer 200, got {status} with \
             body {}",
            String::from_utf8_lossy(&body)
        );
        let (status, body) = s3(
            &router,
            row.region,
            "PUT",
            &format!("/{bucket}/{ASSET_KEY}"),
            ASSET_BODY,
        )
        .await;
        assert_eq!(
            status,
            200,
            "case {case:?}: expected {bucket:?} to take a published asset, got {status} with body \
             {}",
            String::from_utf8_lossy(&body)
        );
        let (status, body) = s3(
            &router,
            row.region,
            "GET",
            &format!("/{bucket}/{ASSET_KEY}"),
            b"",
        )
        .await;
        assert_eq!(
            (status, body.as_ref()),
            (200, ASSET_BODY),
            "case {case:?}: expected {bucket:?} to serve the published asset back, got {status} \
             with body {}",
            String::from_utf8_lossy(&body)
        );

        // ---- IAM: the five roles the template declared. ----
        for (logical_id, infix) in BOOTSTRAP_ROLES {
            let role = bootstrap_role(&row, infix);
            let (status, body) =
                iam(&router, row.region, "GetRole", &[("RoleName", role.clone())]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected iam GetRole on {role:?} ({logical_id}) to answer 200, \
                 got {status} with body {body}"
            );
            assert_eq!(
                element(&body, "RoleName"),
                role,
                "case {case:?}: expected IAM to report the bootstrapped role {role:?}, got {body}"
            );
            assert_eq!(
                element(&body, "Arn"),
                format!("arn:aws:iam::{ACCOUNT}:role/{role}"),
                "case {case:?}: expected the role ARN of {role:?}, got {body}"
            );
        }

        // ---- Systems Manager: the version parameter the template declared. ----
        let (status, read) = ssm(
            &router,
            row.region,
            "GetParameter",
            &json!({ "Name": parameter.clone() }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ssm GetParameter on {parameter:?} to answer 200, got \
             {status} with body {read}"
        );
        assert_eq!(
            read.pointer("/Parameter/Value").and_then(Value::as_str),
            Some(BOOTSTRAP_VERSION),
            "case {case:?}: expected the bootstrap version in {parameter:?}, got {read}"
        );
    }
}
