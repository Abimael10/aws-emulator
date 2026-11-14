// SPDX-License-Identifier: CC0-1.0

//! Whether the Amazon S3 locations an ingestion submission names are locations this
//! account's object store HOLDS.
//!
//! RULE. `InvokeDataAutomationLibraryIngestionJob` is handed Amazon S3 locations — the
//! prefix its results go under (`outputConfiguration.s3Uri`) and, where the submission
//! reads its entities from an object rather than from an inline payload, that object
//! (`inputConfiguration.s3Object.s3Uri`). The modelled `S3Uri`
//! (`smithy_query(query="shape",shape="S3Uri")`) constrains only the FORM of such a value,
//! `^s3://[a-z0-9][\.\-a-z0-9]{1,61}[a-z0-9](/.*)?$`, so the constraint walk that runs
//! ahead of the handler can say a value is a well-formed `s3://` URI and nothing more.
//! Whether that URI addresses anything is a question the OBJECT STORE answers, and a
//! submission pointed at a bucket no object store holds is one the service refuses as
//! carrying an invalid input parameter
//! ([`super::library_ingestion::INVALID_INGESTION_INPUT_MESSAGE`]) rather than accepting
//! and minting a job for.
//!
//! The question is put with Amazon S3's `HeadBucket`, which S3 documents for exactly it
//! ("You can use this operation to determine if a bucket exists and if you have permission
//! to access it"), over the in-process callback every cross-service call of an emulated
//! service is made over ([`::service_common::s3_client`]): no socket is opened, no name is
//! resolved and nothing outside this process is reachable from it, and the caller's value
//! reaches it only as a bucket name.
//!
//! A refusal has to be GROUNDED in what the object store answered: only S3 itself reporting
//! that it holds no such bucket refuses the submission. An emulator assembled with no
//! object store to ask, or any other answer, leaves the submission admitted — nothing is
//! then known about the bucket and it is not the caller's request that is at fault. This is
//! the same reading of a cross-service answer the rest of this emulator's bucket
//! pre-conditions follow.

use ::smithy_server_runtime::EmbeddedEmulator;

use crate::ops::types::{InputConfiguration, OutputConfiguration};

/// The Amazon S3 locations one submission names, in the order they are asked about: the
/// output prefix every submission carries, then the input object when it reads one.
///
/// A submission whose source is an `inlinePayload` carries its entities in the request
/// itself and so names no input location; `outputConfiguration` is modelled `@required`
/// with a `@required` `s3Uri`, so there is always one location to ask about.
#[must_use]
pub fn submitted_locations<'a>(
    input_configuration: &'a InputConfiguration,
    output_configuration: &'a OutputConfiguration,
) -> Vec<&'a str> {
    let mut locations = vec![output_configuration.s3_uri.as_str()];
    if let Some(object) = input_configuration.s3_object.as_ref() {
        locations.push(object.s3_uri.as_str());
    }
    locations
}

/// The first of `locations` the object store ANSWERED it holds no bucket for, or `None`
/// when every one of them may be used.
///
/// Asked location by location so the first grounded refusal is the one reported, and only
/// of a value that names a bucket at all: a value that is not an `s3://` URI names nothing
/// for any store to hold, and the member's own `@pattern` refuses such a value before the
/// handler runs.
pub async fn location_of_no_bucket<'a>(
    callback: &EmbeddedEmulator,
    region: &str,
    locations: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    for location in locations {
        let Some(bucket) = bucket_of(location) else {
            continue;
        };
        if bucket_absent(callback, region, bucket).await {
            return Some(location);
        }
    }
    None
}

/// The bucket an `s3://` URI names: its first path segment, with everything under it being
/// the key or prefix the location addresses inside that bucket.
///
/// `None` for a value that is not an `s3://` URI or that names no bucket — there is then no
/// bucket to ask the object store about.
#[must_use]
pub fn bucket_of(s3_uri: &str) -> Option<&str> {
    let rest = s3_uri.trim().strip_prefix("s3://")?;
    let bucket = rest.split_once('/').map_or(rest, |(bucket, _)| bucket);
    (!bucket.is_empty()).then_some(bucket)
}

