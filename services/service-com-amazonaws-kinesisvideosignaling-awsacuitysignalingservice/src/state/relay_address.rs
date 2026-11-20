// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS the relay URIs of an issued TURN credential set name.
//!
//! `IceServer$Uris` is not a label: a WebRTC client takes each URI verbatim and DIALS the
//! relay it names. So the address has to be one THIS emulator is SERVING the call on. An
//! address nothing here answers on is the failure to avoid — a host AWS publishes its own
//! endpoints at, or a `host:port` the request merely CARRIED (a forwarding front end, an
//! endpoint of an earlier run): a client either leaves the emulated environment for
//! infrastructure the test has no relationship with, or resolves nothing at all, while the
//! answer claims a relay is there for the reported `Ttl`.
//!
//! Which address this emulator is served at is one question with one reading for the whole
//! tree ([`::service_common::data_plane_addressing`]), and it is CALLED here rather than
//! written again:
//!
//! * the address this process is SERVED at — the base
//!   [`::smithy_server_runtime::OperationContext::endpoint`] reports, which is the data
//!   port's own published address and therefore the socket this emulator answers requests
//!   on;
//! * otherwise the address this emulator is CONFIGURED to serve on
//!   ([`configured_authority`]), for a process answering on no socket at all, where the
//!   reading above has nothing to report.
//!
//! NOTHING of a request moves the answer: where this emulator answers is a property of the
//! PROCESS, not of the host a request happened to carry. A carried host is whatever the
//! client was pointed at — a forwarding front end, an endpoint of an earlier run, one of
//! AWS's own names — and re-announcing it as a relay sends the next client to an address
//! this process does not answer on, which is exactly the failure above. So the two readings
//! below are the whole answer, and the same address is announced to every viewer of every
//! channel.
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

/// The `host:port` the relay URIs of an issued credential set name: the address this
/// emulator ANSWERS on, by the reading this module's header states — the endpoint this
/// process is served at (`served_base`, the base
/// [`::smithy_server_runtime::OperationContext::endpoint`] reports), else the one it is
/// configured to serve on.
///
/// Reported with a port in every case, because an address a client DIALS a relay at names
/// one: an authority that states none is dialled on the port `https` implies
/// ([`::service_common::data_plane_addressing::IMPLICIT_HTTPS_PORT`]).
#[must_use]
pub fn relay_authority(served_base: &str) -> String {
    use ::service_common::data_plane_addressing as addressing;
    match addressing::served_endpoint_authority(served_base) {
        Some(authority) => {
            let host = addressing::host_only(&authority).to_string();
            let port = addressing::addressed_port(Some(authority.as_str()))
                .and_then(|port| u16::try_from(port).ok())
                .unwrap_or(addressing::IMPLICIT_HTTPS_PORT);
            addressing::host_port_authority(&host, port)
        }
        None => configured_authority(),
    }
}

/// The relay URIs an issued credential set is good for: one per entry of
/// [`RELAY_TRANSPORTS`], all naming [`relay_authority`].
#[must_use]
pub fn relay_uris(served_base: &str) -> Vec<String> {
    turn_uris(&relay_authority(served_base))
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

    /// The rule: the relay authority is the address this emulator ANSWERS on — the endpoint
    /// this process is served at, else the one it is configured to serve on — whatever a
    /// request carries as its own addressing.
    ///
    /// The rows differ along what the reading generalises over: the form of the served base
    /// (a loopback literal with a port, a bracketed IPv6 literal, a name carrying no port,
    /// a trailing `/`) and a process served on no port at all.
    #[test]
    fn the_relay_authority_is_an_address_this_emulator_is_served_at() {
        let configured = configured_authority();
        let cases: [(&str, &str, String); 6] = [
            (
                "the loopback endpoint this process is served at",
                "https://127.0.0.1:9443",
                "127.0.0.1:9443".to_string(),
            ),
            (
                "an ephemeral port chosen at boot",
                "https://127.0.0.1:35713",
                "127.0.0.1:35713".to_string(),
            ),
            (
                "a bracketed IPv6 endpoint keeps its brackets",
                "https://[::1]:9443",
                "[::1]:9443".to_string(),
            ),
            (
                "a served base naming no port is dialled on the https port",
                "https://localhost",
                "localhost:443".to_string(),
            ),
            (
                "a served base with a trailing path separator",
                "https://127.0.0.1:9443/",
                "127.0.0.1:9443".to_string(),
            ),
            (
                "served on no port at all: the configured address",
                ::smithy_server_runtime::UNSERVED_URL_BASE,
                configured.clone(),
            ),
        ];
        for (case, base, expected) in cases {
            let resolved = relay_authority(base);
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
