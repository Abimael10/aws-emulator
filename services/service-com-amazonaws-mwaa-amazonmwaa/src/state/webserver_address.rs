// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS an environment's Apache Airflow web server is PUBLISHED at: the one place
//! this service builds it, for every member that carries it.
//!
//! `Environment$WebserverUrl` is "the Apache Airflow *web server* host name for the Amazon
//! MWAA environment", and the `WebServerHostname` member `CreateCliToken` and
//! `CreateWebLoginToken` report beside a minted token is "the Airflow web server hostname
//! for the environment". Neither is a label a caller reads back and puts away: the
//! documented thing to do with the pair is to DIAL that web server — open the Apache
//! Airflow UI at it, or `POST <host>/aws_mwaa/cli` with the minted token as a bearer
//! credential. So the address a response carries has to be an address this emulator is
//! reached at; a host in Amazon MWAA's own zone resolves to nothing on the machine this
//! emulator runs on, and a client handed one either fails to resolve it or leaves the
//! emulator for public AWS with a secret this emulator minted.
//!
//! The requirement is RELATIONAL, not literal: whatever address the responder is reachable
//! at is the address it must publish. This emulator serves this service's whole surface —
//! the environment data endpoint among it, the operations the model hosts behind the `env.`
//! host prefix ([`crate::service::env_endpoint`]) included — at the ONE endpoint
//! [`::smithy_server_runtime::OperationContext::endpoint`] reports ("the base URL a later
//! request addresses… Callers send normally signed requests to it"), so that endpoint is
//! the address an environment's web server answers at and the address its record publishes.
//! It is read through the tree's shared reading of a published address
//! (`service_common::data_plane_addressing`), so this service writes no second rule for
//! what an authority or a host is.
//!
//! # Why the two members do not carry the same text
//!
//! `WebserverUrl` is a URL (`@pattern ^https://.+$`), so it carries the endpoint whole —
//! scheme, host and, when the emulator is served on one, the PORT a client has to dial.
//! `Hostname` carries a host and nothing else (`@pattern
//! ^(([a-zA-Z0-9]|[a-zA-Z0-9][a-zA-Z0-9\-]*[a-zA-Z0-9])\.)*([A-Za-z0-9]|…)$`, `@length(1,
//! 255)`): neither a port nor the brackets of an IPv6 literal fit inside it. So the host
//! member reports the host of that same URL ([`webserver_hostname`]) and reports NOTHING
//! for an endpoint whose host the shape cannot spell — an address a member cannot carry is
//! better left absent than replaced by one nothing answers at, and the URL beside it still
//! names the address in full.

/// The bounds `Hostname` admits a value inside (`@length(1, 255)`).
const HOSTNAME_LENGTH: ::std::ops::RangeInclusive<usize> = 1..=255;

/// The URL an environment's Apache Airflow web server is published at: the endpoint THIS
/// emulator serves this service at, which is where every operation of the environment's
/// web server is answered.
///
/// Stored on the environment's record by the create, so every later read of that
/// environment reports the address this emulator answers it at and no reader computes a
/// second answer of its own.
pub fn webserver_url(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    published_webserver_url(ctx.endpoint().as_str())
}

/// [`webserver_url`] for an emulator endpoint already in hand (`scheme://host[:port]`),
/// which is the reading itself: the endpoint's authority under the scheme `WebserverUrl`'s
/// own pattern requires (`^https://.+$`), and nothing of a path.
///
/// The authority is read with the tree's shared reading of a URL's authority, so a value
/// carrying no scheme at all is an authority already and is published as one.
pub fn published_webserver_url(emulator_endpoint: &str) -> String {
    let authority = ::service_common::data_plane_addressing::url_authority(emulator_endpoint)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    format!("https://{authority}")
}

/// The `Hostname` of an environment's Apache Airflow web server: the HOST of that
/// environment's stored [`webserver_url`], with the scheme, the port and any path removed.
///
/// Reading it off the stored URL is what keeps the host `CreateCliToken` and
/// `CreateWebLoginToken` report equal to the address `GetEnvironment` reports for the same
/// environment.
///
/// `None` when that host is not a value the member can carry ([`is_hostname`]) — an IPv6
/// literal, whose brackets and colons the shape's pattern admits nowhere. The URL member
/// still names the address in full, and the token operations report no host rather than one
/// outside the modelled shape or one nothing answers at.
pub fn webserver_hostname(webserver_url: &str) -> Option<String> {
    let authority = ::service_common::data_plane_addressing::url_authority(webserver_url)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    let host = ::service_common::data_plane_addressing::host_only(authority);
    is_hostname(host).then(|| host.to_string())
}

