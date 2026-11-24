// SPDX-License-Identifier: CC0-1.0

//! WHICH object `GetTrustStoreCaCertificatesBundle` addresses, and what following that
//! address serves back, end to end over the wire.
//!
//! The suite beside this one (`crate::trust_store_ca_bundle_reads`) pins that a read answers
//! a pre-signed address of the trust store's own bundle. This one pins the two things that
//! address has to carry beyond the bucket and the key, each over a SET of inputs:
//!
//! 1. the address names the object the trust store was POINTED AT, object version included
//!    — `CaCertificatesBundleS3ObjectVersion` is "The Amazon S3 object version for the ca
//!    certificates bundle. If undefined the current version is used"
//!    (`smithy_query(query="operation",operation="CreateTrustStore,ModifyTrustStore")`), so a
//!    bundle named with a version is addressed with that version and later writes at the same
//!    key change nothing the read serves, while a bundle named without one is addressed
//!    without one and so follows the key's current object;
//! 2. the bytes served back are the staged object BYTE FOR BYTE, whatever the bundle holds
//!    around its certificate blocks — the address leads to the object itself, so nothing on
//!    the way re-encodes, re-wraps or truncates it.
//!
//! The rows differ along what those rules generalise over: how many certificates each
//! version of a bundle holds, the shape of the object key, whether the version was named by
//! the create or by a rotation, and what the bundle's bytes carry besides certificate blocks
//! (CRLF line endings, a prose header, bytes that are not text at all).
//!
//! Every request goes through the router exactly as a client's would, and every bundle is one
//! this suite really staged in the Amazon S3 mounted beside this service on the same engine
//! (the harness `crate::trust_store_lifecycle` builds).

use crate::provisioning_loop::{action, call, p, Reply};
use crate::trust_store_ca_bundle_reads::{
    assert_presigned, get_ca_bundle, reported_certificates, reported_location,
};
use crate::trust_store_lifecycle::{
    ca_bundle, create_trust_store, emulator_with_bundles, Emulator, BUNDLE_BUCKET,
};
use crate::trust_store_revocations::fetch;

// ---------------------------------------------------------------------------
// Staging objects the way a caller does, in a bucket that keeps every version
// ---------------------------------------------------------------------------

/// Turn on versioning for [`BUNDLE_BUCKET`], so every write at a key is kept as its own
/// version and reports the id it is addressed under ("All objects added to the bucket
/// receive a unique version ID", `PutBucketVersioning`).
async fn keep_every_version(emulator: &Emulator) {
    emulator
        .s3()
        .put_bucket_versioning()
        .bucket(BUNDLE_BUCKET)
        .versioning_configuration(
            aws_sdk_s3::types::VersioningConfiguration::builder()
                .status(aws_sdk_s3::types::BucketVersioningStatus::Enabled)
                .build(),
        )
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("enabling versioning on {BUNDLE_BUCKET} must succeed, got {error:?}")
        });
}

/// Put the bytes `body` at `key` of [`BUNDLE_BUCKET`] and answer the version id the store
/// minted for that write, if any.
///
/// Bytes rather than text, because rule 2 is about bytes: a bundle below carries CRLF line
/// endings, a prose header and a tail that is no text at all.
async fn stage_bytes(emulator: &Emulator, key: &str, body: &[u8]) -> Option<String> {
    emulator
        .s3()
        .put_object()
        .bucket(BUNDLE_BUCKET)
        .key(key)
        .body(aws_sdk_s3::primitives::ByteStream::from(body.to_vec()))
        .send()
        .await
        .unwrap_or_else(|error| panic!("staging {key} must succeed, got {error:?}"))
        .version_id()
        .map(str::to_string)
}

/// Stage `body` at `key` and answer the version id it is held under, or panic: a
/// versioning-enabled bucket mints one for every write.
async fn stage_version(emulator: &Emulator, key: &str, body: &[u8], case: &str) -> String {
    stage_bytes(emulator, key, body)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: staging {key} must report a version id"))
}

/// `ModifyTrustStore` pointing `trust_store` at `key`, optionally at one object version of it.
async fn rotate_to(
    emulator: &Emulator,
    trust_store: &str,
    key: &str,
    version: Option<&str>,
) -> Reply {
    let mut form = action("ModifyTrustStore");
    form.push(p("TrustStoreArn", trust_store));
    form.push(p("CaCertificatesBundleS3Bucket", BUNDLE_BUCKET));
    form.push(p("CaCertificatesBundleS3Key", key));
    if let Some(version) = version {
        form.push(p("CaCertificatesBundleS3ObjectVersion", version));
    }
    call(&emulator.router, &form).await
}

/// Assert that `served` is `staged` byte for byte, naming the case and what differs.
fn assert_same_bytes(case: &str, served: &[u8], staged: &[u8]) {
    let first_difference = served
        .iter()
        .zip(staged.iter())
        .position(|(left, right)| left != right);
    assert!(
        served == staged,
        "case {case:?}: expected the staged bundle byte for byte ({} bytes), \
         got {} bytes, first differing byte at {first_difference:?}",
        staged.len(),
        served.len()
    );
}

