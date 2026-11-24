// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the AUTHORIZATION decision every operation of
//! this service is served behind.
//!
//! RULE: every operation of this service documents the permission it takes ("You
//! must have the s3tables:GetTableBucketEncryption permission to use this
//! operation", `smithy_query(query="doc", shape="GetTableBucketEncryption")`), so
//! a signed call is decided — before the handler reads or writes anything —
//! against the policies in force on the CALLING PRINCIPAL (the identity the
//! access key id the request presents speaks for, which for an `sts:AssumeRole`
//! credential is the session, whose permissions are the role's) and on the
//! RESOURCE the request addresses (the table bucket policy, "a resource-based AWS
//! Identity and Access Management (IAM) policy"). The decision is IAM's own: an
//! explicit `Deny` in EITHER document wins over every `Allow`, within one account
//! a grant from either side is enough, a resource statement is about the request
//! only when its `Principal` names the caller, and a principal with no grant at
//! all is refused. The resource decided against is what the operation acts on —
//! the table when the request addresses one, else the table bucket. A refusal is
//! HTTP 403 `AccessDeniedException` naming the principal, the action
//! `s3tables:<OperationName>`, that resource and the rule that withheld the
//! grant, and it neither reports nor changes anything.
//!
//! The suites beside this one cover the rows one operation's policy sets span
//! (`table_bucket_encryption_authorization.rs` for a put,
//! `table_encryption_authorization.rs` for a table read). What this one adds is
//! the SURFACE and the two documents: that the same decision is taken for
//! operations across this service, addressed by a path label, by path
//! coordinates, by a query member and by a resource ARN, reporting and mutating
//! alike; and that the table bucket's own policy decides a call its caller's
//! identity policies allow.
//!
//! The principals are made with IAM's and STS's OWN operations over the same
//! engine, through the assembly `table_encryption_authorization.rs` states, so
//! the documents this service reads are the ones those services wrote.

use serde_json::{json, Value};

use super::harness::{encode, query_string};
use super::table_encryption_authorization::{
    assumed_role, statement, Emulator, Principal, ACCOUNT_KEY,
};

/// The error code an authorization refusal carries, in the shape this service's
/// model declares for one.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The namespace the fixture tables live in.
const NAMESPACE: &str = "authz_ns";

/// The sentence IAM refuses a call with, stated here independently of the
/// service under test: who was refused, which action, on which resource and
/// which of IAM's rules withheld the grant.
fn refusal_sentence(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    format!("User: {principal} is not authorized to perform: {action} on resource: {resource} {reason}")
}

/// IAM's clause for a request no readable identity-based document allows.
fn no_policy_allows(action: &str) -> String {
    format!("because no identity-based policy allows the {action} action")
}

/// One resource-based policy document carrying one statement, which — unlike an
/// identity-based one — names the principals it is about.
fn resource_policy(effect: &str, principal: &str, action: &str, resource: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Principal": principal,
            "Action": action,
            "Resource": resource,
        }],
    })
    .to_string()
}

/// A table bucket holding one namespace with the tables `tables` names,
/// answering `(bucket ARN, the ARN of each table in order)`.
async fn fixture(aws: &Emulator, bucket: &str, tables: &[&str]) -> (String, Vec<String>) {
    let arn = aws
        .ok("PUT", "/buckets", Some(&json!({ "name": bucket })))
        .await
        .text("arn");
    aws.ok(
        "PUT",
        &format!("/namespaces/{}", encode(&arn)),
        Some(&json!({ "namespace": [NAMESPACE] })),
    )
    .await;
    let mut arns = Vec::new();
    for table in tables {
        let made = aws
            .ok(
                "PUT",
                &format!("/tables/{}/{}", encode(&arn), encode(NAMESPACE)),
                Some(&json!({ "name": table, "format": "ICEBERG" })),
            )
            .await;
        arns.push(made.text("tableARN"));
    }
    (arn, arns)
}

/// The path `GetTableBucketEncryption` and `PutTableBucketEncryption` are
/// addressed at.
fn encryption_path(bucket_arn: &str) -> String {
    format!("/buckets/{}/encryption", encode(bucket_arn))
}

/// The path a table-coordinate operation is addressed at: the bucket's ARN, the
/// namespace and the table's name, then `suffix` when the operation has one.
fn table_path(bucket_arn: &str, table: &str, suffix: &str) -> String {
    format!(
        "/tables/{}/{}/{}{suffix}",
        encode(bucket_arn),
        encode(NAMESPACE),
        encode(table)
    )
}

