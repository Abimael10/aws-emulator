// SPDX-License-Identifier: CC0-1.0

//! The SOCKET a portal's website is reached on, and what a request made there is served by.
//!
//! `CreatePortal` mints an address a CONSUMER fetches: the portal's default domain name is
//! "generated and managed by API Gateway"
//! (`EndpointConfigurationResponse$PortalDefaultDomainName`), and what `PublishPortal` builds
//! is served on it — so a client that reads that member off the create or off `GetPortal` and
//! asks for its root expects the portal's site, and expects to stop getting it once
//! `DisablePortal` has taken the publication down.
//!
//! A name under API Gateway's own zone resolves to nothing on the machine this emulator runs
//! on, so each portal is given a socket of the EMULATOR's own
//! ([`crate::state::portal_transport`]) and publishes the `host:port` that socket answers on.
//! This module is the handler the core hands each connection accepted there:
//!
//! 1. encryption is negotiated in band, because the address is published for `https` callers
//!    and the handshake's first bytes have to be read by this socket to tell a TLS client from
//!    a plaintext one ([`::service_common::in_band_tls`] states that reading). A TLS
//!    connection is terminated by the CORE's own TLS server, under the emulator's own
//!    certificate: this address is the one API Gateway generates and manages, so no
//!    certificate of the account's belongs on it and none is invented here;
//! 2. the request is read, bounded and handed on by the one HTTP endpoint every core-owned
//!    socket of this service shares ([`super::websocket::endpoint_http`]);
//! 3. before it is handed on, it is RE-ADDRESSED at the portal's own endpoint on this
//!    emulator's front door ([`crate::state::PORTAL_ENDPOINT_LABEL`] behind
//!    [`crate::state::LOCAL_ENDPOINT_PREFIX`]) with the path the client asked for carried on
//!    unchanged — which is what makes the answer the portal data plane's own
//!    ([`super::portal`]): the build of its last settled publish at the root, its preview
//!    under the announced path, the refusal a portal serving nothing earns, and `404` for a
//!    path the portal has no page at.
//!
//! The portal is read PER REQUEST, by the id its record is stored under, so a `DisablePortal`
//! or a `DeletePortal` decides the next request on an already-open connection; and a portal
//! the account no longer holds serves nothing here, whatever this socket was opened for.

use std::pin::Pin;
use std::sync::Arc;

use ::emulator_server::{ListenerConn, NativeHandler, TlsUpgrader};
use ::smithy_server_runtime::EmbeddedEmulator;

use super::websocket::endpoint_http::{Addressed, Readdress};
use crate::state::ComAmazonawsApigatewayv2Apigatewayv2State as ServiceStore;

/// The host the re-addressed request names this emulator's front door by — the one every
/// core-owned socket of this service re-addresses at
/// ([`super::data_plane::SERVICE_ENDPOINT_HOST`]). The portal itself is named by the reserved
/// data-plane path, not by this host.
const SERVICE_ENDPOINT: &str = super::data_plane::SERVICE_ENDPOINT_HOST;

/// The status a request this address serves no page for is turned away with, and the sentence
/// beside it: the refusal every data plane of this service answers such a request with.
const FORBIDDEN: u16 = super::data_plane::FORBIDDEN_STATUS;
const FORBIDDEN_MESSAGE: &str = super::data_plane::FORBIDDEN_MESSAGE;

