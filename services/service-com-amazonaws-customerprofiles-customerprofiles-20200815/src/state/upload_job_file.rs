// SPDX-License-Identifier: CC0-1.0

//! The upload TARGET of an upload job: the address `GetUploadJobPath` hands out for the
//! job's file, the plaintext data key a write to it is authorized with, and the bytes such
//! a write leaves on the job.
//!
//! `GetUploadJobPath` "retrieves the pre-signed URL and client token for uploading the file
//! associated with the upload job", and its `ClientToken` documents how the pair is used:
//! "the plaintext data key used to encrypt the upload file. To persist to the pre-signed
//! url, use the client token and MD5 client token as header. The required headers are as
//! follows: x-amz-server-side-encryption-customer-key: Client Token,
//! x-amz-server-side-encryption-customer-key-MD5: MD5 Client Token,
//! x-amz-server-side-encryption-customer-algorithm: AES256". So the target is written by ONE
//! request carrying those three headers, and this module is both halves of that: the address
//! and the key a read of the path reports, and the reading back of a request made to that
//! address (`crate::service` serves it, so the published target is one this emulator
//! answers).
//!
//! The data key is minted ONCE, with the job ([`super::StoredUploadJob::new`]): the file is
//! encrypted under it, so a key minted per read would leave a file written under an earlier
//! one unreadable — and `GetUploadJobPath` is `@readonly`, so it reports the key the job
//! carries rather than writing one. What a read does mint per call is the SIGNATURE on the
//! address, which is why the reported `ValidUntil` moves with the call and the stored job
//! does not.

use std::time::Duration;

use md5::{Digest, Md5};

/// How long a target this service hands out stays valid, which is the `ValidUntil`
/// `GetUploadJobPath` reports.
///
/// AWS documents no window for it, so this is a CHOSEN span: an hour is long enough to write
/// one upload file and short enough that an address is not usable for the life of the job.
/// Nothing is stored for the deadline and no handler waits for it — the SigV4 signature the
/// address carries is what stops verifying at the end of it.
pub const UPLOAD_TARGET_VALIDITY: Duration = Duration::from_secs(60 * 60);

/// The encryption algorithm a write to a target states, as the operation documents the
/// required header: `x-amz-server-side-encryption-customer-algorithm: AES256`.
pub const UPLOAD_ENCRYPTION_ALGORITHM: &str = "AES256";

/// The header a write carries the job's `ClientToken` in.
pub const CUSTOMER_KEY_HEADER: &str = "x-amz-server-side-encryption-customer-key";

/// The header a write carries the digest of that token in.
pub const CUSTOMER_KEY_MD5_HEADER: &str = "x-amz-server-side-encryption-customer-key-md5";

/// The header a write states [`UPLOAD_ENCRYPTION_ALGORITHM`] in.
pub const CUSTOMER_ALGORITHM_HEADER: &str = "x-amz-server-side-encryption-customer-algorithm";

/// The bytes of the data key one upload job's file is encrypted under: a 256-bit key, the
/// size [`UPLOAD_ENCRYPTION_ALGORITHM`] takes.
const DATA_KEY_BYTES: usize = 32;

/// Emulator resource guard, NOT an AWS quota: the most bytes ONE upload job's file may hold.
///
/// The file is caller content kept in this process, so the write is refused before anything
/// is kept rather than after. Set well above any upload an emulator test performs.
pub const MAX_UPLOAD_FILE_BYTES: usize = 5 * 1024 * 1024;

/// Emulator resource guard, NOT an AWS quota: the most bytes this emulator holds for upload
/// files AT ONCE, across every job of every domain.
///
/// [`MAX_UPLOAD_FILE_BYTES`] bounds one file; this bounds the total, so no number of jobs
/// grows the store without limit.
pub const MAX_STORED_UPLOAD_FILE_BYTES: usize = 32 * 1024 * 1024;

/// A fresh plaintext data key for one upload job's file: [`DATA_KEY_BYTES`] random bytes,
/// base64 encoded, which is the form the customer-key header carries a key in.
///
/// The randomness is the system's, drawn through the same v4 UUID generator this service
/// mints its identifiers with, twice — one UUID is 16 bytes.
pub fn new_data_key() -> String {
    let mut key = [0_u8; DATA_KEY_BYTES];
    for half in key.chunks_mut(16) {
        half.copy_from_slice(&::uuid::Uuid::new_v4().into_bytes()[..half.len()]);
    }
    ::aws_smithy_types::base64::encode(key)
}

