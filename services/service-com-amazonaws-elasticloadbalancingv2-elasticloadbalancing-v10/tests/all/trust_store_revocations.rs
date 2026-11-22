// SPDX-License-Identifier: CC0-1.0

//! The ELBv2 trust-store REVOCATION plane, end to end over the wire.
//!
//! Every request goes through the router exactly as a client's would — awsQuery form body
//! in, `<{Operation}Response>` XML out — so each case exercises routing, the awsQuery
//! (de)serialization, the handler, the persisted state and the serializer. Amazon S3 is
//! MOUNTED beside this service on the same engine (the harness
//! `crate::trust_store_lifecycle` builds), because a revocation file is an object that
//! store holds: every list `AddTrustStoreRevocations` reads is one this suite really staged
//! with `PutObject`, and each is a real certificate revocation list signed by a real CA this
//! suite generated.
//!
//! The rules under test, each asserted over a SET of inputs rather than one request:
//!
//! 1. an add PERSISTS one revocation file on the trust store, under the revocation id the
//!    trust store mints for it, reporting the entries READ out of the object's bytes; a
//!    later describe answers from that record, the trust store's `TotalRevokedEntries` is
//!    the sum over its files, and the content read hands back an address that serves the
//!    very bytes staged;
//! 2. a call whose revocation file could not be READ is refused and changes nothing:
//!    `RevocationContentNotFound` where nothing can be fetched at the address,
//!    `InvalidRevocationContent` where what is there is no revocation list;
//! 3. a trust store holds at most the published number of revocation files, and the
//!    quota refusal names that number — the same number `DescribeAccountLimits` reports;
//! 4. one call adds one file, so a request naming more is refused as a bad request;
//! 5. every call of this plane that addresses a trust store by an ARN this account holds
//!    none under is `TrustStoreNotFound` naming that ARN, and a call naming a revocation id
//!    the trust store holds no file under is `RevocationIdNotFound` naming that id;
//! 6. a removal releases the files it names and nothing else — all or nothing — and the
//!    ids it released are never handed out again;
//! 7. a paged describe walks every file of the trust store exactly once.

