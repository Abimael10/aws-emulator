// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for the two values a registration does NOT take from its request:
//! the DIGEST of the container image it references, and the IDENTITY that created it.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`, and the `awsQuery` form IAM and STS take) through the same
//! `MultiServiceRouter` the production router uses, so what these tests exercise is what a
//! real SDK client gets. Amazon ECR, IAM and STS are mounted on the SAME engine, because
//! both values belong to those services: the image and its manifest digest are the
//! registry's, and the signing identity is the account's.
//!
//! Two RULES, each asserted over a TABLE of inputs rather than one recorded call:
//!
//! * the digest reported for a referenced image is the digest the REGISTRY holding that
//!   image reports for it — one digest per image, the same for every resource and every
//!   operation naming it, and the emulator's own per-reference identifier only where no
//!   registry holds the image at all;
//! * the creator recorded for a registration is the identity whose credentials SIGNED it,
//!   as `sts:GetCallerIdentity` reports that identity — never a value derived from the
//!   access key id, which is a credential id rather than a user name.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::image_digest_round_trip::{
    container_digest, inference_specification, is_reportable_digest, training_specification,
};
// The one reading of an `awsQuery` answer's `<name>` element this tree has, shared with the
// suite that already needed it.
use crate::lineage_read_authorization::element;

/// The canonical account id every emulator ARN carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The Region every call below is signed for.
const REGION: &str = "us-east-1";

/// The access key id of the account's own published credential: a key neither STS nor IAM
/// holds a record for, which is the position AWS account root credentials are in.
pub(crate) const ACCOUNT_KEY_ID: &str = "AKIDEXAMPLE";

