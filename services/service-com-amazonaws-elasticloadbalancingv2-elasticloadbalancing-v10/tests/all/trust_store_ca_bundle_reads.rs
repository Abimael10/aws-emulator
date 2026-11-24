// SPDX-License-Identifier: CC0-1.0

//! What `GetTrustStoreCaCertificatesBundle` ANSWERS, end to end over the wire.
//!
//! Every request goes through the router exactly as a client's would — awsQuery form body
//! in, `<{Operation}Response>` XML out — so each case exercises routing, the awsQuery
//! (de)serialization, the handler, the persisted state and the serializer. Amazon S3 is
//! MOUNTED beside this service on the same engine (the harness
//! `crate::trust_store_lifecycle` builds), because a trust store's CA certificates bundle is
//! an object that store holds: every bundle read back below is one this suite really staged
//! with `PutObject`, holding real self-signed X.509 certificates it generated.
//!
//! The rules under test, each asserted over a SET of inputs rather than one request:
//!
//! 1. a read that reports WHERE one of a trust store's own objects is fetched from answers a
//!    pre-signed `GET` of that very object in this emulator's own object store, valid for the
//!    ten minutes the model documents ("This action returns a pre-signed S3 URI which is
//!    active for ten minutes",
//!    `smithy_query(query="operation",operation="GetTrustStoreCaCertificatesBundle")`), so
//!    following the address serves the staged bytes back — the object the trust store was
//!    last pointed at, whether that is the create's bundle or the one a modification rotated
//!    it to;
//! 2. the read REPORTS: it changes nothing a later caller sees, so the same address serves
//!    the same bytes again and the trust store still reports the certificates it was built
//!    from;
//! 3. a `TrustStoreArn` this account holds no trust store under is the modelled
//!    `TrustStoreNotFound` (HTTP 400) naming that ARN — the one wording every refusal about a
//!    trust store addressed by ARN carries — and nothing is reported with it.
//!
//! The rows differ along what those rules generalise over: how many certificates the bundle
//! holds, the shape of the object key, whether the bundle was rotated after the create,
//! where the trust store is in its `CREATING` -> `ACTIVE` move, and which kind of ARN holds
//! no trust store (one fabricated, one of a released trust store, one of another account).

use service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::state::{
    trust_store_arn_not_found_message, TRUST_STORE_ACTIVE, TRUST_STORE_OBJECT_URI_VALIDITY,
};

use crate::provisioning_loop::{action, call, first_text, p, Reply, REGION};
use crate::target_group_attributes_and_modification::unescaped;
use crate::trust_store_lifecycle::{
    absent_trust_store_arn, ca_bundle, create_trust_store, delete_trust_store,
    describe_trust_stores, emulator_with_bundles, message, modify_trust_store, only_trust_store,
    Emulator, BUNDLE_BUCKET,
};
use crate::trust_store_revocations::fetch;

/// The awsQuery error code the model declares for this operation — its ONE error
/// (`smithy_query(query="operation",operation="GetTrustStoreCaCertificatesBundle")`:
/// `TrustStoreNotFoundException`, `awsQueryError` code `TrustStoreNotFound`, HTTP 400).
const TRUST_STORE_NOT_FOUND: &str = "TrustStoreNotFound";

/// An account that is not the one this emulator answers for, so an ARN built with it names a
/// trust store no caller of this account holds.
const OTHER_ACCOUNT: &str = "210987654321";

/// `GetTrustStoreCaCertificatesBundle` for `trust_store_arn`.
async fn get_ca_bundle(emulator: &Emulator, trust_store_arn: &str) -> Reply {
    let mut form = action("GetTrustStoreCaCertificatesBundle");
    form.push(p("TrustStoreArn", trust_store_arn));
    call(&emulator.router, &form).await
}

/// The `Location` a read reported, with the XML entities the serializer writes resolved — a
/// pre-signed address carries `&` between its query fields, so the assertion compares the
/// VALUE rather than its encoding.
fn reported_location(reply: &Reply, case: &str) -> String {
    unescaped(
        &first_text(&reply.body, "Location")
            .unwrap_or_else(|| panic!("case {case:?}: no Location in {}", reply.body)),
    )
}

