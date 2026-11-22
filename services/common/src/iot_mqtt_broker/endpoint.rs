// SPDX-License-Identifier: CC0-1.0

//! The broker's ENDPOINT: the socket a device opens its MQTT connection on, and the
//! `host:port` that socket is published at.
//!
//! AWS publishes an account's data endpoint as one HOST, and serves two wires on it: MQTT
//! over TLS on 8883 and the HTTPS data-plane API on 443. A client discovers that host
//! (`DescribeEndpoint`) and then dials it with whichever of the two it speaks. This
//! emulator has one host and must carry a PORT, so the published address names the socket
//! opened here — and that socket serves BOTH wires, so the one address a client is handed
//! is as complete as the host AWS hands it: a connection whose first byte opens an MQTT
//! CONNECT is served by the broker ([`super::wire`]), and an HTTP request is served by this
//! emulator's own router ([`super::https_plane`]).
//!
//! The socket is the CORE's ([`::emulator_server::Listeners::tcp_plaintext`]): the core
//! owns the bind interface (loopback unless the operator says otherwise), the port, the
//! TLS certificate and key, the connection caps, the idle timeout and the teardown, and
//! this module only hands it the protocol handler. The encryption is negotiated IN BAND
//! ([`crate::in_band_tls`]), because the one address has to serve both the TLS a device
//! dials 8883 with and a plaintext connection, and neither protocol can be told from the
//! other before byte 0 is read.
//!
//! The socket's life is the store's: the handle is held on the broker's state, so the
//! address stays open for as long as a session can be read, and dropping the store closes
//! it and severs every connection in flight. It is opened by the call that CREATES the
//! account's endpoint — "the first time DescribeEndpoint is called, an endpoint is
//! created" — and read by every later call, which is why opening it is idempotent and the
//! first address opened is the one kept.
//!
//! The same handler answers at BOTH ends of the published address: the socket the core
//! accepts a device's connections on, and a connection a client running in THIS process
//! opens to the address the control plane published
//! ([`crate::dialable_data_planes`]) — the code of a Lambda function this emulator runs,
//! which opens no host socket. Which service answers HTTP there is recorded for that same
//! client ([`crate::published_data_planes`]), because nothing can be read off an address.

use std::sync::{Arc, OnceLock};

use ::emulator_server::ResourceListener;
use ::smithy_server_runtime::EmbeddedEmulator;

use super::{broker_state, wire};

/// The service that opened the socket, as the register of the planes this emulator serves
/// in process names it: the SigV4 signing name of the IoT DATA plane (`iotdata`), which is
/// the service whose operations report on what the broker holds.
const SIGNING_NAME: &str = "iotdata";

/// The broker's endpoint: the address it answers at, and the handles that keep it open.
pub struct BrokerEndpoint {
    /// The `host:port` a client dials, which is the address `DescribeEndpoint` publishes.
    authority: String,
    /// The socket itself. Never read: it is held because dropping it closes the port.
    _listener: ResourceListener,
    /// The record that resolves this address for an in-process client opening a native
    /// CONNECTION to it. Never read: it is held because dropping it withdraws the address.
    _planes: crate::dialable_data_planes::OpenedPlanes,
    /// The engine-wide record of which service answers HTTP at [`Self::authority`] — the
    /// other wire this one address serves — withdrawn when this handle is dropped.
    published: Option<crate::published_data_planes::PublishedPlanes>,
}

/// Withdraws the engine-wide records with the port: the socket closes when this handle is
/// dropped, so nothing answers at that address any more and a client addressed there must
/// not be routed as though something did.
impl Drop for BrokerEndpoint {
    fn drop(&mut self) {
        if let Some(published) = &self.published {
            published.withdraw(&self.authority);
        }
    }
}

/// Written by hand rather than derived: a core `ResourceListener` is not `Debug`. The
/// endpoint is reported by the address it answers at, which is what a reader needs.
impl std::fmt::Debug for BrokerEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrokerEndpoint")
            .field("authority", &self.authority)
            .finish()
    }
}

impl BrokerEndpoint {
    /// The `host:port` a client reaches the broker at.
    #[must_use]
    pub fn authority(&self) -> &str {
        &self.authority
    }
}

