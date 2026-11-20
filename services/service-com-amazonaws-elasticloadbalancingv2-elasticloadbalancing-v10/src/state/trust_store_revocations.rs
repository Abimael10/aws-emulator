// SPDX-License-Identifier: CC0-1.0

//! The REVOCATION half of a trust store: the revocation files in force in one, the Amazon
//! S3 object each was read from, and what reading that object established.
//!
//! A revocation file is a certificate revocation list an HTTPS listener checks a CLIENT
//! certificate against, so the two facts the API reports about one — its `RevocationId` and
//! its `NumberOfRevokedEntries` — are a property of the OBJECT a caller staged, not of the
//! request: the id is the one this trust store mints for the file it accepted, and the
//! count is how many certificates the list inside those bytes revokes. So an add cannot
//! answer without READING that object, which is the shared read every service pointed at an
//! object makes ([`::service_common::s3_object_read`]), exactly as the CA certificates
//! bundle beside it is read ([`super::ca_certificates_bundle`]).
//!
//! The files live on the trust store's own record ([`RevocationList`] on
//! `crate::state::TrustStoreRecord`), so the add, the describe, the content read and the
//! removal all read and write the ONE copy, and releasing a trust store releases its
//! revocation files with it.
//!
//! ## What a read establishes, and what this API answers for it
//!
//! This model declares two refusals about a revocation file, and they split exactly as the
//! bundle's two do — on WHERE the failure is: `RevocationContentNotFoundException` ("The
//! specified revocation file does not exist") is about the ADDRESS, and
//! `InvalidRevocationContentException` ("The provided revocation file is an invalid format,
//! or uses an incorrect algorithm") is about the BYTES. A read nothing answered at all is
//! neither: no object store is mounted to ask, which is this emulator's own inability to
//! serve the operation rather than the caller's fault.
//!
//! The PEM layer and the list layer are each read by the crate that owns it:
//! `rustls-pemfile` unwraps the `X509 CRL` blocks of a PEM file, and `x509-cert` decodes the
//! DER inside one as the RFC 5280 `CertificateList` — so what is counted is entries a parser
//! produced, never occurrences of a marker in the text.

use ::std::time::Duration;

use ::x509_cert::der::Decode;

/// How many revocation files one trust store holds before an add refuses with the modelled
/// `TooManyTrustStoreRevocationEntriesException`.
///
/// The per-account, per-Region default Elastic Load Balancing publishes for
/// `revocations-per-trust-store`, which this service reports through
/// `DescribeAccountLimits` — reported from this constant, so the limit a caller reads is the
/// limit it hits.
pub const REVOCATIONS_PER_TRUST_STORE: usize = 5;

/// How many revocation files ONE call may name.
///
/// Recorded behaviour: a single `AddTrustStoreRevocations` carrying more than one
/// `RevocationContents` entry is refused as a bad REQUEST, so the bound belongs to the
/// operation's input rather than to the trust store's quota.
pub const REVOCATION_CONTENTS_PER_CALL: usize = 1;

/// The most revocation-file bytes this emulator reads into the process.
///
/// CHOSEN by this emulator rather than read off a published quota: it bounds what one
/// control-plane call buffers, and it is far above the revocation lists an emulated workflow
/// stages. The read is RANGED to it, so a larger object is refused rather than buffered
/// whole.
pub const MAX_REVOCATION_CONTENT_BYTES: usize = 4_194_304;

/// The type of revocation file this API takes: the only member of the modelled
/// `RevocationType` enum (`crate::ops::types::REVOCATION_TYPE_VALUES`), and what a
/// `RevocationContent` naming no type is.
pub const REVOCATION_TYPE_CRL: &str = "CRL";

/// How long the pre-signed address a revocation-file read answers with stays valid: "This
/// action returns a pre-signed S3 URI which is active for ten minutes"
/// (`GetTrustStoreRevocationContent`'s own model documentation).
pub const REVOCATION_CONTENT_URI_VALIDITY: Duration = Duration::from_secs(10 * 60);

