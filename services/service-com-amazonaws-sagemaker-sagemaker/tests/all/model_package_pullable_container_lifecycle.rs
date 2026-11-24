// SPDX-License-Identifier: CC0-1.0

//! The WHOLE life of a model package registered around a container image SageMaker can PULL —
//! register, read back, delete — driven end to end over the real wire path (an awsJson1_1
//! request through the multi-service router into the mounted service), so what is asserted is
//! what a client sees.
//!
//! RULE: whose Amazon ECR registry an inference container comes from decides only whether the
//! registration is ADMITTED — the registries SageMaker pulls for a request are the caller's own
//! in the Region of the request and the AWS-owned registries AWS publishes its SageMaker
//! containers from in that Region — and it is no property of the package that results: an
//! admitted registration is an ordinary model package, so `CreateModelPackage` answers 200 with
//! its `ModelPackageArn`, `DescribeModelPackage` answers 200 replaying it with the members the
//! service resolves for it and `ModelPackageStatus` `Completed`, `DeleteModelPackage` answers
//! 200 carrying no members, and after that delete neither the read nor a second delete answers
//! for it. Every row below makes exactly the same assertions, which is the rule: an
//! AWS-published container yields the same package an own-registry container does.
//!
//! The members a read replays are `DescribeModelPackageOutput`'s
//! (`smithy_query(query="operation",operation="DescribeModelPackage")`): `ModelPackageName`,
//! `ModelPackageArn` and `ModelPackageStatus` (required), `CreationTime`,
//! `ModelPackageStatusDetails`, `CertifyForMarketplace`, `ModelPackageRegistrationType`,
//! `CreatedBy`, `ModelPackageDescription`, and `InferenceSpecification` whose containers carry
//! `Image`, the `ImageDigest` the service resolves (`@pattern
//! ^[Ss][Hh][Aa]256:[0-9a-fA-F]{64}$`) and `IsCheckpoint`.
//!
//! The rows differ along what the rule generalises over: who owns the registry (the caller, the
//! two built-in algorithm registries AWS publishes per Region, the registry of a framework
//! SageMaker builds itself, the Deep Learning Container registry), the Region, the repository
//! and its depth, whether the reference carries a tag, a digest or neither, and whether the
//! package is registered as a VERSION inside a model package group. The AWS-owned registry
//! account of each row is the one AWS publishes that family of image from IN THAT REGION, from
//! the SageMaker Developer Guide's "Docker Registry Paths and Example Code" tables and the Deep
//! Learning Containers' own registry table; none of the rows is the subject.
//!
//! What this suite does NOT cover: which references are refused — that is
//! `model_package_image_pull_admission`'s table and
//! `model_package_aws_provided_container`'s second test — and the registration statuses a
//! `ValidationSpecification` puts a package through, which `model_package_registry` tables.
//!
//! The harness is the one `model_package_registry` declares, so these cases drive the same
//! router every other suite of this crate drives.

use serde_json::{json, Value};

use crate::image_digest_round_trip::{container_digest, hex64, is_reportable_digest};
use crate::model_package_image_pull_admission::registry;
use crate::model_package_registry::{
    container, error_code, expected_arn, message, str_field, Emulator,
};

/// The canonical account id this emulator serves, whose Amazon ECR registry is the caller's own
/// (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// A digest in the form `ImageDigest` publishes (`^[Ss][Hh][Aa]256:[0-9a-fA-F]{64}$`), for the
/// row that PINS its image by digest rather than naming a tag.
fn pinned_digest() -> String {
    format!("sha256:{}", hex64("89abcdef"))
}

/// One registration the whole lifecycle is driven for.
struct Row {
    /// What this row varies, named in every assertion it makes.
    case: &'static str,
    /// The Region the request is signed for, which is the Region its registry host names.
    region: &'static str,
    /// The account whose Amazon ECR registry publishes the image: this emulator's own, or an
    /// AWS-owned registry account of `region`.
    registry_account: &'static str,
    /// The repository and its tag or digest inside that registry.
    repository_reference: String,
    /// Whether the package is registered as a VERSION inside a model package group.
    versioned: bool,
}

