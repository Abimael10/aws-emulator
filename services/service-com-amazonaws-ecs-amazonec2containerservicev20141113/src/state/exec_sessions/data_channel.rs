// SPDX-License-Identifier: CC0-1.0

//! The SOCKET a session's stream is carried on, and the address a session reports for it.
//!
//! A session's `streamUrl` is "a URL to the managed agent on the container that the SSM
//! Session Manager client uses to send commands and receive output from the container": a
//! client opens it as a WebSocket and then exchanges the data channel's own messages over it.
//! The emulator's front door serves one buffered request per call and refuses a WebSocket
//! upgrade, so the channel is served on a socket the CORE opens for it
//! ([`::emulator_server::Listeners::tcp_plaintext`], bound on the emulator's own interface —
//! loopback unless the operator says otherwise — with the core owning the port, the connection
//! caps and their lifetime), and this module holds the handle its lifetime hangs off.
//!
//! ONE listener serves every session of the store, as one regional Systems Manager endpoint
//! serves every session of a Region: the connection names its session on the data-channel path
//! ([`DATA_CHANNEL_PATH_PREFIX`]) and authenticates with that session's own token, so nothing
//! about the address has to. It is opened on the first session and kept for the life of the
//! store, so an announced `streamUrl` never moves under a client.
//!
//! The announced scheme is `wss`, and that scheme is served: the connection is handed over
//! plaintext and its FIRST byte says what the client opened with — a TLS record goes to the
//! core's own TLS server and everything after it is read decrypted, anything else is read as
//! it stands ([`::service_common::in_band_tls`]). So one address serves both the `wss://` a
//! client of AWS dials and the `ws://` a client that does not verify a self-signed certificate
//! dials, and this crate never touches a key or a certificate.

use crate::state::*;

/// The scheme a session's `streamUrl` carries: the secure WebSocket scheme a Session Manager
/// client dials.
pub const DATA_CHANNEL_SCHEME: &str = "wss";

/// The path a session's data channel is served at, under which the session's own id stands —
/// the path the Systems Manager data channel of a session is addressed by.
pub const DATA_CHANNEL_PATH_PREFIX: &str = "/v1/data-channel/";

/// The role a client opens the channel in, which it carries as the query of the `streamUrl`:
/// a Session Manager client both sends and receives on one connection.
pub const DATA_CHANNEL_ROLE: &str = "publish_subscribe";

/// The socket every session's data channel is served on, and the address it landed on.
///
/// The store OWNS the listener: dropping it closes the port and severs every connection on it,
/// so it must outlive every `streamUrl` handed out — which is the whole life of the store.
pub struct ExecDataChannelEndpoint {
    /// `host:port`, the authority the announced `wss://` URL carries.
    authority: String,
    /// Held only to keep the port open; the core owns the socket and its lifetime.
    _listener: ::emulator_server::ResourceListener,
}

/// Written by hand rather than derived: a [`::emulator_server::ResourceListener`] is a live
/// socket and carries no `Debug`. What is diagnosable about the endpoint is the address it
/// announced.
impl ::std::fmt::Debug for ExecDataChannelEndpoint {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("ExecDataChannelEndpoint")
            .field("authority", &self.authority)
            .finish()
    }
}

impl ExecDataChannelEndpoint {
    /// The `host:port` this channel is dialled at.
    #[must_use]
    pub fn authority(&self) -> &str {
        &self.authority
    }
}

