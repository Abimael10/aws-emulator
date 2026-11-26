// SPDX-License-Identifier: CC0-1.0

//! The RTMP wire the stage contribution plane serves: the server half of the handshake, the
//! chunk stream and the AMF0 commands a publishing encoder speaks.
//!
//! The protocol itself is a maintained implementation of it (the `rml_rtmp` crate), so what
//! is written here is only this service's half of the conversation: which publishes are
//! admitted, and what an admitted publish writes into the store
//! ([`super::accept_publish`] and its siblings). An encoder that completes the handshake and
//! publishes with an ingest key this store holds joins that key's stage as a publishing
//! participant; one that presents a key no ingest configuration minted is refused on the
//! wire and changes nothing.
//!
//! Containment is the core's: it owns the socket, the TLS certificate and key, the bind
//! interface, the port range, the handshake deadline, the per-connection idle timeout and
//! the connection cap. This module reaches no host path, no socket and no credential — it
//! reads the connection it is handed and writes this service's own store — and it retains
//! none of the media it reads, so one publisher's cost to this store is the participant
//! record its publish joined and nothing per message.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;

use ::emulator_server::{ListenerConn, NativeHandler, TlsUpgrader};
use ::rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use ::rml_rtmp::sessions::{
    ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult,
};
use ::tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

/// How much of the connection is read at a time.
const READ_BYTES: usize = 16 * 1024;

/// The status an ingest server refuses a publish with when the stream name it presented is
/// not one it may publish — RTMP's own code for it, which is what an encoder reports to its
/// operator.
const PUBLISH_REFUSED_CODE: &str = "NetStream.Publish.BadName";

/// The text beside that code: what the publisher got wrong, and nothing about the ingest
/// keys this store does hold.
const PUBLISH_REFUSED_DESCRIPTION: &str =
    "The ingest key presented is not an ingest key of any ingest configuration of a stage";

/// The status this plane refuses a PLAY request with: it ingests contributions, and a stage
/// is subscribed to over the WHIP and events endpoints it announces beside these two.
const PLAY_REFUSED_CODE: &str = "NetStream.Play.Failed";

/// The text beside that code.
const PLAY_REFUSED_DESCRIPTION: &str =
    "This endpoint ingests stage contributions; a stage is subscribed to over its WHIP and \
     events endpoints";

/// The `ParticipantProtocol` a publisher that dialled the ENCRYPTED half of this address
/// broadcasts with — one of the two values the modelled enum carries for an ingest
/// protocol.
const PROTOCOL_ENCRYPTED: &str = "RTMPS";

/// The same for a publisher that dialled the address in the clear, which is what a
/// cleartext ingest configuration's publisher does
/// (`CreateIngestConfigurationRequest$insecureIngest` "allows insecure RTMP ingest").
const PROTOCOL_CLEARTEXT: &str = "RTMP";

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

/// The handler the core runs for every connection accepted on the contribution socket.
pub(super) struct StageIngest {
    /// This service's store, so a publish authenticates against the ingest keys as they
    /// are NOW: a configuration deleted while a session is open is in force for the next
    /// publish.
    pub(super) state:
        ::engine::ServiceState<ComAmazonawsIvsrealtimeAmazoninteractivevideoservicerealtimeState>,
    /// The core's TLS server, for a publisher that dialled `rtmps://`, or `None` on an
    /// emulator that could prepare none — whose plaintext publishers are still served.
    pub(super) upgrader: Option<TlsUpgrader>,
}

impl NativeHandler for StageIngest {
    fn serve(&self, conn: ListenerConn) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let state = self.state.clone();
        let upgrader = self.upgrader.clone();
        Box::pin(async move {
            // The connection with encryption negotiated: `rtmps://` is TLS from the first
            // byte and `rtmp://` is the RTMP handshake from the first byte, so the byte
            // itself says which the publisher dialled — read through the tree's one
            // reading of that negotiation. WHICH it was is the protocol the participant
            // broadcasts with, so it is carried into the publish.
            let Some((conn, encrypted)) =
                ::service_common::in_band_tls::negotiated_scheme(conn, upgrader).await
            else {
                return;
            };
            let mut session = IngestConnection {
                state,
                protocol: match encrypted {
                    true => PROTOCOL_ENCRYPTED,
                    false => PROTOCOL_CLEARTEXT,
                },
                publish: None,
            };
            session.serve(conn).await;
        })
    }
}

