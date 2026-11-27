// SPDX-License-Identifier: CC0-1.0

//! A bucket a stack creates carries the DEFAULT ENCRYPTION its declaration stated, even when
//! every member of that declaration is spelled as an intrinsic over a condition, a parameter
//! and an attribute of another resource of the same stack.
//!
//! The rule under test: **the `BucketEncryption` a stack's `AWS::S3::Bucket` declares reaches
//! Amazon S3 as the bucket's whole default-encryption configuration — the `SSEAlgorithm` and
//! the `KMSMasterKeyID` each as the value the template's intrinsics resolved to, and a member
//! that resolved away left out — so the bucket reports exactly that configuration and every
//! later write that names no encryption of its own is stored under it.** Which intrinsic a
//! member was spelled with changes nothing: a value chosen by an `Fn::If`, by an `Fn::If`
//! nested inside another one, or read off an `Fn::GetAtt`/`Fn::Sub` over a key the SAME stack
//! created, is the value a literal in its place would be.
//!
//! That is the declaration a toolkit bootstrap gives its staging bucket when it is installed
//! with a customer-managed key: the stack creates the CMK and points the bucket's default
//! encryption at the key KMS minted for it, so a published asset is stored under that key.
//!
//! Three crates have to agree for the rule to hold — CloudFormation makes the calls, KMS mints
//! the key and is the only side that can resolve it, and Amazon S3 owns the bucket's default
//! encryption and applies it to the writes that inherit it — so the suite lives here rather
//! than in any one of them.
//!
//! The rows differ along what the rule generalises over: which branch the declaration's
//! conditions take (a key the stack creates, a key the deployment names, the managed key that
//! leaves the member resolved away), which algorithm that branch names (`aws:kms`,
//! `aws:kms:dsse`, `AES256`), how deep the chosen value sits (an `Fn::If` branch, an `Fn::If`
//! inside an `Fn::If`), and which identifier form the named key is given (a key ARN and a bare
//! key id). One row deploys through a CHANGE SET rather than `CreateStack`, which is the path a
//! deploying toolkit takes.
//!
//! Every expected value is computed from what the row declares, except the key's identity,
//! which is KMS's to mint: the rows that create one assert that the bucket names the key the
//! stack published and that KMS's own `DescribeKey` resolves it as a customer managed key.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{blocks_of, form_encode, opt_element, outputs_of};

/// The region every request here is signed for, and so the Region the stack's key, bucket and
/// ARNs belong to.
const REGION: &str = "us-east-1";

/// The account every resource this emulator holds belongs to.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// How a read waits for a deployment's clock-owed transition to come due: look again, with a
/// deadline generous enough that a loaded host still reaches it.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const POLL_ATTEMPTS: usize = 300;

/// The header Amazon S3 reports an object's server-side encryption in, and the one it reports
/// the KMS key that encryption used in.
const SSE_HEADER: &str = "x-amz-server-side-encryption";
const SSE_KMS_KEY_HEADER: &str = "x-amz-server-side-encryption-aws-kms-key-id";

