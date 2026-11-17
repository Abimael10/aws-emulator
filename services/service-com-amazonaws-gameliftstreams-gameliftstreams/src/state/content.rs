// SPDX-License-Identifier: CC0-1.0

//! The ONE read of the content an application is created from: WHAT the Amazon S3
//! location a request's `ApplicationSourceUri` names holds, and whether it can be read
//! at all.
//!
//! "Enter an Amazon S3 URI to a bucket that contains your game or other application"
//! (`CreateApplication`'s `ApplicationSourceUri`), and the create "copies the content
//! and caching for future deployment in a stream group" (`ApplicationStatus`'s
//! `PROCESSING`). So the create is the one call that reads the caller's bucket
//! ("Creating an application is the only time Amazon GameLift Streams accesses your
//! Amazon S3 bucket"), and three things about that location are properties of the
//! OBJECTS there at the instant of the create:
//!
//! * whether any object is there at all — content the service cannot find is not an
//!   application that streams nothing, it is a request to correct;
//! * whether the objects may be LISTED and READ — the two actions the copy needs;
//! * how many bytes they hold, which the application's read path reports as
//!   `StorageBytes` — so it is read once, here, and recorded on the application record
//!   rather than recomputed on every read.
//!
//! All three come from ONE answer ([`SourceContent`]), given by the object store itself:
//! nothing here decides that a location is missing or unreadable on its own, and a call
//! nothing answered establishes neither.
//!
//! The read is made the way any emulated service reaches another: the AWS SDK for Amazon
//! S3 driven over this emulator's own in-process transport, through the operation
//! callback ([`smithy_server_runtime::OperationContext::callback`]), built by the one
//! shared constructor ([`service_common::s3_client`]). No socket is opened, no name is
//! resolved and nothing outside this process is reachable from it, so the objects a
//! caller staged with `PutObject` are the objects this read sums.
//!
//! The walk is BOUNDED in every direction — at most [`MAX_CONTENT_PAGES`] pages,
//! [`MAX_CONTENT_OBJECTS`] objects and [`MAX_ACCESS_PROBES`] read probes — because one
//! call of `CreateApplication` has to reach an answer whatever a bucket holds.

/// How many listing pages of the content location this read walks before it stops.
///
/// A listing that kept handing back a continuation token would otherwise be awaited
/// without end.
const MAX_CONTENT_PAGES: usize = 100;

/// How many objects of the content location this read counts before it stops.
const MAX_CONTENT_OBJECTS: usize = 100_000;

/// How many of the objects found at the content location this read asks the store for
/// read access to.
///
/// The copy needs to read EVERY object under the location, so every object found is a
/// probe — up to this bound, which keeps one `CreateApplication` call bounded over a
/// location holding more objects than a request can walk.
const MAX_ACCESS_PROBES: usize = 100;

/// What a read of the content at an application's source URI established.
///
/// Each variant is something the OBJECT STORE answered, never something this read
/// assumed: an absence is the store saying it holds nothing there, a denial is the store
/// refusing the actions the copy needs, and a call nothing answered establishes neither.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceContent {
    /// The store served the listing, and the objects at the location hold this many
    /// bytes in total.
    Stored(i64),
    /// The store answered that it holds no object at the location — an empty prefix, or
    /// no such bucket — or the value names no Amazon S3 location at all.
    Empty,
    /// The store answered that listing the location, or reading an object of it, is not
    /// permitted.
    Denied,
    /// Nothing answered the read: this emulator holds no object store to ask, or the
    /// call could not be dispatched. Nothing about the location is established.
    Unanswered,
}