/// Assert that `location` is a pre-signed address of the object `key` under
/// [`BUNDLE_BUCKET`], valid for the window the model documents.
fn assert_presigned(case: &str, location: &str, key: &str) {
    assert!(
        location.contains(&format!("/{BUNDLE_BUCKET}/")),
        "case {case:?}: expected an address of an object under {BUNDLE_BUCKET}, \
         got {location:?}"
    );
    assert!(
        location.contains("X-Amz-Signature=") && location.contains("X-Amz-Credential="),
        "case {case:?}: expected a PRE-SIGNED address for {key:?}, got {location:?}"
    );
    // The window is the one the model documents for this read, taken from the constant the
    // service signs with rather than from a number written here twice.
    let expires = format!("X-Amz-Expires={}", TRUST_STORE_OBJECT_URI_VALIDITY.as_secs());
    assert!(
        location.contains(&expires),
        "case {case:?}: expected the address to carry {expires:?}, got {location:?}"
    );
}

/// How many certificates the trust store `arn` reports, read back through the describe.
async fn reported_certificates(emulator: &Emulator, arn: &str, case: &str) -> i64 {
    let described = describe_trust_stores(emulator, &[p("TrustStoreArns.member.1", arn)]).await;
    let trust_store = only_trust_store(&described, case);
    first_text(&trust_store, "NumberOfCaCertificates")
        .and_then(|count| count.parse::<i64>().ok())
        .unwrap_or_else(|| {
            panic!("case {case:?}: no NumberOfCaCertificates in {}", described.body)
        })
}

// ---------------------------------------------------------------------------
// RULE 1 and 2: the address serves the bundle the trust store was built from, twice
// ---------------------------------------------------------------------------

/// One trust store whose bundle is read back: the case, the trust store's name, the key its
/// bundle is staged at, and how many certificates that bundle holds.
struct Read {
    case: &'static str,
    name: &'static str,
    key: &'static str,
    certificates: usize,
}

/// The rows differ along what the rule generalises over: how many certificates the bundle
/// holds (one, several), and the shape of the object key (flat, nested, mixed case with
/// periods, and one carrying a space — a key is a byte string, so the address has to carry
/// it intact).
const READS: &[Read] = &[
    Read {
        case: "one certificate under a flat key",
        name: "ts-bundle-read-flat",
        key: "flat-bundle.pem",
        certificates: 1,
    },
    Read {
        case: "three certificates under a nested key",
        name: "ts-bundle-read-nested",
        key: "bundles/nested/ca-bundle.pem",
        certificates: 3,
    },
    Read {
        case: "two certificates under a mixed-case key carrying periods",
        name: "ts-bundle-read-dotted",
        key: "Bundles/CA.bundle.v2.pem",
        certificates: 2,
    },
    Read {
        case: "one certificate under a key carrying a space",
        name: "ts-bundle-read-spaced",
        key: "bundles/ca bundle.pem",
        certificates: 1,
    },
];