/// The `ServerSideEncryption` value a bucket with no SSE-KMS default stores an object under:
/// "by default, all buckets have a default encryption configuration that uses server-side
/// encryption with Amazon S3 managed keys (SSE-S3)" (`GetBucketEncryption`).
const AES256: &str = "AES256";

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
        service_com_amazonaws_kms_trentservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, String)],
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
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one KMS operation over its awsJson1_1 wire.
async fn kms(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("kms.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("TrentService.{operation}"))
        .header("authorization", authorization("kms"))
        .body(Bytes::from(
            serde_json::to_vec(request).expect("the request serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One KMS call that must succeed.
async fn kms_ok(router: &MultiServiceRouter, case: &str, operation: &str, request: Value) -> Value {
    let (status, parsed) = kms(router, operation, &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: KMS {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon S3 answer: its status, its headers (lower-cased names) and its body.
struct S3Answer {
    status: u16,
    headers: BTreeMap<String, String>,
    body: String,
}

impl S3Answer {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

/// One Amazon S3 request, addressed path-style against the Region's endpoint.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> S3Answer {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("content-type", "application/octet-stream")
        .header("authorization", authorization("s3"))
        .body(Bytes::copy_from_slice(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_ascii_lowercase(),
                String::from_utf8_lossy(value.as_bytes()).to_string(),
            )
        })
        .collect();
    S3Answer {
        status,
        headers,
        body: String::from_utf8_lossy(response.body()).to_string(),
    }
}

/// One default-encryption rule as `GetBucketEncryption` reports it.
#[derive(Debug, PartialEq, Eq)]
struct ReportedRule {
    algorithm: Option<String>,
    kms_key: Option<String>,
}

/// The rules one `GetBucketEncryption` body reports, in document order.
fn reported_rules(body: &str) -> Vec<ReportedRule> {
    blocks_of(body, "Rule")
        .iter()
        .map(|rule| ReportedRule {
            algorithm: opt_element(rule, "SSEAlgorithm"),
            kms_key: opt_element(rule, "KMSMasterKeyID"),
        })
        .collect()
}

/// The key policy a toolkit bootstrap gives the key it creates: the account's own identities
/// may use it, which is what lets the publishing caller write under it. `principal` is the
/// account the statement names, which a template states as a `Ref` of the account pseudo
/// parameter and a direct `CreateKey` has to state as a value.
fn key_policy(principal: Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "Default",
            "Effect": "Allow",
            "Principal": {"AWS": principal},
            "Action": "kms:*",
            "Resource": "*",
        }],
    })
}

/// The template a toolkit bootstrap installs for its staging bucket: the bucket's default
/// encryption names the algorithm one condition chooses and the key a SECOND, nested condition
/// chooses — the key this stack creates, the key the deployment named, or neither.
///
/// `algorithm` is the algorithm the SSE-KMS branch names, so one template shape serves every
/// SSE-KMS algorithm.
fn bootstrap_template(algorithm: &str) -> String {
    json!({
        "Parameters": {
            "Qualifier": {"Type": "String", "Default": "hnb659fds"},
            "FileAssetsBucketName": {"Type": "String", "Default": ""},
            "FileAssetsBucketKmsKeyId": {"Type": "String", "Default": "AWS_MANAGED_KEY"},
        },
        "Conditions": {
            "HasCustomFileAssetsBucketName": {
                "Fn::Not": [{"Fn::Equals": [{"Ref": "FileAssetsBucketName"}, ""]}],
            },
            "CreateNewKey": {"Fn::Equals": [{"Ref": "FileAssetsBucketKmsKeyId"}, ""]},
            "UseAwsManagedKey": {
                "Fn::Equals": [{"Ref": "FileAssetsBucketKmsKeyId"}, "AWS_MANAGED_KEY"],
            },
        },
        "Resources": {
            "FileAssetsBucketEncryptionKey": {
                "Type": "AWS::KMS::Key",
                "Condition": "CreateNewKey",
                "Properties": {
                    "Description": {"Fn::Sub": "the cdk-${Qualifier} staging bucket key"},
                    "KeyPolicy": key_policy(json!({"Ref": "AWS::AccountId"})),
                },
            },
            "StagingBucket": {
                "Type": "AWS::S3::Bucket",
                "Properties": {
                    "BucketName": {
                        "Fn::If": [
                            "HasCustomFileAssetsBucketName",
                            {"Fn::Sub": "${FileAssetsBucketName}"},
                            {"Fn::Sub": "cdk-${Qualifier}-assets-${AWS::AccountId}-${AWS::Region}"},
                        ],
                    },
                    "BucketEncryption": {
                        "ServerSideEncryptionConfiguration": [{
                            "ServerSideEncryptionByDefault": {
                                "SSEAlgorithm": {
                                    "Fn::If": ["UseAwsManagedKey", AES256, algorithm],
                                },
                                "KMSMasterKeyID": {
                                    "Fn::If": [
                                        "UseAwsManagedKey",
                                        {"Ref": "AWS::NoValue"},
                                        {"Fn::If": [
                                            "CreateNewKey",
                                            {"Fn::Sub": "${FileAssetsBucketEncryptionKey.Arn}"},
                                            {"Fn::Sub": "${FileAssetsBucketKmsKeyId}"},
                                        ]},
                                    ],
                                },
                            },
                        }],
                    },
                    "VersioningConfiguration": {"Status": "Enabled"},
                },
            },
        },
        "Outputs": {
            "BucketName": {"Value": {"Ref": "StagingBucket"}},
            "BucketEncryptionKeyArn": {
                "Condition": "CreateNewKey",
                "Value": {"Fn::GetAtt": ["FileAssetsBucketEncryptionKey", "Arn"]},
            },
        },
    })
    .to_string()
}

/// Which key the deployment points the staging bucket at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeclaredKey {
    /// The stack creates the key, and the bucket names the ARN KMS minted for it.
    CreatedByTheStack,
    /// The deployment names a key that already exists, by its ARN.
    NamedByArn,
    /// The deployment names a key that already exists, by its bare key id.
    NamedByKeyId,
    /// The deployment asks for the managed key, so the declared `KMSMasterKeyID` resolves away.
    AwsManaged,
}

/// How the deployment reaches CloudFormation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Deployment {
    CreateStack,
    ChangeSet,
}

/// One row: what the deployment declares, and what the bucket must then report.
struct Row {
    case: &'static str,
    stack: &'static str,
    qualifier: &'static str,
    /// The algorithm the SSE-KMS branch of the declaration names.
    algorithm: &'static str,
    key: DeclaredKey,
    deployment: Deployment,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a key the stack creates, named through a nested condition",
            stack: "CDKToolkit-customer-key",
            qualifier: "hnb659fds",
            algorithm: "aws:kms",
            key: DeclaredKey::CreatedByTheStack,
            deployment: Deployment::CreateStack,
        },
        Row {
            case: "a key the stack creates, deployed through a change set",
            stack: "CDKToolkit-customer-key-change-set",
            qualifier: "q1",
            algorithm: "aws:kms",
            key: DeclaredKey::CreatedByTheStack,
            deployment: Deployment::ChangeSet,
        },
        Row {
            case: "a dual-layer default over a key the stack creates",
            stack: "CDKToolkit-dual-layer",
            qualifier: "dsse",
            algorithm: "aws:kms:dsse",
            key: DeclaredKey::CreatedByTheStack,
            deployment: Deployment::CreateStack,
        },
        Row {
            case: "a pre-existing key the deployment names by its ARN",
            stack: "CDKToolkit-existing-key-arn",
            qualifier: "existingarn",
            algorithm: "aws:kms",
            key: DeclaredKey::NamedByArn,
            deployment: Deployment::CreateStack,
        },
        Row {
            case: "a pre-existing key the deployment names by its key id",
            stack: "CDKToolkit-existing-key-id",
            qualifier: "existingid",
            algorithm: "aws:kms",
            key: DeclaredKey::NamedByKeyId,
            deployment: Deployment::CreateStack,
        },
        Row {
            case: "the managed key, so the declared KMSMasterKeyID resolves away",
            stack: "CDKToolkit-managed-key",
            qualifier: "managed",
            algorithm: "aws:kms",
            key: DeclaredKey::AwsManaged,
            deployment: Deployment::CreateStack,
        },
    ]
}