use service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::state::{
    revocation_id_not_found_message, too_many_trust_store_revocation_entries_message,
    trust_store_arn_not_found_message, INVALID_REVOCATION_CONTENT_MESSAGE,
    REVOCATIONS_PER_TRUST_STORE, REVOCATION_CONTENT_NOT_FOUND_MESSAGE,
    REVOCATION_TYPE_CRL,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::provisioning_loop::{account, action, call, first_text, members, p, Reply, REGION};
use crate::target_group_attributes_and_modification::unescaped;
use crate::trust_store_lifecycle::{emulator_with_bundles, Emulator, BUNDLE_BUCKET};

/// The awsQuery error codes the model declares for the revocation operations
/// (`smithy_query(query="operation",operation="AddTrustStoreRevocations,\
/// DescribeTrustStoreRevocations,RemoveTrustStoreRevocations,\
/// GetTrustStoreRevocationContent")`: each error's `awsQueryError` code).
const TRUST_STORE_NOT_FOUND: &str = "TrustStoreNotFound";
const REVOCATION_CONTENT_NOT_FOUND: &str = "RevocationContentNotFound";
const INVALID_REVOCATION_CONTENT: &str = "InvalidRevocationContent";
const TOO_MANY_REVOCATION_ENTRIES: &str = "TooManyTrustStoreRevocationEntries";
const REVOCATION_ID_NOT_FOUND: &str = "RevocationIdNotFound";

/// The code this service answers a request whose VALUES it cannot accept with, for a rule
/// its model states in prose rather than as an error shape.
const VALIDATION_ERROR: &str = "ValidationError";

// ---------------------------------------------------------------------------
// Real certification authorities, and the revocation lists they sign
// ---------------------------------------------------------------------------

/// A certification authority: its self-signed CA certificate and the key pair behind it.
///
/// GENERATED here (`rcgen`), so a trust store below is built from a real CA bundle and every
/// revocation list is one that CA really signed — what a caller stages in Amazon S3.
pub(crate) fn authority() -> (rcgen::Certificate, rcgen::KeyPair) {
    let key = rcgen::KeyPair::generate().expect("a key pair");
    let mut params =
        rcgen::CertificateParams::new(vec!["ca.example.com".to_string()]).expect("parameters");
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let certificate = params.self_signed(&key).expect("a self-signed certificate");
    (certificate, key)
}

/// A revocation list revoking `revoked` certificates, signed by the authority `certificate`
/// and `key`, PEM encoded as a caller stages one.
pub(crate) fn crl_pem(
    certificate: &rcgen::Certificate,
    key: &rcgen::KeyPair,
    revoked: usize,
) -> String {
    let params = rcgen::CertificateRevocationListParams {
        this_update: rcgen::date_time_ymd(2026, 1, 1),
        next_update: rcgen::date_time_ymd(2026, 2, 1),
        crl_number: rcgen::SerialNumber::from(1_u64),
        issuing_distribution_point: None,
        revoked_certs: (0..revoked)
            .map(|index| rcgen::RevokedCertParams {
                serial_number: rcgen::SerialNumber::from(index as u64 + 1),
                revocation_time: rcgen::date_time_ymd(2026, 1, 2),
                reason_code: Some(rcgen::RevocationReason::KeyCompromise),
                invalidity_date: None,
            })
            .collect(),
        key_identifier_method: rcgen::KeyIdMethod::Sha256,
    };
    params
        .signed_by(certificate, key)
        .expect("a signed revocation list")
        .pem()
        .expect("the list in PEM")
}

// ---------------------------------------------------------------------------
// The requests of this plane
// ---------------------------------------------------------------------------

/// A trust store built from `authority`'s own certificate, staged under `name`, and its ARN.
pub(crate) async fn trust_store(
    emulator: &Emulator,
    name: &str,
    certificate: &rcgen::Certificate,
) -> String {
    let key = format!("{name}-bundle.pem");
    emulator.stage(&key, &certificate.pem()).await;
    let mut form = action("CreateTrustStore");
    form.push(p("Name", name));
    form.push(p("CaCertificatesBundleS3Bucket", BUNDLE_BUCKET));
    form.push(p("CaCertificatesBundleS3Key", &key));
    let created = call(&emulator.router, &form).await;
    assert_eq!(
        created.status, 200,
        "case {name:?}: creating the trust store must succeed, got {} body {}",
        created.status, created.body
    );
    first_text(&created.body, "TrustStoreArn")
        .unwrap_or_else(|| panic!("case {name:?}: no TrustStoreArn in {}", created.body))
}

/// `AddTrustStoreRevocations` for the object `bucket`/`key`, with the extra fields `extra`.
pub(crate) async fn add_revocation(
    emulator: &Emulator,
    trust_store_arn: &str,
    bucket: &str,
    key: &str,
    extra: &[(String, String)],
) -> Reply {
    let mut form = action("AddTrustStoreRevocations");
    form.push(p("TrustStoreArn", trust_store_arn));
    form.push(p("RevocationContents.member.1.S3Bucket", bucket));
    form.push(p("RevocationContents.member.1.S3Key", key));
    form.extend_from_slice(extra);
    call(&emulator.router, &form).await
}

/// `DescribeTrustStoreRevocations` for `trust_store_arn`, with the extra fields `fields`.
pub(crate) async fn describe_revocations(
    emulator: &Emulator,
    trust_store_arn: &str,
    fields: &[(String, String)],
) -> Reply {
    let mut form = action("DescribeTrustStoreRevocations");
    form.push(p("TrustStoreArn", trust_store_arn));
    form.extend_from_slice(fields);
    call(&emulator.router, &form).await
}

/// The revocation ids a request names, as the `RevocationIds.member.N` form fields a client
/// submits them in — the one member both the removal and the read select by.
///
/// `pub(crate)`: the read suite beside this one (`crate::trust_store_revocation_reads`)
/// selects by the same member, so there is ONE spelling of it.
pub(crate) fn revocation_id_fields(revocation_ids: &[i64]) -> Vec<(String, String)> {
    revocation_ids
        .iter()
        .enumerate()
        .map(|(index, revocation_id)| {
            p(
                &format!("RevocationIds.member.{}", index + 1),
                &revocation_id.to_string(),
            )
        })
        .collect()
}

/// Walk every revocation file a read reports for `fields`, a page of at most `page_size` at
/// a time and following each `NextMarker` the way a paginating client does, answering the
/// revocation ids in the order they were walked. `expected` bounds how many pages the walk
/// may take, so a walk that never ends fails the case instead of looping.
///
/// `pub(crate)`: the read suite beside this one walks a NARROWED selection the same way, so
/// there is ONE walk rather than two that could drift on how a page is asked for or on when
/// the walk is over.
pub(crate) async fn walk_revocations(
    emulator: &Emulator,
    trust_store_arn: &str,
    fields: &[(String, String)],
    page_size: usize,
    expected: usize,
) -> Vec<i64> {
    let mut walked: Vec<i64> = Vec::new();
    let mut marker: Option<String> = None;
    for _ in 0..expected + 2 {
        let mut page_fields = fields.to_vec();
        page_fields.push(p("PageSize", &page_size.to_string()));
        if let Some(marker) = marker.as_deref() {
            page_fields.push(p("Marker", marker));
        }
        let page = describe_revocations(emulator, trust_store_arn, &page_fields).await;
        assert_eq!(
            page.status, 200,
            "case PageSize {page_size}: expected 200 for a page, got {} body {}",
            page.status, page.body
        );
        let reported = reported_revocations(&page);
        assert!(
            reported.len() <= page_size,
            "case PageSize {page_size}: a page holds at most {page_size}, got {reported:?}"
        );
        walked.extend(reported.iter().map(|entry| entry.0));
        marker = page.text("NextMarker");
        if marker.is_none() {
            break;
        }
    }
    assert_eq!(
        marker, None,
        "case PageSize {page_size}: the walk must end with no marker"
    );
    walked
}

/// `RemoveTrustStoreRevocations` for `revocation_ids` of `trust_store_arn`.
async fn remove_revocations(
    emulator: &Emulator,
    trust_store_arn: &str,
    revocation_ids: &[i64],
) -> Reply {
    let mut form = action("RemoveTrustStoreRevocations");
    form.push(p("TrustStoreArn", trust_store_arn));
    form.extend_from_slice(&revocation_id_fields(revocation_ids));
    call(&emulator.router, &form).await
}

/// `GetTrustStoreRevocationContent` for one revocation file of `trust_store_arn`.
async fn get_revocation_content(
    emulator: &Emulator,
    trust_store_arn: &str,
    revocation_id: i64,
) -> Reply {
    let mut form = action("GetTrustStoreRevocationContent");
    form.push(p("TrustStoreArn", trust_store_arn));
    form.push(p("RevocationId", &revocation_id.to_string()));
    call(&emulator.router, &form).await
}

/// `DescribeTrustStores` for one trust store.
async fn describe_trust_store(emulator: &Emulator, trust_store_arn: &str) -> Reply {
    let mut form = action("DescribeTrustStores");
    form.push(p("TrustStoreArns.member.1", trust_store_arn));
    call(&emulator.router, &form).await
}

/// Follow a location this service published, the way a client follows one it was handed:
/// the address exactly as reported, the `Host` it names, and NO credential of the client's
/// own — a pre-signed address carries its own authentication in the query.
async fn fetch(router: &MultiServiceRouter, uri: &str) -> (u16, Vec<u8>) {
    let host = uri
        .split_once("://")
        .and_then(|(_scheme, rest)| rest.split('/').next())
        .unwrap_or_else(|| panic!("a published location names a host, got {uri:?}"))
        .to_string();
    let request = http::Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", host)
        .body(bytes::Bytes::new())
        .expect("the request builds");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        response.body().as_ref().to_vec(),
    )
}