/// The signing name of the store a revocation file lives in, which is the store a fetch of
/// a reported address has to reach: the object is the one a caller staged in this emulator's
/// own Amazon S3 and the add read over the in-process callback, so the signed address names
/// that very service.
const OBJECT_STORE_SIGNING_NAME: &str = "s3";

/// The message a call whose revocation file nothing can be fetched at is refused with, under
/// the modelled `RevocationContentNotFound`.
///
/// Elastic Load Balancing words this refusal as a FIXED sentence about the file — neither
/// the bucket nor the key the request named is in it — exactly as it words the same refusal
/// about a CA certificates bundle
/// ([`super::CA_CERTIFICATES_BUNDLE_NOT_FOUND_MESSAGE`]).
pub const REVOCATION_CONTENT_NOT_FOUND_MESSAGE: &str = "Provided revocation file was not found";

/// The message a call whose revocation file could be fetched and is no certificate
/// revocation list is refused with, under the modelled `InvalidRevocationContent`.
///
/// Recorded behaviour: the whole sentence is fixed text about what reading the file FOUND —
/// no revocation list could be read out of it, which is what every outcome refused under
/// this code has in common (bytes that are no PEM and no DER, PEM carrying no `X509 CRL`
/// block, a block holding no `CertificateList`, an object with no bytes at all). Nothing of
/// the request is interpolated into it.
pub const INVALID_REVOCATION_CONTENT_MESSAGE: &str =
    "Provided revocation file is not in the correct format";

/// The message an add past [`REVOCATIONS_PER_TRUST_STORE`] is refused with, under the
/// modelled `TooManyTrustStoreRevocationEntries`.
///
/// Recorded behaviour: the fixed text is "Trust store can have up to " and " revocation
/// contents", with the account's revocation-file quota as the one variable part — so the
/// number comes from the constant this emulator enforces rather than from a sentence written
/// around one account's figure.
pub fn too_many_trust_store_revocation_entries_message() -> String {
    format!("Trust store can have up to {REVOCATIONS_PER_TRUST_STORE} revocation contents")
}

/// The message a call naming a revocation id the trust store holds no file under is refused
/// with, under the modelled `RevocationIdNotFound` ("The specified revocation ID does not
/// exist").
///
/// Built from the request, and worded about the KIND of identifier it carried, the way every
/// not-found refusal of this service's trust-store surface is
/// ([`super::trust_store_arn_not_found_message`]): the id the caller named is the one value
/// it can act on.
pub fn revocation_id_not_found_message(revocation_id: i64) -> String {
    format!("Revocation ID '{revocation_id}' not found")
}

/// The message a call naming more than [`REVOCATION_CONTENTS_PER_CALL`] revocation files is
/// refused with, under this service's own `ValidationError`.
///
/// Recorded behaviour: the model gives `RevocationContents` no length at all, so the bound is
/// one Elastic Load Balancing states in prose and refuses as a bad request rather than as a
/// quota of the trust store.
pub fn one_revocation_content_per_call_message() -> String {
    format!("Up to {REVOCATION_CONTENTS_PER_CALL} revocation contents can be specified")
}

/// Where one call said its revocation file is: the three members the modelled
/// `RevocationContent` carries (`S3Bucket`, `S3Key` and the optional `S3ObjectVersion`).
///
/// Persisted on the trust store's record, so the object a revocation file was read from is
/// what the store holds — which is what lets the content read hand a caller an address of
/// that very object rather than one re-derived per call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevocationContentLocation {
    pub bucket: String,
    pub key: String,
    pub version: Option<String>,
}

/// What a read of a revocation file established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevocationFile {
    /// The store served the file, and the revocation lists in it revoke this many
    /// certificates (possibly none: a list may be published empty).
    Counted(i64),
    /// Nothing can be fetched at the address the request named.
    Absent,
    /// Something is at that address and no revocation list can be read from it.
    Invalid,
    /// Nothing answered the read, so nothing is known about the file.
    Unanswered,
}