/// The `host:port` the broker answers at, or `None` while no call has opened it.
#[must_use]
pub fn published_broker_endpoint<Eng: Send + Sync + 'static>(engine: &Eng) -> Option<String> {
    broker_state(engine)?.lock().authority()
}

/// The `host:port` the broker answers at, OPENING its socket on first use — the address a
/// device then dials — or `None` when this engine vends no store or no listener at all, in
/// which case the caller publishes the address it would have before this endpoint existed.
///
/// Idempotent and it changes no session: the first socket is kept, so the broker answers at
/// the same address for every endpoint type and for as long as the store lives, and a client
/// may keep a published address. The store guard is taken and released around the bind
/// rather than held across it, because a lock held across an await would make this future
/// non-`Send`.
pub async fn ensure_broker_endpoint<Eng: Send + Sync + 'static>(
    engine: &Eng,
    callback: &EmbeddedEmulator,
) -> Option<String> {
    let state = broker_state(engine)?;
    if let Some(open) = state.lock().authority() {
        return Some(open);
    }
    let shared = (engine as &dyn ::std::any::Any).downcast_ref::<::engine::InMemoryEngine>()?;
    let listeners = shared.listeners();
    let planes = crate::dialable_data_planes::DialablePlanes::of(engine)?;
    // The address the socket is published at, which the protocol handler reads back to
    // report a connection's TARGET socket information. It is known only once the core has
    // bound the port, so the handler is handed the cell the address is then written into.
    let published: Arc<OnceLock<String>> = Arc::new(OnceLock::new());
    let handler = Arc::new(wire::MqttEndpoint {
        state: state.clone(),
        // The core's own TLS server, lent to this plaintext socket so the TLS half of the
        // address is terminated by the core rather than here. An emulator that could
        // prepare none serves the connections made in the clear alone.
        upgrader: listeners.tls_upgrader().ok(),
        published: Arc::clone(&published),
        callback: callback.clone(),
        // The shared stores the reserved `$aws/commands/*` exchange is served from, taken
        // from the SAME engine as the broker's own store, so an execution a device drives
        // over this socket is the execution the control plane reports on.
        commands: super::commands::CommandsPlane::of(engine),
    });
    let served = crate::dialable_data_planes::served_in_process(Arc::clone(&handler), None);
    let listening = crate::dialable_data_planes::SharedHandler(Arc::clone(&handler));
    // Opened on a task of its OWN, so a runtime that cannot bind a socket at all — one
    // built with no IO driver, which is what a host driving this emulator in process may
    // hand a request — leaves this call without an address instead of taking the request
    // down with it.
    let opened = ::tokio::spawn(async move { listeners.tcp_plaintext(listening).await.ok() }).await;
    let listener = opened.ok().flatten()?;
    let authority = crate::data_plane_addressing::listener_authority(&listener);
    let registered = planes.opening([authority.clone()], SIGNING_NAME, &served);

    let mut held = state.lock();
    // Another call may have opened one while this one was binding; keep the first, so a
    // published address never changes under a client. Dropping the loser closes its port
    // and withdraws its record.
    if let Some(open) = held.authority() {
        return Some(open);
    }
    let _ = published.set(authority.clone());
    // An address nobody can read the service off — it is a `host:port`, not an endpoint
    // hostname — so the one record of WHICH service answers HTTP there is the engine-wide
    // one, written here and withdrawn when the port closes. It is the IoT DATA plane: a
    // client built from an account's data endpoint calls that plane's API on it.
    let answering = crate::published_data_planes::PublishedPlanes::of(engine);
    if let Some(answering) = &answering {
        answering.publish(&authority, SIGNING_NAME);
    }
    held.endpoint = Some(BrokerEndpoint {
        authority: authority.clone(),
        _listener: listener,
        _planes: registered,
        published: answering,
    });
    drop(held);
    Some(authority)
}

/// The host and port one published authority spells, for the socket information a
/// connection's session reports as its TARGET: the address the client reached the broker
/// at. `None` for a port that is no port at all.
#[must_use]
pub fn target_of(authority: &str) -> Option<(String, i32)> {
    let host = crate::data_plane_addressing::host_only(authority).to_string();
    let port = crate::data_plane_addressing::addressed_port(Some(authority))?;
    (!host.is_empty()).then_some((host, port))
}