#[tokio::test]
async fn a_bucket_encryption_read_is_decided_against_the_callers_identity_based_policies() {
    let action = "s3tables:GetTableBucketEncryption";
    let aws = Emulator::new();
    let subject = aws
        .ok("PUT", "/buckets", Some(&json!({ "name": "s3t-read-authz" })))
        .await
        .text("arn");
    let other = aws
        .ok(
            "PUT",
            "/buckets",
            Some(&json!({ "name": "s3t-read-authz-other" })),
        )
        .await
        .text("arn");

    // (case, the inline policies the role carries, the refusal's reason clause
    // when the call must be refused) — the rows span what the rule generalises
    // over: no grant, a broad grant, a grant scoped to this bucket, one scoped to
    // another bucket, one for another action of this service, and a grant plus an
    // explicit deny, where the deny and not a missing allow is what refuses.
    let rows: Vec<(&str, Vec<String>, Option<String>)> = vec![
        (
            "a role carrying no policy at all",
            Vec::new(),
            Some(no_policy_allows(action)),
        ),
        (
            "a role granted every action of this service on every resource",
            vec![statement("Allow", "s3tables:*", "*")],
            None,
        ),
        (
            "a role granted this action on this bucket's ARN",
            vec![statement("Allow", action, &subject)],
            None,
        ),
        (
            "a role granted this action on ANOTHER bucket's ARN",
            vec![statement("Allow", action, &other)],
            Some(no_policy_allows(action)),
        ),
        (
            "a role granted another action of this service on this bucket",
            vec![statement(
                "Allow",
                "s3tables:PutTableBucketEncryption",
                &subject,
            )],
            Some(no_policy_allows(action)),
        ),
        (
            "a role granted everything and denied this action",
            vec![
                statement("Allow", "s3tables:*", "*"),
                statement("Deny", action, &subject),
            ],
            Some("with an explicit deny in an identity-based policy".to_string()),
        ),
    ];

    for (index, (case, policies, reason)) in rows.iter().enumerate() {
        let principal = assumed_role(&aws, &format!("s3t-read-authz-{index}"), policies).await;
        let read = aws
            .call_as(
                &principal.access_key_id,
                "GET",
                &encryption_path(&subject),
                None,
            )
            .await;
        match reason {
            None => {
                assert_eq!(
                    read.status, 200,
                    "case {case:?}: an allowed read must be served, got {status} {body}",
                    status = read.status,
                    body = read.body
                );
                assert_eq!(
                    read.body
                        .get("encryptionConfiguration")
                        .and_then(|configuration| configuration.get("sseAlgorithm"))
                        .and_then(Value::as_str),
                    Some("AES256"),
                    "case {case:?}: an allowed read reports the configuration, got {body}",
                    body = read.body
                );
            }
            Some(reason) => {
                assert_eq!(
                    (read.status, read.error_code.as_deref()),
                    (403, Some(ACCESS_DENIED)),
                    "case {case:?}: expected 403 {ACCESS_DENIED}, got {status} {body}",
                    status = read.status,
                    body = read.body
                );
                assert_eq!(
                    read.message(),
                    refusal_sentence(&principal.arn, action, &subject, reason),
                    "case {case:?}: expected IAM's sentence, got {actual:?}",
                    actual = read.message()
                );
                assert!(
                    read.body.get("encryptionConfiguration").is_none(),
                    "case {case:?}: a refusal reports nothing of the bucket, got {body}",
                    body = read.body
                );
            }
        }
    }

    // The account's own published credential holds no IAM policies — the position
    // AWS account root credentials are in — so its read keeps being served.
    let served = aws
        .call_as(ACCOUNT_KEY, "GET", &encryption_path(&subject), None)
        .await;
    assert_eq!(
        served.status, 200,
        "case \"the account's own credential\": the read must be served, got {status} {body}",
        status = served.status,
        body = served.body
    );
}

