// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the IAM AUTHORIZATION the table-replication
//! operations take.
//!
//! RULE: an operation of this service that documents a permission ("You must
//! have the s3tables:PutTableReplication permission to use this operation") and
//! models `AccessDeniedException` is decided against the CALLING PRINCIPAL's
//! identity-based IAM policies before this service reads or writes anything —
//! the principal being the identity the access key id the request presents
//! speaks for, which for an `sts:AssumeRole` credential is the session, whose
//! permissions are the role's. The decision is IAM's own: an explicit `Deny`
//! about the request wins over every `Allow`, an `Allow` with no `Deny` permits,
//! and a principal with neither is refused. The resource decided against is the
//! one the operation acts on, which for a table's replication configuration is
//! the TABLE, addressed by its own ARN. A refusal carries the modelled
//! `AccessDeniedException` (HTTP 403) naming the principal, the action
//! `s3tables:<OperationName>`, that resource and the rule that withheld the
//! grant; it reports nothing of the configuration, and a refused WRITE persists
//! nothing.
//!
//! The rows span what the rule generalises over: the operation (the write and
//! the read of one table's configuration), and whether the principal holds no
//! policy / a broad grant / a grant scoped to this table's ARN / a grant scoped
//! to ANOTHER table's ARN / a grant plus an explicit deny — beside the account's
//! own published credential, which holds no IAM policies and keeps being served.
//!
//! The principals are made with IAM's and STS's OWN operations over the same
//! engine ([`super::table_encryption_authorization`] owns that harness, shared
//! rather than copied), so the documents this service reads are the ones those
//! services wrote, and every call drives this service the way a client does.

use serde_json::{json, Value};

use super::harness::{encode, query_string, Reply};
use super::table_encryption_authorization::{assumed_role, statement, Emulator, ACCOUNT_KEY};

/// The modelled error code of an authorization refusal.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The IAM action each operation under test is performed under.
const PUT_ACTION: &str = "s3tables:PutTableReplication";
const GET_ACTION: &str = "s3tables:GetTableReplication";

/// The namespace the fixture tables live in.
const NAMESPACE: &str = "repl_auth_ns";

/// The fixed sentence this service states a table that has no replication
/// configuration with.
const NO_REPLICATION_CONFIGURATION: &str = "The specified replication configuration does not exist.";

/// A configuration whose one rule replicates to `destination`.
fn configuration(destination: &str) -> Value {
    json!({
        "role": format!(
            "arn:aws:iam::{account}:role/s3tables-replication",
            account = engine::emulator_account_id()
        ),
        "rules": [{ "destinations": [{ "destinationTableBucketARN": destination }] }],
    })
}

/// `PutTableReplication` on `table_arn`, as `access_key_id`.
async fn put_as(
    aws: &Emulator,
    access_key_id: &str,
    table_arn: &str,
    settings: &Value,
) -> Reply {
    aws.call_as(
        access_key_id,
        "PUT",
        &format!(
            "/table-replication{}",
            query_string(&[("tableArn", table_arn.to_string())])
        ),
        Some(&json!({ "configuration": settings })),
    )
    .await
}

/// `GetTableReplication` on `table_arn`, as `access_key_id`.
async fn get_as(aws: &Emulator, access_key_id: &str, table_arn: &str) -> Reply {
    aws.call_as(
        access_key_id,
        "GET",
        &format!(
            "/table-replication{}",
            query_string(&[("tableArn", table_arn.to_string())])
        ),
        None,
    )
    .await
}

/// A table bucket holding one namespace with the tables `tables` names,
/// answering `(bucket ARN, the ARN of each table in order)`.
async fn fixture(aws: &Emulator, bucket: &str, tables: &[&str]) -> (String, Vec<String>) {
    let created = aws
        .ok("PUT", "/buckets", Some(&json!({ "name": bucket })))
        .await;
    let arn = created.text("arn");
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

/// The sentence IAM refuses a call with, stated here independently of the
/// service under test: who was refused, which action, on which resource and
/// which of IAM's rules withheld the grant.
fn refusal_sentence(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} {reason}"
    )
}

