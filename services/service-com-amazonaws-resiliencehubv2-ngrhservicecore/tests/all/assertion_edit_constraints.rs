// SPDX-License-Identifier: CC0-1.0

//! The two halves of `UpdateAssertion`'s edit rule that `assertion_lifecycle` does not
//! reach: the requests the MODEL refuses, and what a SEQUENCE of edits of one record leaves.
//!
//! The rule under test, the same one `assertion_lifecycle` states for a single edit: an edit
//! addresses one stored assertion and replaces its stored `text` in place. So a request
//! whose values break a rule the model declares — a `@length` bound on `text`, the `Uuid`
//! form of `assertionId`, the `Arn` form of `serviceArn`, a `@required` member left out — is
//! refused HTTP 400 `ValidationException` and stores nothing, and an edit is never final:
//! after a sequence of edits the record holds the text the LAST text-carrying one supplied,
//! with `assertionId`, `serviceArn`, `source` and `createdAt` where the create put them and
//! `updatedAt` never going backwards.
//!
//! The suites are TABLES whose rows differ along what the rule generalises over: the FORM of
//! the violation, and the LENGTH of a sequence of edits with the position of a step that
//! supplies no text. The harness is the one `assertion_lifecycle` already builds, so both
//! files drive the same wire path (`MultiServiceRouter::dispatch`) and cannot drift.

use serde_json::{json, Value};

use crate::assertion_lifecycle::{
    assert_edited, create_one, create_service, emulator, error_code, listed_assertion,
    update_assertion, ASSERTION_TEXT_MAX_LENGTH, TEXT_BEFORE_THE_EDIT,
};

/// `Arn`'s modelled `@length` minimum, and an ARN-shaped value below it.
///
/// SOURCE: `smithy_query(query="operation", operation="UpdateAssertion")` reports
/// `serviceArn` targeting `Arn` with `length {min: 31}`.
const ARN_MIN_LENGTH: usize = 31;
const SHORT_ARN: &str = "arn:aws:resiliencehub";

/// An ARN-shaped value long enough to clear that minimum whose Region segment is not a
/// Region, so it breaks `Arn`'s `@pattern` alone.
const MALFORMED_ARN: &str = "arn:aws:resiliencehub:not_a_region:000000000000:service/x";

/// `Uuid`-shaped values that each break one nibble class of that shape's `@pattern`: a
/// version nibble past the modelled `[0-5]`, and a variant nibble outside `[089ab]`.
///
/// SOURCE: `smithy_query(query="operation", operation="UpdateAssertion")` reports
/// `assertionId` targeting `Uuid` with `length {min: 36, max: 36}` and pattern
/// `^[0-9a-f]{8}-[0-9a-f]{4}-[0-5][0-9a-f]{3}-[089ab][0-9a-f]{3}-[0-9a-f]{12}$`.
const UUID_WITH_AN_UNMODELLED_VERSION: &str = "0ac1f2e3-4b5d-6c6e-8f90-1a2b3c4d5e6f";
const UUID_WITH_AN_UNMODELLED_VARIANT: &str = "0ac1f2e3-4b5d-4c6e-cf90-1a2b3c4d5e6f";

