// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ACL gate a bucket's OBJECT OWNERSHIP setting puts on every route
//! that creates an object: `PutObject`, `CopyObject` and `CreateMultipartUpload`.
//!
//! Every assertion drives a real HTTP request through the router (the harness the
//! bucket-lifecycle suite in this crate owns), so what is pinned is the wire behaviour —
//! status, error code, message, and what the bucket holds afterwards.
//!
//! The rules under test, with their sources:
//!
//! * "Buckets that use this setting only accept PUT requests that don't specify an ACL or PUT
//!   requests that specify bucket owner full control ACLs … PUT requests that contain other
//!   ACLs … fail and return a 400 error with the error code `AccessControlListNotSupported`"
//!   (`PutObjectRequest$ACL`, and `CopyObjectRequest$ACL` says the same of a copy) — so the
//!   setting that disables ACLs admits an ACL that leaves the bucket owner in full control of
//!   the object and grants nobody else anything, and refuses every other. Which grantees and
//!   permissions a canned value names comes from the published *Canned ACL* table (Amazon S3
//!   User Guide), whose values are the ones `ObjectCannedACL` publishes
//!   (`smithy_query(query="shape",shape="ObjectCannedACL")`).
//! * The gate belongs to the WRITE, not to one operation: `CreateMultipartUpload`'s own `ACL`
//!   member is the same `x-amz-acl` header targeting the same enum, and "when you initiate a
//!   multipart upload, you specify the options you want on the finished object" — so an
//!   initiation naming an ACL the bucket does not allow is refused before any upload exists,
//!   which `ListMultipartUploads` is what shows.
//! * A refusal is the published envelope: `AccessControlListNotSupported` with "The bucket
//!   does not allow ACLs" at HTTP 400 (Amazon S3 "Error responses", list of error codes), and
//!   it leaves nothing behind — no object at the key and no open upload.
//! * A bucket whose ownership setting ENABLES ACLs ("ObjectWriter") takes every one of those
//!   same ACLs, which is what makes the setting the guard rather than the value.

use service_com_amazonaws_s3_amazons3::state::{
    canonical_user_id, ACL_NOT_SUPPORTED_CODE, ACL_NOT_SUPPORTED_MESSAGE, ACL_REFUSAL_STATUS,
    ALL_USERS_GROUP_URI, LOG_DELIVERY_GROUP_URI, OBJECT_CANNED_A_C_L_VALUES,
};
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{create_bucket, router, send, send_bytes, Answer};
use super::bucket_ownership_controls::put_ownership_controls;
use super::bucket_public_access_block::put_public_access_block;
use super::kms_key_references::{copy_object, create_multipart_upload};
use super::multipart_upload_completion::{checksummed_completion_payload, complete, upload_part};
use super::object_and_tagging_roundtrip::{get_object, put_object, text};

/// The bytes every object in this suite is written with: the gate is about WHO may reach the
/// object, not about what is in it, so one payload serves every row.
const BODY: &[u8] = b"ownership-gated-write";

/// The key the copy source of every row is written at.
const SOURCE_KEY: &str = "source.bin";

/// One row of the ACL table: the case, the headers that name the ACL, and whether a bucket
/// whose ownership setting DISABLES ACLs accepts it.
type AclRow = (&'static str, Vec<(&'static str, String)>, bool);

