// SPDX-License-Identifier: CC0-1.0

//! WHERE this service's two data planes are ANNOUNCED: the origin every URL the control
//! plane publishes for a caller to dial later is rooted at — a channel's HLS ingest URLs
//! (`CreateChannel`) and an origin endpoint's playback URLs (`CreateOriginEndpoint`).
//!
//! `IngestEndpoint$Url` is "The ingest URL to which the source stream should be sent" and
//! `OriginEndpoint$Url` "The URL of the packaged OriginEndpoint for consumption"
//! (`smithy_query(query="shape",shape="com.amazonaws.mediapackage#IngestEndpoint",model_path="services/service-com-amazonaws-mediapackage-mediapackage/smithy-model/com-amazonaws-mediapackage-mediapackage.json")`),
//! so each is an address a client DIALS afterwards — an encoder driven entirely by what the
//! ingest address answers, a viewer by what the playback address answers
//! ([`super::data_plane`]). Both therefore have to name a host this emulator answers on.
//! AWS publishes both planes of an account beneath its own regional domain
//! ([`super::media_hostname`]), and a client on the machine this emulator runs on resolves
//! none of those names: announcing one sends that client off this process entirely, where
//! it is never challenged, never refused and never served. The address announced is
//! therefore the endpoint the creating request reached, in three cases:
//!
//! * a request that reached the EMULATOR'S OWN endpoint comes from a client configured
//!   with that one endpoint and resolving nothing else, so the resource is announced there;
//! * a request that addressed some other host on an emulator that IS served on a port is
//!   announced that port — the one address this process answers on — so a resource minted
//!   by an in-process call of another service publishes an address its caller can reach
//!   too;
//! * a request that addressed one of AWS's own hosts on an emulator served on no port at
//!   all comes from a client that resolves them, and there is no own address to announce,
//!   so the hostname AWS itself would publish stands.
//!
//! The choice is made ONCE, by the request that creates the resource, and persisted on that
//! resource's record: a channel's in the ingest endpoints stored on it, an origin
//! endpoint's in [`super::OriginEndpointRecord::playback_origin`]. Every projection of
//! either resource renders those stored values — the create answer, both `Describe*`, both
//! `List*`, both updates, `ConfigureLogs` and the two credential rotations
//! ([`super::ChannelRecord::to_channel`],
//! [`super::OriginEndpointRecord::to_origin_endpoint`]) — so a resource is announced at one
//! stable address however it is read.

use super::*;

/// The scheme-and-separator prefix an announced data-plane origin carries. Both planes are
/// TLS-only, so the prefix is always this one; it carries the `://` separator, so the path
/// below the origin ([`super::data_plane::ingest_path`],
/// [`super::data_plane::PLAYBACK_PATH_PREFIX`]) is appended and no scheme is re-added.
pub const MEDIA_ORIGIN_PREFIX: &str = "https://";

/// The origin the data-plane URLs of a resource created by `parts` are ANNOUNCED under —
/// the scheme and authority every one of that resource's published URLs is rooted at.
///
/// See the module docs for the three cases. `parts` is the request that creates the
/// resource and `served_base` the base URL
/// [`::smithy_server_runtime::OperationContext::endpoint`] reports for this emulator.
pub fn announced_media_origin(
    parts: &::http::request::Parts,
    served_base: &str,
    region: &str,
) -> String {
    let authority = reached_emulator_endpoint(parts)
        .or_else(|| served_endpoint_authority(served_base))
        .unwrap_or_else(|| media_hostname(region));
    format!("{MEDIA_ORIGIN_PREFIX}{authority}")
}

/// The EMULATOR'S OWN endpoint this request reached — an address or a loopback name, which
/// is what a client configured with one local endpoint URL sends — and `None` for a request
/// whose host names a service, which is a client that resolves AWS's own hosts.
///
/// The tree's one reading of that question, taken off the request parts a handler is given
/// (`service_common::data_plane_addressing::reached_emulator_endpoint_of_parts`), so this
/// service writes no second rule for which host is the emulator's own and repeats no
/// extraction of where a request carries it.
fn reached_emulator_endpoint(parts: &::http::request::Parts) -> Option<String> {
    ::service_common::data_plane_addressing::reached_emulator_endpoint_of_parts(parts)
}

/// The `host[:port]` this emulator is SERVED at, or `None` for one served on no port at
/// all — the tree's one reading of that question
/// (`service_common::data_plane_addressing::served_endpoint_authority`).
fn served_endpoint_authority(served_base: &str) -> Option<String> {
    ::service_common::data_plane_addressing::served_endpoint_authority(served_base)
}
