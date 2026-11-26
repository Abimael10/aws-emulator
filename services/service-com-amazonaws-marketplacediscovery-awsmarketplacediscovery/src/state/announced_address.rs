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
//! Which address that is, is read off the ANSWERING REQUEST first, and off this process
//! second:
//!
//! * the authority that request ARRIVED on is, by construction, an address that reaches this
//!   process: the client dialled it and got here, so a fetch it sends there arrives the same
//!   way — through whatever stands between the two, a published container port or a
//!   forwarding front end included, whose address is then the only one that leads back to it.
//!   A front end that re-addresses the request to this process's own socket replaces the
//!   `Host` the client wrote, so the address the client dialled is read out of what that
//!   front end STATES about it, ahead of what the request states itself
//!   (`service_common::data_plane_addressing::forwarded_emulator_endpoint`, consulted by the
//!   reading below). The HOST SPELLING the client dialled is kept
//!   (`localhost` stays `localhost`), since that is the name it resolved to get here. It is
//!   published only where it CAN lead back ([`leads_back_here`]);
//! * the endpoint this emulator is SERVED at — the data port's own published base
//!   ([`::smithy_server_runtime::OperationContext::endpoint`], "the base URL a later request
//!   addresses… Callers send normally signed requests to it") — stands behind it, for the
//!   request that states no authority a fetch can lead back here by: one addressed to a host
//!   of AWS's own, which is an in-process call of another service, and one naming no port
//!   where this process is not served on the port `https` implies;
//! * for a process served on no port at all (embedded in another process, or a test) there
//!   is no address of this process to publish, so the authority the caller addressed is the
//!   only one that can lead back here, and the unserved base names it otherwise.
//!
//! That ordering — the endpoint a request addressed, else the one this process is served at —
//! is the tree's own for every address published for a plane served at the front door
//! (`service_common::data_plane_addressing::reached_emulator_host_and_port`), so this service
//! announces its planes where every other service announces its own.
//!
//! Every reading of an address below is the tree's own
//! (`service_common::data_plane_addressing`) and is CALLED here rather than written again, so
//! this service cannot drift from the others on what the address this emulator answers on is.

/// The scheme an address of THIS PROCESS is dialled over: every listener of this process
/// presents a certificate for it, which is why the served base
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
    let authority = announced_authority(parts, served_base);
    format!("{}://{authority}", announced_scheme(parts, &authority))
}

/// The scheme the published address `announced` is dialled over.
///
/// For an address of a FORWARDING FRONT END — the one case where what is published is not an
/// address of this process — it is the scheme that front end states the client dialled it over
/// (`service_common::data_plane_addressing::forwarded_scheme`): the client reaches this
/// process through it, so the scheme it presents is the scheme that address is dialled over,
/// and this process's own has nothing to say about it. For every address of this process
/// itself it is [`ANNOUNCED_SCHEME`], and so it is for a front end that states no scheme.
fn announced_scheme(parts: &::http::request::Parts, announced: &str) -> &'static str {
    use ::service_common::data_plane_addressing as addressing;
    let stated_by_the_front_end = addressing::forwarded_authority(&parts.headers)
        .is_some_and(|forwarded| forwarded == announced);
    match stated_by_the_front_end {
        true => addressing::forwarded_scheme(&parts.headers).unwrap_or(ANNOUNCED_SCHEME),
        false => ANNOUNCED_SCHEME,
    }
}

/// The `host[:port]` a published address names: the authority the answering request arrived
/// on where that can lead back here, the authority this emulator is served at otherwise, and
/// the authority of the base itself for a process served on no port.
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
    match carried.filter(|carried| leads_back_here(carried, &served)) {
        Some(carried) => carried,
        None => served,
    }
}

