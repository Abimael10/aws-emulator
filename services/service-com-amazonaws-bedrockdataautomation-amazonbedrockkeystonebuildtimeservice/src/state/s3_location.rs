// SPDX-License-Identifier: CC0-1.0

//! Which S3 location a submission of this service can name.
//!
//! RULE. A submission that points at Amazon S3 is accepted only if the locations it points
//! at are THERE: a sample asset and its ground truth are objects the job reads, and an
//! output location is the bucket it writes into, so a submission naming one the account's
//! object storage does not hold describes work that cannot be carried out, and is refused
//! with the modelled `ValidationException` (HTTP 400) before anything is minted. That
//! refusal carries this service's fixed sentence for an S3 location it cannot use
//! ([`INVALID_S3_URI_MESSAGE`]), exactly as a submission naming a Data Automation profile
//! that resolves to nothing carries its own
//! ([`super::data_automation_profile::INVALID_PROFILE_MESSAGE`]).
//!
//! The modelled `@pattern` on `S3Uri` (`smithy_query(shape="S3Uri")`:
//! `^s3://[a-z0-9][\.\-a-z0-9]{1,61}[a-z0-9](/.*)?$`) cannot make that decision: it is a
//! SHAPE check — the request boundary's constraint walk refuses a value outside it before
//! any handler runs, with the framework's constraint sentence — and the values it admits
//! include URIs that address nothing at all, let alone anything the store holds.
//!
//! What a URI names and whether the store holds it are not decided here: both are the
//! shared reading in [`::service_common::s3_uri`], over the emulator's own in-process hop
//! into Amazon S3. This module is only which QUESTION each member of this service's
//! submissions asks, and what the job DOES with the location decides that: a member the job
//! READS — a sample asset, its ground truth, an object to ingest — has to name an object the
//! store holds ([`object_is_absent`]), while a member the job WRITES — an output location —
//! has to name a bucket it holds, since the object under it is not there yet
//! ([`location_is_absent`]). The model is what says which: the members the job reads are
//! the ones it documents as the asset, the ground truth and the input object, and the
//! output members are documented as where the results are placed, whether the model gives
//! that member the `S3Object` shape (the optimization output) or a bare `S3Uri` (the
//! ingestion job's `OutputConfiguration$s3Uri`).

/// What this service answers for a request carrying an S3 location it cannot use.
///
/// FIXED text: it names neither the URI the request carried nor the member that carried
/// it, exactly as this service's other input refusals name nothing of the request
/// ([`super::data_automation_profile::INVALID_PROFILE_MESSAGE`]).
pub const INVALID_S3_URI_MESSAGE: &str = "Request has invalid S3 URI";

/// The most DISTINCT sample locations ONE submission makes this emulator resolve.
///
/// THIS EMULATOR'S OWN CEILING, not an AWS quota: `BlueprintOptimizationSamples` carries no
/// `@length` (`smithy_query(shape="BlueprintOptimizationSamples")`), so the number of
/// locations a request can name is bounded by nothing the request boundary checks, and each
/// resolution is a signed call into another emulated service. A submission therefore drives
/// at most this many reads, plus the one for the output location it names, and they are the
/// first that many distinct locations IT named. Past the bound, nothing is established
/// about the remaining locations and the submission is therefore not refused on them — the
/// same reading as a deployment with no object store to ask. It sits far above the handful
/// of samples a blueprint-optimization submission carries.
pub const MAX_RESOLVED_SUBMISSION_LOCATIONS: usize = 100;

/// Whether the OBJECT `s3_uri` names is one nothing holds — what a member the job READS
/// asks of its location.
///
/// Absence is only ever what Amazon S3 itself answered, or a URI that names no object for
/// any store to hold; with no object store mounted to ask, nothing is established and no
/// submission is refused on an assumption.
pub async fn object_is_absent(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    s3_uri: &str,
) -> bool {
    ::service_common::s3_uri::object_is_absent(callback, region, s3_uri).await
}

/// Whether the PLACE `s3_uri` names is one nothing holds — what a member the job WRITES
/// asks of its location, since the object under it is not there yet, so its bucket is what
/// has to be.
///
/// Same grounding as [`object_is_absent`].
pub async fn location_is_absent(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    s3_uri: &str,
) -> bool {
    ::service_common::s3_uri::bucket_is_absent(callback, region, s3_uri).await
}
