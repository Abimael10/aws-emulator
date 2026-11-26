// SPDX-License-Identifier: CC0-1.0

//! What the KEY a dialled CloudFront edge decrypts an origin object with is, and WHO the key's
//! policy decides the fetch for: CloudFront, Amazon S3 and KMS on one engine, read off a real
//! socket at the address the distribution published.
//!
//! RULE: the customer-managed KMS key an origin-access-control fetch has to be granted
//! `kms:Decrypt` on is the key the object was STORED under — which, for an object written with
//! no encryption header of its own, is the key the bucket's default encryption names, in
//! whichever form it names it (`ApplyServerSideEncryptionByDefault$KMSMasterKeyID` is "the
//! Amazon Web Services KMS key ID or the Amazon Resource Name (ARN) of the KMS key") — and the
//! subject that key's policy decides is the `cloudfront.amazonaws.com` SERVICE PRINCIPAL acting
//! for that distribution, so the decision follows the policy's `Principal` and `Effect` as well
//! as its scope: a statement naming another service principal reaches this fetch not at all,
//! and an explicit `Deny` beside a matching allow withholds it. Because that subject belongs to
//! the FETCH alone, the account's own direct read of the very same object — decided against the
//! credential it presents, which the key's own default statement delegates to the account — is
//! served whatever the policy said about CloudFront.
//!
//! Only a cross-service suite can state this: the published address and the edge are
//! CloudFront's, the bucket's default encryption and the object are Amazon S3's, the key and its
//! policy are KMS's, and the subject is the identity the three of them agree the fetch was made
//! as. What the siblings leave to it:
//! `cloudfront_dialled_edge_sse_kms_key_policy_decision` dials one published address while
//! rewriting the SCOPE of one grant to the CloudFront principal, over objects whose write named
//! their key itself, and asserts nothing about another principal, about an explicit `Deny`, or
//! about the account's own read; `cloudfront_oac_kms_s3_origin_edge` tables key-policy grants
//! over requests DISPATCHED in process, which says nothing about whether the published name is
//! an address a client can open a connection to; and
//! `cloudfront_dialled_edge_address_over_a_private_s3_origin` dials one over a BUCKET document
//! alone. Its helpers — the three services on one engine, the key, its policy documents and the
//! members of a dialled `<Error>` — are the first sibling's, and the read-back of a published
//! name is `cloudfront_spa_rewrite_at_the_dialled_edge_address`'s: reused rather than copied.
//!
//! The rows differ along what the rule generalises over: the form the bucket's DEFAULT
//! encryption names the key by (its ARN, its key id), the key's own shape (a bare name, a
//! nested prefix), and what the key policy says about the fetch's principal (a scoped allow, an
//! explicit `Deny` beside that allow, an allow to another service principal entirely, and — as
//! the control the sibling suite also keeps a beat for — a grant scoped to another
//! distribution's ARN). Each row has its own bucket and its own key, so no row's document can
//! decide another's read, and each is driven end to end: the published `DomainName` is held to
//! being an address this emulator opened, `GetDistribution` is held to reporting that same
//! address, the viewer DIALS it with no credential at all, and the account's own direct read of
//! the same object is read back beside the answer.
//!
//! Every expected value is computed from what the row declares. The distribution's id, ARN and
//! published address are CloudFront's to mint and the key's ARN and id are KMS's: what is
//! asserted about them is their FORM, that a read reports the same one, and what they decide.
//! The setup calls carry a credential scope (which is what routes them and what names the
//! Region) on a plane with request authentication off, as every suite in this crate drives its
//! setup; the viewer carries no credential and does not go through the router at all.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::cloudfront_dialled_edge_address_over_a_private_s3_origin::{dial, is_dialable, Served};
use super::cloudfront_dialled_edge_sse_kms_key_policy_decision::{
    another_distribution_arn, created_key, edge_grant, element, emulator_with_kms, key_policy_with,
    put_key_policy, service_grant, ACCESS_DENIED, ACCESS_DENIED_MESSAGE, ACCESS_DENIED_STATUS,
    ALGORITHM_HEADER, CACHING_DISABLED, DECRYPT_ACTION, MISS, SSE_KMS,
};
use super::cloudfront_oac_kms_s3_origin_edge::sse_kms_default_encryption_document;
use super::cloudfront_oac_s3_origin_edge::{
    awaited_deployment, call, create_bucket, create_distribution, create_origin_access_control,
    edge_listener_authority, is_published_edge_address, policy, put_bucket_policy,
    regional_endpoint, CLOUDFRONT_PRINCIPAL, REGION,
};
use super::cloudfront_spa_rewrite_at_the_dialled_edge_address::read_domain_name;

