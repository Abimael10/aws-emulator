// SPDX-License-Identifier: CC0-1.0

//! The scheme and authority this service PUBLISHES the planes it serves itself on.
//!
//! Four members of this service carry an address a client DEREFERENCES rather than displays —
//! a record's `logoThumbnailUrl` ([`super::listing_logo`]), a review source's `sourceUrl`
//! ([`super::listing_reviews`]) and a SaaS fulfillment option's `fulfillmentUrl` and
//! `launchUrl` ([`super::saas_fulfillment`]) — and this emulator answers a fetch of all four
//! at its own front door (`crate::service`). So the one thing such an address has to be is
//! one THIS PROCESS ANSWERS ON: an address published on anything else is one a client can
//! render but never fetch, and the round trip then fails for what was announced rather than
//! for how it is routed.
//!
//! Which address that is, is a property of the PROCESS rather than of the request:
//!
//! * the endpoint this emulator is SERVED at — the data port's own published base
//!   ([`::smithy_server_runtime::OperationContext::endpoint`], "the base URL a later request
//!   addresses… Callers send normally signed requests to it"), which is the socket this
//!   process accepts requests on;
//! * an authority a request merely CARRIED is published only where it addresses that same
//!   socket ([`answers_requests_here`]), because a carried authority is whatever the client
//!   was pointed at — a forwarding front end, an endpoint of an earlier run, one of AWS's
//!   own names — and re-announcing one of those hands the next fetch an address this process
//!   does not answer on. Where it does address this socket, the HOST SPELLING the client
//!   dialled is kept (`localhost` stays `localhost`), since that is the name that client
//!   resolved to get here;
//! * for a process served on no port at all (embedded in another process, or a test) there
//!   is no address of this process to publish, so the authority the caller addressed is the
//!   only one that can lead back here, and the unserved base names it otherwise.
//!
//! Every reading of an address below is the tree's own
//! (`service_common::data_plane_addressing`) and is CALLED here rather than written again, so
//! this service cannot drift from the others on what the address this emulator answers on is.

/// The scheme a published address is dialled over: every listener of this process presents a
/// certificate for it, which is why the served base
/// ([`::smithy_server_runtime::OperationContext::endpoint`]) is always written with it.
const ANNOUNCED_SCHEME: &str = "https";

/// The base URL the pages and images this service serves are ANNOUNCED on, for the request
/// `ctx` is answering: the scheme and authority this emulator answers a fetch of them at, by
/// the reading this module's header states.
///
/// The three published planes share it, so no member of a response can be announced on an
/// authority another member of the same response is not.
pub fn announced_page_base(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    announced_base(ctx.parts(), ctx.endpoint().as_str())
}

/// [`announced_page_base`] over the two things it reads — the request's own parts and the
/// base this process is served at — so the reading is exercised without a served socket.
pub(super) fn announced_base(parts: &::http::request::Parts, served_base: &str) -> String {
    format!(
        "{ANNOUNCED_SCHEME}://{}",
        announced_authority(parts, served_base)
    )
}

/// The `host[:port]` a published address names: the authority this emulator is served at,
/// the one the request carried where that addresses the same socket, and the authority of the
/// base itself for a process served on no port.
fn announced_authority(parts: &::http::request::Parts, served_base: &str) -> String {
    use ::service_common::data_plane_addressing as addressing;
    let carried = addressing::reached_emulator_endpoint_of_parts(parts);
    let Some(served) = addressing::served_endpoint_authority(served_base) else {
        return carried.unwrap_or_else(|| {
            addressing::url_authority(served_base)
                .trim_end_matches('/')
                .to_string()
        });
    };
    match carried.filter(|carried| answers_requests_here(carried, &served)) {
        Some(carried) => carried,
        None => served,
    }
}

/// Whether the authority `carried` a request stated addresses the SOCKET this emulator is
/// served at (`served`), which is what makes it publishable as an address a fetch of this
/// service's media is answered at.
///
/// Two readings, both the tree's own:
///
/// * the PORT dialled has to be the one this process is served on ([`dialled_port`]) — a
///   different port is a different socket, which this process does not answer on however the
///   request reached it (a forwarding front end in between, an address left over from an
///   earlier run);
/// * the HOST has to be one this emulator answers that port on
///   (`service_common::data_plane_addressing::host_names_the_bound_interface`, "whether
///   `host` names the interface this emulator's listeners are BOUND on"), so a name that
///   merely reached this process through something else is not re-announced as the address of
///   the socket.
fn answers_requests_here(carried: &str, served: &str) -> bool {
    ::service_common::data_plane_addressing::host_names_the_bound_interface(carried)
        && dialled_port(carried) == dialled_port(served)
}

