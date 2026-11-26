// SPDX-License-Identifier: CC0-1.0

//! What a load READS: the objects its `source` names, taken out of this account's own
//! object store.
//!
//! A bulk load is a real run here, not a promise: the location the request named is listed
//! and fetched through the emulator's OWN Amazon S3 over the in-process callback
//! (`::service_common::s3_client`), each object is decoded in the format the request named
//! ([`super::formats`]), and what the load writes into the graph is exactly what those
//! objects held. So every number the job reports is a measurement of that run, and a load
//! reports that it completed only for data the graph really holds afterwards.
//!
//! The read happens in the handler that accepts the load — the one place with a request
//! context to reach another service through — and it holds no state guard while it awaits.
//! What it could not read is the LOAD's own outcome and error log, never a refusal of the
//! request that accepted it: Amazon S3 is where the loader's own work happens, so a source
//! it cannot read is a load that failed rather than a request that was wrong.

use super::*;

/// The most objects one load reads out of its source location.
///
/// Emulator resource guard, NOT a quota AWS publishes: the data a load reads is buffered
/// in this process, so without a ceiling one request could ask the emulator to hold an
/// unbounded amount of it.
pub const MAX_LOAD_SOURCE_OBJECTS: usize = 64;

/// The most bytes one object of a load's source may hold.
pub const MAX_LOAD_OBJECT_BYTES: usize = 4 * 1024 * 1024;

/// The most bytes one load reads out of its source location in total.
pub const MAX_LOAD_SOURCE_BYTES: usize = 16 * 1024 * 1024;

/// Read the data `request`'s source names out of this emulator's own Amazon S3, and report
/// what that established.
///
/// Call it with NO state guard held — it awaits.
pub async fn read_load_source(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    request: &LoadRequest,
) -> LoadReading {
    // The `s3://` URI and the two Amazon S3 endpoint-URL spellings the member documents
    // ("The URI can be in any of the following formats") are read by the tree's ONE
    // reading of such a location, so this loader resolves the same object a caller's URI
    // names as every other service does. A location naming a bucket and no key addresses
    // the whole bucket, which is a prefix of every key in it.
    let address = ::service_common::s3_uri::url_object_address(&request.source)
        .or_else(|| ::service_common::s3_uri::bucket_name(&request.source).map(|bucket| (bucket, String::new())));
    let Some((bucket, prefix)) = address else {
        return LoadReading::halted(
            LOAD_S3_READ_ERROR,
            LoadErrorLog::new(
                S3_READ_ERROR_CODE,
                "The source names no Amazon S3 location, so the load had nothing to read.",
                super::super::echoed(&request.source),
                0,
            ),
        );
    };
    let client = ::service_common::s3_client::s3_client(ctx.callback(), &request.region);
    let objects = match source_objects(&client, &bucket, &prefix).await {
        Ok(objects) => objects,
        Err(message) => {
            return LoadReading::halted(
                LOAD_S3_READ_ERROR,
                LoadErrorLog::new(S3_READ_ERROR_CODE, message, object_uri(&bucket, &prefix), 0),
            );
        }
    };
    if objects.is_empty() {
        return LoadReading::halted(
            LOAD_S3_READ_ERROR,
            LoadErrorLog::new(
                S3_READ_ERROR_CODE,
                "The source location holds no object to load.",
                object_uri(&bucket, &prefix),
                0,
            ),
        );
    }

    let mut builder = LoadBuilder::new(request);
    let mut remaining = MAX_LOAD_SOURCE_BYTES;
    for (key, listed) in &objects {
        if builder.stopped() {
            break;
        }
        builder.begin_object(object_uri(&bucket, key));
        // The LISTED size decides whether the object is fetched at all, so the ceilings
        // bound what this process holds rather than being checked once it already holds
        // it.
        if *listed > MAX_LOAD_OBJECT_BYTES || *listed > remaining {
            builder.object_too_large(MAX_LOAD_OBJECT_BYTES);
            break;
        }
        let fetched = client.get_object().bucket(&bucket).key(key).send().await;
        let Ok(object) = fetched else {
            builder.object_unreadable();
            continue;
        };
        let Ok(body) = object.body.collect().await else {
            builder.object_unreadable();
            continue;
        };
        let bytes = body.into_bytes();
        // The backstop for an object whose fetched bytes disagree with its listing.
        if bytes.len() > MAX_LOAD_OBJECT_BYTES || bytes.len() > remaining {
            builder.object_too_large(MAX_LOAD_OBJECT_BYTES);
            break;
        }
        remaining -= bytes.len();
        let read_as = request.clone();
        // Decoding an object is CPU-bound and sized by what it holds, so it runs on a
        // BLOCKING task rather than on a runtime worker a large source would starve:
        // awaiting the join is a real await point, so a cancellation and the health
        // endpoint keep working while one object decodes.
        let decoded = ::tokio::task::spawn_blocking(move || {
            read_object(&mut builder, &read_as, &bytes);
            builder
        })
        .await;
        match decoded {
            Ok(carried) => builder = carried,
            // The decode task did not come back, so the load has no reading that
            // describes a run.
            Err(_) => {
                return LoadReading::halted(
                    LOAD_S3_READ_ERROR,
                    LoadErrorLog::new(
                        S3_READ_ERROR_CODE,
                        "An object of the source could not be decoded.",
                        object_uri(&bucket, key),
                        0,
                    ),
                );
            }
        }
    }
    builder.finish()
}