/// `source_uri` as the PREFIX it names: the same value with exactly one trailing `/`.
///
/// "The location can have a multi-level prefix structure, but it must include all the
/// files needed to run the content. Amazon GameLift Streams copies everything under the
/// specified location" (`ApplicationSourceUri`), so the value is a LOCATION rather than
/// an object — and the service reports it back in that one spelling: a value already
/// ending in the separator is answered unchanged, and one that does not gains a single
/// separator. The application's read path reports the normalized value, because the
/// stored record is what every read answers from.
///
/// It is the REPORTED spelling only. What the content is read from is the value as the
/// request wrote it ([`source_content`]), since a listing prefix is a byte prefix of the
/// key and a separator added to it would exclude the object a URI names directly.
#[must_use]
pub fn normalized_source_uri(source_uri: &str) -> String {
    match source_uri.ends_with('/') {
        true => source_uri.to_string(),
        false => format!("{source_uri}/"),
    }
}

/// The bucket and key prefix an `s3://bucket/prefix` URI names, or `None` when the
/// value is not such a URI at all.
///
/// The prefix is the key portion exactly as written: `s3://b/build/` names the keys
/// under `build/`, `s3://b` names every key of the bucket. A trailing separator is
/// NOT added or removed — an S3 listing prefix is a byte prefix of the key, and adding
/// one would exclude the object a URI names directly.
#[must_use]
pub fn source_location(source_uri: &str) -> Option<(&str, &str)> {
    let rest = source_uri.strip_prefix("s3://")?;
    let (bucket, prefix) = match rest.split_once('/') {
        Some((bucket, prefix)) => (bucket, prefix),
        None => (rest, ""),
    };
    (!bucket.is_empty()).then_some((bucket, prefix))
}

/// What the content at `source_uri` is, read through this emulator's own Amazon S3 in
/// `region`, with the URI's key portion as the listing prefix exactly as the request
/// wrote it ([`source_location`]).
///
/// The location is LISTED — the `s3:ListBucket` half of what the copy needs — and the
/// objects it reports are then asked for individually — the `s3:GetObject` half — so a
/// location the store will not let this service read is reported as refused rather than
/// as empty. A listing that names no object is [`SourceContent::Empty`], and a listing
/// the store served is [`SourceContent::Stored`] with the sum of the sizes of its
/// objects.
pub async fn source_content(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    source_uri: &str,
) -> SourceContent {
    let Some((bucket, prefix)) = source_location(source_uri) else {
        return SourceContent::Empty;
    };
    // Built by the one shared constructor, so this hop is configured exactly as every
    // other emulated service's hop into the object store is. The endpoint it carries is
    // addressing for the emulator's own router, not a host anything connects to.
    let client = ::service_common::s3_client::s3_client(ctx.callback(), region);
    let mut total: i64 = 0;
    let mut counted: usize = 0;
    // The objects whose read access is probed below, collected while the listing is
    // walked so the store is asked about the keys it ITSELF reported.
    let mut probes: Vec<String> = Vec::new();
    let mut continuation: Option<String> = None;
    for _ in 0..MAX_CONTENT_PAGES {
        let mut listing = client.list_objects_v2().bucket(bucket).prefix(prefix);
        if let Some(token) = continuation {
            listing = listing.continuation_token(token);
        }
        let page = match listing.send().await {
            Ok(page) => page,
            Err(failure) => {
                ::tracing::debug!(
                    bucket,
                    prefix,
                    error = %::aws_smithy_types::error::display::DisplayErrorContext(&failure),
                    "the content location of an application could not be listed"
                );
                return answered_about_the_location(&failure);
            }
        };
        for object in page.contents() {
            counted = counted.saturating_add(1);
            if counted > MAX_CONTENT_OBJECTS {
                ::tracing::debug!(
                    bucket,
                    prefix,
                    "the content location of an application holds more objects than this \
                     read sizes"
                );
                return SourceContent::Stored(total);
            }
            // Saturating: the total is reported as a `Long`, so no bucket's contents
            // can make the arithmetic overflow.
            total = total.saturating_add(object.size().unwrap_or(0).max(0));
            if probes.len() < MAX_ACCESS_PROBES {
                if let Some(key) = object.key() {
                    probes.push(key.to_string());
                }
            }
        }
        continuation = page.next_continuation_token().map(str::to_string);
        if continuation.is_none() {
            break;
        }
    }
    if counted == 0 {
        return SourceContent::Empty;
    }
    // The copy reads each object it found, so each one is asked for: a HEAD, because
    // what is asked is whether this service may read the object and not what is in it.
    // An object the listing named and that is no longer there is a store that changed
    // under the walk, which establishes nothing about the caller's request.
    for key in &probes {
        if let Err(failure) = client.head_object().bucket(bucket).key(key).send().await {
            if answered_about_the_location(&failure) == SourceContent::Denied {
                ::tracing::debug!(
                    bucket,
                    key,
                    "an object of an application's content location may not be read"
                );
                return SourceContent::Denied;
            }
        }
    }
    SourceContent::Stored(total)
}

