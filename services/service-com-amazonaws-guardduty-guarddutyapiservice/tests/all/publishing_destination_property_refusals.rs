// SPDX-License-Identifier: CC0-1.0

//! What a publishing destination's PROPERTIES are refused for, and in which WORDS.
//!
//! Every request goes over the wire through
//! [`smithy_server_runtime::MultiServiceRouter::dispatch`] (the harness lives in the
//! `detector_lifecycle` suite), so routing, restJson1 (de)serialization,
//! modelled-constraint validation and the handlers are exercised together.
//!
//! The rule under test, asserted over a TABLE of inputs rather than over one recorded
//! call: a `kmsKeyArn` the service cannot encrypt published findings with — one the
//! request left out, an empty one, and every form that is not a KMS key ARN — is
//! refused with HTTP 400 under the `InvalidInputException` class, in AWS's ONE fixed
//! sentence saying the VALUE specified for that parameter is not valid. It never says
//! the parameter is missing: a caller told to supply a key it did supply looks in the
//! wrong place, so absent and unusable are worded alike. The `destinationArn` keeps
//! its own refusals, which name that member instead, so the two members cannot drift
//! into one message.
//!
//! The rows differ along what the rule generalises over: whether the member is
//! PRESENT at all, and the FORM of the value when it is (another service's ARN, an
//! ARN with no resource, a bare key id, a key alias written without the ARN, text
//! that is no ARN). The same table is driven through the CREATE, which cannot do
//! without the member, and through the UPDATE, which carries only what it changes —
//! one rule, two operations.
//!
//! A usable pair is the control: it is accepted, and `DescribePublishingDestination`
//! reports the properties back, so every refusal above is a judgement about the value
//! and not a create that never worked.

use serde_json::{json, Value};
use service_com_amazonaws_guardduty_guarddutyapiservice::state;

use crate::detector_lifecycle::{
    call, enabled_detector, error_code, error_shape, new_router, Router,
};

/// The error CLASS a client rejection carries in the body's `__type` and `type`.
const CLASS: &str = "InvalidInputException";

/// The modelled error SHAPE restJson1 resolves it to (the `x-amzn-errortype` header).
const ENVELOPE: &str = "BadRequestException";

/// The only member of the `DestinationType` enum
/// (`smithy_query(shape="DestinationType")`).
const S3: &str = "S3";

/// AWS's sentence for a `kmsKeyArn` it cannot use, written out here rather than read
/// from the service so this suite pins the WORDS and not merely the service's
/// agreement with itself.
const KEY_NOT_VALID: &str =
    "The request failed because the value specified for the kmsKeyArn parameter is not valid.";

/// The claim the sentence above must never make: the caller supplied the member in
/// most of the rows below, and is told nothing is missing in the ones where it did
/// not either.
const MISSING_CLAIM: &str = "is required";

/// A usable destination ARN — an S3 bucket ARN, the form "the ARN of the resource to
/// publish to" carries (`smithy_query(shape="DestinationProperties")`).
const BUCKET_ARN: &str = "arn:aws:s3:::guardduty-findings-probe";

/// A usable KMS key ARN, the form `DestinationProperties.KmsKeyArn` carries ("The ARN
/// of the KMS key to use for encryption", same shape).
const KEY_ARN: &str = "arn:aws:kms:us-east-1:111122223333:key/1234abcd-12ab-34cd-56ef-1234567890ab";

async fn create_destination(router: &Router, detector_id: &str, properties: Value) -> (u16, Value) {
    call(
        router,
        "POST",
        &format!("/detector/{detector_id}/publishingDestination"),
        Some(json!({"destinationType": S3, "destinationProperties": properties})),
    )
    .await
}

async fn update_destination(
    router: &Router,
    detector_id: &str,
    destination_id: &str,
    properties: Value,
) -> (u16, Value) {
    call(
        router,
        "POST",
        &format!("/detector/{detector_id}/publishingDestination/{destination_id}"),
        Some(json!({"destinationProperties": properties})),
    )
    .await
}

async fn describe_destination(
    router: &Router,
    detector_id: &str,
    destination_id: &str,
) -> (u16, Value) {
    call(
        router,
        "GET",
        &format!("/detector/{detector_id}/publishingDestination/{destination_id}"),
        None,
    )
    .await
}

/// Assert one refusal: the status, the class, the envelope shape, the sentence word
/// for word, that it makes no missing-member claim, and that it quotes nothing of the
/// request back.
fn assert_key_refusal(case: &str, status: u16, body: &Value, sent: Option<&str>) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} {body:?}"
    );
    assert_eq!(
        error_code(body),
        CLASS,
        "case {case:?}: expected the {CLASS} class, got {body:?}"
    );
    assert_eq!(
        error_shape(body),
        ENVELOPE,
        "case {case:?}: expected the {ENVELOPE} envelope shape, got {body:?}"
    );
    let message = body["message"].as_str().unwrap_or_default();
    assert_eq!(
        message, KEY_NOT_VALID,
        "case {case:?}: expected message {KEY_NOT_VALID:?}, got {message:?}"
    );
    assert!(
        !message.contains(MISSING_CLAIM),
        "case {case:?}: a supplied value must not be reported as missing, got {message:?}"
    );
    if let Some(sent) = sent.filter(|sent| !sent.is_empty()) {
        assert!(
            !message.contains(sent),
            "case {case:?}: the refusal must not quote {sent:?} back, got {message:?}"
        );
    }
}