/// Which of this model's refusals a call owes when the revocation file it named could not be
/// read — the ONE reading of that split, so no two operations that take a revocation file
/// word it differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevocationFileRefusal {
    /// `RevocationContentNotFoundException`: nothing can be fetched at that address.
    NotFound,
    /// `InvalidRevocationContentException`: something is there and no revocation list can be
    /// read from it.
    Invalid,
    /// Nothing answered the read, so this emulator cannot serve the operation at all — which
    /// is not the caller's request to fix.
    Unanswered,
}

/// What the revocation file at `location` can establish about itself, read through the
/// emulator's own Amazon S3 in `region` — the Region the call was signed for, since the
/// operation's input carries none.
pub async fn resolve_revocation_file(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    location: &RevocationContentLocation,
) -> RevocationFile {
    let read = ::service_common::s3_object_read::read_object(
        ctx.callback(),
        region,
        &location.bucket,
        &location.key,
        location.version.as_deref(),
        MAX_REVOCATION_CONTENT_BYTES,
    )
    .await;
    match read {
        ::service_common::s3_object_read::ObjectRead::Served(body) => match revoked_entries_in(
            &body,
        ) {
            Some(counted) => RevocationFile::Counted(counted),
            None => RevocationFile::Invalid,
        },
        // The address is what the caller fixes for both of these: the store holds nothing
        // there, or it will not serve what it holds there, and this API models one refusal
        // for "the revocation file does not exist" and none for a permission of the object
        // store.
        ::service_common::s3_object_read::ObjectRead::Absent
        | ::service_common::s3_object_read::ObjectRead::Denied => RevocationFile::Absent,
        ::service_common::s3_object_read::ObjectRead::Unreadable => RevocationFile::Invalid,
        ::service_common::s3_object_read::ObjectRead::Unanswered => RevocationFile::Unanswered,
    }
}

/// How many certificates the revocation file at `location` revokes, or which refusal the
/// read of it earned ([`RevocationFileRefusal`]).
///
/// The one place a read outcome is turned into this model's answer: the operation that adds a
/// revocation file calls it and maps the three refusals onto its own modelled error enum.
pub async fn counted_revoked_entries(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    location: &RevocationContentLocation,
) -> Result<i64, RevocationFileRefusal> {
    match resolve_revocation_file(ctx, region, location).await {
        RevocationFile::Counted(counted) => Ok(counted),
        RevocationFile::Absent => Err(RevocationFileRefusal::NotFound),
        RevocationFile::Invalid => Err(RevocationFileRefusal::Invalid),
        RevocationFile::Unanswered => Err(RevocationFileRefusal::Unanswered),
    }
}

/// How many certificates the revocation lists in `body` revoke, or `None` for bytes no
/// revocation list can be read out of.
///
/// Both framings a caller stages a list in are read: a PEM file, whose `X509 CRL` blocks
/// `rustls-pemfile` unwraps, and the bare DER of one list. Inside a block it is `x509-cert`
/// that decodes the RFC 5280 `CertificateList`, so a block that only LOOKS like a revocation
/// list (a truncated structure, another kind of DER, a certificate) is not read as one. A
/// list that revokes nothing is still a list, which is the `Some(0)` this answers with —
/// distinct from the `None` of bytes that hold no list at all.
pub fn revoked_entries_in(body: &[u8]) -> Option<i64> {
    let mut reader = ::std::io::BufReader::new(body);
    let blocks: Vec<Vec<u8>> = ::rustls_pemfile::crls(&mut reader)
        .filter_map(|block| block.ok().map(|der| der.as_ref().to_vec()))
        .collect();
    if blocks.is_empty() {
        // No PEM revocation-list block at all: the bytes may still be the DER of one list,
        // which is the other framing an object holds a list in.
        return revoked_entries_of(body);
    }
    let mut total: i64 = 0;
    let mut read_any = false;
    for der in blocks {
        let Some(counted) = revoked_entries_of(&der) else {
            continue;
        };
        read_any = true;
        total = total.saturating_add(counted);
    }
    read_any.then_some(total)
}

