// SPDX-License-Identifier: CC0-1.0

//! The CONTRIBUTE plane of a stage: the socket an encoder publishes a stage's live
//! contribution to, and what an accepted publish writes into this store.
//!
//! RULE: an endpoint a service announces for a wire protocol is an address this emulator
//! ANSWERS that protocol on, reachable by a client that runs OUTSIDE this process.
//! `StageEndpoints$rtmp` and `$rtmps` are "the endpoint to be used for IVS real-time
//! streaming using the RTMP protocol" and "… the RTMPS protocol"
//! (`smithy_query(query="shape",shape="StageEndpoints")`) — values whose whole purpose is
//! to be DIALED: a broadcaster points its encoder at
//! `rtmp(s)://<endpoint>/<streamKey>` and expects the handshake to complete and the stage
//! to report the broadcast. So this service opens a socket of the EMULATOR's own through
//! the core ([`::emulator_server::Listeners::tcp_plaintext`], which owns the socket, the
//! bind interface, the port range, the TLS, the handshake deadline, the idle timeout and
//! the connection caps), and the `host:port` the CORE reports for that socket
//! ([`StageIngestPlane::authority`]) is exactly what the two contribution endpoints
//! announce ([`super::announced_stage_endpoints`]) — so the value a client is handed is
//! the address it dials, whether it runs in this process or holds a socket of its own. A
//! connection an in-process client opens to that address is served by the same handler
//! ([`::service_common::dialable_data_planes`], the tree's one register of the planes it
//! has opened).
//!
//! ONE socket serves the whole deployment, because the endpoints a stage announces carry
//! no part of a stage's own identity ([`super::stage_endpoints`]) and WHICH stage a
//! contribution reaches is carried by the ingest key the encoder authenticates with —
//! exactly as it is against AWS, where an ingest configuration's `streamKey` is the
//! "ingest-key value for the RTMP(S) protocol". That is also why the socket is opened by
//! `CreateStage`: the call that creates the thing an encoder contributes to is the call
//! that has to be able to tell it where.
//!
//! Both the `rtmps://` an AWS client dials and the `rtmp://` a client of a cleartext
//! ingest configuration dials are served on that one address: the connection's first byte
//! says which ([`::service_common::in_band_tls`], the tree's one reading of that
//! negotiation), a TLS record is handed to the core's own TLS server and everything after
//! it is read decrypted, and an RTMP handshake is read as it stands. The wire itself is
//! [`rtmp_wire`].
//!
//! What an accepted publish WRITES is the state the reads already derive from, and nothing
//! else: the stage session the contribution joins and the publishing participant inside it
//! ([`super::join_stage_session`], the one place a participant of this service joins). A
//! publish presenting an ingest key this store does not hold is refused on the wire and
//! writes nothing. When the publisher goes away — or `DisconnectParticipant` or a forced
//! `DeleteIngestConfiguration` takes the participant out of the stage — the contribution is
//! unwound through the one exit every departure takes ([`super::leave_stage_session`]).
//!
//! The media itself is read off the wire and kept nowhere: no operation of this service
//! serves a stage's contributed media back (a stage is subscribed to over the WHIP and
//! events endpoints a client of the real-time SDK uses), so the bytes are accounted for by
//! the participant's publishing occurrences and nothing is retained per message.

use super::*;

/// The wire an encoder publishes over, served on the socket this module opens.
mod rtmp_wire;

/// The service that opened the socket, as the register of the planes this emulator serves
/// in process names it: this service's own SigV4 signing name
/// (`smithy_query(query="service_info")` — `sigv4Name`), because the contribute plane is
/// the data plane of this service.
pub(super) const SIGNING_NAME: &str = "ivs";

/// The socket this deployment's stage contributions are published to, as the control plane
/// announces it.
///
/// Dropping it closes the port and severs every connection on it, so it is held on the
/// store for as long as this deployment serves stages.
pub struct StageIngestPlane {
    /// The `host:port` the core says the socket is dialable at, which is the address the
    /// control plane announces for this plane ([`super::announced_stage_endpoints`]) —
    /// where a publisher anywhere on this machine opens its connection.
    authority: String,
    /// The socket itself. Never read: it is held because dropping it closes the port.
    _listener: ::emulator_server::ResourceListener,
    /// The record that resolves this plane for an in-process client opening a connection
    /// to the announced contribution address. Never read: it is held because dropping it
    /// withdraws those addresses, exactly as dropping the listener closes the port. `None`
    /// on an engine that vends no such register.
    _planes: Option<::service_common::dialable_data_planes::OpenedPlanes>,
}

