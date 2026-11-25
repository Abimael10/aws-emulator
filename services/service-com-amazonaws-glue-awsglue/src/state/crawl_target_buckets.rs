// SPDX-License-Identifier: CC0-1.0

//! Resolving the BUCKET behind each of a crawler's Amazon S3 targets, against the object
//! store that holds it.
//!
//! `S3Target$Path` is "the path to the Amazon S3 target" — data held by ANOTHER service, which
//! a crawler definition REFERS to. What a crawler write can still establish about such a
//! reference is the one thing the object store answers for: does Amazon S3 hold a bucket under
//! that name at all. Only S3 ANSWERING that it holds no such bucket says the target names no
//! data anywhere, and only that answer is a fault in the request; a bucket it serves, and a
//! bucket whose read it refuses — the case of a bucket that IS there and may not be read —
//! establish no such thing.
//!
//! The question is put with `HeadBucket`, the operation S3 documents for exactly it ("you can
//! use this operation to determine if a bucket exists"), over the framework's in-process
//! callback ([`smithy_server_runtime::OperationContext::callback`]) with the SAME client
//! configuration every other read this service makes of the object store uses
//! ([`super::column_statistics_source::s3_client`]) — so nothing here opens a socket, resolves
//! a name or reaches anything outside this process, and a bucket a caller created with
//! `CreateBucket` is a bucket this resolution finds.
//!
//! What the resolution may conclude is narrow, and a refusal has to be grounded in what S3
//! ANSWERED rather than assumed:
//!
//! * S3 served the head — the bucket is there, and the target is resolved;
//! * S3 answered that it holds no bucket under that name — the target names no data anywhere,
//!   which is the one outcome a crawler write is refused for;
//! * S3 answered anything else — the read is not permitted, or the answer could not be read —
//!   so the bucket is not shown to be absent and the write goes ahead;
//! * nothing answered at all (an emulator assembled with no object store to ask) — nothing is
//!   established, and the write goes ahead.
//!
//! The work one request can ask for is bounded on the only factor a caller controls — how many
//! DISTINCT buckets its targets name ([`MAX_RESOLVED_S3_TARGET_BUCKETS`]) — and the bound is
//! applied while the targets are walked rather than after: the walk stops at the ceiling, and
//! membership is decided by a hash lookup, so neither the collection nor the comparisons grow
//! with the length of the list the request sent. Awaited with no store guard held, like every
//! other question this service puts to another service.

use super::*;

/// The most Amazon S3 buckets one crawler write resolves.
///
/// CHOSEN, not documented, for the reason [`MAX_CHECKED_EVENT_QUEUES`] gives: the target lists
/// carry no modelled length, so the number of distinct buckets a request can name — and with
/// it the number of calls the write makes on the object store — is bounded here rather than
/// left to the request. Targets past the ceiling are NOT resolved and NOT refused: a write is
/// refused only for a bucket S3 was asked about and answered was not there, and whether a
/// crawl can read a target it did resolve is decided by the crawl, on the crawl
/// ([`super::crawl_source`]).
pub const MAX_RESOLVED_S3_TARGET_BUCKETS: usize = 32;

/// Every DISTINCT bucket the Amazon S3 targets of `targets` name, each beside the FIRST target
/// path that named it — which is the path a refusal about that bucket is worded from — up to
/// [`MAX_RESOLVED_S3_TARGET_BUCKETS`] of them.
///
/// A target whose `Path` names no Amazon S3 bucket at all (a value under no `s3://`-style
/// scheme, or one naming no bucket) names no bucket to resolve: what such a path is worth is
/// decided where it is read, on the crawl ([`super::crawl_source`]). Two targets in one bucket
/// are one bucket to ask about, and the walk STOPS once the ceiling has been reached, so the
/// set this holds — and every comparison made against it, each a hash lookup — is bounded by
/// that ceiling however long the list the request carried is.
pub fn crawl_target_buckets(targets: &CrawlerTargets) -> Vec<(String, String)> {
    let mut resolved: Vec<(String, String)> = Vec::new();
    let mut named: HashSet<String> = HashSet::new();
    for path in targets
        .s3_targets
        .iter()
        .flatten()
        .filter_map(|target| target.path.as_deref())
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        if resolved.len() >= MAX_RESOLVED_S3_TARGET_BUCKETS {
            break;
        }
        let Some((bucket, _)) = split_s3_location(path) else {
            continue;
        };
        if !named.insert(bucket.clone()) {
            continue;
        }
        resolved.push((bucket, path.to_string()));
    }
    resolved
}

/// The Amazon S3 target of `targets` whose bucket the object store ANSWERED it holds nothing
/// under, as the request wrote that target's `Path` — or `None` when no target is shown to name
/// an absent bucket.
///
/// Call it with NO store guard held: it awaits the object store. See the module docs for what
/// each answer establishes; every answer but "no such bucket" leaves the write to go ahead.
pub async fn absent_s3_target(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    targets: &CrawlerTargets,
) -> Option<String> {
    let resolved = crawl_target_buckets(targets);
    if resolved.is_empty() {
        return None;
    }
    let client = super::column_statistics_source::s3_client(ctx, region);
    for (bucket, path) in resolved {
        if held_bucket(&client, &bucket).await == HeldBucket::No {
            return Some(path);
        }
    }
    None
}

/// What Amazon S3 answered about holding the bucket `bucket` — the ONE statement of that
/// question in this module, so no two readers of it can come to differ about what an answer
/// meant.
///
/// The question is put with `HeadBucket`, and only S3 ANSWERING that it holds no such bucket is
/// [`HeldBucket::No`]: any other failure is a bucket this emulator may not read or an object
/// store it could not reach, which establishes nothing about the target — and is worth a
/// warning, because a deployment with no object store to ask is one where this resolution does
/// not run.
async fn held_bucket(client: &::aws_sdk_s3::Client, bucket: &str) -> HeldBucket {
    match client.head_bucket().bucket(bucket).send().await {
        Ok(_) => HeldBucket::Yes,
        Err(error) if reported_absent(&error) => HeldBucket::No,
        Err(error) => {
            // The caller's own bucket name is rendered in its escaped `Debug` form, so a name
            // carrying a newline cannot forge a log line.
            ::tracing::debug!(
                bucket = ?bucket,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "a Glue crawler's Amazon S3 target bucket was not resolved against the object \
                 store"
            );
            HeldBucket::Unanswered
        }
    }
}

/// What [`held_bucket`] established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeldBucket {
    /// Amazon S3 holds the bucket.
    Yes,
    /// Amazon S3 answered that it holds no bucket under that name.
    No,
    /// Amazon S3 did not answer the question: the read is not permitted, or there was no
    /// object store to ask. Nothing is established.
    Unanswered,
}

/// Whether a `HeadBucket` failure is Amazon S3 reporting that it holds no such bucket
/// (`NotFound`, the only error that operation models — the head of a bucket carries no
/// document, so the 404 arrives as that shape), as opposed to the call not reaching S3 or
/// being refused for want of access.
fn reported_absent(
    error: &::aws_sdk_s3::error::SdkError<
        ::aws_sdk_s3::operation::head_bucket::HeadBucketError,
        ::aws_smithy_runtime_api::client::orchestrator::HttpResponse,
    >,
) -> bool {
    error
        .as_service_error()
        .is_some_and(::aws_sdk_s3::operation::head_bucket::HeadBucketError::is_not_found)
}
