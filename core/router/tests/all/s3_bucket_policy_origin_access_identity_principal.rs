// SPDX-License-Identifier: CC0-1.0

//! What a bucket policy naming a CLOUDFRONT ORIGIN ACCESS IDENTITY is resolved against, over
//! the two services on one engine.
//!
//! RULE: `PutBucketPolicy` resolves every identity its document grants to against the records
//! of the service that OWNS that identity, as they stand when the call is made, and refuses a
//! document naming one that is not held with `MalformedPolicy` (HTTP 400) and
//! `Invalid principal in policy`, leaving the policy the bucket already carried exactly as it
//! was. A CloudFront origin access identity is such an identity: it is named as an IAM user of
//! CLOUDFRONT's own account — `arn:aws:iam::cloudfront:user/CloudFront Origin Access Identity
//! <id>`, the spelling the Amazon S3 User Guide's origin-access-identity bucket policy carries
//! — so the value can name nothing else, and the only record of one is the CloudFront store.
//! A grant to an identity nothing minted would otherwise be stored as a grant to nobody, and
//! the deployment that wrote it would look like it had succeeded, with the misconfiguration
//! surfacing only as refused fetches at the edge.
//!
//! Only a cross-service suite can pin this: the identity is CloudFront's record, the document
//! that names it is Amazon S3's, and the rule is observable only where both are mounted on one
//! engine. `cloudfront_oai_s3_origin_edge` beside it pins what such a grant then ADMITS at the
//! edge; this one pins which documents are stored at all.
//!
//! The first table's rows span what the rule generalises over: WHICH identity the document
//! names (one the account holds, one it minted and then deleted, an id of the documented form
//! nothing ever minted), the PARTITION the principal ARN is written in, the FORM the element
//! names it in (alone, one entry of a list beside a value that always resolves, the
//! complement element of a `Deny`), and the other spelling AWS publishes for an identity (the
//! `S3CanonicalUserId`, a value that names an account owner of any account just as readily, so
//! nothing in it says it was meant as an identity and it resolves).

use std::sync::Arc;

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use super::cloudfront_oac_s3_origin_edge::{
    call, create_bucket, emulator, regional_endpoint, Answer, API_VERSION, CLOUDFRONT_HOST,
};

/// The code Amazon S3 refuses a document it cannot resolve an identity of with (Amazon S3
/// "Error responses"), and the status that list carries for it.
const MALFORMED_POLICY: &str = "MalformedPolicy";
/// See [`MALFORMED_POLICY`].
const MALFORMED_POLICY_STATUS: u16 = 400;
/// The message S3 words that refusal with: fixed text in full — the value that did not resolve
/// is not quoted back. Written out here rather than imported from the service, so the wording
/// has something to be wrong about.
const INVALID_PRINCIPAL: &str = "Invalid principal in policy";

/// The status a stored bucket policy answers (`PutBucketPolicy` carries no body).
const STORED_STATUS: u16 = 204;

/// An origin access identity id of the form `CloudFrontOriginAccessIdentity$Id` documents by
/// example ("E74FTE3AJFJ256A") that no create in this suite produces: the account holds no
/// identity under it, and the rows that name it assert so against the ids it really minted.
const NEVER_MINTED_ID: &str = "E74FTE3AJFJ256A";

/// The account every emulated ARN of this emulator carries, whose root is the value each
/// row's baseline document grants to and the one that always resolves.
fn account_root() -> String {
    format!("arn:aws:iam::{}:root", engine::emulator_account_id())
}

/// The IAM principal an origin access identity is named by in a bucket policy, in
/// `partition`, as the Amazon S3 User Guide writes it.
fn identity_principal(partition: &str, id: &str) -> String {
    format!("arn:{partition}:iam::cloudfront:user/CloudFront Origin Access Identity {id}")
}

/// A one-statement bucket policy whose `element` of `effect` names `principal` under the
/// principal kind `kind`, granting `s3:GetObject` over `bucket`'s objects.
fn document(
    sid: &str,
    effect: &str,
    element: &str,
    kind: &str,
    principal: &str,
    bucket: &str,
) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\
         \"Sid\":\"{sid}\",\"Effect\":\"{effect}\",\
         \"{element}\":{{\"{kind}\":\"{principal}\"}},\
         \"Action\":\"s3:GetObject\",\"Resource\":\"arn:aws:s3:::{bucket}/*\"}}]}}"
    )
}

