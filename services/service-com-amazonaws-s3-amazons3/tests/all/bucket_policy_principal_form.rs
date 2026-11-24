// SPDX-License-Identifier: CC0-1.0

//! The FORM a bucket policy's `Principal` values have to be written in.
//!
//! The rule under test: a resource policy grants to principals, and the policy LANGUAGE
//! publishes the form a principal is written in for each key of the element — an AWS account
//! or an IAM identity under `AWS` (the wildcard, an account id, or an ARN of an account root,
//! an IAM user or role, or a token-service session), a service principal's DNS name under
//! `Service`, a 64-character canonical user id under `CanonicalUser`, and a provider's ARN or
//! issuer under `Federated`; the untyped shorthand is defined for the wildcard alone. A value
//! outside its key's form, and a key the language defines no principal type under, name no
//! principal at all, so the statement grants to nobody: `PutBucketPolicy` refuses the whole
//! document — `MalformedPolicy`, HTTP 400, "Invalid principal in policy", the same refusal an
//! identity that does not EXIST is answered with, fixed text carrying nothing of the request
//! — and the bucket's own policy is left exactly as it was.
//!
//! What the rule generalises over, and what the rows below therefore differ along: WHICH key
//! carries the value, which way the form is wrong (a service name under the account key, a
//! resource ARN of a service that mints no identity, a truncated account id, a name that is
//! no DNS name, a canonical id of the wrong length, a key naming no kind at all), whether
//! the element is written as a bare value, a list or a map, and whether it is `Principal` or
//! `NotPrincipal`.
//!
//! Every assertion drives a real HTTP request through the router (the harness the
//! bucket-lifecycle suite in this crate owns), so what is pinned is the wire behaviour.
//!
//! The FORM is all this suite is about. Whether an identity of an admitted form EXISTS is a
//! question for the stores that hold the identities, answered in the same sentence by
//! `crate::bucket_policy_principal_lifecycle` over the harness that holds them — so the
//! legacy origin access identity, whose form names an IAM user of CloudFront's own account,
//! is covered by the form reading's own unit test rather than by a row here.

use service_com_amazonaws_s3_amazons3::state::MALFORMED_POLICY_CODE;
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{create_bucket, get_bucket_policy, put_bucket_policy, router};
use super::bucket_public_access_block::allow_public_access;

/// How S3 words the `MalformedPolicy` of a document whose `Principal` names no principal —
/// fixed text with no variable part.
const INVALID_PRINCIPAL: &str = "Invalid principal in policy";

/// The bucket every row is put on.
const BUCKET: &str = "principal-form-bucket";

/// A document the bucket takes, which every refused row is asserted to have left in place.
fn baseline() -> String {
    document(serde_json::json!("*"), Some("GrantToEveryone"))
}

/// A one-statement document granting the objects' read to `principal`, with `sid` when one is
/// given — the shape every row varies the `Principal` element of.
fn document(principal: serde_json::Value, sid: Option<&str>) -> String {
    let mut statement = serde_json::Map::new();
    if let Some(sid) = sid {
        statement.insert("Sid".to_string(), serde_json::json!(sid));
    }
    statement.insert("Effect".to_string(), serde_json::json!("Allow"));
    statement.insert("Principal".to_string(), principal);
    statement.insert("Action".to_string(), serde_json::json!("s3:GetObject"));
    statement.insert(
        "Resource".to_string(),
        serde_json::json!(format!("arn:aws:s3:::{BUCKET}/*")),
    );
    serde_json::json!({"Version": "2012-10-17", "Statement": [statement]}).to_string()
}

