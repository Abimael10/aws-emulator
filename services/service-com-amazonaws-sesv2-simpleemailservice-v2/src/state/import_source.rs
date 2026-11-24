// SPDX-License-Identifier: CC0-1.0

//! The Amazon S3 data source an import job's run reads, and what the object store's answer
//! about it means for that run.
//!
//! The RULE this module implements is the one the model draws between an import job's two
//! terminal statuses. `JobStatus` documents `COMPLETED` as "job has completed processing
//! successfully" and `FAILED` as the status of a job an error occurred during the
//! processing of, and `FailureInfo` carries "a message about why the job failed" — so the
//! terminal status a run reaches is decided by whether the run could process the source it
//! was created for, which is the Amazon S3 object `ImportDataSource$S3Url` names ("an
//! Amazon S3 URL in the format s3://<bucket_name>/<object>"). A run whose source the
//! account's object storage ANSWERS that it holds nothing at, or that it will not serve,
//! processed nothing and reaches [`super::IMPORT_JOB_FAILED`] carrying the failure details;
//! only a run whose source the store answered FOR reaches
//! [`super::IMPORT_JOB_COMPLETED`].
//!
//! Neither the URL's reading nor the question put to the store is this service's own: what
//! an `s3://` URI names is [`::service_common::s3_uri::object_address`]'s, the hop into the
//! account's own Amazon S3 is [`::service_common::s3_client::s3_client`]'s — the AWS SDK
//! over the emulator's in-process transport, which opens no socket and reaches nothing
//! outside this process — and what the store's refusal ESTABLISHED about the address is
//! [`::service_common::s3_object_read::answered_refusal`]'s. So an object a client staged
//! with `PutObject` is the object a run resolves, and absence is only ever what the store
//! itself said: with no object store mounted to ask, nothing is established and the run is
//! not failed on an assumption.
//!
//! WHEN the resolution happens is the run's own instant: it falls due with the run's
//! terminal transition ([`super::ImportJobRecord::source_resolution_falls_due`]), because
//! that is when a run reads what it was created to read. It is therefore owed by the CLOCK
//! — due at that instant whether or not anybody called — so an operation that reports an
//! import job materialises it before reading the job, and records what the store answered
//! so two readers of one job cannot disagree about it.

use std::time::SystemTime;

use ::service_common::s3_object_read::ObjectRead;
use ::smithy_server_runtime::EmbeddedEmulator;

/// The data sources ONE call resolves.
///
/// THIS EMULATOR'S OWN CEILING, not an AWS quota: an account may hold
/// [`super::MAX_IMPORT_JOBS`] import jobs, and each resolution is a signed call into
/// another emulated service, so one reporting call cannot be made to pay for every
/// resolution the clock has made due at once. The rest stay owed, and the next call
/// resolves them — the same reading the export-job plane's per-call cap takes
/// ([`super::MAX_EXPORT_FILES_PER_CALL`]).
pub const MAX_RESOLVED_SOURCES_PER_CALL: usize = 25;

/// Resolve the data source of every import job whose run the clock has made due and whose
/// source the object store has not answered about yet, and record what it answered.
///
/// What an operation that reports an import job calls before it reads one. The store's lock
/// is NOT held across the resolution: the ids and URLs are copied out, the reads are made,
/// and the answers are written back — so a reporting call never holds the store while it
/// waits on another service.
pub async fn resolve_due_sources(
    state: &::engine::ServiceState<super::ComAmazonawsSesv2SimpleemailserviceV2State>,
    callback: &EmbeddedEmulator,
    region: &str,
    now: SystemTime,
) {
    let owed: Vec<(String, String)> = {
        let store = state.lock();
        store
            .import_jobs
            .values()
            .filter(|record| record.source_resolution_falls_due(now))
            .map(|record| (record.job_id.clone(), record.data_source.s3_url.clone()))
            .take(MAX_RESOLVED_SOURCES_PER_CALL)
            .collect()
    };
    for (job_id, s3_url) in owed {
        let answer = read_source(callback, region, &s3_url).await;
        let failure = match resolved_source(answer, &s3_url) {
            // Nothing answered, so nothing is established: the resolution stays owed and
            // the next call puts the question again.
            ResolvedSource::Owed => continue,
            ResolvedSource::Processed => None,
            ResolvedSource::Failed(failure) => Some(failure),
        };
        let mut store = state.lock();
        if let Some(record) = store.import_jobs.get_mut(&job_id) {
            record.source_answered = true;
            record.failure = failure;
        }
    }
}

