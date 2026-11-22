// SPDX-License-Identifier: CC0-1.0

//! The RTMP wire the contribute plane serves: the server half of the handshake, the chunk
//! stream and the AMF0 commands a publishing encoder speaks.
//!
//! The protocol itself is a maintained implementation of it (the `rml_rtmp` crate), so what
//! is written here is only this service's half of the conversation: which publishes are
//! admitted, and what an admitted publish writes into the store
//! ([`super::accept_publish`] and its siblings). An encoder that completes the handshake and
//! publishes with a stream key this store holds brings that key's channel live; one that
//! presents a key no channel minted is refused on the wire and changes nothing.
//!
//! Containment is the core's: it owns the socket, the TLS certificate and key, the bind
//! interface, the port range, the handshake deadline, the per-connection idle timeout and
//! the connection cap. This module reaches no host path, no socket and no credential — it
//! reads the connection it is handed and writes this service's own store — and the media it
//! packages is bounded by the playlist's own window ([`super::ChannelPlaylist`]).

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;

use ::emulator_server::{ListenerConn, NativeHandler, TlsUpgrader};
use ::rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use ::rml_rtmp::sessions::{
    ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult, StreamMetadata,
};
use ::tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

/// How much of the connection is read at a time.
const READ_BYTES: usize = 16 * 1024;

/// The status an ingest server refuses a publish with when the stream name it presented is
/// not one it may publish — RTMP's own code for it, which is what an encoder reports to its
/// operator.
const PUBLISH_REFUSED_CODE: &str = "NetStream.Publish.BadName";

/// The text beside that code: what the publisher got wrong, and nothing about the keys this
/// store does hold.
const PUBLISH_REFUSED_DESCRIPTION: &str =
    "The stream key presented is not a stream key of any channel";

/// The status this plane refuses a PLAY request with: it ingests, and a channel is played
/// back over HLS from the address its `playbackUrl` announces.
const PLAY_REFUSED_CODE: &str = "NetStream.Play.Failed";

/// The text beside that code.
const PLAY_REFUSED_DESCRIPTION: &str =
    "This endpoint ingests contributions; a channel is played back from its playback URL";

/// What one event leaves the connection doing: the responses to write, and whether the
/// connection carries on once they are written.
///
/// A refusal is a response: it is written and flushed BEFORE the connection ends, so a
/// publisher learns why it was turned away instead of seeing a bare close.
enum Served {
    /// Write these, and keep serving.
    Serving(Vec<ServerSessionResult>),
    /// Write these, then end the connection.
    Closing(Vec<ServerSessionResult>),
}

/// The responses that TELL a client its request is refused, under `code` and `description`,
/// or none when the session cannot state one — in which case the connection simply ends.
fn refusal(
    session: &mut ServerSession,
    request_id: u32,
    code: &str,
    description: &str,
) -> Vec<ServerSessionResult> {
    session
        .reject_request(request_id, code, description)
        .unwrap_or_default()
}

/// The handler the core runs for every connection accepted on the contribute socket.
pub(super) struct RtmpIngest {
    /// This service's store, so a publish authenticates against the stream keys as they
    /// are NOW: a key deleted while a session is open is in force for the next publish.
    pub(super) state: ::engine::ServiceState<ComAmazonawsIvsAmazoninteractivevideoserviceState>,
    /// The core's TLS server, for a publisher that dialled `rtmps://`, or `None` on an
    /// emulator that could prepare none — whose plaintext publishers are still served.
    pub(super) upgrader: Option<TlsUpgrader>,
}

impl NativeHandler for RtmpIngest {
    fn serve(&self, conn: ListenerConn) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let state = self.state.clone();
        let upgrader = self.upgrader.clone();
        Box::pin(async move {
            // The connection with encryption negotiated: `rtmps://` is TLS from the first
            // byte and `rtmp://` is the RTMP handshake from the first byte, so the byte
            // itself says which the publisher dialled — read through the tree's one
            // reading of that negotiation.
            let Some(conn) = ::service_common::in_band_tls::negotiated(conn, upgrader).await
            else {
                return;
            };
            let mut session = IngestConnection {
                state,
                publish: None,
            };
            session.serve(conn).await;
        })
    }
}

/// One publisher's connection, and the broadcast it has brought up on it.
struct IngestConnection {
    /// This service's store.
    state: ::engine::ServiceState<ComAmazonawsIvsAmazoninteractivevideoserviceState>,
    /// The broadcast this connection is publishing, once a publish has been admitted.
    publish: Option<AcceptedPublish>,
}

