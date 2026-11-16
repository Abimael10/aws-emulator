// SPDX-License-Identifier: CC0-1.0

//! What a directory bucket's SESSION decides about the writes made under it, and what an
//! in-progress multipart upload occupies in such a bucket's listing namespace.
//!
//! Every assertion drives a real HTTP request through the router (the harness, the
//! directory-bucket create, the object writer and the KMS-backed engine are the ones
//! [`super::bucket_lifecycle`], [`super::directory_bucket_sessions_and_appends`],
//! [`super::object_and_tagging_roundtrip`] and [`super::kms_key_references`] already own), so
//! what is pinned is the wire behaviour — status, bound headers, error code and message, and
//! the state a later read serves.
//!
//! The rules under test, with their sources:
//!
//! * The ENCRYPTION of a Zonal endpoint write is the session's, both ways round: "in the Zonal
//!   endpoint API calls (except `CopyObject` and `UploadPartCopy`) … the encryption request
//!   headers must match the encryption settings that are specified in the `CreateSession`
//!   request. You can't override the values of the encryption settings. … You don't need to
//!   explicitly specify these encryption settings values in Zonal endpoint API calls, and
//!   Amazon S3 will use the encryption settings values from the `CreateSession` request to
//!   protect new objects in the directory bucket"
//!   (`smithy_query(query="doc",shape="PutObjectRequest",member="ServerSideEncryption")`) — so
//!   a write naming another algorithm than its session committed is refused `InvalidRequest`
//!   (400) with the sentence naming both algorithms and changes nothing, and a write naming no
//!   encryption member at all is stored under the session's own settings.
//! * "A `ReadOnly` session is constrained to execute the following Zonal endpoint API
//!   operations: `GetObject`, `HeadObject`, `ListObjectsV2`, `GetObjectAttributes`,
//!   `ListParts`, and `ListMultipartUploads`"
//!   (`smithy_query(query="doc",shape="CreateSessionRequest",member="SessionMode")`) — so every
//!   other operation under such a session is refused `AccessDenied` (403) and leaves the state
//!   a `ReadWrite` session put there exactly as it was.
//! * A directory bucket keeps its keys in a directory hierarchy, so the key of an in-progress
//!   multipart upload already occupies the bucket's listing namespace through the prefix it
//!   rolls up into — reported among `CommonPrefixes`
//!   (`smithy_query(query="operation",operation="ListObjectsV2")`) and never among `Contents`,
//!   because the key itself is no object until the upload completes.
//!
//! The sibling suite [`super::directory_bucket_session_constraints`] states the same three
//! rules over other rows — the published read set operation by operation, the mismatch over the
//! algorithms a session established on the bucket's default is written against, and the
//! in-flight prefix up to the ABORT that releases it. What this suite adds is the other side of
//! each: what the session's committed settings do to a write that names none, the mismatch under
//! a session committed to each of the other published algorithms and in the APPEND form of the
//! write, the state a refused mutation leaves behind, and the listing shapes an in-flight key
//! rolls up into (or does not) up to the COMPLETION that turns it into an object. The vend
//! helper and the two header constants are that suite's, reused rather than restated.

use service_com_amazonaws_s3_amazons3::service::express_session::SESSION_TOKEN_HEADER;
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{send, send_bytes, Answer};
use super::directory_bucket_session_constraints::{SESSION_MODE_HEADER, SSE_HEADER};
use super::directory_bucket_sessions_and_appends::directory_bucket;
use super::kms_key_references::{created_key, router_with_kms};
use super::multipart_upload_completion::completion_payload;
use super::object_and_tagging_roundtrip::{bucket, put_object, text};
use super::object_encryption::head_object;

/// `CreateSessionRequest$SSEKMSKeyId` and `PutObjectRequest$SSEKMSKeyId` both bind
/// `x-amz-server-side-encryption-aws-kms-key-id`
/// (`smithy_query(query="operation",operation="CreateSession,PutObject")`).
const KEY_ID_HEADER: &str = "x-amz-server-side-encryption-aws-kms-key-id";

