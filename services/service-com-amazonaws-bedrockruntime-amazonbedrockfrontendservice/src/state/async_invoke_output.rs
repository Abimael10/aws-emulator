// SPDX-License-Identifier: CC0-1.0

//! What a finished asynchronous invocation WRITES, and the settle that delivers it.
//!
//! # The rule this module implements
//!
//! `StartAsyncInvoke` accepts an invocation with an output location
//! (`outputDataConfig.s3OutputDataConfig.s3Uri`), and the whole point of the asynchronous
//! API is that a client polls the invocation to a terminal status and then READS what it
//! produced out of its own bucket. So an invocation reaching the modelled `Completed`
//! status is not the status field alone: its output has to be there to read, under the
//! invocation's own segment of the submitted prefix, by the time any read reports it
//! finished. An invocation whose output could NOT be written carried out none of the work
//! it was accepted for, so it ends in the modelled `Failed` status carrying the reason
//! ([`super::ASYNC_INVOKE_FAILURE_MESSAGE`]) rather than reporting a success no read of the
//! object store would confirm.
//!
//! The location a submission configures is a prefix every invocation submitted against it
//! shares, so the location an invocation is REPORTED at is derived from it rather than
//! echoed: that prefix extended by the minted invocation id
//! ([`invocation_output_data_config`]), which is the one location the artifacts are written
//! under and every read of the invocation reports.
//!
//! # What it writes and what it will not claim
//!
//! Two objects per finished invocation, both under that per-invocation location:
//!
//! * the generated MEDIA object of the requested modality
//!   ([`super::async_invoke_video::MEDIA_OBJECT`]), which is what a client polls the
//!   invocation for and then reads out of its own bucket;
//! * the MANIFEST of the job, at `manifest.json`, which is what indexes that output under
//!   the prefix every invocation submitted against that location shares. Everything it
//!   states is a fact of the record — which invocation it is, which model was invoked, when
//!   the invocation was submitted and when it ended, and where its own segment is.
//!
//! No video model runs in this emulator, and the media object does not pretend one did:
//! it is a real video file, of the geometry and length the request configured, whose
//! frames this emulator renders and a maintained encoder writes — and what those frames
//! SAY is that no model ran, beside the facts of the invocation that was served (see
//! [`super::async_invoke_video`]). The status, the timestamps, the objects and their
//! contents are all real; the scene a model would have rendered is the one thing absent,
//! and nothing here is dressed up as it.
//!
//! # Where the delivery happens
//!
//! Through the Amazon S3 emulator's own store on the shared engine — the SAME store
//! `PutObject` writes and `GetObject` / `ListObjectsV2` read — so one object has one home
//! and a client reads the invocation's output through Amazon S3's own operations. The
//! guard of this service's store is NEVER held while the object store's is taken (see
//! [`settle_due_invocations`]), exactly as the metric publication beside it
//! ([`super::invocation_metrics`]) holds neither.

use ::service_com_amazonaws_s3_amazons3 as s3;

use crate::ops::types::{AsyncInvokeOutputDataConfig, AsyncInvokeS3OutputDataConfig};

use super::AsyncInvokeRecord;

/// The object a finished invocation writes under its own segment of the submitted prefix
/// beside its generated media: the manifest that indexes what the invocation produced.
pub const MANIFEST_OBJECT: &str = "manifest.json";

/// The version the manifest states for its own layout, so a reader can tell one layout
/// from the next.
const MANIFEST_VERSION: &str = "1.0";

/// What delivering one due invocation's output needs, taken off the record under this
/// service's guard so nothing is held while the object store is written.
#[derive(Debug, Clone)]
pub struct DueInvocation {
    /// The invocation's own ARN: what the settle applies the terminal transition to.
    pub invocation_arn: String,
    /// The invocation id inside that ARN, which is the segment its output lives under.
    pub invocation_id: String,
    /// The id of the call that submitted the invocation, which its delivered output names
    /// as the request it came from.
    pub request_id: String,
    /// The model the invocation was submitted against.
    pub model_id: String,
    /// The invocation's OWN output location ([`invocation_output_data_config`]): where the
    /// output goes, which is also what every read reports for it.
    pub output_data_config: AsyncInvokeOutputDataConfig,
    /// `modelInput` as submitted, as JSON: what the generated artifact is rendered from —
    /// the geometry and the length the invoked model's own request schema states
    /// ([`super::async_invoke_video::plan`]).
    pub model_input: ::serde_json::Value,
    /// Epoch milliseconds of the submitting call.
    pub submit_time: i64,
    /// Epoch milliseconds the invocation's terminal transition was due at, which is the
    /// instant the record carries as its end time.
    pub settles_at: i64,
}

