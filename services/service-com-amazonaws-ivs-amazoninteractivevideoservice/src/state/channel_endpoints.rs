// SPDX-License-Identifier: CC0-1.0

//! The two ADDRESSES every channel this service reports announces — the one an encoder
//! CONTRIBUTES to and the one a player FETCHES from — built here and nowhere else, so
//! every operation that projects a channel announces the same pair.
//!
//! RULE: a member this API fills with a DNS NAME is announced as a DNS name — a minted
//! label, the domain labels AWS publishes that plane under, and a zone this emulator may
//! announce a name of its own in — carrying no scheme, no port and no path; a member that
//! carries a URL the client FETCHES keeps AWS's scheme and the path layout AWS lays the
//! resource out at, and is rooted on the `host[:port]` this request reached this emulator
//! at, because a URL is dialed exactly as it stands.
//!
//! Neither member is modelled with a form (`Channel$ingestEndpoint`, `Channel$playbackUrl`
//! and `Srt$endpoint` are plain strings with no `@length` and no `@pattern`,
//! `smithy_query(query="shape",shape="Channel,Srt")`), and a client does PARSE what it is
//! handed: it appends its own port to the contribute address
//! (`rtmps://<ingestEndpoint>:443/app/<streamKey>`, `srt://<srt.endpoint>:<port>`), splits
//! the host on `.`, or stores the value in a hostname-typed field — all of which an address
//! literal carrying an explicit port breaks. So the contribute address is a NAME, while the
//! playback URL, which a player dials as given, keeps the address it was reached at.
//!
//! What fills the template is this emulator's, because this emulator is the only thing
//! serving the channel:
//!
//! * `ingestEndpoint`, and the `srt.endpoint` that equals it, is "part of the definition
//!   of an ingest server, used when you set up streaming software" — the ACCOUNT's
//!   contribute server, under the one contribute domain that plane is published in and not
//!   a per-channel name ([`announced_ingest_endpoint`]): every channel this emulator serves
//!   reports the SAME value, so a client that cached the ingest server it was handed once
//!   configures the next channel's encoder correctly, and nothing of a channel's own
//!   identity is in it.
//! * `playbackUrl` is a URL a player FETCHES ("URL of the master playlist, required by the
//!   video player to play the HLS stream", `Stream$playbackUrl`), so it keeps AWS's `https`
//!   scheme and the playlist path AWS lays a channel's manifest out at ([`playback_path`]:
//!   the Region, the account and the channel id, all three read off the channel's own ARN)
//!   on the address the player can fetch it from ([`announced_authority`]) — an address
//!   written as the NAME that resolves to it wherever one does, and never a name belonging
//!   to someone else.
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
//! holds no outbound client, socket or resolver; every value it composes is one of this
//! process's own inbound addresses or a name in its own zone, so no value it reports leads
//! off this machine.

use super::*;

/// The domain label the IVS CONTRIBUTE plane is published under, between the minted label
/// and the zone: the ingest server is published once for the whole account rather than per
/// Region, which is what the label says.
const CONTRIBUTE_DOMAIN_LABEL: &str = "global-contribute";

/// How many digest bytes a minted host label carries: six, which is the twelve
/// lowercase-hex characters an announced IVS host label is written with.
const HOST_LABEL_BYTES: usize = 6;

/// What the ACCOUNT's contribute label is minted from, beside the account itself: the
/// purpose, so a label minted for one plane can never collide with a label minted for
/// another from the same subject.
const CONTRIBUTE_LABEL_PURPOSE: &str = "ivs-contribute";

/// The path AWS serves a channel's HLS master playlist at, which is what a `playbackUrl`
/// carries after its host.
const PLAYBACK_PATH_PREFIX: &str = "/api/video/v1";

/// The extension of that playlist: an HLS master playlist.
const PLAYBACK_PATH_SUFFIX: &str = ".m3u8";

/// One channel as this service ANNOUNCES it to the request in `ctx`: the record it holds,
/// with both addresses composed for the addressing that request reached this emulator
/// through.
///
/// Every operation that answers a `Channel` runs its record through this, so the single
/// read, the batch read, the create and the update all announce one pair of addresses. The
/// `srt` block keeps the passphrase the create generated and stored — the secret is the
/// channel's, while the endpoint beside it is the account's contribute server.
#[must_use]
pub fn announced_channel(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    channel: Channel,
) -> Channel {
    let ingest_endpoint = announced_ingest_endpoint(ctx);
    let playback_url = channel
        .arn
        .as_deref()
        .and_then(|arn| announced_playback_url(ctx, arn));
    let srt = channel.srt.map(|srt| crate::ops::types::Srt {
        endpoint: Some(ingest_endpoint.clone()),
        ..srt
    });
    Channel {
        ingest_endpoint: Some(ingest_endpoint),
        playback_url,
        srt,
        ..channel
    }
}

