// SPDX-License-Identifier: CC0-1.0

//! The ONE construction of the in-process Amazon S3 client an emulated service WRITES an
//! object through, so no two services configure that hop differently — the sibling of
//! [`crate::eventbridge_client`] and [`crate::kinesis_client`] for the account's object
//! store.
//!
//! A service that delivers something into a bucket a client named — the log files of a
//! trail that is logging ([`crate::trail_log_files`]) among them — reaches Amazon S3 the way
//! any emulated service reaches another: the AWS SDK driven over the emulator's own
//! in-process transport, through the operation callback
//! ([`smithy_server_runtime::OperationContext::callback`]). The transport is what makes the
//! write in-process: no socket is opened, no name is resolved, and nothing outside this
//! process is reachable from it. The endpoint URL below is addressing for that transport,
//! not a host this process connects to, and it is built from the Region alone — no part of a
//! caller's request is a destination.
//!
//! The copies that predate this module — one per service that writes to a bucket — are to be
//! MIGRATED onto it, each in the change that owns its service. Nothing new should build the
//! client by hand.
//!
//! Beside the client, this module holds the one thing a service POINTED AT a bucket needs
//! before it stores that destination: whether Amazon S3 holds the bucket at all
//! ([`bucket_is_absent`]).

use smithy_server_runtime::EmbeddedEmulator;

/// The Amazon S3 client a service writes an object through, addressed to `region`.
///
/// Path-style addressing is what the emulated object store routes on (its operations are
/// bound under a `{Bucket}` path label). The transport is in-process, so there is nothing to
/// retry, time out or watch for a stalled stream — each of those would need a sleep
/// implementation this client is not given — and the credentials the callback hands over are
/// used directly rather than cached. Those credentials are the emulator's own: the caller's
/// `Authorization` header is never forwarded.
#[must_use]
pub fn s3_client(callback: &EmbeddedEmulator, region: &str) -> ::aws_sdk_s3::Client {
    client_signing_with(callback, region, callback.aws_credentials_provider())
}

/// Whether Amazon S3 ANSWERED that it holds no bucket under `name`.
///
/// A service POINTED at a bucket it is to deliver into has to resolve that bucket where
/// Amazon S3 holds it before it stores the destination, or it leaves an operator believing a
/// delivery exists where no object is ever written. The question is put with `HeadBucket`, the
/// operation Amazon S3 documents for exactly this ("You can use this operation to determine
/// if a bucket exists"), and the answer is read by
/// [`crate::s3_object_read::answered_refusal`], the one classification of what an object
/// store's refusal establishes about the address a request named.
///
/// `false` — establish nothing — for every answer that is NOT the store saying the bucket is
/// not there: an emulator assembled with no Amazon S3 to ask, and an answer that could not be
/// read. A refusal has to be grounded in what the owning service said, which is the rule
/// every cross-service pre-condition in this tree follows
/// (`services/common/src/network.rs` states which resolution is which).
///
/// The copies that predate this reading — one per service that resolves such a destination
/// (`service-com-amazonaws-s3files-s3files`'s `state::bucket_is_absent`,
/// `service-com-amazonaws-braket-braket`'s `state::task_results::output_bucket_is_absent`) —
/// are to be MIGRATED onto it, each in the change that owns its service.
///
/// Call it with NO state guard held — it awaits.
pub async fn bucket_is_absent(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    name: &str,
) -> bool {
    bucket_is_absent_over(ctx.callback(), region, name).await
}

/// [`bucket_is_absent`] for a caller that holds the in-process transport ITSELF rather than a
/// request's context — the `s3://` URI form ([`crate::s3_uri::bucket_is_absent`]) among them,
/// so the two cannot disagree about which answer means the bucket is not there.
pub async fn bucket_is_absent_over(
    callback: &EmbeddedEmulator,
    region: &str,
    name: &str,
) -> bool {
    match s3_client(callback, region)
        .head_bucket()
        .bucket(name)
        .send()
        .await
    {
        Ok(_) => false,
        Err(error) => {
            crate::s3_object_read::answered_refusal(&error)
                == crate::s3_object_read::ObjectRead::Absent
        }
    }
}

/// The same client, signing every call it makes as `credentials` rather than with the
/// emulator's own.
///
/// What a service uses when the read or write it makes is one ANOTHER principal makes
/// through it — a job run fetching its script as the job's execution role, say — so the
/// object store authorizes it against the policies in force on that principal and against
/// the bucket's own policy, exactly as it authorizes any other signed request. The
/// credentials are a session an emulated service minted through the token service; nothing
/// here constructs an access key id or a secret.
#[must_use]
pub fn s3_client_signed_as(
    callback: &EmbeddedEmulator,
    region: &str,
    credentials: ::aws_sdk_s3::config::Credentials,
) -> ::aws_sdk_s3::Client {
    client_signing_with(
        callback,
        region,
        ::aws_sdk_s3::config::SharedCredentialsProvider::new(credentials),
    )
}

/// The ONE builder both constructions above share, so the hop is configured one way whoever
/// the call is signed as.
fn client_signing_with(
    callback: &EmbeddedEmulator,
    region: &str,
    credentials: impl ::aws_sdk_s3::config::ProvideCredentials + 'static,
) -> ::aws_sdk_s3::Client {
    let config = ::aws_sdk_s3::Config::builder()
        .behavior_version(::aws_sdk_s3::config::BehaviorVersion::latest())
        .http_client(callback.http_client())
        .credentials_provider(credentials)
        .region(::aws_sdk_s3::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://s3.{region}.amazonaws.com"))
        .force_path_style(true)
        // The object this hop writes carries the properties the SERVICE sets and no others.
        // Left at its default, the SDK computes a checksum of its own choosing for every
        // upload and declares it, and the object store records THAT algorithm as the one the
        // object was stored under — so a later read of the object reports a transport
        // decision rather than what the service asked for. Asking for a checksum only where
        // an operation requires one leaves the store to apply its own default to a write
        // that named none.
        .request_checksum_calculation(
            ::aws_sdk_s3::config::RequestChecksumCalculation::WhenRequired,
        )
        .retry_config(::aws_sdk_s3::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_s3::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_s3::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_s3::config::StalledStreamProtectionConfig::disabled())
        .build();
    ::aws_sdk_s3::Client::from_conf(config)
}
