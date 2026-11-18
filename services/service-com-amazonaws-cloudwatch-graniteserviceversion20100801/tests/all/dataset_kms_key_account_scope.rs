// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the ACCOUNT half of the key a dataset association names: a `KmsKeyArn`
//! whose account segment is not the calling identity's own account.
//!
//! The rule: an association names a key by a fully qualified ARN
//! (`AssociateDatasetKmsKeyInput$KmsKeyArn`, `@pattern` with a 12-digit account segment), and a
//! key belonging to an account other than the caller's is one nothing about this association can
//! ever be validated against — so the request is refused as the invalid parameter it is, in the
//! request-validation class, naming both accounts, and the dataset keeps whatever association it
//! already carried. The rows differ along what a caller can vary about that segment: an account
//! above the caller's own, one below it, a never-used account, and the partition the ARN is
//! written in; the caller's OWN account is the control that is not refused on these grounds.

use serde_json::{json, Value};

use crate::harness::{build_router, call, call_in_query_mode, error_code, error_message, ok};

/// The account this emulator serves, which is the account every caller of it speaks for.
fn caller_account() -> String {
    ::emulator_server::emulator_account_id().to_string()
}

/// A key ARN of the form the member's `@pattern` admits, in `partition` and `account`.
fn key_arn(partition: &str, account: &str) -> String {
    format!("arn:{partition}:kms:us-east-1:{account}:key/1a2b3c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d")
}

/// The sentence this service states for a key of another account: the two accounts a caller has
/// to reconcile, and nothing else.
fn account_mismatch(key_account: &str, caller: &str) -> String {
    format!("KMS key account {key_account} does not match caller account {caller}")
}

/// The `KmsKeyArn` the default dataset of this Region reports, or `None` when it carries none.
async fn reported_key(router: &smithy_server_runtime::MultiServiceRouter) -> Option<String> {
    ok(router, "GetDataset", &json!({"DatasetIdentifier": "default"}))
        .await
        .get("KmsKeyArn")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// A key ARN whose account segment is not the caller's own is refused as an invalid parameter,
/// naming both accounts, and writes nothing.
#[tokio::test]
async fn a_key_of_another_account_is_refused_and_writes_nothing() {
    // (the case, the partition the ARN names, the account segment it carries)
    let rows: [(&str, &str, &str); 4] = [
        ("an account numerically above the caller's", "aws", "999999999999"),
        ("an account numerically below the caller's", "aws", "000000000001"),
        ("the account id AWS's own examples use", "aws", "123456789012"),
        // The partition is not what decides this refusal: the ACCOUNT segment is.
        ("another partition's account", "aws-cn", "210987654321"),
    ];

    let caller = caller_account();
    for (case, partition, account) in rows {
        let router = build_router();
        let foreign = key_arn(partition, account);
        let (status, answered) = call(
            &router,
            "AssociateDatasetKmsKey",
            &json!({"DatasetIdentifier": "default", "KmsKeyArn": foreign}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {answered}"
        );
        let message = error_message(&answered);
        assert_eq!(
            message,
            account_mismatch(account, &caller),
            "case {case:?}: expected the two-account sentence, got {message:?}"
        );
        assert_eq!(
            reported_key(&router).await,
            None,
            "case {case:?}: a refused association must leave the dataset with no key"
        );
    }
}

/// The class that refusal travels under: the request-validation class, whose legacy query code
/// — what a query-compatible client branches on — is `ValidationError`.
#[tokio::test]
async fn the_account_refusal_carries_the_request_validation_class() {
    let router = build_router();
    let foreign = key_arn("aws", "123456789012");
    let (status, headers, answered) = call_in_query_mode(
        &router,
        "AssociateDatasetKmsKey",
        &json!({"DatasetIdentifier": "default", "KmsKeyArn": foreign}),
    )
    .await;
    let case = "a key of another account, asked for by a query-compatible client";
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {answered}"
    );
    let reported = headers
        .get("x-amzn-query-error")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        reported, "ValidationError;Sender",
        "case {case:?}: expected the legacy query code of the request-validation class, got \
         {reported:?}"
    );
    assert_eq!(
        error_code(&answered),
        "ValidationException",
        "case {case:?}: expected the request-validation class in the body, got {answered}"
    );
}

/// The control: a key ARN of the CALLER's own account is not refused on these grounds — with no
/// KMS mounted to state the key's own facts, the association is written and read back.
#[tokio::test]
async fn a_key_of_the_callers_own_account_is_not_refused_on_the_account() {
    let router = build_router();
    let case = "a key ARN of the caller's own account";
    let own = key_arn("aws", &caller_account());
    let (status, answered) = call(
        &router,
        "AssociateDatasetKmsKey",
        &json!({"DatasetIdentifier": "default", "KmsKeyArn": own.clone()}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the association to be accepted, got {status} with {answered}"
    );
    assert_eq!(
        reported_key(&router).await,
        Some(own),
        "case {case:?}: the dataset must report the key it was associated with"
    );
}
