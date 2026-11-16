// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the IAM AUTHORIZATION `PutTableBucketEncryption`
//! takes.
//!
//! RULE: an operation of this service that documents a permission ("You must
//! have the s3tables:PutTableBucketEncryption permission to use this operation",
//! `smithy_query(query="doc", shape="PutTableBucketEncryption")`) is decided
//! against the CALLING PRINCIPAL's identity-based IAM policies before this
//! service reads or writes anything — the principal being the identity the
//! access key id the request presents speaks for, and for an `sts:AssumeRole`
//! credential that is the session, whose permissions are the role's. The
//! decision is IAM's own: an explicit `Deny` about the request wins over every
//! `Allow`, an `Allow` with no `Deny` permits, and a principal with neither is
//! refused. The resource decided against is what the operation acts on, which
//! for a table bucket's encryption is the TABLE BUCKET, by the ARN the request
//! addressed. A refusal carries `AccessDeniedException` (HTTP 403) naming the
//! principal, the action `s3tables:<OperationName>`, that resource and the rule
//! that withheld the grant — and it changes nothing, so the bucket's owner still
//! reads the configuration the bucket carried.
//!
//! The rows span what the rule generalises over: whether the principal holds no
//! policy / a broad grant / a grant scoped to this bucket's ARN / a grant scoped
//! to ANOTHER bucket's ARN / a grant for another action of this service / a
//! grant plus an explicit deny — and, beside the decision, that the account's
//! own published credential keeps being served and that a refusal precedes the
//! lookup, so a caller holding no grant is refused for a bucket the account does
//! not even hold.
//!
//! The principals are made with IAM's and STS's OWN operations over the same
//! engine, through the assembly `table_encryption_authorization.rs` states for
//! the sibling read, so the documents this service reads are the ones those
//! services wrote.

use serde_json::{json, Value};

use super::harness::encode;
use super::table_encryption_authorization::{
    assumed_role, statement, Emulator, ACCOUNT_KEY, REGION,
};

/// The IAM action the operation under test is performed under.
const ACTION: &str = "s3tables:PutTableBucketEncryption";

/// The error code an identity-based refusal carries.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The configuration every call here puts: S3-managed keys, which name no KMS
/// key, so what the rows differ along is WHO is calling and nothing else.
fn configuration() -> Value {
    json!({ "encryptionConfiguration": { "sseAlgorithm": "AES256" } })
}

/// The path `PutTableBucketEncryption` is addressed at.
fn path(bucket_arn: &str) -> String {
    format!("/buckets/{}/encryption", encode(bucket_arn))
}

/// The sentence IAM refuses a call with, stated here independently of the
/// service under test: who was refused, which action, on which resource and
/// which of IAM's rules withheld the grant.
fn refusal_sentence(principal: &str, resource: &str, reason: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {ACTION} on resource: {resource} \
         {reason}"
    )
}

