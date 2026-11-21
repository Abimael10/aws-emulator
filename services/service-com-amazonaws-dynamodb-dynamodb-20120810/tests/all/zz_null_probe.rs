// SPDX-License-Identifier: CC0-1.0

//! The two answers a `null` standing for a member the input REQUIRES earns, side by side.
//!
//! The rule both suites that own these answers state is one rule with two observed answers:
//! the VECTOR-SEARCH surface answers such a `null` with the reader's own diagnosis of the
//! body — the protocol's `SerializationException`, whose prose names a position in the
//! request (`crate::body_form_and_stated_absences`'s rule 3) — while every other operation
//! answers it as the CONSTRAINT report it is
//! (`crate::required_null_member_reports`). Nothing about the model distinguishes
//! them: the member is `@required` either way, which is why the difference is recorded as a
//! list of operations rather than derived.
//!
//! This suite is the one place the two stand in the SAME test, over the same defect in the
//! same shape of body, so a change that merged the two groups fails here instead of silently
//! making one of them answer the other's error class.

use serde_json::{json, Value};

use crate::emulator::{call, emulator, error_code};

#[tokio::test]
async fn the_vector_surface_states_the_readers_diagnosis_and_every_other_operation_the_clause() {
    let router = emulator();

    // The vector read: a required string stated absent earns the protocol's own class, and
    // the prose is in the lowercase slot that class states a diagnosis in.
    let (status, diagnosed) = call(
        &router,
        "SearchVectors",
        json!({"TableName": "Split", "IndexName": null, "SearchVector": [{"N": "1"}],
               "TopK": 3}),
    )
    .await;
    assert_eq!(
        (status, error_code(&diagnosed).as_str()),
        (400, "SerializationException"),
        "the vector surface answers the reader's own diagnosis, got {status} with {diagnosed}"
    );
    assert!(
        diagnosed
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|stated| stated.contains("expected a string")),
        "that diagnosis names what the read expected, got {diagnosed}"
    );

    // The same defect in the same place on another operation: the constraint report, with
    // the member path the validating walk reports that member under.
    let (status, reported) = call(
        &router,
        "UpdateTimeToLive",
        json!({"TableName": "Split",
               "TimeToLiveSpecification": {"Enabled": null, "AttributeName": "ttl"}}),
    )
    .await;
    assert_eq!(
        (status, error_code(&reported).as_str()),
        (400, "ValidationException"),
        "every other operation answers the constraint report, got {status} with {reported}"
    );
    assert_eq!(
        reported
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        "1 validation error detected: Value null at 'timeToLiveSpecification.enabled' failed \
         to satisfy constraint: Member must not be null",
        "that report states one clause for the member with no value, got {reported}"
    );
}