/// The ingest server every channel of this account contributes to:
/// `<minted label>.global-contribute.<zone>`, a NAME carrying no scheme and no port,
/// because a client appends its own protocol and port to it.
///
/// It is the ACCOUNT's and not a channel's — two channels report the same value, which is
/// what lets one encoder configuration serve the next channel — so the label is minted
/// from the account the channels belong to and nothing else, and the plane is global rather
/// than regional, so a caller signing for another Region is handed the one contribute
/// server this deployment has.
#[must_use]
pub fn announced_ingest_endpoint(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    format!(
        "{}.{CONTRIBUTE_DOMAIN_LABEL}.{}",
        minted_host_label(
            CONTRIBUTE_LABEL_PURPOSE,
            ::emulator_server::emulator_account_id()
        ),
        announced_zone(ctx)
    )
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
/// AWS's own scheme and path layout on the address this caller can fetch the playlist
/// from: the whole identity of the channel is in the path ([`playback_path`]), which is
/// what survives moving the value off AWS's hostname and onto this emulator's address.
#[must_use]
pub fn announced_playback_url(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    channel_arn: &str,
) -> Option<String> {
    let (region, account, channel_id) = channel_of_arn(channel_arn)?;
    Some(format!(
        "https://{}{}",
        announced_authority(ctx),
        playback_path(region, account, channel_id)
    ))
}

/// The `host[:port]` a URL this service announces is rooted on: the address this request
/// reached this emulator at ([`reached_authority`]), with a host that names the LOOPBACK
/// interface written as the name of the zone that interface resolves in
/// (`service_common::data_plane_addressing::LOOPBACK_ZONE`), port kept.
///
/// A URL is dialed exactly as it stands, so the value has to carry both halves of the
/// address the player reaches this process at — the port included, which is the one thing a
/// name alone cannot carry. Spelling a loopback address as the loopback NAME changes
/// nothing about where it leads (RFC 6761 reserves that name for this interface) and makes
/// the host of the URL a hostname rather than an address literal, which is what a player
/// that parses the host, or asserts TLS by hostname, expects. Any other host is answered as
/// it stands: a request that reached this emulator at a routable address or at a name of
/// its own is told that address, because a client off this machine cannot resolve one of
/// its own interfaces.
fn announced_authority(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    let reached = reached_authority(ctx);
    if !::service_common::data_plane_addressing::names_the_loopback_interface(&reached) {
        return reached;
    }
    let zone = ::service_common::data_plane_addressing::LOOPBACK_ZONE;
    match ::service_common::data_plane_addressing::addressed_port(Some(&reached))
        .and_then(|port| u16::try_from(port).ok())
    {
        Some(port) => ::service_common::data_plane_addressing::host_port_authority(zone, port),
        None => zone.to_string(),
    }
}

/// One label of an announced name, minted for `purpose` from `seed`: the twelve
/// lowercase-hex characters an IVS host label is written with.
///
/// Derived rather than drawn from the store's mint counter, because the label has to be the
/// one value for its subject on every request that announces it while nothing about it is
/// persisted: the account's contribute label is the account's for the life of the
/// deployment. A separator the purpose and the subject cannot carry joins the two, so two
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

/// The `host[:port]` this request reached this emulator at, which is the address every
/// announced value is built on.
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
/// moving the value off AWS's hostname and onto this emulator's address.
fn playback_path(region: &str, account: &str, channel_id: &str) -> String {
    format!(
        "{PLAYBACK_PATH_PREFIX}/{region}.{account}.channel.{channel_id}{PLAYBACK_PATH_SUFFIX}"
    )
}

/// The Region, the account and the channel id a `ChannelArn` names
/// (`arn:aws:ivs:<region>:<account>:channel/<id>`, the form the modelled `ChannelArn`
/// pattern pins), or `None` for a value that is not one.
fn channel_of_arn(arn: &str) -> Option<(&str, &str, &str)> {
    let segments: Vec<&str> = arn.splitn(6, ':').collect();
    let [_, _, _, region, account, resource] = segments[..] else {
        return None;
    };
    let id = resource.strip_prefix("channel/")?;
    (!region.is_empty() && !account.is_empty() && !id.is_empty())
        .then_some((region, account, id))
}
