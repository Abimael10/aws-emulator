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
//! ONE socket serves every HTTP api of the account, because the api is named by the HOST: the
//! endpoint announced on it is the api's own `execute-api` host — the api id, the
//! `execute-api` label and the api's Region as its three leading labels
//! ([`crate::state::execute_api_host_in_zone`]) — which is the addressing `Api$ApiEndpoint`
//! documents, and a request arriving here under it is placed on the api its leading label
//! names ([`crate::service::invoke_plane_socket`]), exactly as this emulator's front door
//! places one addressed host-style. The socket is opened by the first api create that needs
//! one (there is nothing to serve before then) and is never re-opened, so a URL a client is
//! holding keeps naming the address that serves it; its life is the store's, and dropping the
//! store closes the port.
//!
//! The ZONE that host is written in is the one this emulator may announce a name of its own in
//! (`service_common::data_plane_addressing::announced_zone_of_authority`), which for the
//! address a core listener lands on is the loopback zone RFC 6761 reserves for the interface
//! the core binds on — so the name a caller is given leads back to this process rather than to
//! anything of AWS's own, and the PORT it carries is the one this socket landed on. The api is
//! served at the reserved data-plane path on this same address too
//! ([`crate::state::LOCAL_ENDPOINT_PREFIX`], the addressing the front door serves), so a
//! client that rewrote an endpoint into a path reaches the same api.

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
/// scheme an HTTP api is invoked over ([`crate::state::execute_api_scheme`]) over the api's
/// own `execute-api` host ([`announced_authority`]), and no path of its own.
///
/// That is the value `Api$ApiEndpoint` documents — "The URI of the API, of the form
/// `{api-id}.execute-api.{region}.amazonaws.com`. The stage name is typically appended to this
/// URI to form a complete path to a deployed API stage" — so a client recovers the api's id
/// from the host's first label and its Region from the label behind `execute-api`, and
/// appending `/{stage}/{route-path}` to it is the complete invoke path. The host is written in
/// a zone that leads back to this process and carries the port this socket landed on, and this
/// socket serves it ([`crate::service::invoke_plane_socket`]), so the address a caller is given
/// is the address the api answers at.
pub fn announced_endpoint(authority: &str, api_id: &str, region: &str) -> String {
    format!(
        "{}://{}",
        crate::state::execute_api_scheme(crate::state::HTTP_PROTOCOL_TYPE),
        announced_authority(authority, api_id, region),
    )
}

/// The AUTHORITY an api is announced at on this plane's socket: the api's own `execute-api`
/// host ([`crate::state::execute_api_host_in_zone`]) in the zone this emulator may announce a
/// name of its own in, carrying the PORT the socket landed on.
///
/// `authority` is the `host:port` the core's listener reports ([`authority_of`]). Its HOST
/// decides the zone, through the tree's one reading of that question
/// (`service_common::data_plane_addressing::announced_zone_of_authority`): the loopback zone
/// RFC 6761 reserves for the interface the core binds on, which is the zone the ADDRESS
/// LITERAL a core listener lands on names. Its PORT is carried over unchanged, because that is
/// the port this plane answers on — an authority that names none is announced none, and the
/// `https` an HTTP api is invoked over is dialled on its implicit port there.
///
/// The api's id and its Region are folded to one DNS label each
/// (`service_common::data_plane_addressing::dns_label`) before they are written into the name:
/// the Region is read off the credential scope of the request that minted the api, so folding
/// it is what keeps a value carrying a host or path delimiter from making the announced address
/// name an authority the caller chose.
pub fn announced_authority(authority: &str, api_id: &str, region: &str) -> String {
    use ::service_common::data_plane_addressing as addressing;

    let host = crate::state::execute_api_host_in_zone(
        &addressing::dns_label(api_id),
        &addressing::dns_label(region),
        &addressing::announced_zone_of_authority(authority),
    );
    match addressing::addressed_port(Some(authority)).and_then(|port| u16::try_from(port).ok()) {
        Some(port) => addressing::host_port_authority(&host, port),
        None => host,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the endpoint announced on this plane's socket is the HTTP api scheme over the
    /// api's own `execute-api` host — its id, the endpoint label and its Region as the three
    /// leading labels — in a zone that leads back to this process, on the port the socket
    /// landed on, and with no path of its own.
    #[test]
    fn an_announced_endpoint_names_the_api_in_the_leading_labels_of_its_own_host() {
        // The rows span what the reading generalises over: the address a core listener
        // reports (an IPv4 literal, a bracketed IPv6 one, a loopback name, an operator's own
        // name, and one carrying no port at all), the api id and the Region.
        let cases: &[(&str, &str, &str, &str)] = &[
            (
                "127.0.0.1:34567",
                "abc1230000",
                "us-west-2",
                "https://abc1230000.execute-api.us-west-2.localhost:34567",
            ),
            (
                "[::1]:8443",
                "zzz9990000",
                "eu-central-1",
                "https://zzz9990000.execute-api.eu-central-1.localhost:8443",
            ),
            (
                "localhost:9297",
                "0123456789",
                "ap-southeast-2",
                "https://0123456789.execute-api.ap-southeast-2.localhost:9297",
            ),
            (
                "127.0.0.42:41",
                "mixedcase00",
                "eu-north-1",
                "https://mixedcase00.execute-api.eu-north-1.localhost:41",
            ),
            (
                "127.0.0.1",
                "abc1230000",
                "us-east-1",
                "https://abc1230000.execute-api.us-east-1.localhost",
            ),
        ];
        for (authority, api_id, region, expected) in cases {
            let announced = announced_endpoint(authority, api_id, region);
            assert_eq!(
                announced, *expected,
                "case {authority:?}: expected {expected:?}, got {announced:?}"
            );
            // And the announced value carries no path at all, so appending the stage name to
            // it is the complete path to a deployed stage, which is what the member it fills
            // documents.
            let path = announced
                .split_once("://")
                .and_then(|(_scheme, rest)| rest.find('/').map(|at| rest[at..].to_string()))
                .unwrap_or_default();
            assert_eq!(
                path, "",
                "case {authority:?}: expected an announced host with no path, got {path:?}"
            );
            // The api and its Region are recoverable from the three leading labels of the
            // host, which is the whole point of the form.
            let host = announced
                .split_once("://")
                .map(|(_scheme, rest)| rest)
                .unwrap_or(&announced);
            let labels: Vec<&str> = host.split(':').next().unwrap_or(host).split('.').collect();
            assert_eq!(
                labels.iter().take(3).copied().collect::<Vec<&str>>(),
                vec![*api_id, crate::state::EXECUTE_API_LABEL, *region],
                "case {authority:?}: expected the api, the endpoint label and the Region as \
                 the leading labels, got {labels:?}"
            );
        }
    }
}
