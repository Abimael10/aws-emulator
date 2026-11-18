// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for CODE SIGNING against a CONTAINER-IMAGE package.
//!
//! RULE: "You can use code signing if your deployment package is a .zip file archive" (the
//! model's own `CreateFunction` documentation) — the signature a code-signing
//! configuration's checks are made against is borne by a .zip package, while a container
//! image is a reference into a registry the function is pinned to the manifest digest of, so
//! a request that would put a code-signing configuration on an image function names a
//! control Lambda cannot apply. It is refused with the modelled
//! `InvalidParameterValueException` (HTTP 400) and the one sentence `Code signing is not
//! supported for functions created with container images.`, nothing is stored, and the
//! refusal is made on the PARAMETERS alone: it precedes resolving the image, so a request
//! that pairs the two is never told to fix its image URI instead.
//!
//! The rows differ along what the rule generalises over: WHICH operation forms the pairing
//! (the create that asks for both at once, and the association put on a function already
//! deployed from an image), and — for the create — what the image reference would have
//! resolved to (an image really pushed to this Region's registry, a tag nothing was pushed
//! under, a repository the registry does not hold, and a URI that is no registry URI at
//! all), which is what makes the ordering observable. The rows beside them are the control:
//! a .zip create naming the same configuration is served, and so is an image create naming
//! none.
//!
//! Every row drives real HTTP requests through the production `MultiServiceRouter`, and the
//! images it resolves are pushed through Amazon ECR's own surface on the same engine
//! (`function_lifecycle::seed_image`), so what a create resolves is what the registry
//! minted. The outcome is read back through `GetFunction` and
//! `GetFunctionCodeSigningConfig`, so a refusal that nevertheless changed the store fails
//! here.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{
    create_function, json_call, make_activation_due, package_bytes, router_on, seed_image,
    zip_file, Answer, ACCOUNT, REGION, ROLE,
};

/// Where `CreateCodeSigningConfig` is bound
/// (`smithy_query(query="operation", operation="CreateCodeSigningConfig")`:
/// `POST /2020-04-22/code-signing-configs`).
const CODE_SIGNING_CONFIGS_URI: &str = "/2020-04-22/code-signing-configs";

/// The modelled error every refusal in this suite carries
/// (`smithy_query(query="shape", shape="InvalidParameterValueException")`: `httpError 400`).
const INVALID_PARAMETER: &str = "InvalidParameterValueException";

/// The one sentence Lambda answers for a code-signing configuration asked for on a
/// container-image function.
const UNSUPPORTED: &str = "Code signing is not supported for functions created with container \
                           images.";

/// The `message` of a wire error, under either casing the JSON envelopes use.
fn message(answer: &Answer) -> String {
    answer
        .string("message")
        .or_else(|| answer.string("Message"))
        .unwrap_or_default()
}

/// A signing profile version ARN satisfying the model's `Arn` pattern, in the form AWS
/// Signer vends one.
fn signing_profile(index: usize) -> String {
    format!("arn:aws:signer:{REGION}:{ACCOUNT}:/signing-profiles/Profile{index}/{index:08x}")
}

/// `CreateCodeSigningConfig`, answering the ARN of the configuration it persisted.
async fn create_config(router: &MultiServiceRouter) -> String {
    let answer = json_call(
        router,
        "POST",
        CODE_SIGNING_CONFIGS_URI,
        &json!({
            "AllowedPublishers": { "SigningProfileVersionArns": [signing_profile(1)] },
            "Description": "the configuration this suite asks for",
        }),
    )
    .await;
    assert_eq!(
        answer.status,
        201,
        "CreateCodeSigningConfig must answer 201, got {} {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    answer.json()["CodeSigningConfig"]["CodeSigningConfigArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "CreateCodeSigningConfig must answer an ARN, got {}",
                String::from_utf8_lossy(&answer.body)
            )
        })
        .to_string()
}

/// The registry URI of the repository `path` in the Region the harness signs for.
fn repository_uri(path: &str) -> String {
    format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/{path}")
}

