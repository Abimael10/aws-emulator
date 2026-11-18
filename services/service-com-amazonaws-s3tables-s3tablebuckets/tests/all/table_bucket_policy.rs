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
//! stored policy left as it was and with the one fixed malformed-policy sentence
//! whichever clause of the policy grammar the document broke.
//!
//! The rows span what the rule generalises over: the shape of the document
//! (one statement, several statements, a `Deny`, a document with an `Id`, each
//! policy version the language admits), the LAYOUT it is submitted in (compact
//! and already-rendered, which must read back identically), the sub-resource's
//! state (never put, put, deleted), and whether the bucket exists at all.
//!
//! The second rule, from [`an_explicit_deny_in_the_bucket_policy_refuses_the_calls_it_names`]
//! on: an attached policy DECIDES calls rather than only being stored. A
//! statement of it that explicitly denies the requested action on the resource
//! the request addresses, for the calling principal, refuses that request with
//! the error code S3 Tables answers an unauthorized call with
//! (`AccessDeniedException`, HTTP 403) and IAM's own sentence, and the call has
//! no effect; a statement that is not about the request — another action,
//! another resource, another principal, or an `Allow` — refuses nothing. Those
//! rows span each element the decision is read from, each class of call (a
//! bucket-level read, a table-level read, a listing, a call addressed by a
//! resource ARN and a write), and the policy's own presence.

use serde_json::{json, Value};

use crate::harness::{assert_served, expected_bucket_arn, Emulator, Reply, ACCOUNT};

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
/// this suite pins the wording itself. The malformed-policy sentence is the
/// service's refusal for EVERY clause of the policy grammar a document breaks:
/// it states the class and says nothing about which clause, and it carries no
/// closing full stop.
const NO_SUCH_POLICY: &str = "The specified bucket policy does not exist.";
const NO_SUCH_BUCKET: &str = "The specified bucket does not exist.";
const INVALID_POLICY: &str = "The specified policy is malformed";

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
        (
            "a statement that is not an object",
            json!({
                "Version": "2012-10-17",
                "Statement": ["s3tables:GetTable"],
            })
            .to_string(),
        ),
        (
            "a statement naming no action",
            json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": "*",
                    "Resource": "*",
                }],
            })
            .to_string(),
        ),
        (
            "a document holding no statement at all",
            json!({ "Version": "2012-10-17", "Statement": [] }).to_string(),
        ),
        ("JSON that is a scalar", json!(17).to_string()),
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

// ---------------------------------------------------------------------------
// What an attached policy DECIDES.
// ---------------------------------------------------------------------------

/// The error code S3 Tables refuses a call no policy in force permits with.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The namespace and table name every case below works in.
const NAMESPACE: &str = "ledger";
const TABLE: &str = "orders";

/// A resource-based policy whose one statement DENIES `action` on `resource` for
/// `principal`.
///
/// The three elements are exactly what decides whether a statement is ABOUT a
/// request, so each case varies one of them.
fn denying(principal: Value, action: &str, resource: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Deny",
            "Principal": principal,
            "Action": action,
            "Resource": resource,
        }],
    })
    .to_string()
}

/// A resource-based policy whose one statement ALLOWS everything of this service
/// for everyone — the counterpart of [`denying`], which must refuse nothing.
fn allowing() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": "*",
            "Action": "s3tables:*",
            "Resource": "*",
        }],
    })
    .to_string()
}

/// One call under test, by the operation it performs. The resource each is
/// decided against is the ARN of what it acts on: the table for a call that
/// names one, else the table bucket it addresses.
const CALLS: [&str; 5] = [
    "GetTableBucket",
    "ListNamespaces",
    "GetTableMaintenanceConfiguration",
    "ListTagsForResource",
    "GetTable",
];

/// Make the call `operation` names against the bucket and table the case holds.
async fn invoke(emulator: &Emulator, operation: &str, bucket: &str, table_arn: &str) -> Reply {
    match operation {
        "GetTableBucket" => emulator.get_table_bucket(REGION, bucket).await,
        "ListNamespaces" => emulator.list_namespaces(REGION, bucket, &[]).await,
        "GetTableMaintenanceConfiguration" => {
            emulator
                .get_table_maintenance_configuration(REGION, bucket, NAMESPACE, TABLE)
                .await
        }
        "ListTagsForResource" => emulator.list_tags_for_resource(REGION, table_arn).await,
        // Addressed by the table's OWN ARN rather than by its coordinates, which
        // is the other way this model's inputs name a table.
        "GetTable" => emulator.get_table_by_arn(REGION, table_arn).await,
        other => panic!("the suite makes no {other} call"),
    }
}