/// The same document, naming `principal` as ONE ENTRY of a list whose other entry — the
/// account's own root — always resolves: so the document stands or falls on `principal` alone.
fn document_in_a_list(sid: &str, principal: &str, bucket: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\
         \"Sid\":\"{sid}\",\"Effect\":\"Allow\",\
         \"Principal\":{{\"AWS\":[\"{principal}\",\"{root}\"]}},\
         \"Action\":\"s3:GetObject\",\"Resource\":\"arn:aws:s3:::{bucket}/*\"}}]}}",
        root = account_root()
    )
}

/// The document a row's bucket carries BEFORE the row's own write: a grant to the account's
/// own root, which always resolves. What a refusal has to leave exactly as it was.
fn baseline(bucket: &str) -> String {
    document(
        "Baseline",
        "Allow",
        "Principal",
        "AWS",
        &account_root(),
        bucket,
    )
}

/// `cloudfront:CreateCloudFrontOriginAccessIdentity`, answering the identity's
/// `(Id, S3CanonicalUserId, ETag)` — the `ETag` being the version token its delete has to
/// present.
async fn create_origin_access_identity(
    router: &MultiServiceRouter,
    case: &str,
    caller_reference: &str,
) -> (String, String, String) {
    let body = format!(
        "<CloudFrontOriginAccessIdentityConfig \
         xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Comment>an identity a bucket policy names</Comment>\
         </CloudFrontOriginAccessIdentityConfig>"
    );
    let created = call(
        router,
        "cloudfront",
        CLOUDFRONT_HOST,
        "POST",
        &format!("/{API_VERSION}/origin-access-identity/cloudfront"),
        &[("content-type", "application/xml")],
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateCloudFrontOriginAccessIdentity must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    let (id, canonical, etag) = (
        created.element("Id"),
        created.element("S3CanonicalUserId"),
        created.header("etag"),
    );
    assert!(
        !id.is_empty() && !canonical.is_empty() && !etag.is_empty(),
        "case {case:?}: the created identity reports no Id, S3CanonicalUserId or ETag: {:?}",
        created.text()
    );
    (id, canonical, etag)
}

/// `cloudfront:DeleteCloudFrontOriginAccessIdentity` under the identity's own version token,
/// so the account holds it no more.
async fn delete_origin_access_identity(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
    etag: &str,
) {
    let deleted = call(
        router,
        "cloudfront",
        CLOUDFRONT_HOST,
        "DELETE",
        &format!("/{API_VERSION}/origin-access-identity/cloudfront/{id}"),
        &[("if-match", etag)],
        Bytes::new(),
    )
    .await;
    assert_eq!(
        deleted.status,
        204,
        "case {case:?}: DeleteCloudFrontOriginAccessIdentity({id:?}) must succeed, got {} with \
         {:?}",
        deleted.status,
        deleted.text()
    );
}

/// `s3:PutBucketPolicy`, answering what it said rather than asserting success.
async fn put_bucket_policy(router: &MultiServiceRouter, bucket: &str, policy: &str) -> Answer {
    call(
        router,
        "s3",
        &regional_endpoint(),
        "PUT",
        &format!("/{bucket}?policy"),
        &[("content-type", "application/json")],
        Bytes::from(policy.to_string()),
    )
    .await
}

/// `s3:GetBucketPolicy` — the document the bucket carries, byte for byte.
async fn get_bucket_policy(router: &MultiServiceRouter, bucket: &str) -> Answer {
    call(
        router,
        "s3",
        &regional_endpoint(),
        "GET",
        &format!("/{bucket}?policy"),
        &[],
        Bytes::new(),
    )
    .await
}

/// Store the baseline document on `bucket` and answer it, so each row's own write runs against
/// a bucket already carrying a policy.
async fn store_baseline(router: &MultiServiceRouter, case: &str, bucket: &str) -> String {
    let document = baseline(bucket);
    let stored = put_bucket_policy(router, bucket, &document).await;
    assert_eq!(
        stored.status, STORED_STATUS,
        "case {case:?}: the baseline policy must be stored, got {} with {:?}",
        stored.status,
        stored.text()
    );
    document
}

/// WHICH principal a row's document names, and in which form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Named {
    /// The IAM principal of an identity the account holds, in the given partition.
    HeldIdentity { partition: &'static str },
    /// The IAM principal of an identity the account held and then DELETED.
    DeletedIdentity,
    /// The IAM principal of an id of the documented form nothing ever minted, in the given
    /// partition.
    NeverMinted { partition: &'static str },
    /// That same unminted principal as one entry of a list beside the account's own root.
    NeverMintedInAList,
    /// That same unminted principal as the COMPLEMENT element of a `Deny` statement.
    NeverMintedAsTheComplement,
    /// The `S3CanonicalUserId` of an identity the account holds — the other spelling AWS
    /// publishes for one.
    HeldIdentityCanonicalUser,
    /// A canonical user id nothing in this emulator minted: a value that names an account
    /// owner of any account as readily as an identity of this one.
    UnmintedCanonicalUser,
}

/// One row: the bucket it writes, the principal its document names, and whether the document
/// is stored.
struct Row {
    case: &'static str,
    /// This row's own bucket, so one row's document cannot decide another's read.
    bucket: &'static str,
    named: Named,
    /// Whether `PutBucketPolicy` stores the document.
    stored: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "the IAM principal of an identity the account holds",
        bucket: "oai-principal-held",
        named: Named::HeldIdentity { partition: "aws" },
        stored: true,
    },
    Row {
        case: "the same identity named in another partition",
        bucket: "oai-principal-held-partition",
        named: Named::HeldIdentity {
            partition: "aws-cn",
        },
        stored: true,
    },
    Row {
        case: "an identity the account minted and then deleted",
        bucket: "oai-principal-deleted",
        named: Named::DeletedIdentity,
        stored: false,
    },
    Row {
        case: "an identity id nothing ever minted",
        bucket: "oai-principal-never-minted",
        named: Named::NeverMinted { partition: "aws" },
        stored: false,
    },
    Row {
        case: "an identity id nothing ever minted, in another partition",
        bucket: "oai-principal-never-minted-partition",
        named: Named::NeverMinted {
            partition: "aws-us-gov",
        },
        stored: false,
    },
    Row {
        case: "an unminted identity beside a principal that resolves",
        bucket: "oai-principal-never-minted-list",
        named: Named::NeverMintedInAList,
        stored: false,
    },
    Row {
        case: "an unminted identity as the complement of a Deny",
        bucket: "oai-principal-never-minted-complement",
        named: Named::NeverMintedAsTheComplement,
        stored: false,
    },
    Row {
        case: "the canonical user id of an identity the account holds",
        bucket: "oai-principal-canonical",
        named: Named::HeldIdentityCanonicalUser,
        stored: true,
    },
    Row {
        case: "a canonical user id nothing minted",
        bucket: "oai-principal-canonical-unminted",
        named: Named::UnmintedCanonicalUser,
        stored: true,
    },
];

/// RULE: a bucket policy naming a CloudFront origin access identity is stored only while that
/// identity is one CloudFront holds; one naming an identity it does not hold is refused 400
/// `MalformedPolicy` / `Invalid principal in policy` and leaves the stored policy untouched.
#[tokio::test]
async fn a_bucket_policy_resolves_an_origin_access_identity_against_cloudfront() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let emulator = emulator(&engine, false);

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        create_bucket(&emulator, case, row.bucket).await;
        let held = store_baseline(&emulator, case, row.bucket).await;

        // The identity each row names, minted through CloudFront's own API: the record that
        // decides the write is one a client really created.
        let (id, canonical, etag) = create_origin_access_identity(
            &emulator,
            case,
            &format!("oai-principal-identity-{index}"),
        )
        .await;
        assert_ne!(
            id, NEVER_MINTED_ID,
            "case {case:?}: the minted id must differ from the one no create produces"
        );

        let document = match row.named {
            Named::HeldIdentity { partition } => document(
                "NamesTheIdentity",
                "Allow",
                "Principal",
                "AWS",
                &identity_principal(partition, &id),
                row.bucket,
            ),
            Named::DeletedIdentity => {
                delete_origin_access_identity(&emulator, case, &id, &etag).await;
                document(
                    "NamesTheIdentity",
                    "Allow",
                    "Principal",
                    "AWS",
                    &identity_principal("aws", &id),
                    row.bucket,
                )
            }
            Named::NeverMinted { partition } => document(
                "NamesTheIdentity",
                "Allow",
                "Principal",
                "AWS",
                &identity_principal(partition, NEVER_MINTED_ID),
                row.bucket,
            ),
            Named::NeverMintedInAList => document_in_a_list(
                "NamesTheIdentity",
                &identity_principal("aws", NEVER_MINTED_ID),
                row.bucket,
            ),
            Named::NeverMintedAsTheComplement => document(
                "NamesTheIdentity",
                "Deny",
                "NotPrincipal",
                "AWS",
                &identity_principal("aws", NEVER_MINTED_ID),
                row.bucket,
            ),
            Named::HeldIdentityCanonicalUser => document(
                "NamesTheIdentity",
                "Allow",
                "Principal",
                "CanonicalUser",
                &canonical,
                row.bucket,
            ),
            Named::UnmintedCanonicalUser => document(
                "NamesTheIdentity",
                "Allow",
                "Principal",
                "CanonicalUser",
                // The canonical user id form: 64 hex characters, as the identity's own
                // `S3CanonicalUserId` is written.
                &"a".repeat(canonical.len()),
                row.bucket,
            ),
        };

        let answer = put_bucket_policy(&emulator, row.bucket, &document).await;
        if row.stored {
            assert_eq!(
                answer.status,
                STORED_STATUS,
                "case {case:?}: the document must be stored, got {} with {:?}",
                answer.status,
                answer.text()
            );
        } else {
            assert_eq!(
                (
                    answer.status,
                    answer.element("Code"),
                    answer.element("Message")
                ),
                (
                    MALFORMED_POLICY_STATUS,
                    MALFORMED_POLICY.to_string(),
                    INVALID_PRINCIPAL.to_string()
                ),
                "case {case:?}: the document must be refused, got {} with {:?}",
                answer.status,
                answer.text()
            );
        }

        let read = get_bucket_policy(&emulator, row.bucket).await;
        let expected = if row.stored { &document } else { &held };
        assert_eq!(
            (read.status, read.text().as_str()),
            (200, expected.as_str()),
            "case {case:?}: expected the bucket to carry {expected:?}, got {} with {:?}",
            read.status,
            read.text()
        );
    }
}

