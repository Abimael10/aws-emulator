// SPDX-License-Identifier: CC0-1.0

//! The CONTRIBUTE plane of this service: the socket an encoder publishes a channel's live
//! stream to, and what an accepted publish writes into this store.
//!
//! RULE: an endpoint a service announces for a wire protocol is an address this emulator
//! ANSWERS that protocol on, reachable by a client that runs OUTSIDE this process.
//! `Channel$ingestEndpoint` is "part of the definition of an
//! ingest server, used when you set up streaming software"
//! (`smithy_query(query="shape",shape="Channel")`) — a value whose whole purpose is to be
//! DIALED: a broadcaster points its encoder at `rtmps://<ingestEndpoint>/app/<streamKey>`
//! and expects the handshake to complete and the channel to go live. So this service opens a
//! socket of the EMULATOR's own through the core
//! ([`::emulator_server::Listeners::tcp_plaintext`], which owns the socket, the bind
//! interface, the port range, the TLS, the handshake deadline, the idle timeout and the
//! connection caps), and the `host:port` the CORE reports for that socket
//! ([`IngestPlane::authority`]) is exactly what the control plane announces
//! ([`super::announced_channel`]) — so the value a client is handed is the address it dials,
//! whether it runs in this process or holds a socket of its own. A connection an in-process
//! client opens to that address is served by the same handler
//! ([`::service_common::dialable_data_planes`], the tree's one register of the planes it has
//! opened).
//!
//! ONE socket serves the whole account, because AWS publishes one ingest server per account
//! rather than one per channel, and WHICH channel a connection publishes to is carried by
//! the stream key the encoder authenticates with — exactly as it is against AWS. That is
//! also why the socket is opened by `CreateChannel`: the call that creates the thing an
//! encoder contributes to is the call that has to be able to tell it where.
//!
//! Both the `rtmps://` an AWS client dials and the `rtmp://` a client that does not verify a
//! self-signed certificate dials are served on that one address: the connection's first byte
//! says which ([`::service_common::in_band_tls`], the tree's one reading of that
//! negotiation), a TLS record is handed to the core's own TLS server and everything after it
//! is read decrypted, and an RTMP handshake is read as it stands. The wire itself is
//! [`rtmp_wire`].
//!
//! What an accepted publish WRITES is the state the reads already derive from, and nothing
//! else: the channel's `LIVE` stream ([`super::start_stream`]), the session that broadcast
//! leaves behind ([`super::start_session`]) and the playlist its media is packaged into
//! ([`super::ChannelPlaylist`]). A publish presenting a stream key this store does not hold
//! is refused on the wire and writes nothing. When the publisher goes away — or
//! `StopStream` disconnects it, or `DeleteChannel` removes the channel — the broadcast is
//! unwound through the one exit every ended broadcast takes ([`super::end_stream`]).

use super::*;

/// The wire an encoder publishes over, served on the socket this module opens.
mod rtmp_wire;

/// The service that opened the socket, as the register of the planes this emulator serves
/// in process names it: this service's own SigV4 signing name, because the contribute plane
/// is the IVS data plane.
///
/// Read by the playback plane too ([`super::publish_playback_addresses`]), which records the
/// name a channel's playlists are fetched at under the same service: one deployment, one
/// signing name, whichever of its planes a client was handed an address for.
pub(super) const SIGNING_NAME: &str = "ivs";

/// The socket the contribute plane answers on, as the control plane publishes it.
///
/// Dropping it closes the port and severs every connection on it, so it is held on the
/// store for as long as this deployment serves channels.
pub struct IngestPlane {
    /// The `host:port` the core says the socket is dialable at, which is the address the
    /// control plane announces for this plane ([`super::announced_channel`]) — where a
    /// publisher anywhere on this machine opens its connection.
    authority: String,
    /// The socket itself. Never read: it is held because dropping it closes the port.
    _listener: ::emulator_server::ResourceListener,
    /// The record that resolves this plane for an in-process client opening a connection to
    /// the announced contribute address. Never read: it is held
    /// because dropping it withdraws those addresses, exactly as dropping the listener
    /// closes the port. `None` on an engine that vends no such register.
    _planes: Option<::service_common::dialable_data_planes::OpenedPlanes>,
}