/// The reason clause a principal that carries no grant for `action` is refused
/// with.
fn no_grant(action: &str) -> String {
    format!("because no identity-based policy allows the {action} action")
}

/// The reason clause an explicit `Deny` is refused with.
const EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// One row: the inline policies the calling role carries, and the refusal's
/// reason clause when the call must be refused.
struct Case {
    label: &'static str,
    policies: Vec<String>,
    refused_because: Option<String>,
}

/// The rows for `action` on `subject`, with `other` standing for a resource the
/// grant may name instead.
fn cases(action: &str, subject: &str, other: &str) -> Vec<Case> {
    vec![
        Case {
            label: "a role carrying no policy at all",
            policies: Vec::new(),
            refused_because: Some(no_grant(action)),
        },
        Case {
            label: "a role granted every action of this service on every resource",
            policies: vec![statement("Allow", "s3tables:*", "*")],
            refused_because: None,
        },
        Case {
            label: "a role granted this action on this table's ARN",
            policies: vec![statement("Allow", action, subject)],
            refused_because: None,
        },
        Case {
            label: "a role granted this action on ANOTHER table's ARN",
            policies: vec![statement("Allow", action, other)],
            refused_because: Some(no_grant(action)),
        },
        Case {
            label: "a role granted everything and denied this action",
            policies: vec![
                statement("Allow", "s3tables:*", "*"),
                statement("Deny", action, subject),
            ],
            refused_because: Some(EXPLICIT_DENY.to_string()),
        },
    ]
}

/// Assert `refused` is the modelled 403 worded with IAM's own sentence.
fn assert_refused(refused: &Reply, case: &str, sentence: &str) {
    assert_eq!(
        refused.status, 403,
        "case {case:?}: a refused call must answer 403, got {status} {body}",
        status = refused.status,
        body = refused.body
    );
    assert_eq!(
        refused.error_code.as_deref(),
        Some(ACCESS_DENIED),
        "case {case:?}: expected {ACCESS_DENIED}, got {actual:?}",
        actual = refused.error_code
    );
    assert_eq!(
        refused.message(),
        sentence,
        "case {case:?}: expected IAM's sentence, got {actual:?}",
        actual = refused.message()
    );
}

#[tokio::test]
async fn a_replication_write_is_decided_against_the_callers_identity_based_policies() {
    let aws = Emulator::new();
    let (_, tables) = fixture(&aws, "s3t-repl-auth-put", &["subject", "other"]).await;
    let subject = tables[0].clone();
    let other = tables[1].clone();
    let destination = aws
        .ok(
            "PUT",
            "/buckets",
            Some(&json!({ "name": "s3t-repl-auth-put-dst" })),
        )
        .await
        .text("arn");
    let settings = configuration(&destination);

    for (index, case) in cases(PUT_ACTION, &subject, &other).iter().enumerate() {
        let principal =
            assumed_role(&aws, &format!("s3t-repl-put-role-{index}"), &case.policies).await;
        let put = put_as(&aws, &principal.access_key_id, &subject, &settings).await;
        match &case.refused_because {
            None => {
                assert_eq!(
                    put.status, 200,
                    "case {label:?}: an allowed put must be served, got {status} {body}",
                    label = case.label,
                    status = put.status,
                    body = put.body
                );
                assert!(
                    !put.text("versionToken").is_empty(),
                    "case {label:?}: an accepted put mints a version token, got {body}",
                    label = case.label,
                    body = put.body
                );
            }
            Some(reason) => {
                assert_refused(
                    &put,
                    case.label,
                    &refusal_sentence(&principal.arn, PUT_ACTION, &subject, reason),
                );
                assert!(
                    put.body.get("versionToken").is_none(),
                    "case {label:?}: a refusal reports no token, got {body}",
                    label = case.label,
                    body = put.body
                );
            }
        }
    }

    // The account's own published credential holds no IAM policies — the
    // position root credentials are in — so its put keeps being served, and what
    // the allowed writes left is what the owner reads back.
    let served = put_as(&aws, ACCOUNT_KEY, &subject, &settings).await;
    assert_eq!(
        served.status, 200,
        "case \"the account's own credential\": the put must be served, got {status} {body}",
        status = served.status,
        body = served.body
    );
    let read = get_as(&aws, ACCOUNT_KEY, &subject).await;
    assert_eq!(
        read.status, 200,
        "case \"the account's own credential\": the read must be served, got {status} {body}",
        status = read.status,
        body = read.body
    );
    assert_eq!(
        read.text("versionToken"),
        served.text("versionToken"),
        "case \"the account's own credential\": the read reports the token of the last write"
    );
}

