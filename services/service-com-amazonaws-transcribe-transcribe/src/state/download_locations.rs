// SPDX-License-Identifier: CC0-1.0

//! The locations this service reports for a CALLER to FETCH, and what makes each one
//! fetchable: the `DownloadUri` of a custom vocabulary, a medical vocabulary or a
//! vocabulary filter, and the transcript, subtitle and clinical-document URIs a finished
//! job reports.
//!
//! TWO KINDS of location, and only one of them is this service's own. A location in a
//! bucket the REQUEST named is the caller's: they created it, they address it with their
//! own credentials, and it is reported exactly as the record holds it. A location in the
//! SERVICE-MANAGED store ([`service_managed_bucket`]) is this service's, and the model
//! says what one of those is: "If you didn't include `OutputBucketName` in your
//! transcription job request, your transcript is stored in a service-managed bucket, and
//! `TranscriptFileUri` provides you with a temporary URI you can use for secure access to
//! your transcript", with "Temporary URIs for service-managed Amazon S3 buckets … only
//! valid for 15 minutes" (`smithy_query(query="doc", shape="Transcript",
//! member="TranscriptFileUri")`). So such a location is reported as a TEMPORARY,
//! PRE-SIGNED address, valid for [`SERVICE_MANAGED_URI_VALIDITY`]
//! ([`service_managed_uri`]).
//!
//! WHICH of the two a record holds is read off the STORED REQUEST, never off the address:
//! a vocabulary family's file is at the bucket and key this service computes from the
//! record ([`vocabulary_download_uri`]), and a job's output is this service's own exactly
//! when the start named no bucket or location of its own
//! ([`TranscriptionJobRecord::service_managed_output_region`],
//! [`CallAnalyticsJobRecord::service_managed_output_region`]). So nothing a request
//! supplied can decide what this service puts its own signature on.
//!
//! WHY the signature is the whole point: a caller follows one of these addresses with no
//! credential of its own — it is a bare `GET` by a browser, `curl` or any HTTP client — so
//! the signature the URL carries is the only authority the fetch presents. An address
//! carrying none is a request by nobody for an object in a private bucket, which the
//! object store refuses, and nothing could ever be read through it.
//!
//! The address is signed through the framework's own URL signer, which is also what makes
//! it one this emulator ANSWERS: it signs for the endpoint this emulator is reached on
//! rather than for a host only AWS serves, and for the signing name of the store the file
//! really lives in — the same store [`store_vocabulary_content`] and [`store_job_outputs`]
//! wrote it to, reached over the in-process callback ([`s3_client`]) — so a fetch of the
//! address is routed back into that store and served the stored file. The bucket and key
//! the address carries are the ones the write addressed, so the location reported and the
//! object stored cannot drift apart.

use super::*;

/// How long a temporary URI into the service-managed store stays valid: "Temporary URIs
/// for service-managed Amazon S3 buckets are only valid for 15 minutes" (model doc for
/// `Transcript$TranscriptFileUri`).
pub const SERVICE_MANAGED_URI_VALIDITY: Duration = Duration::from_secs(15 * 60);

/// The signing name of the store this service's own files live in, which is the store a
/// fetch of a reported location has to reach: the files are written there over the
/// in-process callback ([`s3_client`]), so the signed address names the very service that
/// holds them.
const OBJECT_STORE_SIGNING_NAME: &str = "s3";

/// The temporary URI the object `key` of the service-managed `bucket` is fetched from: a
/// `GET` of that object, pre-signed for [`SERVICE_MANAGED_URI_VALIDITY`] against the
/// endpoint this emulator is reached on (see this module's header).
pub fn service_managed_uri(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    bucket: &str,
    key: &str,
) -> String {
    let path = format!("/{bucket}/{key}");
    let request = smithy_server_runtime::SignedUrlRequest::new(
        smithy_server_runtime::http::Method::GET,
        OBJECT_STORE_SIGNING_NAME,
        &path,
    );
    match ctx.url_signer().sign(request, SERVICE_MANAGED_URI_VALIDITY) {
        Ok(signed) => signed.as_str().to_string(),
        // Nothing about a stored bucket name, a stored key or this emulator's own
        // endpoint makes a `GET` unsignable, so this is unreachable in practice. The file
        // IS at that address either way, so the address is what is reported rather than
        // no location at all. The endpoint is `scheme://host[:port]` with no path of its
        // own, and a trailing separator is dropped so the object's path is the only one
        // the URL carries.
        Err(error) => {
            ::tracing::warn!(
                %error,
                path = %path,
                "a service-managed location could not be signed"
            );
            format!("{}{path}", ctx.endpoint().as_str().trim_end_matches('/'))
        }
    }
}

