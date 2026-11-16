// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS the relay URIs of an issued TURN credential set name.
//!
//! `IceServer$Uris` is not a label: a WebRTC client takes each URI verbatim and DIALS the
//! relay it names. So the address has to be one THIS emulator answers on. An address under
//! a host AWS publishes its own endpoints at is the failure to avoid: a client either leaves
//! the emulated environment for infrastructure the test has no relationship with, or
//! resolves nothing at all, while the answer claims a relay is there for the reported `Ttl`.
//!
//! Which address is this emulator's own is one question with one reading for the whole tree
//! ([`::service_common::data_plane_addressing`]), and it is CALLED here rather than written
//! again, in the order every announced address of this emulator resolves it:
//!
//! * the emulator's OWN endpoint when the request ADDRESSED it — a client that resolves
//!   nothing else, which is the client this plane is reached by, and the same address the
//!   signaling channel's own `HTTPS` and `WSS` endpoints are announced at;
//! * otherwise the address this process is SERVED at, which is what an in-process call of
//!   another service is told, so a credential set minted through one is addressed where its
//!   caller can reach it too;
//! * otherwise the address this emulator is CONFIGURED to serve on
//!   ([`configured_authority`]), for a process answering on no socket at all, where the two
//!   readings above have nothing to report.
//!
//! The emulator serves no TURN relay of its own, so what this address gives a client is the
//! emulator itself: a dial that fails locally, in the environment the call was served from,
//! instead of one that leaves it. Were a TURN listener ever served in-process, its own
//! authority ([`::service_common::data_plane_addressing::listener_authority`]) would be the
//! address to name, and only this module would change.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// The transports one relay is offered over, each as the URI scheme that carries it and the
/// `transport` parameter naming it — the `I-D.petithuguenin-behave-turn-uris` form
/// `IceServer$Uris` documents (`smithy_query(query="shape", shape="IceServer")`), in the
/// order recorded behaviour lists them: the relay in the clear over UDP, and over TLS on
/// UDP and on TCP.
const RELAY_TRANSPORTS: [(&str, &str); 3] = [("turn", "udp"), ("turns", "udp"), ("turns", "tcp")];

/// The `host:port` the relay URIs of a credential set issued for THIS request name: the
/// address this emulator is reached at, by the reading this module's header states.
#[must_use]
pub fn relay_authority(parts: &::http::request::Parts, served_base: &str) -> String {
    match ::service_common::data_plane_addressing::reached_emulator_host_and_port(
        parts,
        served_base,
    ) {
        Some((host, port)) => {
            ::service_common::data_plane_addressing::host_port_authority(&host, port)
        }
        None => configured_authority(),
    }
}

/// The relay URIs a credential set issued for THIS request is good for: one per entry of
/// [`RELAY_TRANSPORTS`], all naming [`relay_authority`].
#[must_use]
pub fn relay_uris(parts: &::http::request::Parts, served_base: &str) -> Vec<String> {
    turn_uris(&relay_authority(parts, served_base))
}

/// The relay URIs that name `authority`, in the documented URI form.
///
/// That form is `scheme ":" host [":" port] ["?transport=" transport]` and carries NO `//`
/// authority marker: `IceServer$Uris` documents the `turn:`/`turns:` URI spec, which spells
/// a relay address that way, and a WebRTC client parses the member by that grammar — so the
/// address goes on the wire as the spec writes it, not as an `https`-style URL.
#[must_use]
pub fn turn_uris(authority: &str) -> Vec<String> {
    RELAY_TRANSPORTS
        .iter()
        .map(|(scheme, transport)| format!("{scheme}:{authority}?transport={transport}"))
        .collect()
}

