// SPDX-License-Identifier: CC0-1.0

//! What the Investigation API refuses, and in which WORDS.
//!
//! Every request goes over the wire through
//! [`smithy_server_runtime::MultiServiceRouter::dispatch`] (the harness lives in the
//! `detector_lifecycle` suite), so routing, restJson1 (de)serialization,
//! modelled-constraint validation and the handlers are exercised together.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than over one
//! recorded call:
//!
//! 1. THE PREREQUISITE. The Investigation API is served only to a detector that has
//!    ENABLED the AI_ANALYST feature — "to use this operation, the AI_ANALYST feature
//!    must be enabled on your detector"
//!    (`smithy_query(query="doc",shape="CreateInvestigation")`) — and the feature is
//!    opt-in, so a detector nothing asked for it on does not have it. A create
//!    against such a detector is refused as an unacceptable INPUT: HTTP 400, the
//!    `BadRequestException` envelope, the `InvalidInputException` class in both body
//!    fields, and the service's one fixed sentence for that prerequisite — never an
//!    authorization refusal, and never a 2xx that records an investigation. Rows
//!    differ along HOW the feature came to be off or on (a bare create, an explicit
//!    status at create time, a later update in either direction), because the gate
//!    reads the STORED switch rather than the call that last touched it.
//! 2. THE UNRESOLVED DETECTOR. A `detectorId` the account and Region do not hold is
//!    the DETECTOR family's own ownership refusal and a value that could not be a
//!    `detectorId` at all is the parameter refusal — both the service's HTTP 400
//!    under the `InvalidInputException` class, on EVERY operation of this API, as on
//!    every other detector-scoped operation of this service. Rows differ along the
//!    FORM of the id (a well-formed id nothing minted, the same id upper-cased, a
//!    deleted detector's own id, and values outside the form on both sides of its
//!    length) and along the OPERATION (the create, the read and the list).
//! 3. THE ABSENT INVESTIGATION. An `investigationId` the detector does not hold is
//!    the modelled `ResourceNotFoundException` (HTTP 404) carrying the Investigation
//!    family's own short sentence, with neither the operation nor the id in it. Rows
//!    span the ids the member's `len 1..64, ^[a-fA-F0-9\-]+$` admits — both length
//!    bounds, a UUID spelling, an upper-cased id, and an id another detector minted.

use serde_json::{json, Value};

use crate::detector_lifecycle::{
    ai_analyst_features, call, create_detector, enabled_detector, error_code, error_shape,
    investigation_detector, new_router, Router, ABSENT_DETECTOR_ID,
};
use service_com_amazonaws_guardduty_guarddutyapiservice::state;

/// The modelled error SHAPE restJson1 resolves this service's 400s to (the
/// `x-amzn-errortype` header).
const ENVELOPE: &str = "BadRequestException";

/// The error CLASS a 400 carries in the body's `__type` and `type`.
const INVALID_INPUT: &str = "InvalidInputException";

/// The modelled error `GetInvestigation` answers an absent investigation with
/// (`smithy_query(operation="GetInvestigation")`: `ResourceNotFoundException`,
/// `httpError` 404).
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The sentence under test for rule 1, written out here rather than read from the
/// service so this suite pins the WORDS and not merely the service's agreement with
/// itself.
const FEATURE_NOT_ENABLED: &str = "The request is rejected because the AI Analyst feature is not enabled for the delegated administrator account. Enable the AI_ANALYST feature on your delegated administrator's detector to use the Investigation API.";

/// The sentence under test for rule 3.
const INVESTIGATION_NOT_FOUND: &str = "Resource not found.";

/// The DETECTOR family's ownership sentence (rule 2).
const DETECTOR_NOT_OWNED: &str =
    "The request is rejected because the input detectorId is not owned by the current account.";

/// The parameter sentence a value that could not be a `detectorId` gets (rule 2).
const DETECTOR_PARAMETER: &str =
    "The request is rejected because the parameter detectorId has an invalid value.";