/// The digest of `data_key` as the `-key-MD5` header carries it: the MD5 of the key's BYTES,
/// base64 encoded, which is what a client computes from the `ClientToken` it was handed.
///
/// `None` for a token that is not a base64 value, so a write presenting a digest of
/// something else is not accidentally admitted.
pub fn data_key_md5(data_key: &str) -> Option<String> {
    let bytes = ::aws_smithy_types::base64::decode(data_key).ok()?;
    Some(::aws_smithy_types::base64::encode(Md5::digest(&bytes)))
}

/// Whether a write presenting these three header values is authorized to persist the file of
/// a job whose `ClientToken` is `data_key`: the algorithm the operation documents, that very
/// token, and the token's own digest.
///
/// A write presenting anything else is not this job's: the token is the key the file is
/// encrypted under, and only the caller the path was handed to holds it.
pub fn write_is_authorized(
    algorithm: Option<&str>,
    presented_key: Option<&str>,
    presented_key_md5: Option<&str>,
    data_key: &str,
) -> bool {
    algorithm == Some(UPLOAD_ENCRYPTION_ALGORITHM)
        && presented_key == Some(data_key)
        && presented_key_md5 == data_key_md5(data_key).as_deref()
}

/// The path the upload target of one job is addressed at, under the domain-scoped
/// upload-job path its modelled operations are bound to.
///
/// The model binds no operation to it (its own suffix, beside the modelled `/path` and
/// `/stop`), which is what lets the service serve a request to it without diverting
/// anything modelled.
pub fn upload_path(domain_name: &str, job_id: &str) -> String {
    format!("/domains/{domain_name}/upload-jobs/{job_id}/file")
}

/// What a request to an upload target names: the domain holding the job, and the job whose
/// file is being written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadTarget {
    /// The `DomainName` the target was handed out under.
    pub domain_name: String,
    /// The `JobId` the target was handed out for.
    pub job_id: String,
}

/// The upload target `path` names, or `None` when the path is not one [`upload_path`]
/// publishes.
///
/// Reading the path back is a SYNTAX check only: whether the job it names is stored, and
/// whether the request is authorized to write it, is decided against the store by the
/// service that serves the request.
pub fn parse_upload_path(path: &str) -> Option<UploadTarget> {
    let mut segments = path.strip_prefix('/')?.split('/');
    let (
        Some("domains"),
        Some(domain_name),
        Some("upload-jobs"),
        Some(job_id),
        Some("file"),
        None,
    ) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    )
    else {
        return None;
    };
    if domain_name.is_empty() || job_id.is_empty() {
        return None;
    }
    Some(UploadTarget {
        domain_name: domain_name.to_string(),
        job_id: job_id.to_string(),
    })
}

impl super::ComAmazonawsCustomerprofilesCustomerprofiles20200815State {
    /// The upload job `target` names, to write its file onto, or `None` when the domain or
    /// the job it names is not stored.
    pub fn upload_job_mut(&mut self, target: &UploadTarget) -> Option<&mut super::StoredUploadJob> {
        self.domains
            .get_mut(&target.domain_name)?
            .upload_jobs
            .get_mut(&target.job_id)
    }

    /// How many bytes of upload file this store already holds, apart from the job `target`
    /// names — the slot a write to that target replaces.
    ///
    /// What [`MAX_STORED_UPLOAD_FILE_BYTES`] is measured against, so a write is sized
    /// against the whole store and not just its own job.
    pub fn stored_upload_file_bytes_apart_from(&self, target: &UploadTarget) -> usize {
        self.domains
            .iter()
            .flat_map(|(domain_name, domain)| {
                domain
                    .upload_jobs
                    .iter()
                    .map(move |(job_id, job)| (domain_name, job_id, job))
            })
            .filter(|(domain_name, job_id, _)| {
                **domain_name != target.domain_name || **job_id != target.job_id
            })
            .filter_map(|(_, _, job)| job.uploaded_file.as_ref().map(Vec::len))
            .sum()
    }
}
