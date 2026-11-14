// SPDX-License-Identifier: CC0-1.0

//! Writing the records a pipeline's `http` source accepted into the sink its configuration
//! names.
//!
//! A pipeline exists to move records from its source to its sink, so a POST the ingestion
//! endpoint answers is backed by the sink really holding what was posted: the object is
//! written into the bucket the configuration names, through the SAME in-process channel any
//! emulated service reaches another by (the AWS SDK over the operation callback, with the one
//! construction of that client this tree holds, [`::service_common::s3_client`]). What the
//! bucket holds is then readable through Amazon S3's own API, so nothing here reports a
//! delivery the object store would not show.
//!
//! One POST is one delivery, which is one object: a Data Prepper sink batches by its own
//! `threshold`, and the smallest batch that threshold admits is the records of a single
//! request — so a client that posts and then reads the bucket finds its records there rather
//! than waiting for a flush this emulator would have to schedule.

use ::std::time::SystemTime;

use ::serde_json::Value;

use super::pipeline_configuration::{S3Sink, SinkCodec};

/// The media type a newline-delimited JSON object carries.
const NDJSON_MEDIA_TYPE: &str = "application/x-ndjson";

/// The media type a JSON object carries.
const JSON_MEDIA_TYPE: &str = "application/json";

/// What every delivered object's name begins with, and the calendar layout the second it was
/// written is spelled in.
///
/// Source: the Data Prepper `s3` sink's `object_key`, whose object names are
/// `events-%{yyyy-MM-dd'T'HH-mm-ss}` followed by a unique suffix, under
/// `object_key.path_prefix`.
const OBJECT_NAME_PREFIX: &str = "events-";
const OBJECT_NAME_SECOND_FORMAT: &str = "%Y-%m-%dT%H-%M-%S";

/// How many characters of the field that tells two deliveries of the same second apart.
const UNIQUE_FIELD_CHARS: usize = 8;

/// One object a delivery wrote: where a reader of the sink finds the records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveredObject {
    pub bucket: String,
    pub key: String,
}

/// Why a delivery wrote nothing.
#[derive(Debug, Clone)]
pub struct UndeliveredRecords {
    reason: String,
}

impl UndeliveredRecords {
    fn new(reason: impl ::std::fmt::Display) -> Self {
        Self {
            reason: reason.to_string(),
        }
    }

    /// Why nothing was written, for the plane's own answer.
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl ::std::fmt::Display for UndeliveredRecords {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        write!(formatter, "{}", self.reason)
    }
}

/// Write `records` into `sink`, as one object.
///
/// `pipeline_region` is where the bucket is addressed when the sink names no Region of its
/// own. Call it with NO state guard held — it awaits.
pub async fn delivered(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    pipeline_region: &str,
    sink: &S3Sink,
    records: &[Value],
    at: SystemTime,
) -> Result<DeliveredObject, UndeliveredRecords> {
    let (body, media_type) = object_bytes(&sink.codec, records)?;
    let key = object_key(&sink.path_prefix, at);
    let region = sink.region.as_deref().unwrap_or(pipeline_region);
    let client = ::service_common::s3_client::s3_client(ctx.callback(), region);
    if let Err(error) = client
        .put_object()
        .bucket(&sink.bucket)
        .key(&key)
        .content_type(media_type)
        .body(::aws_sdk_s3::primitives::ByteStream::from(body))
        .send()
        .await
    {
        let reported = ::aws_smithy_types::error::display::DisplayErrorContext(&error).to_string();
        ::tracing::warn!(
            bucket = %sink.bucket,
            key = %key,
            error = %reported,
            "these ingested records could not be written to the pipeline's sink bucket"
        );
        return Err(UndeliveredRecords::new(format!(
            "the sink bucket {} did not accept the object: {reported}",
            sink.bucket
        )));
    }
    Ok(DeliveredObject {
        bucket: sink.bucket.clone(),
        key,
    })
}

/// The bytes one delivery's object holds, and the media type it carries, in the format the
/// sink's codec names.
///
/// A codec this emulator does not write is an error rather than bytes in another format: an
/// object written as newline-delimited JSON under a `parquet` codec is not what the
/// configuration asked for, and a reader of the bucket would be the one to find out.
pub fn object_bytes(
    codec: &SinkCodec,
    records: &[Value],
) -> Result<(Vec<u8>, &'static str), UndeliveredRecords> {
    match codec {
        SinkCodec::Ndjson => {
            let mut body = Vec::new();
            for record in records {
                body.extend_from_slice(record.to_string().as_bytes());
                body.push(b'\n');
            }
            Ok((body, NDJSON_MEDIA_TYPE))
        }
        // The delivery's records as one JSON document, which is the whole batch the object
        // holds.
        SinkCodec::Json => Ok((
            Value::Array(records.to_vec()).to_string().into_bytes(),
            JSON_MEDIA_TYPE,
        )),
        SinkCodec::Unsupported(named) => Err(UndeliveredRecords::new(format!(
            "this emulator does not write the {named} codec the sink names"
        ))),
    }
}

/// The key one delivered object takes: the sink's prefix, then the object's own name —
/// `events-<YYYY-MM-DDTHH-MM-SS>-<unique>`.
///
/// `at` is the instant the records were delivered, read as UTC calendar fields (the
/// maintained calendar crate, never by slicing a rendered stamp); the unique field tells two
/// deliveries of the same second apart ([`unique_field`]).
pub fn object_key(path_prefix: &str, at: SystemTime) -> String {
    let millis = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| since.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default();
    let second = ::chrono::DateTime::<::chrono::Utc>::from_timestamp_millis(millis)
        .unwrap_or(::chrono::DateTime::<::chrono::Utc>::UNIX_EPOCH)
        .naive_utc()
        .format(OBJECT_NAME_SECOND_FORMAT);
    format!(
        "{path_prefix}{OBJECT_NAME_PREFIX}{second}-{}",
        unique_field(at)
    )
}

