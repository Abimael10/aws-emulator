// SPDX-License-Identifier: CC0-1.0

//! A parameter only ONE package type admits, carried to a function of the other one.
//!
//! The RULE under test: a request naming a configuration parameter that is meaningful only
//! for the other deployment-package type than the function's is refused
//! `400 InvalidParameterValueException` with the one sentence Lambda words for that family —
//! `The <Parameter> parameter is not supported for functions created with container images.`
//! for a parameter a container-image function has no use for, and `... with deployment
//! packages.` for one a .zip function has no use for — and nothing is written.
//!
//! Which parameters belong to which type comes from the model's own member documentation
//! (`smithy_query(query="doc", shape="CreateFunctionRequest", member="Runtime" |
//! "Handler" | "ImageConfig")`):
//!
//! * `Runtime` is "required if the deployment package is a .zip file archive. Specifying a
//!   runtime results in an error if you're deploying a function using a container image";
//! * `Handler` names the method a managed runtime calls, and for a container image
//!   `CreateFunction` states "You do not need to specify the handler and runtime
//!   properties", because the image's own entry point is what runs;
//! * `ImageConfig` is "Container image configuration values that override the values in the
//!   container image Dockerfile", so a .zip function has no image to override.
//!
//! The rows differ along what the rule generalises over: which parameter is carried, which
//! way round the mismatch goes, whether the package type is stated or defaulted, how many
//! mismatched parameters one request carries, and which of the two writes that accept these
//! members makes the mistake (`CreateFunction` and `UpdateFunctionConfiguration`). Each
//! table also carries the matching rows that must be SERVED, so the refusal is the mismatch
//! and not the member.
//!
//! The harness (the `Answer` wrapper, `json_call`, the registry seeding and the activation
//! window) is the one `function_lifecycle.rs` publishes; nothing here sleeps.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{
    create_function, json_call, make_activation_due, make_update_due, package_bytes, router_on,
    seed_image, zip_file, Answer, ROLE,
};

/// The error code every refusal in this suite carries: `CreateFunction` and
/// `UpdateFunctionConfiguration` both declare `InvalidParameterValueException` (400).
const INVALID_PARAMETER: &str = "InvalidParameterValueException";

/// The container image the `Image`-package functions of this suite are created on. It is
/// pushed to the registry of the same engine first, because a create RESOLVES its
/// reference there.
const IMAGE_URI: &str = "000000000000.dkr.ecr.us-east-1.amazonaws.com/package-type:latest";

/// The `message` of a wire error, under either casing the JSON envelopes use.
fn message(answer: &Answer) -> Option<String> {
    answer.string("message").or_else(|| answer.string("Message"))
}

/// The sentence a parameter carried to a container-image function is refused with.
fn unsupported_for_image(parameter: &str) -> String {
    format!(
        "The {parameter} parameter is not supported for functions created with container \
         images."
    )
}

/// The sentence a parameter carried to a .zip-package function is refused with.
fn unsupported_for_zip(parameter: &str) -> String {
    format!(
        "The {parameter} parameter is not supported for functions created with deployment \
         packages."
    )
}

/// An `ImageConfig` naming every member the shape carries, so a row that supplies one
/// supplies a complete override.
fn image_config() -> Value {
    json!({
        "EntryPoint": ["/usr/bin/app"],
        "Command": ["handler"],
        "WorkingDirectory": "/var/task",
    })
}

/// `GetFunctionConfiguration` on a function, with no qualifier.
async fn get_configuration(router: &MultiServiceRouter, name: &str) -> Answer {
    let uri = format!("/2015-03-31/functions/{name}/configuration");
    json_call(router, "GET", &uri, &Value::Null).await
}

/// `UpdateFunctionConfiguration` on a function.
async fn update_configuration(router: &MultiServiceRouter, name: &str, patch: &Value) -> Answer {
    let uri = format!("/2015-03-31/functions/{name}/configuration");
    json_call(router, "PUT", &uri, patch).await
}

/// Assert one refusal: the status, the error code and the whole sentence.
fn assert_refused(case: &str, answer: &Answer, expected: &str) {
    assert_eq!(
        answer.status, 400,
        "case {case:?}: expected 400, got {} {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    assert_eq!(
        answer.error_code(),
        INVALID_PARAMETER,
        "case {case:?}: expected {INVALID_PARAMETER}, got {}",
        answer.error_code()
    );
    assert_eq!(
        message(answer).as_deref(),
        Some(expected),
        "case {case:?}: expected {expected:?}, got {:?}",
        message(answer)
    );
}

