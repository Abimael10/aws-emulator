// SPDX-License-Identifier: CC0-1.0

//! `UpdateTrustStore`, end to end over HTTP.
//!
//! The rule under test: updating a trust store is a CONDITIONAL, PARTIAL write of the one
//! record the create persisted. The `Id` has to name a stored trust store (`EntityNotFound`,
//! HTTP 404) and the `If-Match` has to be that record's current version token — a value that
//! is no version token at all is `InvalidIfMatchVersion` (HTTP 400) and a well-formed token
//! that is not the current one is `PreconditionFailed` (HTTP 412). Every member beyond those
//! two is optional, so what the request STATES is replaced and what it leaves out is carried
//! over untouched; a `CaCertificatesBundleSource` the request names is READ and the
//! certificate count the record then reports is the count of certificates in THAT object, so
//! a bundle this emulator cannot read is refused (`InvalidArgument` where no bundle can be
//! read at the address, `EntityNotFound` where the object store holds nothing there) and the
//! record stands exactly as it was. An accepted update versions the record afresh — the token
//! the caller matched on no longer satisfies an `If-Match` — and puts the trust store back in
//! its non-terminal status, which the CLOCK then owes the terminal one.
//!
//! Every refusal writes NOTHING: the identity, the certificate count, the revocation
//! behaviour, the status and the version token all stand, so a caller that re-reads the trust
//! store and retries with the version it reports lands.
//!
//! The rows span what the rule generalises over: how many certificates the new bundle holds
//! (more than before, fewer than before, as many as before), whether the request states the
//! OCSP member at all and which value it states, the three HTTP spellings of one entity tag,
//! every way a value can fail to be a version token, ids of forms no trust store answers to,
//! and every way a bundle can fail to be readable.
//!
//! The expected sentences are LITERALS here rather than the crate's own constants, so a
//! constant edited to some other wording fails these rows. The fixture is the one
//! [`crate::trust_store_lifecycle`] owns — CloudFront and Amazon S3 over one engine, with
//! real CA bundles staged through Amazon S3 — and the wire harness is
//! [`crate::distribution_lifecycle`]'s.

use std::time::Instant;

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    call_with_headers, Answer, API_VERSION, POLL_INTERVAL, READY_DEADLINE,
};
use crate::distributions_by_web_acl_id::encode_label;
use crate::key_value_store_lifecycle::{escape, is_version_token, member};
use crate::trust_store_lifecycle::{
    ca_bundle, create_trust_store, get_trust_store, request_xml, Emulator, BUNDLE_BUCKET, REGION,
};

// The sentences this service answers these codes with, spelled out as literals.
/// `InvalidIfMatchVersion`: a property of the HEADER the request carried, so the sentence
/// names nothing about the request or the resource.
const INVALID_IF_MATCH_VERSION: &str =
    "The If-Match version is missing or not valid for the resource.";

/// `PreconditionFailed` on the surface a trust store belongs to — the family of resources a
/// caller conditionally rewrites, which is how the sibling delete of this very resource is
/// worded.
const PRECONDITION_FAILED: &str = "The request failed because it didn't meet the preconditions \
     in one or more request-header fields.";

/// `EntityNotFound` for an id no trust store answers to.
const TRUST_STORE_NOT_FOUND: &str = "The specified trust store does not exist.";

/// `InvalidArgument` for a CA certificates bundle no trust store can be described by.
const INVALID_BUNDLE: &str = "The request contains an invalid CA Certificates Bundle.";

/// `EntityNotFound` for a bundle address the object store holds nothing at.
const BUNDLE_NOT_FOUND: &str = "The specified CA Certificates Bundle does not exist.";

/// The status a trust store whose bundle is still propagating to the edge locations is in.
const PENDING: &str = "pending";

/// The status a trust store validates client certificates in.
const ACTIVE: &str = "active";

/// A version token that is well formed — `E` plus 13 characters of the shape this service
/// mints — but that nothing in a fresh emulator holds, so a precondition carrying it is STALE
/// rather than unreadable.
const UNHELD_VERSION_TOKEN: &str = "E1ABCDEFGHIJKL";