/// The `<Message>` of an error envelope, with the XML entities the serializer writes
/// resolved, so an assertion compares the VALUE rather than its encoding.
fn message(reply: &Reply) -> String {
    unescaped(&reply.text("Message").unwrap_or_default())
}

/// The revocation files a describe reported, as `(RevocationId, RevocationType,
/// NumberOfRevokedEntries, TrustStoreArn)`, in the order it reported them.
pub(crate) fn reported_revocations(reply: &Reply) -> Vec<(i64, String, i64, String)> {
    members(&reply.body, "TrustStoreRevocations")
        .iter()
        .map(|entry| {
            (
                first_text(entry, "RevocationId")
                    .and_then(|id| id.parse::<i64>().ok())
                    .unwrap_or_default(),
                first_text(entry, "RevocationType").unwrap_or_default(),
                first_text(entry, "NumberOfRevokedEntries")
                    .and_then(|count| count.parse::<i64>().ok())
                    .unwrap_or_default(),
                first_text(entry, "TrustStoreArn").unwrap_or_default(),
            )
        })
        .collect()
}

/// What the trust store itself reports as the number of revoked certificates in it.
pub(crate) async fn total_revoked_entries(emulator: &Emulator, trust_store_arn: &str) -> i64 {
    let described = describe_trust_store(emulator, trust_store_arn).await;
    first_text(&described.body, "TotalRevokedEntries")
        .and_then(|total| total.parse::<i64>().ok())
        .unwrap_or_else(|| panic!("no TotalRevokedEntries in {}", described.body))
}

/// An ARN of a trust store this account holds none under, named after `label`.
fn absent_trust_store_arn(label: &str) -> String {
    format!(
        "arn:aws:elasticloadbalancing:{REGION}:{}:truststore/{label}/0123456789abcdef",
        account()
    )
}

// ---------------------------------------------------------------------------
// RULE 1: an add persists what it read, and the reads answer from it
// ---------------------------------------------------------------------------

/// One revocation file a caller adds: how the object is keyed, what the file holds, how many
/// certificates the lists in it revoke, and whether the request names the revocation type.
struct Added {
    case: &'static str,
    key: &'static str,
    /// How many certificates each list in the staged file revokes.
    lists: &'static [usize],
    /// Prose around the lists, which a PEM file may carry.
    prose: bool,
    /// Whether the request names `RevocationType` explicitly.
    names_type: bool,
}

