// SPDX-License-Identifier: CC0-1.0

//! The two ADDRESSES every channel this service reports announces — the one an encoder
//! CONTRIBUTES to and the one a player FETCHES from — built here and nowhere else, so
//! every operation that projects a channel announces the same pair.
//!
//! RULE: a member this API fills with an address announces a HOST of ONE class, whatever the
//! member's own form: a DNS hostname made of a minted label and the labels this service
//! publishes that plane under, in the zone this emulator may announce a name of its own in,
//! carrying no port. A member that carries the host ALONE is that name and nothing else — no
//! scheme, no path and no port, because the client composes the authority itself, appending
//! the port its own protocol dials, and a value that already carried one would compose into a
//! doubly-ported address no client can dial. A member that carries a URL the client FETCHES
//! exactly as it stands keeps the scheme and the path layout the resource is laid out at, and
//! its HOST is a name of that SAME class: the scheme already names the port such a URL is
//! fetched on, so a port in the value is a half of the address the member does not carry —
//! which is what the recording holds for all three of this service's address members.
//!
//! Neither member is modelled with a form (`Channel$ingestEndpoint`, `Channel$playbackUrl`
//! and `Srt$endpoint` are plain strings with no `@length` and no `@pattern`,
//! `smithy_query(query="shape",shape="Channel,Srt")`), and a client does not use either as
//! an opaque string: it points an encoder at `rtmps://<ingestEndpoint>:443/app/<streamKey>`
//! or at `srt://<srt.endpoint>:<port>` and it parses the host of a playback URL to assert
//! TLS by hostname. An address LITERAL is therefore the one thing neither member may be.
//!
//! What fills the form is this emulator's, because this emulator is the only thing serving
//! the channel:
//!
//! * `ingestEndpoint`, and the `srt.endpoint` that equals it, is "part of the definition
//!   of an ingest server, used when you set up streaming software" — the ACCOUNT's
//!   contribute server ([`announced_ingest_endpoint`]): every channel this emulator serves
//!   reports the SAME name, so a client that cached the ingest server it was handed once
//!   configures the next channel's encoder correctly, and nothing of a channel's own
//!   identity is in it — which channel a publish reaches is carried by the stream key,
//!   exactly as on AWS. The address a client COMPOSES from it is served by this
//!   deployment's own contribute plane ([`super::ensure_ingest_plane`], which answers RTMP
//!   and RTMPS on a socket of the core's and is reached in process at the name announced
//!   here on each port a contribution is dialed on, [`CONTRIBUTE_PORTS`]). A publisher that
//!   runs OUT of this process reaches the plane at the socket itself: the core reports the
//!   `host:port` it bound, the deployment states it when the plane opens
//!   ([`super::ensure_ingest_plane`]'s log line) and it is the address the plane's own record
//!   is held under, because a client outside this process cannot be handed a connection
//!   inside it whatever name it is told. The wires that plane speaks are RTMP and RTMPS, so
//!   those are the ports it is reached at; the `srt.endpoint` beside it names the same host
//!   because the recording holds one host for both, and no SRT port is claimed for it
//!   ([`CONTRIBUTE_PORTS`]).
//! * `playbackUrl` is a URL a player FETCHES ("URL of the master playlist, required by the
//!   video player to play the HLS stream", `Stream$playbackUrl`), so it keeps the `https`
//!   scheme and the playlist path a channel's manifest is laid out at ([`playback_path`]:
//!   the Region, the account and the channel id, all three read off the channel's own ARN) —
//!   and its HOST is a name of the same class as the contribute host beside it
//!   ([`announced_playback_url`], composed by [`playback_host_in`]): a minted label, the
//!   Region the channel is in, the label this service publishes its playback edge under and
//!   this service's own domain, in the zone this emulator may announce in, with no port.
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
//! holds no outbound client, socket or resolver; every value it composes is a name in this
//! process's own zone, so no value it reports leads off this machine.

use super::*;

/// The domain label the IVS CONTRIBUTE plane is published under, between the minted label
/// and this service's own domain: the ingest server is published once for the whole account
/// rather than per Region, which is what the label says.
const CONTRIBUTE_DOMAIN_LABEL: &str = "global-contribute";

/// The label this SERVICE's own names sit under, inside the zone this emulator may announce
/// in: the signing name of the service that serves the plane, which is how the other planes
/// of this tree name theirs (`rds.localhost`, `vpce.localhost`).
const SERVICE_DOMAIN_LABEL: &str = "ivs";

/// How many digest bytes a minted host label carries: six, which is the twelve
/// lowercase-hex characters an announced IVS host label is written with.
const HOST_LABEL_BYTES: usize = 6;

/// What the ACCOUNT's contribute label is minted from, beside the account itself: the
/// purpose, so a label minted for one plane can never collide with a label minted for
/// another from the same subject.
const CONTRIBUTE_LABEL_PURPOSE: &str = "ivs-contribute";

/// The domain label the IVS PLAYBACK edge is published under, between the Region label and
/// this service's own domain: a channel's playlists are served in the Region that channel
/// lives in, which is what the Region label in front of this one says.
const PLAYBACK_DOMAIN_LABEL: &str = "playback";