/// The ARN of the trust store a create reported, or a panic naming the case.
fn created_arn(created: &Reply, case: &str) -> String {
    assert_eq!(
        created.status, 200,
        "case {case:?}: creating the trust store must succeed, got {} body {}",
        created.status, created.body
    );
    created
        .text("TrustStoreArn")
        .unwrap_or_else(|| panic!("case {case:?}: no TrustStoreArn in {}", created.body))
}

// ---------------------------------------------------------------------------
// RULE 1: the address names the object version the trust store was pointed at
// ---------------------------------------------------------------------------

/// How a trust store came to be pointed at one version of its bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PointedBy {
    /// `CreateTrustStore` named the version.
    Create,
    /// `CreateTrustStore` took the current object and `ModifyTrustStore` named the version.
    Rotation,
}

/// One pinned bundle: the case, the trust store's name, the key every version of the bundle
/// is written at, how many certificates the PINNED version holds, how many the version
/// written after it holds, and which call named the pinned version.
struct Pinned {
    case: &'static str,
    name: &'static str,
    key: &'static str,
    pinned_certificates: usize,
    later_certificates: usize,
    pointed_by: PointedBy,
}

/// The rows differ along what the rule generalises over: how many certificates the pinned
/// and the later version hold (so the two bundles can never be confused), the shape of the
/// key, and whether the create or a rotation named the version.
const PINNED: &[Pinned] = &[
    Pinned {
        case: "a create pointed at an earlier version of a flat key",
        name: "ts-version-create-flat",
        key: "versions/flat-bundle.pem",
        pinned_certificates: 1,
        later_certificates: 3,
        pointed_by: PointedBy::Create,
    },
    Pinned {
        case: "a create pointed at an earlier version of a nested key",
        name: "ts-version-create-nested",
        key: "versions/nested/ca.bundle.v2.pem",
        pinned_certificates: 3,
        later_certificates: 1,
        pointed_by: PointedBy::Create,
    },
    Pinned {
        case: "a rotation pointed at an earlier version",
        name: "ts-version-rotation",
        key: "versions/rotated bundle.pem",
        pinned_certificates: 2,
        later_certificates: 4,
        pointed_by: PointedBy::Rotation,
    },
];

#[tokio::test]
async fn a_read_addresses_the_object_version_the_trust_store_was_pointed_at() {
    let emulator = emulator_with_bundles().await;
    keep_every_version(&emulator).await;

    for row in PINNED {
        let pinned = ca_bundle(row.pinned_certificates);
        let later = ca_bundle(row.later_certificates);
        let pinned_version = stage_version(&emulator, row.key, pinned.as_bytes(), row.case).await;

        let arn = match row.pointed_by {
            PointedBy::Create => {
                // The version is named by the create, so the later write below is one the
                // trust store was never pointed at.
                let created = create_trust_store(
                    &emulator,
                    row.name,
                    BUNDLE_BUCKET,
                    row.key,
                    &[p("CaCertificatesBundleS3ObjectVersion", &pinned_version)],
                )
                .await;
                let arn = created_arn(&created, row.case);
                stage_version(&emulator, row.key, later.as_bytes(), row.case).await;
                arn
            }
            PointedBy::Rotation => {
                // The create takes the key's current object — which is the pinned version at
                // that moment — then a later write moves the key on and the ROTATION names
                // the pinned version again.
                let created =
                    create_trust_store(&emulator, row.name, BUNDLE_BUCKET, row.key, &[]).await;
                let arn = created_arn(&created, row.case);
                stage_version(&emulator, row.key, later.as_bytes(), row.case).await;
                let rotated = rotate_to(&emulator, &arn, row.key, Some(&pinned_version)).await;
                assert_eq!(
                    rotated.status, 200,
                    "case {:?}: rotating to the pinned version must succeed, got {} body {}",
                    row.case, rotated.status, rotated.body
                );
                arn
            }
        };

        let read = get_ca_bundle(&emulator, &arn).await;
        assert_eq!(
            read.status, 200,
            "case {:?}: expected 200 from GetTrustStoreCaCertificatesBundle, got {} body {}",
            row.case, read.status, read.body
        );
        let location = reported_location(&read, row.case);
        assert_presigned(row.case, &location, row.key);
        let names_version = format!("versionId={pinned_version}");
        assert!(
            location.contains(&names_version),
            "case {:?}: expected the address to carry {names_version:?}, got {location:?}",
            row.case
        );

        let (status, served) = fetch(&emulator.router, &location).await;
        assert_eq!(
            status, 200,
            "case {:?}: following {location:?} must serve the bundle, got {status}",
            row.case
        );
        assert_same_bytes(row.case, &served, pinned.as_bytes());
        assert_ne!(
            served,
            later.as_bytes(),
            "case {:?}: the address must not serve the version written after it, \
             got {location:?}",
            row.case
        );

        // The count the trust store reports is the pinned version's, so the version named is
        // the object the record was really built from rather than whatever the key holds now.
        let counted = reported_certificates(&emulator, &arn, row.case).await;
        assert_eq!(
            counted, row.pinned_certificates as i64,
            "case {:?}: expected the trust store to report {} certificates, got {counted}",
            row.case, row.pinned_certificates
        );
    }
}