/// The ACLs an object-creating write can name, each with whether a bucket whose ownership
/// setting disables ACLs accepts it.
///
/// The canned rows are every value `ObjectCannedACL` publishes, classified by what the
/// published *Canned ACL* table says the row grants and to whom: `private` gives the owner
/// `FULL_CONTROL` and "no one else has access rights", `bucket-owner-full-control` gives "both
/// the object owner and the bucket owner" `FULL_CONTROL` and `bucket-owner-read` adds the
/// bucket owner's `READ` to the object owner's `FULL_CONTROL` — each of the three leaves the
/// bucket owner in full control and reaches nobody else, since the setting that disables ACLs
/// makes the bucket owner the owner of every object. `public-read` and `public-read-write`
/// grant to `AllUsers`, `authenticated-read` to `AuthenticatedUsers` and `aws-exec-read` to an
/// Amazon EC2 account: each of those is a third party.
///
/// The grant-header rows are the member's other form — "specify access permissions explicitly
/// to … grant access permissions to specific Amazon Web Services accounts or groups"
/// (`PutObjectRequest$GrantRead`) — classified by the same two questions: this account's
/// canonical user id is the bucket owner and a predefined group's URI is not, and only
/// `FULL_CONTROL` leaves that owner in full control.
fn acl_rows() -> Vec<AclRow> {
    let owner = canonical_user_id();
    let mut rows: Vec<AclRow> = vec![
        ("the canned ACL private", vec![("x-amz-acl", "private".to_string())], true),
        (
            "the canned ACL bucket-owner-read",
            vec![("x-amz-acl", "bucket-owner-read".to_string())],
            true,
        ),
        (
            "the canned ACL bucket-owner-full-control",
            vec![("x-amz-acl", "bucket-owner-full-control".to_string())],
            true,
        ),
        (
            "the canned ACL public-read",
            vec![("x-amz-acl", "public-read".to_string())],
            false,
        ),
        (
            "the canned ACL public-read-write",
            vec![("x-amz-acl", "public-read-write".to_string())],
            false,
        ),
        (
            "the canned ACL authenticated-read",
            vec![("x-amz-acl", "authenticated-read".to_string())],
            false,
        ),
        (
            "the canned ACL aws-exec-read",
            vec![("x-amz-acl", "aws-exec-read".to_string())],
            false,
        ),
    ];
    rows.extend([
        ("no ACL at all", Vec::new(), true),
        (
            "the explicit grant of the owner's FULL_CONTROL",
            vec![("x-amz-grant-full-control", format!("id={owner}"))],
            true,
        ),
        (
            "the explicit grant of the owner's READ alone, which is not full control",
            vec![("x-amz-grant-read", format!("id={owner}"))],
            false,
        ),
        (
            "an explicit READ grant to every user",
            vec![("x-amz-grant-read", format!("uri={ALL_USERS_GROUP_URI}"))],
            false,
        ),
        (
            "an explicit WRITE_ACP grant to the log-delivery group",
            vec![("x-amz-grant-write-acp", format!("uri={LOG_DELIVERY_GROUP_URI}"))],
            false,
        ),
    ]);
    rows
}

/// The headers of one row, borrowed for a request.
fn headers_of<'a>(row: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    row.iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect()
}

/// A bucket carrying the copy source, with `setting` as its Object Ownership setting and
/// `BlockPublicAcls` OFF.
///
/// The three other Block Public Access switches stay ON: a bucket is created with all four on,
/// and with `BlockPublicAcls` among them a PUBLIC ACL would be refused by that guardrail
/// instead ("PUT Object calls fail if the request includes a public ACL",
/// `PublicAccessBlockConfiguration$BlockPublicAcls`), which would make the table say nothing
/// about the ownership setting under test.
async fn bucket_with_ownership(router: &MultiServiceRouter, name: &str, setting: &str) {
    let created = create_bucket(router, name, None, None).await;
    assert_eq!(
        created.status,
        200,
        "the bucket {name:?} must be created first, got {} with body {:?}",
        created.status,
        text(&created)
    );
    let ownership = put_ownership_controls(router, name, &[setting]).await;
    assert_eq!(
        ownership.status,
        200,
        "the bucket {name:?} must take the {setting:?} ownership setting, got {} with body {:?}",
        ownership.status,
        text(&ownership)
    );
    let guardrails = put_public_access_block(
        router,
        name,
        &[
            ("BlockPublicAcls", false),
            ("IgnorePublicAcls", true),
            ("BlockPublicPolicy", true),
            ("RestrictPublicBuckets", true),
        ],
        &[],
    )
    .await;
    assert_eq!(
        guardrails.status,
        200,
        "the bucket {name:?} must take the Block Public Access posture that leaves the \
         ownership setting the only guard, got {} with body {:?}",
        guardrails.status,
        text(&guardrails)
    );
    let written = put_object(router, name, SOURCE_KEY, BODY, &[]).await;
    assert_eq!(
        written.status,
        200,
        "the copy source must be written into {name:?} first, got {} with body {:?}",
        written.status,
        text(&written)
    );
}

/// `GET /<bucket>?uploads` — `ListMultipartUploads`, which is what shows whether a refused
/// initiation left an upload behind.
async fn list_multipart_uploads(router: &MultiServiceRouter, bucket: &str) -> Answer {
    send_bytes(router, "GET", &format!("/{bucket}?uploads"), &[], b"").await
}

/// `GET /<bucket>?list-type=2` — `ListObjectsV2`, which is what shows the keys a bucket holds.
async fn list_objects(router: &MultiServiceRouter, bucket: &str) -> Answer {
    send(router, "GET", &format!("/{bucket}?list-type=2"), &[], "").await
}