/// What the ACCOUNT's playback label is minted from, beside the account itself, by the same
/// rule as [`CONTRIBUTE_LABEL_PURPOSE`]: the playback edge is the account's, so the label is
/// the account's, and WHICH channel a playlist belongs to is carried by the path
/// ([`playback_path`]) exactly as the recorded URLs carry it.
const PLAYBACK_LABEL_PURPOSE: &str = "ivs-playback";

/// The port a playback URL is DIALED on, which is the port its `https` scheme names and
/// therefore the half of the authority the member itself does not carry.
///
/// Read by the record that resolves the announced playback name for a client inside this
/// emulator ([`super::publish_playback_addresses`]), so the name a player was handed and the
/// address that register answers for are one address.
pub(crate) const PLAYBACK_PORT: u16 = 443;

/// The ports a contribution to the announced contribute host is DIALED on — the port a
/// client appends to the host this service announces, which is the half of the authority
/// the member itself does not carry: RTMPS on 443 and RTMP on 1935, the two an encoder
/// configured with a channel's ingest endpoint and stream key opens
/// (`rtmps://<ingestEndpoint>:443/app/<streamKey>`).
///
/// Those two and no others, because they are the wires this deployment's contribute plane
/// really speaks ([`super::ingest_plane::rtmp_wire`]). The `srt.endpoint` the channel
/// announces beside them is the SAME host — that is what the recording holds — but the port
/// an SRT ingest URL names is not recorded here: nothing of this emulator answers SRT, which
/// is a datagram protocol the core opens no listener for, so claiming an address for it
/// would be claiming a plane that does not exist.
///
/// Read by the plane that SERVES those addresses ([`super::ensure_ingest_plane`]), so the
/// address a client composes from the announced name is one this emulator answers on.
pub(crate) const CONTRIBUTE_PORTS: [u16; 2] = [443, 1935];

/// The path AWS serves a channel's HLS master playlist at, which is what a `playbackUrl`
/// carries after its host.
///
/// Read by the plane that SERVES that path too ([`super::playback_plane`]), so the address
/// this module announces and the path that module answers are one layout.
pub(crate) const PLAYBACK_PATH_PREFIX: &str = "/api/video/v1";

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

/// The ingest server every channel of this account contributes to: a hostname of this
/// emulator's own, under the labels the contribute plane is published in, carrying no port
/// — the client appends the one its protocol dials ([`CONTRIBUTE_PORTS`]).
///
/// It is the ACCOUNT's and not a channel's — two channels report the same value, which is
/// what lets one encoder configuration serve the next channel — so nothing of a channel's
/// identity is in it, and the plane is global rather than regional, so a caller signing for
/// another Region is handed the one contribute server this deployment has.
#[must_use]
pub fn announced_ingest_endpoint(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    contribute_host_in(&announced_zone(ctx))
}

/// The contribute host as a client ON THIS MACHINE resolves it: the same name, in the zone
/// a resolver answers with the interface the core binds its listeners on.
///
/// The spelling the contribute plane records its in-process addresses under
/// ([`super::ensure_ingest_plane`]), and the only one it can: the plane is opened once for
/// the deployment, before any particular request's zone is known, and an in-process client
/// is on this machine by construction — the register serves a connection with a pipe inside
/// this process ([`::service_common::dialable_data_planes`]). A caller whose own addressing
/// puts this emulator in ANOTHER zone ([`announced_zone`]) reached it at a name of its own,
/// so it resolves that zone and not this one, and a client off this machine could not be
/// served a pipe in this process whatever name it was told: for such a caller the address it
/// composes is served by the plane's own socket, at the port the core bound.
#[must_use]
pub(crate) fn contribute_host_on_this_machine() -> String {
    contribute_host_in(::service_common::data_plane_addressing::LOOPBACK_ZONE)
}

/// The account's contribute host in `zone`.
fn contribute_host_in(zone: &str) -> String {
    format!(
        "{}.{CONTRIBUTE_DOMAIN_LABEL}.{SERVICE_DOMAIN_LABEL}.{zone}",
        minted_host_label(
            CONTRIBUTE_LABEL_PURPOSE,
            ::emulator_server::emulator_account_id()
        )
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
/// hostname of the SAME class as the contribute host beside it — a minted label, the
/// channel's Region, the label this service publishes its playback edge under and this
/// service's own domain — carrying no port.
///
/// A playback URL is fetched exactly as it stands, so the port it is dialed on is the one its
/// `https` scheme names ([`PLAYBACK_PORT`]) and not one written into the value: the recording
/// holds one hostname class for every address member of this API, and a host carrying an
/// explicit port is not of it — a player that parses the host to pick an edge, or asserts TLS
/// by hostname, is handed a name either way. What ANSWERS the URL is this deployment's own
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
/// The account is this deployment's own, as the label the contribute host carries is
/// ([`contribute_host_on_this_machine`]): every channel this emulator holds is minted in it.
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
