// SPDX-License-Identifier: CC0-1.0

//! What a bucket policy's `Principal` is resolved against, and for how long a grant it makes
//! is in force: the IAM records the account holds AT THE TIME OF THE CALL, never the ARN text
//! the document carries.
//!
//! The two rules under test:
//!
//! * **`PutBucketPolicy` resolves every identity its document grants to against the account's
//!   IAM records as they stand when the call is made.** The SAME document is accepted while the
//!   named identity exists, refused 400 `MalformedPolicy` / `Invalid principal in policy` once
//!   that identity is deleted — leaving the policy the bucket already carried exactly as it was
//!   — and accepted again once an identity is created under that name. So the refusal is
//!   attributable to the principal's nonexistence alone: not to the ARN string, not to a cache
//!   and not to a denylist.
//! * **A grant naming an identity is in force only while that identity exists.** The document
//!   survives the identity's deletion, but nothing reaches the bucket through it: a credential
//!   the deleted role had minted resolves to no principal, so the request is refused 403 at
//!   credential resolution with the code S3's published list carries for an access key id that
//!   exists in no record, and a LIVE principal the grant does not name is refused 403
//!   `AccessDenied`. A role created again under the identical name carries the identical ARN,
//!   so a NEW session of it is served through the surviving document, while the session of the
//!   deleted role stays dead — a role is identified by its id ("the stable and unique string
//!   identifying the role", `Role$RoleId`), not by its name.
//!
//! Every assertion drives real HTTP requests through a router holding S3 beside IAM and STS
//! over ONE shared engine ([`super::credential_scoped_authorization::Emulator`]): the roles,
//! the users and the sessions are made with those services' own operations (`iam:CreateRole`,
//! `iam:CreateUser`, `iam:CreateAccessKey`, `iam:DeleteRole`, `iam:DeleteUser`,
//! `sts:AssumeRole`), and each S3 request presents the credential's own access key id.
//!
//! The first table's rows differ along what the rule generalises over: which ENTITY TYPE the
//! document names (an IAM role, an IAM user), under which PATH the account holds it (the
//! default path, a path of its own — which the entity's ARN carries), and in which FORM the
//! `Principal` element names it (one string, or one entry of a list beside a value that always
//! resolves).

use serde_json::json;

use super::credential_scoped_authorization::{element, Answer, Emulator, ACCOUNT_KEY};

/// The code and the message `PutBucketPolicy` refuses a document naming an identity it cannot
/// resolve as a grantee with. Written out here rather than imported from the service, so the
/// wording has something to be wrong about.
const MALFORMED_POLICY: &str = "MalformedPolicy";
/// See [`MALFORMED_POLICY`]: fixed text in full — the value that did not resolve is not quoted
/// back.
const INVALID_PRINCIPAL: &str = "Invalid principal in policy";

/// The code S3 answers an authorization refusal with, and the status it carries (Amazon S3
/// "Error responses": `AccessDenied`, HTTP 403).
const ACCESS_DENIED: &str = "AccessDenied";

/// The code S3 answers a request whose access key id resolves to NO principal with ("the AWS
/// access key ID that you provided does not exist in our records", HTTP 403) — what a session
/// of a role the account no longer holds presents.
const INVALID_ACCESS_KEY_ID: &str = "InvalidAccessKeyId";

/// How long a session minted here lasts: comfortably longer than the suite's own run, so no
/// assertion is decided by a credential that expired mid-test.
const SESSION_SECONDS: &str = "3600";

/// WHICH IAM entity a row's document names as its grantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entity {
    /// An IAM role, held under the given path (`None` for IAM's default path).
    Role { path: Option<&'static str> },
    /// An IAM user — a different IAM entity from a role, resolved under its own type.
    User,
}

/// In which FORM the `Principal` element names the row's entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// `"Principal": {"AWS": "<arn>"}` — the entity alone.
    Alone,
    /// `"Principal": {"AWS": ["<arn>", "<account root>"]}` — one entry of a list whose other
    /// entry always resolves, so the list stands or falls on the entity.
    InAList,
}

impl Form {
    /// The `Principal` element this form writes for `arn`.
    fn element(self, arn: &str) -> serde_json::Value {
        match self {
            Self::Alone => json!({"AWS": arn}),
            Self::InAList => json!({"AWS": [arn, account_root()]}),
        }
    }
}

/// One row of the lifecycle table.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// What to call the case in a failure.
    case: &'static str,
    /// The IAM entity the row's document grants to.
    entity: Entity,
    /// The form its `Principal` element names that entity in.
    form: Form,
}