/// What the account's own Amazon S3 answered about the object the `s3://` URL `s3_url`
/// names.
///
/// A HEAD, not a GET: what the run asks of its source here is whether there is an object to
/// process at the address the create supplied, so none of its bytes are read. A URL that
/// names no object at all — another scheme, no key, a name Amazon S3 holds no bucket under
/// — is an address no store could ever serve, so nothing is asked for it.
async fn read_source(callback: &EmbeddedEmulator, region: &str, s3_url: &str) -> ObjectRead {
    let Some((bucket, key)) = ::service_common::s3_uri::object_address(s3_url) else {
        return ObjectRead::Absent;
    };
    let Some(region) = ::service_common::arn::region_label(region) else {
        return ObjectRead::Unanswered;
    };
    match ::service_common::s3_client::s3_client(callback, region)
        .head_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
    {
        Ok(_) => ObjectRead::Served(Vec::new()),
        Err(error) => ::service_common::s3_object_read::answered_refusal(&error),
    }
}

/// What a run's read of its own data source established.
enum ResolvedSource {
    /// The store served the object, so the run has the source it was created to process.
    Processed,
    /// The store answered against the source, and these are the failure details the run
    /// carries.
    Failed(crate::ops::types::FailureInfo),
    /// Nothing answered, so nothing is established and the resolution is still owed.
    Owed,
}

/// What `answer` establishes about the run whose source `s3_url` names.
///
/// * the store SERVED the object, so the run has its source and completes;
/// * the store answered that it holds NOTHING there, that the read is NOT PERMITTED, or
///   something else about that object: the run has no source it can process, and the
///   failure says which of those the store said;
/// * NOTHING answered — no object store mounted, or a call that could not be dispatched —
///   so nothing is known about the source and the run is not failed for it.
fn resolved_source(answer: ObjectRead, s3_url: &str) -> ResolvedSource {
    let reason = match answer {
        ObjectRead::Served(_) => return ResolvedSource::Processed,
        ObjectRead::Absent => source_absent_message(s3_url),
        ObjectRead::Denied => source_denied_message(s3_url),
        ObjectRead::Unreadable => source_unreadable_message(s3_url),
        ObjectRead::Unanswered => return ResolvedSource::Owed,
    };
    ResolvedSource::Failed(crate::ops::types::FailureInfo {
        // "A message about why the job failed": what the store answered about the source
        // this job was created to read.
        error_message: Some(reason),
        // "An Amazon S3 pre-signed URL that contains all the failed records": a run that
        // read no source has no records to report, failed or otherwise, so there is no such
        // file to hand out.
        failed_records_s3_url: None,
    })
}

/// What every failure of a run's own SOURCE says before the store's answer: the run failed
/// on the source rather than on anything in it, which is what tells this failure apart from
/// one diagnosed per record.
const UNREADABLE_SOURCE: &str = "The import job could not read its data source.";

/// How this service words `FailureInfo$ErrorMessage` for a run whose data source the object
/// store answered it holds no object at.
///
/// The model documents the member only as "a message about why the job failed", so the
/// sentence states the condition and the one variable part it has: the data source the
/// create supplied, which is what a client has to fix. Stated here, once per answer the
/// store can give, so every operation that reports an import job reports the same sentence
/// for the same answer.
#[must_use]
pub fn source_absent_message(s3_url: &str) -> String {
    format!("{UNREADABLE_SOURCE} Amazon S3 holds no object at {s3_url}.")
}

/// How this service words `FailureInfo$ErrorMessage` for a run whose data source the object
/// store answered it will not serve. Same grounding as [`source_absent_message`].
#[must_use]
pub fn source_denied_message(s3_url: &str) -> String {
    format!("{UNREADABLE_SOURCE} Amazon SES is not permitted to read the object at {s3_url}.")
}

/// How this service words `FailureInfo$ErrorMessage` for a run whose data source the object
/// store answered something else about. Same grounding as [`source_absent_message`].
#[must_use]
pub fn source_unreadable_message(s3_url: &str) -> String {
    format!("{UNREADABLE_SOURCE} The object at {s3_url} could not be read.")
}
