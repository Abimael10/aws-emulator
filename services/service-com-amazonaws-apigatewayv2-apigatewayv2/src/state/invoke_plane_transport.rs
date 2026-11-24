// SPDX-License-Identifier: CC0-1.0

//! The TRANSPORT an HTTP api's data plane is PUBLISHED on: the core-owned socket this
//! emulator answers an api's `ApiEndpoint` at.
//!
//! `Api$ApiEndpoint` is "the URI of the API … the stage name is typically appended to this
//! URI to form a complete path to a deployed API stage" — an address a CLIENT DIALS with
//! whatever HTTP library it has, not a label it reads. What such a client takes out of the
//! answer is the body AND the fields that say what the body IS, the media type under the name
//! `Content-Type` among them; a client that reads a response's field names as RECEIVED finds
//! a field only under the spelling that was written on the wire.
//!
//! WHO WRITES THAT NAME is the transport, not the handler. A response's field names are held
//! normalised to lower case by the time any handler of this emulator has stated them, so what
//! reaches a client is whatever the HTTP/1.1 encoder serving the connection writes — and the
//! encoder behind this emulator's own front door writes those normalised bytes. An api whose
//! clients are arbitrary HTTP clients therefore cannot be published there.
//!
//! So an HTTP api's data plane is published on a socket of this service's own, which the CORE
//! owns ([`::emulator_server::Listeners::tcp_plaintext`] — the same bind interface
//! (`EMULATOR_BIND_ADDRESS`, loopback unless the operator says otherwise), port range,
//! connection caps, idle timeout and teardown as every other data plane this emulator
//! publishes), served by an HTTP/1.1 server this service configures
//! ([`crate::service::invoke_plane_socket`]) with the encoder's canonical field-name spelling
//! set. A client handed an `ApiEndpoint` on that address reads the media type, the version and
//! every other field of the answer under the name its specification states.
//!
//! ONE socket serves every HTTP api of the account, because the api is named by the PATH: the
//! endpoint announced on it carries the api id behind the reserved data-plane marker
//! ([`crate::state::LOCAL_ENDPOINT_PREFIX`]), the same addressing the front door serves, so
//! every reader of an announced URL — this emulator's own data plane included — places the api
//! exactly as it did before. The socket is opened by the first api create that needs one
//! (there is nothing to serve before then) and is never re-opened, so a URL a client is holding
//! keeps naming the address that serves it; its life is the store's, and dropping the store
//! closes the port.
//!
//! The api's OWN `execute-api` authority is served there as well, because that is the
//! addressing `Api$ApiEndpoint` documents: a request arriving on this socket under the host
//! `{api-id}.execute-api.{region}.{dnsSuffix}` is placed on the api its leading label names
//! ([`crate::service::invoke_plane_socket`]), exactly as this emulator's front door places one
//! addressed host-style. What the endpoint ANNOUNCES is still the address above, because an
//! announced address is one the caller dials and nothing on this machine resolves that name.

use std::sync::Arc;

use ::emulator_server::ResourceListener;

/// The `host:port` a core listener is addressed by — re-exported from the one registry every
/// kind of core-owned socket of this service shares, so a caller of this module reads an
/// address the same way the WEBSOCKET transport does.
pub use super::listener_registry::authority_of;

/// The key the ONE socket of this plane is registered under: the plane is the account's, not
/// one api's, so the registry that keys a socket by its record holds this one under a name of
/// the plane itself.
const PLANE_KEY: &str = "http-api-data-plane";

/// The socket this service publishes every HTTP api's data plane on.
///
/// A field of [`crate::state::ComAmazonawsApigatewayv2Apigatewayv2State`] rather than a store
/// of its own, for the reason the WebSocket transport is one
/// ([`crate::state::websocket_transport`]): the records that say which apis exist and the
/// address those apis announce are read under ONE lock and cannot drift apart.
#[derive(Debug, Default)]
pub struct InvokePlaneTransport {
    /// The listener itself, held in the registry both other kinds of core-owned socket of this
    /// service are kept in ([`super::listener_registry`]). Holding the handle is what keeps the
    /// port open.
    listeners: super::listener_registry::ListenerRegistry,
}

impl InvokePlaneTransport {
    /// The `host:port` this plane is published at, or `None` while no socket has been opened
    /// for it.
    pub fn published(&self) -> Option<String> {
        self.listeners.authority(PLANE_KEY)
    }

    /// Remember the listener this plane is published on, and answer the `host:port` it landed
    /// on — the authority every api's `ApiEndpoint` is then announced over.
    pub fn remember_listener(&mut self, listener: Arc<ResourceListener>) -> String {
        self.listeners.remember(PLANE_KEY, listener)
    }
}

/// The `ApiEndpoint` an HTTP api announces on the socket this plane is published at: the
/// scheme an HTTP api is invoked over ([`crate::state::execute_api_scheme`]), the address the
/// core's listener landed on, and the api's own id behind the reserved data-plane marker.
///
/// The PATH is the one the front door serves the same api at
/// (`/_aws/execute-api/{api-id}`, [`crate::state::emulator_served_api_endpoint`]), so an
/// announced URL says which api it addresses wherever it is read — only the address it is
/// dialled at is this plane's own. The api's own `execute-api` host is served on this socket
/// too ([`crate::service::invoke_plane_socket`]); it is not ANNOUNCED, because no resolver on
/// the machine this emulator runs on answers for it.
pub fn announced_endpoint(authority: &str, api_id: &str) -> String {
    format!(
        "{}://{authority}/{}/{}/{api_id}",
        crate::state::execute_api_scheme(crate::state::HTTP_PROTOCOL_TYPE),
        crate::state::LOCAL_ENDPOINT_PREFIX,
        crate::state::EXECUTE_API_LABEL,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the endpoint announced on this plane's socket is the HTTP api scheme over the
    /// address the core opened, with the api behind the reserved data-plane marker — so the
    /// api id is recoverable from the path whatever address the socket landed on.
    #[test]
    fn an_announced_endpoint_names_the_api_behind_the_marker_on_the_sockets_address() {
        // The rows span the addresses a core listener reports: an IPv4 literal, a bracketed
        // IPv6 one and a name, each on a port of its own.
        let cases: &[(&str, &str, &str)] = &[
            (
                "127.0.0.1:34567",
                "abc1230000",
                "https://127.0.0.1:34567/_aws/execute-api/abc1230000",
            ),
            (
                "[::1]:8443",
                "zzz9990000",
                "https://[::1]:8443/_aws/execute-api/zzz9990000",
            ),
            (
                "localhost:9297",
                "0123456789",
                "https://localhost:9297/_aws/execute-api/0123456789",
            ),
        ];
        for (authority, api_id, expected) in cases {
            let announced = announced_endpoint(authority, api_id);
            assert_eq!(
                announced, *expected,
                "case {authority:?}: expected {expected:?}, got {announced:?}"
            );
            // And the PATH of the announced value is the one the front door serves the same
            // api at, so an announced URL says which api it addresses wherever it is read.
            let path = announced
                .split_once("://")
                .and_then(|(_scheme, rest)| rest.find('/').map(|at| rest[at..].to_string()))
                .unwrap_or_default();
            assert_eq!(
                path,
                format!(
                    "/{}/{}/{api_id}",
                    crate::state::LOCAL_ENDPOINT_PREFIX,
                    crate::state::EXECUTE_API_LABEL
                ),
                "case {authority:?}: expected the announced path to name the api, got {path:?}"
            );
        }
    }
}
