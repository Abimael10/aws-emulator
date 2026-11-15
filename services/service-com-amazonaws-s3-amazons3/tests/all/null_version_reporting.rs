// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the id an answer names the NULL OBJECT VERSION by.
//!
//! Every assertion drives a real HTTP request through the router (the harness the
//! bucket-lifecycle suite in this crate owns), so what is pinned is the wire behaviour — the
//! `x-amz-version-id` response header — rather than any handler's internals. Every request
//! helper comes from the suite that owns the operation it drives, so no call is spelled a
//! second way here.
//!
//! Two rules are under test, each over a SET of inputs rather than one recorded request.
//!
//! 1. **A response reporting the version it RESOLVED names that version by the id it is
//!    ADDRESSED by.** A bucket that carries a versioning configuration addresses every
//!    version it holds by an id: the one Amazon S3 minted while its status was `Enabled`
//!    ("Enabled—Enables versioning for the objects in the bucket. All objects added to the
//!    bucket receive a unique version ID"), and the literal `null` for one stored while its
//!    status was not ("Suspended—Disables versioning for the objects in the bucket. All
//!    objects added to the bucket receive the version ID null", and "Objects stored in your
//!    bucket before you set the versioning state have a version ID of null" — both
//!    `PutBucketVersioning`). A bucket that carries NO versioning configuration addresses no
//!    version at all, so such an answer names none.
//! 2. **A response reporting the version it CREATED names only the id the bucket GENERATED
//!    for the object being stored.** "If you enable versioning for a bucket, Amazon S3
//!    automatically generates a unique version ID for the object being stored. Amazon S3
//!    returns this ID in the response" (`PutObjectOutput$VersionId`, and "If you enable
//!    versioning on the destination bucket, Amazon S3 generates a unique version ID for the
//!    copied object" for `CopyObject`) — so a write into a bucket that is not
//!    versioning-enabled generates no id and reports none, even though a later READ of the
//!    `null` version it stored names that version by rule 1.
//!
//! Rule 1's rows differ along what it generalises over: the bucket's versioning state and
//! the ORDER the states were applied in (never set, suspended from the start, enabled then
//! suspended, suspended then enabled again — the last leaving a null version in a bucket
//! whose status IS `Enabled`), the OPERATION that reports the version (a read of the object,
//! a read of its attributes, and the write, read and removal of two different sub-resources),
//! and whether the request addresses the version by `versionId=null` or takes the key's
//! current one. Rule 2's rows differ along the write that creates the version (`PutObject`,
//! `CopyObject`, `CompleteMultipartUpload`) across the same versioning states.

use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::router;
use super::bucket_tagging::tag;
use super::kms_key_references::copy_object;
use super::multipart_upload_completion::{complete, completion_payload, initiate, upload_part};
use super::object_acl::{get_object_acl, put_object_acl};
use super::object_and_tagging_roundtrip::{bucket, get_object, put_object, text};
use super::object_attributes::get_object_attributes;
use super::object_subresource_version_id::{acls_enabled, SubresourceAnswer};
use super::object_tag_set_removal::delete_object_tagging;
use super::object_tagging::{get_object_tagging, put_object_tagging};
use super::object_versioning::{head_object, set_versioning};

/// The header every object operation's `VersionId` member is bound to
/// (`smithy_query(query="operation",operation="GetObject",
/// model_path="services/service-com-amazonaws-s3-amazons3/smithy-model/com-amazonaws-s3-amazons3.json")`).
const VERSION_ID_HEADER: &str = "x-amz-version-id";

/// The id of the version a bucket stores while its versioning status is not `Enabled`: "All
/// objects added to the bucket receive the version ID null" (`PutBucketVersioning`).
const NULL_VERSION_ID: &str = "null";

/// The bytes every object here is written with: which id a version is addressed by has
/// nothing to do with what it holds, so one payload serves every row.
const BODY: &[u8] = b"null-version-subject";

/// The key every row writes its object at.
const KEY: &str = "versions/subject.bin";

