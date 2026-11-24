// SPDX-License-Identifier: CC0-1.0

//! The SIGNATURE a signed .zip code package carries: how a signing service stamps one into
//! an artifact it produced, and how a service that DEPLOYS that artifact reads it back.
//!
//! A code signature travels with the artifact rather than with the call that deploys it: the
//! signing service writes it into the package it signs, and every later deployment of those
//! bytes is verified against what the package itself carries. Two emulated services sit on
//! the two ends of that handoff — the signing service that produces a signed artifact, and
//! the compute service that refuses to deploy one it cannot attribute to a trusted publisher
//! — so the FORM of the signature is stated once here and neither end states it again.
//!
//! The signature is one added entry of the archive ([`SIGNATURE_ENTRY_NAME`]) holding a JSON
//! envelope ([`render_envelope`]) with:
//!
//! * the signing profile version the signature was made under, and the signing job that made
//!   it — WHO signed, which a verifier checks against the publishers it trusts;
//! * the digest of the archive's own contents ([`content_digest`]) — WHAT was signed, which a
//!   verifier recomputes from the package in front of it, so a signature copied onto other
//!   contents no longer covers them;
//! * the signature, and the public key it verifies under.
//!
//! What the signature COVERS is all three of the first kind at once
//! ([`signed_material`]): the profile version, the job and the content digest, so neither the
//! contents nor the attribution can be changed after signing without the signature ceasing to
//! verify. The key it is checked against is the public half the signing service holds for that
//! profile version, never the copy the envelope carries.
//!
//! The entry name and the member names are this emulator's own: no AWS API names either, and
//! the envelope is read only by the emulated services at the two ends of the handoff.

use std::io::{Cursor, Read as _};

use sha2::Digest as _;

/// The entry a signature envelope travels in, inside a signed archive.
///
/// Prefixed with the signing service's own name so it cannot collide with an entry the
/// package being signed already carries at the archive root.
pub const SIGNATURE_ENTRY_NAME: &str = "aws-signer-signature.json";

/// The envelope member naming the signing profile version the signature was made under.
const MEMBER_PROFILE_VERSION_ARN: &str = "profileVersionArn";

/// The envelope member naming the signing job that made the signature.
const MEMBER_JOB_ID: &str = "jobId";

/// The envelope member carrying the digest of the signed contents, lowercase hexadecimal.
const MEMBER_CONTENT_SHA384: &str = "contentSha384";

/// The envelope member carrying the signature itself, lowercase hexadecimal.
const MEMBER_SIGNATURE: &str = "signature";

/// The envelope member carrying the public key the signature verifies under, lowercase
/// hexadecimal.
const MEMBER_PUBLIC_KEY: &str = "publicKey";

/// The most entries a package this module digests may carry.
///
/// A chosen bound, like the two below it: an archive handed to an emulated service is caller
/// content, and its entries are read into memory one at a time while the digest is taken, so
/// the work is bounded before anything is allocated.
const MAX_ARCHIVE_ENTRIES: usize = 4_096;

/// The most bytes one entry of such a package may expand to.
const MAX_ENTRY_BYTES: usize = 16 * 1024 * 1024;

/// The most bytes a whole such package may expand to.
const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;

/// The signature a signed package carries, as a verifier reads it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageSignature {
    /// The ARN of the signing profile version the signature was made under: WHO signed this
    /// package, which is what a trusted-publisher list is checked against.
    pub profile_version_arn: String,
    /// The id of the signing job that made the signature.
    pub job_id: String,
    /// What the signature COVERS ([`signed_material`]): the two members above together with
    /// the digest of the contents this package carries, already recomputed from the package
    /// it was read out of. A verifier checks the signature against exactly these bytes.
    pub signed_material: Vec<u8>,
    /// The signature over that material, as the DER `Ecdsa-Sig-Value` of RFC 3279 section
    /// 2.2.3.
    pub signature: Vec<u8>,
    /// The public key the envelope states the signature verifies under, as a DER X.509
    /// `SubjectPublicKeyInfo`.
    ///
    /// What the ENVELOPE claims, which is not on its own a statement about the publisher: a
    /// verifier checks the signature against the key the signing service holds for
    /// [`Self::profile_version_arn`], so this copy only tells a reader which key the
    /// envelope was written for.
    pub public_key: Vec<u8>,
}

