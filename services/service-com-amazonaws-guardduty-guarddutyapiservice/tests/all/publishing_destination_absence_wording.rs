// SPDX-License-Identifier: CC0-1.0

//! How the publishing-destination family words "that `DestinationId` does not
//! resolve", end to end over the wire.
//!
//! Every request goes through [`smithy_server_runtime::MultiServiceRouter::dispatch`]
//! (the harness lives in the `detector_lifecycle` suite), so routing, restJson1
//! (de)serialization, modelled-constraint validation and the handlers are exercised
//! together.
//!
//! The rule under test: the wording of an unresolvable `DestinationId` is recorded
//! PER OPERATION, so the properties UPDATE answers that no publishing destination
//! exists for the `publishingDestinationId` it was handed while the read and the
//! delete answer the service's generic invalid-input-parameters sentence — the status
//! (400) and the error class/shape being identical for all three, the message is the
//! only discriminator. Each operation's sentence is wholly FIXED: it quotes neither
//! the detector id, nor the destination id, nor anything else the request carried.
//!
//! The rows span what the rule generalises over: the FORM of the id the call cannot
//! resolve (one a delete removed, one that was never minted, one that could never
//! have been minted, a live id the caller changed the case of, one another detector
//! holds, one another Region holds), the OPERATION addressing it, and — for the
//! update — the request BODY, which the sentence must not depend on either.

use serde_json::{json, Value};

use crate::detector_lifecycle::{
    call_in, enabled_detector, error_code, error_shape, new_router, Router,
};

// The two fixed sentences under test, written out here rather than read from the
// service, so that this suite pins the WORDS and not merely the service's agreement
// with itself.

/// What the publishing-destination READ and DELETE answer for a `DestinationId` they
/// cannot resolve: the service's generic unusable-input-parameter sentence.
const INVALID_INPUT_PARAMETERS: &str =
    "The request is rejected because the one or more input parameters have invalid values.";

/// What the properties UPDATE answers for the same id: that no publishing destination
/// exists for the `publishingDestinationId` it was handed.
const NO_PUBLISHING_DESTINATION: &str =
    "The request failed because no publishingDestination exists with the provided publishingDestinationId.";

/// The member name that is part of the update's fixed text — the member AWS names
/// there, not a value from the request.
const NAMED_MEMBER: &str = "publishingDestinationId";

/// The error class every GuardDuty client rejection carries in the body's `__type`
/// discriminator (see `client_rejections.rs`).
const CLASS: &str = "InvalidInputException";

/// The modelled error SHAPE that class is enveloped in — one of the only two errors
/// `UpdatePublishingDestination`, `DescribePublishingDestination` and
/// `DeletePublishingDestination` declare
/// (`smithy_query(operation="UpdatePublishingDestination")`).
const ENVELOPE: &str = "BadRequestException";

/// The Region the detector these rows address is created under.
const HOME_REGION: &str = "us-east-1";

/// Another Region, for the row whose destination is held by a detector only that
/// Region's endpoint reaches.
const AWAY_REGION: &str = "eu-west-1";

/// The only member of the `DestinationType` enum
/// (`smithy_query(shape="DestinationType")`).
const S3: &str = "S3";

/// A well-formed `DestinationId` (32 lowercase hex characters, the form every id this
/// service mints has) that no create ever minted.
const NEVER_MINTED_ID: &str = "0123456789abcdef0123456789abcdef";

/// A value no create could ever have minted a `DestinationId` as.
const MALFORMED_ID: &str = "not-a-destination-id";

/// The KMS key ARN the destinations here are created with ("The ARN of the KMS key to
/// use for encryption" — `smithy_query(shape="DestinationProperties")`).
const KEY_ARN: &str =
    "arn:aws:kms:us-east-1:111122223333:key/1234abcd-12ab-34cd-56ef-1234567890ab";

/// The destination ARN the destinations here are created with.
const BUCKET_ARN: &str = "arn:aws:s3:::guardduty-findings-wording";

fn destination_path(detector_id: &str, destination_id: &str) -> String {
    format!("/detector/{detector_id}/publishingDestination/{destination_id}")
}