/// The `host:port` every session of this store's data channel is reached at, opening the
/// socket on first use.
///
/// Idempotent: the first listener opened is kept, so the value is the same for every session
/// and for as long as the store lives — a client can key a session on it. `None` when this
/// service was mounted on an engine with no listener surface or no store, or when the core
/// could not bind: the caller then refuses the call rather than reporting a `streamUrl`
/// nothing serves.
pub async fn ensure_exec_data_channel<Eng: Send + Sync + 'static>(
    engine: &::std::sync::Arc<Eng>,
    callback: ::smithy_server_runtime::EmbeddedEmulator,
) -> Option<String> {
    let state = service_state(&**engine)?;
    // The guard is taken and dropped before the await below: a lock held across an await
    // would make this future non-`Send`.
    if let Some(authority) = state.lock().exec_data_channel_authority() {
        return Some(authority);
    }
    let listeners = (&**engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()?
        .listeners();
    let listener = listeners
        .tcp_plaintext(crate::service::exec_data_channel::SessionSocket::new(
            ::std::sync::Arc::clone(engine),
            callback,
            // The core's own TLS server, for a client that dialled `wss://`. A core that
            // cannot prepare one leaves this `None`, and such a client is then dropped at its
            // first byte rather than answered in the clear.
            listeners.tls_upgrader().ok(),
        ))
        .await
        .ok()?;
    let authority = ::service_common::data_plane_addressing::listener_authority(&listener);

    let mut store = state.lock();
    // Another call may have opened one while this was binding; keep the first, so an
    // announced address never changes under a client. Dropping the loser closes its port.
    if let Some(held) = store.exec_data_channel_authority() {
        return Some(held);
    }
    store.exec.endpoint = Some(ExecDataChannelEndpoint {
        authority: authority.clone(),
        _listener: listener,
    });
    ::tracing::info!(%authority, "opened the execute-command sessions' data channel");
    Some(authority)
}

/// The `streamUrl` a session reports: the data channel of THIS emulator, carrying the
/// session's own id on the path a client reads it from and the role it opens the channel in.
#[must_use]
pub fn exec_stream_url(authority: &str, session_id: &str) -> String {
    format!(
        "{DATA_CHANNEL_SCHEME}://{authority}{DATA_CHANNEL_PATH_PREFIX}{session_id}\
         ?role={DATA_CHANNEL_ROLE}"
    )
}

/// The session id the data-channel path `path` names, and `None` for a path that is not one
/// of this channel's.
///
/// The query is not part of the path a connection names its session in, so a request line
/// carrying one still resolves the session it addresses.
#[must_use]
pub fn session_id_from_data_channel_path(path: &str) -> Option<&str> {
    let path = path.split('?').next().unwrap_or(path);
    let session_id = path.strip_prefix(DATA_CHANNEL_PATH_PREFIX)?;
    let session_id = session_id.trim_end_matches('/');
    (!session_id.is_empty() && !session_id.contains('/')).then_some(session_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The address a session reports leads back to the socket this emulator serves, and the
    /// session it names reads back off it. The rows span the authority forms a core listener
    /// lands on and the session id the URL carries.
    #[test]
    fn a_reported_stream_url_names_the_session_it_carries() {
        let rows: [(&str, &str); 3] = [
            ("127.0.0.1:38211", "ecs-execute-command-0000000000000001"),
            ("[::1]:9", "ecs-execute-command-abcdef"),
            ("localhost:65535", "ecs-execute-command-1"),
        ];
        for (authority, session_id) in rows {
            let case = format!("{authority} carrying {session_id}");
            let url = exec_stream_url(authority, session_id);
            assert!(
                url.starts_with(&format!("{DATA_CHANNEL_SCHEME}://{authority}")),
                "case {case:?}: expected the served address, got {url:?}"
            );
            let path = url.trim_start_matches(&format!("{DATA_CHANNEL_SCHEME}://{authority}"));
            assert_eq!(
                session_id_from_data_channel_path(path),
                Some(session_id),
                "case {case:?}: expected the session to read back off {path:?}"
            );
        }
    }

    /// A path that is not one of this channel's names no session: another path of the same
    /// host, the prefix with nothing under it, and a path carrying more segments than a
    /// session id.
    #[test]
    fn a_path_that_is_not_a_data_channel_names_no_session() {
        let rows: [&str; 5] = [
            "/",
            "/v1/data-channel",
            "/v1/data-channel/",
            "/v1/data-channel/one/two",
            "/control-channel/ecs-execute-command-1",
        ];
        for path in rows {
            assert!(
                session_id_from_data_channel_path(path).is_none(),
                "case {path:?}: expected no session to be named"
            );
        }
    }
}
