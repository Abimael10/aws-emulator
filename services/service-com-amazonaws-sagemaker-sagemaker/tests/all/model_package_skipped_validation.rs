// SPDX-License-Identifier: CC0-1.0

//! What a `CreateModelPackage` that asks for its model validation to be SKIPPED is admitted
//! around, and how a retry presenting a SPENT `ClientToken` is worded — both driven end to
//! end over the real wire path (an awsJson1_1 request through the multi-service router into
//! the mounted service), so what is asserted is what a client sees.
//!
//! RULE 1: `SkipModelValidation` set to `All` ("Indicates if you want to skip model
//! validation"; the enum's members are `All` and `None`) turns off the validation SageMaker
//! otherwise performs on the model a registration registers — including resolving the Amazon
//! ECR containers its inference specifications point at — so such a registration is admitted
//! around whatever image path it names, while the same request with `None` or with the member
//! absent is still held to the pull the request can make. The images the two answers differ
//! over are the ones `model_package_image_pull_admission` tables: Amazon ECR is per account
//! and per Region, so a path addressing any registry other than this account's own in the
//! request's Region is one this request cannot pull.
//!
//! RULE 2: the refusal of a retry presenting a spent `ClientToken` states the window the
//! token stays spent for as "the idempotency window" and interpolates only the identity the
//! retry asked for — the model package group a version would join, or the name an unversioned
//! package would be registered under — so the sentence asserts no duration. The token is
//! spent for the ACCOUNT: it identifies one call of the operation, so a later request
//! presenting it is a reuse of that call whichever package it names.
//!
//! The rows span what each rule generalises over: for rule 1 the registry's account and
//! Region against the request's, the tag-versus-digest form, the repository's depth, the
//! member the container sits in, the kind of package registered and each `SkipModelValidation`
//! value (including its absence); for rule 2 the kind of package the token registered, the
//! name shape its identity clause carries, the token's own form at the modelled length bounds
//! and what the retry does.
//!
//! The harness is the one `model_package_registry` declares, and the registry host and the
//! pull refusal's sentence are the ones `model_package_image_pull_admission` states, so these
//! cases drive the same router, and assert the same wording, as every other model-package
//! suite. The status such a registration is reported in — through every reader of it — is
//! tabled by `model_package_registration_status`, which carries a row for the image only this
//! skip admits.

use serde_json::{json, Value};

use service_com_amazonaws_sagemaker_sagemaker::state;

use crate::model_artifact_admission::message;
use crate::model_package_image_pull_admission::{access_denied, registry};
use crate::model_package_registry::{container, error_code, expected_arn, str_field, Emulator};

/// The canonical account id this emulator serves, whose Amazon ECR registry is the one
/// SageMaker pulls from (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// Another account's id, in the 12-digit form `registryId` documents (`^[0-9]{12}$`) — one of
/// the reserved example account ids, so it names no real registry.
const OTHER_ACCOUNT: &str = "111122223333";

/// A second one, so a row can vary the registry the path addresses without varying anything
/// else.
const THIRD_ACCOUNT: &str = "444455556666";

/// How SageMaker words a retry presenting a spent idempotency token: fixed text with only the
/// registration's identity clause interpolated.
fn spent_token_refusal(identity: &str) -> String {
    format!(
        "Specified idempotent client token in Model package with {identity} was used with \
         different request parameters within the idempotency window."
    )
}

/// An `InferenceSpecification` whose one container names `image`.
fn inference_specification(image: &str) -> Value {
    json!({
        "Containers": [container(image)],
        "SupportedContentTypes": ["application/json"],
        "SupportedResponseMIMETypes": ["application/json"],
    })
}

/// One row of the skip table: the image the registration points at, where the container sits,
/// which kind of package is registered, and the registry and repository a refusal must name
/// when this request has to pull the image (`None` for a path it can pull anyway).
struct SkipRow {
    case: &'static str,
    region: &'static str,
    image: String,
    /// `true` when the container sits in an `AdditionalInferenceSpecifications` entry rather
    /// than in `InferenceSpecification`.
    additional: bool,
    /// `true` when the registration joins a model package group as a version.
    versioned: bool,
    unpullable: Option<(String, String)>,
}