/// SageMaker, Amazon ECR, IAM and STS on ONE shared engine, so a reference SageMaker
/// resolves is resolved against the registry a push really reached and an identity it
/// records is one the account really holds.
///
/// Shared with this crate's other suite about the identity a write records
/// (`tests/all/hub_provenance.rs`), which needs exactly this assembly — SageMaker beside
/// the two services that hold the account's credentials — rather than a second copy of it.
pub(crate) struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    pub(crate) fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These tests exercise handler behaviour, not SigV4; a request below carries a
        // credential scope so the Region and the signing identity are the ones it names.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_sagemaker_sagemaker::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(
            service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
                engine,
            ),
        );
        Self { router }
    }

    /// One SageMaker call signed with `access_key_id`, which is what names the identity the
    /// request speaks for.
    async fn sagemaker_as(&self, access_key_id: &str, target: &str, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("SageMaker.{target}"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential={access_key_id}/20260101/{REGION}/sagemaker/aws4_request, \
                     SignedHeaders=host, Signature=not-verified"
                ),
            )
            .body(bytes::Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// One SageMaker call, as the account's own credential, that must be served.
    pub(crate) async fn ok(&self, case: &str, target: &str, body: &Value) -> Value {
        self.ok_as(ACCOUNT_KEY_ID, case, target, body).await
    }

    /// One SageMaker call signed with `access_key_id` that must be served.
    pub(crate) async fn ok_as(
        &self,
        access_key_id: &str,
        case: &str,
        target: &str,
        body: &Value,
    ) -> Value {
        let (status, answered) = self.sagemaker_as(access_key_id, target, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {target}, got {status} with {answered}"
        );
        answered
    }

    /// One Amazon ECR call, over that service's own `awsJson1_1` target, that must be
    /// served.
    async fn ecr(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, answered) = self.ecr_call(operation, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ECR {operation}, got {status} with \
             {answered}"
        );
        answered
    }

    /// One Amazon ECR call, with whatever that service answered.
    async fn ecr_call(&self, operation: &str, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("api.ecr.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.1")
            .header(
                "x-amz-target",
                format!("AmazonEC2ContainerRegistry_V20150921.{operation}"),
            )
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential={ACCOUNT_KEY_ID}/20260101/{REGION}/ecr/aws4_request, \
                     SignedHeaders=host, Signature=not-verified"
                ),
            )
            .body(bytes::Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        (status, answered)
    }

    /// One `awsQuery` action of `service`, signed with the account's own credential.
    pub(crate) async fn query(
        &self,
        case: &str,
        service: &str,
        version: &str,
        action: &str,
        parameters: &[(&str, String)],
    ) -> String {
        let mut body = format!("Action={action}&Version={version}");
        for (name, value) in parameters {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(
                &percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC)
                    .to_string(),
            );
        }
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("{service}.amazonaws.com"))
            .header("content-type", "application/x-www-form-urlencoded")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential={ACCOUNT_KEY_ID}/20260101/{REGION}/{service}/aws4_request, \
                     SignedHeaders=host, Signature=not-verified"
                ),
            )
            .body(bytes::Bytes::from(body))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let answered = String::from_utf8_lossy(response.body()).to_string();
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {service} {action}, got {status} with \
             {answered}"
        );
        answered
    }

    /// The address Amazon ECR ANNOUNCES for `repository` — `Repository$repositoryUri`, "the
    /// URI for the repository. You can use this URI for container image `push` and `pull`
    /// operations" — which is what a client that pushed an image here was handed.
    async fn repository_uri(&self, case: &str, repository: &str) -> String {
        let described = self
            .ecr(
                case,
                "DescribeRepositories",
                &json!({"repositoryNames": [repository]}),
            )
            .await;
        described
            .pointer("/repositories/0/repositoryUri")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected a repositoryUri, got {described}")
            })
            .to_string()
    }

    /// Push a manifest to `repository` under `tag` through Amazon ECR's OWN
    /// `CreateRepository`/`PutImage`, and answer the manifest digest that registry minted.
    ///
    /// The manifest declares no layer and no configuration blob, so the push needs no layer
    /// upload behind it; `content` is what makes two pushes two different images.
    async fn push_image(&self, case: &str, repository: &str, tag: &str, content: &str) -> String {
        let manifest = json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
            "layers": [],
            "annotations": { "org.opencontainers.image.ref.name": content },
        })
        .to_string();
        // A repository a row pushed to before is the one this push reaches: the registry's
        // own refusal of a second create is what says so.
        let (status, created) = self
            .ecr_call("CreateRepository", &json!({"repositoryName": repository}))
            .await;
        let already_held = created
            .get("__type")
            .and_then(Value::as_str)
            .is_some_and(|code| code.contains("RepositoryAlreadyExistsException"));
        assert!(
            status == 200 || already_held,
            "case {case:?}: expected the repository {repository:?} to be held, got {status} \
             with {created}"
        );
        let pushed = self
            .ecr(
                case,
                "PutImage",
                &json!({
                    "repositoryName": repository,
                    "imageTag": tag,
                    "imageManifest": manifest,
                }),
            )
            .await;
        pushed
            .pointer("/image/imageId/imageDigest")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected PutImage to mint a digest, got {pushed}")
            })
            .to_string()
    }
}

/// The ECR registry host of this account, in the Region every call is signed for — the
/// `<account>.dkr.ecr.<region>.amazonaws.com` form the Amazon ECR User Guide gives for a
/// registry.
fn registry() -> String {
    format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com")
}

