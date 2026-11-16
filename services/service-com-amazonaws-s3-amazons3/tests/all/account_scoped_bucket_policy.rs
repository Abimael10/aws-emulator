// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a bucket policy scoped to an ACCOUNT: whether a grant to the
//! wildcard principal that a `Condition` confines to named accounts is public.
//!
//! Every assertion drives a real HTTP request through the router (the harness the
//! bucket-lifecycle suite in this crate owns), so what is pinned is the wire behaviour —
//! status, body, error code — rather than any handler's internals.
//!
//! The rule under test, with its source:
//!
//! * A statement that allows the wildcard principal is public "unless it contains a
//!   `Condition` that restricts access to a set of fixed values" for one of the condition
//!   keys Amazon S3 lists under "The Meaning of 'Public'" (Amazon S3 User Guide, Blocking
//!   public access) — the identity of the calling principal (`aws:PrincipalAccount`,
//!   `aws:PrincipalArn`, the organization keys), the calling source (`aws:SourceAccount`,
//!   `aws:SourceArn`, the VPC keys), or the key the write is encrypted under. A condition
//!   whose operand names every value the key could take (the bare wildcard) restricts
//!   nothing, so the grant stays public.
//! * `BlockPublicPolicy` makes S3 "reject calls to PUT Bucket policy if the specified bucket
//!   policy allows public access" (`PublicAccessBlockConfiguration$BlockPublicPolicy`), so a
//!   policy the classifier deems NOT public is accepted (204) even with the switch on — and
//!   is then readable, both as the document and as its status.
//! * `PolicyStatus$IsPublic` is "the policy status for this bucket. TRUE indicates that this
//!   bucket is public" (`smithy_query(query="shape",shape="PolicyStatus")`): a stored,
//!   non-public policy reads back 200 with `IsPublic` false. The 404 `NoSuchBucketPolicy` is
//!   the answer for a bucket carrying no policy at all, which is what a bucket whose write
//!   was refused still is.
//!
//! The account-scoping condition keys are spelled out in the rows below rather than imported,
//! so the service and this suite cannot agree on a wrong catalog.

use super::bucket_lifecycle::{create_bucket, get_bucket_policy, put_bucket_policy, router, send};
use super::bucket_public_access_block::put_public_access_block;
use smithy_server_runtime::MultiServiceRouter;

/// The four Block Public Access switches, all on — the posture a bucket is created with, set
/// explicitly here so each row states the configuration it is judged under.
const ALL_SWITCHES_ON: [(&str, bool); 4] = [
    ("BlockPublicAcls", true),
    ("IgnorePublicAcls", true),
    ("BlockPublicPolicy", true),
    ("RestrictPublicBuckets", true),
];

/// `GET /<bucket>?policyStatus` — `GetBucketPolicyStatus`.
async fn get_bucket_policy_status(
    router: &MultiServiceRouter,
    bucket: &str,
) -> super::bucket_lifecycle::Answer {
    send(router, "GET", &format!("/{bucket}?policyStatus"), &[], "").await
}

/// The code and status S3 answers a call its Block Public Access configuration denies with.
const DENIED_CODE: &str = "AccessDenied";
const DENIED_STATUS: u16 = 403;

/// The code and status a policy-status read answers for a bucket carrying no policy.
const NO_POLICY_CODE: &str = "NoSuchBucketPolicy";
const NO_POLICY_STATUS: u16 = 404;

/// A document whose single `Allow` reaches the wildcard principal and carries `condition` as
/// its whole `Condition` element (`None` for a statement carrying none at all).
fn wildcard_policy(bucket: &str, condition: Option<serde_json::Value>) -> String {
    let mut statement = serde_json::json!({
        "Effect": "Allow",
        "Principal": "*",
        "Action": "s3:GetObject",
        "Resource": format!("arn:aws:s3:::{bucket}/*"),
    });
    if let Some(condition) = condition {
        statement["Condition"] = condition;
    }
    serde_json::json!({"Version": "2012-10-17", "Statement": [statement]}).to_string()
}