/// One row of the versioning-state table: the case, the bucket it stands up, the statuses
/// applied in order — the LAST write always being made while the status is not `Enabled`,
/// which is what stores the null version — and the id that version is addressed by.
struct StateRow {
    case: &'static str,
    bucket: &'static str,
    /// The statuses applied before the write that stores the null version.
    before: &'static [&'static str],
    /// The statuses applied AFTER it, so a row can leave the bucket in a state its stored
    /// null version was not written under.
    after: &'static [&'static str],
    /// What every answer about that version must name it by.
    addressed: Option<&'static str>,
}

fn state_rows() -> Vec<StateRow> {
    vec![
        StateRow {
            case: "a bucket whose versioning was never set",
            bucket: "null-version-plain",
            before: &[],
            after: &[],
            addressed: None,
        },
        StateRow {
            case: "a bucket suspended from the start",
            bucket: "null-version-suspended",
            before: &["Suspended"],
            after: &[],
            addressed: Some(NULL_VERSION_ID),
        },
        StateRow {
            case: "a bucket enabled and then suspended",
            bucket: "null-version-resuspended",
            before: &["Enabled", "Suspended"],
            after: &[],
            addressed: Some(NULL_VERSION_ID),
        },
        StateRow {
            case: "a bucket suspended for the write and enabled again after it",
            bucket: "null-version-reenabled",
            before: &["Suspended"],
            after: &["Enabled"],
            addressed: Some(NULL_VERSION_ID),
        },
    ]
}

/// The one attribute every object has, which is what the attributes read below asks for
/// (`smithy_query(query="shape",shape="ObjectAttributes")` lists it first).
const ETAG_ATTRIBUTE: &str = "ETag";

/// `GET /<bucket>/<key>?attributes<query>` — `GetObjectAttributes`, driven through the suite
/// that owns that sub-resource ([`super::object_attributes`]).
async fn object_attributes(
    router: &MultiServiceRouter,
    bucket: &str,
    key: &str,
    query: &str,
) -> super::bucket_lifecycle::Answer {
    get_object_attributes(router, bucket, key, query, &[ETAG_ATTRIBUTE]).await
}

/// Stand `row` up: a bucket with ACLs enabled, the statuses `before` applied in order, an
/// object written while the last of them is in force, and then the statuses `after`.
///
/// The write is asserted to generate no version id — rule 2 for the state this row is in —
/// so what every answer below names is the id the stored version is ADDRESSED by rather than
/// an id the write handed out.
async fn stand_up(router: &MultiServiceRouter, row: &StateRow) {
    let case = row.case;
    bucket(router, row.bucket).await;
    acls_enabled(router, row.bucket).await;
    for status in row.before {
        set_versioning(router, row.bucket, status).await;
    }
    let written = put_object(router, row.bucket, KEY, BODY, &[]).await;
    assert_eq!(
        written.status, 200,
        "case {case:?}: the object must be stored first, got {} with body {:?}",
        written.status,
        text(&written)
    );
    assert_eq!(
        written.header(VERSION_ID_HEADER),
        None,
        "case {case:?}: a write made while the bucket is not versioning-enabled generates no \
         version id, got {:?}",
        written.header(VERSION_ID_HEADER)
    );
    for status in row.after {
        set_versioning(router, row.bucket, status).await;
    }
}

/// Every answer that reports the version it resolved at the address `object_query` /
/// `subresource_query` names — the two spellings of the same address, the sub-resource and
/// attribute calls carrying it after a query literal of their own (`?tagging`, `?acl`,
/// `?attributes`) and the object reads carrying it as the whole query.
///
/// `PutObjectTagging` takes no `versionId` here (naming one is a scaffolded branch of that
/// operation, pinned by [`super::object_tagging`]), so it always acts on the key's current
/// version — which is the version every row's write stored.
async fn answers_about(
    router: &MultiServiceRouter,
    name: &str,
    object_query: &str,
    subresource_query: &str,
) -> Vec<SubresourceAnswer> {
    vec![
        SubresourceAnswer {
            operation: "GetObject",
            expected_status: 200,
            answer: get_object(router, name, KEY, object_query, &[]).await,
        },
        SubresourceAnswer {
            operation: "HeadObject",
            expected_status: 200,
            answer: head_object(router, name, KEY, object_query).await,
        },
        SubresourceAnswer {
            operation: "GetObjectAttributes",
            expected_status: 200,
            answer: object_attributes(router, name, KEY, subresource_query).await,
        },
        SubresourceAnswer {
            operation: "PutObjectTagging",
            expected_status: 200,
            answer: put_object_tagging(router, name, KEY, &[tag("owner", "null-version-suite")])
                .await,
        },
        SubresourceAnswer {
            operation: "GetObjectTagging",
            expected_status: 200,
            answer: get_object_tagging(router, name, KEY, subresource_query).await,
        },
        SubresourceAnswer {
            operation: "DeleteObjectTagging",
            expected_status: 204,
            answer: delete_object_tagging(router, name, KEY, subresource_query, &[]).await,
        },
        SubresourceAnswer {
            operation: "PutObjectAcl",
            expected_status: 200,
            answer: put_object_acl(
                router,
                name,
                KEY,
                subresource_query,
                &[("x-amz-acl", "private")],
                "",
            )
            .await,
        },
        SubresourceAnswer {
            operation: "GetObjectAcl",
            expected_status: 200,
            answer: get_object_acl(router, name, KEY, subresource_query, &[]).await,
        },
    ]
}

