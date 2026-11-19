// SPDX-License-Identifier: CC0-1.0

//! The locations this service reports for a CALLER to FETCH, and what makes each one
//! fetchable: the `DownloadUri` of a custom vocabulary, a medical vocabulary or a
//! vocabulary filter, and the transcript, subtitle and clinical-document URIs a finished
//! job reports.
//!
//! THE ONE RULE: such a location is an address on an endpoint THIS EMULATOR is reached
//! on, naming the object this emulator really stored — never a host only the real Amazon
//! S3 serves, and never an address no HTTP client can fetch at all. The model says what
//! these members are for ("You can use this URI to access or download your transcript",
//! "use this URI to view or download the custom vocabulary"), so a client follows the
//! value it was handed; one that leads anywhere else cannot be followed from the
//! environment the API was reached in, however exactly its bucket and key are spelled.
//!
//! TWO KINDS of location, which differ only in the AUTHORITY the fetch presents. A
//! location in the SERVICE-MANAGED store ([`service_managed_bucket`]) is this service's
//! own, and the model says what one of those is: "If you didn't include
//! `OutputBucketName` in your transcription job request, your transcript is stored in a
//! service-managed bucket, and `TranscriptFileUri` provides you with a temporary URI you
//! can use for secure access to your transcript", with "Temporary URIs for
//! service-managed Amazon S3 buckets … only valid for 15 minutes"
//! (`smithy_query(query="doc", shape="Transcript", member="TranscriptFileUri")`). So such
//! a location is reported as a TEMPORARY, PRE-SIGNED address, valid for
//! [`SERVICE_MANAGED_URI_VALIDITY`] ([`service_managed_uri`]): the caller holds no
//! credential for a bucket of this service's, so the signature the URL carries is the only
//! authority its fetch presents. A location in a bucket the REQUEST named is the
//! caller's: they created it and they address it with their OWN credentials, so it is
//! reported as the plain, unsigned address of that object ([`callers_location`]) — this
//! service signs nothing for a bucket it does not own.
//!
//! WHICH of the two a record holds is read off the STORED REQUEST, never off the address:
//! a vocabulary family's file is at the bucket and key this service computes from the
//! record ([`vocabulary_download_uri`]), and a job's output is this service's own exactly
//! when the start named no bucket or location of its own
//! ([`TranscriptionJobRecord::service_managed_output_region`],
//! [`CallAnalyticsJobRecord::service_managed_output_region`]; the two medical families
//! require `OutputBucketName`, so their output is always the caller's). So nothing a
//! request supplied can decide what this service puts its own signature on.
//!
//! The address is built through the framework's own seams, which is what makes it one this
//! emulator ANSWERS: the URL signer signs for the endpoint this emulator is reached on
//! rather than for a host only AWS serves, and
//! [`::smithy_server_runtime::OperationContext::endpoint`] is that same endpoint for the
//! unsigned form. The store behind it is the one [`store_vocabulary_content`] and
//! [`store_job_outputs`] wrote the file to, reached over the in-process callback
//! ([`s3_client`]), and the bucket and key the address carries are the ones that write
//! addressed — so a fetch of a reported address is routed back into this emulator and
//! served the stored file, and the location reported and the object stored cannot drift
//! apart.

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

/// The address a location in a bucket the REQUEST named is reported at: the object that
/// location names, addressed path-style under the endpoint this emulator is reached on,
/// with nothing signed for it.
///
/// WHY the form changes at all: the record holds such a location as the `s3://BUCKET/KEY`
/// the request's own members spell ([`output_uri`]), and that is not an address any client
/// can fetch — it names no host, so the member's own promise ("You can use this URI to
/// access or download your transcript") cannot be kept by it. The object store holding the
/// object is THIS emulator's, reached at the endpoint this request arrived at, so that is
/// the authority the reported address carries, with the caller's own bucket and key
/// beneath it.
///
/// UNSIGNED, unlike a location of this service's own store ([`service_managed_uri`]): the
/// bucket is the caller's, they address it with their own credentials, and this service
/// puts no signature of its own on a bucket it does not own.
///
/// A value that names no object of the object store at all is reported as the record holds
/// it: there is nothing to address under this emulator's endpoint.
pub fn callers_location(ctx: &smithy_server_runtime::OperationContext<'_>, uri: &str) -> String {
    match s3_object_address(uri) {
        Some((bucket, key)) => format!(
            "{endpoint}/{bucket}/{key}",
            // The endpoint is `scheme://host[:port]` with no path of its own; a trailing
            // separator is dropped so the object's path is the only one the URL carries.
            endpoint = ctx.endpoint().as_str().trim_end_matches('/'),
        ),
        None => uri.to_string(),
    }
}

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