/// What one route answered, and what the bucket holds at that key afterwards.
struct Attempt {
    /// The answer to the write itself.
    answer: Answer,
    /// Whether the bucket holds the thing the write would have created: an object at the key,
    /// or an open upload for it.
    created: bool,
}

/// `PutObject` of `key` carrying `headers`, and whether an object is then readable at it.
async fn attempt_put(
    router: &MultiServiceRouter,
    bucket: &str,
    key: &str,
    headers: &[(&str, &str)],
) -> Attempt {
    let answer = put_object(router, bucket, key, BODY, headers).await;
    let read = get_object(router, bucket, key, "", &[]).await;
    Attempt {
        answer,
        created: read.status == 200,
    }
}

/// `CopyObject` from the suite's source key to `key` carrying `headers`, and whether an object
/// is then readable at it.
async fn attempt_copy(
    router: &MultiServiceRouter,
    bucket: &str,
    key: &str,
    headers: &[(&str, &str)],
) -> Attempt {
    let answer = copy_object(
        router,
        bucket,
        key,
        &format!("/{bucket}/{SOURCE_KEY}"),
        headers,
    )
    .await;
    let read = get_object(router, bucket, key, "", &[]).await;
    Attempt {
        answer,
        created: read.status == 200,
    }
}

/// `CreateMultipartUpload` of `key` carrying `headers`, and whether an upload for that key is
/// then open.
async fn attempt_initiation(
    router: &MultiServiceRouter,
    bucket: &str,
    key: &str,
    headers: &[(&str, &str)],
) -> Attempt {
    let answer = create_multipart_upload(router, bucket, key, headers).await;
    let listed = list_multipart_uploads(router, bucket).await;
    let created = listed.elements("Key").iter().any(|listed| listed == key);
    Attempt { answer, created }
}

/// The three routes that create an object from a request naming an ACL, each by its operation
/// name and the attempt it drives.
const ROUTES: [&str; 3] = ["PutObject", "CopyObject", "CreateMultipartUpload"];

/// One route's attempt at `key` with `headers`.
async fn attempt(
    router: &MultiServiceRouter,
    route: &str,
    bucket: &str,
    key: &str,
    headers: &[(&str, &str)],
) -> Attempt {
    match route {
        "PutObject" => attempt_put(router, bucket, key, headers).await,
        "CopyObject" => attempt_copy(router, bucket, key, headers).await,
        _ => attempt_initiation(router, bucket, key, headers).await,
    }
}

#[tokio::test]
async fn the_table_covers_every_canned_acl_the_model_publishes() {
    let rows = acl_rows();
    for canned in OBJECT_CANNED_A_C_L_VALUES {
        let covered = rows.iter().any(|(_, headers, _)| {
            headers
                .iter()
                .any(|(name, value)| *name == "x-amz-acl" && value == canned)
        });
        assert!(
            covered,
            "case {canned:?}: every published ObjectCannedACL value needs a row saying whether \
             a bucket with ACLs disabled accepts it, and the table has none"
        );
    }
}

#[tokio::test]
async fn a_bucket_with_acls_disabled_admits_only_a_bucket_owner_full_control_acl() {
    let router = router();
    let bucket = "ownership-gate-enforced";
    bucket_with_ownership(&router, bucket, "BucketOwnerEnforced").await;

    for (index, (case, headers, accepted)) in acl_rows().into_iter().enumerate() {
        let borrowed = headers_of(&headers);
        for route in ROUTES {
            let key = format!("gated/{index}-{route}.bin");
            let attempted = attempt(&router, route, bucket, &key, &borrowed).await;
            if accepted {
                assert_eq!(
                    attempted.answer.status,
                    200,
                    "case {case:?} on {route}: an ACL leaving the bucket owner in full control \
                     and granting nobody else anything must be served, got {} with body {:?}",
                    attempted.answer.status,
                    text(&attempted.answer)
                );
                assert!(
                    attempted.created,
                    "case {case:?} on {route}: a served write must leave its object or its \
                     upload behind, and {key:?} holds neither"
                );
                continue;
            }
            assert_eq!(
                attempted.answer.status,
                ACL_REFUSAL_STATUS,
                "case {case:?} on {route}: expected HTTP {ACL_REFUSAL_STATUS}, got {} with body \
                 {:?}",
                attempted.answer.status,
                text(&attempted.answer)
            );
            assert_eq!(
                attempted.answer.error_code(),
                ACL_NOT_SUPPORTED_CODE,
                "case {case:?} on {route}: expected the code {ACL_NOT_SUPPORTED_CODE:?}, got {:?}",
                attempted.answer.error_code()
            );
            assert_eq!(
                attempted.answer.error_message(),
                ACL_NOT_SUPPORTED_MESSAGE,
                "case {case:?} on {route}: expected the message {ACL_NOT_SUPPORTED_MESSAGE:?}, \
                 got {:?}",
                attempted.answer.error_message()
            );
            assert!(
                !attempted.created,
                "case {case:?} on {route}: a refused write must leave nothing behind, and \
                 {key:?} has an object or an open upload"
            );
        }
    }
}

