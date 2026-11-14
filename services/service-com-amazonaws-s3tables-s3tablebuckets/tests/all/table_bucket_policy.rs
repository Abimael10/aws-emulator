// SPDX-License-Identifier: CC0-1.0

//! The table bucket policy sub-resource: put, read, delete, read again.
//!
//! The rule under test: a table bucket carries a resource-based policy only once
//! a put attaches one. A read of a bucket that carries none is refused with the
//! modelled `NotFoundException` (404,
//! `smithy_query(query="operation", operation="GetTableBucketPolicy")`) and the
//! one fixed sentence for that class, which is how a client tells "no policy"
//! from a policy that grants nothing; a read after a put answers the document
//! that was attached, in the one layout the workspace reports an attached policy
//! in (`service_common::iam_policy::canonical_document`: a service whose API
//! takes a policy as a string renders the parsed document rather than echoing
//! the caller's bytes, so two clients that submit the same policy written
//! differently read the same text back); the delete is `@idempotent` and names no
//! policy, so it reports success whether or not one was there and leaves the
//! bucket with none; and a document that is not a resource policy at all is the
//! client request error the model's `BadRequestException` (400) names, with the
//! stored policy left as it was.
//!
//! The rows span what the rule generalises over: the shape of the document
//! (one statement, several statements, a `Deny`, a document with an `Id`, each
//! policy version the language admits), the LAYOUT it is submitted in (compact
//! and already-rendered, which must read back identically), the sub-resource's
//! state (never put, put, deleted), and whether the bucket exists at all.

use serde_json::json;

use crate::harness::{expected_bucket_arn, Emulator, Reply};

/// The region every case works in.
const REGION: &str = "us-east-1";

/// The layout an attached policy document is reported in, which is the one the
/// whole workspace renders such a document in — read from the shared crate
/// rather than spelled out here, so this suite cannot drift from the rendering
/// every other attaching service answers with.
fn reported(policy: &str) -> String {
    ::service_common::iam_policy::canonical_document(policy)
}

/// The fixed sentences, spelled out here rather than read from the crate, so
/// this suite pins the wording itself.
const NO_SUCH_POLICY: &str = "The specified bucket policy does not exist.";
const NO_SUCH_BUCKET: &str = "The specified bucket does not exist.";
const INVALID_POLICY: &str = "The specified resource policy is not valid.";

/// Documents that ARE resource policies, one per shape of the language this
/// service accepts: the policy versions it admits (`2012-10-17`, `2008-10-17`),
/// one statement and several, an `Allow` and a `Deny`, and a document carrying
/// an `Id`.
fn policies() -> Vec<(&'static str, String)> {
    vec![
        (
            "one allow statement",
            json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "AWS": "arn:aws:iam::000000000000:role/analytics" },
                    "Action": "s3tables:GetTable",
                    "Resource": "*",
                }],
            })
            .to_string(),
        ),
        (
            "an allow and a deny",
            json!({
                "Version": "2012-10-17",
                "Id": "table-bucket-policy",
                "Statement": [
                    {
                        "Effect": "Allow",
                        "Principal": "*",
                        "Action": ["s3tables:GetTable", "s3tables:ListTables"],
                        "Resource": "*",
                    },
                    {
                        "Effect": "Deny",
                        "Principal": "*",
                        "Action": "s3tables:DeleteTable",
                        "Resource": "*",
                    },
                ],
            })
            .to_string(),
        ),
        (
            "the earlier policy version the language admits",
            json!({
                "Version": "2008-10-17",
                "Statement": [{
                    "Effect": "Deny",
                    "NotPrincipal": { "AWS": "arn:aws:iam::000000000000:root" },
                    "NotAction": "s3tables:*",
                    "Resource": "*",
                }],
            })
            .to_string(),
        ),
    ]
}

/// Documents that are NOT resource policies, one per clause of the grammar they
/// break.
fn unacceptable_documents() -> Vec<(&'static str, String)> {
    vec![
        ("text that is not JSON", "not a policy at all".to_string()),
        ("JSON that is not an object", json!(["Allow"]).to_string()),
        (
            "a document declaring no statement",
            json!({ "Version": "2012-10-17" }).to_string(),
        ),
        (
            "a statement naming no principal",
            json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Action": "s3tables:GetTable",
                    "Resource": "*",
                }],
            })
            .to_string(),
        ),
        (
            "a statement declaring no effect",
            json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Principal": "*",
                    "Action": "s3tables:GetTable",
                    "Resource": "*",
                }],
            })
            .to_string(),
        ),
        (
            "a policy version that does not exist",
            json!({
                "Version": "2525-01-01",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": "s3tables:GetTable",
                    "Resource": "*",
                }],
            })
            .to_string(),
        ),
    ]
}