/// What the object store's refusal `failure` established about the content location,
/// read through the tree's ONE reading of such an answer
/// ([`service_common::s3_object_read::answered_refusal`]) so this service and every
/// other one take an S3 answer to mean the same thing.
fn answered_about_the_location<E>(failure: &E) -> SourceContent
where
    E: ::aws_smithy_types::error::metadata::ProvideErrorMetadata + ::std::fmt::Debug,
{
    match ::service_common::s3_object_read::answered_refusal(failure) {
        ::service_common::s3_object_read::ObjectRead::Denied => SourceContent::Denied,
        ::service_common::s3_object_read::ObjectRead::Absent => SourceContent::Empty,
        // The store answered something else, or nothing answered at all: neither says
        // the location is missing and neither says it may not be read.
        _ => SourceContent::Unanswered,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: an `s3://bucket/key` URI names a bucket and the key prefix under it, and a
    /// value that is not such a URI names no location at all.
    #[test]
    fn a_content_uri_names_a_bucket_and_a_key_prefix() {
        let rows: &[(&str, Option<(&str, &str)>)] = &[
            ("s3://content/build/", Some(("content", "build/"))),
            ("s3://content/build", Some(("content", "build"))),
            ("s3://content/", Some(("content", ""))),
            ("s3://content", Some(("content", ""))),
            (
                "s3://content/deep/nested/path/",
                Some(("content", "deep/nested/path/")),
            ),
            ("s3://", None),
            ("s3:///build/", None),
            ("https://content/build/", None),
            ("content/build/", None),
            ("", None),
        ];
        for (uri, expected) in rows {
            let actual = source_location(uri);
            assert_eq!(
                actual, *expected,
                "case {uri:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the content location is reported as the prefix it names — exactly one
    /// trailing separator, whatever the request spelled.
    ///
    /// The rows differ along what the rule generalises over: whether the separator is
    /// already there, how deep the prefix is, whether the value names a bucket alone,
    /// and whether the key looks like a file rather than a folder.
    #[test]
    fn a_content_location_is_reported_as_one_prefix() {
        let rows: &[(&str, &str, &str)] = &[
            (
                "a prefix already ending in the separator",
                "s3://content/build/",
                "s3://content/build/",
            ),
            (
                "a prefix without the separator",
                "s3://content/build",
                "s3://content/build/",
            ),
            (
                "a multi-level prefix",
                "s3://content/games/alpha/build",
                "s3://content/games/alpha/build/",
            ),
            (
                "a bucket with a bare separator",
                "s3://content/",
                "s3://content/",
            ),
            ("a bucket alone", "s3://content", "s3://content/"),
            (
                "a key spelled like a file",
                "s3://content/build/game.zip",
                "s3://content/build/game.zip/",
            ),
        ];
        for (case, submitted, expected) in rows {
            let actual = normalized_source_uri(submitted);
            assert_eq!(
                actual, *expected,
                "case {case:?} ({submitted}): expected {expected:?}, got {actual:?}"
            );
            // Normalizing twice is normalizing once: the stored value is already the
            // one spelling, so a later write of it cannot grow another separator.
            let again = normalized_source_uri(&actual);
            assert_eq!(
                again, actual,
                "case {case:?}: normalizing the stored value again must not change it, \
                 got {again:?}"
            );
        }
    }
}