/// A service principal that is not the one an origin access control makes CloudFront's fetch
/// as: the principal the row granting elsewhere names.
const ANOTHER_SERVICE_PRINCIPAL: &str = "s3.amazonaws.com";

// ---------------------------------------------------------------------------
// The calls a row makes that its siblings do not
// ---------------------------------------------------------------------------

/// `s3:PutBucketEncryption`, making SSE-KMS under `key_reference` the bucket's DEFAULT — the
/// configuration an origin bucket holding encrypted objects carries, and what an object written
/// with no encryption header of its own is stored under. The document itself is the one every
/// suite sends ([`sse_kms_default_encryption_document`]).
async fn put_bucket_encryption(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    key_reference: &str,
) {
    let stored = call(
        router,
        "s3",
        &regional_endpoint(),
        "PUT",
        &format!("/{bucket}?encryption"),
        &[("content-type", "application/xml")],
        Bytes::from(sse_kms_default_encryption_document(key_reference)),
    )
    .await;
    assert!(
        (200..300).contains(&stored.status),
        "case {case:?}: PutBucketEncryption must succeed, got {} with {:?}",
        stored.status,
        stored.text()
    );
}

/// `s3:PutObject` with no encryption header at all, asserting the write recorded the bucket's
/// DEFAULT encryption for it — which is what says the object really is stored under the key the
/// row's policy then decides, rather than under nothing.
async fn put_object_inheriting_the_bucket_default(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    key: &str,
    body: &str,
) {
    let written = call(
        router,
        "s3",
        &regional_endpoint(),
        "PUT",
        &format!("/{bucket}/{key}"),
        &[],
        Bytes::from(body.to_string()),
    )
    .await;
    assert_eq!(
        written.status,
        200,
        "case {case:?}: PutObject({key:?}) must succeed, got {} with {:?}",
        written.status,
        written.text()
    );
    assert_eq!(
        written.header(ALGORITHM_HEADER),
        SSE_KMS,
        "case {case:?}: expected {key:?} stored under the bucket's default {SSE_KMS:?}, got {:?}",
        written.header(ALGORITHM_HEADER)
    );
}

/// `kms:DescribeKey`, answering the key id KMS holds for `key_arn` — the other form the
/// bucket's default encryption may name the same key by.
async fn key_id_of(router: &MultiServiceRouter, case: &str, key_arn: &str) -> String {
    let described = call(
        router,
        "kms",
        &format!("kms.{REGION}.amazonaws.com"),
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", "TrentService.DescribeKey"),
        ],
        Bytes::from(json!({"KeyId": key_arn}).to_string()),
    )
    .await;
    assert_eq!(
        described.status,
        200,
        "case {case:?}: DescribeKey must succeed, got {} with {:?}",
        described.status,
        described.text()
    );
    let payload: Value = serde_json::from_slice(&described.body).unwrap_or(Value::Null);
    let key_id = payload["KeyMetadata"]["KeyId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !key_id.is_empty(),
        "case {case:?}: DescribeKey reports no KeyId, got {:?}",
        described.text()
    );
    key_id
}

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

/// How the bucket's default encryption names the row's key — the two forms
/// `ApplyServerSideEncryptionByDefault$KMSMasterKeyID` takes ("the Amazon Web Services KMS key
/// ID or the Amazon Resource Name (ARN) of the KMS key").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyReference {
    /// The key's Amazon Resource Name.
    Arn,
    /// The key's id alone.
    KeyId,
}

/// What the viewer dialling the published address must be served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The origin's object, byte for byte, under HTTP 200.
    TheObject,
    /// The origin's own refusal, and none of the object's bytes.
    Refused,
}

/// One row: a bucket whose DEFAULT encryption names this row's key as [`KeyReference`] says, one
/// object inheriting it, a distribution over that bucket under an origin access control, and the
/// statements the key's policy carries about the fetch's principal — and what the dialled
/// address then answers.
struct Row {
    case: &'static str,
    /// This row's own bucket, so one row's documents cannot decide another's read.
    bucket: &'static str,
    key: &'static str,
    body: &'static str,
    reference: KeyReference,
    /// The statements beside the key's account-root statement, built from this distribution's
    /// own ARN once it is known.
    statements: fn(&str) -> Vec<Value>,
    expected: Expected,
}