/// Assert `reply` is the refusal `(status, code, message)` names.
fn assert_refusal(reply: &Reply, status: u16, code: &str, message: &str, case: &str) {
    assert_eq!(
        reply.status, status,
        "case {case:?}: expected status {status}, got {actual} {body}",
        actual = reply.status,
        body = reply.body
    );
    assert_eq!(
        reply.error_code.as_deref(),
        Some(code),
        "case {case:?}: expected error type {code}, got {actual:?}",
        actual = reply.error_code
    );
    assert_eq!(
        reply.message(),
        message,
        "case {case:?}: expected the fixed sentence, got {actual:?}",
        actual = reply.message()
    );
}

/// Create one table bucket and answer its ARN.
async fn bucket(emulator: &Emulator, name: &str) -> String {
    let created = emulator
        .create_table_bucket(REGION, &json!({ "name": name }))
        .await;
    assert_eq!(
        created.status, 200,
        "creating the bucket {name:?} must succeed, got {status} {body}",
        status = created.status,
        body = created.body
    );
    created.text("arn")
}

#[tokio::test]
async fn a_bucket_policy_round_trips_and_is_absent_until_it_is_put() {
    let emulator = Emulator::new();
    let arn = bucket(&emulator, "policy-round-trip").await;

    // A bucket that was never given a policy has none to read.
    let missing = emulator.get_table_bucket_policy(REGION, &arn).await;
    assert_refusal(
        &missing,
        404,
        "NotFoundException",
        NO_SUCH_POLICY,
        "a policy that was never put",
    );

    // Deleting a policy that is not there is the no-op the operation's
    // `@idempotent` trait describes, and leaves the same 404 behind it.
    let deleted_absent = emulator.delete_table_bucket_policy(REGION, &arn).await;
    assert_eq!(
        deleted_absent.status, 204,
        "deleting an absent policy must report success, got {status} {body}",
        status = deleted_absent.status,
        body = deleted_absent.body
    );
    let still_missing = emulator.get_table_bucket_policy(REGION, &arn).await;
    assert_refusal(
        &still_missing,
        404,
        "NotFoundException",
        NO_SUCH_POLICY,
        "a policy after a delete that found none",
    );

    for (case, policy) in policies() {
        let put = emulator
            .put_table_bucket_policy(REGION, &arn, &policy)
            .await;
        assert_eq!(
            put.status, 200,
            "case {case:?}: the put must succeed, got {status} {body}",
            status = put.status,
            body = put.body
        );

        // The read answers the document that was attached, in the layout an
        // attached policy is reported in.
        let read = emulator.get_table_bucket_policy(REGION, &arn).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the read must succeed, got {status} {body}",
            status = read.status,
            body = read.body
        );
        assert_eq!(
            read.text("resourcePolicy"),
            reported(&policy),
            "case {case:?}: the read must answer the document the put stored"
        );
        // Twice over: a read changes nothing, so the second answers the same.
        let again = emulator.get_table_bucket_policy(REGION, &arn).await;
        assert_eq!(
            again.text("resourcePolicy"),
            reported(&policy),
            "case {case:?}: a second read must answer the same document"
        );

        // The delete removes it, and the read after it is the same 404 as before
        // any policy was ever put.
        let deleted = emulator.delete_table_bucket_policy(REGION, &arn).await;
        assert_eq!(
            deleted.status, 204,
            "case {case:?}: the delete must report success, got {status} {body}",
            status = deleted.status,
            body = deleted.body
        );
        let gone = emulator.get_table_bucket_policy(REGION, &arn).await;
        assert_refusal(
            &gone,
            404,
            "NotFoundException",
            NO_SUCH_POLICY,
            &format!("{case}, after the delete"),
        );
    }
}