/// The ARN that names the account itself, which always resolves: the other entry of an
/// [`Form::InAList`] row, and the grantee of every row's BASELINE document.
fn account_root() -> String {
    format!("arn:aws:iam::{}:root", engine::emulator_account_id())
}

/// A one-statement bucket policy granting `s3:GetObject` on `bucket`'s objects to `principal`,
/// under the statement id `sid`.
fn document(sid: &str, principal: &serde_json::Value, bucket: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": sid,
            "Effect": "Allow",
            "Principal": principal,
            "Action": "s3:GetObject",
            "Resource": format!("arn:aws:s3:::{bucket}/*"),
        }],
    })
    .to_string()
}

/// The trust policy a role of this suite carries: the account itself may assume it, which is
/// what lets [`assume_role`] mint a session of it.
fn trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"AWS": account_root()},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// Create the row's entity through IAM's own API and answer the ARN the create reports — the
/// spelling a policy names it by, read from the service rather than built here.
async fn create_entity(emulator: &Emulator, entity: Entity, name: &str) -> String {
    let (status, body) = match entity {
        Entity::Role { path } => {
            let mut params = vec![
                ("RoleName", name.to_string()),
                ("AssumeRolePolicyDocument", trust_policy()),
            ];
            if let Some(path) = path {
                params.push(("Path", path.to_string()));
            }
            emulator
                .query(ACCOUNT_KEY, "iam", "2010-05-08", "CreateRole", &params)
                .await
        }
        Entity::User => {
            emulator
                .query(
                    ACCOUNT_KEY,
                    "iam",
                    "2010-05-08",
                    "CreateUser",
                    &[("UserName", name.to_string())],
                )
                .await
        }
    };
    assert_eq!(
        status, 200,
        "creating {entity:?} {name:?} must succeed, got {status} with {body}"
    );
    element(&body, "Arn")
        .unwrap_or_else(|| panic!("the create must answer the entity Arn, got {body}"))
}

/// Delete the row's entity through IAM's own API, so the account holds it no more. The
/// entities of this suite carry no inline or attached policy, which is what lets the delete
/// through ("before you delete a role, remove the following attached items", `DeleteRole`).
async fn delete_entity(emulator: &Emulator, entity: Entity, name: &str) {
    let (action, params) = match entity {
        Entity::Role { .. } => ("DeleteRole", [("RoleName", name.to_string())]),
        Entity::User => ("DeleteUser", [("UserName", name.to_string())]),
    };
    let (status, body) = emulator
        .query(ACCOUNT_KEY, "iam", "2010-05-08", action, &params)
        .await;
    assert_eq!(
        status, 200,
        "deleting {entity:?} {name:?} must succeed, got {status} with {body}"
    );
}

/// `sts:AssumeRole` on the role `name` of this account, answering the access key id of the
/// session it minted.
async fn assume_role(emulator: &Emulator, name: &str, session_name: &str) -> String {
    let (status, body) = emulator
        .query(
            ACCOUNT_KEY,
            "sts",
            "2011-06-15",
            "AssumeRole",
            &[
                (
                    "RoleArn",
                    format!("arn:aws:iam::{}:role/{name}", engine::emulator_account_id()),
                ),
                ("RoleSessionName", session_name.to_string()),
                ("DurationSeconds", SESSION_SECONDS.to_string()),
            ],
        )
        .await;
    assert_eq!(
        status, 200,
        "assuming the role {name:?} must succeed, got {status} with {body}"
    );
    element(&body, "AccessKeyId")
        .unwrap_or_else(|| panic!("AssumeRole must answer an AccessKeyId, got {body}"))
}

/// `PUT /<bucket>?policy` as the account's own credential.
async fn put_bucket_policy(emulator: &Emulator, bucket: &str, document: &str) -> Answer {
    emulator
        .s3(
            ACCOUNT_KEY,
            "PUT",
            &format!("/{bucket}?policy"),
            document.as_bytes(),
        )
        .await
}

/// `GET /<bucket>?policy` as the account's own credential — the document the bucket carries,
/// byte for byte.
async fn get_bucket_policy(emulator: &Emulator, bucket: &str) -> Answer {
    emulator
        .s3(ACCOUNT_KEY, "GET", &format!("/{bucket}?policy"), b"")
        .await
}

/// Create `bucket` as the account's own credential.
async fn create_bucket(emulator: &Emulator, bucket: &str) {
    let created = emulator.s3(ACCOUNT_KEY, "PUT", &format!("/{bucket}"), b"").await;
    assert_eq!(
        created.status, 200,
        "the bucket {bucket:?} must be created, got {} with {}",
        created.status, created.body
    );
}