#[tokio::test]
async fn a_bundle_named_without_a_version_is_addressed_without_one() {
    let emulator = emulator_with_bundles().await;
    keep_every_version(&emulator).await;
    let case = "a bundle named with no object version, written again afterwards";
    let key = "versions/current-bundle.pem";

    // "If undefined the current version is used": the create names no version, so the record
    // holds the KEY and the address it reports carries no version either.
    let at_create = ca_bundle(2);
    stage_version(&emulator, key, at_create.as_bytes(), case).await;
    let created = create_trust_store(&emulator, "ts-version-current", BUNDLE_BUCKET, key, &[]).await;
    let arn = created_arn(&created, case);
    let current = ca_bundle(4);
    stage_version(&emulator, key, current.as_bytes(), case).await;

    let read = get_ca_bundle(&emulator, &arn).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: expected 200 from GetTrustStoreCaCertificatesBundle, got {} body {}",
        read.status, read.body
    );
    let location = reported_location(&read, case);
    assert_presigned(case, &location, key);
    assert!(
        !location.contains("versionId="),
        "case {case:?}: a bundle named with no version is addressed with none, got {location:?}"
    );

    let (status, served) = fetch(&emulator.router, &location).await;
    assert_eq!(
        status, 200,
        "case {case:?}: following {location:?} must serve the bundle, got {status}"
    );
    assert_same_bytes(case, &served, current.as_bytes());

    // The count stays the one the create READ, which is a property of the record rather than
    // of the key: the read reports where the bundle is fetched from and changes nothing.
    let counted = reported_certificates(&emulator, &arn, case).await;
    assert_eq!(
        counted, 2,
        "case {case:?}: expected the trust store to still report 2 certificates, got {counted}"
    );
}

// ---------------------------------------------------------------------------
// RULE 2: the address serves the staged object byte for byte
// ---------------------------------------------------------------------------

/// One bundle whose bytes carry more than certificate blocks: the case, the trust store's
/// name, the key, the bytes staged, and how many certificates they hold.
struct Fidelity {
    case: &'static str,
    name: &'static str,
    key: &'static str,
    body: Vec<u8>,
    certificates: i64,
}

/// The bundles whose bytes are read back byte for byte. The rows differ along what the
/// bundle carries besides its certificate blocks: CRLF line endings, a prose header and
/// blank lines, and a tail that is no text at all.
fn fidelity_rows() -> Vec<Fidelity> {
    let crlf = ca_bundle(2).replace('\n', "\r\n");
    let commented = format!(
        "# Example corporate CA bundle\n# issued 2026-01-01\n\n{}\n\n",
        ca_bundle(1)
    );
    let mut trailing = ca_bundle(3).into_bytes();
    trailing.extend_from_slice(&[0x00, 0xFF, 0xFE, 0x0A]);
    vec![
        Fidelity {
            case: "a bundle with CRLF line endings",
            name: "ts-bytes-crlf",
            key: "fidelity/crlf-bundle.pem",
            body: crlf.into_bytes(),
            certificates: 2,
        },
        Fidelity {
            case: "a bundle with a prose header and blank lines",
            name: "ts-bytes-commented",
            key: "fidelity/commented-bundle.pem",
            body: commented.into_bytes(),
            certificates: 1,
        },
        Fidelity {
            case: "a bundle with a tail that is not text",
            name: "ts-bytes-binary-tail",
            key: "fidelity/binary-tail-bundle.pem",
            body: trailing,
            certificates: 3,
        },
    ]
}

#[tokio::test]
async fn the_address_serves_the_staged_bundle_byte_for_byte() {
    let emulator = emulator_with_bundles().await;

    for row in fidelity_rows() {
        stage_bytes(&emulator, row.key, &row.body).await;
        let created = create_trust_store(&emulator, row.name, BUNDLE_BUCKET, row.key, &[]).await;
        let arn = created_arn(&created, row.case);

        let read = get_ca_bundle(&emulator, &arn).await;
        assert_eq!(
            read.status, 200,
            "case {:?}: expected 200 from GetTrustStoreCaCertificatesBundle, got {} body {}",
            row.case, read.status, read.body
        );
        let location = reported_location(&read, row.case);
        assert_presigned(row.case, &location, row.key);

        let (status, served) = fetch(&emulator.router, &location).await;
        assert_eq!(
            status, 200,
            "case {:?}: following {location:?} must serve the bundle, got {status}",
            row.case
        );
        assert_same_bytes(row.case, &served, &row.body);

        let counted = reported_certificates(&emulator, &arn, row.case).await;
        assert_eq!(
            counted, row.certificates,
            "case {:?}: expected the trust store to report {} certificates, got {counted}",
            row.case, row.certificates
        );
    }
}