/// How many certificates the ONE revocation list DER-encoded in `der` revokes, or `None`
/// when those bytes hold no `CertificateList`.
fn revoked_entries_of(der: &[u8]) -> Option<i64> {
    let list = ::x509_cert::crl::CertificateList::from_der(der).ok()?;
    let revoked = list
        .tbs_cert_list
        .revoked_certificates
        .map(|entries| entries.len())
        .unwrap_or_default();
    Some(i64::try_from(revoked).unwrap_or(i64::MAX))
}

/// One revocation file in force in a trust store: what `DescribeTrustStoreRevocations`
/// reports about it, plus the object it was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustStoreRevocationRecord {
    /// The id this trust store minted for the file, which every later call addresses it by.
    pub revocation_id: i64,
    /// "The type of revocation file" — the type the add resolved, which the modelled enum
    /// gives one value ([`REVOCATION_TYPE_CRL`]).
    pub revocation_type: String,
    /// "The number of revoked certificates" — read out of the file's own bytes when it was
    /// added ([`revoked_entries_in`]), never a figure derived from the request.
    pub number_of_revoked_entries: i64,
    /// WHERE those bytes came from: the Amazon S3 object the add named, so the content read
    /// addresses the object the trust store really holds its list in.
    pub content: RevocationContentLocation,
}

/// The revocation files one trust store holds, in the order their ids were minted, and the
/// counter the next id comes from.
///
/// A list, not a set keyed by the object's address: AWS mints a fresh id for every file
/// accepted, so the same object staged twice is two files, and an id is never handed out
/// again once a removal has released it.
#[derive(Debug, Clone, Default)]
pub struct RevocationList {
    entries: Vec<TrustStoreRevocationRecord>,
    next_revocation_id: i64,
}

impl RevocationList {
    /// The files this trust store holds, oldest id first — the order a paginated read walks
    /// them in.
    pub fn entries(&self) -> &[TrustStoreRevocationRecord] {
        &self.entries
    }

    /// How many files it holds, which is what [`REVOCATIONS_PER_TRUST_STORE`] bounds.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether it holds no file at all.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The file `revocation_id` names, or `None` when this trust store holds none under it.
    pub fn get(&self, revocation_id: i64) -> Option<&TrustStoreRevocationRecord> {
        self.entries
            .iter()
            .find(|entry| entry.revocation_id == revocation_id)
    }

    /// Record the revocation file read out of `content`, as type `revocation_type` revoking
    /// `number_of_revoked_entries` certificates, and answer the record it was stored as.
    ///
    /// The id is MINTED here, from this trust store's own counter, so it is unique within the
    /// trust store and is never one a removal already released.
    pub fn add(
        &mut self,
        revocation_type: &str,
        number_of_revoked_entries: i64,
        content: RevocationContentLocation,
    ) -> TrustStoreRevocationRecord {
        self.next_revocation_id = self.next_revocation_id.saturating_add(1);
        let record = TrustStoreRevocationRecord {
            revocation_id: self.next_revocation_id,
            revocation_type: revocation_type.to_string(),
            number_of_revoked_entries,
            content,
        };
        self.entries.push(record.clone());
        record
    }

    /// The first id of `revocation_ids` this trust store holds no file under, or `None` when
    /// it holds one under every single one.
    ///
    /// What makes a removal ALL OR NOTHING: the caller is answered the modelled refusal and
    /// nothing is released, so a list naming one unknown id does not silently remove the
    /// known ones beside it.
    pub fn first_absent(&self, revocation_ids: &[i64]) -> Option<i64> {
        revocation_ids
            .iter()
            .copied()
            .find(|revocation_id| self.get(*revocation_id).is_none())
    }

    /// Release the files `revocation_ids` names.
    pub fn remove(&mut self, revocation_ids: &[i64]) {
        self.entries
            .retain(|entry| !revocation_ids.contains(&entry.revocation_id));
    }

    /// How many certificates everything this trust store holds revokes: the sum over its
    /// files, which is "The number of revoked certificates in the trust store"
    /// (`TrustStore$TotalRevokedEntries`).
    pub fn total_revoked_entries(&self) -> i64 {
        self.entries
            .iter()
            .fold(0_i64, |total, entry| {
                total.saturating_add(entry.number_of_revoked_entries)
            })
    }
}