/// The address the location `uri` of a record whose output is in `own_region`'s
/// SERVICE-MANAGED store — or, for `None`, in a bucket the request named — is reported at.
///
/// The one dispatch between this module's two kinds of location, so every member of every
/// family is reported through the same decision.
///
/// WHOSE bucket a record's files are in is NOT read out of the value: the caller of this
/// function has already established it from the stored request
/// ([`TranscriptionJobRecord::service_managed_output_region`],
/// [`CallAnalyticsJobRecord::service_managed_output_region`]), and `own_region` is that
/// record's own Region. All the service-managed branch does is take the KEY out of the
/// address this service's own writer built, by stripping the EXACT prefix it builds for
/// that Region ([`service_managed_address_prefix`], written from `own_region` and from this
/// service's own bucket constant). A value that does not carry that exact prefix is not an
/// address of this store and is reported as the record holds it, so no text a request
/// supplied can decide what this service signs.
fn reported_location(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    own_region: Option<&str>,
    uri: &str,
) -> String {
    let Some(region) = own_region else {
        return callers_location(ctx, uri);
    };
    let prefix = service_managed_address_prefix(region);
    match uri.strip_prefix(&prefix).filter(|key| !key.is_empty()) {
        Some(key) => service_managed_uri(ctx, &service_managed_bucket(region), key),
        None => uri.to_string(),
    }
}

/// [`reported_location`] over an output member that may carry no location at all.
fn report(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    own_region: Option<&str>,
    member: &mut Option<String>,
) {
    if let Some(uri) = member.take() {
        *member = Some(reported_location(ctx, own_region, &uri));
    }
}

/// Report both locations a `Transcript` names, for a record whose output is in
/// `own_region`'s service-managed store or, for `None`, in a bucket the request named.
///
/// One function for the two job families that carry the shape — a transcription job and a
/// Call Analytics job both report their transcript through it — so neither can report a
/// location the other would not.
pub fn report_transcript_locations(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    own_region: Option<&str>,
    transcript: &mut Transcript,
) {
    report(ctx, own_region, &mut transcript.transcript_file_uri);
    report(ctx, own_region, &mut transcript.redacted_transcript_file_uri);
}

/// Report every location a projected `TranscriptionJob` names for a caller to fetch: its
/// transcript, the redacted twin of that transcript, and one subtitle file per requested
/// format.
///
/// `own_region` is what the record answers with
/// ([`TranscriptionJobRecord::service_managed_output_region`]): the Region of this
/// service's own store for a job that named no output bucket, and `None` for one whose
/// start named a bucket of its own, whose locations are then the caller's own addresses
/// with nothing signed for them.
pub fn report_transcription_job_locations(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    own_region: Option<&str>,
    job: &mut TranscriptionJob,
) {
    if let Some(transcript) = job.transcript.as_mut() {
        report_transcript_locations(ctx, own_region, transcript);
    }
    if let Some(subtitles) = job.subtitles.as_mut() {
        // ONE address per DISTINCT location, however many times the record reports it.
        // `Subtitles$Formats` is a list the model gives no length, and a request may name
        // one format over and over — each naming is the same file, which is why the write
        // path stores it once ([`due_output_objects`]). Reporting follows the same bound,
        // so what a read computes is decided by how many distinct locations the job HAS
        // rather than by how long a list the request sent.
        let mut reported: BTreeMap<String, String> = BTreeMap::new();
        for uri in subtitles.subtitle_file_uris.iter_mut().flatten() {
            if let Some(address) = reported.get(uri.as_str()) {
                *uri = address.clone();
                continue;
            }
            let address = reported_location(ctx, own_region, uri);
            reported.insert(uri.clone(), address.clone());
            *uri = address;
        }
    }
}

/// Report every location a projected `CallAnalyticsJob` names for a caller to fetch: the
/// transcript of the call, at each location the job reports one at. Called with the
/// record's own answer about whose bucket that is, exactly as
/// [`report_transcription_job_locations`] is.
pub fn report_call_analytics_job_locations(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    own_region: Option<&str>,
    job: &mut CallAnalyticsJob,
) {
    if let Some(transcript) = job.transcript.as_mut() {
        report_transcript_locations(ctx, own_region, transcript);
    }
}

/// Report the location a projected `MedicalTranscriptionJob` names for a caller to fetch.
///
/// `OutputBucketName` is a REQUIRED member of this family's start
/// (`smithy_query(query="operation", operation="StartMedicalTranscriptionJob")`), so its
/// transcript is always in a bucket the request named and never in this service's own
/// store — the one caller-bucket branch of the rule, with nothing signed for it.
pub fn report_medical_transcription_job_locations(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    job: &mut MedicalTranscriptionJob,
) {
    if let Some(transcript) = job.transcript.as_mut() {
        report(ctx, None, &mut transcript.transcript_file_uri);
    }
}

/// Report both locations a projected `MedicalScribeJob` names for a caller to fetch: its
/// transcript and the clinical document beside it.
///
/// `OutputBucketName` is a REQUIRED member of this family's start too
/// (`smithy_query(query="operation", operation="StartMedicalScribeJob")`), so both are
/// always in a bucket the request named, exactly as
/// [`report_medical_transcription_job_locations`] states. The two members are modelled
/// REQUIRED, so each is a value rather than an option.
pub fn report_medical_scribe_job_locations(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    job: &mut MedicalScribeJob,
) {
    if let Some(output) = job.medical_scribe_output.as_mut() {
        output.transcript_file_uri = callers_location(ctx, &output.transcript_file_uri);
        output.clinical_document_uri = callers_location(ctx, &output.clinical_document_uri);
    }
}