/// The bytes a code signature is made over: the signing profile version, the signing job and
/// the digest of the signed contents, each length-prefixed so no two different triples
/// encode alike.
///
/// Everything the envelope ATTESTS to is in here, which is what lets a verifier reject an
/// envelope whose attribution was rewritten after it was signed as readily as one whose
/// contents were.
#[must_use]
pub fn signed_material(
    profile_version_arn: &str,
    job_id: &str,
    content_digest: &[u8],
) -> Vec<u8> {
    let mut material = Vec::new();
    for part in [
        profile_version_arn.as_bytes(),
        job_id.as_bytes(),
        content_digest,
    ] {
        material.extend_from_slice(&(part.len() as u64).to_be_bytes());
        material.extend_from_slice(part);
    }
    material
}

/// The JSON envelope a signing service stamps into an artifact it signed.
///
/// Every member is computed from the job's own inputs: `content_digest` is the digest of the
/// contents being signed ([`content_digest`]), `signature` is the signature over the material
/// those contents and this attribution make up ([`signed_material`]), and `public_key` is the
/// public half of the pair that made it.
#[must_use]
pub fn render_envelope(
    content_digest: &[u8],
    signature: &[u8],
    public_key: &[u8],
    profile_version_arn: &str,
    job_id: &str,
) -> Vec<u8> {
    let envelope = ::serde_json::json!({
        MEMBER_PROFILE_VERSION_ARN: profile_version_arn,
        MEMBER_JOB_ID: job_id,
        MEMBER_CONTENT_SHA384: ::hex::encode(content_digest),
        MEMBER_SIGNATURE: ::hex::encode(signature),
        MEMBER_PUBLIC_KEY: ::hex::encode(public_key),
    });
    // A JSON document always renders, and the fallback is never a document a verifier could
    // mistake for one: the only member that could fail to render is a non-finite number, and
    // this object carries none.
    ::serde_json::to_vec(&envelope).unwrap_or_default()
}

/// The digest a code signature is made over: SHA-384 over the archive's own CONTENTS — every
/// entry except the signature envelope itself, in archive order, each as the length and
/// bytes of its name followed by the length and bytes of its content.
///
/// The lengths are part of the digest so no two different archives — one entry named `ab`
/// holding `c`, one named `a` holding `bc` — hash alike. The signature entry is left out
/// because it is what carries the digest, so what a verifier recomputes from a signed package
/// is exactly what the signing service digested before it added that entry.
///
/// `None` for an object no zip reader can open, for one past the bounds this module holds
/// caller content to ([`MAX_ARCHIVE_ENTRIES`], [`MAX_ENTRY_BYTES`], [`MAX_ARCHIVE_BYTES`]),
/// and for one carrying the signature entry more than once — the one entry a verifier reads
/// the envelope back from is a single entry, so a package where that name covers several
/// would leave content outside the digest. None of the three shows what such a package's
/// contents hash to.
#[must_use]
pub fn content_digest(package: &[u8]) -> Option<Vec<u8>> {
    let mut archive = ::zip::ZipArchive::new(Cursor::new(package)).ok()?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return None;
    }
    let mut digest = sha2::Sha384::new();
    let mut read = 0usize;
    let mut signature_entries = 0usize;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).ok()?;
        // An entry whose name is not valid UTF-8 is not an entry this digest can state, so
        // the package it belongs to is one nothing is shown about.
        let name = entry.name().ok()?.to_string();
        if name == SIGNATURE_ENTRY_NAME {
            signature_entries += 1;
            // A second entry under that name is content the envelope read back from the
            // first would not cover. No writer in this workspace can produce such an
            // archive — the zip writer refuses a duplicate name — so this is a guard
            // against a hand-assembled one rather than a case with a path through it.
            if signature_entries > 1 {
                return None;
            }
            continue;
        }
        // The remaining budget plus one byte: reading MORE than the budget is what says the
        // entry is over it, and the limited reader stops there whatever the entry's header
        // claimed.
        let allowed = MAX_ARCHIVE_BYTES.saturating_sub(read).min(MAX_ENTRY_BYTES);
        let mut content = Vec::new();
        ::std::io::Read::take(&mut entry, allowed as u64 + 1)
            .read_to_end(&mut content)
            .ok()?;
        if content.len() > allowed {
            return None;
        }
        read += content.len();
        digest.update((name.len() as u64).to_be_bytes());
        digest.update(name.as_bytes());
        digest.update((content.len() as u64).to_be_bytes());
        digest.update(&content);
    }
    Some(digest.finalize().to_vec())
}