/// The rows differ along everything the rule generalises over: how many entries the list
/// revokes, how many lists the file holds, whether anything surrounds them, the shape of the
/// object key, and whether the request names the one modelled revocation type.
const ADDED_ROWS: &[Added] = &[
    Added {
        case: "a list revoking one certificate, a flat key, the type named",
        key: "revocations/one.pem",
        lists: &[1],
        prose: false,
        names_type: true,
    },
    Added {
        case: "a list revoking three certificates, the type left out",
        key: "revocations/nested/three.crl.pem",
        lists: &[3],
        prose: false,
        names_type: false,
    },
    Added {
        case: "a list that revokes nothing",
        key: "empty-list.pem",
        lists: &[0],
        prose: false,
        names_type: true,
    },
    Added {
        case: "two lists in one file, beside prose",
        key: "revocations/two.lists.pem",
        lists: &[2, 4],
        prose: true,
        names_type: false,
    },
];

#[tokio::test]
async fn an_added_revocation_file_is_reported_from_the_list_it_was_built_from() {
    let emulator = emulator_with_bundles().await;
    let (certificate, key) = authority();
    let arn = trust_store(&emulator, "ts-revocations", &certificate).await;

    let mut expected_total = 0_i64;
    for (index, row) in ADDED_ROWS.iter().enumerate() {
        let mut staged = String::new();
        if row.prose {
            staged.push_str("Issued by the example CA\n");
        }
        for revoked in row.lists {
            staged.push_str(&crl_pem(&certificate, &key, *revoked));
        }
        emulator.stage(row.key, &staged).await;

        let extra = if row.names_type {
            vec![p(
                "RevocationContents.member.1.RevocationType",
                REVOCATION_TYPE_CRL,
            )]
        } else {
            Vec::new()
        };
        let added = add_revocation(&emulator, &arn, BUNDLE_BUCKET, row.key, &extra).await;
        assert_eq!(
            added.status, 200,
            "case {:?}: expected 200 from AddTrustStoreRevocations, got {} body {}",
            row.case, added.status, added.body
        );

        // The id is the trust store's own, minted in sequence; the entries are the file's.
        let expected_id = index as i64 + 1;
        let expected_entries: i64 = row.lists.iter().map(|revoked| *revoked as i64).sum();
        let answered = reported_revocations(&added);
        assert_eq!(
            answered,
            vec![(
                expected_id,
                REVOCATION_TYPE_CRL.to_string(),
                expected_entries,
                arn.clone()
            )],
            "case {:?}: expected exactly the file just added, got {answered:?} in {}",
            row.case,
            added.body
        );

        // What a read answers from the record: the whole list, and the one file narrowed by
        // its own id.
        let described = describe_revocations(&emulator, &arn, &[]).await;
        let held = reported_revocations(&described);
        assert_eq!(
            held.len(),
            index + 1,
            "case {:?}: expected {} revocation file(s) held, got {held:?}",
            row.case,
            index + 1
        );
        assert!(
            held.contains(&(
                expected_id,
                REVOCATION_TYPE_CRL.to_string(),
                expected_entries,
                arn.clone()
            )),
            "case {:?}: expected the added file among {held:?}",
            row.case
        );
        let narrowed = describe_revocations(
            &emulator,
            &arn,
            &[p("RevocationIds.member.1", &expected_id.to_string())],
        )
        .await;
        assert_eq!(
            reported_revocations(&narrowed),
            vec![(
                expected_id,
                REVOCATION_TYPE_CRL.to_string(),
                expected_entries,
                arn.clone()
            )],
            "case {:?}: a read narrowed to one id reports that file alone, got {}",
            row.case,
            narrowed.body
        );

        // The trust store's own count follows the files it holds.
        expected_total += expected_entries;
        let total = total_revoked_entries(&emulator, &arn).await;
        assert_eq!(
            total, expected_total,
            "case {:?}: expected TotalRevokedEntries {expected_total}, got {total}",
            row.case
        );

        // And the content read hands back an address that serves the staged bytes.
        let content = get_revocation_content(&emulator, &arn, expected_id).await;
        assert_eq!(
            content.status, 200,
            "case {:?}: expected 200 from GetTrustStoreRevocationContent, got {} body {}",
            row.case, content.status, content.body
        );
        let location = unescaped(&first_text(&content.body, "Location").unwrap_or_else(|| {
            panic!("case {:?}: no Location in {}", row.case, content.body)
        }));
        assert!(
            location.contains(&format!("/{BUNDLE_BUCKET}/{}", row.key)),
            "case {:?}: expected an address of the staged object, got {location:?}",
            row.case
        );
        let (status, served) = fetch(&emulator.router, &location).await;
        assert_eq!(
            status, 200,
            "case {:?}: following {location:?} must serve the file, got {status}",
            row.case
        );
        assert_eq!(
            String::from_utf8_lossy(&served),
            staged,
            "case {:?}: expected the staged bytes back from {location:?}",
            row.case
        );
    }
}