/// The grant an admitted fetch needs: the decrypt to the CloudFront service principal
/// ([`edge_grant`]), scoped by `aws:SourceArn` to the distribution `arn` names.
fn scoped_to(arn: &str) -> Value {
    edge_grant(Some(json!({"StringEquals": {"AWS:SourceArn": arn}})))
}

/// The table: one row per form the bucket names the key by and per thing the key's policy says
/// about the principal the fetch is made as.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the bucket's default encryption naming the key by its ARN, decrypt granted \
                   for this distribution",
            bucket: "dialled-kms-default-by-arn",
            key: "reports/quarter.csv",
            body: "the object stored under the bucket's default key\n",
            reference: KeyReference::Arn,
            statements: |arn| vec![scoped_to(arn)],
            expected: Expected::TheObject,
        },
        Row {
            case: "the same bucket default naming the key by its KEY ID, under a nested prefix",
            bucket: "dialled-kms-default-by-key-id",
            key: "static/app/main.js",
            body: "export const served = true;\n",
            reference: KeyReference::KeyId,
            statements: |arn| vec![scoped_to(arn)],
            expected: Expected::TheObject,
        },
        Row {
            case: "an explicit Deny beside the grant this distribution matches",
            bucket: "dialled-kms-explicit-deny",
            key: "denied.csv",
            body: "the object a Deny withholds\n",
            reference: KeyReference::Arn,
            statements: |arn| {
                vec![
                    scoped_to(arn),
                    service_grant(CLOUDFRONT_PRINCIPAL, "Deny", DECRYPT_ACTION, None),
                ]
            },
            expected: Expected::Refused,
        },
        Row {
            case: "the decrypt granted to another service principal entirely",
            bucket: "dialled-kms-another-principal",
            key: "elsewhere/asset.txt",
            body: "the object granted to another service\n",
            reference: KeyReference::Arn,
            statements: |_arn| {
                vec![service_grant(
                    ANOTHER_SERVICE_PRINCIPAL,
                    "Allow",
                    DECRYPT_ACTION,
                    None,
                )]
            },
            expected: Expected::Refused,
        },
        Row {
            case: "a grant scoped to ANOTHER distribution's ARN, which this one is not",
            bucket: "dialled-kms-another-distribution",
            key: "not-mine.csv",
            body: "the object scoped to another distribution\n",
            reference: KeyReference::KeyId,
            statements: |_arn| {
                vec![edge_grant(Some(
                    json!({"StringEquals": {"AWS:SourceArn": another_distribution_arn()}}),
                ))]
            },
            expected: Expected::Refused,
        },
    ]
}

/// What one row left behind for the dialling phase.
struct Prepared {
    case: &'static str,
    bucket: &'static str,
    key: &'static str,
    body: &'static str,
    expected: Expected,
    id: String,
    /// The value the create PUBLISHED as this distribution's `DomainName`.
    domain: String,
    /// The `host:port` this emulator opened for the distribution, which the viewer dials.
    address: String,
}