/// Whether the authority `carried` a request stated can lead a later fetch BACK to this
/// process, which is what makes it publishable as an address this service's media is answered
/// at — rather than the base this process is served at (`served`).
///
/// Two readings, both the tree's own:
///
/// * the HOST has to be one this emulator answers on
///   (`service_common::data_plane_addressing::host_names_the_bound_interface`, "whether
///   `host` names the interface this emulator's listeners are BOUND on"), so a name that
///   merely reached this process through something that resolves it — a name of AWS's own,
///   an operator's alias — is not re-announced as an address of this emulator;
/// * the authority has to name the PORT the client dialled ([`dialled_port`]), because that
///   port is the one thing about the request that says which socket the fetch has to arrive
///   at: it need not be the port this process is SERVED on, since a published container port
///   or a forwarding front end reaches this one from another, but an authority naming NO port
///   names the port `https` implies, which this process answers on only when that is the port
///   it is served on.
fn leads_back_here(carried: &str, served: &str) -> bool {
    use ::service_common::data_plane_addressing as addressing;
    addressing::host_names_the_bound_interface(carried)
        && (addressing::addressed_port(Some(carried)).is_some()
            || dialled_port(served) == addressing::IMPLICIT_HTTPS_PORT)
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

    /// RULE: a published address names the authority the answering request ARRIVED on where
    /// that can lead a fetch back here, and the authority this process is served at where it
    /// cannot.
    ///
    /// The rows differ along what the reading generalises over: the form of the served base
    /// (a loopback literal, a loopback NAME, a bracketed IPv6 literal, a base with a trailing
    /// separator, a process served on no port at all) and what the request carried (nothing,
    /// the served authority, another spelling of the same socket, a port this process is not
    /// served on, no port at all, and one of AWS's own endpoint hosts).
    #[test]
    fn a_published_address_names_the_authority_the_request_arrived_on() {
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
                "a port this process is not served on, which reached it all the same",
                "https://127.0.0.1:36637",
                Some("127.0.0.1:38377"),
                "https://127.0.0.1:38377",
            ),
            (
                "an authority naming no port at all",
                "https://127.0.0.1:36637",
                Some("127.0.0.1"),
                "https://127.0.0.1:36637",
            ),
            (
                "an authority naming no port, this process served on the implicit one",
                "https://127.0.0.1:443",
                Some("127.0.0.1"),
                "https://127.0.0.1",
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
                "https://[::1]:9443",
            ),
            (
                "a loopback name on a port this process is not served on",
                "https://127.0.0.1:9297",
                Some("localhost:39355"),
                "https://localhost:39355",
            ),
            (
                "one of AWS's own endpoint hosts carrying the served port",
                "https://127.0.0.1:9297",
                Some("discovery-marketplace.us-east-1.amazonaws.com:9297"),
                "https://127.0.0.1:9297",
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

    /// One row of the forwarded-address table: what the row is, what a front end stated
    /// about the address the client dialled it at, and the base the published addresses must
    /// then carry.
    type ForwardedRow = (&'static str, &'static [(&'static str, &'static str)], &'static str);

    /// A request a FRONT END re-addressed to this process: the `Host` it wrote for this
    /// process's own socket, plus what it states about the address the client dialled IT at.
    fn forwarded_request(host: &str, stated: &[(&str, &str)]) -> ::http::request::Parts {
        let mut builder = ::http::Request::builder()
            .uri("/2026-02-05/listFulfillmentOptions")
            .header(::http::header::HOST, host);
        for (name, value) in stated {
            builder = builder.header(*name, *value);
        }
        builder
            .body(())
            .expect("the row is a well-formed request")
            .into_parts()
            .0
    }

    /// RULE: where a FRONT END states the address the client dialled it at, that address is
    /// the one the answering request arrived on — the `Host` this process was re-addressed
    /// with is the front end's own upstream, not an address the client has — so it is the one
    /// published, over the scheme that front end states; where the front end states an
    /// address that cannot lead back here, the address this process is served at is published
    /// over this process's own scheme.
    ///
    /// The rows differ along what the reading generalises over: whether the stated address
    /// names its port, states it separately or names none; a chain of hops; the scheme stated
    /// beside it; an address that cannot lead back here (one of AWS's own hosts, an address
    /// this emulator is not bound on) and a statement of a scheme alone.
    #[test]
    fn a_published_address_names_what_a_front_end_states_the_client_dialled() {
        /// The base this process is served at in every row: its own socket, which is what the
        /// front end re-addressed the request to.
        const SERVED: &str = "https://127.0.0.1:39027";
        /// What a front end re-addressing a request to that socket writes as the `Host`.
        const UPSTREAM: &str = "127.0.0.1:39027";

        let cases: &[ForwardedRow] = &[
            (
                "the address the client dialled, port and all",
                &[("x-forwarded-host", "127.0.0.1:37623")],
                "https://127.0.0.1:37623",
            ),
            (
                "that address with its port stated separately",
                &[("x-forwarded-host", "localhost"), ("x-forwarded-port", "37623")],
                "https://localhost:37623",
            ),
            (
                "a chain of front ends, the client's own address at its head",
                &[("x-forwarded-host", "localhost:37623, 127.0.0.1:39027")],
                "https://localhost:37623",
            ),
            (
                "a front end the client dialled over plaintext",
                &[
                    ("x-forwarded-host", "127.0.0.1:37623"),
                    ("x-forwarded-proto", "http"),
                ],
                "http://127.0.0.1:37623",
            ),
            (
                "a front end the client dialled over TLS",
                &[
                    ("x-forwarded-host", "127.0.0.1:37623"),
                    ("x-forwarded-proto", "https"),
                ],
                "https://127.0.0.1:37623",
            ),
            (
                "an address naming no port, where this process is not served on the implicit \
                 one",
                &[("x-forwarded-host", "127.0.0.1")],
                SERVED,
            ),
            (
                "one of AWS's own endpoint hosts",
                &[(
                    "x-forwarded-host",
                    "discovery-marketplace.us-east-1.amazonaws.com",
                )],
                SERVED,
            ),
            (
                "an address this emulator is not bound on, and its scheme with it",
                &[
                    ("x-forwarded-host", "198.51.100.7:443"),
                    ("x-forwarded-proto", "http"),
                ],
                SERVED,
            ),
            (
                "a scheme stated with no address at all",
                &[("x-forwarded-proto", "http")],
                SERVED,
            ),
        ];
        for (case, stated, expected) in cases {
            let announced = announced_base(&forwarded_request(UPSTREAM, stated), SERVED);
            assert_eq!(
                &announced, expected,
                "case {case:?} (served at {SERVED:?}, re-addressed to {UPSTREAM:?} stating \
                 {stated:?}): expected {expected:?}, got {announced:?}"
            );
        }
    }
}