// ---------------------------------------------------------------------------
// RULE 2: a file that could not be read refuses the call and stores nothing
// ---------------------------------------------------------------------------

/// One revocation file a call cannot be served from: how the case addresses it, and the
/// refusal it earns.
struct Unreadable {
    case: &'static str,
    bucket: String,
    key: String,
    /// The object staged at that key, if any.
    staged: Option<String>,
    code: &'static str,
    message: &'static str,
}

/// The rows span both halves of the split the model declares: nothing can be FETCHED at the
/// address, or what is there is no revocation LIST.
fn unreadable_rows() -> Vec<Unreadable> {
    vec![
        Unreadable {
            case: "a bucket the object store holds no bucket under",
            bucket: "elbv2-revocations-no-such-bucket".to_string(),
            key: "absent.pem".to_string(),
            staged: None,
            code: REVOCATION_CONTENT_NOT_FOUND,
            message: REVOCATION_CONTENT_NOT_FOUND_MESSAGE,
        },
        Unreadable {
            case: "a key the bucket holds nothing at",
            bucket: BUNDLE_BUCKET.to_string(),
            key: "nothing/here.pem".to_string(),
            staged: None,
            code: REVOCATION_CONTENT_NOT_FOUND,
            message: REVOCATION_CONTENT_NOT_FOUND_MESSAGE,
        },
        Unreadable {
            case: "a bucket name Amazon S3 holds no bucket under at all",
            bucket: "Not A Bucket Name".to_string(),
            key: "absent.pem".to_string(),
            staged: None,
            code: REVOCATION_CONTENT_NOT_FOUND,
            message: REVOCATION_CONTENT_NOT_FOUND_MESSAGE,
        },
        Unreadable {
            case: "an object holding prose rather than PEM",
            bucket: BUNDLE_BUCKET.to_string(),
            key: "not-a-crl.pem".to_string(),
            staged: Some("these are not revocations\n".to_string()),
            code: INVALID_REVOCATION_CONTENT,
            message: INVALID_REVOCATION_CONTENT_MESSAGE,
        },
        Unreadable {
            case: "an object holding a certificate rather than a revocation list",
            bucket: BUNDLE_BUCKET.to_string(),
            key: "certificate-only.pem".to_string(),
            staged: Some(
                "-----BEGIN CERTIFICATE-----\nMTIzNDU2Nzg5MA==\n-----END CERTIFICATE-----\n"
                    .to_string(),
            ),
            code: INVALID_REVOCATION_CONTENT,
            message: INVALID_REVOCATION_CONTENT_MESSAGE,
        },
        Unreadable {
            case: "an object holding a revocation-list block with no X.509 structure",
            bucket: BUNDLE_BUCKET.to_string(),
            key: "corrupt.crl.pem".to_string(),
            staged: Some(
                "-----BEGIN X509 CRL-----\nMTIzNDU2Nzg5MA==\n-----END X509 CRL-----\n".to_string(),
            ),
            code: INVALID_REVOCATION_CONTENT,
            message: INVALID_REVOCATION_CONTENT_MESSAGE,
        },
        Unreadable {
            case: "an object holding no bytes at all",
            bucket: BUNDLE_BUCKET.to_string(),
            key: "empty.crl".to_string(),
            staged: Some(String::new()),
            code: INVALID_REVOCATION_CONTENT,
            message: INVALID_REVOCATION_CONTENT_MESSAGE,
        },
    ]
}

#[tokio::test]
async fn an_add_whose_file_cannot_be_read_is_refused_and_stores_nothing() {
    let emulator = emulator_with_bundles().await;
    let (certificate, _key) = authority();
    let arn = trust_store(&emulator, "ts-unreadable", &certificate).await;

    for row in unreadable_rows() {
        if let Some(body) = row.staged.as_deref() {
            emulator.stage(&row.key, body).await;
        }
        let refused = add_revocation(&emulator, &arn, &row.bucket, &row.key, &[]).await;
        assert_eq!(
            (refused.status, refused.error_code().as_str()),
            (400, row.code),
            "case {:?}: expected 400 {}, got {} body {}",
            row.case,
            row.code,
            refused.status,
            refused.body
        );
        assert_eq!(
            message(&refused),
            row.message,
            "case {:?}: expected the modelled message, got {}",
            row.case,
            message(&refused)
        );

        // Nothing was added: the trust store holds no revocation file and revokes nothing.
        let described = describe_revocations(&emulator, &arn, &[]).await;
        assert_eq!(
            reported_revocations(&described),
            Vec::new(),
            "case {:?}: a refused add must store nothing, got {}",
            row.case,
            described.body
        );
        let total = total_revoked_entries(&emulator, &arn).await;
        assert_eq!(
            total, 0,
            "case {:?}: a refused add must leave TotalRevokedEntries 0, got {total}",
            row.case
        );
    }
}