/// Written by hand rather than derived: a listener handle carries no `Debug`, and what is
/// diagnosable about this record is where it answers.
impl std::fmt::Debug for StageIngestPlane {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StageIngestPlane")
            .field("authority", &self.authority)
            .finish_non_exhaustive()
    }
}

impl StageIngestPlane {
    /// The `host:port` of the SOCKET this plane answers on, which is both what the control
    /// plane announces and where every publisher dials it.
    #[must_use]
    pub fn authority(&self) -> &str {
        &self.authority
    }
}

/// The `host:port` this deployment's contribution plane answers on, as the control plane
/// ANNOUNCES it, or `None` when no plane is open — a deployment whose engine vends no
/// listener surface, or where the core could not bind, which has no contribution address to
/// announce at all.
///
/// Read off the store a caller already holds, which is where the plane records its socket
/// ([`ensure_ingest_plane`]): one deployment, one contribution plane, one address for every
/// stage of the account.
#[must_use]
pub fn contribute_authority(
    state: &ComAmazonawsIvsrealtimeAmazoninteractivevideoservicerealtimeState,
) -> Option<&str> {
    state.ingest_plane.as_ref().map(StageIngestPlane::authority)
}

/// The `host:port` the contribution plane answers on, OPENING its socket on first use, or
/// `None` when this engine vends no listener surface or the core could not bind — in which
/// case there is no plane, and a stage announces no contribution address, which is the
/// honest report of a plane that could not be served.
///
/// Idempotent and it disturbs no connection: the first socket is kept, so every stage of
/// this deployment is served by one contribution plane and a broadcaster may cache the
/// announced value the way the model's `StageEndpoints` doc tells it to ("we recommend that
/// you cache these values at stage creation"). The store guard is taken and released around
/// the bind rather than held across it, because a lock held across an await would make this
/// future non-`Send`.
pub async fn ensure_ingest_plane<Eng: Send + Sync + 'static>(
    engine: &Eng,
    state: &::engine::ServiceState<ComAmazonawsIvsrealtimeAmazoninteractivevideoservicerealtimeState>,
) -> Option<String> {
    if let Some(open) = state.lock().ingest_plane.as_ref() {
        return Some(open.authority().to_string());
    }
    let shared = (engine as &dyn ::std::any::Any).downcast_ref::<::engine::InMemoryEngine>()?;
    let listeners = shared.listeners();
    let handler = ::std::sync::Arc::new(rtmp_wire::StageIngest {
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
    // served by the very handler the socket's connections are, under this service's own
    // store guard. The register and this store are ONE engine's, so another engine in this
    // process resolves none of them and shares no plane with this one.
    let registered = ::service_common::dialable_data_planes::DialablePlanes::of(engine)
        .map(|planes| planes.opening(vec![authority.clone()], SIGNING_NAME, &served));
    if registered.is_none() {
        // An engine that vends no register leaves an in-process client to the socket
        // itself, which is a deployment fact worth being able to read off a log rather than
        // an absence that looks like a successful record.
        ::tracing::debug!(
            %authority,
            "this engine vends no register of open planes: the stage contribution address \
             is reachable at the socket alone"
        );
    }
    store.ingest_plane = Some(StageIngestPlane {
        authority: authority.clone(),
        _listener: listener,
        _planes: registered,
    });
    drop(store);
    ::tracing::info!(
        %authority,
        "opened the IVS RealTime stage contribution plane: stages announce this address as \
         their RTMP and RTMPS endpoints"
    );
    Some(authority)
}

/// One publish accepted on the contribution plane: the participant it publishes as, in the
/// session of the stage it reached.
#[derive(Debug, Clone)]
pub struct AcceptedPublish {
    /// The ingest configuration whose ingest key the publisher presented.
    pub ingest_configuration_arn: String,
    /// The stage that configuration is associated with, which the publish joined.
    pub stage_arn: String,
    /// The session of that stage the publish is in.
    pub session_id: String,
    /// The participant the publisher appears as in that session.
    pub participant_id: String,
}

impl AcceptedPublish {
    /// Which participant of which session this publish is a connection OF — the key the
    /// store counts its live connections under ([`ComAmazonawsIvsrealtimeAmazoninteractivevideoservicerealtimeState::ingest_publish_connections`]).
    #[must_use]
    fn connection_key(&self) -> IngestPublishConnection {
        (
            self.stage_arn.clone(),
            self.session_id.clone(),
            self.participant_id.clone(),
        )
    }
}

/// Which participant of which session a live publish connection belongs to: the stage, the
/// session inside it and the participant publishing.
///
/// Redundant ingest is published over MORE THAN ONE connection for one participant ("see
/// Redundant Ingest", as `IngestConfiguration$redundantIngestCredentials` points at), and
/// an encoder that reconnects may hold two for a moment, so the store counts the
/// connections per participant and the participant leaves when the last of them is gone.
pub type IngestPublishConnection = (String, String, String);

/// Admit a publish presenting `stream_key_value` over `protocol`, putting the participant
/// that key was minted for into its stage as a publisher, or `None` when no ingest
/// configuration of this store mints that key for a stage it holds — in which case nothing
/// is written and the publisher is refused on the wire.
///
/// This is the one thing that makes an IVS RealTime ingest configuration `ACTIVE`, exactly
/// as on AWS: the member is `ACTIVE` "if a publisher currently is publishing to the stage
/// associated with the ingest configuration", and no operation of the control plane makes a
/// publisher publish. What it writes is what the reads already derive from — the stage's
/// session, the participant record inside it and that participant's join and publish
/// occurrences — so `GetIngestConfiguration`, `GetStage`, `ListStageSessions`,
/// `GetParticipant` and `ListParticipantEvents` all report one contribution.
///
/// `protocol` is the `ParticipantProtocol` the connection itself was opened with — the wire
/// the encoder dialled, which is what `Participant$protocol` ("type of ingest protocol that
/// the participant employs for broadcasting") reports.
pub fn accept_publish(
    state: &mut ComAmazonawsIvsrealtimeAmazoninteractivevideoservicerealtimeState,
    stream_key_value: &str,
    protocol: &str,
) -> Option<AcceptedPublish> {
    let presented = presented_credential(state, stream_key_value)?;
    // An ingest key authenticates a publish to the stage its configuration is associated
    // with, so a configuration associated with no stage ([`NO_ASSOCIATED_STAGE_ARN`]) — and
    // one naming a stage this store no longer holds — has nothing to publish to.
    if !state.stages.contains_key(&presented.stage_arn) {
        return None;
    }
    let joined = JoinedParticipant {
        participant_id: presented.participant_id.clone(),
        // The user id the configuration was created WITH, or none where it was created
        // without one: the configuration materialises that member as the empty string for
        // its own projections ([`UNNAMED_PARTICIPANT_USER_ID`]), and a participant carries
        // a user id only where one was really named.
        user_id: (!presented.user_id.is_empty()).then(|| presented.user_id.clone()),
        // "Application-provided attributes to store in the IngestConfiguration and attach
        // to a stage", as `IngestConfiguration$attributes` states what they are for: the
        // publish is what attaches them.
        attributes: presented.attributes.clone(),
        published: true,
        // This emulator writes no participant media, so nothing records this participant
        // and the join takes [`NOT_RECORDED_PARTICIPANT_RECORDING_STATE`].
        recording_state: None,
        redundant_ingest: presented.redundant_ingest,
        ingest_configuration_arn: Some(presented.configuration_arn.clone()),
        replica: false,
        // An RTMP publisher is an encoder rather than a browser SDK, so it reports none of
        // the client attributes a joining SDK does; the members stay absent rather than
        // carrying a value nothing observed.
        client: ParticipantClient::default(),
        protocol: Some(protocol.to_string()),
    };
    let record = join_stage_session(state, &presented.stage_arn, joined)?;
    let session_id = state
        .stages
        .get(&presented.stage_arn)?
        .active_session_id
        .clone()?;
    let publish = AcceptedPublish {
        ingest_configuration_arn: presented.configuration_arn,
        stage_arn: presented.stage_arn,
        session_id,
        participant_id: record.participant_id,
    };
    *state
        .ingest_publish_connections
        .entry(publish.connection_key())
        .or_insert(0) += 1;
    Some(publish)
}

/// Whether the contribution `publish` brought up is STILL the one this store holds: the
/// participant is in the session and publishing media to it right now.
///
/// `false` is a publisher the ingest server disconnects, which is what the caller then does
/// on the wire: `DisconnectParticipant` ("disconnects a specified participant from a
/// specified stage") and a forced `DeleteIngestConfiguration` both take the participant out
/// of the session, and a publisher left sending bytes into a stage it is no longer in is
/// exactly what those calls mean to stop.
#[must_use]
pub fn publishing(
    state: &ComAmazonawsIvsrealtimeAmazoninteractivevideoservicerealtimeState,
    publish: &AcceptedPublish,
) -> bool {
    state
        .stage_session(&publish.stage_arn, &publish.session_id)
        .is_some_and(|session| {
            session
                .participants
                .iter()
                .any(|participant| {
                    participant.participant_id == publish.participant_id
                        && participant.state == PARTICIPANT_STATE_CONNECTED
                })
                && session.is_publishing(&publish.participant_id)
        })
}

/// Unwind the contribution `publish` brought up, because its publisher has stopped sending.
///
/// The participant leaves through the one place a participant of this service leaves
/// ([`leave_stage_session`]), so it is reported `DISCONNECTED` with its publish and
/// departure occurrences recorded, a session it emptied ends and the stage reports no
/// active session again, and the ingest configuration it published with reports `INACTIVE`.
///
/// Only once its LAST connection is gone: redundant ingest publishes one participant over
/// more than one connection, so a second path dropping leaves the participant publishing
/// over the path that is still open.
pub fn end_publish(
    state: &mut ComAmazonawsIvsrealtimeAmazoninteractivevideoservicerealtimeState,
    publish: &AcceptedPublish,
) {
    let key = publish.connection_key();
    let remaining = match state.ingest_publish_connections.get_mut(&key) {
        Some(connections) => {
            *connections = connections.saturating_sub(1);
            *connections
        }
        None => 0,
    };
    if remaining > 0 {
        return;
    }
    state.ingest_publish_connections.remove(&key);
    leave_stage_session(
        state,
        &publish.stage_arn,
        &publish.session_id,
        &publish.participant_id,
    );
}

/// What an ingest key a publisher presented authenticates it as: the configuration that
/// minted the key, the stage it publishes to, and the identity it publishes under.
struct PresentedCredential {
    configuration_arn: String,
    stage_arn: String,
    participant_id: String,
    user_id: String,
    attributes: HashMap<String, String>,
    redundant_ingest: bool,
}

/// The credential `stream_key_value` is, or `None` for a value no ingest configuration of
/// this store minted.
///
/// The key is matched on its VALUE — `IngestConfiguration$streamKey` is the "ingest-key
/// value for the RTMP(S) protocol" — because that is the only thing the publisher presents,
/// and the key names the participant it was minted for: a configuration's own key publishes
/// as its `participantId`, and each of its redundant ingest credentials publishes as its
/// own ([`IngestCredential`]).
///
/// By creation order, so a deployment holding several configurations resolves one presented
/// key the same way on every connection.
fn presented_credential(
    state: &ComAmazonawsIvsrealtimeAmazoninteractivevideoservicerealtimeState,
    stream_key_value: &str,
) -> Option<PresentedCredential> {
    if stream_key_value.is_empty() {
        return None;
    }
    let mut configurations: Vec<&IngestConfigurationRecord> =
        state.ingest_configurations.values().collect();
    configurations.sort_by_key(|configuration| configuration.sequence);
    for configuration in configurations {
        let participant_id = if configuration.stream_key == stream_key_value {
            configuration.participant_id.clone()
        } else if let Some(credential) = configuration
            .redundant_ingest_credentials
            .iter()
            .find(|credential| credential.stream_key == stream_key_value)
        {
            credential.participant_id.clone()
        } else {
            continue;
        };
        return Some(PresentedCredential {
            configuration_arn: configuration.arn.clone(),
            stage_arn: configuration.stage_arn.clone(),
            participant_id,
            user_id: configuration.user_id.clone(),
            attributes: configuration.attributes.clone(),
            redundant_ingest: configuration.redundant_ingest,
        });
    }
    None
}