impl DueInvocation {
    /// What one record needs delivering, or `None` for a record whose ARN names no
    /// invocation id — which is not a record this service minted, so it has no segment to
    /// write under.
    pub fn of(record: &AsyncInvokeRecord) -> Option<Self> {
        let invocation_id = ::service_common::arn::resource_id_of_type(
            &record.invocation_arn,
            super::ASYNC_INVOKE_ARN_RESOURCE_TYPE,
        )?;
        Some(Self {
            invocation_arn: record.invocation_arn.clone(),
            invocation_id: invocation_id.to_string(),
            request_id: record.request_id.clone(),
            model_id: record.model_id.clone(),
            output_data_config: record.output_data_config.clone(),
            model_input: super::document_to_json(&record.model_input),
            submit_time: record.submit_time,
            settles_at: record.settles_at,
        })
    }
}

/// The bucket and the prefix an `s3OutputDataConfig.s3Uri` addresses, with any trailing
/// key separator dropped.
///
/// An output location is a PREFIX rather than one object, so a URI naming only a bucket
/// addresses its root (an empty prefix) rather than nothing, and the same prefix written
/// with and without a trailing separator addresses the same place. `None` for a value that
/// addresses no bucket at all.
pub fn output_location(config: &AsyncInvokeOutputDataConfig) -> Option<(&str, &str)> {
    let AsyncInvokeOutputDataConfig::s3OutputDataConfig(s3_config) = config;
    let rest = s3_config.s3_uri.trim().strip_prefix("s3://")?;
    let (bucket, tail) = rest.split_once('/').unwrap_or((rest, ""));
    (!bucket.is_empty()).then_some((bucket, tail.trim_end_matches('/')))
}

/// What this service answers a submission whose configured output location it cannot
/// write to.
///
/// The sentence Amazon Bedrock answers such a submission with, entirely fixed: it names
/// neither the location nor the bucket, so the wording is the whole requirement. The
/// refusal is about the CREDENTIALS the service would write with — the destination is
/// resolved as the submitting account, and a location that account cannot reach is
/// refused at submit time rather than accepted as a job that could never deliver.
pub const UNWRITABLE_OUTPUT_LOCATION_MESSAGE: &str = "Invalid S3 credentials";

/// Whether an invocation submitted against `config` could DELIVER its output — which is
/// what `StartAsyncInvoke` resolves before it accepts one.
///
/// The destination is the caller's own Amazon S3 location, so the question is Amazon S3's
/// to answer: the bucket the location addresses has to be one the object store holds,
/// because a write names an existing bucket rather than creating one
/// (`service_com_amazonaws_s3_amazons3::state::Store::put_object`). A value that addresses
/// no bucket at all ([`output_location`]) is not a location this account can write either.
///
/// An engine that carries no Amazon S3 store at all — one this crate is mounted on without
/// that service's state type — has nothing to resolve the destination against, so no
/// submission is refused on this ground there; such an invocation still reports the modelled
/// `Failed` with the reason when its output cannot be delivered ([`deliver`]), so nothing
/// reports a delivery that did not happen.
///
/// Call it with no store of this service held: the object store's own guard is taken here.
pub fn destination_is_writable<Eng: Send + Sync + 'static>(
    engine: &Eng,
    config: &AsyncInvokeOutputDataConfig,
) -> bool {
    let Some((bucket, _)) = output_location(config) else {
        return false;
    };
    let Some(store) = s3::state::service_state(engine) else {
        return true;
    };
    let held = store.lock();
    held.bucket(bucket).is_some()
}

/// One key under `prefix`: the prefix, then `segment`.
///
/// The ONE separator rule the layout of an invocation's output is cut with — the location
/// it reports and the keys it writes are both this join — so a prefix addressing the bucket
/// root contributes no segment of its own and nothing doubles a separator.
fn join_key(prefix: &str, segment: &str) -> String {
    if prefix.is_empty() {
        segment.to_string()
    } else {
        format!("{prefix}/{segment}")
    }
}

/// One invocation's own segment of the submitted prefix: the prefix, then the invocation
/// id.
///
/// The prefix a submission names is shared by every invocation submitted against it, so
/// one invocation's output lives under a segment of its OWN id.
fn invocation_prefix(prefix: &str, invocation_id: &str) -> String {
    join_key(prefix, invocation_id)
}