/// Whether `host` is a value the modelled `Hostname` admits: dot-separated labels of ASCII
/// letters, digits and inner hyphens, inside `@length(1, 255)`.
///
/// That is the shape's own `@pattern` read as what it accepts — a label is non-empty, holds
/// only those characters and neither opens nor closes with a hyphen, and no trailing dot
/// leaves an empty label behind. An IPv4 literal is such a value; a bracketed IPv6 literal
/// and any `host:port` are not.
pub fn is_hostname(host: &str) -> bool {
    HOSTNAME_LENGTH.contains(&host.len())
        && host.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The published URL is the endpoint this emulator is reached at, under the scheme the
    /// member's pattern requires — whichever form that endpoint takes.
    #[test]
    fn the_published_url_is_the_endpoint_this_emulator_is_reached_at() {
        for (case, endpoint, expected) in [
            ("a served loopback port", "https://127.0.0.1:9297", "https://127.0.0.1:9297"),
            ("a loopback name", "https://localhost:443", "https://localhost:443"),
            ("an IPv6 endpoint", "https://[::1]:9297", "https://[::1]:9297"),
            ("a routable bind", "https://10.0.0.7:9297", "https://10.0.0.7:9297"),
            (
                "an emulator served on no port",
                ::smithy_server_runtime::UNSERVED_URL_BASE,
                "https://emulator.invalid",
            ),
            ("a trailing slash", "https://127.0.0.1:9297/", "https://127.0.0.1:9297"),
            ("an authority alone", "127.0.0.1:9297", "https://127.0.0.1:9297"),
        ] {
            let published = published_webserver_url(endpoint);
            assert_eq!(
                published, expected,
                "case {case:?}: expected {expected:?}, got {published:?}"
            );
            assert!(
                published.starts_with("https://") && published.len() > "https://".len(),
                "case {case:?}: WebserverUrl is @pattern ^https://.+$, got {published:?}"
            );
        }
    }

    /// The host member carries the host of that same URL — port, brackets and path off —
    /// and carries nothing where the shape has no spelling for the address.
    #[test]
    fn the_published_host_is_the_url_host_the_shape_can_carry() {
        for (case, url, expected) in [
            ("a served loopback port", "https://127.0.0.1:9297", Some("127.0.0.1")),
            ("a loopback name", "https://localhost:443", Some("localhost")),
            ("no port at all", "https://127.0.0.1", Some("127.0.0.1")),
            ("a DNS endpoint", "https://emulator.invalid", Some("emulator.invalid")),
            ("a routable bind", "https://10.0.0.7:9297", Some("10.0.0.7")),
            ("a path beneath it", "https://127.0.0.1:9297/airflow", Some("127.0.0.1")),
            ("an IPv6 literal", "https://[::1]:9297", None),
            ("a bare IPv6 literal", "https://::1", None),
        ] {
            let host = webserver_hostname(url);
            assert_eq!(
                host.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {host:?}"
            );
            if let Some(host) = host {
                assert!(
                    is_hostname(&host),
                    "case {case:?}: a reported host must satisfy the Hostname shape, got \
                     {host:?}"
                );
            }
        }
    }

    /// What the `Hostname` shape admits, spanning the character classes and the bounds its
    /// pattern and length state.
    #[test]
    fn only_a_value_the_hostname_shape_admits_is_reportable() {
        for (case, host, admitted) in [
            ("an IPv4 literal", "127.0.0.1".to_string(), true),
            ("a single label", "localhost".to_string(), true),
            ("a hyphenated label", "my-emulator-host".to_string(), true),
            ("a multi-label name", "env.c2.example.invalid".to_string(), true),
            ("the shortest value", "a".to_string(), true),
            ("the longest value", "a".repeat(255), true),
            ("one byte past the maximum", "a".repeat(256), false),
            ("the empty value", String::new(), false),
            ("a host and a port", "127.0.0.1:9297".to_string(), false),
            ("a bracketed IPv6 literal", "[::1]".to_string(), false),
            ("a trailing dot", "localhost.".to_string(), false),
            ("an empty inner label", "a..b".to_string(), false),
            ("a leading hyphen", "-host".to_string(), false),
            ("a trailing hyphen", "host-".to_string(), false),
            ("an underscore", "my_host".to_string(), false),
        ] {
            let read = is_hostname(&host);
            assert_eq!(
                read, admitted,
                "case {case:?}: expected is_hostname {admitted}, got {read}"
            );
        }
    }
}