/// Written by hand rather than derived: a listener handle carries no `Debug`, and what is
/// diagnosable about this record is where it answers.
impl std::fmt::Debug for IngestPlane {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IngestPlane")
            .field("authority", &self.authority)
            .finish_non_exhaustive()
    }
}

impl IngestPlane {
    /// The `host:port` of the SOCKET this plane answers on, which is both what the control
    /// plane announces and where every publisher dials it.
    #[must_use]
    pub fn authority(&self) -> &str {
        &self.authority
    }
}

/// The `host:port` this deployment's contribute plane answers on, as the control plane
/// ANNOUNCES it, or `None` when no plane is open — a deployment whose engine vends no
/// listener surface, or where the core could not bind, which has no contribute address to
/// announce at all.
///
/// Read off the store a caller already holds, which is where the plane records its socket
/// ([`ensure_ingest_plane`]): one deployment, one contribute plane, one address for every
/// channel of the account.
#[must_use]
pub fn contribute_authority(
    state: &ComAmazonawsIvsAmazoninteractivevideoserviceState,
) -> Option<&str> {
    state.ingest_plane.as_ref().map(IngestPlane::authority)
}

/// The `host:port` the contribute plane answers on, OPENING its socket on first use, or
/// `None` when this engine vends no listener surface or the core could not bind — in which
/// case there is no plane, and a channel announces no contribute address, which is the
/// honest report of a plane that could not be served.
///
/// Idempotent and it disturbs no connection: the first socket is kept, so every channel of
/// this deployment is served by one contribute plane and a broadcaster may cache the
/// announced value the way AWS's own documentation tells it to. The store guard is taken and
/// released around the bind rather than held across it, because a lock held across an await
/// would make this future non-`Send`.
pub async fn ensure_ingest_plane<Eng: Send + Sync + 'static>(
    engine: &Eng,
    state: &::engine::ServiceState<ComAmazonawsIvsAmazoninteractivevideoserviceState>,
) -> Option<String> {
    if let Some(open) = state.lock().ingest_plane.as_ref() {
        return Some(open.authority().to_string());
    }
    let shared = (engine as &dyn ::std::any::Any).downcast_ref::<::engine::InMemoryEngine>()?;
    let listeners = shared.listeners();
    let handler = ::std::sync::Arc::new(rtmp_wire::RtmpIngest {
        state: state.clone(),
        // The core's own TLS server, lent to this plaintext socket so an `rtmps://`
        // publisher's handshake is terminated by the core rather than here. An emulator
        // that could prepare none serves the publishers that dial in the clear alone.
        upgrader: listeners.tls_upgrader().ok(),
    });
    // ONE handler at both ends of this plane's address: the core's socket, and a connection
    // an in-process client opens to that same address. Byte 0 is the handler's on both,
    // because the encryption of a contribution is negotiated by the protocol itself rather
    // than by the transport.
    let served = ::service_common::dialable_data_planes::served_in_process(
        ::std::sync::Arc::clone(&handler),
        None,
    );
    let listening = ::service_common::dialable_data_planes::SharedHandler(handler);
    // Opened on a task of its OWN, so a runtime that cannot bind a socket at all — one
    // built with no IO driver, which is what a host driving this emulator in process may
    // hand a request — leaves this call without an address instead of taking the request
    // down with it.
    let opened = ::tokio::spawn(async move { listeners.tcp_plaintext(listening).await.ok() }).await;
    let listener = opened.ok().flatten()?;
    // The address the CORE says the listener is dialable at, read through the one shared
    // reading of it: a wildcard bind has no single address to dial and is published as
    // loopback, and an IPv6 bind keeps its brackets.
    let authority = ::service_common::data_plane_addressing::listener_authority(&listener);
    let mut store = state.lock();
    // Another call may have opened one while this one was binding; keep the first, so an
    // announced address never changes under a broadcaster. Dropping the loser closes its
    // own port, and it has recorded nothing.
    if let Some(open) = store.ingest_plane.as_ref() {
        return Some(open.authority().to_string());
    }
    // The address this plane is ANNOUNCED at, recorded so an in-process client handed it is
    // served by the very handler the socket's connections are. Recorded under this service's
    // own store guard, which is the order the register documents for a publisher.
    //
    // The register and this store are ONE engine's: the records are read back out of the
    // same shared engine this store lives in
    // (`::service_common::dialable_data_planes::DialablePlanes::of(engine)`), so another
    // engine in this process resolves none of them and shares no plane with this one, and
    // the guard taken above is that engine's single IVS store rather than one of several.
    let registered = ::service_common::dialable_data_planes::DialablePlanes::of(engine)
        .map(|planes| planes.opening(vec![authority.clone()], SIGNING_NAME, &served));
    if registered.is_none() {
        // An engine that vends no register leaves an in-process client to the socket itself,
        // which is a deployment fact worth being able to read off a log rather than an
        // absence that looks like a successful record.
        ::tracing::debug!(
            %authority,
            "this engine vends no register of open planes: the contribute address is \
             reachable at the socket alone"
        );
    }
    store.ingest_plane = Some(IngestPlane {
        authority: authority.clone(),
        _listener: listener,
        _planes: registered,
    });
    drop(store);
    ::tracing::info!(
        %authority,
        "opened the IVS contribute plane: channels announce this address as their ingest \
         server"
    );
    Some(authority)
}