/// The address this emulator is CONFIGURED to serve on: the interface its listeners bind
/// (`EMULATOR_BIND_ADDRESS`, loopback unless the operator says otherwise) and the port its
/// API is served at (`EMULATOR_DATA_PORT`, the well-known default when unset), both read
/// through the core's own resolvers so this crate holds no second reading of either.
///
/// A WILDCARD bind names no single address a client could dial, so it is reported as
/// loopback — the address that reaches a process bound on every interface from the machine
/// it runs on. A bind the core cannot read at all is reported as loopback too, which is
/// where it binds.
fn configured_authority() -> String {
    let bound = ::emulator_server::bind_address_from_env().unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    let dialable = match bound {
        IpAddr::V6(address) if address.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        address if address.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        address => address,
    };
    ::service_common::data_plane_addressing::host_port_authority(
        &dialable.to_string(),
        ::emulator_server::data_port_from_env(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request carrying `host` on its `Host` header, addressed at this route.
    fn parts(host: Option<&str>, uri: &str) -> ::http::request::Parts {
        let mut builder = ::http::Request::builder().method("POST").uri(uri);
        if let Some(host) = host {
            builder = builder.header(::http::header::HOST, host);
        }
        builder
            .body(())
            .expect("the request under test is well formed")
            .into_parts()
            .0
    }

    /// The rule: the relay authority is the address this emulator is reached at — the
    /// endpoint the request addressed, else the one this process is served at, and never a
    /// host AWS publishes an endpoint at.
    ///
    /// The rows differ along what the reading generalises over: where the request carries
    /// its address (header, absolute-form URI, neither), whether that address is the
    /// emulator's own or a host naming a service, whether it states a port, and whether
    /// this process is served on one.
    #[test]
    fn the_relay_authority_is_an_address_this_emulator_is_reached_at() {
        let served = "https://127.0.0.1:9443";
        let unserved = ::smithy_server_runtime::UNSERVED_URL_BASE;
        let configured = configured_authority();
        let cases: [(&str, Option<&str>, &str, &str, String); 6] = [
            (
                "the loopback endpoint the request addressed, with its port",
                Some("127.0.0.1:9297"),
                "/v1/get-ice-server-config",
                served,
                "127.0.0.1:9297".to_string(),
            ),
            (
                "a loopback name without a port is dialled on the https port",
                Some("localhost"),
                "/v1/get-ice-server-config",
                served,
                "localhost:443".to_string(),
            ),
            (
                "an absolute-form URI carries the address when no header does",
                None,
                "https://127.0.0.1:9443/v1/get-ice-server-config",
                served,
                "127.0.0.1:9443".to_string(),
            ),
            (
                "a host naming a service is AWS's own, so the served address answers",
                Some("kinesisvideo.us-west-2.amazonaws.com"),
                "/v1/get-ice-server-config",
                served,
                "127.0.0.1:9443".to_string(),
            ),
            (
                "a bracketed IPv6 endpoint keeps its brackets",
                Some("[::1]:9297"),
                "/v1/get-ice-server-config",
                served,
                "[::1]:9297".to_string(),
            ),
            (
                "neither an own endpoint nor a served port: the configured address",
                Some("kinesisvideo.us-west-2.amazonaws.com"),
                "/v1/get-ice-server-config",
                unserved,
                configured.clone(),
            ),
        ];
        for (case, host, uri, base, expected) in cases {
            let resolved = relay_authority(&parts(host, uri), base);
            assert_eq!(
                resolved, expected,
                "case {case:?}: expected the relay authority {expected:?}, got {resolved:?}"
            );
            assert!(
                !::service_common::data_plane_addressing::addresses_an_aws_endpoint(&resolved),
                "case {case:?}: the relay authority must not name an AWS endpoint, got \
                 {resolved:?}"
            );
        }
    }

    /// The URI form: one `turn:`/`turns:` URI per offered transport, each naming the relay
    /// authority and the transport it is reached over.
    #[test]
    fn every_relay_uri_names_the_authority_in_the_documented_form() {
        // The rows differ along the authority's form: a loopback address with a port, a
        // name without one, and a bracketed IPv6 literal.
        for authority in ["127.0.0.1:9297", "localhost:443", "[::1]:9297"] {
            let uris = turn_uris(authority);
            let expected: Vec<String> = RELAY_TRANSPORTS
                .iter()
                .map(|(scheme, transport)| format!("{scheme}:{authority}?transport={transport}"))
                .collect();
            assert_eq!(
                uris, expected,
                "case {authority:?}: expected {expected:?}, got {uris:?}"
            );
        }
    }
}