/// `GetFunction` on a function, with no qualifier.
async fn get_function(router: &MultiServiceRouter, name: &str) -> Answer {
    json_call(
        router,
        "GET",
        &format!("/2015-03-31/functions/{name}"),
        &Value::Null,
    )
    .await
}

/// `PutFunctionCodeSigningConfig` / `GetFunctionCodeSigningConfig`
/// (`PUT|GET /2020-06-30/functions/{FunctionName}/code-signing-config`).
fn association_uri(name: &str) -> String {
    format!("/2020-06-30/functions/{name}/code-signing-config")
}

/// What the image reference of a create row would have resolved to.
#[derive(Clone, Copy, Debug)]
enum Reference {
    /// An image really pushed to this Region's registry.
    Pushed,
    /// A tag of a held repository nothing was pushed under.
    AbsentTag,
    /// A repository the registry does not hold at all.
    AbsentRepository,
    /// A URI that is no registry URI of this Region.
    NoRegistry,
}

/// One row of the create table.
struct CreateCase {
    case: &'static str,
    function: &'static str,
    /// The package the create deploys: an image reference, or a .zip archive.
    reference: Option<Reference>,
    /// Whether the create names the code-signing configuration.
    signs: bool,
    /// Whether the create is refused with the code-signing sentence.
    refused: bool,
}

fn create_cases() -> Vec<CreateCase> {
    vec![
        CreateCase {
            case: "an image that really resolves, with a configuration",
            function: "csc-image-held",
            reference: Some(Reference::Pushed),
            signs: true,
            refused: true,
        },
        CreateCase {
            // The ordering: the reference would have been refused for not existing, and the
            // combination is refused before it is followed.
            case: "a tag nothing was pushed under, with a configuration",
            function: "csc-image-absent-tag",
            reference: Some(Reference::AbsentTag),
            signs: true,
            refused: true,
        },
        CreateCase {
            case: "a repository the registry does not hold, with a configuration",
            function: "csc-image-absent-repository",
            reference: Some(Reference::AbsentRepository),
            signs: true,
            refused: true,
        },
        CreateCase {
            case: "a URI that is no registry URI at all, with a configuration",
            function: "csc-image-no-registry",
            reference: Some(Reference::NoRegistry),
            signs: true,
            refused: true,
        },
        CreateCase {
            case: "an image that resolves, naming no configuration",
            function: "csc-image-unsigned",
            reference: Some(Reference::Pushed),
            signs: false,
            refused: false,
        },
        CreateCase {
            case: "a .zip archive, with a configuration, which is what code signing covers",
            function: "csc-zip-signed",
            reference: None,
            signs: true,
            refused: false,
        },
    ]
}

/// The `Code` member and `PackageType` this row's create carries.
fn package(reference: Option<Reference>) -> (Value, Option<&'static str>) {
    match reference {
        None => (json!({ "ZipFile": zip_file(&package_bytes(64)) }), None),
        Some(Reference::Pushed) => (
            json!({ "ImageUri": format!("{}:v1", repository_uri("csc-held")) }),
            Some("Image"),
        ),
        Some(Reference::AbsentTag) => (
            json!({ "ImageUri": format!("{}:never", repository_uri("csc-held")) }),
            Some("Image"),
        ),
        Some(Reference::AbsentRepository) => (
            json!({ "ImageUri": format!("{}:v1", repository_uri("csc-absent")) }),
            Some("Image"),
        ),
        Some(Reference::NoRegistry) => (
            json!({ "ImageUri": "docker.io/library/alpine:3" }),
            Some("Image"),
        ),
    }
}