/// One publisher's connection, and the contribution it has brought up on it.
struct IngestConnection {
    /// This service's store.
    state:
        ::engine::ServiceState<ComAmazonawsIvsrealtimeAmazoninteractivevideoservicerealtimeState>,
    /// The `ParticipantProtocol` this connection was opened with.
    protocol: &'static str,
    /// The contribution this connection is publishing, once a publish has been admitted.
    publish: Option<AcceptedPublish>,
}

impl IngestConnection {
    /// Serve the connection until the publisher stops, the participant is taken out of the
    /// stage under it, or the wire cannot be read — and unwind the contribution it brought
    /// up either way, because an ingest configuration is `ACTIVE` exactly while something
    /// is publishing with it.
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
                        ::tracing::debug!(
                            %error,
                            "an RTMP publisher sent a message this wire could not read"
                        );
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
        self.end_contribution();
    }

    /// Carry out what the session asked for: write every response it produced, and act on
    /// every event it raised — which may itself produce more responses.
    ///
    /// `false` ends the connection: the publisher has finished, its participant has been
    /// taken out of the stage under it, or it asked for something this plane does not
    /// serve. A refusal the publisher is TOLD about is written and flushed before that
    /// happens ([`Served::Closing`]), because the status is the refusal — a bare close says
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
                ServerSessionResult::RaisedEvent(event) => match self.handle_event(session, event)
                {
                    Served::Serving(more) => pending.extend(more),
                    Served::Closing(last) => {
                        pending.extend(last);
                        serving = false;
                    }
                },
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
    fn handle_event(&mut self, session: &mut ServerSession, event: ServerSessionEvent) -> Served {
        match event {
            // A publisher connects before it publishes, and the application it names is
            // not what identifies the stage — the ingest key it authenticates with is — so
            // the connection is accepted and the stage is resolved at the publish.
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
            ServerSessionEvent::AudioDataReceived { .. }
            | ServerSessionEvent::VideoDataReceived { .. } => self.handle_media(),
            // The publisher has stopped: the contribution it brought up is over, and the
            // connection with it.
            ServerSessionEvent::PublishStreamFinished { .. } => Served::Closing(Vec::new()),
            // This plane INGESTS. A stage is subscribed to over the WHIP and events
            // endpoints it announces beside these two, so a client asking to play on the
            // contribution socket is told this is not a stream it may play, and the
            // connection ends once that status is written.
            ServerSessionEvent::PlayStreamRequested { request_id, .. } => Served::Closing(
                refusal(session, request_id, PLAY_REFUSED_CODE, PLAY_REFUSED_DESCRIPTION),
            ),
            // Everything else is the wire's own bookkeeping (a chunk size change, an
            // acknowledgement, a ping answer, the encoder's own metadata, a command this
            // plane has no answer for), which changes nothing about the contribution: no
            // member of a `Participant` reports what an encoder says it is sending, so
            // nothing is recorded for it.
            _ => Served::Serving(Vec::new()),
        }
    }

    /// Admit or refuse a publish presenting `stream_key`.
    ///
    /// The key authenticates the publish: a key this store holds puts the participant it
    /// was minted for into its stage as a publisher, and anything else is refused on the
    /// wire — the publisher is TOLD its stream name is not one it may publish (the status
    /// is written before the connection ends), and nothing is written to the store.
    fn handle_publish(
        &mut self,
        session: &mut ServerSession,
        request_id: u32,
        stream_key: &str,
    ) -> Served {
        let accepted = {
            let mut store = self.state.lock();
            let mut accepted = None;
            for presented in presented_ingest_keys(stream_key) {
                accepted = accept_publish(&mut store, presented, self.protocol);
                if accepted.is_some() {
                    break;
                }
            }
            accepted
        };
        let Some(accepted) = accepted else {
            ::tracing::debug!(
                "a publish presenting an ingest key no configuration of this store mints was \
                 refused"
            );
            return Served::Closing(refusal(
                session,
                request_id,
                PUBLISH_REFUSED_CODE,
                PUBLISH_REFUSED_DESCRIPTION,
            ));
        };
        ::tracing::info!(
            stage_arn = %accepted.stage_arn,
            session_id = %accepted.session_id,
            participant_id = %accepted.participant_id,
            "a publisher joined an IVS RealTime stage on the contribution plane"
        );
        self.publish = Some(accepted);
        match session.accept_request(request_id) {
            Ok(results) => Served::Serving(results),
            Err(_unanswerable) => Served::Closing(Vec::new()),
        }
    }

    /// Account for one ingested message, and keep serving only while the participant this
    /// publisher joined as is still publishing in its session.
    ///
    /// The media itself is retained nowhere: no operation of this service serves a stage's
    /// contributed media back, so what an accepted message means to this store is that the
    /// publisher is still there.
    fn handle_media(&mut self) -> Served {
        // Media before an admitted publish belongs to no contribution of this store, so
        // the connection carries on and nothing is recorded.
        let Some(publish) = self.publish.clone() else {
            return Served::Serving(Vec::new());
        };
        let store = self.state.lock();
        match publishing(&store, &publish) {
            true => Served::Serving(Vec::new()),
            // The participant was taken out of the stage under this publisher
            // (`DisconnectParticipant`, a forced `DeleteIngestConfiguration`, the delete of
            // the stage): the connection is severed, which is what disconnecting a
            // publisher means on the wire.
            false => Served::Closing(Vec::new()),
        }
    }

    /// Unwind the contribution this connection brought up, once it is over.
    fn end_contribution(&mut self) {
        let Some(publish) = self.publish.take() else {
            return;
        };
        let mut store = self.state.lock();
        end_publish(&mut store, &publish);
    }
}