#[tokio::test]
async fn a_read_answers_a_presigned_address_that_serves_the_staged_bundle() {
    let emulator = emulator_with_bundles().await;

    for row in READS {
        let staged = ca_bundle(row.certificates);
        emulator.stage(row.key, &staged).await;
        let created = create_trust_store(&emulator, row.name, BUNDLE_BUCKET, row.key, &[]).await;
        assert_eq!(
            created.status, 200,
            "case {:?}: creating the trust store must succeed, got {} body {}",
            row.case, created.status, created.body
        );
        let arn = first_text(&created.body, "TrustStoreArn")
            .unwrap_or_else(|| panic!("case {:?}: no TrustStoreArn in {}", row.case, created.body));

        // RULE 2: the read reports, so it is asked TWICE and each answer must serve the same
        // bytes — and the trust store must still report the certificates it was built from.
        for pass in ["first read", "second read"] {
            let read = get_ca_bundle(&emulator, &arn).await;
            assert_eq!(
                read.status, 200,
                "case {:?} ({pass}): expected 200 from GetTrustStoreCaCertificatesBundle, \
                 got {} body {}",
                row.case, read.status, read.body
            );
            let location = reported_location(&read, row.case);
            assert_presigned(row.case, &location, row.key);

            let (status, served) = fetch(&emulator.router, &location).await;
            assert_eq!(
                status, 200,
                "case {:?} ({pass}): following {location:?} must serve the bundle, got {status}",
                row.case
            );
            assert_eq!(
                String::from_utf8_lossy(&served),
                staged,
                "case {:?} ({pass}): expected the staged bundle back from {location:?}",
                row.case
            );
        }

        let reported = reported_certificates(&emulator, &arn, row.case).await;
        assert_eq!(
            reported, row.certificates as i64,
            "case {:?}: after the reads the trust store must still report \
             {} certificates, got {reported}",
            row.case, row.certificates
        );
    }
}

#[tokio::test]
async fn a_read_after_a_rotation_answers_the_bundle_the_trust_store_was_moved_to() {
    let emulator = emulator_with_bundles().await;
    let case = "a bundle rotated by ModifyTrustStore";

    let first = ca_bundle(2);
    let rotated = ca_bundle(4);
    emulator.stage("rotation/first.pem", &first).await;
    emulator.stage("rotation/rotated.pem", &rotated).await;

    let created = create_trust_store(
        &emulator,
        "ts-bundle-rotation",
        BUNDLE_BUCKET,
        "rotation/first.pem",
        &[],
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: creating the trust store must succeed, got {} body {}",
        created.status, created.body
    );
    let arn = first_text(&created.body, "TrustStoreArn")
        .unwrap_or_else(|| panic!("case {case:?}: no TrustStoreArn in {}", created.body));

    let modified = modify_trust_store(&emulator, &arn, BUNDLE_BUCKET, "rotation/rotated.pem").await;
    assert_eq!(
        modified.status, 200,
        "case {case:?}: rotating the bundle must succeed, got {} body {}",
        modified.status, modified.body
    );

    let read = get_ca_bundle(&emulator, &arn).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: expected 200 from GetTrustStoreCaCertificatesBundle, got {} body {}",
        read.status, read.body
    );
    let location = reported_location(&read, case);
    assert_presigned(case, &location, "rotation/rotated.pem");
    let (status, served) = fetch(&emulator.router, &location).await;
    assert_eq!(
        status, 200,
        "case {case:?}: following {location:?} must serve the bundle, got {status}"
    );
    let served = String::from_utf8_lossy(&served).to_string();
    assert_eq!(
        served, rotated,
        "case {case:?}: expected the ROTATED bundle back from {location:?}"
    );
    assert_ne!(
        served, first,
        "case {case:?}: the address must not serve the bundle the create was pointed at, \
         got {location:?}"
    );
}