fn skip_rows() -> Vec<SkipRow> {
    let default = state::DEFAULT_REGION;
    vec![
        SkipRow {
            case: "another account's registry in the request's Region",
            region: default,
            image: format!("{}/inference:1", registry(OTHER_ACCOUNT, default)),
            additional: false,
            versioned: false,
            unpullable: Some((OTHER_ACCOUNT.to_string(), "inference".to_string())),
        },
        SkipRow {
            case: "this account's registry in ANOTHER Region",
            region: default,
            image: format!("{}/inference:1", registry(ACCOUNT, "us-west-2")),
            additional: false,
            versioned: true,
            unpullable: Some((ACCOUNT.to_string(), "inference".to_string())),
        },
        SkipRow {
            case: "another account's registry, a namespaced repository and a digest",
            region: default,
            image: format!("{}/team/inference@sha256:abc", registry(THIRD_ACCOUNT, default)),
            additional: false,
            versioned: false,
            unpullable: Some((THIRD_ACCOUNT.to_string(), "team/inference".to_string())),
        },
        SkipRow {
            case: "another account's registry, in an additional specification's container",
            region: default,
            image: format!("{}/extra:2", registry(OTHER_ACCOUNT, default)),
            additional: true,
            versioned: true,
            unpullable: Some((OTHER_ACCOUNT.to_string(), "extra".to_string())),
        },
        SkipRow {
            case: "another account's registry, a request signed for another Region",
            region: "eu-west-1",
            image: format!("{}/inference:3", registry(OTHER_ACCOUNT, "eu-west-1")),
            additional: false,
            versioned: false,
            unpullable: Some((OTHER_ACCOUNT.to_string(), "inference".to_string())),
        },
        SkipRow {
            case: "this account's registry in the request's Region, no tag at all",
            region: default,
            image: format!("{}/inference", registry(ACCOUNT, default)),
            additional: false,
            versioned: false,
            unpullable: None,
        },
        SkipRow {
            case: "a host that addresses no Amazon ECR registry",
            region: default,
            image: "registry.example.com/team/inference:1".to_string(),
            additional: true,
            versioned: false,
            unpullable: None,
        },
    ]
}

/// The create body for one row: the package it names, and the container in the member the row
/// puts it in. A row whose container sits in an additional specification still carries an
/// `InferenceSpecification`, around an image this request can pull, so what the case varies is
/// only where the row's own image sits.
fn skip_body(row: &SkipRow, identifier: &str, skip: Option<&str>) -> Value {
    let pullable = format!("{}/base:1", registry(ACCOUNT, row.region));
    let mut body = if row.versioned {
        json!({"ModelPackageGroupName": identifier})
    } else {
        json!({"ModelPackageName": identifier})
    };
    if row.additional {
        body["InferenceSpecification"] = inference_specification(&pullable);
        body["AdditionalInferenceSpecifications"] = json!([{
            "Name": "extra",
            "Containers": [container(&row.image)],
        }]);
    } else {
        body["InferenceSpecification"] = inference_specification(&row.image);
    }
    if let Some(skip) = skip {
        body["SkipModelValidation"] = json!(skip);
    }
    body
}

