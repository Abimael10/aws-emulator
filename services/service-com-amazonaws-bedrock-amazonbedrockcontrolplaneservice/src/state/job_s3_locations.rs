// SPDX-License-Identifier: CC0-1.0

//! Whether the Amazon S3 locations a job-creating request names are locations Amazon Bedrock
//! could reach.
//!
//! A job whose configuration points at object storage is accepted on the understanding that
//! the service can READ the input it names and WRITE under the output prefix it names. Both
//! live in a bucket of the caller's own account, so whether that bucket is there at all is a
//! question the object store answers, and a submission naming one it does not hold describes
//! work that can never be carried out: Amazon Bedrock refuses it as input — the modelled
//! `ValidationException` (`@httpError(400)`) — rather than minting a job that would read
//! nothing.
//!
//! The question is put over the in-process callback every cross-service call of an emulated
//! service uses ([`super::model_invocation_run::s3_client`] — no socket, no outbound request,
//! no host resolution; the caller's URI reaches it only as a bucket name), and only the
//! object store's own ANSWER refuses anything: an emulator assembled with no Amazon S3 to
//! ask, and an answer that could not be read, leave the submission admitted, because a
//! refusal has to be grounded — the same rule the rest of this service's cross-service
//! decisions follow ([`super::model_source`], [`super::logging_destination`],
//! [`super::model_invocation_run`]).
//!
//! # What the refusal says
//!
//! The sentence Amazon Bedrock words is its own diagnosis of the call it could not make,
//! around the exception the Amazon S3 client raised for it: the LOCATION it could not reach,
//! then that service's own sentence for a bucket it does not hold, then the envelope the SDK
//! layer states around a failed call ([`inaccessible_s3_location_message`]). Every variable
//! part comes from the request being served or from the answer the object store actually gave
//! — never from one recorded run.

use ::service_common::relayed_refusal::{relay_envelope_answered, RelayedCall};

use super::model_invocation_run::{names_nothing, prefix_address, s3_client};

/// The service name the SDK layer reports Amazon S3 under in the envelope it states around a
/// failed call, which is neither that service's endpoint prefix nor the casing of its
/// `sdkId`.
const S3_SDK_SERVICE_NAME: &str = "S3";

/// Amazon S3's OWN sentence for a bucket it does not hold: the message its `NoSuchBucket`
/// answer carries (Amazon S3 API Reference, *Error Responses*: `NoSuchBucket` — "The
/// specified bucket does not exist").
///
/// Stated here because the call that establishes the absence is a `HeadBucket`, which answers
/// no body at all for a client to read a message out of — so the relaying service words the
/// called service's sentence for the class its answer put the call in, and nothing else of
/// that answer is invented: the status and both request ids below are read off the very
/// response that came back.
const NO_SUCH_BUCKET_SENTENCE: &str = "The specified bucket does not exist";

/// How an Amazon S3 location is written in a refusal: the bucket and the key prefix under it,
/// separated by `/` and closed by the separator a prefix is written with — so a location
/// names the PLACE a job reads from or writes under rather than one object in it.
///
/// `prefix` carries no separator of its own ([`prefix_address`]), so a URI naming the bucket
/// alone is the bucket and that one separator.
#[must_use]
pub fn s3_location(bucket: &str, prefix: &str) -> String {
    if prefix.is_empty() {
        return format!("{bucket}/");
    }
    format!("{bucket}/{prefix}/")
}

/// What Amazon Bedrock answers for an Amazon S3 location it could not reach: the location the
/// request named, then the exception the Amazon S3 client raised for the call it made about
/// it — that service's own sentence and the envelope the SDK layer states around a failed
/// call.
///
/// Every variable part is of THIS call: the location comes from the request being served, and
/// the status and request ids come from the answer the object store gave
/// ([`relay_envelope_answered`], the workspace's one rendering of that envelope). The fixed
/// text is the opening and the separator between them.
#[must_use]
pub fn inaccessible_s3_location_message(
    location: &str,
    reason: &str,
    call: RelayedCall<'_>,
    headers: &::aws_smithy_runtime_api::http::Headers,
) -> String {
    format!(
        "Cannot access S3 location: {location}. Error: {reason}{}",
        relay_envelope_answered(call, headers)
    )
}

/// Why the Amazon S3 location `uri` names is not one this service could reach, or `None` when
/// it is — which is the ordinary case, and the answer whenever nothing could be established.
///
/// The one question asked is whether the object store HOLDS the bucket: the object under an
/// input prefix may be staged after the job is accepted and the output objects do not exist
/// yet at all, so the bucket is what has to be there. A URI of the S3 scheme that names no
/// bucket cannot be asked about and is left to the member's own `@pattern`, which refuses it
/// ahead of the handler.
///
/// Call it with NO store of this service held: it awaits.
pub async fn inaccessible_s3_location(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    uri: &str,
) -> Option<String> {
    let (bucket, prefix) = prefix_address(uri)?;
    let error = s3_client(ctx.callback(), region)
        .head_bucket()
        .bucket(&bucket)
        .send()
        .await
        .err()?;
    if !names_nothing(&error) {
        // Not the object store reporting an absent bucket, so it establishes nothing about
        // this location — and it is worth a warning, because an emulator with no object store
        // mounted to ask is one where this pre-condition cannot be decided. The caller's own
        // value is rendered in its escaped `Debug` form, so a bucket name carrying a newline
        // cannot forge a log line.
        ::tracing::warn!(
            bucket = ?bucket,
            detail = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
            "the job's Amazon S3 location could not be resolved in this emulator's Amazon S3"
        );
        return None;
    }
    // The status and the identifiers of the call that was actually made, read off its own
    // answer rather than chosen here.
    let answer = error.raw_response()?;
    Some(inaccessible_s3_location_message(
        &s3_location(&bucket, &prefix),
        NO_SUCH_BUCKET_SENTENCE,
        RelayedCall::once(S3_SDK_SERVICE_NAME, answer.status().as_u16()),
        answer.headers(),
    ))
}

#[cfg(test)]
mod tests {
    use super::s3_location;

    /// RULE: an S3 location is the bucket and the key prefix under it, closed by the
    /// separator a prefix is written with.
    ///
    /// The rows span what an `S3Uri`/`S3UriFolder` member can name: a prefix, a nested
    /// prefix and the bucket alone.
    #[test]
    fn a_location_is_the_bucket_and_the_prefix_under_it() {
        // (case, bucket, prefix, the location it is written as)
        let rows = [
            ("a prefix", "apo-bucket", "apo-input", "apo-bucket/apo-input/"),
            (
                "a nested prefix",
                "apo-bucket",
                "jobs/apo-input",
                "apo-bucket/jobs/apo-input/",
            ),
            ("the bucket alone", "apo-bucket", "", "apo-bucket/"),
        ];
        for (case, bucket, prefix, expected) in rows {
            let actual = s3_location(bucket, prefix);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