/// The ARN the decision on `operation` is taken against: the table's own ARN for
/// a call that names a table, and the bucket's for a call that names only the
/// bucket.
fn decided_on(operation: &str, bucket: &str, table_arn: &str) -> String {
    match operation {
        "GetTableBucket" | "ListNamespaces" => bucket.to_string(),
        _ => table_arn.to_string(),
    }
}

/// Assert `reply` is the refusal an explicit deny in a resource-based policy
/// earns: HTTP 403 `AccessDeniedException`, carrying IAM's sentence for that
/// class — the principal that signed, the action it asked for, the resource it
/// addressed and which document withheld it. The sentence is spelled out here so
/// the suite pins the wording itself.
fn assert_denied(reply: &Reply, caller: &str, operation: &str, resource: &str, case: &str) {
    assert_eq!(
        reply.status, 403,
        "case {case:?}: expected the denied {operation} to be refused with 403, got {status} \
         {body}",
        status = reply.status,
        body = reply.body
    );
    assert_eq!(
        reply.error_code.as_deref(),
        Some(ACCESS_DENIED),
        "case {case:?}: expected error type {ACCESS_DENIED} from {operation}, got {actual:?}",
        actual = reply.error_code
    );
    let expected = format!(
        "User: {caller} is not authorized to perform: s3tables:{operation} on resource: \
         {resource} with an explicit deny in a resource-based policy"
    );
    assert_eq!(
        reply.message(),
        expected,
        "case {case:?}: expected IAM's sentence for {operation}, got {actual:?}",
        actual = reply.message()
    );
}

/// A table bucket holding one namespace with one table in it: the bucket's ARN
/// and the table's own ARN.
async fn bucket_with_a_table(emulator: &Emulator, name: &str) -> (String, String) {
    super::table_maintenance_configuration::create_table(emulator, REGION, name, NAMESPACE, TABLE)
        .await
}

#[tokio::test]
async fn an_explicit_deny_in_the_bucket_policy_refuses_the_calls_it_names() {
    let emulator = Emulator::new();
    // The principal a refusal names is the one the caller reads back for the
    // credentials these calls are signed with, asked for the way a client asks.
    let caller = emulator.caller_identity_arn(REGION).await;
    let every_call: Vec<(&str, bool)> = CALLS.iter().map(|call| (*call, true)).collect();
    let no_call: Vec<(&str, bool)> = CALLS.iter().map(|call| (*call, false)).collect();

    // (case, the document attached to the case's own bucket, and per call
    // whether that document refuses it). The document is built from the bucket's
    // ARN, so a statement can name the bucket or the tables inside it.
    type Document = Box<dyn Fn(&str) -> String>;
    /// One case: its name, the document attached to its own bucket, and per call
    /// whether that document refuses it.
    type Row = (&'static str, Document, Vec<(&'static str, bool)>);
    let rows: Vec<Row> = vec![
        (
            "the one action it names, on every resource",
            Box::new(|_| {
                denying(
                    json!("*"),
                    "s3tables:GetTableMaintenanceConfiguration",
                    "*",
                )
            }),
            vec![
                ("GetTableMaintenanceConfiguration", true),
                ("GetTableBucket", false),
                ("ListTagsForResource", false),
            ],
        ),
        (
            "every action of this service, on every resource",
            Box::new(|_| denying(json!("*"), "s3tables:*", "*")),
            every_call.clone(),
        ),
        (
            "a wildcard run inside the action name",
            Box::new(|_| denying(json!("*"), "s3tables:ListT*", "*")),
            vec![
                ("ListTagsForResource", true),
                ("ListNamespaces", false),
                ("GetTableBucket", false),
            ],
        ),
        (
            "the tables inside the bucket",
            Box::new(|bucket| {
                denying(json!("*"), "s3tables:*", &format!("{bucket}/table/*"))
            }),
            vec![
                ("GetTableMaintenanceConfiguration", true),
                ("ListTagsForResource", true),
                ("GetTable", true),
                ("GetTableBucket", false),
                ("ListNamespaces", false),
            ],
        ),
        (
            "the bucket itself",
            Box::new(|bucket| denying(json!("*"), "s3tables:*", bucket)),
            vec![
                ("GetTableBucket", true),
                ("ListNamespaces", true),
                ("GetTableMaintenanceConfiguration", false),
                ("ListTagsForResource", false),
            ],
        ),
        (
            "another bucket's resources",
            Box::new(|_| {
                denying(
                    json!("*"),
                    "s3tables:*",
                    &expected_bucket_arn(REGION, "another-bucket"),
                )
            }),
            no_call.clone(),
        ),
        (
            "another service's actions",
            Box::new(|_| denying(json!("*"), "s3:*", "*")),
            no_call.clone(),
        ),
        (
            "a principal that is not the caller",
            Box::new(|_| {
                denying(
                    json!({ "AWS": format!("arn:aws:iam::{ACCOUNT}:role/not-the-caller") }),
                    "s3tables:*",
                    "*",
                )
            }),
            no_call.clone(),
        ),
        (
            "the account the caller belongs to",
            Box::new(|_| {
                denying(
                    json!({ "AWS": format!("arn:aws:iam::{ACCOUNT}:root") }),
                    "s3tables:*",
                    "*",
                )
            }),
            every_call.clone(),
        ),
        (
            "the caller's own principal",
            Box::new({
                let caller = caller.clone();
                move |_| denying(json!({ "AWS": caller.clone() }), "s3tables:*", "*")
            }),
            every_call.clone(),
        ),
        (
            "a statement that allows rather than denies",
            Box::new(|_| allowing()),
            no_call.clone(),
        ),
    ];

    for (index, (case, policy, expectations)) in rows.into_iter().enumerate() {
        // A FRESH bucket per case: a deny over every action of this service
        // locks the caller out of the policy sub-resource itself, which is the
        // document's own doing rather than something a case may undo.
        let (bucket, table_arn) =
            bucket_with_a_table(&emulator, &format!("policy-decides-call-{index}")).await;
        let policy = policy(&bucket);
        let put = emulator
            .put_table_bucket_policy(REGION, &bucket, &policy)
            .await;
        assert_eq!(
            put.status, 200,
            "case {case:?}: attaching the policy must succeed, got {status} {body}",
            status = put.status,
            body = put.body
        );
        for (operation, refused) in expectations {
            let reply = invoke(&emulator, operation, &bucket, &table_arn).await;
            if refused {
                assert_denied(
                    &reply,
                    &caller,
                    operation,
                    &decided_on(operation, &bucket, &table_arn),
                    case,
                );
            } else {
                assert_served(&reply, operation, case);
            }
        }
    }
}