impl IngestConnection {
    /// Serve the connection until the publisher stops, the broadcast is disconnected under
    /// it, or the wire cannot be read — and unwind the broadcast it brought up either way,
    /// because a channel is live exactly while something is publishing to it.
    async fn serve(&mut self, mut conn: ListenerConn) {
        let Some(leftover) = server_handshake(&mut conn).await else {
            return;
        };
        let Ok((mut session, initial)) = ServerSession::new(ServerSessionConfig::new()) else {
            return;
        };
        let mut served = self.dispatch(&mut conn, &mut session, initial).await;
        let mut arriving = leftover;
        while served {
            if !arriving.is_empty() {
                let results = match session.handle_input(&arriving) {
                    Ok(results) => results,
                    Err(error) => {
                        ::tracing::debug!(%error, "an RTMP publisher sent a message this wire could not read");
                        break;
                    }
                };
                arriving = Vec::new();
                served = self.dispatch(&mut conn, &mut session, results).await;
                continue;
            }
            let mut buffer = [0_u8; READ_BYTES];
            match conn.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => arriving = buffer[..read].to_vec(),
            }
        }
        self.end_broadcast();
    }

    /// Carry out what the session asked for: write every response it produced, and act on
    /// every event it raised — which may itself produce more responses.
    ///
    /// `false` ends the connection: the publisher has finished, its broadcast has been
    /// disconnected under it, or it asked for something this plane does not serve. A
    /// refusal the publisher is TOLD about is written and flushed before that happens
    /// ([`Served::Closing`]), because the status is the refusal — a bare close says
    /// nothing.
    async fn dispatch(
        &mut self,
        conn: &mut ListenerConn,
        session: &mut ServerSession,
        results: Vec<ServerSessionResult>,
    ) -> bool {
        let mut pending: VecDeque<ServerSessionResult> = results.into();
        let mut serving = true;
        while let Some(result) = pending.pop_front() {
            match result {
                ServerSessionResult::OutboundResponse(packet) => {
                    if conn.write_all(&packet.bytes).await.is_err() {
                        return false;
                    }
                }
                ServerSessionResult::RaisedEvent(event) => {
                    match self.handle_event(session, event) {
                        Served::Serving(more) => pending.extend(more),
                        Served::Closing(last) => {
                            pending.extend(last);
                            serving = false;
                        }
                    }
                }
                // A message this wire's implementation does not model: the publisher is
                // told nothing about it and the connection carries on, which is what an
                // ingest server does with a command it has no answer for.
                ServerSessionResult::UnhandleableMessageReceived(_) => {}
            }
        }
        if conn.flush().await.is_err() {
            return false;
        }
        serving
    }

    /// Act on one event the session raised: the responses it produces, and whether the
    /// connection carries on after they are written.
    fn handle_event(
        &mut self,
        session: &mut ServerSession,
        event: ServerSessionEvent,
    ) -> Served {
        match event {
            // A publisher connects before it publishes, and the application it names is
            // not what identifies the channel — the stream key it authenticates with is —
            // so the connection is accepted and the channel is resolved at the publish.
            ServerSessionEvent::ConnectionRequested { request_id, .. }
            | ServerSessionEvent::ReleaseStreamRequested { request_id, .. } => {
                match session.accept_request(request_id) {
                    Ok(results) => Served::Serving(results),
                    Err(_unanswerable) => Served::Closing(Vec::new()),
                }
            }
            ServerSessionEvent::PublishStreamRequested {
                request_id,
                stream_key,
                ..
            } => self.handle_publish(session, request_id, &stream_key),
            ServerSessionEvent::StreamMetadataChanged { metadata, .. } => {
                // Metadata an encoder sent before its publish was admitted belongs to no
                // broadcast of this store, so it is recorded nowhere and the connection
                // carries on — the publish that follows is what admits it.
                let Some(publish) = self.publish.clone() else {
                    return Served::Serving(Vec::new());
                };
                let metadata = published_metadata(&metadata);
                let mut store = self.state.lock();
                publish_metadata(&mut store, &publish, &metadata);
                Served::Serving(Vec::new())
            }
            ServerSessionEvent::AudioDataReceived {
                data, timestamp, ..
            } => self.handle_media(MediaKind::Audio, timestamp.value, &data),
            ServerSessionEvent::VideoDataReceived {
                data, timestamp, ..
            } => self.handle_media(MediaKind::Video, timestamp.value, &data),
            // The publisher has stopped: the broadcast it brought up is over, and the
            // connection with it.
            ServerSessionEvent::PublishStreamFinished { .. } => Served::Closing(Vec::new()),
            // This plane INGESTS. A channel is played back over HLS from the address its
            // `playbackUrl` announces ([`super::super::playback_plane`]), so a client
            // asking to play on the contribute socket is told this is not a stream it may
            // play, and the connection ends once that status is written.
            ServerSessionEvent::PlayStreamRequested { request_id, .. } => {
                Served::Closing(refusal(session, request_id, PLAY_REFUSED_CODE, PLAY_REFUSED_DESCRIPTION))
            }
            // Everything else is the wire's own bookkeeping (a chunk size change, an
            // acknowledgement, a ping answer, a command this plane has no answer for),
            // which changes nothing about the broadcast.
            _ => Served::Serving(Vec::new()),
        }
    }

    /// Admit or refuse a publish presenting `stream_key`.
    ///
    /// The key authenticates the publish: a key this store holds brings its own channel
    /// live, and anything else is refused on the wire — the publisher is TOLD its stream
    /// name is not one it may publish (the status is written before the connection ends),
    /// and nothing is written to the store.
    fn handle_publish(
        &mut self,
        session: &mut ServerSession,
        request_id: u32,
        stream_key: &str,
    ) -> Served {
        let presented = presented_stream_key(stream_key);
        let accepted = {
            let mut store = self.state.lock();
            accept_publish(&mut store, presented)
        };
        let Some(accepted) = accepted else {
            ::tracing::debug!(
                "a publish presenting a stream key no channel of this store mints was refused"
            );
            return Served::Closing(refusal(
                session,
                request_id,
                PUBLISH_REFUSED_CODE,
                PUBLISH_REFUSED_DESCRIPTION,
            ));
        };
        ::tracing::info!(
            channel_arn = %accepted.channel_arn,
            stream_id = %accepted.stream_id,
            "a publisher brought an IVS channel live on the contribute plane"
        );
        self.publish = Some(accepted);
        match session.accept_request(request_id) {
            Ok(results) => Served::Serving(results),
            Err(_unanswerable) => Served::Closing(Vec::new()),
        }
    }

    /// Package one ingested message, and keep serving only while the broadcast it belongs
    /// to is still the channel's live one.
    fn handle_media(&mut self, kind: MediaKind, timestamp: u32, payload: &[u8]) -> Served {
        // Media before an admitted publish belongs to no broadcast of this store, so it
        // is packaged nowhere and the connection carries on.
        let Some(publish) = self.publish.clone() else {
            return Served::Serving(Vec::new());
        };
        let mut store = self.state.lock();
        match publish_media(&mut store, &publish, kind, timestamp, payload) {
            true => Served::Serving(Vec::new()),
            // The broadcast was disconnected under this publisher (`StopStream`, or the
            // channel's delete): the connection is severed, which is what disconnecting an
            // ingest session means on the wire.
            false => Served::Closing(Vec::new()),
        }
    }

    /// Unwind the broadcast this connection brought up, once it is over.
    fn end_broadcast(&mut self) {
        let Some(publish) = self.publish.take() else {
            return;
        };
        let mut store = self.state.lock();
        end_publish(&mut store, &publish);
    }
}