#[tokio::test]
async fn every_operation_is_decided_for_the_resource_its_own_input_addresses() {
    let aws = Emulator::new();
    let (bucket, tables) = fixture(&aws, "s3t-surface-authz", &["subject", "spare"]).await;
    let subject = tables[0].clone();
    let spare = tables[1].clone();
    let principal = assumed_role(&aws, "s3t-surface-authz-none", &[]).await;

    // (case, method, path, the action the call is performed under, the ARN of the
    // resource it acts on, the status the owner's own identical call answers) —
    // the rows span the forms an input addresses a resource by: a table bucket as
    // a path label, a tagged resource's ARN as one, a table by its coordinates in
    // the path, a table by query members; and reporting beside mutating.
    let rows: Vec<(&str, &str, String, &str, &String, u16)> = vec![
        (
            "a bucket read addressed by a path label",
            "GET",
            format!("/buckets/{}", encode(&bucket)),
            "s3tables:GetTableBucket",
            &bucket,
            200,
        ),
        (
            "a listing of the bucket's namespaces",
            "GET",
            format!("/namespaces/{}", encode(&bucket)),
            "s3tables:ListNamespaces",
            &bucket,
            200,
        ),
        (
            "a listing of the bucket's tags, addressed by resourceArn",
            "GET",
            format!("/tag/{}", encode(&bucket)),
            "s3tables:ListTagsForResource",
            &bucket,
            200,
        ),
        (
            "a table read addressed by query members",
            "GET",
            format!(
                "/get-table{}",
                query_string(&[
                    ("tableBucketARN", bucket.clone()),
                    ("namespace", NAMESPACE.to_string()),
                    ("name", "subject".to_string()),
                ])
            ),
            "s3tables:GetTable",
            &subject,
            200,
        ),
        (
            "a table's maintenance configuration, addressed by path coordinates",
            "GET",
            table_path(&bucket, "subject", "/maintenance"),
            "s3tables:GetTableMaintenanceConfiguration",
            &subject,
            200,
        ),
        (
            "a table's metadata location",
            "GET",
            table_path(&bucket, "subject", "/metadata-location"),
            "s3tables:GetTableMetadataLocation",
            &subject,
            200,
        ),
        (
            "a mutating call: the delete of a table",
            "DELETE",
            table_path(&bucket, "spare", ""),
            "s3tables:DeleteTable",
            &spare,
            204,
        ),
    ];

    for (case, method, path, action, resource, owner_status) in &rows {
        let refused = aws
            .call_as(&principal.access_key_id, method, path, None)
            .await;
        assert_eq!(
            (refused.status, refused.error_code.as_deref()),
            (403, Some(ACCESS_DENIED)),
            "case {case:?}: expected 403 {ACCESS_DENIED}, got {status} {body}",
            status = refused.status,
            body = refused.body
        );
        assert_eq!(
            refused.message(),
            refusal_sentence(&principal.arn, action, resource, &no_policy_allows(action)),
            "case {case:?}: expected IAM's sentence, got {actual:?}",
            actual = refused.message()
        );
        // A refused call changed nothing and reported nothing: the owner's own
        // identical call still answers what the resource carries — which for the
        // delete above means the table it names is still there to be deleted.
        let served = aws.call_as(ACCOUNT_KEY, method, path, None).await;
        assert_eq!(
            served.status, *owner_status,
            "case {case:?}: the owner's own call must answer {owner_status}, got {status} {body}",
            status = served.status,
            body = served.body
        );
    }
}