/// What a publish accepted on the contribute plane is publishing.
#[derive(Debug, Clone)]
pub struct AcceptedPublish {
    /// The channel the stream key the publisher presented belongs to.
    pub channel_arn: String,
    /// The stream the broadcast was brought up as.
    pub stream_id: String,
}

/// Admit a publish presenting `stream_key_value` and bring the channel it belongs to live,
/// or `None` when no channel of this store mints that key — in which case nothing is
/// written and the publisher is refused on the wire.
///
/// This is the one thing that starts an IVS broadcast, exactly as on AWS: "a channel goes
/// live by publishing to the ingest endpoint", and no operation of the control plane does
/// it. What it writes is what the reads already derive from — the `LIVE` stream, the
/// session the broadcast leaves behind, and the playlist its media is packaged into — so
/// `GetStream`, `GetStreamSession` and the playback URL all report one broadcast.
///
/// A channel that is ALREADY broadcasting keeps the stream it has ([`super::start_stream`]):
/// IVS holds one stream per channel, so a second publisher of the same key joins the
/// broadcast that is running rather than minting a second stream id for it.
pub fn accept_publish(
    state: &mut ComAmazonawsIvsAmazoninteractivevideoserviceState,
    stream_key_value: &str,
) -> Option<AcceptedPublish> {
    let channel_arn = channel_of_stream_key(state, stream_key_value)?;
    if !channel_exists(state, &channel_arn) {
        return None;
    }
    let stream = start_stream(state, &channel_arn);
    let stream_id = stream.stream_id.clone()?;
    let started_at = stream
        .start_time
        .and_then(|start| SystemTime::try_from(start).ok())
        .unwrap_or_else(SystemTime::now);
    start_session(state, &channel_arn, &stream_id, started_at);
    state
        .playlists
        .entry(channel_arn.clone())
        .or_insert_with(|| ChannelPlaylist::new(&stream_id));
    Some(AcceptedPublish {
        channel_arn,
        stream_id,
    })
}

