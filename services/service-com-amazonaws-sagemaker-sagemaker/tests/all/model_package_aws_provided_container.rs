// SPDX-License-Identifier: CC0-1.0

//! Registering a model package around a container AWS ITSELF publishes, driven end to end over
//! the real wire path (an awsJson1_1 request through the multi-service router into the mounted
//! service), so what is asserted is what a client sees.
//!
//! RULE: besides the caller's own registry, SageMaker pulls an inference container out of the
//! Amazon ECR registries AWS ITSELF publishes its containers from — AWS-owned accounts, one per
//! Region per family of image, whose repositories grant SageMaker the pull for any account — so
//! a registration naming an image published for the Region of the request is admitted exactly
//! like one naming the caller's own repository: HTTP 200 with the `ModelPackageArn`, and a
//! package that reads back with the image it was registered around and a `ModelPackageStatus`
//! of `Completed`. The pairing is what makes such a registry addressable: an account AWS
//! publishes from in one Region addresses no registry in another, which the second test below
//! still refuses.
//!
//! `ModelPackageContainerDefinition$Image` is documented for both of those cases — "If you are
//! using your own custom algorithm instead of an algorithm provided by SageMaker, the inference
//! code must meet SageMaker requirements"
//! (`smithy_query(query="doc",shape="ModelPackageContainerDefinition",member="Image")`) — and
//! an algorithm provided by SageMaker is published from an AWS-owned registry, never from the
//! caller's.
//!
//! The registry accounts the rows below name are AWS's published ones, from the SageMaker
//! Developer Guide's "Docker Registry Paths and Example Code" tables and the Deep Learning
//! Containers' own registry table, which give one registry account per Region for each family
//! of image AWS publishes: the two built-in algorithm registries, the SageMaker-provided
//! framework images, and the Deep Learning Containers. The rows differ along that family and
//! its account, the Region, the repository, the tag-versus-digest form, and whether the package
//! is versioned inside a group — none of the rows is the subject: what the rule turns on is the
//! registry's (Region, account) pairing, not any one image.
//!
//! The harness is the one `model_package_registry` declares, so these cases drive the same
//! router every other suite of this crate drives.

use serde_json::{json, Value};

use crate::model_package_image_pull_admission::registry;
use crate::model_package_registry::{container, expected_arn, str_field, Emulator};

/// One AWS-published inference container a registration is made around.
struct Row {
    /// What this row varies, named in every assertion it makes.
    case: &'static str,
    /// The Region the request is signed for, which is the Region its registry host names.
    region: &'static str,
    /// The AWS-owned registry account the image is published from.
    registry_account: &'static str,
    /// The repository and its tag or digest inside that registry.
    repository_reference: &'static str,
    /// Whether the package is registered as a VERSION inside a model package group.
    versioned: bool,
}

/// The AWS-published containers the rule is asserted over.
///
/// Each account id is the registry AWS publishes that family of image from IN THAT REGION
/// ("Docker Registry Paths and Example Code", plus the Deep Learning Containers' own registry
/// table): the two built-in algorithm registries, the registries of the frameworks SageMaker
/// builds itself (`sagemaker-xgboost`, `sagemaker-scikit-learn`), and the Deep Learning
/// Container registry. None of the rows is the subject — the rule is that an AWS-published
/// registry of the request's Region is one SageMaker pulls for any account.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a built-in algorithm registry, a tagged algorithm image",
            region: "us-west-2",
            registry_account: "433757028032",
            repository_reference: "xgboost:1",
            versioned: false,
        },
        Row {
            case: "the other built-in algorithm registry of the same Region",
            region: "us-west-2",
            registry_account: "174872318107",
            repository_reference: "linear-learner:1",
            versioned: false,
        },
        Row {
            case: "a built-in algorithm registry of another Region",
            region: "us-east-1",
            registry_account: "811284229777",
            repository_reference: "blazingtext:1",
            versioned: false,
        },
        Row {
            case: "a built-in algorithm registry, no tag at all",
            region: "us-east-1",
            registry_account: "382416733822",
            repository_reference: "kmeans",
            versioned: false,
        },
        Row {
            case: "a SageMaker framework registry, a tagged framework image",
            region: "us-west-2",
            registry_account: "246618743249",
            repository_reference: "sagemaker-xgboost:1.7-1",
            versioned: false,
        },
        Row {
            case: "a SageMaker framework registry of another Region",
            region: "us-east-1",
            registry_account: "683313688378",
            repository_reference: "sagemaker-scikit-learn:1.2-1",
            versioned: false,
        },
        Row {
            case: "a SageMaker framework registry of a Region with one algorithm registry",
            region: "eu-west-2",
            registry_account: "764974769150",
            repository_reference: "sagemaker-sparkml-serving:2.4",
            versioned: false,
        },
        Row {
            case: "the Deep Learning Container registry, a tagged image",
            region: "eu-west-1",
            registry_account: "763104351884",
            repository_reference: "huggingface-pytorch-inference:2.1.0-transformers4.37.0-cpu-py310",
            versioned: false,
        },
        Row {
            case: "the Deep Learning Container registry, pinned by digest",
            region: "us-west-2",
            registry_account: "763104351884",
            repository_reference: concat!(
                "tensorflow-inference@sha256:",
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            ),
            versioned: false,
        },
        Row {
            case: "a VERSION inside a group, around a built-in algorithm image",
            region: "us-west-2",
            registry_account: "433757028032",
            repository_reference: "xgboost:1",
            versioned: true,
        },
        Row {
            case: "a VERSION inside a group, around a framework image",
            region: "us-west-2",
            registry_account: "246618743249",
            repository_reference: "sagemaker-xgboost:1.7-1",
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
    })
}

