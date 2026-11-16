// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the FORM every failure of this service names its error shape in,
//! and for the refusal the version read path answers for an own-account identifier that
//! resolves to no image — driven over the same router the emulator boots.
//!
//! RULE: a `restJson1` client resolves a failure's error shape from the `x-amzn-errortype`
//! response header, and what this service's wire carries there is the QUALIFIED form — the
//! bare shape name, a `:`, and the locator of the namespace that declares the shape under the
//! service stack's locator root — for EVERY failure it answers, whichever operation answered
//! and whichever layer rendered it; the body keeps naming the bare shape in `__type`, and a
//! success names no shape at all.
//!
//! RULE: a `MicrovmImageIdentifier` is classified BEFORE anything is refused — AWS's own
//! image namespace is 403 `AccessDeniedException`, a value that is no identifier of this API
//! is 400 `ValidationException`, and an identifier of the caller's OWN namespace that resolves
//! to no image is the modelled 404 `ResourceNotFoundException` naming the whole address the
//! request carried.
//!
//! Each test states the rule and exercises it over a SET of inputs; the values any one row
//! happens to carry are examples, not the specification. The wire harness (`emulator`,
//! `call_for_error_type`, `image_body`, …) is the one `microvm_lifecycle` owns.

use serde_json::{json, Value};
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::{
    image_access_denied_message, image_not_found_message, image_version_not_found_message,
    invalid_arn_format_message, managed_base_image_arn, version_address_image_not_found_message,
    FIRST_IMAGE_VERSION, MANAGED_BASE_IMAGE_IDS,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::microvm_lifecycle::{
    as_uri_value, call_for_error_type, emulator, expected_image_arn, image_body, poll_until,
    ACCOUNT, REGION,
};

/// The locator this service's wire qualifies every error shape with: the separator, the
/// service stack's locator root, and the namespace that declares this service's shapes
/// (`crate::service::error_type_locator::SHAPE_NAMESPACE`, which is the service stack's own
/// namespace and not the Smithy model's `com.amazonaws.lambdamicrovms`). Written out here so
/// the expectation is the recorded wire value rather than whatever the crate computes.
const LOCATOR: &str = ":http://internal.amazon.com/coral/com.amazonaws.lambda.microvms/";

/// One row of the table below: one request this service refuses, and the answer the rule
/// fixes for it.
struct Row {
    /// What makes this row its own case, named in every failure.
    case: String,
    method: &'static str,
    uri: String,
    body: Option<Value>,
    /// The modelled status, the bare error shape, and the message where the rule fixes one.
    status: u16,
    shape: &'static str,
    message: Option<String>,
}

/// One request, with the `x-amzn-errortype` header of the answer beside its status and body.
async fn request(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, String, Value) {
    call_for_error_type(router, REGION, method, uri, body).await
}

/// `GET /2025-09-09/microvm-images/{imageIdentifier}/versions/{imageVersion}`.
async fn get_version(
    router: &MultiServiceRouter,
    identifier: &str,
    image_version: &str,
) -> (u16, String, Value) {
    let uri = format!(
        "/2025-09-09/microvm-images/{}/versions/{}",
        as_uri_value(identifier),
        as_uri_value(image_version)
    );
    request(router, "GET", &uri, None).await
}

/// Create an image and wait for the CONDITION its asynchronous build owes — its first
/// version readable — rather than for a span of time; its name is returned.
async fn built_image(router: &MultiServiceRouter, name: &str) -> String {
    let (status, _error_type, body) = request(
        router,
        "POST",
        "/2025-09-09/microvm-images",
        Some(image_body(name, json!({}))),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {name:?}: create expected 201, got {status} {body}"
    );
    poll_until(name, "the first version is readable", || async {
        let (status, _error_type, _body) = get_version(router, name, FIRST_IMAGE_VERSION).await;
        status == 200
    })
    .await;
    name.to_string()
}

/// Assert one refused answer: its status, the bare shape its body names, the QUALIFIED shape
/// its header names, and — where the rule fixes it — its message.
fn assert_refusal(
    case: &str,
    (status, error_type, body): &(u16, String, Value),
    expected_status: u16,
    shape: &str,
    message: Option<&str>,
) {
    assert_eq!(
        *status, expected_status,
        "case {case:?}: expected HTTP {expected_status}, got {status} {body}"
    );
    assert_eq!(
        body["__type"].as_str(),
        Some(shape),
        "case {case:?}: expected the body to name {shape:?}, got {body}"
    );
    let expected = format!("{shape}{LOCATOR}");
    assert_eq!(
        error_type, &expected,
        "case {case:?}: expected x-amzn-errortype {expected:?}, got {error_type:?}"
    );
    if let Some(message) = message {
        assert_eq!(
            body["message"].as_str(),
            Some(message),
            "case {case:?}: expected the message {message:?}, got {:?}",
            body["message"]
        );
    }
}

/// RULE: an `imageIdentifier` of the CALLER'S OWN image namespace that resolves to no image
/// is the modelled 404 `ResourceNotFoundException`, whose message names the whole address the
/// version read was given — the identifier label and the version label the request carried,
/// each echoed verbatim — and whose shape is named in the qualified form.
#[tokio::test]
async fn an_absent_own_account_image_version_is_not_found() {
    let router = emulator();

    // An image this account DOES hold, so the rows below are refused for the identifier they
    // name rather than for an empty store.
    let held = built_image(&router, "held-image").await;

    // Rows differ in WHY the identifier resolves to no image of this region's store: a name
    // nothing was created under, the held name with a suffix, a name held in ANOTHER region
    // (another endpoint's record, an absence here), and a name spelled like an id rather than
    // a word.
    let identifiers = [
        (
            "this account's namespace, a name nothing was created under",
            expected_image_arn("never-created"),
        ),
        (
            "this account's namespace, the held name with a suffix",
            format!("{}-missing", expected_image_arn(&held)),
        ),
        (
            "this account's namespace in another region, a name it holds",
            format!("arn:aws:lambda:eu-central-1:{ACCOUNT}:microvm-image:{held}"),
        ),
        (
            "this account's namespace, a name that is an id",
            expected_image_arn("img-0a1b2c3d4e5f"),
        ),
    ];

    // `imageVersion` is `@length(1..2048)` with pattern `^[^\s]+$`
    // (`smithy_query(query="operation", operation="GetMicrovmImageVersion")`), so each label
    // below is one the boundary admits and the lookup must answer for. The rows differ in
    // form: the initial generation, a later one, a bare major, and a label that is no version
    // of this service at all.
    let versions = [FIRST_IMAGE_VERSION, "2.0", "1", "latest"];

    for (case, identifier) in &identifiers {
        for image_version in versions {
            let row = format!("{case} ({identifier}) version {image_version}");
            let answered = get_version(&router, identifier, image_version).await;
            assert_refusal(
                &row,
                &answered,
                404,
                "ResourceNotFoundException",
                Some(&version_address_image_not_found_message(
                    identifier,
                    image_version,
                )),
            );
        }
    }

    // The controls that must NOT move: an AWS-OWNED managed identifier is still the 403 of a
    // namespace no account's policy can name, and this account's own held version is still
    // served — so the 404s above are the classification of the identifier and not a dead path.
    for image_id in MANAGED_BASE_IMAGE_IDS {
        let identifier = managed_base_image_arn(REGION, image_id);
        let row = format!("an AWS-owned identifier {identifier}");
        let answered = get_version(&router, &identifier, FIRST_IMAGE_VERSION).await;
        assert_refusal(
            &row,
            &answered,
            403,
            "AccessDeniedException",
            Some(&image_access_denied_message(
                "GetMicrovmImageVersion",
                &identifier,
            )),
        );
    }

    let (status, error_type, served) = get_version(&router, &held, FIRST_IMAGE_VERSION).await;
    assert_eq!(
        status, 200,
        "this account's own version is served, got {status} {served}"
    );
    assert_eq!(
        error_type, "",
        "a success names no error shape, got {error_type:?}"
    );
    assert_eq!(
        served["imageArn"].as_str(),
        Some(expected_image_arn(&held).as_str()),
        "the read must report the stored record, got {served}"
    );
}

/// RULE: every failure of this service names its error shape in `x-amzn-errortype` in the
/// qualified form, whichever operation answered and whichever layer rendered the failure,
/// while the body keeps naming the bare shape.
#[tokio::test]
async fn every_failure_names_its_error_shape_in_the_qualified_form() {
    let router = emulator();
    let held = built_image(&router, "qualified").await;
    let absent = expected_image_arn("never-created");
    let managed = managed_base_image_arn(REGION, "al2023-1");
    let not_an_identifier = "not-an-identifier";

    // Rows differ in the error CLASS, the status, the operation that answered and the LAYER
    // that rendered the failure: a handler's modelled error, and a boundary refusal raised
    // before any handler ran. Every row names a shape THIS model declares, which is what the
    // rule is about: the locator names the namespace that declares the shape.
    let rows = vec![
        Row {
            case: "a version read on an own-account identifier that names no image".to_string(),
            method: "GET",
            uri: format!(
                "/2025-09-09/microvm-images/{}/versions/{FIRST_IMAGE_VERSION}",
                as_uri_value(&absent)
            ),
            body: None,
            status: 404,
            shape: "ResourceNotFoundException",
            message: Some(version_address_image_not_found_message(
                &absent,
                FIRST_IMAGE_VERSION,
            )),
        },
        Row {
            case: "a version the held image does not hold".to_string(),
            method: "GET",
            uri: format!(
                "/2025-09-09/microvm-images/{}/versions/9.9",
                as_uri_value(&held)
            ),
            body: None,
            status: 404,
            shape: "ResourceNotFoundException",
            message: Some(image_version_not_found_message(
                &expected_image_arn(&held),
                "9.9",
            )),
        },
        Row {
            case: "a version read on an AWS-owned identifier".to_string(),
            method: "GET",
            uri: format!(
                "/2025-09-09/microvm-images/{}/versions/{FIRST_IMAGE_VERSION}",
                as_uri_value(&managed)
            ),
            body: None,
            status: 403,
            shape: "AccessDeniedException",
            message: Some(image_access_denied_message(
                "GetMicrovmImageVersion",
                &managed,
            )),
        },
        Row {
            case: "a version read on a value that is no identifier of this API".to_string(),
            method: "GET",
            uri: format!(
                "/2025-09-09/microvm-images/{not_an_identifier}/versions/{FIRST_IMAGE_VERSION}"
            ),
            body: None,
            status: 400,
            shape: "ValidationException",
            message: Some(invalid_arn_format_message(not_an_identifier)),
        },
        Row {
            case: "an image read on an own-account identifier that names no image".to_string(),
            method: "GET",
            uri: format!("/2025-09-09/microvm-images/{}", as_uri_value(&absent)),
            body: None,
            status: 404,
            shape: "ResourceNotFoundException",
            message: Some(image_not_found_message(&absent)),
        },
        Row {
            case: "a version listing on an AWS-owned identifier".to_string(),
            method: "GET",
            uri: format!(
                "/2025-09-09/microvm-images/{}/versions",
                as_uri_value(&managed)
            ),
            body: None,
            status: 403,
            shape: "AccessDeniedException",
            message: Some(image_access_denied_message(
                "ListMicrovmImageVersions",
                &managed,
            )),
        },
        Row {
            case: "a create whose body omits every required member".to_string(),
            method: "POST",
            uri: "/2025-09-09/microvm-images".to_string(),
            body: Some(json!({})),
            status: 400,
            shape: "ValidationException",
            message: None,
        },
    ];

    for row in rows {
        let answered = request(&router, row.method, &row.uri, row.body).await;
        assert_refusal(
            &row.case,
            &answered,
            row.status,
            row.shape,
            row.message.as_deref(),
        );
    }

    // A success is answered byte for byte: it names no error shape, in either place.
    let (status, error_type, served) = request(
        &router,
        "GET",
        &format!("/2025-09-09/microvm-images/{}", as_uri_value(&held)),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "the held image is served, got {status} {served}"
    );
    assert_eq!(
        error_type, "",
        "a success names no error shape, got {error_type:?}"
    );
    assert!(
        served["__type"].is_null(),
        "a success names no error shape in its body either, got {served}"
    );
}
