// SPDX-License-Identifier: CC0-1.0

//! Which container images a `CreateModelPackage` may be registered around, driven end to end
//! over the real wire path (an awsJson1_1 request through the multi-service router into the
//! mounted service), so what is asserted is what a client sees.
//!
//! RULE: a model package is registered only around inference container images SageMaker can
//! PULL for the request — Amazon ECR is per account and per Region, so the registries it pulls
//! from are this account's own registry in the Region the request is made in and the registries
//! AWS itself publishes its SageMaker containers from in that Region (those are
//! `model_package_aws_provided_container`'s rows). An image path addressing any OTHER Amazon
//! ECR registry (a third account's, or another Region's) is refused 400 `ValidationException`
//! naming the registry id and the repository the path carries, and registers nothing; a path
//! addressing no Amazon ECR registry at all is left to the format rule
//! `model_package_admission` covers.
//!
//! The rows below span what that generalises over: the registry's account and Region against
//! the request's, the tag-versus-digest form, the repository's depth, the host's case, the
//! member the container sits in (the inference specification and an additional one), and how
//! many containers the request carries.
//!
//! The harness is the one `model_package_registry` declares, so these cases drive the same
//! router every other suite of this crate drives.

use serde_json::{json, Value};

use service_com_amazonaws_sagemaker_sagemaker::state;

use crate::model_package_registry::{container, error_code, expected_arn, str_field, Emulator};

/// The canonical account id this emulator serves, whose Amazon ECR registry is the one
/// SageMaker pulls from (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// Another account's id, in the 12-digit form `registryId` documents
/// (`^[0-9]{12}$`) — one of the reserved example account ids, so it names no real registry.
const OTHER_ACCOUNT: &str = "111122223333";

/// The error message an answer carries, whichever casing the envelope spells it with.
fn message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// How SageMaker words an Amazon ECR reference it cannot pull, stated here independently of
/// the service: the registry and repository the path addressed, then the sentence telling the
/// caller what to check.
pub(crate) fn access_denied(registry_id: &str, repository_name: &str) -> String {
    format!(
        "Access denied for registry ID: {registry_id}, repository name: {repository_name}. \
         Please check if your ECR image exists and has proper pull permissions for SageMaker."
    )
}

/// An Amazon ECR registry host: `<account>.dkr.ecr.<region>.amazonaws.com`, the form the
/// Amazon ECR User Guide gives for a pull.
pub(crate) fn registry(account: &str, region: &str) -> String {
    format!("{account}.dkr.ecr.{region}.amazonaws.com")
}

/// An `InferenceSpecification` whose containers name `images`.
fn inference_specification(images: &[String]) -> Value {
    let containers: Vec<Value> = images.iter().map(|image| container(image)).collect();
    json!({
        "Containers": containers,
        "SupportedContentTypes": ["application/json"],
        "SupportedResponseMIMETypes": ["application/json"],
    })
}

/// One row of the pull-admission table: the case, the Region the request is signed for, the
/// image its one container names, and the registry and repository the refusal must name —
/// `None` for a path the registration admits.
struct Row {
    case: &'static str,
    region: &'static str,
    image: String,
    refused: Option<(String, String)>,
}