/// The registrations the rule is asserted over: the caller's own registry beside each family of
/// registry AWS publishes from, in several Regions and reference forms.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the caller's own registry, a namespaced repository",
            region: "us-east-1",
            registry_account: ACCOUNT,
            repository_reference: "team/inference:1".to_string(),
            versioned: false,
        },
        Row {
            case: "a built-in algorithm registry of the request's Region",
            region: "af-south-1",
            registry_account: "455444449433",
            repository_reference: "kmeans".to_string(),
            versioned: false,
        },
        Row {
            case: "the other built-in algorithm registry of a Region, a tagged image",
            region: "ap-northeast-1",
            registry_account: "351501993468",
            repository_reference: "seq2seq:1".to_string(),
            versioned: false,
        },
        Row {
            case: "a SageMaker framework registry of the request's Region",
            region: "ap-northeast-1",
            registry_account: "354813040037",
            repository_reference: "sagemaker-xgboost:1.7-1".to_string(),
            versioned: false,
        },
        Row {
            case: "the Deep Learning Container registry, pinned by digest",
            region: "eu-west-1",
            registry_account: "763104351884",
            repository_reference: format!("pytorch-inference@{}", pinned_digest()),
            versioned: false,
        },
        Row {
            case: "a VERSION inside a group, around a built-in algorithm image",
            region: "sa-east-1",
            registry_account: "855470959533",
            repository_reference: "forecasting-deepar:1".to_string(),
            versioned: true,
        },
        Row {
            case: "a VERSION inside a group, around the caller's own image",
            region: "eu-west-2",
            registry_account: ACCOUNT,
            repository_reference: "custom/serve:2".to_string(),
            versioned: true,
        },
    ]
}

/// An `InferenceSpecification` around one container naming `image`, with the content types a
/// tabular inference container is registered with.
fn inference_specification(image: &str) -> Value {
    json!({
        "Containers": [container(image)],
        "SupportedContentTypes": ["text/csv"],
        "SupportedResponseMIMETypes": ["text/csv"],
        "SupportedRealtimeInferenceInstanceTypes": ["ml.m5.large"],
        "SupportedTransformInstanceTypes": ["ml.m5.large"],
    })
}

/// How this service states a model package an identifier resolves to no package for: the
/// entity, the ARN the identifier resolves to, and the one sentence — stated here independently
/// of the service.
fn does_not_exist(arn: &str) -> String {
    format!("ModelPackage {arn} does not exist.")
}