#[tokio::test]
async fn a_bucket_whose_ownership_setting_enables_acls_takes_every_one_of_them() {
    let router = router();
    let bucket = "ownership-gate-object-writer";
    bucket_with_ownership(&router, bucket, "ObjectWriter").await;

    for (index, (case, headers, _)) in acl_rows().into_iter().enumerate() {
        let borrowed = headers_of(&headers);
        for route in ROUTES {
            let key = format!("enabled/{index}-{route}.bin");
            let attempted = attempt(&router, route, bucket, &key, &borrowed).await;
            assert_eq!(
                attempted.answer.status,
                200,
                "case {case:?} on {route}: a bucket whose ownership setting enables ACLs takes \
                 any of them, got {} with body {:?}",
                attempted.answer.status,
                text(&attempted.answer)
            );
            assert!(
                attempted.created,
                "case {case:?} on {route}: a served write must leave its object or its upload \
                 behind, and {key:?} holds neither"
            );
        }
    }
}

#[tokio::test]
async fn a_bucket_owner_full_control_canned_acl_carries_a_copy_and_a_multipart_upload_through() {
    let router = router();
    let bucket = "ownership-gate-owner-only";
    bucket_with_ownership(&router, bucket, "BucketOwnerEnforced").await;

    // One flow per canned value a bucket with ACLs disabled admits, so the rule is pinned over
    // the whole class rather than over the one value a client happens to stamp on a copy.
    for (index, canned) in ["bucket-owner-read", "bucket-owner-full-control", "private"]
        .into_iter()
        .enumerate()
    {
        let acl = [("x-amz-acl", canned)];
        let copied = format!("copied-{index}");
        let copy = copy_object(
            &router,
            bucket,
            &copied,
            &format!("/{bucket}/{SOURCE_KEY}"),
            &acl,
        )
        .await;
        assert_eq!(
            copy.status,
            200,
            "case {canned:?}: the copy must be served, got {} with body {:?}",
            copy.status,
            text(&copy)
        );
        let read = get_object(&router, bucket, &copied, "", &[]).await;
        assert_eq!(
            read.status,
            200,
            "case {canned:?}: the copy must be readable back, got {} with body {:?}",
            read.status,
            text(&read)
        );
        assert_eq!(
            read.body.as_ref(),
            BODY,
            "case {canned:?}: the copy must carry the source's bytes, got {:?}",
            text(&read)
        );

        let multi = format!("multi-{index}");
        let initiated = create_multipart_upload(&router, bucket, &multi, &acl).await;
        assert_eq!(
            initiated.status,
            200,
            "case {canned:?}: the initiation must be served, got {} with body {:?}",
            initiated.status,
            text(&initiated)
        );
        let upload_id = initiated
            .element("UploadId")
            .expect("a served initiation reports an UploadId");
        let etag = upload_part(&router, bucket, &multi, &upload_id, 1, BODY).await;
        let completed = complete(
            &router,
            bucket,
            &multi,
            &upload_id,
            &checksummed_completion_payload(&[(1, etag, None)]),
            &[],
        )
        .await;
        assert_eq!(
            completed.status,
            200,
            "case {canned:?}: the completion must be served, got {} with body {:?}",
            completed.status,
            text(&completed)
        );
        let assembled = get_object(&router, bucket, &multi, "", &[]).await;
        assert_eq!(
            assembled.body.as_ref(),
            BODY,
            "case {canned:?}: the assembled object must carry the part's bytes, got {:?}",
            text(&assembled)
        );

        let listed = list_objects(&router, bucket).await;
        let mut held = listed.elements("Key");
        held.sort();
        let mut owed = vec![copied.clone(), multi.clone(), SOURCE_KEY.to_string()];
        owed.sort();
        assert!(
            owed.iter().all(|key| held.contains(key)),
            "case {canned:?}: the bucket must hold {owed:?}, got {held:?}"
        );
    }
}