/// The table every row of the rule is asserted over.
fn rows() -> Vec<Row> {
    let default = state::DEFAULT_REGION;
    vec![
        Row {
            case: "this account's registry in the request's Region",
            region: default,
            image: format!("{}/inference:1", registry(ACCOUNT, default)),
            refused: None,
        },
        Row {
            case: "this account's registry in the request's Region, another Region's request",
            region: "eu-west-1",
            image: format!("{}/inference:1", registry(ACCOUNT, "eu-west-1")),
            refused: None,
        },
        Row {
            case: "this account's registry, no tag at all",
            region: default,
            image: format!("{}/inference", registry(ACCOUNT, default)),
            refused: None,
        },
        Row {
            case: "this account's registry, a digest instead of a tag",
            region: default,
            image: format!("{}/inference@sha256:abc", registry(ACCOUNT, default)),
            refused: None,
        },
        Row {
            case: "this account's registry in ANOTHER Region",
            region: default,
            image: format!("{}/inference:1", registry(ACCOUNT, "us-west-2")),
            refused: Some((ACCOUNT.to_string(), "inference".to_string())),
        },
        Row {
            case: "another Region's request against the default Region's registry",
            region: "ap-southeast-2",
            image: format!("{}/inference:1", registry(ACCOUNT, default)),
            refused: Some((ACCOUNT.to_string(), "inference".to_string())),
        },
        Row {
            case: "another account's registry in the request's Region",
            region: default,
            image: format!("{}/inference:1", registry(OTHER_ACCOUNT, default)),
            refused: Some((OTHER_ACCOUNT.to_string(), "inference".to_string())),
        },
        Row {
            case: "another account's registry in another Region",
            region: default,
            image: format!("{}/inference:1", registry(OTHER_ACCOUNT, "us-west-2")),
            refused: Some((OTHER_ACCOUNT.to_string(), "inference".to_string())),
        },
        Row {
            case: "another account's registry, a namespaced repository",
            region: default,
            image: format!("{}/team/inference:2", registry(OTHER_ACCOUNT, default)),
            refused: Some((OTHER_ACCOUNT.to_string(), "team/inference".to_string())),
        },
        Row {
            case: "another account's registry, a digest instead of a tag",
            region: default,
            image: format!("{}/inference@sha256:abc", registry(OTHER_ACCOUNT, default)),
            refused: Some((OTHER_ACCOUNT.to_string(), "inference".to_string())),
        },
        Row {
            case: "another account's registry, an uppercased host",
            region: default,
            image: format!("{OTHER_ACCOUNT}.DKR.ECR.{default}.amazonaws.com/inference:1"),
            refused: Some((OTHER_ACCOUNT.to_string(), "inference".to_string())),
        },
        // One past the 12-digit bound `registryId` publishes (`^[0-9]{12}$`): the host
        // addresses no Amazon ECR registry, so this rule states nothing about it.
        Row {
            case: "a host whose first label is one digit too long",
            region: default,
            image: format!("{OTHER_ACCOUNT}4.dkr.ecr.{default}.amazonaws.com/inference:1"),
            refused: None,
        },
        Row {
            case: "another registry's host",
            region: default,
            image: "registry.example.com/team/inference:1".to_string(),
            refused: None,
        },
        Row {
            case: "another registry's host with a port",
            region: default,
            image: "registry.example.com:5000/inference".to_string(),
            refused: None,
        },
        // An AWS-published registry of the request's Region: SageMaker pulls it for every
        // account, so it is admitted beside this account's own
        // (`model_package_aws_provided_container` tables that family).
        Row {
            case: "a registry AWS publishes its containers from, in the request's Region",
            region: "us-west-2",
            image: format!("{}/xgboost:1", registry("433757028032", "us-west-2")),
            refused: None,
        },
        // The same account written into ANOTHER Region's host addresses no registry AWS
        // publishes from, so it is refused like any third account's.
        Row {
            case: "an AWS registry account inside another Region's host",
            region: default,
            image: format!("{}/xgboost:1", registry("433757028032", default)),
            refused: Some(("433757028032".to_string(), "xgboost".to_string())),
        },
    ]
}

/// RULE: a registration is admitted exactly when every inference container's image addresses a
/// registry this request can pull from — this account's own Amazon ECR registry in the Region
/// of the request, or one AWS publishes its SageMaker containers from in that Region — or no
/// Amazon ECR registry at all; any other Amazon ECR reference is refused 400
/// `ValidationException` naming that registry and repository, and registers nothing.
#[tokio::test]
async fn a_model_package_is_registered_only_around_an_image_this_request_can_pull() {
    let emulator = Emulator::new();
    for (index, Row { case, region, image, refused }) in rows().into_iter().enumerate() {
        let name = format!("pull{index}");
        let body = json!({
            "ModelPackageName": name,
            "InferenceSpecification": inference_specification(std::slice::from_ref(&image)),
        });
        let (status, answered) = emulator.call_in(region, "CreateModelPackage", &body).await;

        match &refused {
            Some((registry_id, repository_name)) => {
                assert_eq!(
                    (status, error_code(&answered).as_str(), message(&answered).as_str()),
                    (400, "ValidationException", access_denied(registry_id, repository_name).as_str()),
                    "case {case:?} ({image}): expected the pull refusal, got {status} with \
                     {answered}"
                );
                // A refused registration stores nothing: the identifier answers to no
                // package afterwards.
                let (status, read) = emulator
                    .call_in(region, "DescribeModelPackage", &json!({"ModelPackageName": name}))
                    .await;
                assert_eq!(
                    status, 400,
                    "case {case:?} ({image}): a refused registration must register nothing, \
                     got {status} with {read}"
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?} ({image}): expected the registration to be admitted, got \
                     {status} with {answered}"
                );
                assert_eq!(
                    str_field(&answered, "ModelPackageArn"),
                    expected_arn(region, &name),
                    "case {case:?} ({image}): the registered package's ARN, got {answered}"
                );
                // And it reads back through the registry's own reader, around the image it
                // was registered with.
                let (status, described) = emulator
                    .call_in(region, "DescribeModelPackage", &json!({"ModelPackageName": name}))
                    .await;
                assert_eq!(
                    status, 200,
                    "case {case:?} ({image}): the registration must read back, got {status} \
                     with {described}"
                );
                assert_eq!(
                    described
                        .pointer("/InferenceSpecification/Containers/0/Image")
                        .and_then(Value::as_str),
                    Some(image.as_str()),
                    "case {case:?}: the stored container's Image, got {described}"
                );
            }
        }
    }
}