/// The stream key a publisher presented, without any query the encoder appended to it: a
/// broadcaster configures `…/app/<streamKey>`, and some encoders carry their own parameters
/// after it.
fn presented_stream_key(stream_key: &str) -> &str {
    stream_key
        .split('?')
        .next()
        .unwrap_or(stream_key)
        .trim_matches('/')
}

/// What the publisher's own metadata says it is sending, in this service's vocabulary.
fn published_metadata(metadata: &StreamMetadata) -> PublishedMetadata {
    PublishedMetadata {
        video_width: metadata.video_width,
        video_height: metadata.video_height,
        video_codec_id: metadata.video_codec_id,
        video_frame_rate: metadata.video_frame_rate,
        video_bitrate_kbps: metadata.video_bitrate_kbps,
        audio_codec_id: metadata.audio_codec_id,
        audio_bitrate_kbps: metadata.audio_bitrate_kbps,
        audio_sample_rate: metadata.audio_sample_rate,
        audio_channels: metadata.audio_channels,
        encoder: metadata.encoder.clone(),
    }
}

/// Run the server half of the RTMP handshake on `conn`, answering the bytes that arrived
/// after it — which belong to the session — or `None` for a client that did not complete
/// one.
async fn server_handshake(conn: &mut ListenerConn) -> Option<Vec<u8>> {
    let mut handshake = Handshake::new(PeerType::Server);
    let mut buffer = [0_u8; READ_BYTES];
    loop {
        let read = match conn.read(&mut buffer).await {
            Ok(0) | Err(_) => return None,
            Ok(read) => read,
        };
        match handshake.process_bytes(&buffer[..read]) {
            Ok(HandshakeProcessResult::InProgress { response_bytes }) => {
                if !response_bytes.is_empty() && conn.write_all(&response_bytes).await.is_err() {
                    return None;
                }
            }
            Ok(HandshakeProcessResult::Completed {
                response_bytes,
                remaining_bytes,
            }) => {
                if !response_bytes.is_empty() && conn.write_all(&response_bytes).await.is_err() {
                    return None;
                }
                return conn.flush().await.ok().map(|()| remaining_bytes);
            }
            Err(error) => {
                ::tracing::debug!(%error, "an RTMP handshake on the contribute plane was not completed");
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the stream key a publisher presents is the key itself, whatever an encoder
    /// appended to the path it configured.
    #[test]
    fn a_presented_stream_key_is_read_without_what_an_encoder_appended() {
        // The rows span what the reading generalises over: the key alone, a key carrying
        // the encoder's own query, a key written with the path separator, and nothing at
        // all.
        let rows: &[(&str, &str, &str)] = &[
            ("the key alone", "sk_us-west-2_abc_def", "sk_us-west-2_abc_def"),
            (
                "a key carrying a query",
                "sk_us-west-2_abc_def?token=1",
                "sk_us-west-2_abc_def",
            ),
            ("a key written with separators", "/sk_abc/", "sk_abc"),
            ("no key at all", "", ""),
        ];
        for (case, presented, expected) in rows {
            let read = presented_stream_key(presented);
            assert_eq!(
                read, *expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }
}