// ---------------------------------------------------------------------------
// The suite
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_key_a_dialled_edge_decrypts_with_and_the_principal_its_policy_decides() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    // Every create goes through this one plane, so the listener each distribution's edge is
    // served on hands the requests it accepts back to this same emulator — and the origin fetch
    // it makes reaches the S3 and the KMS mounted here.
    let setup = emulator_with_kms(&engine);

    let mut prepared: Vec<Prepared> = Vec::new();
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        create_bucket(&setup, case, row.bucket).await;
        let key_arn = created_key(&setup, case).await;
        let named = match row.reference {
            KeyReference::Arn => key_arn.clone(),
            KeyReference::KeyId => key_id_of(&setup, case, &key_arn).await,
        };
        put_bucket_encryption(&setup, case, row.bucket, &named).await;
        put_object_inheriting_the_bucket_default(&setup, case, row.bucket, row.key, row.body).await;

        let control =
            create_origin_access_control(&setup, case, &format!("dialled-kms-key-{index}"), "always")
                .await;
        let (id, domain, arn) = create_distribution(
            &setup,
            case,
            &format!("dialled-kms-key-reference-{index}"),
            row.bucket,
            &control,
            CACHING_DISABLED,
        )
        .await;
        // The published value is the address a client on this machine reaches the edge at — or
        // there is nothing for a viewer to dial at all.
        assert!(
            is_published_edge_address(&domain),
            "case {case:?}: expected an address this emulator answers on (host:port) as the \
             published DomainName, got {domain:?}"
        );
        let address = edge_listener_authority(&engine, &id).unwrap_or_else(|| {
            panic!("case {case:?}: this emulator opened no address for the distribution {id}")
        });
        assert!(
            is_dialable(&address),
            "case {case:?}: expected a host:port a client on this machine can dial, got \
             {address:?}"
        );

        // Both documents are written AFTER the distribution exists, because each is scoped to
        // its own ARN — the order a client has to follow. The bucket's grant admits the read
        // for this distribution, so what remains for a row to decide is the key policy alone.
        put_bucket_policy(
            &setup,
            case,
            row.bucket,
            &policy(
                CLOUDFRONT_PRINCIPAL,
                "s3:GetObject",
                &format!("arn:aws:s3:::{}/*", row.bucket),
                Some(&json!({"StringEquals": {"AWS:SourceArn": arn.clone()}}).to_string()),
            ),
        )
        .await;
        put_key_policy(
            &setup,
            case,
            &key_arn,
            &key_policy_with((row.statements)(&arn)),
        )
        .await;

        prepared.push(Prepared {
            case,
            bucket: row.bucket,
            key: row.key,
            body: row.body,
            expected: row.expected,
            id,
            domain,
            address,
        });
    }

    for row in &prepared {
        let case = row.case;
        // A configuration that has not reached the edge locations serves no viewer.
        awaited_deployment(&setup, case, &row.id).await;
        // The address CloudFront reports on a READ is the same one the create published: the
        // name is the distribution's, not the create response's.
        let read_back = read_domain_name(&setup, &row.id).await;
        assert_eq!(
            read_back, row.domain,
            "case {case:?}: expected GetDistribution to report the published name {:?}, got \
             {read_back:?}",
            row.domain
        );

        let answer = dial(case, &row.address, &row.domain, &format!("/{}", row.key)).await;
        assert_served(case, &answer, row.expected, row.body);

        // The subject the key policy decided belongs to the FETCH alone: the account's own
        // direct read of the very same object is decided against the credential it presents,
        // which the key's account-root statement delegates to this account — so it is served
        // whatever that policy said about CloudFront.
        let direct = call(
            &setup,
            "s3",
            &regional_endpoint(),
            "GET",
            &format!("/{}/{}", row.bucket, row.key),
            &[],
            Bytes::new(),
        )
        .await;
        assert_eq!(
            direct.status,
            200,
            "case {case:?}: a direct read by the account must be served, got {} with {:?}",
            direct.status,
            direct.text()
        );
        assert_eq!(
            direct.text(),
            row.body,
            "case {case:?}: a direct read by the account must answer the object's bytes, got \
             {:?}",
            direct.text()
        );
    }
}

/// Assert what the viewer's own connection was served.
fn assert_served(case: &str, answer: &Served, expected: Expected, body: &str) {
    // Whatever the answer is, it is the edge's: CloudFront identifies the request it served,
    // which is what says the dialled address is a distribution's data plane and not some other
    // listener that happened to accept the connection.
    assert!(
        !answer.header("x-amz-cf-id").is_empty(),
        "case {case:?}: expected the edge's own X-Amz-Cf-Id on the answer, got {:?}",
        answer.raw
    );
    match expected {
        Expected::TheObject => {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected the origin's object at the published address, got {} \
                 with {:?}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.body, body,
                "case {case:?}: expected the ORIGIN's own bytes, got {:?}",
                answer.body
            );
            assert_eq!(
                answer.header("x-cache"),
                MISS,
                "case {case:?}: expected the answer to be the origin's, got X-Cache {:?}",
                answer.header("x-cache")
            );
        }
        Expected::Refused => {
            assert_eq!(
                answer.status, ACCESS_DENIED_STATUS,
                "case {case:?}: expected HTTP {ACCESS_DENIED_STATUS} at the published address, \
                 got {} with {:?}",
                answer.status, answer.body
            );
            assert_eq!(
                element(&answer.body, "Code"),
                ACCESS_DENIED,
                "case {case:?}: expected the origin's own {ACCESS_DENIED} document, got {:?}",
                answer.body
            );
            assert_eq!(
                element(&answer.body, "Message"),
                ACCESS_DENIED_MESSAGE,
                "case {case:?}: expected the message {ACCESS_DENIED_MESSAGE:?}, got {:?}",
                element(&answer.body, "Message")
            );
            assert!(
                !answer.body.contains(body),
                "case {case:?}: a refused request must carry none of the object's bytes, got \
                 {:?}",
                answer.body
            );
        }
    }
}