#[tokio::test]
async fn a_denied_write_changes_nothing_and_the_policy_decides_only_while_it_is_attached() {
    let emulator = Emulator::new();
    let (bucket, _) = bucket_with_a_table(&emulator, "policy-decides-writes").await;
    let caller = emulator.caller_identity_arn(REGION).await;
    let tags = json!({ "owner": "analytics" });

    let put = emulator
        .put_table_bucket_policy(
            REGION,
            &bucket,
            &denying(json!("*"), "s3tables:TagResource", "*"),
        )
        .await;
    assert_eq!(
        put.status, 200,
        "attaching the policy must succeed, got {status} {body}",
        status = put.status,
        body = put.body
    );

    let refused = emulator.tag_resource(REGION, &bucket, &tags).await;
    assert_denied(
        &refused,
        &caller,
        "TagResource",
        &bucket,
        "a write the policy denies",
    );
    // The refused write changed nothing: the read the policy says nothing about
    // reports the resource as it was.
    let read = emulator.list_tags_for_resource(REGION, &bucket).await;
    assert_served(&read, "ListTagsForResource", "a read the policy allows");
    assert_eq!(
        read.body
            .get("tags")
            .and_then(Value::as_object)
            .and_then(|tags| tags.get("owner")),
        None,
        "case \"a write the policy denies\": expected the refused write to store no tag, got \
         {body}",
        body = read.body
    );

    // The document decides only while it is attached: deleting it lifts the
    // refusal, and the same call is then served and takes effect.
    let deleted = emulator.delete_table_bucket_policy(REGION, &bucket).await;
    assert_eq!(
        deleted.status, 204,
        "deleting the policy must report success, got {status} {body}",
        status = deleted.status,
        body = deleted.body
    );
    let tagged = emulator.tag_resource(REGION, &bucket, &tags).await;
    assert_served(&tagged, "TagResource", "the same write after the delete");
    let read = emulator.list_tags_for_resource(REGION, &bucket).await;
    assert_eq!(
        read.body
            .get("tags")
            .and_then(Value::as_object)
            .and_then(|tags| tags.get("owner"))
            .and_then(Value::as_str),
        Some("analytics"),
        "case \"the same write after the delete\": expected the tag the write applied, got {body}",
        body = read.body
    );
}