/// The port an authority is DIALLED on: the one it names, or the port `https` implies for an
/// authority that names none
/// (`service_common::data_plane_addressing::IMPLICIT_HTTPS_PORT`).
fn dialled_port(authority: &str) -> u16 {
    use ::service_common::data_plane_addressing as addressing;
    addressing::addressed_port(Some(authority))
        .and_then(|port| u16::try_from(port).ok())
        .unwrap_or(addressing::IMPLICIT_HTTPS_PORT)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request that carries `host` as its `Host` header, or none at all, addressed in origin
    /// form so nothing but that header states an authority.
    fn request(host: Option<&str>) -> ::http::request::Parts {
        let mut builder = ::http::Request::builder().uri("/2026-02-05/searchListings");
        if let Some(host) = host {
            builder = builder.header(::http::header::HOST, host);
        }
        builder
            .body(())
            .expect("the row is a well-formed request")
            .into_parts()
            .0
    }

    /// RULE: a published address names the authority this process is SERVED at, and an
    /// authority the request carried only where that addresses the very socket this process
    /// is served on.
    ///
    /// The rows differ along what the reading generalises over: the form of the served base
    /// (a loopback literal, a loopback NAME, a bracketed IPv6 literal, a base with a trailing
    /// separator, a process served on no port at all) and what the request carried (nothing,
    /// the served authority, another spelling of the same socket, a port this process does not
    /// serve, no port at all, and one of AWS's own endpoint hosts).
    #[test]
    fn a_published_address_names_the_socket_this_process_is_served_on() {
        let cases: &[(&str, &str, Option<&str>, &str)] = &[
            (
                "no authority carried at all",
                "https://127.0.0.1:9297",
                None,
                "https://127.0.0.1:9297",
            ),
            (
                "the served authority itself",
                "https://127.0.0.1:9297",
                Some("127.0.0.1:9297"),
                "https://127.0.0.1:9297",
            ),
            (
                "another spelling of the same socket keeps the spelling dialled",
                "https://127.0.0.1:9297",
                Some("localhost:9297"),
                "https://localhost:9297",
            ),
            (
                "a port this process does not serve",
                "https://127.0.0.1:36637",
                Some("127.0.0.1:38377"),
                "https://127.0.0.1:36637",
            ),
            (
                "an authority naming no port at all",
                "https://127.0.0.1:36637",
                Some("127.0.0.1"),
                "https://127.0.0.1:36637",
            ),
            (
                "one of AWS's own endpoint hosts",
                "https://127.0.0.1:9297",
                Some("discovery-marketplace.us-east-1.amazonaws.com"),
                "https://127.0.0.1:9297",
            ),
            (
                "a served base naming the loopback zone",
                "https://localhost:39355",
                Some("127.0.0.1:39355"),
                "https://127.0.0.1:39355",
            ),
            (
                "a bracketed IPv6 served base",
                "https://[::1]:8443",
                Some("[::1]:8443"),
                "https://[::1]:8443",
            ),
            (
                "a bracketed IPv6 request on another port",
                "https://[::1]:8443",
                Some("[::1]:9443"),
                "https://[::1]:8443",
            ),
            (
                "a served base with a trailing separator",
                "https://127.0.0.1:9297/",
                None,
                "https://127.0.0.1:9297",
            ),
            (
                "served on no port: the authority the caller addressed",
                ::smithy_server_runtime::UNSERVED_URL_BASE,
                Some("127.0.0.1:38377"),
                "https://127.0.0.1:38377",
            ),
            (
                "served on no port and no authority carried",
                ::smithy_server_runtime::UNSERVED_URL_BASE,
                None,
                "https://emulator.invalid",
            ),
        ];
        for (case, served_base, carried, expected) in cases {
            let announced = announced_base(&request(*carried), served_base);
            assert_eq!(
                &announced, expected,
                "case {case:?} (served at {served_base:?}, carrying {carried:?}): expected \
                 {expected:?}, got {announced:?}"
            );
        }
    }
}
