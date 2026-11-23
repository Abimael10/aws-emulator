// SPDX-License-Identifier: CC0-1.0

//! How this service words a `@pattern` violation, over the restJson1 wire.
//!
//! The rule under test: a member whose value is outside the `@pattern` its model declares
//! is refused with the modelled `ValidationException` (HTTP 400) worded as ONE sentence —
//! `The value provided for <ModelMemberPath> isn't valid.` — whose only variable part is
//! the dotted MODEL member path of the member that failed, in the model's own PascalCase
//! spelling and with no collection index. The sentence states neither the value the caller
//! sent nor the regular expression the server holds, and it carries no aggregated
//! `N validation error(s) detected` prefix.
//!
//! The members this covers are every `@pattern`-constrained member of this service's input
//! shapes, which the model gives as exactly two —
//! `S3Bucket$BucketName` (`BucketName`) and `S3Bucket$BucketOwner` (`AccountId`,
//! `^[0-9]{12}$`) — from
//! `smithy_query(query="closure", operation="UpdateConnectionRecordingPreferences")`. They
//! are refused by two different LAYERS, which is the other thing these rows span: the
//! boundary constraint walk refuses `BucketOwner`, whose pattern compiles, while
//! `BucketName`'s look-around pattern does not compile there and is checked by the handler
//! — and both must answer the one sentence.
//!
//! The rows differ along what the rule generalises over: which member failed, which layer
//! refused it, and the SHAPE of the offending value (an IP-shaped name, an upper-case
//! name, a name too short, a name too long, a name with an empty label, an owner too
//! short, too long, and one with a non-digit).

use serde_json::{json, Value};

use crate::identity_authorization::{Emulator, ACCOUNT_KEY, KEY_ARN, UPDATE};

/// The sentence this service words a `@pattern` refusal with, written out here rather than
/// read from the crate so the assertion does not compare the implementation with itself.
fn invalid_value(member: &str) -> String {
    format!("The value provided for {member} isn't valid.")
}

/// The dotted model member path of `S3Bucket$BucketName` as the model spells it.
const BUCKET_NAME_PATH: &str =
    "ConnectionRecordingPreferences.RecordingDestinations.S3Buckets.BucketName";

/// The dotted model member path of `S3Bucket$BucketOwner`.
const BUCKET_OWNER_PATH: &str =
    "ConnectionRecordingPreferences.RecordingDestinations.S3Buckets.BucketOwner";

/// An Update body carrying `owner` and `bucket`.
fn body(owner: &str, bucket: &str) -> Value {
    json!({
        "ConnectionRecordingPreferences": {
            "RecordingDestinations": {
                "S3Buckets": [{ "BucketOwner": owner, "BucketName": bucket }]
            },
            "KMSKeyArn": KEY_ARN,
        },
        "ClientToken": "pattern-case",
    })
}