// ---------------------------------------------------------------------------
// RULE 3: the quota bounds how many files a trust store holds
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_quota_bounds_how_many_revocation_files_a_trust_store_holds() {
    let emulator = emulator_with_bundles().await;
    let (certificate, key) = authority();
    let arn = trust_store(&emulator, "ts-quota", &certificate).await;
    let list = crl_pem(&certificate, &key, 1);

    // Up to the quota, every add is served.
    for index in 0..REVOCATIONS_PER_TRUST_STORE {
        let object = format!("quota/list-{index}.pem");
        emulator.stage(&object, &list).await;
        let added = add_revocation(&emulator, &arn, BUNDLE_BUCKET, &object, &[]).await;
        assert_eq!(
            added.status, 200,
            "case file {index}: expected 200 while under the quota, got {} body {}",
            added.status, added.body
        );
    }

    // Past it the add is refused, the quota's own number is named, and nothing is stored.
    let object = "quota/one-too-many.pem";
    emulator.stage(object, &list).await;
    let refused = add_revocation(&emulator, &arn, BUNDLE_BUCKET, object, &[]).await;
    assert_eq!(
        (refused.status, refused.error_code().as_str()),
        (400, TOO_MANY_REVOCATION_ENTRIES),
        "expected 400 {TOO_MANY_REVOCATION_ENTRIES} past the quota, got {} body {}",
        refused.status,
        refused.body
    );
    assert_eq!(
        message(&refused),
        too_many_trust_store_revocation_entries_message(),
        "expected the quota refusal to name the quota, got {}",
        message(&refused)
    );
    let held = reported_revocations(&describe_revocations(&emulator, &arn, &[]).await);
    assert_eq!(
        held.len(),
        REVOCATIONS_PER_TRUST_STORE,
        "a refused add stores nothing, so the trust store still holds {REVOCATIONS_PER_TRUST_STORE} \
         file(s), got {held:?}"
    );
    let total = total_revoked_entries(&emulator, &arn).await;
    assert_eq!(
        total, REVOCATIONS_PER_TRUST_STORE as i64,
        "each held file revokes one certificate, got TotalRevokedEntries {total}"
    );

    // The limit a caller READS is the limit it hit.
    let mut form = action("DescribeAccountLimits");
    form.push(p("PageSize", "400"));
    let limits = call(&emulator.router, &form).await;
    let reported = members(&limits.body, "Limits")
        .iter()
        .find_map(|limit| {
            (first_text(limit, "Name").as_deref() == Some("revocations-per-trust-store"))
                .then(|| first_text(limit, "Max").unwrap_or_default())
        })
        .unwrap_or_else(|| panic!("no revocations-per-trust-store limit in {}", limits.body));
    assert_eq!(
        reported,
        REVOCATIONS_PER_TRUST_STORE.to_string(),
        "expected the reported quota to be the enforced one, got {reported:?}"
    );
}

// ---------------------------------------------------------------------------
// RULE 4: one call adds one file
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_call_naming_more_than_one_revocation_file_is_refused() {
    let emulator = emulator_with_bundles().await;
    let (certificate, key) = authority();
    let arn = trust_store(&emulator, "ts-one-per-call", &certificate).await;
    let list = crl_pem(&certificate, &key, 2);
    for key in ["pair/first.pem", "pair/second.pem", "pair/third.pem"] {
        emulator.stage(key, &list).await;
    }

    // Two entries, and three: a call naming any number past one is the same refusal.
    for (case, extra) in [
        (
            "two revocation contents",
            vec![
                p("RevocationContents.member.2.S3Bucket", BUNDLE_BUCKET),
                p("RevocationContents.member.2.S3Key", "pair/second.pem"),
            ],
        ),
        (
            "three revocation contents",
            vec![
                p("RevocationContents.member.2.S3Bucket", BUNDLE_BUCKET),
                p("RevocationContents.member.2.S3Key", "pair/second.pem"),
                p("RevocationContents.member.3.S3Bucket", BUNDLE_BUCKET),
                p("RevocationContents.member.3.S3Key", "pair/third.pem"),
            ],
        ),
    ] {
        let refused =
            add_revocation(&emulator, &arn, BUNDLE_BUCKET, "pair/first.pem", &extra).await;
        assert_eq!(
            (refused.status, refused.error_code().as_str()),
            (400, VALIDATION_ERROR),
            "case {case:?}: expected 400 {VALIDATION_ERROR}, got {} body {}",
            refused.status,
            refused.body
        );
        assert_eq!(
            message(&refused),
            "Up to 1 revocation contents can be specified",
            "case {case:?}: expected the prose bound to be stated, got {}",
            message(&refused)
        );
        let described = describe_revocations(&emulator, &arn, &[]).await;
        assert_eq!(
            reported_revocations(&described),
            Vec::new(),
            "case {case:?}: a refused call must store nothing, got {}",
            described.body
        );
    }

    // One entry is served, which is what makes the bound the subject rather than the form.
    let added = add_revocation(&emulator, &arn, BUNDLE_BUCKET, "pair/first.pem", &[]).await;
    assert_eq!(
        added.status, 200,
        "one revocation content is served, got {} body {}",
        added.status, added.body
    );
}