/// Every `kmsKeyArn` the service cannot use, with why. `None` is the member left out
/// altogether.
fn unusable_keys() -> Vec<(&'static str, Option<&'static str>)> {
    vec![
        ("the member left out altogether", None),
        ("an empty value", Some("")),
        (
            "an ARN of another service",
            Some("arn:aws:s3:::guardduty-findings-probe"),
        ),
        (
            "an ARN of another service that names a key-shaped resource",
            Some("arn:aws:secretsmanager:us-east-1:111122223333:key/1234abcd"),
        ),
        ("a KMS ARN with no resource at all", Some("arn:aws:kms:::")),
        (
            "a bare key id, with no ARN around it",
            Some("1234abcd-12ab-34cd-56ef-1234567890ab"),
        ),
        (
            "an alias written without the ARN",
            Some("alias/guardduty-findings"),
        ),
        ("text that is no ARN at all", Some("my-encryption-key")),
        (
            "an ARN whose first field is not `arn`",
            Some("aws:kms:us-east-1:111122223333:key/1234abcd"),
        ),
        (
            "an ARN missing its account and resource fields",
            Some("arn:aws:kms:us-east-1"),
        ),
    ]
}

#[tokio::test]
async fn a_create_reports_an_unusable_kms_key_as_a_value_that_is_not_valid() {
    let router = new_router();
    let detector_id = enabled_detector(&router).await;

    for (why, key) in unusable_keys() {
        let mut properties = json!({"destinationArn": BUCKET_ARN});
        if let Some(key) = key {
            properties["kmsKeyArn"] = json!(key);
        }
        let (status, refused) = create_destination(&router, &detector_id, properties).await;
        assert_key_refusal(&format!("create: {why}"), status, &refused, key);
    }

    // The control: the same create with a usable key is accepted and reads back, so
    // each refusal above is about the VALUE and not about the create path.
    let (status, created) = create_destination(
        &router,
        &detector_id,
        json!({"destinationArn": BUCKET_ARN, "kmsKeyArn": KEY_ARN}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"create: a usable pair\": expected 200, got {status} {created:?}"
    );
    let destination_id = created["destinationId"]
        .as_str()
        .unwrap_or_else(|| panic!("case \"create: a usable pair\": expected an id, got {created:?}"))
        .to_string();
    let (status, described) = describe_destination(&router, &detector_id, &destination_id).await;
    assert_eq!(
        status, 200,
        "case \"create: a usable pair\": expected the describe to answer 200, got {status} {described:?}"
    );
    assert_eq!(
        described["destinationProperties"]["kmsKeyArn"],
        json!(KEY_ARN),
        "case \"create: a usable pair\": expected the stored key back, got {described:?}"
    );
    assert_eq!(
        described["destinationProperties"]["destinationArn"],
        json!(BUCKET_ARN),
        "case \"create: a usable pair\": expected the stored destination back, got {described:?}"
    );
}

#[tokio::test]
async fn an_update_reports_an_unusable_kms_key_the_same_way_and_stores_nothing() {
    let router = new_router();
    let detector_id = enabled_detector(&router).await;
    let (status, created) = create_destination(
        &router,
        &detector_id,
        json!({"destinationArn": BUCKET_ARN, "kmsKeyArn": KEY_ARN}),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected the create to answer 200, got {status} {created:?}"
    );
    let destination_id = created["destinationId"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: expected a destinationId, got {created:?}"))
        .to_string();

    for (why, key) in unusable_keys() {
        // An update carries only what it changes, so the member left out is a
        // request that asks for no key change at all: that one is ACCEPTED and
        // leaves the stored key where it was.
        let Some(key) = key else {
            let (status, answered) = update_destination(
                &router,
                &detector_id,
                &destination_id,
                json!({"destinationArn": BUCKET_ARN}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case \"update: {why}\": an update that names no key changes none, got {status} {answered:?}"
            );
            continue;
        };
        let (status, refused) = update_destination(
            &router,
            &detector_id,
            &destination_id,
            json!({"kmsKeyArn": key}),
        )
        .await;
        assert_key_refusal(&format!("update: {why}"), status, &refused, Some(key));

        // A refused update stored nothing: the destination still reports the key the
        // create accepted.
        let (status, described) = describe_destination(&router, &detector_id, &destination_id).await;
        assert_eq!(
            status, 200,
            "case \"update: {why}\": expected the describe to answer 200, got {status} {described:?}"
        );
        assert_eq!(
            described["destinationProperties"]["kmsKeyArn"],
            json!(KEY_ARN),
            "case \"update: {why}\": a refused update must leave the stored key alone, got {described:?}"
        );
    }

    // The same update with a usable key IS written, so the refusals above are about
    // the value rather than about the update path.
    let other_key = "arn:aws:kms:us-east-1:111122223333:key/abcd1234-ab12-cd34-ef56-0987654321ab";
    let (status, answered) = update_destination(
        &router,
        &detector_id,
        &destination_id,
        json!({"kmsKeyArn": other_key}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"update: another usable key\": expected 200, got {status} {answered:?}"
    );
    let (status, described) = describe_destination(&router, &detector_id, &destination_id).await;
    assert_eq!(
        status, 200,
        "case \"update: another usable key\": expected the describe to answer 200, got {status} {described:?}"
    );
    assert_eq!(
        described["destinationProperties"]["kmsKeyArn"],
        json!(other_key),
        "case \"update: another usable key\": expected the updated key back, got {described:?}"
    );

    // The sentence this suite pins is the one the service itself holds, so a change
    // to either is a change to both.
    assert_eq!(
        state::KMS_KEY_NOT_VALID_MESSAGE, KEY_NOT_VALID,
        "the service's unusable-key sentence must be the one asserted here"
    );
}