/// The digest `DescribeAlgorithm` reports for an algorithm's training image.
fn training_digest(described: &Value) -> Option<String> {
    described
        .pointer("/TrainingSpecification/TrainingImageDigest")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// What a row expects of the digest reported for its reference.
#[derive(Debug)]
enum Expected {
    /// The registry holds the image, and this is the digest it minted for it.
    Held(String),
    /// No registry holds the image, so what is reported is the emulator's own identifier
    /// for the reference: a digest the member can carry which is none of the digests the
    /// registry does hold.
    NotHeld,
}

/// RULE: the digest a registration reports for a container image is the digest the registry
/// HOLDING that image reports for it — resolved from the reference, so it is one digest per
/// image, the same for every resource and for every operation that names it. A reference no
/// registry holds an image for is reported with the emulator's own identifier for the
/// reference instead, which no held image's digest collides with.
#[tokio::test]
async fn a_registration_reports_the_digest_the_registry_holds_for_the_referenced_image() {
    let emulator = Emulator::new();
    let registry = registry();

    // Three images really pushed, so a row's expectation is a value the registry minted
    // rather than one this test computed: two tags of one repository carrying DIFFERENT
    // manifests, and a namespaced repository pushed under the registry's default tag.
    let first = emulator.push_image("setup", "trainer", "1", "first").await;
    let second = emulator.push_image("setup", "trainer", "2", "second").await;
    let namespaced = emulator
        .push_image("setup", "team/infer", "latest", "third")
        .await;
    let announced_digest = emulator
        .push_image("setup", "announced", "v1", "fourth")
        .await;
    let announced_uri = emulator.repository_uri("setup", "announced").await;
    assert_ne!(
        first, second,
        "case \"setup\": two different manifests must mint two digests, got {first} twice"
    );

    // The rows differ along how the reference addresses the image — the tag, the
    // repository, a namespaced repository with no tag at all (the registry's default tag),
    // a pinned content digest — and along what no registry holds: an unpushed tag, an
    // absent repository, and another registry's host.
    let rows: Vec<(&str, String, Expected)> = vec![
        (
            "a tagged reference",
            format!("{registry}/trainer:1"),
            Expected::Held(first.clone()),
        ),
        (
            "another tag of the same repository",
            format!("{registry}/trainer:2"),
            Expected::Held(second.clone()),
        ),
        (
            "a namespaced repository with no tag, which names the default one",
            format!("{registry}/team/infer"),
            Expected::Held(namespaced.clone()),
        ),
        (
            "a reference pinning a digest the registry holds",
            format!("{registry}/trainer@{first}"),
            Expected::Held(first.clone()),
        ),
        (
            "the address the registry announces for a push",
            format!("{announced_uri}:v1"),
            Expected::Held(announced_digest.clone()),
        ),
        (
            "a tag the repository holds no image under",
            format!("{registry}/trainer:9"),
            Expected::NotHeld,
        ),
        (
            "a repository this registry does not hold",
            format!("{registry}/absent:1"),
            Expected::NotHeld,
        ),
        (
            "another registry's host",
            "registry.example.com/trainer:1".to_string(),
            Expected::NotHeld,
        ),
    ];

    let held = [first, second, namespaced, announced_digest];
    emulator.ok("setup", "CreateModelPackageGroup", &json!({
        "ModelPackageGroupName": "registry-digests",
    }))
    .await;

    for (index, (case, image, expected)) in rows.iter().enumerate() {
        let algorithm = format!("registry-digest-{index}");
        emulator
            .ok(
                case,
                "CreateAlgorithm",
                &json!({
                    "AlgorithmName": algorithm,
                    "TrainingSpecification": training_specification(image, None),
                    "InferenceSpecification": inference_specification(image, None),
                }),
            )
            .await;
        let described = emulator
            .ok(
                case,
                "DescribeAlgorithm",
                &json!({"AlgorithmName": algorithm}),
            )
            .await;
        let reported = training_digest(&described)
            .unwrap_or_else(|| panic!("case {case:?}: expected a digest, got {described}"));
        assert!(
            is_reportable_digest(&reported),
            "case {case:?}: expected a digest in the modelled ImageDigest form, got \
             {reported:?}"
        );
        match expected {
            Expected::Held(digest) => assert_eq!(
                &reported, digest,
                "case {case:?}: expected the digest the registry holds for {image:?}, got \
                 {reported:?}"
            ),
            Expected::NotHeld => assert!(
                !held.contains(&reported),
                "case {case:?}: no registry holds {image:?}, so no held image's digest may \
                 be reported for it, got {reported:?}"
            ),
        }

        // The digest belongs to the IMAGE, not to the resource or the operation naming it:
        // the algorithm's own inference container and a model package naming the same
        // reference report the same value.
        assert_eq!(
            container_digest(&described, 0).as_deref(),
            Some(reported.as_str()),
            "case {case:?}: expected the algorithm's inference container to report \
             {reported:?}, got {described}"
        );
        let registered = emulator
            .ok(
                case,
                "CreateModelPackage",
                &json!({
                    "ModelPackageGroupName": "registry-digests",
                    "InferenceSpecification": inference_specification(image, None),
                }),
            )
            .await;
        let arn = registered
            .get("ModelPackageArn")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected an ARN, got {registered}"))
            .to_string();
        let package = emulator
            .ok(
                case,
                "DescribeModelPackage",
                &json!({"ModelPackageName": arn}),
            )
            .await;
        assert_eq!(
            container_digest(&package, 0).as_deref(),
            Some(reported.as_str()),
            "case {case:?}: expected the model package's container to report {reported:?}, \
             got {package}"
        );
    }
}

/// RULE: a registration PINS the image it was registered against — a digest already
/// resolved is not resolved again, so an image pushed to the same tag after the
/// registration does not change what the registered resource reports.
#[tokio::test]
async fn a_registration_keeps_the_digest_the_image_had_when_it_was_registered() {
    let emulator = Emulator::new();
    let image = format!("{}/pinned:1", registry());
    let case = "a tag moved after the registration";

    let registered_against = emulator.push_image(case, "pinned", "1", "before").await;
    emulator
        .ok(
            case,
            "CreateAlgorithm",
            &json!({
                "AlgorithmName": "pinned",
                "TrainingSpecification": training_specification(&image, None),
            }),
        )
        .await;

    // The same tag now names other bytes, which is a different image.
    let moved_to = emulator.push_image(case, "pinned", "1", "after").await;
    assert_ne!(
        registered_against, moved_to,
        "case {case:?}: the second push must be a different image, got {moved_to} twice"
    );

    let described = emulator
        .ok(case, "DescribeAlgorithm", &json!({"AlgorithmName": "pinned"}))
        .await;
    assert_eq!(
        training_digest(&described),
        Some(registered_against.clone()),
        "case {case:?}: expected the digest the image had at registration \
         ({registered_against}), got {described}"
    );
}

/// RULE: the creator a registration records is the identity whose credentials SIGNED it, as
/// `sts:GetCallerIdentity` reports that identity — the account's own published identity for
/// a credential no record names, the IAM user that owns a long-term access key, and the
/// session for an assumed-role credential. An access key id is a credential id, so it is
/// never rendered into the identity's ARN.
#[tokio::test]
async fn the_creator_recorded_is_the_identity_that_signed_the_registration() {
    let emulator = Emulator::new();
    let ambient_arn = service_common::caller_principal::ambient_user_arn();
    let ambient_id = service_common::caller_principal::ambient_user_id();

    // An IAM user of this account, with its own long-term access key: a request signed with
    // that key speaks for THAT user.
    let user = "registry-writer";
    emulator
        .query(
            "an IAM user's own access key",
            "iam",
            "2010-05-08",
            "CreateUser",
            &[("UserName", user.to_string())],
        )
        .await;
    let created_key = emulator
        .query(
            "an IAM user's own access key",
            "iam",
            "2010-05-08",
            "CreateAccessKey",
            &[("UserName", user.to_string())],
        )
        .await;
    let user_key_id = element(&created_key, "AccessKeyId")
        .unwrap_or_else(|| panic!("expected an <AccessKeyId> in {created_key}"))
        .to_string();

    // An assumed-role session: the identity is the session, as STS reports it.
    let role = "registry-session-role";
    let session = "registry-session";
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": format!("arn:aws:iam::{ACCOUNT}:root") },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    emulator
        .query(
            "an assumed-role session",
            "iam",
            "2010-05-08",
            "CreateRole",
            &[("RoleName", role.to_string()), ("AssumeRolePolicyDocument", trust)],
        )
        .await;
    let assumed = emulator
        .query(
            "an assumed-role session",
            "sts",
            "2011-06-15",
            "AssumeRole",
            &[
                ("RoleArn", format!("arn:aws:iam::{ACCOUNT}:role/{role}")),
                ("RoleSessionName", session.to_string()),
            ],
        )
        .await;
    let session_key_id = element(&assumed, "AccessKeyId")
        .unwrap_or_else(|| panic!("expected an <AccessKeyId> in {assumed}"))
        .to_string();

    // The rows differ along the KIND of credential the registration is signed with, which
    // is what the rule generalises over.
    let rows: Vec<(&str, String, String)> = vec![
        (
            "the account's own published credential",
            ACCOUNT_KEY_ID.to_string(),
            ambient_arn.clone(),
        ),
        (
            "another credential id no record names",
            "AKIAIOSFODNN7EXAMPLE".to_string(),
            ambient_arn.clone(),
        ),
        (
            "an IAM user's own access key",
            user_key_id.clone(),
            format!("arn:aws:iam::{ACCOUNT}:user/{user}"),
        ),
        (
            "an assumed-role session",
            session_key_id.clone(),
            format!("arn:aws:sts::{ACCOUNT}:assumed-role/{role}/{session}"),
        ),
    ];

    emulator
        .ok("setup", "CreateModelPackageGroup", &json!({
            "ModelPackageGroupName": "creator-group",
        }))
        .await;

    for (index, (case, access_key_id, expected)) in rows.iter().enumerate() {
        let registered = emulator
            .ok_as(
                access_key_id,
                case,
                "CreateModelPackage",
                &json!({
                    "ModelPackageGroupName": "creator-group",
                    "InferenceSpecification": inference_specification(
                        &format!("{}/infer:{index}", registry()),
                        None,
                    ),
                }),
            )
            .await;
        let arn = registered
            .get("ModelPackageArn")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected an ARN, got {registered}"))
            .to_string();
        let described = emulator
            .ok_as(
                access_key_id,
                case,
                "DescribeModelPackage",
                &json!({"ModelPackageName": arn}),
            )
            .await;
        let creator = described
            .pointer("/CreatedBy/IamIdentity/Arn")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected a creator, got {described}"));
        assert_eq!(
            creator, expected,
            "case {case:?}: expected the signing identity {expected:?}, got {creator:?}"
        );
        assert!(
            !creator.contains(access_key_id.as_str()),
            "case {case:?}: an access key id is no part of an identity's ARN, got \
             {creator:?}"
        );
        let principal_id = described
            .pointer("/CreatedBy/IamIdentity/PrincipalId")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected a principal id, got {described}"));
        assert!(
            !principal_id.is_empty(),
            "case {case:?}: expected the id of the signing principal, got {principal_id:?}"
        );

        // The same identity is recorded by every write of this service that records one, so
        // a group created by the same caller reports the same creator.
        let group = format!("creator-group-{index}");
        emulator
            .ok_as(
                access_key_id,
                case,
                "CreateModelPackageGroup",
                &json!({"ModelPackageGroupName": group}),
            )
            .await;
        let described_group = emulator
            .ok_as(
                access_key_id,
                case,
                "DescribeModelPackageGroup",
                &json!({"ModelPackageGroupName": group}),
            )
            .await;
        assert_eq!(
            described_group
                .pointer("/CreatedBy/IamIdentity/Arn")
                .and_then(Value::as_str),
            Some(expected.as_str()),
            "case {case:?}: expected the group's creator to be {expected:?}, got \
             {described_group}"
        );
    }

    // TWO registrations by ONE caller report ONE creator: the identity belongs to the
    // credential, not to the resource.
    let mut creators = Vec::new();
    for index in 0..2 {
        let name = format!("one-caller-{index}");
        let registered = emulator
            .ok(
                "two registrations by one caller",
                "CreateModelPackage",
                &json!({
                    "ModelPackageName": name,
                    "InferenceSpecification": inference_specification(
                        &format!("{}/infer:one-caller", registry()),
                        None,
                    ),
                }),
            )
            .await;
        let arn = registered
            .get("ModelPackageArn")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("expected an ARN, got {registered}"))
            .to_string();
        let described = emulator
            .ok(
                "two registrations by one caller",
                "DescribeModelPackage",
                &json!({"ModelPackageName": arn}),
            )
            .await;
        creators.push(
            described
                .pointer("/CreatedBy/IamIdentity/Arn")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        );
        assert_eq!(
            described
                .pointer("/CreatedBy/IamIdentity/PrincipalId")
                .and_then(Value::as_str),
            Some(ambient_id.as_str()),
            "case \"two registrations by one caller\": expected the account identity's own \
             id, got {described}"
        );
    }
    assert_eq!(
        creators[0], creators[1],
        "case \"two registrations by one caller\": expected one creator for one caller, got \
         {creators:?}"
    );
    assert_eq!(
        creators[0], ambient_arn,
        "case \"two registrations by one caller\": expected {ambient_arn:?}, got {:?}",
        creators[0]
    );
}
