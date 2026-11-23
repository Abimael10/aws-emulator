// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the request a response of this service NAMES, driven over the wire
//! through the shared [`super::harness`].
//!
//! The rule under test: every response this service sends names the request it answered, in
//! the response header this protocol reports that id in (`x-amzn-RequestId`, matched
//! case-insensitively as HTTP matches a field name) — on a success, on a modelled error, on
//! a constraint refusal, on an answer no operation produced, freshly for each request. That
//! id is what an SDK surfaces as the call's `ResponseMetadata.RequestId`.
//!
//! And the id is not only a diagnostic here: `KeyLastUsageData$KmsRequestId` is "the request
//! ID of the cryptographic operation"
//! (`smithy_query(query="shape",shape="KeyLastUsageData")`), so the id the response to a
//! cryptographic call carries and the id the key's usage record carries are ONE value — a
//! caller holding its own response recognises the use it made of the key. The third surface
//! that same id joins, the CloudTrail record of the call, is CloudTrail's to report, so it
//! is pinned where both services are mounted
//! (`core/router/tests/all/kms_management_events.rs`).
//!
//! The rows span what the rule generalises over: the CLASS of answer (a success with a body,
//! the empty 200, a modelled error, a constraint refusal the walk raised before any handler
//! ran, the 501 of an operation this emulator has not implemented, and a target naming no
//! operation at all) rather than one operation's success.

use serde_json::{json, Value};

use super::harness::{b64, create_key, ok, respond, router, text, tracked_last_usage, ACCOUNT_KEY,
    REGION};

/// The header this protocol reports the id of the request a response answers in. AWS spells
/// it `x-amzn-RequestId`; a field name is matched case-insensitively, so the lower-case
/// spelling finds it however the renderer wrote it.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// The id the response to one call named its request by, or `""` when it named none.
async fn named_request(
    router: &smithy_server_runtime::MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, String) {
    let response = respond(router, REGION, ACCOUNT_KEY, operation, body).await;
    let status = response.status().as_u16();
    let named = response
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    (status, named)
}

/// A key id of the right form that this account holds no key for, so a read of it is the
/// modelled "not found" rather than a malformed identifier.
const ABSENT_KEY_ID: &str = "11111111-2222-3333-4444-555555555555";

/// A custom key store id of the right form that this account holds no key store for: the
/// row about the 501 class drives an operation this emulator has not implemented, which
/// answers that status before it judges the identifier at all.
const ABSENT_CUSTOM_KEY_STORE_ID: &str = "cks-1111111111111111111";

/// The message every cryptographic row below seals.
const MESSAGE: &[u8] = b"the request a response names";

/// The rule: every response names the request it answered, whatever class of answer it is,
/// and each request is named by an id of its own.
#[tokio::test]
async fn every_response_names_the_request_it_answered() {
    let router = router();
    let key_id = text(
        "setup",
        &create_key(&router, &json!({"Description": "request ids"})).await,
        "KeyId",
    );

    // (case, operation, request, the status that class of answer carries — `None` where
    // this row is about the ANSWER naming its request and another suite owns the status)
    let rows: Vec<(&str, &str, Value, Option<u16>)> = vec![
        (
            "a success carrying a body",
            "DescribeKey",
            json!({"KeyId": key_id}),
            Some(200),
        ),
        (
            "the empty 200 of an operation that returns nothing",
            "EnableKey",
            json!({"KeyId": key_id}),
            Some(200),
        ),
        (
            "a cryptographic operation",
            "Encrypt",
            json!({"KeyId": key_id, "Plaintext": b64(MESSAGE)}),
            Some(200),
        ),
        (
            "a modelled error: the key is not there",
            "DescribeKey",
            json!({"KeyId": ABSENT_KEY_ID}),
            Some(400),
        ),
        (
            "a constraint refusal raised before any handler ran",
            "DescribeKey",
            json!({"KeyId": ""}),
            Some(400),
        ),
        (
            "an operation this emulator has not implemented",
            "DeleteCustomKeyStore",
            json!({"CustomKeyStoreId": ABSENT_CUSTOM_KEY_STORE_ID}),
            Some(501),
        ),
        (
            "an answer no operation produced: a target naming no operation of this service",
            "NoSuchOperationAtAll",
            json!({}),
            None,
        ),
    ];

    let mut named: Vec<(&str, String)> = Vec::new();
    for (case, operation, request, expected_status) in rows {
        let (status, id) = named_request(&router, operation, &request).await;
        if let Some(expected) = expected_status {
            assert_eq!(
                status, expected,
                "case {case:?}: expected HTTP {expected}, got {status}"
            );
        }
        assert!(
            !id.is_empty(),
            "case {case:?}: every response names the request it answered in \
             {REQUEST_ID_HEADER}, got none"
        );
        named.push((case, id));
    }

    // One id per REQUEST: no two of the calls above are named by the same one, including the
    // two that named the same operation.
    for (index, (one, first)) in named.iter().enumerate() {
        for (other, second) in named.iter().skip(index + 1) {
            assert_ne!(
                first, second,
                "case {one:?} and {other:?}: each request is named by an id of its own, got \
                 {first:?} twice"
            );
        }
    }
}