/// The `sseAlgorithm` the owner's own read reports for `bucket_arn`.
async fn owner_reads(aws: &Emulator, bucket_arn: &str, case: &str) -> String {
    let read = aws
        .call_as(ACCOUNT_KEY, "GET", &path(bucket_arn), None)
        .await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: the owner's own read must be served, got {status} {body}",
        status = read.status,
        body = read.body
    );
    read.body
        .get("encryptionConfiguration")
        .and_then(|configuration| configuration.get("sseAlgorithm"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: the read carries no algorithm: {body}", body = read.body))
        .to_string()
}

#[tokio::test]
async fn a_put_is_decided_against_the_callers_identity_based_policies() {
    let aws = Emulator::new();
    let subject = aws
        .ok("PUT", "/buckets", Some(&json!({ "name": "s3t-put-auth" })))
        .await
        .text("arn");
    let other = aws
        .ok(
            "PUT",
            "/buckets",
            Some(&json!({ "name": "s3t-put-auth-other" })),
        )
        .await
        .text("arn");

    // (case, the inline policies the role carries, the refusal's reason clause
    // when the call must be refused)
    let rows: Vec<(&str, Vec<String>, Option<String>)> = vec![
        (
            "a role carrying no policy at all",
            Vec::new(),
            Some(format!(
                "because no identity-based policy allows the {ACTION} action"
            )),
        ),
        (
            "a role granted every action of this service on every resource",
            vec![statement("Allow", "s3tables:*", "*")],
            None,
        ),
        (
            "a role granted this action on this bucket's ARN",
            vec![statement("Allow", ACTION, &subject)],
            None,
        ),
        (
            "a role granted this action on ANOTHER bucket's ARN",
            vec![statement("Allow", ACTION, &other)],
            Some(format!(
                "because no identity-based policy allows the {ACTION} action"
            )),
        ),
        (
            "a role granted another action of this service on this bucket",
            vec![statement("Allow", "s3tables:GetTableBucketEncryption", &subject)],
            Some(format!(
                "because no identity-based policy allows the {ACTION} action"
            )),
        ),
        (
            "a role granted everything and denied this action",
            vec![
                statement("Allow", "s3tables:*", "*"),
                statement("Deny", ACTION, &subject),
            ],
            Some("with an explicit deny in an identity-based policy".to_string()),
        ),
    ];

    for (index, (case, policies, reason)) in rows.iter().enumerate() {
        let principal = assumed_role(&aws, &format!("s3t-put-auth-role-{index}"), policies).await;
        let put = aws
            .call_as(
                &principal.access_key_id,
                "PUT",
                &path(&subject),
                Some(&configuration()),
            )
            .await;
        match reason {
            None => {
                assert_eq!(
                    put.status, 200,
                    "case {case:?}: an allowed put must be served, got {status} {body}",
                    status = put.status,
                    body = put.body
                );
                assert_eq!(
                    owner_reads(&aws, &subject, case).await,
                    "AES256",
                    "case {case:?}: the served put must be the configuration the owner reads"
                );
            }
            Some(reason) => {
                assert_eq!(
                    put.status, 403,
                    "case {case:?}: a refused put must answer 403, got {status} {body}",
                    status = put.status,
                    body = put.body
                );
                assert_eq!(
                    put.error_code.as_deref(),
                    Some(ACCESS_DENIED),
                    "case {case:?}: expected {ACCESS_DENIED}, got {actual:?}",
                    actual = put.error_code
                );
                assert_eq!(
                    put.message(),
                    refusal_sentence(&principal.arn, &subject, reason),
                    "case {case:?}: expected IAM's sentence, got {actual:?}",
                    actual = put.message()
                );
            }
        }
    }

    // The account's own published credential holds no IAM policies — the
    // position root credentials are in — so its put keeps being served.
    let served = aws
        .call_as(ACCOUNT_KEY, "PUT", &path(&subject), Some(&configuration()))
        .await;
    assert_eq!(
        served.status, 200,
        "case \"the account's own credential\": the put must be served, got {status} {body}",
        status = served.status,
        body = served.body
    );
    assert_eq!(
        owner_reads(&aws, &subject, "the account's own credential").await,
        "AES256",
        "case \"the account's own credential\": the owner reads what it put"
    );
}

#[tokio::test]
async fn a_refusal_precedes_the_lookup_and_changes_nothing() {
    let aws = Emulator::new();
    let resident = aws
        .ok(
            "PUT",
            "/buckets",
            Some(&json!({ "name": "s3t-put-auth-absent" })),
        )
        .await
        .text("arn");
    let principal = assumed_role(&aws, "s3t-put-auth-nothing", &[]).await;

    // An ARN the account holds no bucket at, in the form the model's
    // `TableBucketARN` pattern requires.
    let absent = format!("arn:aws:s3tables:{REGION}:000000000000:bucket/s3t-put-auth-never-made");

    // A caller holding no grant is refused for a bucket the account does not
    // hold, with the same class and sentence as for one it does: the decision is
    // taken before the lookup, so the refusal discloses no existence.
    for (case, arn) in [
        ("a bucket that does not exist", absent.as_str()),
        ("a bucket that does exist", resident.as_str()),
    ] {
        let put = aws
            .call_as(
                &principal.access_key_id,
                "PUT",
                &path(arn),
                Some(&configuration()),
            )
            .await;
        assert_eq!(
            put.status, 403,
            "case {case:?}: expected 403, got {status} {body}",
            status = put.status,
            body = put.body
        );
        assert_eq!(
            put.error_code.as_deref(),
            Some(ACCESS_DENIED),
            "case {case:?}: expected {ACCESS_DENIED}, got {actual:?}",
            actual = put.error_code
        );
        assert_eq!(
            put.message(),
            refusal_sentence(
                &principal.arn,
                arn,
                &format!("because no identity-based policy allows the {ACTION} action")
            ),
            "case {case:?}: expected IAM's sentence, got {actual:?}",
            actual = put.message()
        );
    }

    // And the refused puts left the account exactly as it was: the resident
    // bucket still reports the service default, since no put was ever served on
    // it.
    assert_eq!(
        owner_reads(&aws, &resident, "the owner's own read").await,
        "AES256",
        "case \"the owner's own read\": a refused put must change nothing"
    );

    // Reported, and held: a refused put wrote no configuration of the bucket's
    // own, which is what the owner's own delete of that configuration answers
    // — the bucket has none to remove.
    let delete = aws
        .call_as(ACCOUNT_KEY, "DELETE", &path(&resident), None)
        .await;
    assert_eq!(
        (delete.status, delete.error_code.as_deref()),
        (404, Some("NotFoundException")),
        "case \"the owner's own delete\": a refused put must leave no configuration, got \
         {status} {body}",
        status = delete.status,
        body = delete.body
    );
}