#[tokio::test]
async fn an_edit_breaking_a_modelled_constraint_is_refused_and_changes_nothing() {
    let router = emulator();
    let service_arn = create_service(&router, "Constrained").await;
    let at_create = create_one(&router, "the stored assertion", &service_arn).await;
    let assertion_id = at_create["assertionId"]
        .as_str()
        .unwrap_or_else(|| panic!("no assertionId in {at_create:?}"))
        .to_string();

    // The short-ARN row below tests the length minimum it names, not the pattern alone.
    assert!(
        SHORT_ARN.len() < ARN_MIN_LENGTH,
        "the short-ARN row ({SHORT_ARN:?}) must be shorter than the modelled minimum of \
         {ARN_MIN_LENGTH}"
    );
    // ...and the uppercase row really is a different value from the minted id, so it tests
    // the lowercase-hexadecimal rule rather than resending a well-formed id.
    assert_ne!(
        assertion_id.to_uppercase(),
        assertion_id,
        "the uppercase row must differ from the minted id {assertion_id:?}"
    );

    // The rows differ along the FORM of the violation: a length minimum, a length maximum,
    // each nibble class `Uuid` constrains, the ARN grammar and its length minimum, and each
    // @required member of the request left out.
    let rows: Vec<(&str, Value)> = vec![
        (
            "text below the length minimum",
            json!({"serviceArn": service_arn, "assertionId": assertion_id, "text": ""}),
        ),
        (
            "text one past the length maximum",
            json!({
                "serviceArn": service_arn,
                "assertionId": assertion_id,
                "text": "x".repeat(ASSERTION_TEXT_MAX_LENGTH + 1),
            }),
        ),
        (
            "an assertionId one character short",
            json!({
                "serviceArn": service_arn,
                "assertionId": &assertion_id[..assertion_id.len() - 1],
                "text": "refused",
            }),
        ),
        (
            "an assertionId carrying an uppercase hex digit",
            json!({
                "serviceArn": service_arn,
                "assertionId": assertion_id.to_uppercase(),
                "text": "refused",
            }),
        ),
        (
            "an assertionId whose version nibble is past the modelled range",
            json!({
                "serviceArn": service_arn,
                "assertionId": UUID_WITH_AN_UNMODELLED_VERSION,
                "text": "refused",
            }),
        ),
        (
            "an assertionId whose variant nibble is outside the modelled set",
            json!({
                "serviceArn": service_arn,
                "assertionId": UUID_WITH_AN_UNMODELLED_VARIANT,
                "text": "refused",
            }),
        ),
        (
            "a serviceArn below the length minimum",
            json!({"serviceArn": SHORT_ARN, "assertionId": assertion_id, "text": "refused"}),
        ),
        (
            "a serviceArn whose Region segment is not a Region",
            json!({"serviceArn": MALFORMED_ARN, "assertionId": assertion_id, "text": "refused"}),
        ),
        (
            "no assertionId at all",
            json!({"serviceArn": service_arn, "text": "refused"}),
        ),
        (
            "no serviceArn at all",
            json!({"assertionId": assertion_id, "text": "refused"}),
        ),
    ];

    for (case, request) in rows {
        let (status, body) = update_assertion(&router, request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from UpdateAssertion, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body:?}"
        );
        assert!(
            body["assertion"].is_null(),
            "case {case:?}: a refusal reports no assertion, got {body:?}"
        );

        // Nothing was stored: the record still reads back exactly as the create wrote it.
        let from_list = listed_assertion(&router, case, &service_arn, &assertion_id).await;
        assert_eq!(
            from_list, at_create,
            "case {case:?}: a refused edit must leave the stored record untouched, got \
             {from_list:?}"
        );
    }
}

#[tokio::test]
async fn a_sequence_of_edits_leaves_the_text_the_last_one_supplied() {
    let router = emulator();
    let service_arn = create_service(&router, "Sequenced").await;

    // The rows differ along the LENGTH of the sequence and the position of a step that
    // supplies no text — such a step asks for no new text, so the text in force stays where
    // the step before it put it.
    let rows: Vec<(&str, Vec<Option<&str>>)> = vec![
        ("one edit", vec![Some("RTO <= 60s.")]),
        (
            "two edits",
            vec![Some("RTO <= 60s."), Some("RPO <= 5s; warm standby.")],
        ),
        (
            "an omission between two edits",
            vec![Some("First replacement."), None, Some("Final replacement.")],
        ),
        (
            "an omission last",
            vec![Some("Only replacement."), None, None],
        ),
        (
            "five edits",
            vec![
                Some("one"),
                Some("two"),
                Some("three"),
                Some("four"),
                Some("five"),
            ],
        ),
    ];

    for (case, steps) in rows {
        let at_create = create_one(&router, case, &service_arn).await;
        let assertion_id = at_create["assertionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no assertionId in {at_create:?}"))
            .to_string();

        let mut in_force = TEXT_BEFORE_THE_EDIT.to_string();
        let mut previous_updated_at = at_create["updatedAt"]
            .as_f64()
            .unwrap_or_else(|| panic!("case {case:?}: no updatedAt timestamp in {at_create:?}"));
        for (step, text) in steps.iter().enumerate() {
            let mut request = json!({"serviceArn": service_arn, "assertionId": assertion_id});
            if let Some(text) = text {
                request["text"] = json!(text);
                in_force = (*text).to_string();
            }
            let (status, body) = update_assertion(&router, request).await;
            assert_eq!(
                status, 200,
                "case {case:?} step {step}: expected HTTP 200 from UpdateAssertion, got {status} \
                 {body:?}"
            );
            let edited = body["assertion"].clone();
            assert_edited(case, &in_force, &service_arn, &at_create, &edited);

            // Read as a timestamp rather than compared through an Option, so a non-numeric
            // encoding fails here instead of making the comparison vacuous.
            let updated_at = edited["updatedAt"].as_f64().unwrap_or_else(|| {
                panic!("case {case:?} step {step}: no updatedAt timestamp in {edited:?}")
            });
            assert!(
                updated_at >= previous_updated_at,
                "case {case:?} step {step}: updatedAt must not go backwards, got {updated_at} \
                 after {previous_updated_at}"
            );
            previous_updated_at = updated_at;
        }

        // Every later read reports the text in force after the last step, and the sequence
        // left one record under that id rather than a second one beside it.
        let from_list = listed_assertion(&router, case, &service_arn, &assertion_id).await;
        assert_eq!(
            from_list["text"].as_str(),
            Some(in_force.as_str()),
            "case {case:?}: the last edit's text is what the store holds, got {:?}",
            from_list["text"]
        );
        assert_edited(case, &in_force, &service_arn, &at_create, &from_list);
    }
}