/// RULE: a `@pattern` violation is one sentence naming the member by its model path, for
/// either `@pattern` member of this service's input and whichever layer refuses it.
#[tokio::test]
async fn a_pattern_violation_is_worded_as_the_one_sentence() {
    let too_long_name = "a".repeat(64);
    let valid_owner = "123456789012";
    let valid_name = "rdp-recordings";

    // (case, owner, bucket, the member path the sentence must name)
    let rows: [(&str, String, String, &str); 9] = [
        (
            "an IP-shaped bucket name, which the pattern's negative lookahead excludes",
            valid_owner.to_string(),
            "192.168.1.1".to_string(),
            BUCKET_NAME_PATH,
        ),
        (
            "an upper-case bucket name",
            valid_owner.to_string(),
            "Not-A-Bucket".to_string(),
            BUCKET_NAME_PATH,
        ),
        (
            "a bucket name one character below the pattern's length lookahead",
            valid_owner.to_string(),
            "ab".to_string(),
            BUCKET_NAME_PATH,
        ),
        (
            "a bucket name one character above it",
            valid_owner.to_string(),
            too_long_name.clone(),
            BUCKET_NAME_PATH,
        ),
        (
            "a bucket name with an empty label",
            valid_owner.to_string(),
            "two..dots".to_string(),
            BUCKET_NAME_PATH,
        ),
        (
            "an owner that is not digits at all",
            "abcdefghijkl".to_string(),
            valid_name.to_string(),
            BUCKET_OWNER_PATH,
        ),
        (
            "an owner one digit short",
            "12345678901".to_string(),
            valid_name.to_string(),
            BUCKET_OWNER_PATH,
        ),
        (
            "an owner one digit long",
            "1234567890123".to_string(),
            valid_name.to_string(),
            BUCKET_OWNER_PATH,
        ),
        (
            "an owner of twelve characters that are not all digits",
            "12345678901x".to_string(),
            valid_name.to_string(),
            BUCKET_OWNER_PATH,
        ),
    ];

    for (case, owner, bucket, member) in rows {
        let emulator = Emulator::new();
        // Enrolled, so nothing else about the account can refuse the call: what is left is
        // the member's shape.
        emulator.enroll(true).await;
        let reply = emulator
            .call(ACCOUNT_KEY, UPDATE, &body(&owner, &bucket))
            .await;
        assert_eq!(
            reply.status, 400,
            "case {case:?}: expected 400, got {reply:?}"
        );
        assert_eq!(
            reply.code, "ValidationException",
            "case {case:?}: expected ValidationException, got {reply:?}"
        );
        let expected = invalid_value(member);
        assert_eq!(
            reply.message, expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            reply.message
        );
        // The three things the sentence withholds: the aggregated envelope's prefix, the
        // value the caller sent, and the server's own requirement.
        for withheld in [
            "validation error",
            ".1.member.",
            "regular expression",
            owner.as_str(),
            bucket.as_str(),
        ] {
            assert!(
                !reply.message.contains(withheld),
                "case {case:?}: the sentence must withhold {withheld:?}, got {:?}",
                reply.message
            );
        }
        // And the refused call persisted nothing.
        let read = emulator
            .call(ACCOUNT_KEY, "GetConnectionRecordingPreferences", &json!({}))
            .await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: expected the refused write to persist nothing, got {read:?}"
        );
    }
}

/// RULE: the sentence is a TEMPLATE over the member, so the member it names is the one
/// that failed — two requests failing two different members get two different sentences,
/// and a request failing BOTH is still one sentence.
#[tokio::test]
async fn the_sentence_names_whichever_member_failed() {
    let emulator = Emulator::new();
    emulator.enroll(true).await;

    let by_name = emulator
        .call(ACCOUNT_KEY, UPDATE, &body("123456789012", "192.168.1.1"))
        .await;
    let by_owner = emulator
        .call(ACCOUNT_KEY, UPDATE, &body("abcdefghijkl", "rdp-recordings"))
        .await;
    assert_ne!(
        by_name.message, by_owner.message,
        "expected the sentence to name the member that failed, got {:?} twice",
        by_name.message
    );

    // Both members outside their patterns: one sentence, naming one of the two members —
    // the form is a sentence, not an envelope of clauses.
    let both = emulator
        .call(ACCOUNT_KEY, UPDATE, &body("abcdefghijkl", "192.168.1.1"))
        .await;
    assert_eq!(
        both.status, 400,
        "case \"both members outside their patterns\": expected 400, got {both:?}"
    );
    let candidates = [
        invalid_value(BUCKET_NAME_PATH),
        invalid_value(BUCKET_OWNER_PATH),
    ];
    assert!(
        candidates.contains(&both.message),
        "case \"both members outside their patterns\": expected one of {candidates:?}, got {:?}",
        both.message
    );
}

/// RULE: the refusal is about the member's SHAPE, so a request whose members are all
/// inside their patterns reaches the account-level gate instead — which is the positive
/// control that nothing here refuses a well-shaped request.
#[tokio::test]
async fn a_well_shaped_request_is_not_refused_by_the_pattern_check() {
    let emulator = Emulator::new();

    let long_name = "a".repeat(63);
    // Bucket names the modelled pattern ADMITS, including the two boundary lengths and a
    // single all-digit label (which the negative lookahead does not exclude, since it
    // requires at least one dot).
    let admitted = [
        "rdp-recordings",
        "abc",
        long_name.as_str(),
        "123",
        "one.two.three",
        "a-b.c-d",
    ];
    for bucket in admitted {
        let reply = emulator
            .call(ACCOUNT_KEY, UPDATE, &body("123456789012", bucket))
            .await;
        assert_eq!(
            reply.status, 403,
            "case {bucket:?}: expected a name the pattern admits to reach the account gate, \
             got {reply:?}"
        );
        assert_eq!(
            reply.code, "AccessDeniedException",
            "case {bucket:?}: expected the account gate's refusal, got {reply:?}"
        );
    }
}