/// The signature `package` carries, when it carries one that covers these very contents.
///
/// `None` for a package that bears no signature at all (an unsigned archive, an object that
/// is no archive) and for one whose envelope states a digest that is not what this package's
/// contents hash to — a signature lifted off another artifact, or contents changed after
/// they were signed. Either way the package is one no publisher's signature covers, and what
/// a deployment control does about that is the deploying service's own decision.
///
/// The signature itself is NOT verified here: that takes the public key the signing service
/// holds for the profile version the envelope names, which only that service can answer for.
/// What is handed back for it is the material the signature has to verify over
/// ([`signed_material`]), built from the attribution the envelope states and the digest this
/// package's own contents produced.
#[must_use]
pub fn package_signature(package: &[u8]) -> Option<PackageSignature> {
    let envelope = signature_entry(package)?;
    let stated = parse_envelope(&envelope)?;
    let carried = content_digest(package)?;
    if stated.content_digest != carried {
        return None;
    }
    Some(PackageSignature {
        signed_material: signed_material(
            &stated.profile_version_arn,
            &stated.job_id,
            &carried,
        ),
        profile_version_arn: stated.profile_version_arn,
        job_id: stated.job_id,
        signature: stated.signature,
        public_key: stated.public_key,
    })
}

/// The bytes of the signature entry of `package`, when it carries that entry.
///
/// Read by NAME, so an unsigned package costs one look at the archive's directory and
/// nothing is decompressed for it.
fn signature_entry(package: &[u8]) -> Option<Vec<u8>> {
    let mut archive = ::zip::ZipArchive::new(Cursor::new(package)).ok()?;
    let mut entry = archive.by_name(SIGNATURE_ENTRY_NAME).ok()?;
    let mut bytes = Vec::new();
    // An envelope is a small JSON document; one past that bound is not one this reader
    // states anything about.
    ::std::io::Read::take(&mut entry, MAX_ENTRY_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= MAX_ENTRY_BYTES).then_some(bytes)
}

/// The envelope `bytes` states, or `None` when it is not one: a document that is not JSON,
/// one short of a member, or one whose hexadecimal members do not decode.
fn parse_envelope(bytes: &[u8]) -> Option<StatedEnvelope> {
    let document: ::serde_json::Value = ::serde_json::from_slice(bytes).ok()?;
    let member = |name: &str| -> Option<String> {
        document
            .get(name)
            .and_then(::serde_json::Value::as_str)
            .map(str::to_string)
    };
    let decoded = |name: &str| -> Option<Vec<u8>> { ::hex::decode(member(name)?).ok() };
    Some(StatedEnvelope {
        profile_version_arn: member(MEMBER_PROFILE_VERSION_ARN)?,
        job_id: member(MEMBER_JOB_ID)?,
        content_digest: decoded(MEMBER_CONTENT_SHA384)?,
        signature: decoded(MEMBER_SIGNATURE)?,
        public_key: decoded(MEMBER_PUBLIC_KEY)?,
    })
}

/// What an envelope SAYS, before any of it has been checked against the package it was read
/// out of.
struct StatedEnvelope {
    profile_version_arn: String,
    job_id: String,
    content_digest: Vec<u8>,
    signature: Vec<u8>,
    public_key: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::{
        content_digest, package_signature, render_envelope, signed_material, PackageSignature,
        SIGNATURE_ENTRY_NAME,
    };

    /// The signing profile version and the job the rows below sign under.
    const PROFILE_VERSION_ARN: &str =
        "arn:aws:signer:us-west-2:000000000000:/signing-profiles/P/0000000001";
    const JOB_ID: &str = "job-1";

    /// One entry of an archive a case assembles: its name and its content.
    type Entry<'a> = (&'a str, &'a [u8]);

    /// One row of the digest table: the case, the entries the archive carries, and whether
    /// those entries must hash to the same digest as the base row's.
    type DigestRow<'a> = (&'a str, Vec<Entry<'a>>, bool);