/// The channel a presented stream key belongs to, or `None` for a value no stream key of
/// this store carries.
///
/// The key is matched on its VALUE — "stream key used to authenticate an RTMPS stream for
/// ingestion" (`smithy_query(query="shape",shape="StreamKey")`) — because that is the only
/// thing the publisher presents, and the key names the channel it was minted for.
fn channel_of_stream_key(
    state: &ComAmazonawsIvsAmazoninteractivevideoserviceState,
    stream_key_value: &str,
) -> Option<String> {
    if stream_key_value.is_empty() {
        return None;
    }
    state
        .stream_keys
        .values()
        .find(|key| key.value.as_deref() == Some(stream_key_value))
        .and_then(|key| key.channel_arn.clone())
}

/// Record what the encoder publishing `publish` says it is sending, on the session that
/// broadcast left behind and on the playlist its media is packaged into.
pub fn publish_metadata(
    state: &mut ComAmazonawsIvsAmazoninteractivevideoserviceState,
    publish: &AcceptedPublish,
    metadata: &PublishedMetadata,
) {
    set_session_ingest_configuration(
        state,
        &publish.channel_arn,
        &publish.stream_id,
        ingest_configuration_of_metadata(metadata),
    );
    if let Some(playlist) = state.playlists.get_mut(&publish.channel_arn) {
        playlist.set_resolution(metadata.video_width, metadata.video_height);
    }
}

/// Package one ingested message into the broadcast's playlist, and answer whether that
/// broadcast is STILL the channel's live one and this publisher may keep sending.
///
/// `false` is a publisher the ingest server disconnects, which is what the caller then does
/// on the wire. Two reasons for it:
///
/// * the broadcast has ended underneath it — `StopStream` disconnects the incoming stream,
///   and `DeleteChannel` takes the channel with it;
/// * the message is LARGER than a segment of the live window may hold
///   ([`super::playback_plane::MAX_SEGMENT_BYTES`]), so nothing could package it. A
///   publisher sending one is sending media this plane cannot serve, and is disconnected
///   rather than left sending bytes nothing keeps.
///
/// What an accepted message costs this store is bounded by the playlist's own live window
/// and by nothing here: [`super::ChannelPlaylist::append`] holds at most
/// [`super::playback_plane::MAX_SEGMENT_BYTES`] per segment and drops the oldest segment
/// once the window is full, so one broadcasting channel retains a bounded amount of media
/// however long its publisher keeps sending.
pub fn publish_media(
    state: &mut ComAmazonawsIvsAmazoninteractivevideoserviceState,
    publish: &AcceptedPublish,
    kind: MediaKind,
    timestamp: u32,
    payload: &[u8],
) -> bool {
    if payload.len() > super::playback_plane::MAX_SEGMENT_BYTES {
        return false;
    }
    let live = live_stream(state, &publish.channel_arn)
        .and_then(|stream| stream.stream_id)
        .is_some_and(|stream_id| stream_id == publish.stream_id);
    if !live {
        return false;
    }
    if let Some(playlist) = state.playlists.get_mut(&publish.channel_arn) {
        if playlist.stream_id() == publish.stream_id {
            playlist.append(kind, timestamp, payload);
        }
    }
    true
}

/// Unwind the broadcast `publish` brought up, because its publisher has stopped sending.
///
/// Only the broadcast this publisher started: a publisher that dropped after `StopStream`
/// had already disconnected it, or after another publisher took the channel live again,
/// ends nothing — which is what keeps a late disconnect from ending somebody else's
/// broadcast.
pub fn end_publish(
    state: &mut ComAmazonawsIvsAmazoninteractivevideoserviceState,
    publish: &AcceptedPublish,
) {
    let ours = live_stream(state, &publish.channel_arn)
        .and_then(|stream| stream.stream_id)
        .is_some_and(|stream_id| stream_id == publish.stream_id);
    if ours {
        end_stream(state, &publish.channel_arn);
    }
}