/// `PutObjectRequest$WriteOffsetBytes` binds `x-amz-write-offset-bytes` — the header that makes
/// a write an APPEND (`smithy_query(query="operation",operation="PutObject")`).
const WRITE_OFFSET_HEADER: &str = "x-amz-write-offset-bytes";

/// `PutObjectOutput$Size` binds `x-amz-object-size`, "the size of the object in bytes … only
/// present if you append to an object" (same query).
const OBJECT_SIZE_HEADER: &str = "x-amz-object-size";

/// The two `ServerSideEncryption` values a directory bucket supports, plus the dual-layer KMS
/// value the same enum carries (`smithy_query(query="shape",shape="ServerSideEncryption")`
/// reports `AES256 | aws:backup | aws:fsx | aws:kms | aws:kms:dsse`).
const AES256: &str = "AES256";
/// See [`AES256`].
const KMS: &str = "aws:kms";
/// See [`AES256`].
const DSSE_KMS: &str = "aws:kms:dsse";

/// The bytes every seeded object in this suite starts as.
const SEED: &[u8] = b"seeded bytes";
/// The bytes every append in this suite adds.
const CHUNK: &[u8] = b" and more";

/// `GET /<bucket>?session` — `CreateSession` — and the token it vended.
///
/// `pub(crate)`: the ONE vend helper of this crate's directory-bucket session suites, over the
/// whole header set a vend varies along ([`super::directory_bucket_session_constraints`] takes
/// its mode-only vends through it).
pub(crate) async fn session_token(
    router: &MultiServiceRouter,
    bucket_name: &str,
    headers: &[(&str, &str)],
) -> String {
    let vended = send(router, "GET", &format!("/{bucket_name}?session"), headers, "").await;
    assert_eq!(
        vended.status,
        200,
        "a session on {bucket_name:?} with {headers:?} must be vended, got {} with body {:?}",
        vended.status,
        text(&vended)
    );
    let token = vended.element("SessionToken").unwrap_or_default();
    assert!(
        !token.is_empty(),
        "the vended session must carry a SessionToken, got {:?}",
        text(&vended)
    );
    token
}

/// `PUT /<bucket>/<key>` under the session `token`, naming `algorithm` when it names one and
/// appending at `offset` when the write is an append.
async fn write_under_session(
    router: &MultiServiceRouter,
    bucket_name: &str,
    key: &str,
    token: &str,
    algorithm: Option<&str>,
    offset: Option<usize>,
    body: &[u8],
) -> Answer {
    let offset = offset.map(|offset| offset.to_string());
    let mut headers: Vec<(&str, &str)> = vec![(SESSION_TOKEN_HEADER, token)];
    if let Some(algorithm) = algorithm {
        headers.push((SSE_HEADER, algorithm));
    }
    if let Some(offset) = &offset {
        headers.push((WRITE_OFFSET_HEADER, offset.as_str()));
    }
    send_bytes(
        router,
        "PUT",
        &format!("/{bucket_name}/{key}"),
        &headers,
        body,
    )
    .await
}

/// The sentence S3 refuses a write naming `requested` under a session established with
/// `committed` with: fixed text with the two algorithms substituted, each named the way the
/// published short name of its encryption type names it.
fn mismatch_sentence(committed: &str, requested: &str) -> String {
    format!(
        "Session was created with ServerSideEncryption as {committed} but request header \
         specifies {requested}."
    )
}

/// One row of the mismatch table: the case, the encryption headers the SESSION was created
/// with, the name that session's algorithm carries in the sentence, the algorithm the write
/// names, the name that one carries in the sentence, and whether the write appends.
struct MismatchRow<'a> {
    case: &'a str,
    session: Vec<(&'a str, &'a str)>,
    committed_named: &'a str,
    requested: &'a str,
    requested_named: &'a str,
    appends: bool,
}

