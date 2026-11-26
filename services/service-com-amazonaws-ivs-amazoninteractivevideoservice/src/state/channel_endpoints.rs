// SPDX-License-Identifier: CC0-1.0

//! The two ADDRESSES every channel this service reports announces — the one an encoder
//! CONTRIBUTES to and the one a player FETCHES from — built here and nowhere else, so
//! every operation that projects a channel announces the same pair.
//!
//! RULE: a member this API fills with the address of a data plane announces an address a
//! client OUTSIDE this process can reach that plane at, because being DIALED is the only
//! thing such a member is for. For a plane that speaks its own wire protocol — the CONTRIBUTE
//! plane an encoder publishes to — that address is the authority the core bound this
//! deployment's socket at, the `host:port` the plane itself reports
//! ([`super::IngestPlane::authority`]): not a name this service minted, because nothing off
//! this process resolves one, and not a bare host either, because the port a client would
//! append to it (RTMPS' 443, RTMP's 1935) is not a port this emulator may bind — the half of
//! the authority the client cannot supply is exactly the half the member has to carry. A
//! deployment that could open no contribute plane announces NO contribute address, because
//! there is no address to announce. A member that carries a URL the client FETCHES exactly as
//! it stands keeps the scheme and the path layout the resource is laid out at, and its host
//! is a name of this emulator's own: the scheme already names the port such a URL is fetched
//! on.
//!
//! Neither member is modelled with a form (`Channel$ingestEndpoint`, `Channel$playbackUrl`
//! and `Srt$endpoint` are plain strings with no `@length` and no `@pattern`,
//! `smithy_query(query="shape",shape="Channel,Srt")`), and a client does not use either as
//! an opaque string: it points an encoder at `rtmps://<ingestEndpoint>/app/<streamKey>` —
//! appending `:443` only when the value it was handed names no port — and it fetches a
//! playback URL as it stands.
//!
//! What fills the form is this emulator's, because this emulator is the only thing serving
//! the channel:
//!
//! * `ingestEndpoint`, and the `srt.endpoint` that equals it, is "part of the definition
//!   of an ingest server, used when you set up streaming software" — the ACCOUNT's
//!   contribute server ([`announced_channel`] fills it from the plane's own authority):
//!   every channel this emulator serves reports the SAME address, so a client that cached
//!   the ingest server it was handed once configures the next channel's encoder correctly,
//!   and nothing of a channel's own identity is in it — which channel a publish reaches is
//!   carried by the stream key, exactly as on AWS. What ANSWERS there is this deployment's
//!   own contribute plane ([`super::ensure_ingest_plane`]), which speaks RTMP and RTMPS on
//!   that one socket of the core's, so a publisher anywhere on this machine — in this
//!   process or not — dials the value as it stands and is served. The `srt.endpoint` beside
//!   it names the same address because one socket serves the channel's ingest whatever wire
//!   reaches it — the value is the INGEST SERVER's address, which is what the recording
//!   holds for both members, and no SRT plane is claimed for it: this emulator answers RTMP
//!   and RTMPS there and nothing else, and no plane of this deployment is found at the port
//!   an SRT ingest URL names.
//! * `playbackUrl` is a URL a player FETCHES ("URL of the master playlist, required by the
//!   video player to play the HLS stream", `Stream$playbackUrl`), so it keeps the `https`
//!   scheme and the playlist path a channel's manifest is laid out at ([`playback_path`]:
//!   the Region, the account and the channel id, all three read off the channel's own ARN) —
//!   and its HOST is a name of this emulator's own ([`announced_playback_url`], composed by
//!   [`playback_host_in`]): a minted label, the Region the channel is in, the label this
//!   service publishes its playback edge under and this service's own domain, in the zone
//!   this emulator may announce in, with no port.
//!   What ANSWERS that URL is this deployment's own playback plane
//!   ([`super::playback_plane`]), claimed by the PATH the URL carries whatever host it was
//!   fetched at — the whole identity of the channel is in that path — and the announced name
//!   resolves to this service for a client inside this emulator that dials it
//!   ([`super::publish_playback_addresses`], the register every such client reads).
//!
//! The ZONE a name sits in is the one this emulator may announce a name of its own in
//! ([`::service_common::data_plane_addressing::announced_zone_of_authority`], the
//! workspace's one reading of that question), read off the address the request reached this
//! emulator at ([`reached_authority`]): the zone a resolver answers with the loopback
//! interface — the interface the core binds its listeners on — when that address is an
//! address rather than a name, and the name itself when it is one. It is never a zone AWS
//! serves its own names in: a name a service hands a client is a name the client RESOLVES
//! and then dials, so a name in AWS's zone would send the client out of the emulated
//! environment carrying the credentials it is holding.
//!
//! Both are therefore composed at PROJECTION time, per request ([`announced_channel`],
//! [`announced_stream`]), and neither is stored: a channel reached through one addressing
//! is never answered another addressing's, and a read composes the values without writing
//! anything.
//!
//! Nothing here is ever CONNECTED to. This module formats strings into a response and
//! holds no outbound client, socket or resolver; what it reports is a socket this process
//! bound and a name in this process's own zone, so no value it reports leads off this
//! machine.