/// Open the socket the portal `portal_id` is reached on, answering the socket itself and the
/// `host:port` it landed on — the address that record publishes as its
/// `PortalDefaultDomainName`.
///
/// The core owns the socket, the interface, the port and the connection caps; this hands it
/// the handler and answers the HANDLE, which the caller remembers on this service's own store
/// once the record is written ([`crate::state::portal_transport`]) — so a create that is
/// refused after this point leaves nothing listening and displaces no socket another portal
/// holds. Dropping the handle closes the port. `Err` when this service was mounted on an
/// engine with no listener surface, or when the core could not bind: the caller then publishes
/// the name API Gateway's own zone would carry, which no client on this machine resolves.
///
/// `callback` is the in-process transport a request accepted there is served over — the handle
/// the operation that mints the portal is given (`OperationContext::callback`), kept for the
/// socket's whole life because a request arriving on it is served by this emulator's own front
/// door.
pub async fn open_for_portal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    callback: EmbeddedEmulator,
    portal_id: &str,
) -> Result<Opened, String> {
    let shared = (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .ok_or_else(|| "this emulator's engine has no listener surface".to_string())?;
    let state = crate::state::service_state(engine)
        .ok_or_else(|| "this emulator's engine does not carry this service's store".to_string())?;
    let listeners = shared.listeners();
    let handler = PortalSocket {
        state,
        portal_id: portal_id.to_string(),
        callback,
        // The core's own TLS server, for a client that dialled `https`. A core that cannot
        // prepare it leaves this `None`, and such a client is then refused at the first byte
        // rather than being answered in the clear.
        upgrader: listeners.tls_upgrader().ok(),
    };
    // Opened on a task of its OWN, so a runtime that cannot bind a socket at all — one built
    // with no IO driver, which is what a host that drives this emulator in process may hand a
    // request — leaves this create without an address of its own instead of taking the request
    // down with it.
    let opened = ::tokio::spawn(async move { listeners.tcp_plaintext(handler).await }).await;
    let listener = match opened {
        Ok(Ok(listener)) => Arc::new(listener),
        Ok(Err(error)) => return Err(format!("the portal's socket could not be opened: {error}")),
        Err(error) => {
            return Err(format!(
                "the portal's socket could not be opened on this runtime: {error}"
            ))
        }
    };
    let authority = crate::state::portal_transport::authority_of(&listener);
    ::tracing::info!(
        %portal_id,
        %authority,
        "opened the socket a portal's website is reached on"
    );
    Ok(Opened {
        authority,
        listener,
    })
}

/// The socket one portal is reached on, before the record that owns it is written: the address
/// it landed on, and the handle whose life is that record's.
pub struct Opened {
    /// The `host:port` a client fetches, which is what the record publishes.
    pub authority: String,
    /// The listener itself. Dropping it closes the port, which is what a create refused after
    /// the socket was opened does.
    pub listener: Arc<::emulator_server::ResourceListener>,
}

/// The handler the core hands each connection accepted on ONE portal's socket.
///
/// It carries the shared store (so every request reads the records the control plane writes),
/// the id the portal is stored under, and the in-process transport the request is served over.
struct PortalSocket {
    state: ::engine::ServiceState<ServiceStore>,
    portal_id: String,
    callback: EmbeddedEmulator,
    upgrader: Option<TlsUpgrader>,
}

impl NativeHandler for PortalSocket {
    fn serve(&self, conn: ListenerConn) -> Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        // Owned clones before the future: the core's handler contract hands back a `'static`
        // future, so nothing may borrow `&self` across it.
        let state = self.state.clone();
        let portal_id = self.portal_id.clone();
        let callback = self.callback.clone();
        let upgrader = self.upgrader.clone();
        Box::pin(async move {
            let Some(conn) = ::service_common::in_band_tls::negotiated(conn, upgrader).await else {
                return;
            };
            let readdress: Readdress = Arc::new(move |parts: &::http::request::Parts| {
                site_target(&state, &portal_id, parts)
            });
            super::websocket::endpoint_http::serve_readdressed(callback, conn, Some(readdress))
                .await;
        })
    }
}

/// Where a request `parts` addresses goes on the portal `portal_id`: that portal's own
/// endpoint on this emulator's front door, with the path the client asked for and the query
/// string it sent carried on unchanged.
///
/// `Refused` for a portal the account no longer HOLDS — the socket of a deleted portal serves
/// nothing, which is the refusal a domain with nothing live answers — and for a path that does
/// not render as one UNDER that portal's own endpoint, the one thing a path the CLIENT chose
/// must not be able to leave.
fn site_target(
    state: &::engine::ServiceState<ServiceStore>,
    portal_id: &str,
    parts: &::http::request::Parts,
) -> Addressed {
    {
        let store = state.lock();
        if store.live_portal(portal_id).is_none() {
            return Addressed::Refused(FORBIDDEN, FORBIDDEN_MESSAGE);
        }
    }
    // A DOT SEGMENT (`.` or `..`) is a relative reference: a remainder that could climb back
    // out of the portal's own endpoint would address another plane of this process. The
    // reading is the tree's one, which resolves the escapes before it splits the path, so a
    // separator hidden in an escape is seen too.
    if ::service_common::data_plane_addressing::carries_a_dot_segment(parts.uri.path()) {
        return Addressed::Refused(FORBIDDEN, FORBIDDEN_MESSAGE);
    }
    let prefix = format!(
        "/{marker}/{label}/{portal_id}",
        marker = crate::state::LOCAL_ENDPOINT_PREFIX,
        label = crate::state::PORTAL_ENDPOINT_LABEL,
    );
    let query = parts
        .uri
        .query()
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    let within = parts.uri.path();
    let uri = format!("https://{SERVICE_ENDPOINT}{prefix}{within}{query}");
    // READ BACK off the rendered target: the path is the CLIENT's own, so this is the
    // assertion that nothing it carried moved the request to another portal, another plane of
    // this process or another service.
    let stays_under = ::http::Uri::try_from(&uri)
        .map(|rendered| rendered.path().starts_with(&prefix))
        .unwrap_or(false);
    if !stays_under {
        return Addressed::Refused(FORBIDDEN, FORBIDDEN_MESSAGE);
    }
    Addressed::At {
        uri,
        host: SERVICE_ENDPOINT.to_string(),
    }
}