/// The address the revocation file at `location` is fetched from: a `GET` of that object,
/// pre-signed for [`REVOCATION_CONTENT_URI_VALIDITY`] against the endpoint this emulator is
/// reached on.
///
/// The object is the one the add read in this emulator's own Amazon S3, so a client that
/// follows the address is routed back into this emulator and served the very bytes the
/// caller staged. A versioned object keeps its version in the address, so the file reported
/// is the one the add was pointed at rather than whatever is current.
pub fn revocation_content_uri(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    location: &RevocationContentLocation,
) -> String {
    let path = format!("/{}/{}", location.bucket, location.key);
    let mut request = ::smithy_server_runtime::SignedUrlRequest::new(
        ::smithy_server_runtime::http::Method::GET,
        OBJECT_STORE_SIGNING_NAME,
        &path,
    );
    if let Some(version) = location
        .version
        .as_deref()
        .map(str::trim)
        .filter(|version| !version.is_empty())
    {
        request = request.query("versionId", version);
    }
    match ctx
        .url_signer()
        .sign(request, REVOCATION_CONTENT_URI_VALIDITY)
    {
        Ok(signed) => signed.as_str().to_string(),
        // Nothing about a stored bucket name, a stored key or this emulator's own endpoint
        // makes a `GET` unsignable, so this is unreachable in practice. The object IS at that
        // address either way, so the address is what is reported rather than no location at
        // all. The endpoint is `scheme://host[:port]` with no path of its own, and a trailing
        // separator is dropped so the object's path is the only one the address carries.
        Err(error) => {
            ::tracing::warn!(
                %error,
                path = %path,
                "a revocation file's address could not be signed"
            );
            format!("{}{path}", ctx.endpoint().as_str().trim_end_matches('/'))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A certificate authority a revocation list can be signed by: its self-signed
    /// certificate and the key pair behind it, GENERATED here, so every list below is a real
    /// one a parser produced rather than bytes this test and the reader both guessed.
    fn authority() -> (::rcgen::Certificate, ::rcgen::KeyPair) {
        let key = ::rcgen::KeyPair::generate().expect("a key pair");
        let mut params =
            ::rcgen::CertificateParams::new(vec!["ca.example.com".to_string()]).expect("parameters");
        params.is_ca = ::rcgen::IsCa::Ca(::rcgen::BasicConstraints::Unconstrained);
        let certificate = params.self_signed(&key).expect("a self-signed certificate");
        (certificate, key)
    }

    /// A revocation list revoking `count` certificates, signed by a real authority.
    fn crl(count: usize) -> ::rcgen::CertificateRevocationList {
        let (certificate, key) = authority();
        let params = ::rcgen::CertificateRevocationListParams {
            this_update: ::rcgen::date_time_ymd(2026, 1, 1),
            next_update: ::rcgen::date_time_ymd(2026, 2, 1),
            crl_number: ::rcgen::SerialNumber::from(17_u64),
            issuing_distribution_point: None,
            revoked_certs: (0..count)
                .map(|index| ::rcgen::RevokedCertParams {
                    serial_number: ::rcgen::SerialNumber::from(index as u64 + 1),
                    revocation_time: ::rcgen::date_time_ymd(2026, 1, 2),
                    reason_code: Some(::rcgen::RevocationReason::KeyCompromise),
                    invalidity_date: None,
                })
                .collect(),
            key_identifier_method: ::rcgen::KeyIdMethod::Sha256,
        };
        params
            .signed_by(&certificate, &key)
            .expect("a signed revocation list")
    }

    /// RULE: what is counted is revocation-list entries a parser produced, in either framing
    /// a caller stages a list in — so bytes that are no list at all are refused, and a list
    /// is read for exactly the entries it revokes.
    #[test]
    fn only_decodable_revocation_lists_are_read() {
        let one = crl(1);
        let three = crl(3);
        let empty = crl(0);
        let rows: Vec<(&str, Vec<u8>, Option<i64>)> = vec![
            ("bytes that are no PEM and no DER", b"not a crl".to_vec(), None),
            (
                "a PEM block that is not a revocation list",
                b"-----BEGIN CERTIFICATE-----\nMTIzNDU2Nzg5MA==\n-----END CERTIFICATE-----\n"
                    .to_vec(),
                None,
            ),
            (
                "a revocation-list block holding no CertificateList",
                b"-----BEGIN X509 CRL-----\nMTIzNDU2Nzg5MA==\n-----END X509 CRL-----\n".to_vec(),
                None,
            ),
            ("no bytes at all", Vec::new(), None),
            (
                "a PEM list revoking one certificate",
                one.pem().expect("PEM").into_bytes(),
                Some(1),
            ),
            (
                "a PEM list revoking three certificates",
                three.pem().expect("PEM").into_bytes(),
                Some(3),
            ),
            (
                "a PEM list that revokes nothing",
                empty.pem().expect("PEM").into_bytes(),
                Some(0),
            ),
            (
                "a PEM list beside prose",
                format!("Issued by the example CA\n{}", one.pem().expect("PEM")).into_bytes(),
                Some(1),
            ),
            (
                "two PEM lists in one file",
                format!(
                    "{}{}",
                    one.pem().expect("PEM"),
                    three.pem().expect("PEM")
                )
                .into_bytes(),
                Some(4),
            ),
            (
                "the bare DER of a list revoking three certificates",
                three.der().as_ref().to_vec(),
                Some(3),
            ),
        ];
        for (case, body, expected) in rows {
            let actual = revoked_entries_in(&body);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?} revoked entries, got {actual:?}"
            );
        }
    }

    /// RULE: a trust store's list mints a fresh id for every file it accepts, never hands one
    /// out again, reports its files oldest id first, and reports the sum of what they revoke.
    #[test]
    fn a_revocation_list_mints_ids_and_totals_what_it_holds() {
        let mut list = RevocationList::default();
        assert!(list.is_empty(), "a fresh list holds no revocation file");
        assert_eq!(
            list.total_revoked_entries(),
            0,
            "a fresh list revokes nothing, got {}",
            list.total_revoked_entries()
        );

        let minted: Vec<i64> = [2_i64, 0, 5]
            .iter()
            .enumerate()
            .map(|(index, revoked)| {
                list.add(
                    REVOCATION_TYPE_CRL,
                    *revoked,
                    RevocationContentLocation {
                        bucket: "revocations".to_string(),
                        key: format!("list-{index}.pem"),
                        version: None,
                    },
                )
                .revocation_id
            })
            .collect();
        assert_eq!(
            minted,
            vec![1, 2, 3],
            "expected ids minted in sequence from 1, got {minted:?}"
        );
        assert_eq!(
            list.total_revoked_entries(),
            7,
            "expected the sum of what the files revoke, got {}",
            list.total_revoked_entries()
        );
        assert_eq!(
            list.first_absent(&[1, 4]),
            Some(4),
            "expected the first id no file is held under"
        );
        assert_eq!(
            list.first_absent(&[1, 2, 3]),
            None,
            "every id named is held, so none is absent"
        );

        list.remove(&[2]);
        let held: Vec<i64> = list
            .entries()
            .iter()
            .map(|entry| entry.revocation_id)
            .collect();
        assert_eq!(
            held,
            vec![1, 3],
            "expected the removal to release only the file it named, got {held:?}"
        );
        assert_eq!(
            list.total_revoked_entries(),
            7,
            "the released file revoked nothing, so the total is unchanged, got {}",
            list.total_revoked_entries()
        );
        let next = list
            .add(
                REVOCATION_TYPE_CRL,
                1,
                RevocationContentLocation {
                    bucket: "revocations".to_string(),
                    key: "after-removal.pem".to_string(),
                    version: Some("v2".to_string()),
                },
            )
            .revocation_id;
        assert_eq!(
            next, 4,
            "an id a removal released is never handed out again, got {next}"
        );
    }
}