    /// One row of a package table: the case, the package's bytes, and what the row expects of
    /// it.
    type PackageRow<'a> = (&'a str, Vec<u8>, bool);

    /// A zip archive carrying `entries`, written in memory.
    fn archive(entries: &[Entry<'_>]) -> Vec<u8> {
        let mut writer = ::zip::ZipWriter::new(::std::io::Cursor::new(Vec::new()));
        for (name, content) in entries {
            writer
                .start_file(*name, ::zip::write::SimpleFileOptions::default())
                .expect("the entry starts");
            writer.write_all(content).expect("the entry is written");
        }
        writer
            .finish()
            .expect("the archive is finished")
            .into_inner()
    }

    /// `entries` plus a signature envelope stating `digest`, which is what a signing service
    /// produces: the contents, then the envelope over their digest.
    fn signed(entries: &[Entry<'_>], digest: &[u8]) -> Vec<u8> {
        let envelope = render_envelope(
            digest,
            b"\x30\x06\x02\x01\x01\x02\x01\x01",
            b"\x30\x01\x00",
            PROFILE_VERSION_ARN,
            JOB_ID,
        );
        let mut all: Vec<Entry<'_>> = entries.to_vec();
        all.push((SIGNATURE_ENTRY_NAME, &envelope));
        archive(&all)
    }

    /// The digest covers the contents and nothing else: the same entries hash alike however
    /// the archive was assembled, a signature entry does not take part, and any difference
    /// in a name, a content or the order changes it.
    #[test]
    fn the_digest_covers_the_archives_own_contents() {
        // (case, the entries, whether the digest must equal the first row's)
        let base: Vec<Entry<'_>> = vec![("index.js", b"exports.handler = 1;"), ("a", b"bc")];
        let rows: Vec<DigestRow<'_>> = vec![
            ("the same entries again", base.clone(), true),
            (
                "the same entries with a signature entry beside them",
                {
                    let mut with = base.clone();
                    with.push((SIGNATURE_ENTRY_NAME, b"{\"signature\":\"00\"}"));
                    with
                },
                true,
            ),
            (
                "one content byte changed",
                vec![("index.js", b"exports.handler = 2;"), ("a", b"bc")],
                false,
            ),
            (
                "one entry renamed",
                vec![("index.mjs", b"exports.handler = 1;"), ("a", b"bc")],
                false,
            ),
            (
                "the same bytes split differently across name and content",
                vec![("index.js", b"exports.handler = 1;"), ("ab", b"c")],
                false,
            ),
            (
                "the entries in the other order",
                vec![("a", b"bc"), ("index.js", b"exports.handler = 1;")],
                false,
            ),
            ("one entry dropped", vec![("index.js", b"exports.handler = 1;")], false),
            ("no entries at all", vec![], false),
        ];
        let expected = content_digest(&archive(&base)).expect("the base archive digests");
        assert_eq!(
            expected.len(),
            48,
            "expected a SHA-384 digest, got {} bytes",
            expected.len()
        );
        for (case, entries, same) in rows {
            let digest = content_digest(&archive(&entries))
                .unwrap_or_else(|| panic!("case {case:?}: expected a digest"));
            assert_eq!(
                digest == expected,
                same,
                "case {case:?}: expected the digest to {} the base's, got {}",
                if same { "equal" } else { "differ from" },
                ::hex::encode(&digest)
            );
        }
    }

    /// An object that is no readable archive hashes to nothing at all.
    #[test]
    fn an_object_that_is_no_archive_has_no_content_digest() {
        // (case, the object's bytes)
        let rows: [(&str, &[u8]); 4] = [
            ("plain text", b"this is not a valid zip archive"),
            ("empty", b""),
            ("a truncated local header", b"PK\x03\x04"),
            ("a gzip member", b"\x1f\x8b\x08\x00\x00\x00\x00\x00"),
        ];
        for (case, bytes) in rows {
            let digest = content_digest(bytes);
            assert!(
                digest.is_none(),
                "case {case:?}: expected no digest, got {digest:?}"
            );
        }
    }

    /// A signature is read back only when it covers the package it was read out of.
    #[test]
    fn a_signature_is_read_back_only_when_it_covers_these_contents() {
        let entries: Vec<Entry<'_>> = vec![("index.js", b"exports.handler = 1;")];
        let digest = content_digest(&archive(&entries)).expect("the contents digest");
        let other: Vec<Entry<'_>> = vec![("index.js", b"exports.handler = 2;")];
        // (case, the package, whether a signature is read back)
        let rows: Vec<PackageRow<'_>> = vec![
            ("signed, the envelope covering these contents", signed(&entries, &digest), true),
            (
                "signed, the envelope lifted onto other contents",
                signed(&other, &digest),
                false,
            ),
            (
                "signed, the envelope stating a digest of nothing",
                signed(&entries, b""),
                false,
            ),
            ("unsigned", archive(&entries), false),
            ("no archive at all", b"not a zip".to_vec(), false),
        ];
        for (case, package, carries) in rows {
            let read = package_signature(&package);
            assert_eq!(
                read.is_some(),
                carries,
                "case {case:?}: expected {}a signature, got {read:?}",
                if carries { "" } else { "no " }
            );
            if let Some(PackageSignature {
                profile_version_arn,
                job_id,
                signed_material: covered,
                ..
            }) = read
            {
                assert_eq!(
                    (profile_version_arn.as_str(), job_id.as_str(), covered),
                    (
                        PROFILE_VERSION_ARN,
                        JOB_ID,
                        signed_material(PROFILE_VERSION_ARN, JOB_ID, &digest)
                    ),
                    "case {case:?}: expected the envelope's own values, over the material they \
                     and these contents make up"
                );
            }
        }
    }

    /// RULE: the signature covers the ATTRIBUTION as well as the contents, so the material it
    /// is checked over changes with every part of it — which is what makes an envelope whose
    /// publisher or job was rewritten after signing fail to verify.
    #[test]
    fn the_signed_material_covers_every_part_of_the_attestation() {
        let digest = content_digest(&archive(&[("index.js", b"exports.handler = 1;")]))
            .expect("the contents digest");
        let other_digest = content_digest(&archive(&[("index.js", b"exports.handler = 2;")]))
            .expect("the other contents digest");
        let base = signed_material(PROFILE_VERSION_ARN, JOB_ID, &digest);
        // (case, the material of a changed attestation)
        let rows: Vec<(&str, Vec<u8>)> = vec![
            ("the same attestation again", signed_material(PROFILE_VERSION_ARN, JOB_ID, &digest)),
            (
                "another profile version",
                signed_material(
                    "arn:aws:signer:us-west-2:000000000000:/signing-profiles/P/0000000002",
                    JOB_ID,
                    &digest,
                ),
            ),
            ("another job", signed_material(PROFILE_VERSION_ARN, "job-2", &digest)),
            ("other contents", signed_material(PROFILE_VERSION_ARN, JOB_ID, &other_digest)),
            // The length prefixes are what keep two different triples from encoding alike:
            // moving a character from the ARN into the job id must change the material.
            (
                "a part's bytes moved into the next part",
                signed_material(
                    &PROFILE_VERSION_ARN[..PROFILE_VERSION_ARN.len() - 1],
                    &format!("1{JOB_ID}"),
                    &digest,
                ),
            ),
        ];
        for (case, material) in rows {
            let same = case == "the same attestation again";
            assert_eq!(
                material == base,
                same,
                "case {case:?}: expected the material to {} the base's, got {}",
                if same { "equal" } else { "differ from" },
                ::hex::encode(&material)
            );
        }
    }

    /// An envelope that is not one states no signature: a document short of a member, one
    /// whose hexadecimal does not decode, one that is not JSON at all.
    #[test]
    fn an_envelope_that_is_not_one_states_no_signature() {
        let entries: Vec<Entry<'_>> = vec![("index.js", b"exports.handler = 1;")];
        // (case, the bytes of the signature entry)
        let rows: [(&str, &[u8]); 5] = [
            ("not JSON", b"\x00\x01\x02"),
            ("an empty object", b"{}"),
            (
                "short of the signature member",
                br#"{"profileVersionArn":"arn:aws:signer:us-west-2:000000000000:/signing-profiles/P/1","jobId":"j","contentSha384":"00","publicKey":"00"}"#,
            ),
            (
                "a member that is not hexadecimal",
                br#"{"profileVersionArn":"arn","jobId":"j","contentSha384":"zz","signature":"00","publicKey":"00"}"#,
            ),
            ("a JSON array", b"[]"),
        ];
        for (case, envelope) in rows {
            let mut all: Vec<Entry<'_>> = entries.clone();
            all.push((SIGNATURE_ENTRY_NAME, envelope));
            let read = package_signature(&archive(&all));
            assert!(
                read.is_none(),
                "case {case:?}: expected no signature, got {read:?}"
            );
        }
    }
}