/// The field one object's name ends with, so two deliveries of the same second are two
/// objects and not one overwriting the other.
///
/// Derived from the delivery INSTANT and from how many deliveries this process has written —
/// never from the records, which say nothing about which object holds them. The counter is
/// what makes two deliveries DISTINCT objects even when the clock cannot tell them apart: it
/// is a nonce for the object's name, not state anything reads.
fn unique_field(at: SystemTime) -> String {
    /// How many deliveries this process has named. Monotonic, so no two draws are equal.
    static DELIVERIES: ::std::sync::atomic::AtomicU64 = ::std::sync::atomic::AtomicU64::new(0);

    let nanos = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or_default();
    let drawn = DELIVERIES.fetch_add(1, ::std::sync::atomic::Ordering::Relaxed);
    let mut seed = nanos.to_be_bytes().to_vec();
    seed.extend_from_slice(&drawn.to_be_bytes());
    let digest = <::sha2::Sha256 as ::sha2::Digest>::digest(seed);
    ::hex::encode(&digest[..UNIQUE_FIELD_CHARS / 2])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An instant with a known UTC calendar rendering: 2026-11-14T13:35:48.250Z.
    fn at() -> SystemTime {
        SystemTime::UNIX_EPOCH + ::std::time::Duration::from_millis(1_794_663_348_250)
    }

    /// RULE: an object's key is the sink's prefix, then `events-` and the UTC second it was
    /// written, then a field that tells two deliveries of one second apart.
    #[test]
    fn an_object_key_carries_the_sinks_prefix_and_the_written_second() {
        let rows: &[(&str, &str, &str)] = &[
            ("a sink naming no prefix", "", "events-2026-11-14T13-35-48-"),
            ("a sink naming one", "logs/", "logs/events-2026-11-14T13-35-48-"),
            (
                "a nested prefix",
                "logs/orders/",
                "logs/orders/events-2026-11-14T13-35-48-",
            ),
        ];
        for (case, prefix, expected) in rows {
            let key = object_key(prefix, at());
            assert!(
                key.starts_with(expected),
                "case {case:?}: expected a key under {expected:?}, got {key:?}"
            );
            let unique = &key[expected.len()..];
            assert_eq!(
                unique.len(),
                UNIQUE_FIELD_CHARS / 2 * 2,
                "case {case:?}: expected a {UNIQUE_FIELD_CHARS}-character unique field, got \
                 {unique:?}"
            );
        }
        // Two deliveries are two objects, whether or not the clock can tell them apart.
        let later = at() + ::std::time::Duration::from_nanos(1);
        assert_ne!(
            object_key("", at()),
            object_key("", later),
            "case \"two deliveries a moment apart\": expected distinct keys"
        );
        assert_ne!(
            object_key("", at()),
            object_key("", at()),
            "case \"two deliveries of one instant\": expected distinct keys"
        );
    }

    /// RULE: the object's bytes are the delivery's records in the format the codec names,
    /// and a codec this emulator does not write produces no object at all.
    #[test]
    fn the_object_is_written_in_the_codecs_format() {
        let records: Vec<Value> = (0..3)
            .map(|index| ::serde_json::json!({ "marker": "m", "n": index }))
            .collect();
        for (case, count) in [("no records", 0_usize), ("one record", 1), ("three", 3)] {
            let batch = &records[..count];
            let (body, media_type) = object_bytes(&SinkCodec::Ndjson, batch)
                .unwrap_or_else(|error| panic!("case {case:?}: expected bytes, got {error}"));
            assert_eq!(
                media_type, NDJSON_MEDIA_TYPE,
                "case {case:?}: expected the ndjson media type, got {media_type:?}"
            );
            let text = String::from_utf8(body)
                .unwrap_or_else(|error| panic!("case {case:?}: expected text, got {error}"));
            let lines: Vec<&str> = text.lines().collect();
            assert_eq!(
                lines.len(),
                count,
                "case {case:?}: expected one line per record, got {text:?}"
            );
            for (index, line) in lines.iter().enumerate() {
                let parsed: Value = ::serde_json::from_str(line).unwrap_or_else(|error| {
                    panic!("case {case:?}: line {index} is not JSON: {error}")
                });
                assert_eq!(
                    parsed, records[index],
                    "case {case:?}: expected line {index} to be that record, got {line:?}"
                );
            }

            let (body, media_type) = object_bytes(&SinkCodec::Json, batch)
                .unwrap_or_else(|error| panic!("case {case:?}: expected bytes, got {error}"));
            assert_eq!(
                media_type, JSON_MEDIA_TYPE,
                "case {case:?}: expected the json media type, got {media_type:?}"
            );
            let parsed: Value = ::serde_json::from_slice(&body)
                .unwrap_or_else(|error| panic!("case {case:?}: expected JSON, got {error}"));
            assert_eq!(
                parsed,
                Value::Array(batch.to_vec()),
                "case {case:?}: expected the batch as one document, got {parsed:?}"
            );
        }
        let refused = object_bytes(&SinkCodec::Unsupported("parquet".to_string()), &records);
        assert!(
            refused.is_err(),
            "case \"a codec this emulator does not write\": expected no object, got {refused:?}"
        );
    }
}