// ---------------------------------------------------------------------------
// The trust store under test, and the calls that read and write it
// ---------------------------------------------------------------------------

/// One trust store this fixture holds, as the record a caller can see it.
#[derive(Debug, Clone)]
struct Store {
    name: String,
    id: String,
    arn: String,
    etag: String,
    /// `NumberOfCaCertificates`, as the last accepted call reported it.
    certificates: usize,
    /// `UseClientCertificateOCSPEndpoint`, as the last accepted call reported it.
    ocsp: bool,
}

/// Create one trust store from a freshly staged CA bundle of `certificates` certificates,
/// and answer the record its create minted.
async fn trust_store(
    emulator: &Emulator,
    name: &str,
    certificates: usize,
    ocsp: Option<bool>,
) -> Store {
    let key = format!("{name}.pem");
    emulator.stage(&key, &ca_bundle(certificates)).await;
    let body = request_xml(name, BUNDLE_BUCKET, &key, REGION, ocsp, &[]);
    let answer = create_trust_store(&emulator.router, &body).await;
    assert_eq!(
        answer.status, 201,
        "case {name}: expected HTTP 201 from CreateTrustStore, got {} with {}",
        answer.status, answer.raw
    );
    let created = answer.root();
    Store {
        name: name.to_string(),
        id: member(created, "Id", name, &answer.raw),
        arn: member(created, "Arn", name, &answer.raw),
        etag: answer.header("etag").to_string(),
        certificates,
        ocsp: ocsp.unwrap_or(false),
    }
}

/// The `CaCertificatesBundleSource` document an update carries as its payload, naming the
/// bundle at `bucket`/`key` in [`REGION`].
///
/// The member is bound to the request's PAYLOAD, so the body of an update is that union
/// alone rather than a request envelope around it.
fn bundle_payload(bucket: &str, key: &str) -> String {
    format!(
        "<CaCertificatesBundleSource xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CaCertificatesBundleS3Location><Bucket>{}</Bucket><Key>{}</Key><Region>{}</Region>\
         </CaCertificatesBundleS3Location></CaCertificatesBundleSource>",
        escape(bucket),
        escape(key),
        escape(REGION)
    )
}

/// `PUT /2020-05-31/trust-store/{Id}`, carrying the bundle source as the payload when the
/// row names one and the OCSP member as its header when the row states it.
async fn update_trust_store(
    router: &MultiServiceRouter,
    id: &str,
    bundle: Option<(&str, &str)>,
    ocsp: Option<bool>,
    if_match: Option<&str>,
) -> Answer {
    let body = bundle.map(|(bucket, key)| bundle_payload(bucket, key));
    let ocsp = ocsp.map(|value| value.to_string());
    let mut headers: Vec<(&str, &str)> = Vec::new();
    if let Some(if_match) = if_match {
        headers.push(("if-match", if_match));
    }
    if let Some(ocsp) = ocsp.as_deref() {
        headers.push(("UseClientCertificateOCSPEndpoint", ocsp));
    }
    call_with_headers(
        router,
        "PUT",
        // The id travels as ONE path label, encoded the way an SDK client encodes one.
        &format!("/{API_VERSION}/trust-store/{}", encode_label(id)),
        body.as_deref(),
        &headers,
    )
    .await
}

/// Assert the `TrustStore` document `answer` carries is `store`, in `status`, and answer its
/// `LastModifiedTime`.
fn assert_record(answer: &Answer, case: &str, context: &str, store: &Store, status: &str) -> String {
    let document = answer.root();
    let raw = answer.raw.as_str();

    for (name, expected) in [
        ("Id", store.id.clone()),
        ("Arn", store.arn.clone()),
        ("Name", store.name.clone()),
        ("Status", status.to_string()),
        ("NumberOfCaCertificates", store.certificates.to_string()),
        ("UseClientCertificateOCSPEndpoint", store.ocsp.to_string()),
    ] {
        let actual = member(document, name, case, raw);
        assert_eq!(
            actual, expected,
            "case {case:?} ({context}): expected {name} {expected:?}, got {actual:?}"
        );
    }
    assert!(
        document.at("Reason").is_none(),
        "case {case:?} ({context}): expected no Reason on a usable trust store, got {raw}"
    );
    member(document, "LastModifiedTime", case, raw)
}