/// One row of the create table: the request body built for a name, and the sentence the
/// create is refused with — or `None` where it must be served.
struct CreateCase {
    /// What makes this row its own case.
    case: &'static str,
    /// The members the create carries beside `FunctionName` and `Role`.
    members: Value,
    /// The whole refusal sentence, or `None` for a request that must be served.
    refusal: Option<String>,
}

/// The create table: one row per parameter/package-type mismatch, plus the rows that must
/// be served.
fn create_cases() -> Vec<CreateCase> {
    let zip = zip_file(&package_bytes(48));
    vec![
        CreateCase {
            case: "Handler on an Image package",
            members: json!({
                "PackageType": "Image",
                "Handler": "index.handler",
                "Code": { "ImageUri": IMAGE_URI },
            }),
            refusal: Some(unsupported_for_image("Handler")),
        },
        CreateCase {
            case: "Runtime on an Image package",
            members: json!({
                "PackageType": "Image",
                "Runtime": "python3.12",
                "Code": { "ImageUri": IMAGE_URI },
            }),
            refusal: Some(unsupported_for_image("Runtime")),
        },
        CreateCase {
            case: "both Runtime and Handler on an Image package, refused on the first",
            members: json!({
                "PackageType": "Image",
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Code": { "ImageUri": IMAGE_URI },
            }),
            refusal: Some(unsupported_for_image("Runtime")),
        },
        CreateCase {
            case: "ImageConfig on a Zip package",
            members: json!({
                "PackageType": "Zip",
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "ImageConfig": image_config(),
                "Code": { "ZipFile": zip.clone() },
            }),
            refusal: Some(unsupported_for_zip("ImageConfig")),
        },
        CreateCase {
            case: "ImageConfig with no PackageType at all, which defaults to Zip",
            members: json!({
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "ImageConfig": image_config(),
                "Code": { "ZipFile": zip.clone() },
            }),
            refusal: Some(unsupported_for_zip("ImageConfig")),
        },
        CreateCase {
            case: "an Image package carrying neither Runtime nor Handler",
            members: json!({
                "PackageType": "Image",
                "Code": { "ImageUri": IMAGE_URI },
            }),
            refusal: None,
        },
        CreateCase {
            case: "ImageConfig on the Image package it belongs to",
            members: json!({
                "PackageType": "Image",
                "ImageConfig": image_config(),
                "Code": { "ImageUri": IMAGE_URI },
            }),
            refusal: None,
        },
        CreateCase {
            case: "Runtime and Handler on the Zip package they belong to",
            members: json!({
                "PackageType": "Zip",
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Code": { "ZipFile": zip },
            }),
            refusal: None,
        },
    ]
}

/// The rule on `CreateFunction`: a parameter the package type does not admit is refused
/// with the family's sentence, and no function is created.
#[tokio::test]
async fn a_create_refuses_a_parameter_its_package_type_does_not_admit() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    seed_image(&engine, IMAGE_URI).await;

    for (index, row) in create_cases().into_iter().enumerate() {
        let name = format!("created-{index}");
        let mut request = json!({ "FunctionName": name, "Role": ROLE });
        let members = row
            .members
            .as_object()
            .expect("every row's members are a JSON object")
            .clone();
        for (member, value) in members {
            request[member] = value;
        }
        let answer = create_function(&router, &request).await;
        match row.refusal.as_deref() {
            None => {
                assert_eq!(
                    answer.status, 201,
                    "case {:?}: expected 201 from CreateFunction, got {} {}",
                    row.case,
                    answer.status,
                    String::from_utf8_lossy(&answer.body)
                );
                make_activation_due(&engine);
                let read = get_configuration(&router, &name).await;
                assert_eq!(
                    read.status, 200,
                    "case {:?}: expected the created function to be readable, got {} {}",
                    row.case,
                    read.status,
                    String::from_utf8_lossy(&read.body)
                );
            }
            Some(expected) => {
                assert_refused(row.case, &answer, expected);
                // Nothing was written: the read of the function it would have been is the
                // modelled 404.
                let read = get_configuration(&router, &name).await;
                assert_eq!(
                    read.status, 404,
                    "case {:?}: expected the refused function to be absent, got {} {}",
                    row.case,
                    read.status,
                    String::from_utf8_lossy(&read.body)
                );
            }
        }
    }
}

/// One row of the update table: which function is patched, with what, and the sentence the
/// update is refused with — or `None` where it must be served.
struct UpdateCase {
    /// What makes this row its own case.
    case: &'static str,
    /// The function the patch addresses: the `Image`-package one or the `Zip`-package one.
    function: &'static str,
    /// The `UpdateFunctionConfiguration` request body.
    patch: Value,
    /// The whole refusal sentence, or `None` for a patch that must be served.
    refusal: Option<String>,
}