/// One invocation's OWN output location, DERIVED from the location its submission
/// configured when its id is minted.
///
/// The location a submission configures is a prefix every invocation submitted against it
/// shares, so what an invocation delivers under — and what every read of it reports — is
/// that prefix extended by the invocation's own id as one further key segment
/// ([`invocation_prefix`]): joined by exactly one key separator whether or not the
/// submitted value ended in one, and carrying none after it, so the value addresses the
/// invocation's own prefix. Two invocations submitted against one location therefore report
/// two distinct locations, each naming the invocation that writes it, and the location a
/// read reports is the one the artifacts are under. Every other member of the configured
/// destination is carried over exactly as submitted.
///
/// A value that addresses no bucket at all ([`output_location`]) has no prefix to extend
/// and is left as submitted; a submission configuring one is refused before an invocation
/// is minted ([`destination_is_writable`]).
pub fn invocation_output_data_config(
    submitted: &AsyncInvokeOutputDataConfig,
    invocation_id: &str,
) -> AsyncInvokeOutputDataConfig {
    let AsyncInvokeOutputDataConfig::s3OutputDataConfig(s3_config) = submitted;
    let Some((bucket, prefix)) = output_location(submitted) else {
        return submitted.clone();
    };
    AsyncInvokeOutputDataConfig::s3OutputDataConfig(AsyncInvokeS3OutputDataConfig {
        s3_uri: format!("s3://{bucket}/{}", invocation_prefix(prefix, invocation_id)),
        ..s3_config.clone()
    })
}

/// The key of the manifest of the invocation whose own output location addresses `prefix`.
pub fn manifest_key(prefix: &str) -> String {
    join_key(prefix, MANIFEST_OBJECT)
}

/// The key of the generated media object of the invocation whose own output location
/// addresses `prefix`.
pub fn media_key(prefix: &str) -> String {
    join_key(prefix, super::async_invoke_video::MEDIA_OBJECT)
}

/// The manifest document: which invocation this is, which call submitted it, what it was
/// invoked against, when it ran, and where its own segment of the output location is.
///
/// What it states is exactly what it is the index of — every member comes off the stored
/// record, the location included, so the manifest beside an artifact names the same
/// location every read of the invocation reports.
fn manifest_body(invocation: &DueInvocation) -> ::serde_json::Value {
    let AsyncInvokeOutputDataConfig::s3OutputDataConfig(config) = &invocation.output_data_config;
    ::serde_json::json!({
        "version": MANIFEST_VERSION,
        "jobId": invocation.invocation_id,
        "requestId": invocation.request_id,
        "modelId": invocation.model_id,
        "submitTime": super::datetime_of_millis(invocation.submit_time)
            .fmt(::aws_smithy_types::date_time::Format::DateTime)
            .unwrap_or_default(),
        "endTime": super::datetime_of_millis(invocation.settles_at)
            .fmt(::aws_smithy_types::date_time::Format::DateTime)
            .unwrap_or_default(),
        "s3Uri": config.s3_uri,
    })
}

/// The most invocations ONE read delivers the output of.
///
/// A delivery RENDERS and encodes this invocation's video
/// ([`super::async_invoke_video`]), which is bounded but not free, so the work one read
/// does is bounded too however many invocations the clock has made due at the same
/// instant: the rest are materialised by the reads that follow, each of which was owed the
/// same transition. The invocation a read is ABOUT is always among them
/// ([`settle_due_invocation`]), so a caller polling one job never waits on another
/// caller's delivery and never sees its own held back.
const MAX_DELIVERIES_PER_READ: usize = 8;

/// Materialise every async-invoke transition the clock has already made due at
/// `now_millis`, having first DELIVERED each invocation's output to the location its
/// submission named.
///
/// The ONE settle path every reporting operation takes, so an invocation's output is
/// written and its outcome settled the same way whichever read got there first, and the
/// object is in the caller's bucket before any read reports the invocation finished. The
/// transition was owed at this instant whether or not anybody called, which is what makes
/// materialising it part of answering a read rather than a read with a side effect of its
/// own.
///
/// Each invocation is delivered and settled with NO guard of this service's store held, so
/// this service's store and the object store are never locked at once; an invocation a
/// concurrent read already settled is left exactly as that read left it
/// ([`super::ComAmazonawsBedrockruntimeAmazonbedrockfrontendserviceState::settle_async_invoke`]).
pub async fn settle_due_invocations<Eng: Send + Sync + 'static>(engine: &Eng, now_millis: i64) {
    settle_due(engine, now_millis, None).await;
}

/// The same settle for a read ABOUT one invocation: that one is delivered before any
/// other, so the bound on how many one read delivers can never leave the invocation the
/// caller asked about undelivered.
pub async fn settle_due_invocation<Eng: Send + Sync + 'static>(
    engine: &Eng,
    now_millis: i64,
    invocation_arn: &str,
) {
    settle_due(engine, now_millis, Some(invocation_arn)).await;
}

