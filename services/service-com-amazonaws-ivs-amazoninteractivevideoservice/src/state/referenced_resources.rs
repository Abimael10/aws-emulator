// SPDX-License-Identifier: CC0-1.0

//! The resources of OTHER services an IVS create REFERENCES, resolved where those
//! resources live.
//!
//! Two create operations of this service take a reference to something IVS does not own:
//! `CreateAdConfiguration` names "the customer-created EMT PlaybackConfiguration
//! resource in the same region and account"
//! (`smithy_query(query="shape",shape="MediaTailorPlaybackConfiguration")`), and
//! `CreateRecordingConfiguration` names the "Location (S3 bucket name) where recorded
//! videos will be stored" (`shape="S3DestinationConfiguration"`). A reference that
//! resolves to nothing is a create AWS refuses, so each one is ASKED OF THE SERVICE THAT
//! OWNS the resource before anything is minted — an emulated service reaches another the
//! way any caller does, with the AWS SDK driven over the in-process callback transport
//! (`OperationContext::callback`), so no socket is opened and nothing outside this
//! process is reachable from here.
//!
//! Both readings are deliberately one-sided: they report `true` — the reference resolves
//! to nothing — ONLY for an answer in which the owning service itself said so. Every
//! other outcome (a hop that failed, an answer this emulator could not read, an emulator
//! with no object store or no MediaTailor mounted at all) establishes nothing, so a
//! create is never refused on the strength of a question nobody answered.

use ::smithy_server_runtime::EmbeddedEmulator;

/// The vendor field a MediaTailor ARN carries, from the modelled
/// `MediaTailorPlaybackConfigurationArn` pattern
/// `^arn:aws:mediatailor:[a-z0-9-]+:[0-9]+:playbackConfiguration/[a-zA-Z0-9-]+$`.
const MEDIA_TAILOR_VENDOR: &str = "mediatailor";

/// The resource type that same pattern pins, and so the type whose id is the playback
/// configuration's `Name` — the identity MediaTailor's own API addresses one by.
const PLAYBACK_CONFIGURATION_RESOURCE_TYPE: &str = "playbackConfiguration";

/// How this service NAMES that kind of resource in the not-found it answers for a
/// reference that resolves to nothing (see
/// [`super::referenced_resource_not_found_message`]).
pub const MEDIA_TAILOR_PLAYBACK_CONFIGURATION_KIND: &str = "mediaTailorPlaybackConfiguration";

/// The error code MediaTailor answers a read of a playback configuration it does not
/// hold with (HTTP 404 `NotFoundException`). Read as the ANSWER "there is no such
/// resource": a failure carrying any other code — the router's own answer for a
/// MediaTailor that is not mounted among them — is not that answer.
const PLAYBACK_CONFIGURATION_NOT_FOUND_CODE: &str = "NotFoundException";

/// The playback configuration `arn` names: its `Name` and the Region to ask in, or
/// `None` when the value references no MediaTailor playback configuration at all and so
/// leaves nothing to resolve.
///
/// The ARN is read through the tree's one caller-supplied-ARN parse
/// (`service_common::arn`), which is also what holds the Region field to a Region LABEL —
/// it addresses the hop, so a value that could re-point an endpoint's authority is
/// reported as no Region and the caller's own Region is used instead.
#[must_use]
pub fn media_tailor_playback_configuration_reference<'a>(
    arn: &'a str,
    fallback_region: &'a str,
) -> Option<(&'a str, &'a str)> {
    let parsed = ::service_common::arn::parse_of(arn, MEDIA_TAILOR_VENDOR)?;
    let name = ::service_common::arn::resource_id_of_type(arn, PLAYBACK_CONFIGURATION_RESOURCE_TYPE)
        .filter(|name| !name.is_empty())?;
    let region = ::service_common::arn::region_label(parsed.region).unwrap_or(fallback_region);
    Some((name, region))
}

/// Whether MediaTailor ANSWERED that it holds no playback configuration named `name` in
/// `region`.
///
/// Asked with `GetPlaybackConfiguration`, the operation MediaTailor models for reading
/// one by name. `false` for every answer that is not MediaTailor reporting it absent,
/// for the reason the module docs give.
pub async fn media_tailor_playback_configuration_is_absent(
    callback: &EmbeddedEmulator,
    region: &str,
    name: &str,
) -> bool {
    use ::aws_sdk_mediatailor::error::ProvideErrorMetadata;

    let answer = media_tailor_client(callback, region)
        .get_playback_configuration()
        .name(name)
        .send()
        .await;
    match answer {
        Ok(_) => false,
        Err(error) => error.code() == Some(PLAYBACK_CONFIGURATION_NOT_FOUND_CODE),
    }
}

/// Whether the object store ANSWERED that this account holds no bucket named `bucket` in
/// `region`.
///
/// Asked with `ListBuckets` narrowed by the bucket's own name as the `prefix` member
/// ("All bucket names in the response begin with the specified bucket name prefix"), so
/// the question is answered by a SUCCESSFUL listing in which the bucket is not present
/// rather than by reading a failure: a 404 says "absent" only if it came from the object
/// store at all, and a listing says which buckets there are whoever answered it. `false`
/// for every other outcome, for the reason the module docs give.
pub async fn s3_bucket_is_absent(
    callback: &EmbeddedEmulator,
    region: &str,
    bucket: &str,
) -> bool {
    let answer = ::service_common::s3_client::s3_client(callback, region)
        .list_buckets()
        .prefix(bucket)
        .send()
        .await;
    match answer {
        Ok(listed) => !listed
            .buckets()
            .iter()
            .any(|held| held.name() == Some(bucket)),
        Err(_) => false,
    }
}

/// The AWS Elemental MediaTailor client this service asks that question through,
/// addressed to `region`.
///
/// Built the way every in-process hop in this tree is built
/// (`service_common::s3_client` states the same recipe for the object store): the
/// emulator's own callback transport and credentials, with the SDK's retries, timeouts,
/// identity cache and stalled-stream watchdog off — each of those needs a sleep
/// implementation this client is not given — and an endpoint URL that is addressing for
/// that transport rather than a host this process connects to. The caller's own
/// `Authorization` header is never forwarded.
fn media_tailor_client(callback: &EmbeddedEmulator, region: &str) -> ::aws_sdk_mediatailor::Client {
    let config = ::aws_sdk_mediatailor::Config::builder()
        .behavior_version(::aws_sdk_mediatailor::config::BehaviorVersion::latest())
        .http_client(callback.http_client())
        .credentials_provider(callback.aws_credentials_provider())
        .region(::aws_sdk_mediatailor::config::Region::new(
            region.to_string(),
        ))
        .endpoint_url(format!("https://api.mediatailor.{region}.amazonaws.com"))
        .retry_config(::aws_sdk_mediatailor::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_mediatailor::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_mediatailor::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_mediatailor::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_mediatailor::Client::from_conf(config)
}