/// RULE: a registration around an image published from an AWS-owned Amazon ECR registry in the
/// Region of the request is admitted — 200 with the package's ARN — and the package reads back
/// around that image with `ModelPackageStatus` `Completed`, whichever family of AWS-published
/// container it names and whether it is versioned or not.
#[tokio::test]
async fn a_model_package_registers_around_a_container_aws_publishes() {
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
        let name = format!("provided{index}");
        let body = if versioned {
            emulator.create_group(region, &name).await;
            json!({
                "ModelPackageGroupName": name,
                "InferenceSpecification": inference_specification(&image),
                "ModelApprovalStatus": "Approved",
            })
        } else {
            json!({
                "ModelPackageName": name,
                "InferenceSpecification": inference_specification(&image),
            })
        };

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
        assert_eq!(
            str_field(&answered, "ModelPackageArn"),
            expected_arn(region, &identifier),
            "case {case:?} ({image}): the registered package's ARN, got {answered}"
        );

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
            described
                .pointer("/InferenceSpecification/Containers/0/Image")
                .and_then(Value::as_str),
            Some(image.as_str()),
            "case {case:?}: the stored container's Image, got {described}"
        );
        assert_eq!(
            described.get("ModelPackageStatus").and_then(Value::as_str),
            Some("Completed"),
            "case {case:?} ({image}): a registration with no validation to run is registered \
             Completed, got {described}"
        );
        if versioned {
            assert_eq!(
                described.get("ModelApprovalStatus").and_then(Value::as_str),
                Some("Approved"),
                "case {case:?} ({image}): the approval status the request asked for, got \
                 {described}"
            );
        }
    }
}

/// One registry a registration is refused around: the case, the Region the request is signed
/// for, the registry host's account and Region, and the repository the refusal must name.
struct RefusedRow {
    case: &'static str,
    request_region: &'static str,
    registry_account: &'static str,
    registry_region: &'static str,
    repository: &'static str,
}

/// RULE: an AWS-published registry is one SageMaker pulls for a request made in THE REGION AWS
/// publishes it in — AWS publishes one registry account per Region per family of image, so an
/// account published for one Region addresses no registry in another — and a reference that
/// fails that pairing is refused 400 `ValidationException` naming the registry and the
/// repository the path carried, registering nothing. The admitted rows are above; the refusal
/// of a registry nobody publishes from is `model_package_image_pull_admission`'s table.
#[tokio::test]
async fn an_aws_published_registry_outside_its_region_is_refused() {
    let emulator = Emulator::new();
    let rows = [
        RefusedRow {
            case: "a built-in algorithm registry of its own Region, asked for from another",
            request_region: "us-east-1",
            registry_account: "433757028032",
            registry_region: "us-west-2",
            repository: "xgboost",
        },
        RefusedRow {
            // The host names the request's Region, but with the account AWS publishes from in
            // a DIFFERENT Region: that pair addresses no registry AWS publishes from.
            case: "another Region's algorithm registry account inside this Region's host",
            request_region: "us-west-2",
            registry_account: "382416733822",
            registry_region: "us-west-2",
            repository: "xgboost",
        },
        RefusedRow {
            case: "another Region's framework registry account inside this Region's host",
            request_region: "us-east-1",
            registry_account: "246618743249",
            registry_region: "us-east-1",
            repository: "sagemaker-xgboost",
        },
        RefusedRow {
            // The Deep Learning Containers of the Regions that have their own registry are
            // not published from the account that serves the rest.
            case: "the shared deep-learning registry account in a Region with its own",
            request_region: "me-south-1",
            registry_account: "763104351884",
            registry_region: "me-south-1",
            repository: "pytorch-inference",
        },
    ];

    for (
        index,
        RefusedRow {
            case,
            request_region,
            registry_account,
            registry_region,
            repository,
        },
    ) in rows.into_iter().enumerate()
    {
        let image = format!(
            "{}/{repository}:1",
            registry(registry_account, registry_region)
        );
        let name = format!("refused{index}");
        let (status, answered) = emulator
            .call_in(
                request_region,
                "CreateModelPackage",
                &json!({
                    "ModelPackageName": name,
                    "InferenceSpecification": inference_specification(&image),
                }),
            )
            .await;
        assert_eq!(
            (
                status,
                crate::model_package_registry::error_code(&answered).as_str(),
                message(&answered).as_str()
            ),
            (
                400,
                "ValidationException",
                crate::model_package_image_pull_admission::access_denied(
                    registry_account,
                    repository
                )
                .as_str()
            ),
            "case {case:?} ({image}): expected the pull refusal, got {status} with {answered}"
        );
        // A refused registration stores nothing: the name answers to no package afterwards.
        let (status, read) = emulator
            .call_in(
                request_region,
                "DescribeModelPackage",
                &json!({"ModelPackageName": name}),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?} ({image}): a refused registration must register nothing, got \
             {status} with {read}"
        );
    }
}

/// The error message an answer carries, whichever casing the envelope spells it with.
fn message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}