/// The address a location of THIS SERVICE'S OWN store in `region` is reported at: the
/// temporary pre-signed address of that object.
///
/// WHOSE bucket a record's files are in is NOT read out of the value: the caller of this
/// function has already established, from the stored request, that this record's output is
/// this service's own ([`TranscriptionJobRecord::service_managed_output_region`],
/// [`CallAnalyticsJobRecord::service_managed_output_region`]), and `region` is that
/// record's own. All this does is take the KEY out of the address this service's own writer
/// built, by stripping the EXACT prefix it builds for that Region
/// ([`service_managed_address_prefix`], written from `region` and from this service's own
/// bucket constant). A value that does not carry that exact prefix is not an address of
/// this store and is reported as the record holds it, so no text a request supplied can
/// decide what this service signs.
fn reported_own_location(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    uri: &str,
) -> String {
    let prefix = service_managed_address_prefix(region);
    match uri.strip_prefix(&prefix).filter(|key| !key.is_empty()) {
        Some(key) => service_managed_uri(ctx, &service_managed_bucket(region), key),
        None => uri.to_string(),
    }
}

/// [`reported_own_location`] over an output member that may carry no location at all.
fn report(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    member: &mut Option<String>,
) {
    if let Some(uri) = member.take() {
        *member = Some(reported_own_location(ctx, region, &uri));
    }
}

/// Report both locations a `Transcript` of a record whose output is this service's OWN
/// names.
///
/// One function for the two job families that carry the shape — a transcription job and a
/// Call Analytics job both report their transcript through it — so neither can report a
/// location the other would not.
pub fn report_transcript_locations(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    transcript: &mut Transcript,
) {
    report(ctx, region, &mut transcript.transcript_file_uri);
    report(ctx, region, &mut transcript.redacted_transcript_file_uri);
}

/// Report every location a projected `TranscriptionJob` names for a caller to fetch: its
/// transcript, the redacted twin of that transcript, and one subtitle file per requested
/// format.
///
/// Called for a job whose output is THIS SERVICE'S own, with that job's own Region — which
/// is what the record answers with ([`TranscriptionJobRecord::service_managed_output_region`]);
/// a job whose start named an output bucket of its own reports the caller's own addresses,
/// unchanged, so nothing is signed for it.
pub fn report_transcription_job_locations(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    job: &mut TranscriptionJob,
) {
    if let Some(transcript) = job.transcript.as_mut() {
        report_transcript_locations(ctx, region, transcript);
    }
    if let Some(subtitles) = job.subtitles.as_mut() {
        // ONE signature per DISTINCT location, however many times the record reports it.
        // `Subtitles$Formats` is a list the model gives no length, and a request may name
        // one format over and over — each naming is the same file, which is why the write
        // path stores it once ([`due_output_objects`]). Reporting follows the same bound,
        // so what a read computes is decided by how many distinct locations the job HAS
        // rather than by how long a list the request sent.
        let mut signed: BTreeMap<String, String> = BTreeMap::new();
        for uri in subtitles.subtitle_file_uris.iter_mut().flatten() {
            if let Some(address) = signed.get(uri.as_str()) {
                *uri = address.clone();
                continue;
            }
            let address = reported_own_location(ctx, region, uri);
            signed.insert(uri.clone(), address.clone());
            *uri = address;
        }
    }
}

/// Report every location a projected `CallAnalyticsJob` names for a caller to fetch: the
/// transcript of the call, at each location the job reports one at. Called for a job whose
/// output is this service's own, exactly as [`report_transcription_job_locations`] is.
pub fn report_call_analytics_job_locations(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    job: &mut CallAnalyticsJob,
) {
    if let Some(transcript) = job.transcript.as_mut() {
        report_transcript_locations(ctx, region, transcript);
    }
}