/// Stand the bucket up with the baseline policy stored on it.
async fn bucket_with_baseline() -> MultiServiceRouter {
    let router = router();
    let created = create_bucket(&router, BUCKET, None, None).await;
    assert_eq!(
        created.status, 200,
        "the bucket must be created first, got {} with body {:?}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    // Some rows grant to the wildcard principal, which Block Public Access would deny on its
    // own; this suite is about the principal's FORM, so the guardrail is lifted first.
    allow_public_access(&router, BUCKET).await;
    let stored = put_bucket_policy(&router, BUCKET, &baseline()).await;
    assert_eq!(
        stored.status, 204,
        "the baseline document must be stored, got {} with body {:?}",
        stored.status,
        String::from_utf8_lossy(&stored.body)
    );
    router
}

#[tokio::test]
async fn a_principal_value_of_no_published_form_refuses_the_document() {
    let router = bucket_with_baseline().await;
    let rows: Vec<(&str, String)> = vec![
        (
            // The recorded shape: the service principal a distribution is granted through,
            // written under the key that names ACCOUNTS and IAM identities.
            "a service principal under the account key",
            document(
                serde_json::json!({"AWS": "cloudfront.amazonaws.com"}),
                Some("AllowCloudFrontServicePrincipal"),
            ),
        ),
        (
            // An ARN of a service that mints no identities names a resource, not a principal.
            "a resource ARN of a service that mints no identity",
            document(
                serde_json::json!({
                    "AWS": "arn:aws:cloudfront::111122223333:origin-access-control/E123",
                }),
                Some("ThroughTheAccessControl"),
            ),
        ),
        (
            "a bare value that is neither the wildcard nor an identity",
            document(serde_json::json!("everyone"), Some("Everyone")),
        ),
        (
            "an account id of too few digits",
            document(
                serde_json::json!({"AWS": "1111222233"}),
                Some("ShortAccount"),
            ),
        ),
        (
            "a service principal that is no DNS name",
            document(
                serde_json::json!({"Service": "CloudFront"}),
                Some("ByServiceName"),
            ),
        ),
        (
            "a canonical user id of the wrong length",
            document(
                serde_json::json!({"CanonicalUser": "the-owner"}),
                Some("ByCanonicalUser"),
            ),
        ),
        (
            // A key the language defines no principal type under names no principal whatever
            // value stands under it.
            "a key naming no principal kind",
            document(
                serde_json::json!({"Services": "cloudfront.amazonaws.com"}),
                Some("MisspelledKey"),
            ),
        ),
        (
            // One value of a list, the others well formed.
            "one value of a list of accounts",
            document(
                serde_json::json!({"AWS": ["111122223333", "arn:aws:iam::111122223333:root", "nobody"]}),
                Some("ThreeAccounts"),
            ),
        ),
        (
            // The same reading applies to the element that names who a grant does NOT reach.
            "a NotPrincipal value of no form",
            serde_json::json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Sid": "EveryoneElse",
                    "Effect": "Allow",
                    "NotPrincipal": {"AWS": "cloudfront.amazonaws.com"},
                    "Action": "s3:GetObject",
                    "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
                }],
            })
            .to_string(),
        ),
        (
            // The untyped shorthand the language defines for the wildcard alone: a list is
            // neither that shorthand nor a typed map.
            "the wildcard written as a list",
            document(serde_json::json!(["*"]), Some("EveryoneListed")),
        ),
        (
            "a statement stating no Sid",
            document(serde_json::json!({"AWS": "nobody-at-all"}), None),
        ),
    ];
    for (case, document) in rows {
        let answer = put_bucket_policy(&router, BUCKET, &document).await;
        let body = String::from_utf8_lossy(&answer.body).to_string();
        assert_eq!(
            (answer.status, answer.error_code(), answer.error_message()),
            (
                400,
                MALFORMED_POLICY_CODE.to_string(),
                INVALID_PRINCIPAL.to_string()
            ),
            "case {case:?}: expected HTTP 400 {MALFORMED_POLICY_CODE} {INVALID_PRINCIPAL:?}, \
             got {} with body {body:?}",
            answer.status
        );
        // Nothing the refused document carried reached the store.
        let read = get_bucket_policy(&router, BUCKET).await;
        assert_eq!(
            String::from_utf8_lossy(&read.body),
            baseline(),
            "case {case:?}: a refused put must leave the stored policy as it was, got {} with \
             body {:?}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
    }
}

#[tokio::test]
async fn a_principal_of_a_published_form_is_stored() {
    let router = bucket_with_baseline().await;
    let canonical_user = "9a8b7c6d5e4f30291a8b7c6d5e4f30291a8b7c6d5e4f30291a8b7c6d5e4f3029";
    let rows: Vec<(&str, String)> = vec![
        (
            "the bare wildcard",
            document(serde_json::json!("*"), Some("Everyone")),
        ),
        (
            "the wildcard under the account key",
            document(serde_json::json!({"AWS": "*"}), Some("EveryAccount")),
        ),
        (
            "an account id",
            document(
                serde_json::json!({"AWS": "111122223333"}),
                Some("OneAccount"),
            ),
        ),
        (
            "an account root",
            document(
                serde_json::json!({"AWS": "arn:aws:iam::111122223333:root"}),
                Some("OneRoot"),
            ),
        ),
        (
            "a user and a role of another account",
            document(
                serde_json::json!({"AWS": [
                    "arn:aws:iam::111122223333:user/reader",
                    "arn:aws:iam::111122223333:role/service-role/replicator",
                ]}),
                Some("UserAndRole"),
            ),
        ),
        (
            "a session the token service vended",
            document(
                serde_json::json!({
                    "AWS": "arn:aws:sts::111122223333:assumed-role/replicator/session",
                }),
                Some("OneSession"),
            ),
        ),
        (
            "service principals, one of them a sub-service",
            document(
                serde_json::json!({"Service": [
                    "cloudfront.amazonaws.com",
                    "delivery.logs.amazonaws.com",
                ]}),
                Some("TwoServices"),
            ),
        ),
        (
            "a canonical user id",
            document(
                serde_json::json!({"CanonicalUser": canonical_user}),
                Some("OneCanonicalUser"),
            ),
        ),
        (
            "a federated issuer and a provider ARN",
            document(
                serde_json::json!({"Federated": [
                    "accounts.google.com",
                    "arn:aws:iam::111122223333:saml-provider/Example",
                ]}),
                Some("TwoProviders"),
            ),
        ),
        (
            "an account and a service named together",
            document(
                serde_json::json!({
                    "AWS": "arn:aws:iam::111122223333:root",
                    "Service": "logs.amazonaws.com",
                }),
                Some("AccountAndService"),
            ),
        ),
    ];
    for (case, document) in rows {
        let answer = put_bucket_policy(&router, BUCKET, &document).await;
        assert_eq!(
            answer.status, 204,
            "case {case:?}: expected HTTP 204, got {} with body {:?}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        // Stored, not merely accepted: the read reports the document byte for byte.
        let read = get_bucket_policy(&router, BUCKET).await;
        assert_eq!(
            String::from_utf8_lossy(&read.body),
            document.as_str(),
            "case {case:?}: the stored policy must read back byte for byte, got {} with body \
             {:?}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
    }
}