async fn create_investigation(router: &Router, detector_id: &str, prompt: &str) -> (u16, Value) {
    call(
        router,
        "POST",
        &format!("/detector/{detector_id}/investigation"),
        Some(json!({"triggerPrompt": prompt})),
    )
    .await
}

async fn get_investigation(
    router: &Router,
    detector_id: &str,
    investigation_id: &str,
) -> (u16, Value) {
    call(
        router,
        "GET",
        &format!("/detector/{detector_id}/investigation/{investigation_id}"),
        None,
    )
    .await
}

async fn list_investigations(router: &Router, detector_id: &str) -> (u16, Value) {
    call(
        router,
        "POST",
        &format!("/detector/{detector_id}/investigation/list"),
        Some(json!({})),
    )
    .await
}

/// Initiate an investigation and return its id, failing the test unless the call
/// succeeded.
async fn initiated(router: &Router, detector_id: &str, prompt: &str) -> String {
    let (status, body) = create_investigation(router, detector_id, prompt).await;
    assert_eq!(
        status, 200,
        "create-investigation {prompt:?}: expected 200, got {status} {body:?}"
    );
    body["investigationId"]
        .as_str()
        .unwrap_or_else(|| panic!("create-investigation {prompt:?}: expected an id, got {body:?}"))
        .to_string()
}

/// Assert one refusal: the status, the envelope shape, the class and the sentence
/// word for word, and that nothing of the request was quoted back.
fn assert_refusal(case: &str, status: u16, body: &Value, expected: &Expected, sent: &str) {
    assert_eq!(
        status, expected.status,
        "case {case:?}: expected HTTP {}, got {status} {body:?}",
        expected.status
    );
    assert_eq!(
        error_code(body),
        expected.class,
        "case {case:?}: expected the {} class, got {body:?}",
        expected.class
    );
    assert_eq!(
        error_shape(body),
        expected.envelope,
        "case {case:?}: expected the {} envelope shape, got {body:?}",
        expected.envelope
    );
    let message = body["message"].as_str().unwrap_or_default();
    assert_eq!(
        message, expected.message,
        "case {case:?}: expected message {:?}, got {message:?}",
        expected.message
    );
    assert!(
        sent.is_empty() || !message.contains(sent),
        "case {case:?}: the refusal must not quote {sent:?} back, got {message:?}"
    );
}

/// The answer one refusal must carry.
struct Expected {
    status: u16,
    class: &'static str,
    envelope: &'static str,
    message: &'static str,
}

/// The prerequisite refusal, for every row of rule 1 and 2's tables.
const FEATURE_REFUSAL: Expected = Expected {
    status: 400,
    class: INVALID_INPUT,
    envelope: ENVELOPE,
    message: FEATURE_NOT_ENABLED,
};