/// RULE: a registration around an image SageMaker can pull — the caller's own registry in the
/// Region of the request, or one AWS publishes its SageMaker containers from there — produces
/// an ordinary model package: it is admitted with its ARN, reads back with the members the
/// service resolves and `ModelPackageStatus` `Completed`, deletes with an empty 200, and after
/// the delete answers neither a read nor a second delete.
#[tokio::test]
async fn a_pullable_container_registers_reads_back_and_deletes() {
    let emulator = Emulator::new();
    for (
        index,
        Row {
            case,
            region,
            registry_account,
            repository_reference,
            versioned,
        },
    ) in rows().into_iter().enumerate()
    {
        let image = format!(
            "{}/{repository_reference}",
            registry(registry_account, region)
        );
        let name = format!("pullable{index}");
        let description = format!("a package around {repository_reference}");
        let body = if versioned {
            emulator.create_group(region, &name).await;
            json!({
                "ModelPackageGroupName": name,
                "ModelPackageDescription": description,
                "InferenceSpecification": inference_specification(&image),
            })
        } else {
            json!({
                "ModelPackageName": name,
                "ModelPackageDescription": description,
                "InferenceSpecification": inference_specification(&image),
            })
        };

        // 1. The registration is admitted, with the ARN of the package it registered.
        let (status, answered) = emulator.call_in(region, "CreateModelPackage", &body).await;
        assert_eq!(
            status, 200,
            "case {case:?} ({image}): expected the registration to be admitted, got {status} \
             with {answered}"
        );
        // The identifier a version is registered under is `<group>/<version>`, and the first
        // version of a freshly created group is 1.
        let identifier = if versioned {
            format!("{name}/1")
        } else {
            name.clone()
        };
        let arn = expected_arn(region, &identifier);
        assert_eq!(
            str_field(&answered, "ModelPackageArn"),
            arn,
            "case {case:?} ({image}): the registered package's ARN, got {answered}"
        );

        // 2. And the package reads back: the identity it was registered with, the container it
        //    was registered around, and the members the service resolves for it.
        let (status, described) = emulator
            .call_in(
                region,
                "DescribeModelPackage",
                &json!({"ModelPackageName": identifier}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?} ({image}): the registration must read back, got {status} with \
             {described}"
        );
        assert_eq!(
            (
                described.get("ModelPackageArn").and_then(Value::as_str),
                described.get("ModelPackageName").and_then(Value::as_str),
                described
                    .get("ModelPackageDescription")
                    .and_then(Value::as_str),
                described.get("ModelPackageStatus").and_then(Value::as_str),
                described
                    .get("ModelPackageRegistrationType")
                    .and_then(Value::as_str),
                described
                    .get("CertifyForMarketplace")
                    .and_then(Value::as_bool),
            ),
            (
                Some(arn.as_str()),
                Some(name.as_str()),
                Some(description.as_str()),
                Some("Completed"),
                Some("Registered"),
                Some(false),
            ),
            "case {case:?} ({image}): the package's own members, got {described}"
        );
        assert!(
            described
                .get("CreationTime")
                .and_then(Value::as_f64)
                .is_some_and(|seconds| seconds > 0.0),
            "case {case:?} ({image}): expected a CreationTime, got {described}"
        );
        assert!(
            described.get("CreatedBy").is_some_and(Value::is_object),
            "case {case:?} ({image}): expected the identity that registered it, got {described}"
        );
        // A registration with no validation profile to run holds no per-profile status, and
        // this emulator scans no container image, so both lists of the details are reported
        // EMPTY rather than absent.
        assert_eq!(
            (
                described.pointer("/ModelPackageStatusDetails/ValidationStatuses"),
                described.pointer("/ModelPackageStatusDetails/ImageScanStatuses"),
            ),
            (Some(&json!([])), Some(&json!([]))),
            "case {case:?} ({image}): the registration's status details, got {described}"
        );
        // A version carries the group it joined and its number inside it; an unversioned
        // package carries neither.
        assert_eq!(
            (
                described
                    .get("ModelPackageGroupName")
                    .and_then(Value::as_str),
                described.get("ModelPackageVersion").and_then(Value::as_u64),
            ),
            if versioned {
                (Some(name.as_str()), Some(1))
            } else {
                (None, None)
            },
            "case {case:?} ({image}): the package's place in the registry, got {described}"
        );
        assert_eq!(
            described
                .pointer("/InferenceSpecification/Containers/0/Image")
                .and_then(Value::as_str),
            Some(image.as_str()),
            "case {case:?}: the stored container's Image, got {described}"
        );
        assert_eq!(
            described
                .pointer("/InferenceSpecification/Containers/0/IsCheckpoint")
                .and_then(Value::as_bool),
            Some(false),
            "case {case:?} ({image}): a container nothing said is a checkpoint, got {described}"
        );
        let digest = container_digest(&described, 0).unwrap_or_default();
        assert!(
            is_reportable_digest(&digest),
            "case {case:?} ({image}): expected an ImageDigest in the modelled form, got \
             {digest:?} in {described}"
        );
        // A reference that PINS its image by digest names the image's digest itself, so that
        // is the digest the package reports for it, whichever registry holds it.
        if repository_reference.contains('@') {
            assert_eq!(
                digest,
                pinned_digest(),
                "case {case:?} ({image}): a pinned reference reports the digest it pins, got \
                 {described}"
            );
        }

        // 3. The delete of such a package answers 200 and carries no members.
        let (status, deleted) = emulator
            .call_in(
                region,
                "DeleteModelPackage",
                &json!({"ModelPackageName": identifier}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?} ({image}): the package must delete, got {status} with {deleted}"
        );
        assert!(
            deleted.is_null() || deleted.as_object().is_some_and(serde_json::Map::is_empty),
            "case {case:?} ({image}): DeleteModelPackage carries no members, got {deleted}"
        );

        // 4. After which the package is gone: neither a read nor a second delete answers for
        //    it, and both report the one identifier a successful call would have answered with.
        for target in ["DescribeModelPackage", "DeleteModelPackage"] {
            let (status, read) = emulator
                .call_in(region, target, &json!({"ModelPackageName": identifier}))
                .await;
            assert_eq!(
                (
                    status,
                    error_code(&read).as_str(),
                    message(&read).as_str()
                ),
                (400, "ValidationException", does_not_exist(&arn).as_str()),
                "case {case:?} ({image}): {target} after the delete, got {status} with {read}"
            );
        }
    }
}