#[tokio::test]
async fn an_explicit_deny_in_the_table_bucket_policy_refuses_a_call_the_identity_policies_allow() {
    let aws = Emulator::new();
    let (bucket, tables) = fixture(&aws, "s3t-resource-authz", &["subject", "probe"]).await;
    let subject = tables[0].clone();
    let probe = tables[1].clone();
    // The caller's own documents allow everything this service publishes, so a
    // refusal below can only come from the bucket's own policy.
    let principal = assumed_role(
        &aws,
        "s3t-resource-authz-allowed",
        &[statement("Allow", "s3tables:*", "*")],
    )
    .await;

    // (case, the denied action, the `Resource` the deny names, method, path, the
    // ARN the refusal names) — one deny scoped to the table's own ARN and one to
    // the wildcard the bucket's tables are published under, on a mutating and on
    // a reporting operation. Each row acts on a table of its own, so the call
    // served once the policy is gone cannot disturb the next row.
    let rows: Vec<(&str, &str, String, &str, String, &String)> = vec![
        (
            "a delete denied on the table's own ARN",
            "s3tables:DeleteTable",
            subject.clone(),
            "DELETE",
            table_path(&bucket, "subject", ""),
            &subject,
        ),
        (
            "a maintenance read denied on every table of the bucket",
            "s3tables:GetTableMaintenanceConfiguration",
            format!("{bucket}/table/*"),
            "GET",
            table_path(&bucket, "probe", "/maintenance"),
            &probe,
        ),
    ];

    for (case, action, denied_resource, method, path, resource) in &rows {
        aws.ok(
            "PUT",
            &format!("/buckets/{}/policy", encode(&bucket)),
            Some(&json!({
                "resourcePolicy": resource_policy("Deny", "*", action, denied_resource),
            })),
        )
        .await;

        let refused = aws
            .call_as(&principal.access_key_id, method, path, None)
            .await;
        assert_eq!(
            (refused.status, refused.error_code.as_deref()),
            (403, Some(ACCESS_DENIED)),
            "case {case:?}: expected 403 {ACCESS_DENIED}, got {status} {body}",
            status = refused.status,
            body = refused.body
        );
        assert_eq!(
            refused.message(),
            refusal_sentence(
                &principal.arn,
                action,
                resource,
                "with an explicit deny in a resource-based policy"
            ),
            "case {case:?}: expected IAM's sentence, got {actual:?}",
            actual = refused.message()
        );

        // Every OTHER action on that table stays allowed, and the table itself
        // survived the refused call.
        let read = aws
            .call_as(
                &principal.access_key_id,
                "GET",
                &format!(
                    "/get-table{}",
                    query_string(&[("tableArn", (*resource).clone())])
                ),
                None,
            )
            .await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: an action the policy does not deny stays allowed, got {status} {body}",
            status = read.status,
            body = read.body
        );

        // Deleting the bucket policy lifts the denial: the identical call is
        // served again, so the document and not some other state is what refused
        // it.
        aws.ok(
            "DELETE",
            &format!("/buckets/{}/policy", encode(&bucket)),
            None,
        )
        .await;
        let lifted = aws
            .call_as(&principal.access_key_id, method, path, None)
            .await;
        assert!(
            (200..300).contains(&lifted.status),
            "case {case:?}: the call must be served once the policy is gone, got {status} {body}",
            status = lifted.status,
            body = lifted.body
        );
    }
}

#[tokio::test]
async fn a_resource_policy_grant_names_the_principal_and_not_merely_its_account() {
    let action = "s3tables:GetTableBucketEncryption";
    let aws = Emulator::new();
    let bucket = aws
        .ok(
            "PUT",
            "/buckets",
            Some(&json!({ "name": "s3t-resource-grant" })),
        )
        .await
        .text("arn");
    // A principal carrying NO identity-based document, so only the bucket's own
    // policy can grant the read.
    let principal: Principal = assumed_role(&aws, "s3t-resource-grant-role", &[]).await;
    let role_arn = format!(
        "arn:aws:iam::{account}:role/s3t-resource-grant-role",
        account = engine::emulator_account_id()
    );

    // (case, the `Principal` the grant names, whether the read is served) — IAM
    // reads a resource statement that allows an ACCOUNT as delegating the
    // decision to that account's own identity-based policies, while one that
    // names the principal itself is a grant: within one account "access is
    // granted if either" document allows it.
    let rows: &[(&str, String, bool)] = &[
        ("the role behind the calling session", role_arn, true),
        (
            "the account the principal belongs to",
            format!(
                "arn:aws:iam::{account}:root",
                account = engine::emulator_account_id()
            ),
            false,
        ),
        ("every principal", "*".to_string(), true),
    ];

    for (case, named, served) in rows {
        aws.ok(
            "PUT",
            &format!("/buckets/{}/policy", encode(&bucket)),
            Some(&json!({
                "resourcePolicy": resource_policy("Allow", named, action, &bucket),
            })),
        )
        .await;
        let read = aws
            .call_as(
                &principal.access_key_id,
                "GET",
                &encryption_path(&bucket),
                None,
            )
            .await;
        if *served {
            assert_eq!(
                read.status, 200,
                "case {case:?}: a resource-based grant to the principal serves the read, got \
                 {status} {body}",
                status = read.status,
                body = read.body
            );
        } else {
            assert_eq!(
                (read.status, read.error_code.as_deref()),
                (403, Some(ACCESS_DENIED)),
                "case {case:?}: expected 403 {ACCESS_DENIED}, got {status} {body}",
                status = read.status,
                body = read.body
            );
            assert_eq!(
                read.message(),
                refusal_sentence(&principal.arn, action, &bucket, &no_policy_allows(action)),
                "case {case:?}: expected IAM's sentence, got {actual:?}",
                actual = read.message()
            );
        }
    }
}