#[tokio::test]
async fn a_read_is_served_before_and_after_the_trust_store_settles() {
    let emulator = emulator_with_bundles().await;
    let case = "a trust store read while it is still creating, and once it is active";

    let staged = ca_bundle(1);
    emulator.stage("settling/ca-bundle.pem", &staged).await;
    let created = create_trust_store(
        &emulator,
        "ts-bundle-settling",
        BUNDLE_BUCKET,
        "settling/ca-bundle.pem",
        &[],
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: creating the trust store must succeed, got {} body {}",
        created.status, created.body
    );
    let arn = first_text(&created.body, "TrustStoreArn")
        .unwrap_or_else(|| panic!("case {case:?}: no TrustStoreArn in {}", created.body));

    // Straight after the create, while the clock still owes the trust store its move: the
    // read is about the bundle, which this model declares no state error for.
    let creating = get_ca_bundle(&emulator, &arn).await;
    assert_eq!(
        creating.status, 200,
        "case {case:?}: a creating trust store's bundle is reported, not refused, got {} body {}",
        creating.status, creating.body
    );

    // Then once the trust store is ACTIVE. The CONDITION is polled for — nothing here waits
    // a fixed time — with a deadline far above the span the clock owes.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let status = loop {
        let described =
            describe_trust_stores(&emulator, &[p("TrustStoreArns.member.1", &arn)]).await;
        let status = first_text(&only_trust_store(&described, case), "Status").unwrap_or_default();
        if status == TRUST_STORE_ACTIVE || std::time::Instant::now() >= deadline {
            break status;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    };
    assert_eq!(
        status, TRUST_STORE_ACTIVE,
        "case {case:?}: expected the trust store to reach {TRUST_STORE_ACTIVE}, got {status:?}"
    );

    let active = get_ca_bundle(&emulator, &arn).await;
    assert_eq!(
        active.status, 200,
        "case {case:?}: expected 200 once the trust store is active, got {} body {}",
        active.status, active.body
    );
    let location = reported_location(&active, case);
    assert_presigned(case, &location, "settling/ca-bundle.pem");
    let (served_status, served) = fetch(&emulator.router, &location).await;
    assert_eq!(
        served_status, 200,
        "case {case:?}: following {location:?} must serve the bundle, got {served_status}"
    );
    assert_eq!(
        String::from_utf8_lossy(&served),
        staged,
        "case {case:?}: expected the staged bundle back from {location:?}"
    );
}

// ---------------------------------------------------------------------------
// RULE 3: an ARN no trust store is held under is the modelled refusal naming it
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_read_of_an_arn_no_trust_store_is_held_under_is_the_modelled_refusal() {
    let emulator = emulator_with_bundles().await;

    // A trust store that is RELEASED, so its ARN is one this account held and holds no
    // longer — the same answer as an ARN it never held.
    let staged = ca_bundle(1);
    emulator.stage("released/ca-bundle.pem", &staged).await;
    let created = create_trust_store(
        &emulator,
        "ts-bundle-released",
        BUNDLE_BUCKET,
        "released/ca-bundle.pem",
        &[],
    )
    .await;
    let released = first_text(&created.body, "TrustStoreArn")
        .unwrap_or_else(|| panic!("no TrustStoreArn in {}", created.body));
    let deleted = delete_trust_store(&emulator, &released).await;
    assert_eq!(
        deleted.status, 200,
        "releasing the trust store must succeed, got {} body {}",
        deleted.status, deleted.body
    );

    let rows: Vec<(&str, String)> = vec![
        (
            "an ARN of this account no trust store was ever minted for",
            absent_trust_store_arn("ts-bundle-fabricated"),
        ),
        (
            "the ARN of a trust store that was released",
            released.clone(),
        ),
        (
            "an ARN of another account",
            format!(
                "arn:aws:elasticloadbalancing:{REGION}:{OTHER_ACCOUNT}:truststore/\
                 ts-bundle-elsewhere/0123456789abcdef"
            ),
        ),
        (
            "an ARN in another Region",
            format!(
                "arn:aws:elasticloadbalancing:eu-west-1:{}:truststore/\
                 ts-bundle-other-region/0123456789abcdef",
                crate::provisioning_loop::account()
            ),
        ),
    ];

    for (case, arn) in &rows {
        let read = get_ca_bundle(&emulator, arn).await;
        assert_eq!(
            read.status, 400,
            "case {case:?}: expected HTTP 400, got {} body {}",
            read.status, read.body
        );
        assert_eq!(
            read.error_code(),
            TRUST_STORE_NOT_FOUND,
            "case {case:?}: expected {TRUST_STORE_NOT_FOUND}, got {} in {}",
            read.error_code(),
            read.body
        );
        let answered = message(&read);
        assert_eq!(
            answered,
            trust_store_arn_not_found_message(arn),
            "case {case:?}: expected the refusal to name the ARN from the request, \
             got {answered:?}"
        );
        assert_eq!(
            first_text(&read.body, "Location"),
            None,
            "case {case:?}: a refused read reports no Location, got {}",
            read.body
        );
    }
}