/// The staging bucket's name, which the template composes from the qualifier and the account
/// and Region pseudo parameters.
fn staging_bucket(row: &Row) -> String {
    format!("cdk-{}-assets-{ACCOUNT}-{REGION}", row.qualifier)
}

/// The awsQuery members one stack parameter travels in, by its POSITION in the list a
/// deployment sends: `Parameters.member.<n>.ParameterKey` and its `ParameterValue`.
const PARAMETER_MEMBERS: &[(&str, &str)] = &[
    (
        "Parameters.member.1.ParameterKey",
        "Parameters.member.1.ParameterValue",
    ),
    (
        "Parameters.member.2.ParameterKey",
        "Parameters.member.2.ParameterValue",
    ),
];

/// Deploy `template` with `parameters` and answer the settled `DescribeStacks` body.
async fn deploy(
    router: &MultiServiceRouter,
    row: &Row,
    template: &str,
    parameters: &[(&'static str, String)],
) -> String {
    let case = row.case;
    let mut members = vec![
        ("StackName", row.stack.to_string()),
        ("TemplateBody", template.to_string()),
    ];
    for (position, (key, value)) in parameters.iter().enumerate() {
        let (key_member, value_member) = *PARAMETER_MEMBERS
            .get(position)
            .unwrap_or_else(|| panic!("case {case:?}: more parameters than this suite declares"));
        members.push((key_member, (*key).to_string()));
        members.push((value_member, value.clone()));
    }

    match row.deployment {
        Deployment::CreateStack => {
            let (status, body) = cloudformation(router, "CreateStack", &members).await;
            assert_eq!(
                status, 200,
                "case {case:?}: CreateStack must answer 200, got {status} with {body}"
            );
        }
        Deployment::ChangeSet => {
            let change_set = "cdk-deploy-change-set";
            let mut with_change_set = members.clone();
            with_change_set.push(("ChangeSetName", change_set.to_string()));
            with_change_set.push(("ChangeSetType", "CREATE".to_string()));
            let (status, body) =
                cloudformation(router, "CreateChangeSet", &with_change_set).await;
            assert_eq!(
                status, 200,
                "case {case:?}: CreateChangeSet must answer 200, got {status} with {body}"
            );
            let named = [
                ("StackName", row.stack.to_string()),
                ("ChangeSetName", change_set.to_string()),
            ];
            let mut described = String::new();
            for _ in 0..POLL_ATTEMPTS {
                let (status, body) =
                    cloudformation(router, "DescribeChangeSet", &named).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: DescribeChangeSet must answer 200, got {status} with {body}"
                );
                described = body;
                if opt_element(&described, "Status").as_deref() == Some("CREATE_COMPLETE") {
                    break;
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
            assert_eq!(
                opt_element(&described, "Status").as_deref(),
                Some("CREATE_COMPLETE"),
                "case {case:?}: the change set must become executable, got {described}"
            );
            let (status, body) = cloudformation(router, "ExecuteChangeSet", &named).await;
            assert_eq!(
                status, 200,
                "case {case:?}: ExecuteChangeSet must answer 200, got {status} with {body}"
            );
        }
    }

    let named = [("StackName", row.stack.to_string())];
    let mut described = String::new();
    for _ in 0..POLL_ATTEMPTS {
        let (status, body) = cloudformation(router, "DescribeStacks", &named).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStacks must answer 200, got {status} with {body}"
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
    let (_, events) = cloudformation(router, "DescribeStackEvents", &named).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: the stack must reach CREATE_COMPLETE, got {described}; events {events}"
    );
    described
}

#[tokio::test]
async fn a_stack_bucket_carries_the_declared_default_encryption_over_the_key_it_names() {
    let router = build_router();

    // The key the two "pre-existing key" rows name: one created in KMS on its own, before any
    // stack is deployed, so the declaration carries an identifier of a key the stack did not
    // create.
    let existing = kms_ok(
        &router,
        "the pre-existing key",
        "CreateKey",
        json!({
            "Description": "a key a deployment names",
            "Policy": key_policy(json!(ACCOUNT)).to_string(),
        }),
    )
    .await;
    let existing_key_id = existing["KeyMetadata"]["KeyId"]
        .as_str()
        .expect("CreateKey answers a KeyId")
        .to_string();
    let existing_key_arn = existing["KeyMetadata"]["Arn"]
        .as_str()
        .expect("CreateKey answers an Arn")
        .to_string();

    for row in rows() {
        let case = row.case;
        let template = bootstrap_template(row.algorithm);
        let declared_parameter = match row.key {
            DeclaredKey::CreatedByTheStack => String::new(),
            DeclaredKey::NamedByArn => existing_key_arn.clone(),
            DeclaredKey::NamedByKeyId => existing_key_id.clone(),
            DeclaredKey::AwsManaged => "AWS_MANAGED_KEY".to_string(),
        };
        let described = deploy(
            &router,
            &row,
            &template,
            &[
                ("Qualifier", row.qualifier.to_string()),
                ("FileAssetsBucketKmsKeyId", declared_parameter.clone()),
            ],
        )
        .await;

        let outputs = outputs_of(&described);
        let bucket = outputs
            .get("BucketName")
            .unwrap_or_else(|| panic!("case {case:?}: no BucketName output in {outputs:?}"))
            .clone();
        assert_eq!(
            bucket,
            staging_bucket(&row),
            "case {case:?}: the stack must publish the bucket name it composed, got {outputs:?}"
        );

        // What the DECLARATION resolved to: the algorithm its condition chose, and the key its
        // nested condition chose — the ARN the stack published for the key it created, the
        // identifier the deployment named, or nothing at all.
        let (expected_algorithm, expected_key) = match row.key {
            DeclaredKey::CreatedByTheStack => (
                row.algorithm.to_string(),
                Some(
                    outputs
                        .get("BucketEncryptionKeyArn")
                        .unwrap_or_else(|| {
                            panic!("case {case:?}: no BucketEncryptionKeyArn in {outputs:?}")
                        })
                        .clone(),
                ),
            ),
            DeclaredKey::NamedByArn => (row.algorithm.to_string(), Some(existing_key_arn.clone())),
            DeclaredKey::NamedByKeyId => (row.algorithm.to_string(), Some(existing_key_id.clone())),
            DeclaredKey::AwsManaged => (AES256.to_string(), None),
        };

        let answer = s3(&router, "GET", &format!("/{bucket}?encryption"), b"").await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: GetBucketEncryption({bucket:?}) must answer 200, got {} with {}",
            answer.status, answer.body
        );
        let reported = reported_rules(&answer.body);
        let expected = vec![ReportedRule {
            algorithm: Some(expected_algorithm.clone()),
            kms_key: expected_key.clone(),
        }];
        assert_eq!(
            reported, expected,
            "case {case:?}: {bucket:?} must report {expected:?}, got {reported:?} ({})",
            answer.body
        );

        // The key the bucket names is one KMS resolves, and a key a stack created is this
        // account's own customer managed key rather than an Amazon Web Services managed one.
        if let Some(key) = &expected_key {
            let metadata = kms_ok(&router, case, "DescribeKey", json!({"KeyId": key})).await;
            assert_eq!(
                metadata["KeyMetadata"]["KeyManager"].as_str(),
                Some("CUSTOMER"),
                "case {case:?}: DescribeKey({key:?}) must report a customer managed key, got \
                 {metadata}"
            );
            assert_eq!(
                metadata["KeyMetadata"]["KeyState"].as_str(),
                Some("Enabled"),
                "case {case:?}: DescribeKey({key:?}) must report an enabled key, got {metadata}"
            );
        }

        // And the default is the one in force: a write that names no encryption of its own is
        // stored under it, which is what makes the declared key the key an asset is encrypted
        // with rather than a record nothing applies.
        let key = "assets/0123456789abcdef.zip";
        let written = s3(&router, "PUT", &format!("/{bucket}/{key}"), b"an asset").await;
        assert_eq!(
            written.status, 200,
            "case {case:?}: the write into {bucket:?} must answer 200, got {} with {}",
            written.status, written.body
        );
        assert_eq!(
            written.header(SSE_HEADER),
            Some(expected_algorithm.as_str()),
            "case {case:?}: the write must report {expected_algorithm:?}, got {:?}",
            written.header(SSE_HEADER)
        );
        assert_eq!(
            written.header(SSE_KMS_KEY_HEADER).map(str::to_string),
            expected_key.clone().map(|key| key_arn(&key, &existing_key_arn, &existing_key_id)),
            "case {case:?}: the write must report the declared key, got {:?}",
            written.header(SSE_KMS_KEY_HEADER)
        );

        let read = s3(&router, "HEAD", &format!("/{bucket}/{key}"), b"").await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the object must read back, got {} with {}",
            read.status, read.body
        );
        assert_eq!(
            read.header(SSE_HEADER),
            Some(expected_algorithm.as_str()),
            "case {case:?}: a read of the object must report {expected_algorithm:?}, got {:?}",
            read.header(SSE_HEADER)
        );
        let fetched = s3(&router, "GET", &format!("/{bucket}/{key}"), b"").await;
        assert_eq!(
            fetched.status, 200,
            "case {case:?}: the object must be readable, got {} with {}",
            fetched.status, fetched.body
        );
        assert_eq!(
            fetched.body, "an asset",
            "case {case:?}: the object must read back the bytes written, got {:?}",
            fetched.body
        );
    }
}

/// The ARN of the key an object was stored under, for a declaration that named the key by
/// either of the two identifier forms a row uses: Amazon S3 reports the key an object is
/// encrypted with as its ARN, so a declaration naming the bare key id is reported by the ARN
/// that key id belongs to.
fn key_arn(declared: &str, existing_key_arn: &str, existing_key_id: &str) -> String {
    if declared == existing_key_id {
        existing_key_arn.to_string()
    } else {
        declared.to_string()
    }
}