/// The name of the `Image`-package function the update table patches.
const IMAGED: &str = "imaged";

/// The name of the `Zip`-package function the update table patches.
const ZIPPED: &str = "zipped";

/// The update table: the same mismatches a configuration change can make, in both
/// directions, plus the patches that must be served.
fn update_cases() -> Vec<UpdateCase> {
    vec![
        UpdateCase {
            case: "Handler on an Image function",
            function: IMAGED,
            patch: json!({ "Handler": "index.handler" }),
            refusal: Some(unsupported_for_image("Handler")),
        },
        UpdateCase {
            case: "Runtime on an Image function",
            function: IMAGED,
            patch: json!({ "Runtime": "python3.12" }),
            refusal: Some(unsupported_for_image("Runtime")),
        },
        UpdateCase {
            case: "Handler and Runtime at once on an Image function",
            function: IMAGED,
            patch: json!({ "Handler": "index.handler", "Runtime": "python3.12" }),
            refusal: Some(unsupported_for_image("Runtime")),
        },
        UpdateCase {
            case: "Handler beside a setting the function does admit",
            function: IMAGED,
            patch: json!({ "Handler": "index.handler", "Timeout": 11 }),
            refusal: Some(unsupported_for_image("Handler")),
        },
        UpdateCase {
            case: "ImageConfig on a Zip function",
            function: ZIPPED,
            patch: json!({ "ImageConfig": image_config() }),
            refusal: Some(unsupported_for_zip("ImageConfig")),
        },
        UpdateCase {
            case: "ImageConfig beside a setting the function does admit",
            function: ZIPPED,
            patch: json!({ "ImageConfig": image_config(), "MemorySize": 256 }),
            refusal: Some(unsupported_for_zip("ImageConfig")),
        },
        UpdateCase {
            case: "Handler on the Zip function it belongs to",
            function: ZIPPED,
            patch: json!({ "Handler": "other.handler" }),
            refusal: None,
        },
        UpdateCase {
            case: "ImageConfig on the Image function it belongs to",
            function: IMAGED,
            patch: json!({ "ImageConfig": image_config() }),
            refusal: None,
        },
    ]
}

/// The same rule on `UpdateFunctionConfiguration`: the package type is the function's
/// stored one, so a patch naming a parameter it does not admit is refused with the same
/// sentence, and the stored configuration is left as it was.
#[tokio::test]
async fn an_update_refuses_a_parameter_the_stored_package_type_does_not_admit() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    seed_image(&engine, IMAGE_URI).await;

    let imaged = create_function(
        &router,
        &json!({
            "FunctionName": IMAGED,
            "Role": ROLE,
            "PackageType": "Image",
            "Code": { "ImageUri": IMAGE_URI },
        }),
    )
    .await;
    assert_eq!(
        imaged.status, 201,
        "expected 201 from CreateFunction, got {} {}",
        imaged.status,
        String::from_utf8_lossy(&imaged.body)
    );
    let zipped = create_function(
        &router,
        &json!({
            "FunctionName": ZIPPED,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Code": { "ZipFile": zip_file(&package_bytes(48)) },
        }),
    )
    .await;
    assert_eq!(
        zipped.status, 201,
        "expected 201 from CreateFunction, got {} {}",
        zipped.status,
        String::from_utf8_lossy(&zipped.body)
    );
    // A function still being created cannot be modified at all, so the window the clock
    // owes is closed before any patch is sent.
    make_activation_due(&engine);

    for row in update_cases() {
        // Each row starts from a settled function, so a refusal is this row's and not an
        // update still in flight from the row before it.
        make_update_due(&engine);
        let before = get_configuration(&router, row.function).await;
        assert_eq!(
            before.status, 200,
            "case {:?}: expected the function to be readable first, got {} {}",
            row.case,
            before.status,
            String::from_utf8_lossy(&before.body)
        );
        let answer = update_configuration(&router, row.function, &row.patch).await;
        match row.refusal.as_deref() {
            None => assert_eq!(
                answer.status, 200,
                "case {:?}: expected 200 from UpdateFunctionConfiguration, got {} {}",
                row.case,
                answer.status,
                String::from_utf8_lossy(&answer.body)
            ),
            Some(expected) => {
                assert_refused(row.case, &answer, expected);
                // Nothing was applied: every member the function reported before the
                // refusal is still what it reports.
                let after = get_configuration(&router, row.function).await;
                assert_eq!(
                    after.json(),
                    before.json(),
                    "case {:?}: expected the refused update to change nothing, got {}",
                    row.case,
                    String::from_utf8_lossy(&after.body)
                );
            }
        }
    }
}