// ---------------------------------------------------------------------------
// Rule 1: the AI_ANALYST prerequisite.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_create_is_served_only_to_a_detector_that_enabled_the_feature() {
    // (case, the create body, an update applied after it) — every way the stored
    // switch can be OFF when the create arrives.
    let off: [(&str, Value, Option<Value>); 4] = [
        (
            "a bare create, which leaves the opt-in feature off",
            json!({"enable": true}),
            None,
        ),
        (
            "a create that names other features but not this one",
            json!({"enable": true, "features": [{"name": "S3_DATA_EVENTS", "status": "ENABLED"}]}),
            None,
        ),
        (
            "a create that disables it explicitly",
            json!({"enable": true, "features": [{"name": "AI_ANALYST", "status": "DISABLED"}]}),
            None,
        ),
        (
            "a create that enabled it and an update that switched it back off",
            json!({"enable": true, "features": ai_analyst_features()}),
            Some(json!({"features": [{"name": "AI_ANALYST", "status": "DISABLED"}]})),
        ),
    ];

    for (case, create, update) in off {
        // One detector per account per Region, so each row gets its own store.
        let router = new_router();
        let (status, created) = create_detector(&router, create.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: create-detector {create}: got {status} {created:?}"
        );
        let detector_id = created["detectorId"].as_str().unwrap_or_default().to_string();
        if let Some(update) = update {
            let (status, body) = call(
                &router,
                "POST",
                &format!("/detector/{detector_id}"),
                Some(update),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: update-detector: got {status} {body:?}"
            );
        }

        let (status, refused) =
            create_investigation(&router, &detector_id, "investigate without the feature").await;
        assert_refusal(case, status, &refused, &FEATURE_REFUSAL, &detector_id);

        // Nothing was recorded: the list the refused create would have grown is
        // still empty, and it is served (the list carries no such prerequisite).
        let (status, listed) = list_investigations(&router, &detector_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: list-investigations: expected 200, got {status} {listed:?}"
        );
        assert_eq!(
            listed["investigations"],
            json!([]),
            "case {case:?}: a refused create must record no investigation, got {listed:?}"
        );

        // Switching the feature ON is all that changes, and the same call is served.
        let (status, body) = call(
            &router,
            "POST",
            &format!("/detector/{detector_id}"),
            Some(json!({"features": ai_analyst_features()})),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: update-detector enabling the feature: got {status} {body:?}"
        );
        let id = initiated(&router, &detector_id, "investigate with the feature").await;
        let (status, got) = get_investigation(&router, &detector_id, &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: get-investigation: expected 200, got {status} {got:?}"
        );
        assert_eq!(
            got["investigation"]["investigationId"],
            json!(id),
            "case {case:?}: expected the investigation to read back, got {got:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 2: a detectorId the store does not resolve.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unresolved_detector_is_refused_as_this_services_own_400() {
    let router = new_router();
    let detector_id = investigation_detector(&router).await;
    let investigation_id = initiated(&router, &detector_id, "a real investigation").await;

    // A second account/Region is not reachable from this harness, so the forms below
    // are what a caller can address: an id nothing minted, the same id in another
    // case, and values outside the 32-hexadecimal form on either side of it. The
    // model's own constraint is `len 1..300`, far wider, so every row reaches the
    // handler.
    let ownership = Expected {
        status: 400,
        class: INVALID_INPUT,
        envelope: ENVELOPE,
        message: DETECTOR_NOT_OWNED,
    };
    let parameter = Expected {
        status: 400,
        class: INVALID_INPUT,
        envelope: ENVELOPE,
        message: DETECTOR_PARAMETER,
    };
    let rows: [(&str, String, &Expected); 6] = [
        (
            "a well-formed id nothing minted",
            ABSENT_DETECTOR_ID.to_string(),
            &ownership,
        ),
        (
            "the same id upper-cased, which is well formed too",
            ABSENT_DETECTOR_ID.to_uppercase(),
            &ownership,
        ),
        (
            "31 hexadecimal characters, one under the form",
            "deadbeefdeadbeefdeadbeefdeadbee".to_string(),
            &parameter,
        ),
        (
            "33 hexadecimal characters, one over the form",
            "deadbeefdeadbeefdeadbeefdeadbeef0".to_string(),
            &parameter,
        ),
        (
            "32 characters outside the hexadecimal alphabet",
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz".to_string(),
            &parameter,
        ),
        (
            "a value that is no id at all",
            "not-a-detector-id".to_string(),
            &parameter,
        ),
    ];

    for (why, id, expected) in rows {
        let (status, refused) = create_investigation(&router, &id, "investigate elsewhere").await;
        assert_refusal(
            &format!("create: {why}"),
            status,
            &refused,
            expected,
            id.as_str(),
        );

        let (status, refused) = get_investigation(&router, &id, &investigation_id).await;
        assert_refusal(
            &format!("read: {why}"),
            status,
            &refused,
            expected,
            id.as_str(),
        );

        let (status, refused) = list_investigations(&router, &id).await;
        assert_refusal(
            &format!("list: {why}"),
            status,
            &refused,
            expected,
            id.as_str(),
        );
    }

    // A detector the account held a moment ago and deleted is in the same position
    // as one it never held: the refusal is a LOOKUP, not a constant.
    let (status, body) = call(&router, "DELETE", &format!("/detector/{detector_id}"), None).await;
    assert_eq!(status, 200, "delete-detector: got {status} {body:?}");
    let (status, refused) = get_investigation(&router, &detector_id, &investigation_id).await;
    assert_refusal(
        "read: the detector's own id after its delete",
        status,
        &refused,
        &ownership,
        detector_id.as_str(),
    );
}

// ---------------------------------------------------------------------------
// Rule 3: an investigationId the detector does not hold.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_absent_investigation_is_the_modelled_404_in_the_familys_own_words() {
    let router = new_router();
    let detector_id = investigation_detector(&router).await;
    let held = initiated(&router, &detector_id, "a real investigation").await;

    // Another detector, in its own store, mints an id this detector does not hold:
    // a value that IS an investigation id somewhere is still absent here.
    let elsewhere = new_router();
    let other_detector = investigation_detector(&elsewhere).await;
    let minted_elsewhere = initiated(&elsewhere, &other_detector, "another detector's work").await;

    let not_found = Expected {
        status: 404,
        class: RESOURCE_NOT_FOUND,
        envelope: RESOURCE_NOT_FOUND,
        message: INVESTIGATION_NOT_FOUND,
    };
    // Every row is a value `InvestigationId` admits — `len 1..64`, pattern
    // `^[a-fA-F0-9\-]+$` (`smithy_query(operation="GetInvestigation")`) — so each
    // reaches the handler rather than the boundary.
    let longest = "f".repeat(64);
    let rows: [(&str, String); 6] = [
        ("one character, the modelled minimum", "0".to_string()),
        ("64 characters, the modelled maximum", longest.clone()),
        (
            "a UUID spelling nothing minted",
            "deadbeef-dead-beef-dead-beefdeadbeef".to_string(),
        ),
        (
            "a bare separator, which the pattern admits",
            "-".to_string(),
        ),
        ("the held id upper-cased", held.to_uppercase()),
        (
            "an id another detector minted",
            minted_elsewhere.clone(),
        ),
    ];

    for (why, id) in rows {
        let (status, refused) = get_investigation(&router, &detector_id, &id).await;
        assert_refusal(&format!("read: {why}"), status, &refused, &not_found, &id);
    }

    // The same read over the id this detector DOES hold succeeds, so each refusal
    // above was a lookup over the store.
    let (status, got) = get_investigation(&router, &detector_id, &held).await;
    assert_eq!(
        status, 200,
        "read: the held id: expected 200, got {status} {got:?}"
    );

    // A detector with no AI_ANALYST feature still READS: the prerequisite is the
    // create's, so an absent investigation under it is this same 404 rather than
    // the feature refusal.
    let bare_router = new_router();
    let bare_detector = enabled_detector(&bare_router).await;
    let (status, refused) = get_investigation(&bare_router, &bare_detector, &held).await;
    assert_refusal(
        "read: an absent investigation under a detector without the feature",
        status,
        &refused,
        &not_found,
        &held,
    );

    // And the sentences this suite pins are the ones the service itself holds, so a
    // change to either is a change to both.
    assert_eq!(
        state::INVESTIGATION_NOT_FOUND_MESSAGE, INVESTIGATION_NOT_FOUND,
        "the service's absent-investigation sentence must be the one asserted here"
    );
    assert_eq!(
        state::INVESTIGATION_FEATURE_DENIED_MESSAGE, FEATURE_NOT_ENABLED,
        "the service's feature-prerequisite sentence must be the one asserted here"
    );
}