use super::*;

/// The label this SERVICE's own names sit under, inside the zone this emulator may announce
/// in: the signing name of the service that serves the plane, which is how the other planes
/// of this tree name theirs (`rds.localhost`, `vpce.localhost`).
const SERVICE_DOMAIN_LABEL: &str = "ivs";

/// How many digest bytes a minted host label carries: six, which is the twelve
/// lowercase-hex characters an announced IVS host label is written with.
const HOST_LABEL_BYTES: usize = 6;

/// The domain label the IVS PLAYBACK edge is published under, between the Region label and
/// this service's own domain: a channel's playlists are served in the Region that channel
/// lives in, which is what the Region label in front of this one says.
const PLAYBACK_DOMAIN_LABEL: &str = "playback";

/// What the ACCOUNT's playback label is minted from, beside the account itself: the purpose,
/// so a label minted for one plane can never collide with a label minted for another from
/// the same subject. The playback edge is the account's, so the label is the account's, and
/// WHICH channel a playlist belongs to is carried by the path ([`playback_path`]) exactly as
/// the recorded URLs carry it.
const PLAYBACK_LABEL_PURPOSE: &str = "ivs-playback";

/// The port a playback URL is DIALED on, which is the port its `https` scheme names and
/// therefore the half of the authority the member itself does not carry.
///
/// Read by the record that resolves the announced playback name for a client inside this
/// emulator ([`super::publish_playback_addresses`]), so the name a player was handed and the
/// address that register answers for are one address.
pub(crate) const PLAYBACK_PORT: u16 = 443;

/// The path AWS serves a channel's HLS master playlist at, which is what a `playbackUrl`
/// carries after its host.
///
/// Read by the plane that SERVES that path too ([`super::playback_plane`]), so the address
/// this module announces and the path that module answers are one layout.
pub(crate) const PLAYBACK_PATH_PREFIX: &str = "/api/video/v1";

/// The extension of that playlist: an HLS master playlist.
const PLAYBACK_PATH_SUFFIX: &str = ".m3u8";

/// One channel as this service ANNOUNCES it to the request in `ctx`, contributing to the
/// plane open at `contribute`: the record it holds, with the contribute address the plane
/// really answers at and the playback URL composed for the addressing that request reached
/// this emulator through.
///
/// `contribute` is the `host:port` this deployment's contribute plane answers on
/// ([`super::contribute_authority`], read off the store the caller already holds), or `None`
/// for a deployment that could open none — and then the channel announces no contribute
/// address at all, for either member, because an address nothing answers on is the one thing
/// a member whose whole purpose is to be dialed may not carry.
///
/// Every operation that answers a `Channel` runs its record through this, so the single
/// read, the batch read, the create and the update all announce one pair of addresses. The
/// `srt` block keeps the passphrase the create generated and stored — the secret is the
/// channel's, while the endpoint beside it is the account's contribute server.
#[must_use]
pub fn announced_channel(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    contribute: Option<&str>,
    channel: Channel,
) -> Channel {
    let ingest_endpoint = contribute.map(str::to_string);
    let playback_url = channel
        .arn
        .as_deref()
        .and_then(|arn| announced_playback_url(ctx, arn));
    let srt = channel.srt.map(|srt| crate::ops::types::Srt {
        endpoint: ingest_endpoint.clone(),
        ..srt
    });
    Channel {
        ingest_endpoint,
        playback_url,
        srt,
        ..channel
    }
}

