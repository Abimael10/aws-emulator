// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the usage-MONITORING workflow `GetKeyLastUsage` serves, driven
//! over the wire through the shared [`super::harness`].
//!
//! The rule under test: once KMS's usage tracking has produced a record for a key, every
//! later `GetKeyLastUsage` reports a record — "if the KeyLastUsage response element is
//! present, the key has been used for a successful cryptographic operation since the
//! TrackingStartDate" (`smithy_query(query="doc",shape="GetKeyLastUsage")`) is a monotone
//! fact about the key — and what is reported is "the last successful cryptographic
//! operation performed with" it (same doc), so a later successful use replaces the record
//! already reported and the reported `Timestamp` only moves forward. No call that is not a
//! use of the key — a reporting read or a key-management write — changes it.
//!
//! What a read taken straight after the FIRST use of a key is answered — the empty record,
//! because the tracking has produced none for the key yet
//! ([`state::USAGE_TRACKING_LAG`]) — is [`super::key_last_usage`]'s subject, and the
//! advance of the record across successive uses by ONE operation is
//! [`super::key_last_usage_monotonicity`]'s. This suite is about the three dimensions
//! neither covers: the later use being performed by a DIFFERENT tracked operation than the
//! one that produced the record, the record surviving the calls that are not uses at all,
//! and a BURST of uses the tracking has to keep up with, where the record of a use not yet
//! tracked must never take the key back to the "not used since tracking began" state.
//!
//! The rows of the table are the tracked operations
//! (`smithy_query(query="shape",shape="KeyLastUsageTrackingOperation")`) that a symmetric
//! encryption key serves, so they differ in the operation, in what it takes as input (a
//! plaintext, a ciphertext, a key spec, a key-pair spec) and in what it produces.

use serde_json::{json, Value};

use super::harness::{b64, create_key, ok, router, text, tracked_last_usage};

use service_com_amazonaws_kms_trentservice::state::USAGE_TRACKING_LAG;

/// The plaintext every row seals.
const MESSAGE: &[u8] = b"the monitored key";

/// How many uses the burst case performs back to back, with a read after each. More than
/// a couple, so the reads land while the tracking is being handed record after record.
const BURST_USES: usize = 12;

/// The members of `KeyLastUsageData`
/// (`smithy_query(query="shape",shape="KeyLastUsageData")`), each of which a record the
/// tracking has produced fills and the empty record leaves absent.
const USAGE_MEMBERS: [&str; 4] = [
    "Operation",
    "Timestamp",
    "CloudTrailEventId",
    "KmsRequestId",
];

/// How long a case is willing to wait for the tracking: the start-up span over, so a case
/// is never bounded by how fast the host runs.
fn tracking_deadline() -> std::time::Instant {
    std::time::Instant::now() + USAGE_TRACKING_LAG * 20
}

/// One row of the table: a tracked operation and the request that performs it on the key
/// `key_id` names, given a ciphertext sealed under that key for the rows whose input is
/// one.
struct Row {
    /// The operation, as `KeyLastUsageData.Operation` names it.
    operation: &'static str,
    /// The request that performs `operation` on the key.
    request: fn(&str, &Value) -> Value,
}

/// The table: the tracked operations a symmetric encryption key serves.
fn table() -> Vec<Row> {
    vec![
        Row {
            operation: "Encrypt",
            request: |key_id, _| json!({"KeyId": key_id, "Plaintext": b64(MESSAGE)}),
        },
        Row {
            operation: "Decrypt",
            request: |_, ciphertext| json!({"CiphertextBlob": ciphertext}),
        },
        Row {
            operation: "ReEncrypt",
            request: |key_id, ciphertext| json!({
                "CiphertextBlob": ciphertext,
                "DestinationKeyId": key_id
            }),
        },
        Row {
            operation: "GenerateDataKey",
            request: |key_id, _| json!({"KeyId": key_id, "KeySpec": "AES_256"}),
        },
        Row {
            operation: "GenerateDataKeyWithoutPlaintext",
            request: |key_id, _| json!({"KeyId": key_id, "NumberOfBytes": 32}),
        },
        Row {
            operation: "GenerateDataKeyPair",
            request: |key_id, _| json!({"KeyId": key_id, "KeyPairSpec": "ECC_NIST_P256"}),
        },
        Row {
            operation: "GenerateDataKeyPairWithoutPlaintext",
            request: |key_id, _| json!({"KeyId": key_id, "KeyPairSpec": "ECC_NIST_P256"}),
        },
    ]
}

/// The `GetKeyLastUsage` answer for `key_id`, taken as it is — this is the read a
/// monitoring client takes, so it never waits for anything.
async fn reported(router: &smithy_server_runtime::MultiServiceRouter, key_id: &str) -> Value {
    ok(router, "GetKeyLastUsage", &json!({"KeyId": key_id})).await
}