/// RULE 1: see the module header — every answer about the version it resolved names that
/// version by the id it is addressed by.
#[tokio::test]
async fn every_answer_about_the_null_version_names_it_by_the_id_it_is_addressed_by() {
    let router = router();

    for row in state_rows() {
        stand_up(&router, &row).await;
        let expected = row.addressed.map(str::to_string);

        // The two ways a request reaches that version: by taking the key's current one, and
        // by naming `null` outright — the id a request addresses it by, which is the id the
        // answers must report.
        for (address, object_query, subresource_query) in [
            ("the key's current version", "", ""),
            ("versionId=null", "?versionId=null", "&versionId=null"),
        ] {
            for SubresourceAnswer {
                operation,
                expected_status,
                answer,
            } in answers_about(&router, row.bucket, object_query, subresource_query).await
            {
                let case = format!("{} / {address}", row.case);
                assert_eq!(
                    answer.status, expected_status,
                    "case {case:?}/{operation}: expected HTTP {expected_status}, got {} with \
                     body {:?}",
                    answer.status,
                    text(&answer)
                );
                assert_eq!(
                    answer.header(VERSION_ID_HEADER),
                    expected,
                    "case {case:?}/{operation}: the version resolved is addressed by \
                     {expected:?}, got {:?}",
                    answer.header(VERSION_ID_HEADER)
                );
            }
        }

        // The id reported really does address that version: a read naming it answers the
        // bytes the write stored. A row reporting none has no id to name, and the key itself
        // still reads — which is what makes the absent header "this bucket addresses no
        // version" rather than "there is no object".
        let query = match &expected {
            Some(version_id) => format!("?versionId={version_id}"),
            None => String::new(),
        };
        let read = get_object(&router, row.bucket, KEY, &query, &[]).await;
        assert_eq!(
            read.status, 200,
            "case {:?}: the version the answers named ({expected:?}) must read back, got {} \
             with body {:?}",
            row.case,
            read.status,
            text(&read)
        );
        assert_eq!(
            read.body.as_ref(),
            BODY,
            "case {:?}: that version must hold the bytes the write stored",
            row.case
        );
    }
}

/// One row of the write table: the case, the bucket it stands up, the versioning status that
/// bucket is left in before the write (`None` for a bucket no status was ever set on), and
/// whether that bucket GENERATES a version id for the object being stored.
struct WriteRow {
    case: &'static str,
    bucket: &'static str,
    status: Option<&'static str>,
    generates_an_id: bool,
}

fn write_rows() -> Vec<WriteRow> {
    vec![
        WriteRow {
            case: "a bucket whose versioning was never set",
            bucket: "null-version-write-plain",
            status: None,
            generates_an_id: false,
        },
        WriteRow {
            case: "a versioning-suspended bucket",
            bucket: "null-version-write-suspended",
            status: Some("Suspended"),
            generates_an_id: false,
        },
        WriteRow {
            case: "a versioning-enabled bucket",
            bucket: "null-version-write-enabled",
            status: Some("Enabled"),
            generates_an_id: true,
        },
    ]
}