/// RULE: a registration whose `SkipModelValidation` is `All` is admitted around whatever
/// container image path it names — the pull SageMaker would otherwise have to make is part of
/// the validation that member turns off — while the same registration with `None`, or with the
/// member absent, is refused 400 `ValidationException` naming the registry and repository it
/// cannot pull. What is admitted reads back around the image it was registered with.
#[tokio::test]
async fn skipping_model_validation_admits_a_registration_around_any_image() {
    let emulator = Emulator::new();
    // (case, the `SkipModelValidation` the request carries)
    let skips = [
        ("skipped entirely", Some("All")),
        ("not skipped", Some("None")),
        ("the member absent", None),
    ];

    for (row_index, row) in skip_rows().into_iter().enumerate() {
        for (skip_index, (skip_case, skip)) in skips.iter().enumerate() {
            let case = format!("{} / {skip_case}", row.case);
            let identifier = format!("skip{row_index}x{skip_index}");
            if row.versioned {
                emulator.create_group(row.region, &identifier).await;
            }
            let body = skip_body(&row, &identifier, *skip);
            let (status, answered) = emulator
                .call_in(row.region, "CreateModelPackage", &body)
                .await;

            // The refusal stands only where the request did NOT skip its model validation and
            // the image is one this request cannot pull.
            let refused = match (&row.unpullable, skip) {
                (Some(unpullable), Some("None") | None) => Some(unpullable),
                _ => None,
            };
            match refused {
                Some((registry_id, repository_name)) => {
                    assert_eq!(
                        (status, error_code(&answered).as_str(), message(&answered).as_str()),
                        (
                            400,
                            "ValidationException",
                            access_denied(registry_id, repository_name).as_str()
                        ),
                        "case {case:?} ({}): expected the pull refusal, got {status} with \
                         {answered}",
                        row.image
                    );
                    // A refused registration stores nothing.
                    let read_identifier = if row.versioned {
                        format!("{identifier}/1")
                    } else {
                        identifier.clone()
                    };
                    let (status, read) = emulator
                        .call_in(
                            row.region,
                            "DescribeModelPackage",
                            &json!({"ModelPackageName": read_identifier}),
                        )
                        .await;
                    assert_eq!(
                        status, 400,
                        "case {case:?}: a refused registration must register nothing, got \
                         {status} with {read}"
                    );
                }
                None => {
                    assert_eq!(
                        status, 200,
                        "case {case:?} ({}): expected the registration to be admitted, got \
                         {status} with {answered}",
                        row.image
                    );
                    let resource = if row.versioned {
                        format!("{identifier}/1")
                    } else {
                        identifier.clone()
                    };
                    assert_eq!(
                        str_field(&answered, "ModelPackageArn"),
                        expected_arn(row.region, &resource),
                        "case {case:?}: the registered package's ARN, got {answered}"
                    );
                    // And it reads back around the image it was registered with, through the
                    // registry's own reader.
                    let (status, described) = emulator
                        .call_in(
                            row.region,
                            "DescribeModelPackage",
                            &json!({"ModelPackageName": resource}),
                        )
                        .await;
                    assert_eq!(
                        status, 200,
                        "case {case:?}: the registration must read back, got {status} with \
                         {described}"
                    );
                    let pointer = if row.additional {
                        "/AdditionalInferenceSpecifications/0/Containers/0/Image"
                    } else {
                        "/InferenceSpecification/Containers/0/Image"
                    };
                    assert_eq!(
                        described.pointer(pointer).and_then(Value::as_str),
                        Some(row.image.as_str()),
                        "case {case:?}: the stored container's Image, got {described}"
                    );
                }
            }
        }
    }
}

/// One row of the spent-token table: the package the token registers, and the identity clause
/// the refusal must interpolate.
struct TokenRow {
    case: &'static str,
    /// Which package the registration names, which is what decides the identity clause.
    package: Package,
    /// The `ClientToken` the registration presents (`@length 1..36`, `^[a-zA-Z0-9-]+$`).
    token: String,
}

