// SPDX-License-Identifier: CC0-1.0

//! WHERE a global endpoint is published — the one value every read of an endpoint reports as
//! its `EndpointUrl`.
//!
//! `EndpointUrl` is "the URL of the endpoint" (`DescribeEndpoint$EndpointUrl`), and what a
//! client does with it is DIAL it: a global endpoint is the address an application publishes
//! its events to so that the publication follows the endpoint's failover policy. So the only
//! thing the member may carry is an address THIS emulator answers on, read through the tree's
//! shared reading of a published address
//! ([`::service_common::data_plane_addressing`]) so this service states no second rule for
//! it.
//!
//! A URL under the domain Amazon Web Services publishes its own global endpoints at resolves
//! nowhere near this process or, worse, at genuine infrastructure: a client handed one leaves
//! the emulated environment carrying the credentials it holds, and its events reach no
//! endpoint of this emulator at all. A name of this emulator's own invention is no better —
//! nothing answers for a sub-name of the host this process is reached at, so a client that
//! resolves one reaches nothing either.
//!
//! ONE address therefore answers for every endpoint here, where Amazon Web Services gives
//! each endpoint a subdomain of its own: this process serves its whole surface at a single
//! endpoint, and WHICH global endpoint a publication is for is what the request itself
//! carries (`PutEvents$EndpointId`). The endpoint's own subdomain stays its `EndpointId`,
//! minted once with the endpoint ([`super::endpoint_id`]) and reported unchanged by every
//! read.
//!
//! The PORT is part of the address whenever this emulator is served on one: the published
//! value is what a client dials, and a host without the port it is served on names a port
//! nothing listens on — the same defect as publishing a host that resolves nowhere. That is
//! a deliberate exception to `EndpointUrl`'s modelled `pattern ^(https://)?[\.\-a-z0-9]+$`,
//! which admits no `:`: Amazon Web Services serves its global endpoints on the port `https`
//! is dialled at by default, so the address it publishes never carries one, while an emulator
//! bound anywhere else is reachable ONLY by dialling the port it is bound to. An address on
//! that default port therefore carries no port and is inside the modelled shape, and one on
//! any other port carries it and is dialable.
//!
//! Nothing a request CARRIES decides the host: where this emulator answers is a property of
//! the process, so the address a create publishes and the address every later read reports
//! cannot drift apart. The one request-dependent part is which of this emulator's own
//! addresses a caller reached it at, which is the shared reading's own
//! ([`addressing::reached_emulator_host_and_port`]).

use ::service_common::data_plane_addressing as addressing;

/// The URL the global endpoints of the emulator serving `ctx` are published at.
///
/// Computed per response rather than stored on the endpoint, so a record created before this
/// process was bound where it is now still reports the address it is reachable at.
#[must_use]
pub fn announced_endpoint_url(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    endpoint_url(ctx.parts(), ctx.endpoint().as_str())
}

/// [`announced_endpoint_url`] for a request's parts and an emulator endpoint already in hand:
/// the `https` URL of the address this emulator is reached at, with its port where that port
/// is not the one `https` is dialled at by default.
///
/// An emulator no request addressed directly and that is served on no port of its own (one
/// embedded in another process) publishes the loopback zone
/// ([`addressing::LOOPBACK_ZONE`]) — the interface the core binds its listeners on, and the
/// only name left that leads back here.
#[must_use]
pub fn endpoint_url(parts: &::http::request::Parts, served_base: &str) -> String {
    let (host, port) = addressing::reached_emulator_host_and_port(parts, served_base)
        .unwrap_or_else(|| {
            (
                addressing::LOOPBACK_ZONE.to_string(),
                addressing::IMPLICIT_HTTPS_PORT,
            )
        });
    // A host is case-insensitive, and the member's modelled pattern admits lowercase only.
    let host = host.to_ascii_lowercase();
    if port == addressing::IMPLICIT_HTTPS_PORT {
        return format!("https://{host}");
    }
    format!("https://{}", addressing::host_port_authority(&host, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The parts of a request that addressed `host`.
    fn parts_addressing(host: &str) -> ::http::request::Parts {
        let request = ::http::Request::builder()
            .uri("/")
            .header("host", host)
            .body(())
            .expect("the test request is well formed");
        request.into_parts().0
    }

    /// RULE: the published URL is the `https` address this emulator is reached at — the host
    /// together with the port a client has to dial — and never a host it does not answer on.
    ///
    /// The rows span the forms an emulator endpoint takes: a loopback literal with a port, a
    /// name with a port, an endpoint served on the port `https` defaults to, a bracketed IPv6
    /// bind, an uppercase host, and the base of an emulator served on no port at all.
    #[test]
    fn the_published_url_is_the_address_this_emulator_is_reached_at() {
        // (case, the base this emulator is served at, the URL it publishes)
        let rows: &[(&str, &str, &str)] = &[
            (
                "a loopback literal with a port",
                "https://127.0.0.1:9297",
                "https://127.0.0.1:9297",
            ),
            (
                "a name with a port",
                "https://emulator-host:39355",
                "https://emulator-host:39355",
            ),
            (
                "the port https defaults to",
                "https://emulator-host:443",
                "https://emulator-host",
            ),
            ("no port at all", "https://emulator-host", "https://emulator-host"),
            ("a bracketed IPv6 bind", "https://[::1]:8443", "https://[::1]:8443"),
            ("an uppercase host", "https://Emulator-Host:8443", "https://emulator-host:8443"),
            (
                "an emulator served on no port",
                ::smithy_server_runtime::UNSERVED_URL_BASE,
                "https://localhost",
            ),
        ];
        // A request addressed to the service's own AWS host names no address of this
        // emulator, so every row is answered from the base this process is served at.
        let parts = parts_addressing("events.us-west-2.amazonaws.com");
        for (case, served_base, want) in rows {
            let published = endpoint_url(&parts, served_base);
            assert_eq!(
                published, *want,
                "case {case:?}: expected {want:?}, got {published:?}"
            );
            assert!(
                !published.contains("amazonaws.com"),
                "case {case:?}: a published URL names no Amazon Web Services host, \
                 got {published:?}"
            );
        }
    }

    /// RULE: a caller that reached this emulator's OWN endpoint is published the address it
    /// reached it at, so the URL it is handed is one it is already talking to.
    #[test]
    fn a_caller_that_reached_this_emulator_is_published_that_address() {
        // (case, the host the request addressed, the URL it is published)
        for (case, addressed, want) in [
            ("a loopback port", "127.0.0.1:9297", "https://127.0.0.1:9297"),
            ("the loopback name", "localhost:9297", "https://localhost:9297"),
            ("the default https port", "127.0.0.1:443", "https://127.0.0.1"),
        ] {
            let parts = parts_addressing(addressed);
            let published = endpoint_url(&parts, ::smithy_server_runtime::UNSERVED_URL_BASE);
            assert_eq!(
                published, want,
                "case {case:?}: expected {want:?}, got {published:?}"
            );
        }
    }
}