/// The rows: the case name, the statement's `Condition`, and whether the document is public.
///
/// They differ along the condition KEY (the calling account, its ARN, the organization, the
/// source account), the SPELLING of that key (IAM condition keys are case-insensitive), the
/// OPERAND's form (one value, a list of them) and WHOSE account it names (this emulator's own
/// and a foreign one, since scoping to any fixed account is scoping) — plus the operands that
/// name every value the key could take, which restrict nothing.
fn rows() -> Vec<(String, Option<serde_json::Value>, bool)> {
    let own = engine::emulator_account_id().to_string();
    vec![
        (
            "the calling account, this emulator's own".to_string(),
            Some(serde_json::json!({"StringEquals": {"aws:PrincipalAccount": own}})),
            false,
        ),
        (
            "the calling account, a foreign one".to_string(),
            Some(serde_json::json!({"StringEquals": {"aws:PrincipalAccount": "111122223333"}})),
            false,
        ),
        (
            "two calling accounts, both fixed".to_string(),
            Some(serde_json::json!({
                "StringEquals": {"aws:PrincipalAccount": ["111122223333", "999988887777"]}
            })),
            false,
        ),
        (
            "the calling account, the key spelled lower case".to_string(),
            Some(serde_json::json!({"StringEquals": {"aws:principalaccount": "111122223333"}})),
            false,
        ),
        (
            "the calling principal's ARN".to_string(),
            Some(serde_json::json!({
                "ArnEquals": {"aws:PrincipalArn": "arn:aws:iam::111122223333:role/Reader"}
            })),
            false,
        ),
        (
            "the calling account's organization".to_string(),
            Some(serde_json::json!({"StringEquals": {"aws:PrincipalOrgID": "o-exampleorgid"}})),
            false,
        ),
        (
            "the source account".to_string(),
            Some(serde_json::json!({"StringEquals": {"aws:SourceAccount": "111122223333"}})),
            false,
        ),
        (
            "a calling account named by the bare wildcard, which fixes nothing".to_string(),
            Some(serde_json::json!({"StringLike": {"aws:PrincipalAccount": "*"}})),
            true,
        ),
        (
            "a calling-account list carrying the bare wildcard".to_string(),
            Some(serde_json::json!({
                "StringLike": {"aws:PrincipalAccount": ["111122223333", "*"]}
            })),
            true,
        ),
        (
            "a condition on a key outside the published catalog".to_string(),
            Some(serde_json::json!({"StringEquals": {"s3:prefix": "public/"}})),
            true,
        ),
        ("no condition at all".to_string(), None, true),
    ]
}

#[tokio::test]
async fn an_account_scoped_policy_is_accepted_under_block_public_policy_and_is_not_public() {
    let router = router();
    for (index, (case, condition, public)) in rows().into_iter().enumerate() {
        // One bucket per row, so what each row asserts is about its own document alone.
        let bucket = format!("account-scoped-policy-{index}");
        let created = create_bucket(&router, &bucket, None, None).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the bucket must be created first, got {} with body {:?}",
            created.status,
            String::from_utf8_lossy(&created.body)
        );
        let blocked = put_public_access_block(&router, &bucket, &ALL_SWITCHES_ON, &[]).await;
        assert_eq!(
            blocked.status, 200,
            "case {case:?}: BlockPublicPolicy must be on for this row, got {} with body {:?}",
            blocked.status,
            String::from_utf8_lossy(&blocked.body)
        );

        let document = wildcard_policy(&bucket, condition);
        let put = put_bucket_policy(&router, &bucket, &document).await;
        if public {
            // The switch rejects exactly the documents the classifier deems public, and the
            // bucket is left carrying no policy — which is what its status read then reports.
            assert_eq!(
                put.status, DENIED_STATUS,
                "case {case:?}: expected the public document to be refused with HTTP \
                 {DENIED_STATUS}, got {} with body {:?}",
                put.status,
                String::from_utf8_lossy(&put.body)
            );
            assert_eq!(
                put.error_code(),
                DENIED_CODE,
                "case {case:?}: expected error code {DENIED_CODE}, got {:?}",
                put.error_code()
            );
            let status = get_bucket_policy_status(&router, &bucket).await;
            assert_eq!(
                status.status, NO_POLICY_STATUS,
                "case {case:?}: a bucket carrying no policy has no status to report, got {} \
                 with body {:?}",
                status.status,
                String::from_utf8_lossy(&status.body)
            );
            assert_eq!(
                status.error_code(),
                NO_POLICY_CODE,
                "case {case:?}: expected error code {NO_POLICY_CODE}, got {:?}",
                status.error_code()
            );
            continue;
        }

        assert_eq!(
            put.status, 204,
            "case {case:?}: expected the non-public document to be accepted with HTTP 204, \
             got {} with body {:?}",
            put.status,
            String::from_utf8_lossy(&put.body)
        );
        // Read back through the document reader: what was stored is the document sent.
        let read = get_bucket_policy(&router, &bucket).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected the stored document to read back, got {} with body {:?}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        assert_eq!(
            String::from_utf8_lossy(&read.body),
            document.as_str(),
            "case {case:?}: expected the document byte for byte"
        );
        // And through the status reader: 200 with the classifier's verdict.
        let status = get_bucket_policy_status(&router, &bucket).await;
        assert_eq!(
            status.status, 200,
            "case {case:?}: expected HTTP 200 from the status read, got {} with body {:?}",
            status.status,
            String::from_utf8_lossy(&status.body)
        );
        assert_eq!(
            status.element("IsPublic").as_deref(),
            Some("false"),
            "case {case:?}: expected IsPublic false, got body {:?}",
            String::from_utf8_lossy(&status.body)
        );
    }
}