#[tokio::test]
async fn a_put_replaces_the_policy_the_bucket_already_carries() {
    let emulator = Emulator::new();
    let arn = bucket(&emulator, "policy-replaced").await;

    // Each put leaves exactly ONE policy on the bucket: the document it carried.
    let mut previous: Option<String> = None;
    for (case, policy) in policies() {
        let put = emulator
            .put_table_bucket_policy(REGION, &arn, &policy)
            .await;
        assert_eq!(
            put.status, 200,
            "case {case:?}: the put must succeed, got {status} {body}",
            status = put.status,
            body = put.body
        );
        let read = emulator.get_table_bucket_policy(REGION, &arn).await;
        assert_eq!(
            read.text("resourcePolicy"),
            reported(&policy),
            "case {case:?}: the read must answer the policy the last put stored"
        );
        if let Some(previous) = &previous {
            assert_ne!(
                read.text("resourcePolicy"),
                *previous,
                "case {case:?}: the put must replace the policy the bucket carried"
            );
        }
        previous = Some(reported(&policy));
    }

    // The same put twice is the same one policy.
    let (case, policy) = policies().remove(0);
    for _ in 0..2 {
        let put = emulator
            .put_table_bucket_policy(REGION, &arn, &policy)
            .await;
        assert_eq!(
            put.status, 200,
            "case {case:?}: a repeated put must succeed, got {status} {body}",
            status = put.status,
            body = put.body
        );
    }
    let read = emulator.get_table_bucket_policy(REGION, &arn).await;
    assert_eq!(
        read.text("resourcePolicy"),
        reported(&policy),
        "case {case:?}: a repeated put must leave the one document"
    );

    // The LAYOUT a client submits does not change what it reads back: the same
    // policy written compactly and written as the service reports it are one
    // document, so both read back identically — and putting back what was read
    // changes nothing, which is what makes the reported layout stable.
    let compact = ::serde_json::to_string(&json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": "*",
            "Action": "s3tables:GetTable",
            "Resource": "*",
        }],
    }))
    .expect("the policy serializes");
    let put = emulator
        .put_table_bucket_policy(REGION, &arn, &compact)
        .await;
    assert_eq!(
        put.status, 200,
        "the compact put must succeed, got {status} {body}",
        status = put.status,
        body = put.body
    );
    let first = emulator
        .get_table_bucket_policy(REGION, &arn)
        .await
        .text("resourcePolicy");
    let put_back = emulator.put_table_bucket_policy(REGION, &arn, &first).await;
    assert_eq!(
        put_back.status, 200,
        "putting back what was read must succeed, got {status} {body}",
        status = put_back.status,
        body = put_back.body
    );
    let second = emulator
        .get_table_bucket_policy(REGION, &arn)
        .await
        .text("resourcePolicy");
    assert_eq!(
        second, first,
        "the reported layout must be stable: putting back what was read answers the same \
         document"
    );
}

#[tokio::test]
async fn a_document_that_is_not_a_resource_policy_is_refused_and_stores_nothing() {
    let emulator = Emulator::new();
    let arn = bucket(&emulator, "policy-grammar").await;

    let (_, accepted) = policies().remove(0);
    let put = emulator
        .put_table_bucket_policy(REGION, &arn, &accepted)
        .await;
    assert_eq!(
        put.status, 200,
        "the first put must succeed, got {status} {body}",
        status = put.status,
        body = put.body
    );

    for (case, document) in unacceptable_documents() {
        let refused = emulator
            .put_table_bucket_policy(REGION, &arn, &document)
            .await;
        assert_refusal(&refused, 400, "BadRequestException", INVALID_POLICY, case);

        // Nothing was stored: the bucket still carries the policy it had.
        let read = emulator.get_table_bucket_policy(REGION, &arn).await;
        assert_eq!(
            read.text("resourcePolicy"),
            reported(&accepted),
            "case {case:?}: a refused put must leave the stored policy as it was"
        );
    }
}

#[tokio::test]
async fn a_bucket_the_store_does_not_hold_is_reported_as_absent() {
    let emulator = Emulator::new();
    // An ARN of the modelled form that no create ever minted.
    let absent = expected_bucket_arn(REGION, "never-created-bucket");
    let (_, policy) = policies().remove(0);

    let read = emulator.get_table_bucket_policy(REGION, &absent).await;
    assert_refusal(
        &read,
        404,
        "NotFoundException",
        NO_SUCH_BUCKET,
        "reading the policy of a bucket that does not exist",
    );
    let put = emulator
        .put_table_bucket_policy(REGION, &absent, &policy)
        .await;
    assert_refusal(
        &put,
        404,
        "NotFoundException",
        NO_SUCH_BUCKET,
        "putting a policy on a bucket that does not exist",
    );
    let deleted = emulator.delete_table_bucket_policy(REGION, &absent).await;
    assert_refusal(
        &deleted,
        404,
        "NotFoundException",
        NO_SUCH_BUCKET,
        "deleting the policy of a bucket that does not exist",
    );
}