#[tokio::test]
async fn a_refused_write_persists_nothing() {
    let aws = Emulator::new();
    let (_, tables) = fixture(&aws, "s3t-repl-auth-refused", &["untouched"]).await;
    let subject = tables[0].clone();
    let destination = aws
        .ok(
            "PUT",
            "/buckets",
            Some(&json!({ "name": "s3t-repl-auth-refused-dst" })),
        )
        .await
        .text("arn");
    let principal = assumed_role(&aws, "s3t-repl-auth-nothing", &[]).await;

    let refused = put_as(
        &aws,
        &principal.access_key_id,
        &subject,
        &configuration(&destination),
    )
    .await;
    assert_refused(
        &refused,
        "a role carrying no policy at all",
        &refusal_sentence(&principal.arn, PUT_ACTION, &subject, &no_grant(PUT_ACTION)),
    );

    // The decision precedes the write, so the table still holds no
    // configuration: the owner's own read is the not-found of a table with none.
    let owner = get_as(&aws, ACCOUNT_KEY, &subject).await;
    assert_eq!(
        owner.status, 404,
        "case \"the owner's own read after a refused put\": expected 404, got {status} {body}",
        status = owner.status,
        body = owner.body
    );
    assert_eq!(
        owner.message(),
        NO_REPLICATION_CONFIGURATION,
        "case \"the owner's own read after a refused put\": expected the fixed sentence, got \
         {actual:?}",
        actual = owner.message()
    );
}

#[tokio::test]
async fn a_replication_read_is_decided_the_same_way() {
    let aws = Emulator::new();
    let (_, tables) = fixture(&aws, "s3t-repl-auth-get", &["subject", "other"]).await;
    let subject = tables[0].clone();
    let other = tables[1].clone();
    let destination = aws
        .ok(
            "PUT",
            "/buckets",
            Some(&json!({ "name": "s3t-repl-auth-get-dst" })),
        )
        .await
        .text("arn");
    // The configuration the read reports, written by the owner before any of the
    // rows below calls.
    let stored = put_as(&aws, ACCOUNT_KEY, &subject, &configuration(&destination)).await;
    assert_eq!(
        stored.status, 200,
        "the setup put must be served, got {status} {body}",
        status = stored.status,
        body = stored.body
    );

    for (index, case) in cases(GET_ACTION, &subject, &other).iter().enumerate() {
        let principal =
            assumed_role(&aws, &format!("s3t-repl-get-role-{index}"), &case.policies).await;
        let read = get_as(&aws, &principal.access_key_id, &subject).await;
        match &case.refused_because {
            None => {
                assert_eq!(
                    read.status, 200,
                    "case {label:?}: an allowed read must be served, got {status} {body}",
                    label = case.label,
                    status = read.status,
                    body = read.body
                );
                assert_eq!(
                    read.text("versionToken"),
                    stored.text("versionToken"),
                    "case {label:?}: an allowed read reports the stored configuration's token",
                    label = case.label
                );
            }
            Some(reason) => {
                assert_refused(
                    &read,
                    case.label,
                    &refusal_sentence(&principal.arn, GET_ACTION, &subject, reason),
                );
                assert!(
                    read.body.get("configuration").is_none(),
                    "case {label:?}: a refusal reports nothing of the configuration, got {body}",
                    label = case.label,
                    body = read.body
                );
            }
        }
    }
}