/// The keys one load reads under `prefix` with their LISTED sizes, in listing order, or
/// the sentence the load's error log carries when the location could not be listed.
///
/// A source may name one object or a prefix holding many, and a key is a prefix of itself,
/// so both are read with the one listing the prefix gives. A key ending in the delimiter is
/// a folder marker rather than a file of data.
async fn source_objects(
    client: &::aws_sdk_s3::Client,
    bucket: &str,
    prefix: &str,
) -> Result<Vec<(String, usize)>, String> {
    let mut keys: Vec<(String, usize)> = Vec::new();
    // Every key the listing ANSWERS counts against the ceiling, not only the ones read: a
    // prefix of folder markers would otherwise page on without the guard ever firing.
    let mut scanned = 0usize;
    let mut continuation: Option<String> = None;
    loop {
        let page = client
            .list_objects_v2()
            .bucket(bucket)
            .prefix(prefix)
            .set_continuation_token(continuation.clone())
            .send()
            .await;
        let page = match page {
            Ok(page) => page,
            Err(error) => {
                // Either the object store answered that it holds nothing at this address,
                // or nothing answered the read at all — and the second is worth a warning,
                // because a deployment with no Amazon S3 mounted to ask is one where no
                // load ever reads anything. The caller's own values are rendered in their
                // escaped form, so a bucket or key carrying a newline cannot forge a log
                // line.
                ::tracing::warn!(
                    bucket = ?bucket,
                    prefix = ?prefix,
                    code = ?::aws_smithy_types::error::metadata::ProvideErrorMetadata::code(&error),
                    "a neptunedata bulk load could not list its source in the emulator's Amazon S3"
                );
                return Err(
                    "The source location could not be read in Amazon S3.".to_string()
                );
            }
        };
        for object in page.contents() {
            let Some(key) = object.key() else { continue };
            scanned += 1;
            if scanned > MAX_LOAD_SOURCE_OBJECTS {
                return Err(format!(
                    "The source location holds more than the {MAX_LOAD_SOURCE_OBJECTS} objects \
                     this emulator reads in one load."
                ));
            }
            if key.ends_with('/') {
                continue;
            }
            keys.push((
                key.to_string(),
                usize::try_from(object.size().unwrap_or_default().max(0)).unwrap_or(0),
            ));
        }
        match page.next_continuation_token() {
            Some(token) => continuation = Some(token.to_string()),
            None => break,
        }
    }
    Ok(keys)
}

/// The `s3://` location of one object, which is what a job's error log names a file by.
fn object_uri(bucket: &str, key: &str) -> String {
    format!("s3://{bucket}/{key}")
}