/// The `Timestamp` of the record `answer` carries, in milliseconds since the epoch as the
/// wire renders an instant, or `None` when the record is empty.
fn timestamp(answer: &Value) -> Option<f64> {
    answer["KeyLastUsage"]["Timestamp"].as_f64()
}

/// Assert `answer` reports a record of `operation` at or after `floor` — the key has been
/// used, so the element must be present, every member of it filled, and the reported
/// instant must not have moved backwards — and report its `Timestamp`.
fn assert_present(case: &str, operation: &str, answer: &Value, floor: f64) -> f64 {
    let Some(reported_at) = timestamp(answer) else {
        panic!(
            "case {case}: the tracking has produced a record for this key, so the element \
             must stay present — expected a Timestamp at or after {floor}, got {answer}"
        );
    };
    for member in USAGE_MEMBERS {
        assert!(
            !answer["KeyLastUsage"][member].is_null(),
            "case {case}: a record the tracking has produced fills {member}, got {answer}"
        );
    }
    assert_eq!(
        answer["KeyLastUsage"]["Operation"]
            .as_str()
            .unwrap_or_default(),
        operation,
        "case {case}: expected the record to name {operation}, got {answer}"
    );
    assert!(
        reported_at >= floor,
        "case {case}: the reported Timestamp must not move backwards from {floor}, got \
         {reported_at}"
    );
    reported_at
}

/// The FIRST record of a key, once the tracking has produced it — the shared harness owns
/// that wait ([`tracked_last_usage`]), since every suite that is about WHAT was recorded
/// needs it; this adds only the floor the record must not precede.
async fn first_record(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    operation: &str,
    key_id: &str,
    floor: f64,
) -> Value {
    let answer = tracked_last_usage(router, case, key_id, operation, &Value::Null).await;
    assert_present(case, operation, &answer, floor);
    answer
}