/// One live stream as this service announces it: "the stream is played back from its
/// channel's own URL", so the member is composed from the channel the stream is ingested
/// on, by the same rule [`announced_channel`] applies.
#[must_use]
pub fn announced_stream(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    stream: Stream,
) -> Stream {
    let playback_url = stream
        .channel_arn
        .as_deref()
        .and_then(|arn| announced_playback_url(ctx, arn));
    Stream {
        playback_url,
        ..stream
    }
}

/// The absolute URL a player fetches the channel `channel_arn` names at, or `None` when
/// that ARN names no channel — in which case there is no channel to compose a URL for and
/// the member is reported as absent rather than as a URL to nowhere.
///
/// AWS's own scheme and path layout, on a playback host of this emulator's own: the whole
/// identity of the channel is in the path ([`playback_path`]), which is what survives moving
/// the value off AWS's hostname and onto a name this emulator may announce
/// ([`playback_host_in`]).
#[must_use]
pub fn announced_playback_url(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    channel_arn: &str,
) -> Option<String> {
    let (region, account, channel_id) = channel_of_arn(channel_arn)?;
    Some(format!(
        "https://{}{}",
        playback_host_in(&announced_zone(ctx), region, account),
        playback_path(region, account, channel_id)
    ))
}

/// The host a channel of `region` in `account` has its playlists fetched at, in `zone`: a
/// hostname of this emulator's own — a minted label, the channel's Region, the label this
/// service publishes its playback edge under and this service's own domain — carrying no
/// port.
///
/// A playback URL is fetched exactly as it stands, so the port it is dialed on is the one its
/// `https` scheme names ([`PLAYBACK_PORT`]) and not one written into the value — a player
/// that parses the host to pick an edge, or asserts TLS by hostname, is handed a name either
/// way. What ANSWERS the URL is this deployment's own
/// playback plane, which claims the PATH the URL carries whatever host it was fetched at
/// ([`super::playback_plane`]), and the name itself resolves to this service for a client
/// inside this emulator that dials it ([`super::publish_playback_addresses`]).
///
/// The Region is the channel's own, read off its ARN like every other part of the value, and
/// drawn into the alphabet of a DNS label by the workspace's one reading of that question
/// (`::service_common::data_plane_addressing::dns_label`) — a label it empties is left out
/// altogether rather than announced as an empty one, so the host is a well-formed name for
/// every ARN this store can hold.
fn playback_host_in(zone: &str, region: &str, account: &str) -> String {
    let regional = match ::service_common::data_plane_addressing::dns_label(region) {
        label if label.is_empty() => String::new(),
        label => format!("{label}."),
    };
    format!(
        "{}.{regional}{PLAYBACK_DOMAIN_LABEL}.{SERVICE_DOMAIN_LABEL}.{zone}",
        minted_host_label(PLAYBACK_LABEL_PURPOSE, account)
    )
}

/// The playback host of a channel of `region` as a client ON THIS MACHINE resolves it: the
/// same name, in the zone a resolver answers with the interface the core binds its listeners
/// on.
///
/// The spelling the announced name is RECORDED under for an in-process client
/// ([`super::publish_playback_addresses`]), and the only one it can be: the records follow
/// the channels this store holds, written before any later request's zone is known, and an
/// in-process client is on this machine by construction. A caller whose own addressing puts this
/// emulator in another zone ([`announced_zone`]) is announced that zone and resolves it
/// itself.
///
/// The account is this deployment's own: every channel this emulator holds is minted in it.
#[must_use]
pub(crate) fn playback_host_on_this_machine(region: &str) -> String {
    playback_host_in(
        ::service_common::data_plane_addressing::LOOPBACK_ZONE,
        region,
        ::emulator_server::emulator_account_id(),
    )
}