/// RULE, the lifecycle half: the SAME document is stored while the identity it names exists
/// and refused once that identity is deleted — so the refusal is attributable to the
/// identity's absence alone, and not to the ARN text, a cache or a denylist.
#[tokio::test]
async fn the_same_document_is_refused_once_the_identity_it_names_is_deleted() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let emulator = emulator(&engine, false);
    let case = "one document across the identity's lifetime";
    let bucket = "oai-principal-lifecycle";
    create_bucket(&emulator, case, bucket).await;

    let (id, _canonical, etag) =
        create_origin_access_identity(&emulator, case, "oai-principal-lifecycle").await;
    let names_the_identity = document(
        "NamesTheIdentity",
        "Allow",
        "Principal",
        "AWS",
        &identity_principal("aws", &id),
        bucket,
    );

    // Leg 1: the identity exists, so the document naming it is stored and read back whole.
    let accepted = put_bucket_policy(&emulator, bucket, &names_the_identity).await;
    assert_eq!(
        accepted.status,
        STORED_STATUS,
        "case {case:?}: a document naming the live identity {id:?} must be stored, got {} with \
         {:?}",
        accepted.status,
        accepted.text()
    );
    let read = get_bucket_policy(&emulator, bucket).await;
    assert_eq!(
        read.text(),
        names_the_identity,
        "case {case:?}: the stored document must be reported byte for byte, got {:?}",
        read.text()
    );

    // A DIFFERENT document is put back, so the refusal below has something of its own to
    // leave alone.
    let held = store_baseline(&emulator, case, bucket).await;

    // Leg 2: the identity is deleted, and the byte-identical document is refused.
    delete_origin_access_identity(&emulator, case, &id, &etag).await;
    let refused = put_bucket_policy(&emulator, bucket, &names_the_identity).await;
    assert_eq!(
        (
            refused.status,
            refused.element("Code"),
            refused.element("Message")
        ),
        (
            MALFORMED_POLICY_STATUS,
            MALFORMED_POLICY.to_string(),
            INVALID_PRINCIPAL.to_string()
        ),
        "case {case:?}: the same document must be refused once {id:?} is deleted, got {} with \
         {:?}",
        refused.status,
        refused.text()
    );
    let read = get_bucket_policy(&emulator, bucket).await;
    assert_eq!(
        read.text(),
        held,
        "case {case:?}: the refused write must leave the stored policy as it was, got {:?}",
        read.text()
    );
}