/// One row of the position table: the case, the inference specification's container images,
/// an additional specification's container images, and the registry and repository the
/// refusal must name — `None` for an admitted request.
struct PositionRow {
    case: &'static str,
    inference: Vec<String>,
    additional: Vec<String>,
    refused: Option<(String, String)>,
}

/// RULE: the pull applies to every container image the registration carries, wherever the
/// member sits — the inference specification's containers and an additional specification's —
/// and the refusal names the FIRST one the request cannot pull.
#[tokio::test]
async fn the_first_unpullable_image_of_a_registration_is_the_one_named() {
    let emulator = Emulator::new();
    let region = state::DEFAULT_REGION;
    let pullable = format!("{}/inference:1", registry(ACCOUNT, region));
    let other_account = format!("{}/first:1", registry(OTHER_ACCOUNT, region));
    let other_region = format!("{}/second:2", registry(ACCOUNT, "us-west-2"));

    let rows: Vec<PositionRow> = vec![
        PositionRow {
            case: "every container pullable",
            inference: vec![pullable.clone(), pullable.clone()],
            additional: vec![pullable.clone()],
            refused: None,
        },
        PositionRow {
            case: "the first inference container",
            inference: vec![other_account.clone(), other_region.clone()],
            additional: vec![pullable.clone()],
            refused: Some((OTHER_ACCOUNT.to_string(), "first".to_string())),
        },
        PositionRow {
            case: "a later inference container",
            inference: vec![pullable.clone(), other_region.clone()],
            additional: vec![other_account.clone()],
            refused: Some((ACCOUNT.to_string(), "second".to_string())),
        },
        PositionRow {
            case: "an additional specification's container alone",
            inference: vec![pullable.clone()],
            additional: vec![pullable.clone(), other_account.clone()],
            refused: Some((OTHER_ACCOUNT.to_string(), "first".to_string())),
        },
    ];

    for (
        index,
        PositionRow {
            case,
            inference,
            additional,
            refused,
        },
    ) in rows.into_iter().enumerate()
    {
        let name = format!("position{index}");
        let body = json!({
            "ModelPackageName": name,
            "InferenceSpecification": inference_specification(&inference),
            "AdditionalInferenceSpecifications": [{
                "Name": "extra",
                "Containers": additional.iter().map(|image| container(image)).collect::<Vec<Value>>(),
            }],
        });
        let (status, answered) = emulator.call_in(region, "CreateModelPackage", &body).await;
        match &refused {
            Some((registry_id, repository_name)) => assert_eq!(
                (status, error_code(&answered).as_str(), message(&answered).as_str()),
                (400, "ValidationException", access_denied(registry_id, repository_name).as_str()),
                "case {case:?}: expected the pull refusal, got {status} with {answered}"
            ),
            None => assert_eq!(
                status, 200,
                "case {case:?}: expected the registration to be admitted, got {status} with \
                 {answered}"
            ),
        }
    }
}

/// RULE: the pull is asked of a VERSIONED registration too — the kind of package the request
/// asks for does not change which images SageMaker can pull — and a refused version joins no
/// group, so the group's next version is still the first one.
#[tokio::test]
async fn a_versioned_registration_is_held_to_the_same_pull() {
    let emulator = Emulator::new();
    let region = state::DEFAULT_REGION;
    let group = "pullgroup";
    emulator.create_group(region, group).await;

    let unpullable = format!("{}/inference:1", registry(OTHER_ACCOUNT, region));
    let (status, answered) = emulator
        .call_in(
            region,
            "CreateModelPackage",
            &json!({
                "ModelPackageGroupName": group,
                "InferenceSpecification": inference_specification(std::slice::from_ref(&unpullable)),
            }),
        )
        .await;
    assert_eq!(
        (status, error_code(&answered).as_str(), message(&answered).as_str()),
        (
            400,
            "ValidationException",
            access_denied(OTHER_ACCOUNT, "inference").as_str()
        ),
        "case \"a version around an image this request cannot pull\": expected the pull \
         refusal, got {status} with {answered}"
    );

    // Nothing joined the group: the next registration is version 1.
    let pullable = format!("{}/inference:1", registry(ACCOUNT, region));
    let (status, created) = emulator
        .call_in(
            region,
            "CreateModelPackage",
            &json!({
                "ModelPackageGroupName": group,
                "InferenceSpecification": inference_specification(&[pullable]),
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case \"the next version after a refused one\": expected the registration to be \
         admitted, got {status} with {created}"
    );
    assert_eq!(
        str_field(&created, "ModelPackageArn"),
        expected_arn(region, &format!("{group}/1")),
        "case \"the next version after a refused one\": the version a refused registration \
         left free, got {created}"
    );
}