/// One label of an announced name, minted for `purpose` from `seed`: the twelve
/// lowercase-hex characters an IVS host label is written with.
///
/// Derived rather than drawn from the store's mint counter, because the label has to be the
/// one value for its subject on every request that announces it while nothing about it is
/// persisted: the account's contribute and playback labels are the account's for the life of
/// the deployment. A separator the purpose and the subject cannot carry joins the two, so two
/// subjects cannot run together into one preimage.
fn minted_host_label(purpose: &str, seed: &str) -> String {
    let digest = Sha256::digest(format!("{purpose}:{seed}").as_bytes());
    ::hex::encode(&digest[..HOST_LABEL_BYTES])
}

/// The DNS zone this emulator announces a name of its own in, for the request in `ctx`.
///
/// The workspace's one reading of that question
/// ([`::service_common::data_plane_addressing::announced_zone_of_authority`]), applied to
/// the address this request reached this emulator at: the loopback zone — which a resolver
/// answers with the interface the core binds its listeners on — for a request that reached
/// an address, the reached name itself for a request that reached one of this emulator's
/// own names, and never a zone AWS publishes its own endpoints in.
fn announced_zone(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    ::service_common::data_plane_addressing::announced_zone_of_authority(&reached_authority(ctx))
}

/// The `host[:port]` this request reached this emulator at, which is the address the zone
/// every announced name sits in is read off ([`announced_zone`]) — every name this module
/// composes, the host of the playback URL included.
///
/// In the order the other data planes of this tree read it:
///
/// * a request that addressed the EMULATOR'S OWN endpoint
///   (`service_common::data_plane_addressing::reached_emulator_endpoint_of_parts`) comes
///   from a client that resolves nothing else, so that is the address it reached;
/// * a request on an emulator that IS served on a port
///   (`service_common::data_plane_addressing::served_endpoint_authority`) — a caller that
///   addressed one of AWS's own hosts, including an in-process call of another service —
///   is read off the one address this process answers on;
/// * an emulator served on no port at all has one base and that base is read, so the
///   value still names this emulator rather than a host belonging to someone else.
fn reached_authority(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    let base = ctx.endpoint();
    ::service_common::data_plane_addressing::reached_emulator_endpoint_of_parts(ctx.parts())
        .or_else(|| {
            ::service_common::data_plane_addressing::served_endpoint_authority(base.as_str())
        })
        .unwrap_or_else(|| {
            ::service_common::data_plane_addressing::url_authority(base.as_str())
                .trim_end_matches('/')
                .to_string()
        })
}

/// The path a channel's master playlist is served at:
/// `/api/video/v1/<region>.<account>.channel.<channel-id>.m3u8`, the layout AWS's own
/// playback URLs carry — the whole identity of the channel, which is why it survives
/// moving the value off AWS's hostname and onto a name of this emulator's.
fn playback_path(region: &str, account: &str, channel_id: &str) -> String {
    format!(
        "{PLAYBACK_PATH_PREFIX}/{region}.{account}.channel.{channel_id}{PLAYBACK_PATH_SUFFIX}"
    )
}

/// The Region, the account and the channel id a `ChannelArn` names
/// (`arn:aws:ivs:<region>:<account>:channel/<id>`, the form the modelled `ChannelArn`
/// pattern pins), or `None` for a value that is not one.
///
/// The one reader of that form: the plane that serves a channel's playback path resolves
/// the channel a path names by comparing the identity in the path with the identity each
/// stored ARN carries ([`super::playback_plane::playback_channel_arn`]).
pub(crate) fn channel_of_arn(arn: &str) -> Option<(&str, &str, &str)> {
    let segments: Vec<&str> = arn.splitn(6, ':').collect();
    let [_, _, _, region, account, resource] = segments[..] else {
        return None;
    };
    let id = resource.strip_prefix("channel/")?;
    (!region.is_empty() && !account.is_empty() && !id.is_empty())
        .then_some((region, account, id))
}
