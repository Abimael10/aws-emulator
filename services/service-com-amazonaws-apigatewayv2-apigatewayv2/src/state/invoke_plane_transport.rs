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
//! endpoint announced on it carries the api's own id behind the reserved data-plane marker
//! (`/_aws/execute-api/{api-id}`), which is the addressing this emulator's front door serves
//! an api's data plane at, so a request arriving here is placed on the api its path names and
//! answered by the same records
//! ([`crate::service::invoke_plane_socket`]). That socket also answers for the api's own
//! `execute-api` host for a client that resolves it ([`crate::state::execute_api_host_in_zone`],
//! the addressing `Api$ApiEndpoint` documents), placing such a request on the api its leading
//! label names, exactly as the front door does. The socket is opened by the first api create
//! that needs one (there is nothing to serve before then) and is never re-opened, so a URL a
//! client is holding keeps naming the address that serves it; its life is the store's, and
//! dropping the store closes the port.
//!
//! The address announced is not a NAME: what is published is the `host:port` the core's
//! listener landed on — an address literal on the interface the core binds — with the api's
//! own id behind the reserved data-plane path ([`crate::state::LOCAL_ENDPOINT_PREFIX`], the
//! addressing this socket and this emulator's front door both serve). A client DIALS the
//! published value as it stands, and nothing on the machine this emulator runs on resolves a
//! name this service invented, so a name of its own is announced nowhere — exactly as this
//! service's other core-owned addresses are published
//! ([`super::websocket_transport::announced_endpoint`] for a WebSocket api,
//! `crate::state::portal_preview_url` for a portal's site).

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
/// address that socket landed on, with the api's own id behind the reserved data-plane path
/// ([`crate::state::api_data_plane_endpoint`], the one place that spells an announced URI over
/// an address of this emulator).
///
/// `Api$ApiEndpoint` is "The URI of the API, of the form
/// `{api-id}.execute-api.{region}.amazonaws.com`. The stage name is typically appended to this
/// URI to form a complete path to a deployed API stage" — a value whose whole purpose is that
/// a client DIALS it. AWS's own form is a NAME, and AWS answers for it; a name of this
/// emulator's own minting is answered for by no resolver on the machine it runs on, so what is
/// published is the socket's own address and the api is named in the path under it — the path
/// this socket serves ([`crate::service::invoke_plane_socket`]) and the one this emulator's
/// front door serves the same api at. Appending `/{stage}/{route-path}` to the value is still
/// the complete invoke path.
///
/// `authority` is the `host:port` the core's listener reports ([`authority_of`]), carried over
/// unchanged: the port is the one this plane answers on, and the host is the address the core
/// bound, so the value is dialable as it stands.
pub fn announced_endpoint(authority: &str, api_id: &str) -> String {
    crate::state::api_data_plane_endpoint(authority, api_id, crate::state::HTTP_PROTOCOL_TYPE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the endpoint announced on this plane's socket is the HTTP api scheme over the
    /// ADDRESS that socket landed on — a `host[:port]` a client dials as given, never a name
    /// this emulator minted — with the api named behind the reserved data-plane path.
    #[test]
    fn an_announced_endpoint_names_the_api_under_the_address_its_socket_landed_on() {
        // The rows span what the reading generalises over: the address a core listener
        // reports (an IPv4 literal, a bracketed IPv6 one, a loopback name, an operator's own
        // name, and one carrying no port at all) and the api id.
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
            (
                "127.0.0.42:41",
                "mixedcase00",
                "https://127.0.0.42:41/_aws/execute-api/mixedcase00",
            ),
            (
                "127.0.0.1",
                "abc1230000",
                "https://127.0.0.1/_aws/execute-api/abc1230000",
            ),
        ];
        for (authority, api_id, expected) in cases {
            let announced = announced_endpoint(authority, api_id);
            assert_eq!(
                announced, *expected,
                "case {authority:?}: expected {expected:?}, got {announced:?}"
            );
            // The authority a client dials is the address the socket landed on, carried over
            // unchanged: a published address is dialled AS GIVEN, so no label of it is this
            // service's to mint.
            let dialled = announced
                .split_once("://")
                .map(|(_scheme, rest)| rest.split('/').next().unwrap_or_default())
                .unwrap_or_default();
            assert_eq!(
                dialled, *authority,
                "case {authority:?}: expected the socket's own address, got {dialled:?}"
            );
            // And the api is recoverable from the path, which is where this form names it, so
            // appending the stage name reaches a deployed stage of THIS api.
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
                "case {authority:?}: expected the api behind the reserved data-plane path, got \
                 {path:?}"
            );
        }
    }
}