/// RULE (the refusing half): a Zonal endpoint write whose `x-amz-server-side-encryption` names
/// another algorithm than its session committed is refused `InvalidRequest` (400) with the
/// sentence naming both algorithms, and leaves the object exactly as it was — while the
/// identical write naming the session's own algorithm is served.
///
/// The rows differ along WHICH algorithm each side names — the session's committed one is the
/// bucket's default, an explicitly named `AES256`, SSE-KMS and dual-layer SSE-KMS; the write's
/// is each of the other published values — and along the FORM of the write, an overwrite and an
/// append, because `x-amz-write-offset-bytes` is a Zonal-endpoint-only write that must be
/// judged by the same rule.
#[tokio::test]
async fn a_zonal_write_naming_another_encryption_than_its_session_is_refused_and_changes_nothing() {
    let router = router_with_kms();
    let (_, key_arn) = created_key(&router, "the key these sessions commit to").await;
    let bucket_name = "sessioncommit--use1-az4--x-s3";
    directory_bucket(&router, bucket_name, "use1-az4").await;

    let rows = [
        MismatchRow {
            case: "a session on the bucket's default, appending as SSE-KMS",
            session: Vec::new(),
            committed_named: AES256,
            requested: KMS,
            requested_named: "SSE-KMS",
            appends: true,
        },
        MismatchRow {
            case: "a session on AES256, overwriting as dual-layer SSE-KMS",
            session: vec![(SSE_HEADER, AES256)],
            committed_named: AES256,
            requested: DSSE_KMS,
            requested_named: "DSSE-KMS",
            appends: false,
        },
        MismatchRow {
            case: "a session on SSE-KMS, appending as AES256",
            session: vec![(SSE_HEADER, KMS), (KEY_ID_HEADER, key_arn.as_str())],
            committed_named: "SSE-KMS",
            requested: AES256,
            requested_named: AES256,
            appends: true,
        },
        MismatchRow {
            case: "a session on dual-layer SSE-KMS, overwriting as SSE-KMS",
            session: vec![(SSE_HEADER, DSSE_KMS), (KEY_ID_HEADER, key_arn.as_str())],
            committed_named: "DSSE-KMS",
            requested: KMS,
            requested_named: "SSE-KMS",
            appends: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let key = format!("committed-{index}.bin");
        let token = session_token(&router, bucket_name, &row.session).await;
        // The seed names no encryption of its own, so it is stored under the session's own
        // settings — which is what the refusal below is measured against.
        let seeded =
            write_under_session(&router, bucket_name, &key, &token, None, None, SEED).await;
        assert_eq!(
            seeded.status,
            200,
            "case {case:?}: the object must be seeded first, got {} with body {:?}",
            seeded.status,
            text(&seeded)
        );

        let offset = row.appends.then_some(SEED.len());
        let body: &[u8] = match row.appends {
            true => CHUNK,
            false => b"a later revision",
        };
        let refused = write_under_session(
            &router,
            bucket_name,
            &key,
            &token,
            Some(row.requested),
            offset,
            body,
        )
        .await;
        assert_eq!(
            (refused.status, refused.error_code()),
            (400, "InvalidRequest".to_string()),
            "case {case:?}: expected 400 InvalidRequest, got {} with body {:?}",
            refused.status,
            text(&refused)
        );
        assert_eq!(
            refused.error_message(),
            mismatch_sentence(row.committed_named, row.requested_named),
            "case {case:?}: the sentence must name both algorithms, got {:?}",
            refused.error_message()
        );

        // Nothing the refused write would have applied was applied: the object still holds the
        // seeded bytes alone.
        let read = send_bytes(&router, "GET", &format!("/{bucket_name}/{key}"), &[], b"").await;
        assert_eq!(
            (read.status, read.body.as_ref()),
            (200, SEED),
            "case {case:?}: a refused write must leave the object as it was, got {} with body \
             {:?}",
            read.status,
            text(&read)
        );

        // And the identical write naming the session's OWN algorithm is served, which is what
        // makes the refusal about the mismatch rather than about the write.
        let served = write_under_session(
            &router,
            bucket_name,
            &key,
            &token,
            row.session
                .iter()
                .find(|(name, _)| *name == SSE_HEADER)
                .map(|(_, value)| *value),
            offset,
            body,
        )
        .await;
        assert_eq!(
            served.status,
            200,
            "case {case:?}: the write naming the session's own algorithm must be served, got {} \
             with body {:?}",
            served.status,
            text(&served)
        );
        if row.appends {
            assert_eq!(
                served.header(OBJECT_SIZE_HEADER),
                Some((SEED.len() + CHUNK.len()).to_string()),
                "case {case:?}: the served append must report the object's new size, got {:?}",
                served.header(OBJECT_SIZE_HEADER)
            );
        }
    }
}

/// One row of the committed-settings table: the case, the encryption headers the SESSION was
/// created with, and the algorithm and KMS key every write under it must report.
struct CommittedRow<'a> {
    case: &'a str,
    session: Vec<(&'a str, &'a str)>,
    algorithm: &'a str,
    key_arn: Option<&'a str>,
}