/// The rule, continued: the id the response to a cryptographic call names its request by is
/// the `KmsRequestId` of the usage record that call wrote — one id, not two — and a later
/// call replaces it with its own.
#[tokio::test]
async fn a_cryptographic_calls_response_names_the_request_its_usage_record_reports() {
    let router = router();
    let key_id = text(
        "setup",
        &create_key(&router, &json!({"Description": "usage join"})).await,
        "KeyId",
    );

    // Rows differing in the cryptographic operation that used the key, so the join is the
    // RULE's and not one operation's: each one's response names the request its own record
    // reports.
    let rows: Vec<(&str, Value)> = vec![
        (
            "Encrypt",
            json!({
                "KeyId": key_id,
                "Plaintext": b64(MESSAGE),
                "EncryptionContext": {"purpose": "join", "tier": "test", "n": "3"},
            }),
        ),
        (
            "GenerateDataKey",
            json!({"KeyId": key_id, "KeySpec": "AES_256"}),
        ),
        (
            "ReEncrypt",
            json!({
                "CiphertextBlob": ok(
                    &router,
                    "Encrypt",
                    &json!({"KeyId": key_id, "Plaintext": b64(MESSAGE)}),
                )
                .await["CiphertextBlob"]
                    .clone(),
                "DestinationKeyId": key_id,
            }),
        ),
    ];

    let mut previous = Value::Null;
    let mut seen: Vec<String> = Vec::new();
    for (operation, request) in rows {
        let (status, answered_as) = named_request(&router, operation, &request).await;
        assert_eq!(
            status, 200,
            "case {operation:?}: expected the call to be served, got HTTP {status}"
        );

        // The record the tracking produces for that call reports the very request the
        // caller was answered under.
        let reported = tracked_last_usage(&router, operation, &key_id, operation, &previous).await;
        let record = &reported["KeyLastUsage"];
        assert_eq!(
            record["KmsRequestId"].as_str().unwrap_or_default(),
            answered_as,
            "case {operation:?}: the record must report the request the response named \
             ({answered_as:?}), got {reported}"
        );
        assert!(
            !seen.contains(&answered_as),
            "case {operation:?}: expected a request id of this call's own, got {answered_as:?} \
             again"
        );
        seen.push(answered_as);

        // Reading the record is not a use of the key, so it does not take the join over:
        // the read's own response names a different request.
        let (_, read_as) = named_request(&router, "GetKeyLastUsage", &json!({"KeyId": key_id})).await;
        assert_ne!(
            record["KmsRequestId"].as_str().unwrap_or_default(),
            read_as,
            "case {operation:?}: the record names the cryptographic call, not the read of it, \
             got {reported}"
        );
        previous = reported;
    }
}