/// The two kinds of model package a registration can name, which are mutually exclusive:
/// `CreateModelPackage` takes `ModelPackageGroupName` for a version or `ModelPackageName` for
/// an unversioned package, never both.
enum Package {
    /// A version joining the model package group of this name.
    Version { group: &'static str },
    /// An unversioned package registered under this name.
    Unversioned { name: &'static str },
}

impl TokenRow {
    /// The clause SageMaker names the registration's identity with.
    fn identity(&self) -> String {
        match self.package {
            Package::Version { group } => format!("Group:{group}"),
            Package::Unversioned { name } => format!("Name:{name}"),
        }
    }

    fn members(&self) -> Value {
        match self.package {
            Package::Version { group } => json!({"ModelPackageGroupName": group}),
            Package::Unversioned { name } => json!({"ModelPackageName": name}),
        }
    }
}

/// RULE: a retry presenting a `ClientToken` this account has SPENT is refused 400
/// `ConflictException` whose message is SageMaker's fixed sentence with only the
/// registration's identity clause interpolated — it names the window as "the idempotency
/// window" and states no duration of its own.
#[tokio::test]
async fn a_spent_client_token_refusal_names_the_idempotency_window() {
    let emulator = Emulator::new();
    let region = state::DEFAULT_REGION;
    for group in ["windowgroup", "g", "models-2024"] {
        emulator.create_group(region, group).await;
    }

    let rows = vec![
        TokenRow {
            case: "a version in a group, a single-character token",
            package: Package::Version { group: "windowgroup" },
            token: "t".to_string(),
        },
        TokenRow {
            case: "a version in a single-character group, a token at the modelled maximum",
            package: Package::Version { group: "g" },
            // 36 characters, the upper bound `ClientToken` publishes (`@length 1..36`).
            token: "a".repeat(36),
        },
        TokenRow {
            case: "a version in a hyphenated group, a mixed-case token",
            package: Package::Version { group: "models-2024" },
            token: "Token-9F-b2".to_string(),
        },
        TokenRow {
            case: "an unversioned package",
            package: Package::Unversioned { name: "window-package" },
            token: "unversioned-token-1".to_string(),
        },
        TokenRow {
            case: "an unversioned package with a single-character name",
            package: Package::Unversioned { name: "w" },
            token: "unversioned-token-2".to_string(),
        },
    ];

    for row in rows {
        let mut body = row.members();
        body["ClientToken"] = json!(row.token);
        body["InferenceSpecification"] =
            inference_specification(&format!("{}/a:1", registry(ACCOUNT, region)));

        let registered = emulator.create(row.case, &body).await;
        assert!(
            !registered.is_empty(),
            "case {:?}: the first registration must answer with an ARN, got {registered:?}",
            row.case
        );

        // Every retry presenting the spent token is refused with the same sentence: a
        // byte-identical one, one that differs in a member, and — once the registration is
        // gone — the identical one again.
        let differing = {
            let mut differing = body.clone();
            differing["ModelPackageDescription"] = json!("a second registration");
            differing
        };
        let expected = spent_token_refusal(&row.identity());
        for (retry, request) in [
            ("the same request again", body.clone()),
            ("a request that differs", differing),
        ] {
            let (status, answered) = emulator.call("CreateModelPackage", &request).await;
            assert_eq!(
                (status, error_code(&answered).as_str(), message(&answered).as_str()),
                (400, "ConflictException", expected.as_str()),
                "case {:?} / {retry:?}: expected the spent-token refusal, got {status} with \
                 {answered}",
                row.case
            );
            assert!(
                !message(&answered).contains("hours"),
                "case {:?} / {retry:?}: the refusal must state no window duration, got {:?}",
                row.case,
                message(&answered)
            );
        }
    }
}

/// RULE: a spent `ClientToken` is spent for the ACCOUNT, not for one package — an
/// idempotency token identifies one call of the operation, so a later request presenting it
/// is refused whichever package it names, and the refusal's sentence interpolates the
/// identity THAT request asked for.
#[tokio::test]
async fn a_spent_client_token_is_refused_whichever_package_the_retry_names() {
    let emulator = Emulator::new();
    let region = state::DEFAULT_REGION;
    for group in ["windowfirst", "windowother"] {
        emulator.create_group(region, group).await;
    }
    let token = "shared-token-1";
    let pullable = format!("{}/a:1", registry(ACCOUNT, region));

    // The token is spent registering a version in one group.
    let registered = emulator
        .create(
            "the call that spends the token",
            &json!({
                "ModelPackageGroupName": "windowfirst",
                "ClientToken": token,
                "InferenceSpecification": inference_specification(&pullable),
            }),
        )
        .await;
    assert_eq!(
        registered,
        expected_arn(region, "windowfirst/1"),
        "case \"the call that spends the token\": expected the first version's ARN, got \
         {registered:?}"
    );

    // (case, the members the retry names, the identity clause its refusal must carry, the
    // identifier that must answer to no package afterwards)
    let rows = [
        (
            "another group",
            json!({"ModelPackageGroupName": "windowother"}),
            "Group:windowother",
            "windowother/1",
        ),
        (
            "the same group again",
            json!({"ModelPackageGroupName": "windowfirst"}),
            "Group:windowfirst",
            "windowfirst/2",
        ),
        (
            "an unversioned package",
            json!({"ModelPackageName": "window-elsewhere"}),
            "Name:window-elsewhere",
            "window-elsewhere",
        ),
    ];
    for (case, members, identity, absent) in rows {
        let mut body = members;
        body["ClientToken"] = json!(token);
        body["InferenceSpecification"] = inference_specification(&pullable);
        let (status, answered) = emulator.call("CreateModelPackage", &body).await;
        assert_eq!(
            (status, error_code(&answered).as_str(), message(&answered).as_str()),
            (400, "ConflictException", spent_token_refusal(identity).as_str()),
            "case {case:?}: expected the spent-token refusal, got {status} with {answered}"
        );
        let (status, read) = emulator
            .call("DescribeModelPackage", &json!({"ModelPackageName": absent}))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: the refused retry must register nothing, got {status} with {read}"
        );
    }

    // A FRESH token registers normally afterwards, so what the rule refuses is the reuse and
    // nothing else.
    let fresh = emulator
        .create(
            "a fresh token after a spent one",
            &json!({
                "ModelPackageGroupName": "windowother",
                "ClientToken": "shared-token-2",
                "InferenceSpecification": inference_specification(&pullable),
            }),
        )
        .await;
    assert_eq!(
        fresh,
        expected_arn(region, "windowother/1"),
        "case \"a fresh token after a spent one\": expected the first version's ARN, got \
         {fresh:?}"
    );
}