/// Create a destination under `detector_id` through `region`'s endpoint and return
/// its id.
async fn create_destination(router: &Router, region: &str, detector_id: &str) -> String {
    let (status, body) = call_in(
        router,
        region,
        "POST",
        &format!("/detector/{detector_id}/publishingDestination"),
        Some(json!({
            "destinationType": S3,
            "destinationProperties": {"destinationArn": BUCKET_ARN, "kmsKeyArn": KEY_ARN},
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "create-publishing-destination in {region}: expected 200, got {status} {body:?}"
    );
    body["destinationId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("create-publishing-destination in {region}: expected a destinationId, got {body:?}")
        })
        .to_string()
}

/// Create an enabled detector through `region`'s endpoint and return its id.
async fn detector_in(router: &Router, region: &str) -> String {
    let (status, body) = call_in(router, region, "POST", "/detector", Some(json!({"enable": true}))).await;
    assert_eq!(
        status, 200,
        "create-detector in {region}: expected 200, got {status} {body:?}"
    );
    body["detectorId"]
        .as_str()
        .unwrap_or_else(|| panic!("create-detector in {region}: expected a detectorId, got {body:?}"))
        .to_string()
}

/// Assert the shared half of every one of these refusals: HTTP 400, the one client
/// error class and envelope shape this family declares, and a message that quotes
/// nothing of the request back.
fn assert_classified(case: &str, status: u16, response: &Value, forbidden: &[&str]) -> String {
    assert_eq!(
        status, 400,
        "case {case:?}: expected 400, got {status} {response:?}"
    );
    assert_eq!(
        error_code(response),
        CLASS,
        "case {case:?}: expected the {CLASS} class, got {response:?}"
    );
    assert_eq!(
        error_shape(response),
        ENVELOPE,
        "case {case:?}: expected the {ENVELOPE} envelope shape, got {response:?}"
    );
    let message = response["message"].as_str().unwrap_or_default().to_string();
    for value in forbidden {
        assert!(
            !message.contains(value),
            "case {case:?}: the refusal must not quote {value:?} back, got {message:?}"
        );
    }
    message
}

/// The read, the delete and the properties update all refuse an id they cannot
/// resolve with the same status and class, and the update's message is the one that
/// says the publishing destination does not exist.
#[tokio::test]
async fn the_update_words_an_unresolvable_destination_id_as_an_absent_destination() {
    let router = new_router();
    let detector_id = enabled_detector(&router).await;

    // A destination a delete removed, a live one (for the case the caller changed),
    // and one only another Region's detector holds.
    let deleted_id = create_destination(&router, HOME_REGION, &detector_id).await;
    let (status, body) = call_in(
        &router,
        HOME_REGION,
        "DELETE",
        &destination_path(&detector_id, &deleted_id),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "the delete that makes the deleted-id row: expected 200, got {status} {body:?}"
    );
    let live_id = create_destination(&router, HOME_REGION, &detector_id).await;
    let away_detector = detector_in(&router, AWAY_REGION).await;
    let away_id = create_destination(&router, AWAY_REGION, &away_detector).await;

    // (case, the detector the request names, the id it cannot resolve)
    let rows: Vec<(&str, String, String)> = vec![
        (
            "an id a delete removed",
            detector_id.clone(),
            deleted_id.clone(),
        ),
        (
            "a well-formed id nothing minted",
            detector_id.clone(),
            NEVER_MINTED_ID.to_string(),
        ),
        (
            "an id that could never have been minted",
            detector_id.clone(),
            MALFORMED_ID.to_string(),
        ),
        (
            "a live id the caller upper-cased",
            detector_id.clone(),
            live_id.to_uppercase(),
        ),
        (
            "an id another detector, in another Region, holds",
            detector_id.clone(),
            away_id.clone(),
        ),
    ];

    for (row, detector, destination) in rows {
        let path = destination_path(&detector, &destination);
        let forbidden = [destination.as_str()];

        // The READ: the service's generic invalid-input-parameters sentence.
        let (status, response) = call_in(&router, HOME_REGION, "GET", &path, None).await;
        let read_message = assert_classified(
            &format!("{row}: a describe"),
            status,
            &response,
            &forbidden,
        );

        // The UPDATE: the absent-publishing-destination sentence instead.
        let (status, response) = call_in(
            &router,
            HOME_REGION,
            "POST",
            &path,
            Some(json!({"destinationProperties": {"destinationArn": BUCKET_ARN}})),
        )
        .await;
        let update_message = assert_classified(
            &format!("{row}: an update"),
            status,
            &response,
            &forbidden,
        );

        // The DELETE: worded like the read.
        let (status, response) = call_in(&router, HOME_REGION, "DELETE", &path, None).await;
        let delete_message = assert_classified(
            &format!("{row}: a delete"),
            status,
            &response,
            &forbidden,
        );

        assert_eq!(
            update_message, NO_PUBLISHING_DESTINATION,
            "case {row:?}: expected the update to answer \
             {NO_PUBLISHING_DESTINATION:?}, got {update_message:?}"
        );
        assert_eq!(
            read_message, INVALID_INPUT_PARAMETERS,
            "case {row:?}: expected the describe to answer \
             {INVALID_INPUT_PARAMETERS:?}, got {read_message:?}"
        );
        assert_eq!(
            delete_message, INVALID_INPUT_PARAMETERS,
            "case {row:?}: expected the delete to answer \
             {INVALID_INPUT_PARAMETERS:?}, got {delete_message:?}"
        );
        // The message is the ONLY discriminator the family gives a client that has to
        // tell "the destination is gone" from "your request was malformed".
        assert_ne!(
            update_message, read_message,
            "case {row:?}: the update must not answer the read's sentence, got {update_message:?}"
        );
        // The member name AWS states is part of that fixed text.
        assert!(
            update_message.contains(NAMED_MEMBER),
            "case {row:?}: expected the update's refusal to name the {NAMED_MEMBER} \
             member, got {update_message:?}"
        );
    }

    // None of those refusals wrote anything: the live destination still reports what
    // its create stored.
    let (status, read) = call_in(
        &router,
        HOME_REGION,
        "GET",
        &destination_path(&detector_id, &live_id),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the live destination after the refusals\": expected 200, got {status} {read:?}"
    );
    assert_eq!(
        read["destinationProperties"]["destinationArn"].as_str(),
        Some(BUCKET_ARN),
        "case \"the live destination after the refusals\": expected the stored destinationArn, \
         got {read:?}"
    );
}

/// The update's absent-destination sentence is wholly FIXED: two calls that differ in
/// the detector, the destination id and the body they carry are refused with
/// byte-identical text.
#[tokio::test]
async fn the_updates_absent_destination_sentence_interpolates_nothing() {
    let router = new_router();
    let first = enabled_detector(&router).await;
    let second = detector_in(&router, AWAY_REGION).await;

    // (case, the Region the call is signed for, the detector, the id, the body)
    let rows: Vec<(&str, &str, String, String, Option<Value>)> = vec![
        (
            "both properties, a well-formed id nothing minted",
            HOME_REGION,
            first.clone(),
            NEVER_MINTED_ID.to_string(),
            Some(json!({
                "destinationProperties": {"destinationArn": BUCKET_ARN, "kmsKeyArn": KEY_ARN},
            })),
        ),
        (
            "an empty destinationProperties object",
            HOME_REGION,
            first.clone(),
            MALFORMED_ID.to_string(),
            Some(json!({"destinationProperties": {}})),
        ),
        (
            "no destinationProperties at all",
            HOME_REGION,
            first.clone(),
            NEVER_MINTED_ID.to_uppercase(),
            Some(json!({})),
        ),
        (
            "another detector, through its own Region's endpoint",
            AWAY_REGION,
            second.clone(),
            NEVER_MINTED_ID.to_string(),
            Some(json!({"destinationProperties": {"expectedBucketOwner": "111122223333"}})),
        ),
    ];

    for (row, region, detector, destination, body) in rows {
        let (status, response) = call_in(
            &router,
            region,
            "POST",
            &destination_path(&detector, &destination),
            body,
        )
        .await;
        let message = assert_classified(row, status, &response, &[detector.as_str(), destination.as_str()]);
        assert_eq!(
            message, NO_PUBLISHING_DESTINATION,
            "case {row:?}: expected {NO_PUBLISHING_DESTINATION:?}, got {message:?}"
        );
    }
}

/// The new refusal is reached only when the destination really does not resolve: an
/// update of a destination the detector holds still succeeds and is still readable
/// back through the describe.
#[tokio::test]
async fn an_update_of_a_destination_the_detector_holds_is_unaffected() {
    let router = new_router();
    let detector_id = enabled_detector(&router).await;
    let destination_id = create_destination(&router, HOME_REGION, &detector_id).await;

    let rotated = "arn:aws:kms:us-east-1:111122223333:alias/guardduty-exports";
    let (status, response) = call_in(
        &router,
        HOME_REGION,
        "POST",
        &destination_path(&detector_id, &destination_id),
        Some(json!({"destinationProperties": {"kmsKeyArn": rotated}})),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"an update of a held destination\": expected 200, got {status} {response:?}"
    );

    let (status, read) = call_in(
        &router,
        HOME_REGION,
        "GET",
        &destination_path(&detector_id, &destination_id),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the read back\": expected 200, got {status} {read:?}"
    );
    assert_eq!(
        read["destinationProperties"]["kmsKeyArn"].as_str(),
        Some(rotated),
        "case \"the read back\": expected the rotated kmsKeyArn, got {read:?}"
    );
}