/// RULE: `PutBucketPolicy` resolves the identities its document grants to against the account's
/// IAM records as they stand when the call is made — so one document is accepted, then refused
/// once the identity it names is deleted (leaving the stored policy untouched), then accepted
/// again once an identity exists under that name.
#[tokio::test]
async fn principal_resolution_follows_the_account_s_iam_records() {
    let rows: [Row; 4] = [
        Row {
            case: "a role at IAM's default path",
            entity: Entity::Role { path: None },
            form: Form::Alone,
        },
        Row {
            case: "a role the account holds under a path of its own",
            entity: Entity::Role {
                path: Some("/sim2real/"),
            },
            form: Form::Alone,
        },
        Row {
            case: "an IAM user",
            entity: Entity::User,
            form: Form::Alone,
        },
        Row {
            case: "a role named in a list beside the account's own root",
            entity: Entity::Role { path: None },
            form: Form::InAList,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let emulator = Emulator::new();
        let bucket = format!("principal-lifecycle-{index}");
        let name = format!("principal-lifecycle-{index}");
        create_bucket(&emulator, &bucket).await;

        // The document the bucket carries before the refused write, whose grantee always
        // resolves: what the refusal has to leave alone.
        let baseline = document("Baseline", &json!({"AWS": account_root()}), &bucket);
        let stored = put_bucket_policy(&emulator, &bucket, &baseline).await;
        assert_eq!(
            stored.status, 204,
            "case {case:?}: the baseline policy must be stored, got {} with {}",
            stored.status, stored.body
        );

        // Leg 1: the identity exists, so the document naming it is accepted and read back.
        let arn = create_entity(&emulator, row.entity, &name).await;
        let names_the_entity = document("NamesTheEntity", &row.form.element(&arn), &bucket);
        let accepted = put_bucket_policy(&emulator, &bucket, &names_the_entity).await;
        assert_eq!(
            accepted.status, 204,
            "case {case:?}: a document naming the live identity {arn:?} must be accepted, got \
             {} with {}",
            accepted.status, accepted.body
        );
        let read = get_bucket_policy(&emulator, &bucket).await;
        assert_eq!(
            read.body, names_the_entity,
            "case {case:?}: the accepted document must be reported byte for byte, got {:?}",
            read.body
        );

        // Put the baseline back, so the refusal below has a DIFFERENT document to leave alone.
        let restored = put_bucket_policy(&emulator, &bucket, &baseline).await;
        assert_eq!(
            restored.status, 204,
            "case {case:?}: the baseline must be restorable, got {} with {}",
            restored.status, restored.body
        );

        // Leg 2: the identity is deleted, and the byte-identical document is refused.
        delete_entity(&emulator, row.entity, &name).await;
        let refused = put_bucket_policy(&emulator, &bucket, &names_the_entity).await;
        assert_eq!(
            (
                refused.status,
                refused.error_code(),
                refused.error_message()
            ),
            (
                400,
                MALFORMED_POLICY.to_string(),
                INVALID_PRINCIPAL.to_string()
            ),
            "case {case:?}: a document naming the deleted identity {arn:?} must be refused, got \
             {} with {}",
            refused.status,
            refused.body
        );
        let read = get_bucket_policy(&emulator, &bucket).await;
        assert_eq!(
            read.body, baseline,
            "case {case:?}: the refused write must leave the stored policy as it was, got {:?}",
            read.body
        );

        // Leg 3: an identity exists under that name again — the identical ARN — and the same
        // call succeeds, so the refusal was about the identity and not about the text.
        let recreated = create_entity(&emulator, row.entity, &name).await;
        assert_eq!(
            recreated, arn,
            "case {case:?}: the identity created again under the same name must carry the same \
             ARN, got {recreated:?}"
        );
        let accepted = put_bucket_policy(&emulator, &bucket, &names_the_entity).await;
        assert_eq!(
            accepted.status, 204,
            "case {case:?}: the same document must be accepted again, got {} with {}",
            accepted.status, accepted.body
        );
        let read = get_bucket_policy(&emulator, &bucket).await;
        assert_eq!(
            read.body, names_the_entity,
            "case {case:?}: the document accepted again must be the stored one, got {:?}",
            read.body
        );
    }
}

/// RULE: a grant naming an identity is in force only while that identity exists — the document
/// survives the identity's deletion and reaches nothing through it, and a role created again
/// under the identical name is granted by the surviving document while the deleted role's own
/// session stays dead.
#[tokio::test]
async fn a_grant_to_a_deleted_role_reaches_nothing_while_the_document_survives() {
    let emulator = Emulator::new();
    let bucket = "principal-lifecycle-grant";
    let key = "reports/held.csv";
    let body = b"principal-lifecycle-subject";
    let role = "principal-lifecycle-grantee";
    create_bucket(&emulator, bucket).await;
    let written = emulator
        .s3(ACCOUNT_KEY, "PUT", &format!("/{bucket}/{key}"), body)
        .await;
    assert_eq!(
        written.status, 200,
        "the object must be stored first, got {} with {}",
        written.status, written.body
    );

    // The role, a session of it, and the bucket policy that grants THAT role the read. The
    // role carries no identity-based policy of its own, so the bucket's document is the only
    // one in force on the session.
    let arn = create_entity(&emulator, Entity::Role { path: None }, role).await;
    let first_session = assume_role(&emulator, role, "before-the-delete").await;
    let grant = document("AllowTheRole", &json!({"AWS": &arn}), bucket);
    let stored = put_bucket_policy(&emulator, bucket, &grant).await;
    assert_eq!(
        stored.status, 204,
        "the grant naming the live role must be stored, got {} with {}",
        stored.status, stored.body
    );

    let served = emulator
        .s3(&first_session, "GET", &format!("/{bucket}/{key}"), b"")
        .await;
    assert_eq!(
        (served.status, served.body.as_str()),
        (200, String::from_utf8_lossy(body).as_ref()),
        "case {:?}: the grant must authorize the role's own session, got {} with {}",
        "a session of the live role",
        served.status,
        served.body
    );

    // A LIVE principal the grant does not name reaches nothing through it: a grant to one
    // identity is not a grant to the account.
    let bystander = user_with_a_key(&emulator, "principal-lifecycle-bystander").await;
    let refused = emulator
        .s3(&bystander, "GET", &format!("/{bucket}/{key}"), b"")
        .await;
    assert_eq!(
        (refused.status, refused.error_code()),
        (403, ACCESS_DENIED.to_string()),
        "case {:?}: a principal the grant does not name must be refused, got {} with {}",
        "a live principal the document does not name",
        refused.status,
        refused.body
    );

    // The role is deleted: the document survives, and the session it had minted speaks for
    // nobody, so the request is refused at credential resolution.
    delete_entity(&emulator, Entity::Role { path: None }, role).await;
    let dead = emulator
        .s3(&first_session, "GET", &format!("/{bucket}/{key}"), b"")
        .await;
    assert_eq!(
        (dead.status, dead.error_code()),
        (403, INVALID_ACCESS_KEY_ID.to_string()),
        "case {:?}: the deleted role's session must reach nothing, got {} with {}",
        "a session of the deleted role",
        dead.status,
        dead.body
    );
    let read = get_bucket_policy(&emulator, bucket).await;
    assert_eq!(
        read.body, grant,
        "case {:?}: the stored document must survive the deletion of the identity it names, \
         got {:?}",
        "the document after the delete", read.body
    );

    // A role under the identical name carries the identical ARN, so the surviving document
    // grants to a NEW session of it — while the first session stays dead, because a role is
    // identified by its id and not by its name.
    let recreated = create_entity(&emulator, Entity::Role { path: None }, role).await;
    assert_eq!(
        recreated, arn,
        "the role created again under the same name must carry the same ARN, got {recreated:?}"
    );
    let second_session = assume_role(&emulator, role, "after-the-recreate").await;
    let served = emulator
        .s3(&second_session, "GET", &format!("/{bucket}/{key}"), b"")
        .await;
    assert_eq!(
        (served.status, served.body.as_str()),
        (200, String::from_utf8_lossy(body).as_ref()),
        "case {:?}: the surviving grant must authorize a session of the role created again, \
         got {} with {}",
        "a session of the role created again",
        served.status,
        served.body
    );
    let dead = emulator
        .s3(&first_session, "GET", &format!("/{bucket}/{key}"), b"")
        .await;
    assert_eq!(
        (dead.status, dead.error_code()),
        (403, INVALID_ACCESS_KEY_ID.to_string()),
        "case {:?}: the session of the deleted role must stay dead, got {} with {}",
        "the first session after the role was created again",
        dead.status,
        dead.body
    );
}

/// An IAM user carrying no policy at all, and the access key id its requests are signed with:
/// the live principal a grant to another identity does not reach.
async fn user_with_a_key(emulator: &Emulator, name: &str) -> String {
    create_entity(emulator, Entity::User, name).await;
    let (status, body) = emulator
        .query(
            ACCOUNT_KEY,
            "iam",
            "2010-05-08",
            "CreateAccessKey",
            &[("UserName", name.to_string())],
        )
        .await;
    assert_eq!(
        status, 200,
        "an access key for {name:?} must be vended, got {status} with {body}"
    );
    element(&body, "AccessKeyId")
        .unwrap_or_else(|| panic!("CreateAccessKey must answer an AccessKeyId, got {body}"))
}