/// Assert what a write's `x-amz-version-id` reports for `row`: an id of the bucket's own
/// when it generates one, and nothing at all when it does not.
fn assert_generated(case: &str, operation: &str, generated: Option<String>, generates_an_id: bool) {
    if generates_an_id {
        assert!(
            generated
                .as_deref()
                .is_some_and(|id| !id.is_empty() && id != NULL_VERSION_ID),
            "case {case:?}/{operation}: a versioning-enabled bucket generates a version id of \
             its own, got {generated:?}"
        );
    } else {
        assert_eq!(
            generated, None,
            "case {case:?}/{operation}: a bucket that generates no version id for the object \
             being stored reports none, got {generated:?}"
        );
    }
}

/// RULE 2: see the module header — a write names only the id the bucket generated for the
/// object being stored, whichever of the three writes created it.
#[tokio::test]
async fn a_write_names_only_the_version_id_the_bucket_generated() {
    let router = router();
    // One source object, in a bucket of its own, for the copy rows to read.
    let source = "null-version-write-source";
    bucket(&router, source).await;
    let stored = put_object(&router, source, KEY, BODY, &[]).await;
    assert_eq!(
        stored.status, 200,
        "the copy source must be stored first, got {} with body {:?}",
        stored.status,
        text(&stored)
    );

    for row in write_rows() {
        let case = row.case;
        let generates_an_id = row.generates_an_id;
        bucket(&router, row.bucket).await;
        if let Some(status) = row.status {
            set_versioning(&router, row.bucket, status).await;
        }

        let put = put_object(&router, row.bucket, KEY, BODY, &[]).await;
        assert_eq!(
            put.status, 200,
            "case {case:?}/PutObject: expected HTTP 200, got {} with body {:?}",
            put.status,
            text(&put)
        );
        assert_generated(
            case,
            "PutObject",
            put.header(VERSION_ID_HEADER),
            generates_an_id,
        );

        let copied = copy_object(
            &router,
            row.bucket,
            "copied.bin",
            &format!("/{source}/{KEY}"),
            &[],
        )
        .await;
        assert_eq!(
            copied.status, 200,
            "case {case:?}/CopyObject: expected HTTP 200, got {} with body {:?}",
            copied.status,
            text(&copied)
        );
        assert_generated(
            case,
            "CopyObject",
            copied.header(VERSION_ID_HEADER),
            generates_an_id,
        );

        let upload_id = initiate(&router, row.bucket, "assembled.bin").await;
        let etag = upload_part(&router, row.bucket, "assembled.bin", &upload_id, 1, BODY).await;
        let completed = complete(
            &router,
            row.bucket,
            "assembled.bin",
            &upload_id,
            &completion_payload(&[(1, etag)]),
            &[],
        )
        .await;
        assert_eq!(
            completed.status, 200,
            "case {case:?}/CompleteMultipartUpload: expected HTTP 200, got {} with body {:?}",
            completed.status,
            text(&completed)
        );
        assert_generated(
            case,
            "CompleteMultipartUpload",
            completed.header(VERSION_ID_HEADER),
            generates_an_id,
        );
    }
}

/// RULE 1 for the SOURCE of a copy: "the version of the source object that was copied, if
/// you have enabled versioning on the source bucket"
/// (`CopyObjectOutput$CopySourceVersionId`) — the id the version the copy read is addressed
/// by, which is the `null` one for a source bucket carrying a versioning configuration that
/// is not `Enabled`, and none at all for a source bucket carrying none.
#[tokio::test]
async fn a_copy_names_the_source_version_by_the_id_it_is_addressed_by() {
    let router = router();
    let destination = "copy-source-null-destination";
    bucket(&router, destination).await;

    for row in state_rows() {
        let case = row.case;
        stand_up(&router, &row).await;
        let copied = copy_object(
            &router,
            destination,
            &format!("{}/copied.bin", row.bucket),
            &format!("/{}/{KEY}", row.bucket),
            &[],
        )
        .await;
        assert_eq!(
            copied.status, 200,
            "case {case:?}: the copy must be served, got {} with body {:?}",
            copied.status,
            text(&copied)
        );
        assert_eq!(
            copied.header("x-amz-copy-source-version-id").as_deref(),
            row.addressed,
            "case {case:?}: the source version copied is addressed by {:?}, got {:?}",
            row.addressed,
            copied.header("x-amz-copy-source-version-id")
        );
    }
}