// ---------------------------------------------------------------------------
// RULE 5: an ARN or an id that resolves to nothing is the modelled refusal
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_revocation_call_refuses_a_trust_store_this_account_does_not_hold() {
    let emulator = emulator_with_bundles().await;
    let (certificate, key) = authority();
    // A real trust store beside it, so what the refusals turn on is the ARN rather than an
    // empty store.
    let held = trust_store(&emulator, "ts-held", &certificate).await;
    emulator
        .stage("absent/list.pem", &crl_pem(&certificate, &key, 1))
        .await;
    let added = add_revocation(&emulator, &held, BUNDLE_BUCKET, "absent/list.pem", &[]).await;
    assert_eq!(
        added.status, 200,
        "the control add must succeed, got {} body {}",
        added.status, added.body
    );

    for label in ["fabricated", "another-name"] {
        let absent = absent_trust_store_arn(label);
        let calls: Vec<(&str, Reply)> = vec![
            (
                "AddTrustStoreRevocations",
                add_revocation(&emulator, &absent, BUNDLE_BUCKET, "absent/list.pem", &[]).await,
            ),
            (
                "DescribeTrustStoreRevocations",
                describe_revocations(&emulator, &absent, &[]).await,
            ),
            (
                "RemoveTrustStoreRevocations",
                remove_revocations(&emulator, &absent, &[1]).await,
            ),
            (
                "GetTrustStoreRevocationContent",
                get_revocation_content(&emulator, &absent, 1).await,
            ),
        ];
        for (operation, refused) in calls {
            assert_eq!(
                (refused.status, refused.error_code().as_str()),
                (400, TRUST_STORE_NOT_FOUND),
                "case {label:?}/{operation}: expected 400 {TRUST_STORE_NOT_FOUND}, got {} body {}",
                refused.status,
                refused.body
            );
            assert_eq!(
                message(&refused),
                trust_store_arn_not_found_message(&absent),
                "case {label:?}/{operation}: expected the refusal to name the ARN, got {}",
                message(&refused)
            );
        }
    }
}

#[tokio::test]
async fn a_revocation_id_no_file_is_held_under_is_refused() {
    let emulator = emulator_with_bundles().await;
    let (certificate, key) = authority();
    let arn = trust_store(&emulator, "ts-unknown-id", &certificate).await;
    emulator
        .stage("ids/list.pem", &crl_pem(&certificate, &key, 1))
        .await;
    let added = add_revocation(&emulator, &arn, BUNDLE_BUCKET, "ids/list.pem", &[]).await;
    assert_eq!(
        added.status, 200,
        "the file every case reads around must be added, got {} body {}",
        added.status, added.body
    );

    // Ids no file is held under: before the first minted id, past the last, far past it, and
    // one a removal has released.
    emulator
        .stage("ids/second.pem", &crl_pem(&certificate, &key, 2))
        .await;
    let second = add_revocation(&emulator, &arn, BUNDLE_BUCKET, "ids/second.pem", &[]).await;
    assert_eq!(second.status, 200, "the second file must be added");
    let released = remove_revocations(&emulator, &arn, &[2]).await;
    assert_eq!(
        released.status, 200,
        "releasing the second file must succeed, got {} body {}",
        released.status, released.body
    );

    for revocation_id in [0_i64, 2, 7, i64::MAX] {
        let calls: Vec<(&str, Reply)> = vec![
            (
                "DescribeTrustStoreRevocations",
                describe_revocations(
                    &emulator,
                    &arn,
                    &[p("RevocationIds.member.1", &revocation_id.to_string())],
                )
                .await,
            ),
            (
                "RemoveTrustStoreRevocations",
                remove_revocations(&emulator, &arn, &[revocation_id]).await,
            ),
            (
                "GetTrustStoreRevocationContent",
                get_revocation_content(&emulator, &arn, revocation_id).await,
            ),
        ];
        for (operation, refused) in calls {
            assert_eq!(
                (refused.status, refused.error_code().as_str()),
                (400, REVOCATION_ID_NOT_FOUND),
                "case {revocation_id}/{operation}: expected 400 {REVOCATION_ID_NOT_FOUND}, got \
                 {} body {}",
                refused.status,
                refused.body
            );
            assert_eq!(
                message(&refused),
                revocation_id_not_found_message(revocation_id),
                "case {revocation_id}/{operation}: expected the refusal to name the id, got {}",
                message(&refused)
            );
        }
    }

    // A removal naming one unknown id beside a known one releases NOTHING.
    let refused = remove_revocations(&emulator, &arn, &[1, 9]).await;
    assert_eq!(
        (refused.status, refused.error_code().as_str()),
        (400, REVOCATION_ID_NOT_FOUND),
        "expected the whole removal refused, got {} body {}",
        refused.status,
        refused.body
    );
    let held = reported_revocations(&describe_revocations(&emulator, &arn, &[]).await);
    assert_eq!(
        held.iter().map(|entry| entry.0).collect::<Vec<i64>>(),
        vec![1],
        "an all-or-nothing removal leaves every named file in force, got {held:?}"
    );
}