/// Every ingest key a publisher's stream name may be, in the order they are tried.
///
/// A broadcaster configures `rtmp(s)://<endpoint>/<streamKey>`, and what reaches an ingest
/// server as the "stream name" depends on where that encoder split the application off from
/// the key: most present the key alone, some present the whole path after the host, and some
/// append parameters of their own after it. The key itself is the one thing that
/// authenticates the publish and names the stage — `IngestConfiguration$streamKey` is the
/// "ingest-key value for the RTMP(S) protocol" — so the candidates are the readings of what
/// the publisher presented, most literal first, and the store says which of them is a key it
/// minted.
fn presented_ingest_keys(stream_key: &str) -> Vec<&str> {
    let without_query = stream_key.split('?').next().unwrap_or(stream_key);
    let mut candidates = vec![
        stream_key,
        without_query,
        without_query.trim_matches('/'),
    ];
    // What follows the application segment, and the last segment on its own: an encoder that
    // presented the whole path after the host.
    let trimmed = without_query.trim_matches('/');
    if let Some((_application, rest)) = trimmed.split_once('/') {
        candidates.push(rest);
    }
    if let Some(last) = trimmed.rsplit('/').next() {
        candidates.push(last);
    }
    candidates.retain(|candidate| !candidate.is_empty());
    candidates.dedup();
    candidates
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
                ::tracing::debug!(
                    %error,
                    "an RTMP handshake on the stage contribution plane was not completed"
                );
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the ingest key a publisher presents is read out of whatever it presented —
    /// the key alone, the key under the application path it dialed, and with or without a
    /// query the encoder appended.
    #[test]
    fn a_presented_ingest_key_is_read_out_of_what_an_encoder_presented() {
        // The rows span what the reading generalises over: the key alone, a key carrying
        // the encoder's own query, a key written with the path separator, a publisher that
        // presented the whole path after the host (an encoder that split no application off
        // the key), a key that itself carries a path separator, and nothing at all.
        let rows: &[(&str, &str, &str)] = &[
            (
                "the key alone",
                "rt_000000000000_us-east-1_abc_secret",
                "rt_000000000000_us-east-1_abc_secret",
            ),
            (
                "a key carrying a query",
                "rt_000000000000_us-east-1_abc_secret?token=1",
                "rt_000000000000_us-east-1_abc_secret",
            ),
            ("a key written with separators", "/rt_abc/", "rt_abc"),
            (
                "the whole path after the host",
                "app/rt_000000000000_us-east-1_abc_secret",
                "rt_000000000000_us-east-1_abc_secret",
            ),
            (
                "the whole path, with a query",
                "/app/rt_abc?token=1",
                "rt_abc",
            ),
            (
                "a key that itself carries a separator",
                "app/rt_abc_se/cret",
                "rt_abc_se/cret",
            ),
        ];
        for (case, presented, expected) in rows {
            let candidates = presented_ingest_keys(presented);
            assert!(
                candidates.contains(expected),
                "case {case:?}: expected {expected:?} among the keys read, got {candidates:?}"
            );
        }
        assert!(
            presented_ingest_keys("").is_empty(),
            "case no key at all: expected no key to be read, got {:?}",
            presented_ingest_keys("")
        );
    }
}
