// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS an environment's Apache Airflow web server is PUBLISHED at: the one place
//! this service builds it, for every member that carries it.
//!
//! `Environment$WebserverUrl` is "the Apache Airflow *web server* HOST NAME for the Amazon
//! MWAA environment", and the `WebServerHostname` member `CreateCliToken` and
//! `CreateWebLoginToken` report beside a minted token is "the Airflow web server hostname
//! for the environment". Both name the SAME web server, and neither is a label a caller
//! reads back and puts away: the documented thing to do with it is to DIAL that web server
//! — open the Apache Airflow UI at `https://<address>/`, or `POST
//! https://<address>/aws_mwaa/cli` with a minted CLI token as a bearer credential. So one
//! address is published for an environment, and every member that carries it carries that
//! same value.
//!
//! # No scheme
//!
//! What is published is an ADDRESS, not a URL: a host (with the port, where one has to be
//! dialled) and nothing else. That is what the member documentation says — a host name —
//! and what the recorded responses carry, and it is what makes the value composable, since
//! a client builds `https://<address>/<path>` from it. The `WebserverUrl` SHAPE carries a
//! `@pattern ^https://.+$`, which is the one signal that disagrees; a value satisfying it
//! would make `https://{WebserverUrl}/…` — the composition every client performs — a URL
//! with two schemes in it, and the documentation and the recordings are what this follows.
//!
//! # The address is this emulator's own
//!
//! The requirement is RELATIONAL, not literal: whatever address the responder is reachable
//! at is the address it must publish. This emulator serves this service's whole surface —
//! the environment data endpoint among it, and the Apache Airflow CLI endpoint of every
//! environment ([`crate::service::airflow_cli`]) — at the ONE endpoint
//! [`::smithy_server_runtime::OperationContext::endpoint`] reports ("the base URL a later
//! request addresses… Callers send normally signed requests to it"), so that endpoint's
//! AUTHORITY is the address an environment's web server answers at and the address its
//! record publishes. A host in Amazon MWAA's own zone resolves to nothing on the machine
//! this emulator runs on, and a client handed one either fails to resolve it or leaves the
//! emulator for public AWS with a secret this emulator minted.
//!
//! The PORT is part of that address whenever the emulator is served on one: the published
//! value is what a client dials, and a host without the port it is served on names a port
//! nothing listens on — which is the same defect as publishing a host that resolves
//! nowhere. The authority is read with the tree's shared reading of a published address
//! (`service_common::data_plane_addressing`), so this service writes no second rule for
//! what an authority is.

/// The address an environment's Apache Airflow web server is published at: the endpoint
/// THIS emulator serves this service at, which is where every request to that web server is
/// answered.
///
/// Stored on the environment's record by the create, so every later read of that
/// environment reports the address this emulator answers it at and no reader computes a
/// second answer of its own.
pub fn webserver_url(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    published_webserver_url(ctx.endpoint().as_str())
}

/// [`webserver_url`] for an emulator endpoint already in hand (`scheme://host[:port]`),
/// which is the reading itself: that endpoint's authority, and nothing of its scheme or its
/// path.
///
/// The authority is read with the tree's shared reading of a URL's authority, so a value
/// carrying no scheme at all is an authority already and is published as one.
pub fn published_webserver_url(emulator_endpoint: &str) -> String {
    ::service_common::data_plane_addressing::url_authority(emulator_endpoint)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The address a token operation reports beside a minted token, read off the address the
/// environment's record publishes: the SAME value, because both members name the same web
/// server.
///
/// Read off the stored address rather than computed again, which is what keeps the host a
/// caller is handed with a token equal to the address `GetEnvironment` reports for that
/// environment. A stored value that carries a scheme (one an older record kept) still
/// publishes an authority.
pub fn webserver_hostname(webserver_url: &str) -> String {
    published_webserver_url(webserver_url)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the published address is the AUTHORITY of the endpoint this emulator is
    /// reached at — the host together with the port a client has to dial — with no scheme
    /// and no path, whichever form that endpoint takes.
    #[test]
    fn the_published_address_is_the_authority_this_emulator_is_reached_at() {
        for (case, endpoint, expected) in [
            ("a served loopback port", "https://127.0.0.1:9297", "127.0.0.1:9297"),
            ("a loopback name", "https://localhost:443", "localhost:443"),
            ("an IPv6 endpoint", "https://[::1]:9297", "[::1]:9297"),
            ("a routable bind", "https://10.0.0.7:9297", "10.0.0.7:9297"),
            (
                "an emulator served on no port",
                ::smithy_server_runtime::UNSERVED_URL_BASE,
                "emulator.invalid",
            ),
            ("a trailing slash", "https://127.0.0.1:9297/", "127.0.0.1:9297"),
            ("a path beneath it", "https://127.0.0.1:9297/airflow", "127.0.0.1:9297"),
            ("an authority alone", "127.0.0.1:9297", "127.0.0.1:9297"),
        ] {
            let published = published_webserver_url(endpoint);
            assert_eq!(
                published, expected,
                "case {case:?}: expected {expected:?}, got {published:?}"
            );
            assert!(
                !published.contains("://"),
                "case {case:?}: a published address carries no scheme, got {published:?}"
            );
        }
    }

    /// RULE: the host member a token operation reports is the address the record publishes,
    /// value for value — and reads an authority out of a stored value that carries a scheme.
    #[test]
    fn the_token_operations_report_the_published_address() {
        for (case, stored) in [
            ("a served loopback port", "127.0.0.1:9297"),
            ("an IPv6 endpoint", "[::1]:9297"),
            ("a DNS endpoint", "emulator.invalid"),
        ] {
            let reported = webserver_hostname(stored);
            assert_eq!(
                reported, stored,
                "case {case:?}: expected the stored address {stored:?}, got {reported:?}"
            );
        }
        let reported = webserver_hostname("https://127.0.0.1:9297/airflow");
        assert_eq!(
            reported, "127.0.0.1:9297",
            "case \"a stored value carrying a scheme\": expected its authority, got {reported:?}"
        );
    }
}