// ---------------------------------------------------------------------------
// RULE 6: a removal releases what it names, and ids are never reused
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_removal_releases_the_files_it_names_and_nothing_else() {
    let emulator = emulator_with_bundles().await;
    let (certificate, key) = authority();
    let arn = trust_store(&emulator, "ts-removals", &certificate).await;

    // Three files, each revoking a different number of certificates, so what the totals
    // follow is which files are still held.
    for (index, revoked) in [1_usize, 2, 4].iter().enumerate() {
        let object = format!("removals/list-{index}.pem");
        emulator
            .stage(&object, &crl_pem(&certificate, &key, *revoked))
            .await;
        let added = add_revocation(&emulator, &arn, BUNDLE_BUCKET, &object, &[]).await;
        assert_eq!(
            added.status, 200,
            "case file {index}: the add must succeed, got {} body {}",
            added.status, added.body
        );
    }
    assert_eq!(
        total_revoked_entries(&emulator, &arn).await,
        7,
        "the three files revoke 7 certificates between them"
    );

    // One file released: the others stay, and the count follows.
    let removed = remove_revocations(&emulator, &arn, &[2]).await;
    assert_eq!(
        removed.status, 200,
        "expected 200 from RemoveTrustStoreRevocations, got {} body {}",
        removed.status, removed.body
    );
    let held = reported_revocations(&describe_revocations(&emulator, &arn, &[]).await);
    assert_eq!(
        held.iter()
            .map(|entry| (entry.0, entry.2))
            .collect::<Vec<(i64, i64)>>(),
        vec![(1, 1), (3, 4)],
        "expected the two files not named to stay in force, got {held:?}"
    );
    assert_eq!(
        total_revoked_entries(&emulator, &arn).await,
        5,
        "the released file stops being counted"
    );

    // An id a removal released is never handed out again.
    emulator
        .stage("removals/after.pem", &crl_pem(&certificate, &key, 3))
        .await;
    let added = add_revocation(&emulator, &arn, BUNDLE_BUCKET, "removals/after.pem", &[]).await;
    let minted = reported_revocations(&added);
    assert_eq!(
        minted.iter().map(|entry| entry.0).collect::<Vec<i64>>(),
        vec![4],
        "expected a fresh id rather than the released one, got {minted:?}"
    );

    // Two files released in one call, which empties the trust store's list.
    let removed = remove_revocations(&emulator, &arn, &[1, 3, 4]).await;
    assert_eq!(
        removed.status, 200,
        "expected 200 releasing every file, got {} body {}",
        removed.status, removed.body
    );
    let described = describe_revocations(&emulator, &arn, &[]).await;
    assert_eq!(
        reported_revocations(&described),
        Vec::new(),
        "expected no revocation file left, got {}",
        described.body
    );
    assert_eq!(
        total_revoked_entries(&emulator, &arn).await,
        0,
        "a trust store holding no revocation file revokes nothing"
    );
}

// ---------------------------------------------------------------------------
// RULE 7: a paged describe walks every file exactly once
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_paged_describe_walks_every_revocation_file_once() {
    let emulator = emulator_with_bundles().await;
    let (certificate, key) = authority();
    let arn = trust_store(&emulator, "ts-paged", &certificate).await;
    let held = 3_usize;
    for index in 0..held {
        let object = format!("paged/list-{index}.pem");
        emulator
            .stage(&object, &crl_pem(&certificate, &key, index + 1))
            .await;
        let added = add_revocation(&emulator, &arn, BUNDLE_BUCKET, &object, &[]).await;
        assert_eq!(added.status, 200, "case file {index}: the add must succeed");
    }

    for page_size in 1..=held + 1 {
        let walked = walk_revocations(&emulator, &arn, &[], page_size, held).await;
        assert_eq!(
            walked,
            (1..=held as i64).collect::<Vec<i64>>(),
            "case PageSize {page_size}: expected every file once in order, got {walked:?}"
        );
    }

    // A marker this store did not mint names no position at all.
    let refused = describe_revocations(&emulator, &arn, &[p("Marker", "not-a-marker")]).await;
    assert_eq!(
        (refused.status, refused.error_code().as_str()),
        (400, VALIDATION_ERROR),
        "expected a marker this store did not mint to be refused, got {} body {}",
        refused.status,
        refused.body
    );
}