/// RULE (the storing half): a Zonal endpoint write that names NO encryption member is stored
/// under the settings its session committed — "Amazon S3 will use the encryption settings
/// values from the `CreateSession` request to protect new objects in the directory bucket" —
/// which is what every answer about that object then reports, and what the multipart form of
/// the same write settles for the finished object too.
///
/// The rows differ along WHAT the session committed: nothing (so the bucket's own default
/// encryption configuration), an explicit `AES256`, SSE-KMS under a key KMS holds, and
/// dual-layer SSE-KMS under that same key. The control at the end is the same write under NO
/// session, which inherits the bucket's default — so what the rows pin is the session's doing.
#[tokio::test]
async fn a_zonal_write_naming_no_encryption_is_stored_under_its_sessions_committed_settings() {
    let router = router_with_kms();
    let (_, key_arn) = created_key(&router, "the key these sessions commit to").await;
    let bucket_name = "sessionsettings--use1-az4--x-s3";
    directory_bucket(&router, bucket_name, "use1-az4").await;

    let rows = [
        CommittedRow {
            case: "a session naming no encryption at all",
            session: Vec::new(),
            algorithm: AES256,
            key_arn: None,
        },
        CommittedRow {
            case: "a session naming AES256",
            session: vec![(SSE_HEADER, AES256)],
            algorithm: AES256,
            key_arn: None,
        },
        CommittedRow {
            case: "a session naming SSE-KMS under a key KMS holds",
            session: vec![(SSE_HEADER, KMS), (KEY_ID_HEADER, key_arn.as_str())],
            algorithm: KMS,
            key_arn: Some(key_arn.as_str()),
        },
        CommittedRow {
            case: "a session naming dual-layer SSE-KMS under that key",
            session: vec![(SSE_HEADER, DSSE_KMS), (KEY_ID_HEADER, key_arn.as_str())],
            algorithm: DSSE_KMS,
            key_arn: Some(key_arn.as_str()),
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let token = session_token(&router, bucket_name, &row.session).await;

        // The one-shot write.
        let key = format!("settings-{index}.bin");
        let written =
            write_under_session(&router, bucket_name, &key, &token, None, None, SEED).await;
        assert_reported(case, "PutObject", &written, row.algorithm, row.key_arn);
        // Read back through an implemented reader, so what is pinned is the STORED
        // configuration rather than one response's echo.
        let headed = head_object(&router, bucket_name, &key).await;
        assert_reported(case, "HeadObject", &headed, row.algorithm, row.key_arn);

        // And the multipart form of the same write, whose initiation "settles the options you
        // want on the finished object".
        let parted = format!("settings-{index}-parted.bin");
        let initiated = send_bytes(
            &router,
            "POST",
            &format!("/{bucket_name}/{parted}?uploads"),
            &[(SESSION_TOKEN_HEADER, token.as_str())],
            b"",
        )
        .await;
        assert_reported(
            case,
            "CreateMultipartUpload",
            &initiated,
            row.algorithm,
            row.key_arn,
        );
        let upload_id = initiated
            .element("UploadId")
            .unwrap_or_else(|| panic!("case {case:?}: an initiation must report an UploadId"));
        let part = send_bytes(
            &router,
            "PUT",
            &format!("/{bucket_name}/{parted}?partNumber=1&uploadId={upload_id}"),
            &[(SESSION_TOKEN_HEADER, token.as_str())],
            SEED,
        )
        .await;
        assert_eq!(
            part.status,
            200,
            "case {case:?}: the part must be uploaded, got {} with body {:?}",
            part.status,
            text(&part)
        );
        let etag = part
            .header("etag")
            .unwrap_or_else(|| panic!("case {case:?}: a part must report its ETag"));
        let completed = send(
            &router,
            "POST",
            &format!("/{bucket_name}/{parted}?uploadId={upload_id}"),
            &[(SESSION_TOKEN_HEADER, token.as_str())],
            &completion_payload(&[(1, etag)]),
        )
        .await;
        assert_reported(
            case,
            "CompleteMultipartUpload",
            &completed,
            row.algorithm,
            row.key_arn,
        );
    }

    // The control: the same write under NO session inherits the bucket's own default
    // encryption configuration, which is SSE-S3 for a bucket nothing configured.
    let unsessioned = put_object(&router, bucket_name, "unsessioned.bin", SEED, &[]).await;
    assert_reported(
        "a write under no session at all",
        "PutObject",
        &unsessioned,
        AES256,
        None,
    );
}

/// Assert `answer` was served and reports `algorithm` as the encryption it stored the object
/// under, and `key_arn` as the key that protects it.
fn assert_reported(
    case: &str,
    operation: &str,
    answer: &Answer,
    algorithm: &str,
    key_arn: Option<&str>,
) {
    assert_eq!(
        answer.status / 100,
        2,
        "case {case:?}: {operation} must be served, got {} with body {:?}",
        answer.status,
        text(answer)
    );
    assert_eq!(
        answer.header(SSE_HEADER).as_deref(),
        Some(algorithm),
        "case {case:?}: {operation} must report {algorithm:?} as the encryption, got {:?}",
        answer.header(SSE_HEADER)
    );
    assert_eq!(
        answer.header(KEY_ID_HEADER).as_deref(),
        key_arn,
        "case {case:?}: {operation} must report {key_arn:?} as the key, got {:?}",
        answer.header(KEY_ID_HEADER)
    );
}

/// One row of the read-only table: the case, the operation the call names, and the request it
/// is — method, the path and query under the bucket, the body, and whether the write appends
/// (which is what makes it carry `x-amz-write-offset-bytes`).
type MutatingCall = (
    &'static str,
    &'static str,
    &'static str,
    &'static [u8],
    bool,
);

/// RULE: a call under a `ReadOnly` session that names an operation outside the published set is
/// refused `AccessDenied` (403) with the sentence S3 words that refusal with, and NOTHING it
/// would have changed changes: the object keeps the bytes the `ReadWrite` session wrote and no
/// upload is registered — while the same call under a `ReadWrite` session is served.
///
/// The rows differ along WHICH mutation the call is: an overwrite, an append, a delete, a batch
/// delete and a multipart initiation — every shape of change a Zonal endpoint data plane makes,
/// rather than one of them.
#[tokio::test]
async fn a_mutation_refused_under_a_read_only_session_leaves_the_state_it_addressed_alone() {
    let router = router_with_kms();
    let bucket_name = "readonlystate--use1-az4--x-s3";
    let key = "subject.txt";
    directory_bucket(&router, bucket_name, "use1-az4").await;

    let read_write = session_token(&router, bucket_name, &[]).await;
    let read_only =
        session_token(&router, bucket_name, &[(SESSION_MODE_HEADER, "ReadOnly")]).await;
    let seeded = write_under_session(&router, bucket_name, key, &read_write, None, None, SEED)
        .await;
    assert_eq!(
        seeded.status,
        200,
        "the read-write session's write must be served, got {} with body {:?}",
        seeded.status,
        text(&seeded)
    );

    let offset = SEED.len().to_string();
    let batch_delete: &[u8] = br#"<Delete><Object><Key>subject.txt</Key></Object></Delete>"#;
    let rows: [MutatingCall; 5] = [
        ("PutObject", "PUT", "/subject.txt", b"overwritten", false),
        ("PutObject (appending)", "PUT", "/subject.txt", CHUNK, true),
        ("DeleteObject", "DELETE", "/subject.txt", b"", false),
        ("DeleteObjects", "POST", "?delete", batch_delete, false),
        (
            "CreateMultipartUpload",
            "POST",
            "/subject.txt?uploads",
            b"",
            false,
        ),
    ];

    for (operation, method, path, body, appends) in rows {
        let mut headers: Vec<(&str, &str)> = vec![(SESSION_TOKEN_HEADER, read_only.as_str())];
        if appends {
            headers.push((WRITE_OFFSET_HEADER, offset.as_str()));
        }
        let refused = send_bytes(
            &router,
            method,
            &format!("/{bucket_name}{path}"),
            &headers,
            body,
        )
        .await;
        assert_eq!(
            (
                refused.status,
                refused.error_code(),
                refused.error_message()
            ),
            (403, "AccessDenied".to_string(), "Access Denied".to_string()),
            "case {operation:?}: a read-only session must be refused this mutation, got {} with \
             body {:?}",
            refused.status,
            text(&refused)
        );

        // The state it addressed is untouched: the bytes the read-write session wrote are what
        // the key holds, and no upload was registered.
        let read = send_bytes(&router, "GET", &format!("/{bucket_name}/{key}"), &[], b"").await;
        assert_eq!(
            (read.status, read.body.as_ref()),
            (200, SEED),
            "case {operation:?}: the refused mutation must leave the object alone, got {} with \
             body {:?}",
            read.status,
            text(&read)
        );
        let uploads = send(&router, "GET", &format!("/{bucket_name}?uploads"), &[], "").await;
        assert!(
            uploads.elements("UploadId").is_empty(),
            "case {operation:?}: the refused mutation must register no upload, got {:?}",
            text(&uploads)
        );
    }

    // The same mutation under the READ-WRITE session is served and does change the object,
    // which is what makes every refusal above about the session's mode.
    let overwritten = write_under_session(
        &router,
        bucket_name,
        key,
        &read_write,
        None,
        None,
        b"overwritten",
    )
    .await;
    assert_eq!(
        overwritten.status,
        200,
        "the read-write session's overwrite must be served, got {} with body {:?}",
        overwritten.status,
        text(&overwritten)
    );
    let read = send_bytes(&router, "GET", &format!("/{bucket_name}/{key}"), &[], b"").await;
    assert_eq!(
        (read.status, read.body.as_ref()),
        (200, b"overwritten".as_ref()),
        "the read-write session's bytes are what the object then holds, got {} with body {:?}",
        read.status,
        text(&read)
    );
}

/// One row of the namespace table: the case, the listing's query under the bucket, the `Prefix`
/// the listing itself names, the common prefixes it must roll up and the keys it must report.
type ListingRow = (
    &'static str,
    &'static str,
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
);

/// The key in flight, and the settled object beside it.
const IN_FLIGHT_KEY: &str = "inflight/deep/big.bin";
/// See [`IN_FLIGHT_KEY`].
const SETTLED_KEY: &str = "settled/a.txt";

/// RULE: in a directory bucket the key of an in-progress multipart upload occupies the bucket's
/// listing namespace through the prefix it rolls up into — reported among `CommonPrefixes` and
/// never among `Contents` — and the key becomes an entry of the listing only once the upload
/// completes; a general purpose bucket, which has no such hierarchy, reports nothing for it.
///
/// The rows differ along the SHAPE of the listing: no delimiter at all, a delimiter at the root,
/// and a delimiter under each of the two prefixes the in-flight key lies beneath — so what is
/// pinned is the roll-up rule over the in-flight key rather than one listing, including the
/// listing that rolls it up into nothing and therefore reports nothing for it.
#[tokio::test]
async fn an_in_flight_uploads_key_occupies_a_directory_buckets_namespace_only_as_a_prefix() {
    let router = router_with_kms();
    let directory = "inflightspace--use1-az4--x-s3";
    let general_purpose = "inflight-space-general-purpose";
    directory_bucket(&router, directory, "use1-az4").await;
    bucket(&router, general_purpose).await;

    let rows: [ListingRow; 4] = [
        (
            "no delimiter at all",
            "?list-type=2",
            "",
            &[],
            &[SETTLED_KEY],
        ),
        (
            "a delimiter at the root",
            "?list-type=2&delimiter=%2F",
            "",
            &["inflight/", "settled/"],
            &[],
        ),
        (
            "a delimiter under the upload's first prefix",
            "?list-type=2&prefix=inflight%2F&delimiter=%2F",
            "inflight/",
            &["inflight/deep/"],
            &[],
        ),
        (
            "a delimiter under the directory holding the key itself",
            "?list-type=2&prefix=inflight%2Fdeep%2F&delimiter=%2F",
            "inflight/deep/",
            &[],
            &[],
        ),
    ];

    for (bucket_name, kind, in_flight_rolls_up) in [
        (directory, "a directory bucket", true),
        (general_purpose, "a general purpose bucket", false),
    ] {
        let settled = put_object(&router, bucket_name, SETTLED_KEY, SEED, &[]).await;
        assert_eq!(
            settled.status,
            200,
            "case {kind:?}: the settled object must be written, got {} with body {:?}",
            settled.status,
            text(&settled)
        );
        let initiated = send(
            &router,
            "POST",
            &format!("/{bucket_name}/{IN_FLIGHT_KEY}?uploads"),
            &[],
            "",
        )
        .await;
        assert_eq!(
            initiated.status,
            200,
            "case {kind:?}: the upload must be initiated, got {} with body {:?}",
            initiated.status,
            text(&initiated)
        );
        let upload_id = initiated
            .element("UploadId")
            .expect("an initiation must report an UploadId");

        for (case, query, named_prefix, rolled_up, keys) in rows {
            let listed = send(&router, "GET", &format!("/{bucket_name}{query}"), &[], "").await;
            assert_eq!(
                listed.status,
                200,
                "case {kind:?} / {case:?}: the listing must be served, got {} with body {:?}",
                listed.status,
                text(&listed)
            );
            // The response's own `Prefix` element comes first — the prefix this listing named —
            // and the rolled-up ones follow it. An in-flight upload rolls up into a prefix in a
            // directory bucket alone, so a general purpose bucket's expectation is the same
            // table with that prefix dropped.
            let mut expected = vec![named_prefix.to_string()];
            expected.extend(
                rolled_up
                    .iter()
                    .filter(|prefix| in_flight_rolls_up || !prefix.starts_with("inflight/"))
                    .map(|prefix| prefix.to_string()),
            );
            assert_eq!(
                listed.elements("Prefix"),
                expected,
                "case {kind:?} / {case:?}: expected the prefixes {expected:?}, got {:?}",
                text(&listed)
            );
            assert_eq!(
                listed.elements("Key"),
                keys.iter().map(|key| key.to_string()).collect::<Vec<_>>(),
                "case {kind:?} / {case:?}: an in-flight upload's key is no object, so only \
                 {keys:?} are entries, got {:?}",
                listed.elements("Key")
            );
        }

        // Completing the upload leaves an OBJECT at the key, so the key joins the listing as an
        // entry of its own — the prefix it rolled up into (where it rolled up into one at all)
        // outlives the upload because an object now occupies it.
        let part = send_bytes(
            &router,
            "PUT",
            &format!("/{bucket_name}/{IN_FLIGHT_KEY}?partNumber=1&uploadId={upload_id}"),
            &[],
            SEED,
        )
        .await;
        assert_eq!(
            part.status,
            200,
            "case {kind:?}: the part must be uploaded, got {} with body {:?}",
            part.status,
            text(&part)
        );
        let etag = part.header("etag").expect("a part must report its ETag");
        let completed = send(
            &router,
            "POST",
            &format!("/{bucket_name}/{IN_FLIGHT_KEY}?uploadId={upload_id}"),
            &[],
            &completion_payload(&[(1, etag)]),
        )
        .await;
        assert_eq!(
            completed.status,
            200,
            "case {kind:?}: the upload must complete, got {} with body {:?}",
            completed.status,
            text(&completed)
        );
        let after = send(
            &router,
            "GET",
            &format!("/{bucket_name}?list-type=2"),
            &[],
            "",
        )
        .await;
        assert_eq!(
            after.elements("Key"),
            vec![IN_FLIGHT_KEY.to_string(), SETTLED_KEY.to_string()],
            "case {kind:?}: a completed upload leaves an object at its key, got {:?}",
            text(&after)
        );
    }
}