/// Perform `operation` on `key_id` until the record reported for the key is that use's:
/// its `Timestamp` strictly after the one `before` carries. The wire renders an instant in
/// milliseconds, so the advance is driven by repeating the USE — never by sleeping — and
/// every read along the way is asserted to report a present record of `operation` that has
/// not moved backwards, which is what a use the tracking has not caught up with must not
/// disturb.
async fn use_until_advanced(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
    key_id: &str,
    before: &Value,
) -> Value {
    let previous = timestamp(before).unwrap_or_else(|| {
        panic!("case {case}: this case starts from a record the tracking produced, got {before}")
    });
    let deadline = tracking_deadline();
    loop {
        ok(router, operation, request).await;
        let answer = reported(router, key_id).await;
        let reported_at = assert_present(case, operation, &answer, previous);
        if reported_at > previous {
            for member in ["CloudTrailEventId", "KmsRequestId"] {
                assert_ne!(
                    answer["KeyLastUsage"][member], before["KeyLastUsage"][member],
                    "case {case}: expected a {member} of this call's own, got the replaced \
                     record's, in {answer}"
                );
            }
            return answer;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case}: the record of a later use replaces the one reported, so the \
             Timestamp must advance past {previous}, got {answer}"
        );
        // The advance needs the wire's millisecond resolution to tick, so the loop waits
        // between uses rather than spinning.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

/// The rule: a later use by ANY tracked operation replaces the record already reported —
/// the element stays present, the record names the operation that performed the later use,
/// and its `Timestamp` advances.
#[tokio::test]
async fn a_later_use_by_any_tracked_operation_replaces_the_record_already_reported() {
    let router = router();
    let rows = table();

    // One key per row. Every key's first use is an `Encrypt`, so the record each row
    // advances was produced by an operation other than the row's own, and that call's
    // ciphertext is the input of the rows that need one.
    let mut keys: Vec<(String, Value, f64)> = Vec::new();
    for row in &rows {
        let metadata = create_key(&router, &json!({"Description": row.operation})).await;
        let key_id = text(row.operation, &metadata, "KeyId");
        let sealed = ok(
            &router,
            "Encrypt",
            &json!({"KeyId": &key_id, "Plaintext": b64(MESSAGE)}),
        )
        .await["CiphertextBlob"]
            .clone();
        keys.push((
            key_id,
            sealed,
            metadata["CreationDate"].as_f64().unwrap_or_default(),
        ));
    }

    // One wait for the whole table, until the tracking is recording each key: the key's
    // creation date is the floor of its first record, since tracking starts when the key
    // is minted.
    let mut records: Vec<Value> = Vec::new();
    for (row, (key_id, _, creation_date)) in rows.iter().zip(&keys) {
        let case = format!("{} first record", row.operation);
        records.push(first_record(&router, &case, "Encrypt", key_id, *creation_date).await);
    }

    for ((row, (key_id, sealed, _)), first) in rows.iter().zip(&keys).zip(&records) {
        let case = format!("{} after an Encrypt", row.operation);
        let advanced = use_until_advanced(
            &router,
            &case,
            row.operation,
            &(row.request)(key_id, sealed),
            key_id,
            first,
        )
        .await;

        // Reporting the record does not change it: asking again answers the same.
        let again = reported(&router, key_id).await;
        assert_eq!(
            again, advanced,
            "case {case}: a second read must answer the same record, got {again}"
        );
        assert_eq!(
            again["KeyId"].as_str().unwrap_or_default(),
            key_id,
            "case {case}: this answer names the key by its key ID, got {again}"
        );
    }
}

/// The rule, on the calls that are NOT uses of the key: a reporting read and a
/// key-management write are not cryptographic operations, so neither replaces the record
/// reported nor takes the element back to empty — and the next use still advances it.
#[tokio::test]
async fn the_reported_record_survives_every_call_that_is_not_a_use_of_the_key() {
    let router = router();
    let metadata = create_key(&router, &json!({"Description": "monitored"})).await;
    let key_id = text("setup", &metadata, "KeyId");
    let encrypt = json!({"KeyId": &key_id, "Plaintext": b64(MESSAGE)});
    ok(&router, "Encrypt", &encrypt).await;
    let first = first_record(
        &router,
        "setup",
        "Encrypt",
        &key_id,
        metadata["CreationDate"].as_f64().unwrap_or_default(),
    )
    .await;

    // Calls that do not use the key in a cryptographic operation: the reporting reads of
    // the key, its tagging, and the key-state changes the decommissioning workflow makes
    // while it watches the usage record ("first disable the key and monitor CloudTrail",
    // `smithy_query(query="doc",shape="GetKeyLastUsage")`).
    let not_a_use: Vec<(&str, Value)> = vec![
        ("DescribeKey", json!({"KeyId": &key_id})),
        ("GetKeyLastUsage", json!({"KeyId": &key_id})),
        ("GetKeyPolicy", json!({"KeyId": &key_id})),
        ("ListResourceTags", json!({"KeyId": &key_id})),
        (
            "TagResource",
            json!({"KeyId": &key_id, "Tags": [{"TagKey": "watched", "TagValue": "yes"}]}),
        ),
        ("DisableKey", json!({"KeyId": &key_id})),
        ("GetKeyRotationStatus", json!({"KeyId": &key_id})),
        ("EnableKey", json!({"KeyId": &key_id})),
    ];
    for (operation, request) in &not_a_use {
        ok(&router, operation, request).await;
        let answer = reported(&router, &key_id).await;
        assert_eq!(
            answer, first,
            "case {operation}: this call is not a cryptographic operation, so the record \
             reported must be untouched — expected {first}, got {answer}"
        );
    }

    // And the next use of the key still advances the record those calls left in place.
    let advanced =
        use_until_advanced(&router, "after the writes", "Encrypt", &encrypt, &key_id, &first).await;
    assert!(
        timestamp(&advanced) > timestamp(&first),
        "case after the writes: expected the record to advance past {first}, got {advanced}"
    );
}

/// The rule, under a BURST: a use the tracking has not caught up with never erases the
/// record already reported, so every read during a run of back-to-back uses reports a
/// present record whose `Timestamp` has not moved backwards.
#[tokio::test]
async fn a_burst_of_uses_never_takes_the_reported_record_back_to_empty() {
    let router = router();
    let metadata = create_key(&router, &json!({"Description": "burst"})).await;
    let key_id = text("setup", &metadata, "KeyId");
    let encrypt = json!({"KeyId": &key_id, "Plaintext": b64(MESSAGE)});
    ok(&router, "Encrypt", &encrypt).await;
    let first = first_record(
        &router,
        "setup",
        "Encrypt",
        &key_id,
        metadata["CreationDate"].as_f64().unwrap_or_default(),
    )
    .await;

    let mut floor = timestamp(&first).unwrap_or_default();
    for use_number in 2..=BURST_USES {
        ok(&router, "Encrypt", &encrypt).await;
        let answer = reported(&router, &key_id).await;
        // Each read is taken straight after a use, with no wait: the element is present
        // and the instant it reports is at or after the one the read before it saw.
        floor = assert_present(
            &format!("burst use {use_number}"),
            "Encrypt",
            &answer,
            floor,
        );
    }

    // Over the burst as a whole the reported record moved FORWARD: it is not the record
    // the first use produced.
    let advanced =
        use_until_advanced(&router, "burst end", "Encrypt", &encrypt, &key_id, &first).await;
    assert!(
        timestamp(&advanced) > timestamp(&first),
        "case burst end: after {BURST_USES} uses the record reported must be a later one \
         than {first}, got {advanced}"
    );
}