/// Read the trust store back and assert it is `store`, in `status`, reporting the version
/// token `store` carries.
async fn assert_reads_back(
    router: &MultiServiceRouter,
    case: &str,
    context: &str,
    store: &Store,
    status: &str,
) {
    let read = get_trust_store(router, &store.id).await;
    assert_eq!(
        read.status, 200,
        "case {case:?} ({context}): expected HTTP 200 from GetTrustStore, got {} with {}",
        read.status, read.raw
    );
    assert_record(&read, case, context, store, status);
    assert_eq!(
        read.header("etag"),
        store.etag,
        "case {case:?} ({context}): expected the version token {:?}, got {:?}",
        store.etag,
        read.header("etag")
    );
}

/// Look again until the trust store `id` reports itself [`ACTIVE`] — the transition the CLOCK
/// owes it — or the deadline passes, which fails the case.
///
/// The CONDITION is what is waited for, never a span of the host's clock.
async fn awaited_active(router: &MultiServiceRouter, case: &str, id: &str) {
    let deadline = Instant::now() + READY_DEADLINE;
    loop {
        let read = get_trust_store(router, id).await;
        if read.root().find_text("Status").unwrap_or_default() == ACTIVE {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the trust store never reached {ACTIVE}, last answer {}",
            read.raw
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

// ---------------------------------------------------------------------------
// The rules
// ---------------------------------------------------------------------------

/// One row of the accepted-update table.
#[derive(Debug, Clone)]
struct Update {
    case: &'static str,
    /// How many certificates the trust store was created from.
    created_with: usize,
    /// `UseClientCertificateOCSPEndpoint` the create stated, or `None` to leave it out.
    created_ocsp: Option<bool>,
    /// How many certificates the bundle the update names holds.
    updated_with: usize,
    /// `UseClientCertificateOCSPEndpoint` the update states, or `None` to leave the header
    /// out.
    updated_ocsp: Option<bool>,
}

/// RULE: with the current version token an update replaces exactly the members the request
/// states, carries the rest over untouched, versions the record afresh, and puts the trust
/// store back in its non-terminal status — and the read afterwards answers from that one
/// record. The token the caller matched on no longer satisfies an `If-Match`.
///
/// The rows differ along how many certificates the new bundle holds (more than before, fewer
/// than before, as many as before) and along whether the request states the OCSP member at
/// all and which value it states — which is what the carry-over rule is asserted through.
#[tokio::test]
async fn an_update_replaces_exactly_the_members_the_request_states() {
    let emulator = Emulator::new();
    emulator.bundle_bucket().await;

    let rows: [Update; 5] = [
        Update {
            case: "a larger new bundle, the OCSP member left out",
            created_with: 2,
            created_ocsp: Some(false),
            updated_with: 4,
            updated_ocsp: None,
        },
        Update {
            case: "a smaller new bundle, the OCSP member left out",
            created_with: 3,
            created_ocsp: Some(true),
            updated_with: 1,
            updated_ocsp: None,
        },
        Update {
            case: "a new bundle and the OCSP member turned on",
            created_with: 1,
            created_ocsp: None,
            updated_with: 3,
            updated_ocsp: Some(true),
        },
        Update {
            case: "a new bundle and the OCSP member turned off",
            created_with: 2,
            created_ocsp: Some(true),
            updated_with: 2,
            updated_ocsp: Some(false),
        },
        Update {
            case: "a bundle of the same size, nothing else stated",
            created_with: 2,
            created_ocsp: Some(true),
            updated_with: 2,
            updated_ocsp: None,
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        let mut store = trust_store(
            &emulator,
            &format!("updated-{index}"),
            row.created_with,
            row.created_ocsp,
        )
        .await;

        // The trust store is let reach its terminal status first, so the update putting it
        // back in the transient one is observable rather than coinciding with the create's
        // own propagation.
        awaited_active(&emulator.router, case, &store.id).await;

        let key = format!("updated-{index}-new.pem");
        emulator.stage(&key, &ca_bundle(row.updated_with)).await;

        let before = store.clone();
        let updated = update_trust_store(
            &emulator.router,
            &store.id,
            Some((BUNDLE_BUCKET, key.as_str())),
            row.updated_ocsp,
            Some(&store.etag),
        )
        .await;
        assert_eq!(
            updated.status, 200,
            "case {case:?}: expected HTTP 200 from UpdateTrustStore, got {} with {}",
            updated.status, updated.raw
        );

        // What the request STATED is replaced; what it left out stands.
        store.certificates = row.updated_with;
        store.ocsp = row.updated_ocsp.unwrap_or(before.ocsp);
        store.etag = updated.header("etag").to_string();

        assert!(
            is_version_token(&store.etag),
            "case {case:?}: expected a version token in the ETag header, got {:?}",
            store.etag
        );
        assert_ne!(
            store.etag, before.etag,
            "case {case:?}: an accepted update must version the record afresh, got {:?}",
            store.etag
        );
        assert_record(&updated, case, "the update", &store, PENDING);

        // The read answers from the record the update wrote, not from the request.
        assert_reads_back(&emulator.router, case, "after the update", &store, PENDING).await;

        // The token the caller matched on is spent: it no longer satisfies an `If-Match`.
        let stale = update_trust_store(
            &emulator.router,
            &store.id,
            Some((BUNDLE_BUCKET, key.as_str())),
            None,
            Some(&before.etag),
        )
        .await;
        assert_eq!(
            stale.status, 412,
            "case {case:?}: expected HTTP 412 for the replaced version token, got {} with {}",
            stale.status, stale.raw
        );

        // The clock owes the rewritten trust store its terminal status, with no call asking.
        awaited_active(&emulator.router, case, &store.id).await;
        let mut settled = store.clone();
        settled.etag = store.etag.clone();
        assert_reads_back(
            &emulator.router,
            case,
            "once the clock has settled it",
            &settled,
            ACTIVE,
        )
        .await;
    }
}

/// RULE: an `If-Match` that is not the trust store's current version token refuses the update
/// and writes NOTHING — a value that is no version token at all with `InvalidIfMatchVersion`
/// (400), a well-formed token that is not the current one with `PreconditionFailed` (412).
/// The precondition is evaluated BEFORE any bundle is read, so a refused update does not
/// apply the bundle it carried; afterwards the record stands in every member, and the update
/// lands with the token a read reports.
#[tokio::test]
async fn an_if_match_that_is_not_the_current_version_refuses_the_update() {
    let emulator = Emulator::new();
    emulator.bundle_bucket().await;
    let mut store = trust_store(&emulator, "guarded-trust-store", 2, Some(false)).await;
    awaited_active(&emulator.router, "the guarded trust store", &store.id).await;
    emulator.stage("guarded-new.pem", &ca_bundle(5)).await;

    // (case, the `If-Match` the row sends, status, code, message). What makes a row a 400 is
    // its FORM; what makes one a 412 is that it is a token the record does not hold.
    let rows: [(&str, String, u16, &str, &str); 8] = [
        (
            "a well-formed token nothing holds",
            UNHELD_VERSION_TOKEN.to_string(),
            412,
            "PreconditionFailed",
            PRECONDITION_FAILED,
        ),
        (
            "that same token quoted",
            format!("\"{UNHELD_VERSION_TOKEN}\""),
            412,
            "PreconditionFailed",
            PRECONDITION_FAILED,
        ),
        (
            "that same token as a weak tag",
            format!("W/\"{UNHELD_VERSION_TOKEN}\""),
            412,
            "PreconditionFailed",
            PRECONDITION_FAILED,
        ),
        (
            "a value wider than a version token",
            "E000000000FAKEBOGUS".to_string(),
            400,
            "InvalidIfMatchVersion",
            INVALID_IF_MATCH_VERSION,
        ),
        (
            "a value narrower than a version token",
            "E1ABCDEF".to_string(),
            400,
            "InvalidIfMatchVersion",
            INVALID_IF_MATCH_VERSION,
        ),
        (
            "the right width in the wrong alphabet",
            UNHELD_VERSION_TOKEN.to_lowercase(),
            400,
            "InvalidIfMatchVersion",
            INVALID_IF_MATCH_VERSION,
        ),
        (
            "prose",
            "not a version at all".to_string(),
            400,
            "InvalidIfMatchVersion",
            INVALID_IF_MATCH_VERSION,
        ),
        (
            "the trust store's own id",
            store.id.clone(),
            400,
            "InvalidIfMatchVersion",
            INVALID_IF_MATCH_VERSION,
        ),
    ];

    for (case, if_match, status, code, message) in rows {
        // Each refused row carries a bundle AND the OCSP member, so a refusal that wrote
        // anything would show up in the read below.
        let refused = update_trust_store(
            &emulator.router,
            &store.id,
            Some((BUNDLE_BUCKET, "guarded-new.pem")),
            Some(true),
            Some(&if_match),
        )
        .await;
        assert_eq!(
            refused.status, status,
            "case {case:?}: expected HTTP {status} for If-Match {if_match:?}, got {} with {}",
            refused.status, refused.raw
        );
        assert_eq!(
            refused.error_code(),
            code,
            "case {case:?}: expected {code}, got {}",
            refused.raw
        );
        let stated = refused.root().find_text("Message").unwrap_or_default();
        assert_eq!(
            stated, message,
            "case {case:?}: expected the message {message:?}, got {stated:?}"
        );

        // Nothing moved: not the certificate count, not the revocation behaviour, not the
        // status the clock had already settled, not the version token.
        assert_reads_back(&emulator.router, case, "after the refusal", &store, ACTIVE).await;
    }

    // The version token a read reports is the one that lands, after every one of those
    // refusals.
    let landed = update_trust_store(
        &emulator.router,
        &store.id,
        Some((BUNDLE_BUCKET, "guarded-new.pem")),
        Some(true),
        Some(&store.etag),
    )
    .await;
    assert_eq!(
        landed.status, 200,
        "expected HTTP 200 with the current version token, got {} with {}",
        landed.status, landed.raw
    );
    store.certificates = 5;
    store.ocsp = true;
    store.etag = landed.header("etag").to_string();
    assert_record(&landed, "after the refusals", "the update", &store, PENDING);
    assert_reads_back(
        &emulator.router,
        "after the refusals",
        "the read",
        &store,
        PENDING,
    )
    .await;
}

/// RULE: the `Id` is "the trust store ID" — the token the create minted and the key the
/// record is held under, not the identifier the reads of this resource resolve — and it is
/// resolved BEFORE the precondition, so an id no stored trust store answers to is
/// `EntityNotFound` (404) whatever the request's `If-Match` carries.
#[tokio::test]
async fn an_id_no_trust_store_answers_to_is_not_found() {
    let emulator = Emulator::new();
    emulator.bundle_bucket().await;
    // One trust store that DOES exist, so a refusal below cannot be the store being empty.
    let present = trust_store(&emulator, "present-trust-store", 1, Some(false)).await;
    awaited_active(&emulator.router, "the trust store that exists", &present.id).await;
    // A bundle that READS, so the only thing at fault in every row below is the id.
    emulator.stage("absent-target.pem", &ca_bundle(2)).await;

    let absent: [(&str, String); 4] = [
        (
            "an id of the minted shape nothing holds",
            "ts_0123456789ABC".to_string(),
        ),
        ("a name never created", "absent-trust-store".to_string()),
        (
            "the name of the trust store that exists, which is no id",
            present.name.clone(),
        ),
        ("an id of no form this service mints", "1234".to_string()),
    ];

    for (case, id) in absent {
        for (spelling, if_match) in [
            ("a token another record holds", present.etag.clone()),
            ("no version token at all", "bogus".to_string()),
            ("a token nothing holds", UNHELD_VERSION_TOKEN.to_string()),
        ] {
            let refused = update_trust_store(
                &emulator.router,
                &id,
                Some((BUNDLE_BUCKET, "absent-target.pem")),
                Some(true),
                Some(&if_match),
            )
            .await;
            assert_eq!(
                refused.status, 404,
                "case {case:?} ({spelling}): expected HTTP 404 for {id:?}, got {} with {}",
                refused.status, refused.raw
            );
            assert_eq!(
                refused.error_code(),
                "EntityNotFound",
                "case {case:?} ({spelling}): expected EntityNotFound, got {}",
                refused.raw
            );
            let stated = refused.root().find_text("Message").unwrap_or_default();
            assert_eq!(
                stated, TRUST_STORE_NOT_FOUND,
                "case {case:?} ({spelling}): expected the message {TRUST_STORE_NOT_FOUND:?}, got \
                 {stated:?}"
            );
        }
    }

    // The trust store that does exist is untouched by every one of those refusals.
    assert_reads_back(
        &emulator.router,
        "the trust store that exists",
        "after the refusals",
        &present,
        ACTIVE,
    )
    .await;
}

/// RULE: `NumberOfCaCertificates` is a property of the bundle's bytes, so a bundle source the
/// object store serves nothing usable for refuses the update and the record stands exactly as
/// it was — `InvalidArgument` (400) where no bundle can be read at the address the request
/// named, `EntityNotFound` (404) where the object store answers that it holds nothing there.
/// A trust store never reports certificates that were never read.
///
/// The rows cover both halves of the address and every way the bytes can fall short, with the
/// OCSP member stated alongside each one so a refusal that wrote the members it COULD apply
/// would show up.
#[tokio::test]
async fn a_bundle_that_cannot_be_read_leaves_the_trust_store_as_it_was() {
    let emulator = Emulator::new();
    emulator.bundle_bucket().await;
    let store = trust_store(&emulator, "unchanged-trust-store", 2, Some(false)).await;
    awaited_active(&emulator.router, "the trust store under test", &store.id).await;

    emulator.stage("prose.pem", "not a certificate bundle").await;
    emulator.stage("empty.pem", "").await;
    emulator
        .stage(
            "other-block.pem",
            &pem::encode(&pem::Pem::new("PUBLIC KEY".to_string(), b"0123456789".to_vec())),
        )
        .await;

    // (case, bucket, key, status, code, message)
    let rows: [(&str, &str, &str, u16, &str, &str); 7] = [
        (
            "a bucket name Amazon S3 holds no bucket under",
            "Invalid_Bucket_Name",
            "unchanged-trust-store.pem",
            400,
            "InvalidArgument",
            INVALID_BUNDLE,
        ),
        (
            "the blank key",
            BUNDLE_BUCKET,
            " ",
            400,
            "InvalidArgument",
            INVALID_BUNDLE,
        ),
        (
            "an object holding no bytes at all",
            BUNDLE_BUCKET,
            "empty.pem",
            400,
            "InvalidArgument",
            INVALID_BUNDLE,
        ),
        (
            "an object that is no PEM at all",
            BUNDLE_BUCKET,
            "prose.pem",
            400,
            "InvalidArgument",
            INVALID_BUNDLE,
        ),
        (
            "a PEM object carrying no certificate",
            BUNDLE_BUCKET,
            "other-block.pem",
            400,
            "InvalidArgument",
            INVALID_BUNDLE,
        ),
        (
            "a bucket the object store does not hold",
            "cloudfront-trust-store-absent",
            "unchanged-trust-store.pem",
            404,
            "EntityNotFound",
            BUNDLE_NOT_FOUND,
        ),
        (
            "an object the bucket does not hold",
            BUNDLE_BUCKET,
            "never-staged.pem",
            404,
            "EntityNotFound",
            BUNDLE_NOT_FOUND,
        ),
    ];

    for (case, bucket, key, status, code, message) in rows {
        let refused = update_trust_store(
            &emulator.router,
            &store.id,
            Some((bucket, key)),
            Some(true),
            Some(&store.etag),
        )
        .await;
        assert_eq!(
            refused.status, status,
            "case {case:?}: expected HTTP {status}, got {} with {}",
            refused.status, refused.raw
        );
        assert_eq!(
            refused.error_code(),
            code,
            "case {case:?}: expected {code}, got {}",
            refused.raw
        );
        let stated = refused.root().find_text("Message").unwrap_or_default();
        assert_eq!(
            stated, message,
            "case {case:?}: expected the message {message:?}, got {stated:?}"
        );

        assert_reads_back(&emulator.router, case, "after the refusal", &store, ACTIVE).await;
    }
}