/// Deliver and settle at most [`MAX_DELIVERIES_PER_READ`] due invocations, `first` before
/// any other when a read named one.
async fn settle_due<Eng: Send + Sync + 'static>(
    engine: &Eng,
    now_millis: i64,
    first: Option<&str>,
) {
    let Some(state) = super::service_state(engine) else {
        return;
    };
    let mut due = state.lock().due_async_invokes(now_millis);
    if let Some(first) = first {
        // A stable sort on "is not the one asked about" moves that invocation to the front
        // and leaves the order of the rest as it was.
        due.sort_by_key(|invocation| invocation.invocation_arn != first);
    }
    due.truncate(MAX_DELIVERIES_PER_READ);
    for invocation in due {
        let failure = deliver(engine, &invocation).await;
        state
            .lock()
            .settle_async_invoke(&invocation.invocation_arn, failure);
    }
}

/// Write one finished invocation's output into the location its submission named, and
/// report what stopped it, or `None` once the output is in the caller's bucket.
///
/// BOTH objects are written before anything reports the invocation finished — the
/// generated media first, then the manifest that indexes it — so a `Completed` a client
/// reads is never a prefix holding an index of an artifact that is not there. An object
/// that could not be produced or could not be written is reported as the invocation
/// FAILING.
///
/// The failure is reported in the service's own words
/// ([`super::ASYNC_INVOKE_FAILURE_MESSAGE`]) and what exactly refused the write is logged
/// where a developer can see it, with the caller's own values rendered in their escaped
/// `Debug` form so a bucket name carrying a newline cannot forge a log line.
async fn deliver<Eng: Send + Sync + 'static>(
    engine: &Eng,
    invocation: &DueInvocation,
) -> Option<String> {
    let failed = || Some(super::ASYNC_INVOKE_FAILURE_MESSAGE.to_string());
    let Some((bucket, prefix)) = output_location(&invocation.output_data_config) else {
        ::tracing::warn!(
            invocation = ?invocation.invocation_id,
            "an async invocation's output location addresses no bucket, so its output was not \
             written"
        );
        return failed();
    };
    let Some(store) = s3::state::service_state(engine) else {
        ::tracing::warn!(
            invocation = ?invocation.invocation_id,
            "no Amazon S3 is mounted beside this service, so an async invocation's output \
             could not be written"
        );
        return failed();
    };
    let mut objects: Vec<(String, Vec<u8>, &str)> = Vec::new();
    match media(invocation).await {
        Ok(Some(body)) => objects.push((
            media_key(prefix),
            body,
            super::async_invoke_video::MEDIA_MIME,
        )),
        // A model this service serves asynchronously but renders no artifact for delivers
        // its manifest alone rather than nothing at all.
        Ok(None) => {}
        Err(detail) => {
            ::tracing::warn!(
                invocation = ?invocation.invocation_id,
                detail,
                "an async invocation's generated media could not be produced"
            );
            return failed();
        }
    }
    match ::serde_json::to_vec(&manifest_body(invocation)) {
        Ok(body) => objects.push((
            manifest_key(prefix),
            body,
            super::JSON_MIME,
        )),
        Err(error) => {
            ::tracing::warn!(%error, "an async invocation's manifest could not be rendered");
            return failed();
        }
    }
    for (key, body, mime) in objects {
        let object = s3::state::Object::stored(body, Some(mime.to_string()));
        let written = {
            let mut store = store.lock();
            store.put_object(bucket, &key, object)
        };
        match written {
            Some(s3::state::ObjectWrite::Created | s3::state::ObjectWrite::Replaced) => {}
            refused => {
                ::tracing::warn!(
                    bucket = ?bucket,
                    key = ?key,
                    outcome = ?refused,
                    "an async invocation's output could not be written to this emulator's \
                     Amazon S3"
                );
                return failed();
            }
        }
    }
    None
}

/// The bytes of one invocation's generated media object, or `None` for an invoked model
/// whose artifact this service renders none of.
///
/// Rendering and encoding frames is synchronous, CPU-bound work, so it runs through the
/// ONE stage this service runs such work in ([`super::turn_composition::off_worker`]):
/// off the async worker, so a read that materialises a delivery never holds the worker
/// serving it, and inside that stage's budget.
///
/// `Err` carries what stopped the renderer, for [`deliver`] to log: an invocation whose
/// media could not be produced reports the modelled failure rather than a `Completed`
/// whose output is not there.
async fn media(invocation: &DueInvocation) -> Result<Option<Vec<u8>>, String> {
    let Some(plan) =
        super::async_invoke_video::plan(&invocation.model_id, &invocation.model_input)
    else {
        return Ok(None);
    };
    let subject = super::async_invoke_video::VideoSubject {
        model_id: invocation.model_id.clone(),
        invocation_id: invocation.invocation_id.clone(),
    };
    match super::turn_composition::off_worker(move || {
        super::async_invoke_video::render(&plan, &subject)
    })
    .await
    {
        Ok(Ok(media)) => Ok(Some(media)),
        Ok(Err(detail)) => Err(detail),
        Err(not_composed) => Err(not_composed.message),
    }
}