#[tokio::test]
async fn a_create_pairing_a_code_signing_configuration_with_an_image_is_refused_for_that() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let config = create_config(&router).await;
    // One image really pushed to this Region's registry, so the rows whose reference would
    // have resolved are not refused for the reference.
    seed_image(&engine, &format!("{}:v1", repository_uri("csc-held"))).await;

    for case in create_cases() {
        let label = case.case;
        let (code, package_type) = package(case.reference);
        let mut body = json!({
            "FunctionName": case.function,
            "Role": ROLE,
            "Code": code,
        });
        if let Some(package_type) = package_type {
            body["PackageType"] = json!(package_type);
        } else {
            body["Runtime"] = json!("python3.12");
            body["Handler"] = json!("index.handler");
        }
        if case.signs {
            body["CodeSigningConfigArn"] = json!(config.clone());
        }
        let answer = create_function(&router, &body).await;

        if !case.refused {
            assert_eq!(
                answer.status,
                201,
                "case {label:?}: expected 201 from CreateFunction, got {} {}",
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
            make_activation_due(&engine);
            continue;
        }

        assert_eq!(
            answer.status,
            400,
            "case {label:?}: expected HTTP 400, got {} {}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        assert_eq!(
            answer.error_code(),
            INVALID_PARAMETER,
            "case {label:?}: expected {INVALID_PARAMETER}, got {}",
            answer.error_code()
        );
        assert_eq!(
            message(&answer),
            UNSUPPORTED,
            "case {label:?}: expected {UNSUPPORTED:?}, got {:?}",
            message(&answer)
        );
        // Nothing was created: the refusal is instead of the function, not beside it.
        let read = get_function(&router, case.function).await;
        assert_eq!(
            read.status,
            404,
            "case {label:?}: a refused create must leave no function behind, but GetFunction \
             answered {} {}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
    }
}

#[tokio::test]
async fn an_association_put_on_an_image_function_is_refused_and_a_zip_function_keeps_it() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let config = create_config(&router).await;
    seed_image(&engine, &format!("{}:v1", repository_uri("csc-put-held"))).await;

    // (case, the function, the package it is deployed from, whether the put is refused)
    let rows: Vec<(&str, &str, Value, Option<&str>, bool)> = vec![
        (
            "a function deployed from a container image",
            "csc-put-image",
            json!({ "ImageUri": format!("{}:v1", repository_uri("csc-put-held")) }),
            Some("Image"),
            true,
        ),
        (
            "a function deployed from a .zip archive",
            "csc-put-zip",
            json!({ "ZipFile": zip_file(&package_bytes(96)) }),
            None,
            false,
        ),
    ];

    for (label, function, code, package_type, refused) in rows {
        let mut body = json!({ "FunctionName": function, "Role": ROLE, "Code": code });
        if let Some(package_type) = package_type {
            body["PackageType"] = json!(package_type);
        } else {
            body["Runtime"] = json!("python3.12");
            body["Handler"] = json!("index.handler");
        }
        let created = create_function(&router, &body).await;
        assert_eq!(
            created.status,
            201,
            "case {label:?}: expected 201 from CreateFunction, got {} {}",
            created.status,
            String::from_utf8_lossy(&created.body)
        );
        make_activation_due(&engine);

        let put = json_call(
            &router,
            "PUT",
            &association_uri(function),
            &json!({ "CodeSigningConfigArn": config.clone() }),
        )
        .await;
        let read = json_call(&router, "GET", &association_uri(function), &Value::Null).await;

        if refused {
            assert_eq!(
                put.status,
                400,
                "case {label:?}: expected HTTP 400, got {} {}",
                put.status,
                String::from_utf8_lossy(&put.body)
            );
            assert_eq!(
                put.error_code(),
                INVALID_PARAMETER,
                "case {label:?}: expected {INVALID_PARAMETER}, got {}",
                put.error_code()
            );
            assert_eq!(
                message(&put),
                UNSUPPORTED,
                "case {label:?}: expected {UNSUPPORTED:?}, got {:?}",
                message(&put)
            );
            assert_eq!(
                read.string("CodeSigningConfigArn"),
                None,
                "case {label:?}: a refused put must leave the function holding no \
                 association, got {}",
                String::from_utf8_lossy(&read.body)
            );
            continue;
        }

        assert_eq!(
            put.status,
            200,
            "case {label:?}: expected HTTP 200, got {} {}",
            put.status,
            String::from_utf8_lossy(&put.body)
        );
        assert_eq!(
            read.string("CodeSigningConfigArn"),
            Some(config.clone()),
            "case {label:?}: expected the association to read back, got {}",
            String::from_utf8_lossy(&read.body)
        );
    }
}