/// Whether this emulator's own Amazon S3 reports that this account holds no bucket named
/// `bucket`.
///
/// `false` means the submission may proceed, which includes every answer that is NOT S3
/// reporting the bucket absent (an emulator with no object store mounted, or any other
/// failure to ask): nothing is then known about the bucket, and a refusal has to be
/// grounded in what S3 answered rather than assumed. Such an answer is warned about,
/// because an emulator where this pre-condition cannot be decided is worth noticing.
async fn bucket_absent(callback: &EmbeddedEmulator, region: &str, bucket: &str) -> bool {
    let answer = ::service_common::s3_client::s3_client(callback, region)
        .head_bucket()
        .bucket(bucket)
        .send()
        .await;
    match answer {
        // The object store holds the bucket, so the location is one it can serve.
        Ok(_) => false,
        // S3 says this account holds no such bucket: the refusal is grounded.
        Err(error) if bucket_reported_absent(&error) => true,
        Err(error) => {
            // The caller's own value is rendered in its escaped `Debug` form, so a bucket
            // name carrying a newline cannot forge a log line.
            ::tracing::warn!(
                bucket = ?bucket,
                detail = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "an ingestion submission's bucket could not be resolved against this \
                 emulator's Amazon S3"
            );
            false
        }
    }
}

/// Whether an Amazon S3 `HeadBucket` failure is S3 reporting that the bucket does not exist
/// (`NotFound`, the only error that operation models), as opposed to the call not reaching
/// an object store at all.
fn bucket_reported_absent(
    error: &::aws_sdk_s3::error::SdkError<
        ::aws_sdk_s3::operation::head_bucket::HeadBucketError,
        ::aws_smithy_runtime_api::client::orchestrator::HttpResponse,
    >,
) -> bool {
    error
        .as_service_error()
        .is_some_and(::aws_sdk_s3::operation::head_bucket::HeadBucketError::is_not_found)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the bucket an `s3://` URI names is its first path segment, whatever follows
    /// it, and a value naming no bucket names nothing for the object store to hold.
    #[test]
    fn the_bucket_is_the_first_segment_of_an_s3_uri() {
        let rows: &[(&str, &str, Option<&str>)] = &[
            ("a bucket root", "s3://ingestion-bucket", Some("ingestion-bucket")),
            (
                "a bucket root with a trailing separator",
                "s3://ingestion-bucket/",
                Some("ingestion-bucket"),
            ),
            (
                "a prefix under the bucket",
                "s3://ingestion-bucket/library/output/",
                Some("ingestion-bucket"),
            ),
            (
                "an object under the bucket",
                "s3://ingestion-bucket/library/entities.json",
                Some("ingestion-bucket"),
            ),
            ("no scheme at all", "ingestion-bucket/output", None),
            ("another scheme", "https://example.invalid/output", None),
            ("a scheme naming no bucket", "s3:///output", None),
        ];
        for (case, s3_uri, expected) in rows {
            let actual = bucket_of(s3_uri);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the locations a submission names are the output prefix and, only when the
    /// submission reads an object, that object.
    #[test]
    fn a_submission_names_its_output_and_any_input_object() {
        let output = OutputConfiguration {
            s3_uri: "s3://output-bucket/out/".to_string(),
        };
        let inline_only = InputConfiguration::default();
        assert_eq!(
            submitted_locations(&inline_only, &output),
            vec!["s3://output-bucket/out/"],
            "case \"a submission with no input object\": only the output location is named"
        );
        let from_object = InputConfiguration {
            s3_object: Some(crate::ops::types::S3Object {
                s3_uri: "s3://input-bucket/entities.json".to_string(),
                version: None,
            }),
            inline_payload: None,
        };
        assert_eq!(
            submitted_locations(&from_object, &output),
            vec![
                "s3://output-bucket/out/",
                "s3://input-bucket/entities.json"
            ],
            "case \"a submission reading an object\": both locations are named"
        );
    }
}
