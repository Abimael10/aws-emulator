// SPDX-License-Identifier: CC0-1.0

//! The health-check members a probe carries only when it speaks HTTP semantics, and the
//! refusal a write that names one on a probe that does not earns.
//!
//! Two members of a health check are bracketed to an HTTP-side probe by their own
//! documentation (`smithy_query(query="operation",operation="CreateTargetGroup")`):
//!
//! * `HealthCheckPath` — "[HTTP/HTTPS health checks] The destination for health checks on
//!   the targets";
//! * `Matcher` — "[HTTP/HTTPS health checks] The HTTP or gRPC codes to use when checking
//!   for a successful response from a target".
//!
//! A probe that opens a connection rather than sending a request has nowhere to send a
//! path and no response code to compare, so neither member means anything to it: the
//! configuration cannot hold together, which is why it is refused as the modelled
//! `InvalidConfigurationRequest` rather than as a bad value — each value is a perfectly
//! good path or matcher, just not one such a check can carry.
//!
//! Which protocol the probe speaks is the EFFECTIVE one: what the request names, else the
//! default `HealthCheckProtocol` documents for that target group ("For Application Load
//! Balancers, the default is HTTP. For Network Load Balancers and Gateway Load Balancers,
//! the default is TCP") on a create, or what the target group already stores on a modify.
//! So a request that names a path and no protocol is judged against the protocol the
//! check it asks for would actually speak.

use super::is_http_protocol;

/// The protocol the health check of a target group whose own protocol is `protocol`
/// speaks when the request names none, from `HealthCheckProtocol`'s own documentation:
/// "For Application Load Balancers, the default is HTTP. For Network Load Balancers and
/// Gateway Load Balancers, the default is TCP" — an HTTP or HTTPS target group is an
/// Application Load Balancer's, every other protocol a Network or Gateway one's.
pub fn default_health_check_protocol(protocol: Option<&str>) -> &'static str {
    if is_http_protocol(protocol) {
        "HTTP"
    } else {
        "TCP"
    }
}

/// How Elastic Load Balancing names each HTTP-only member in the refusal: the plural noun
/// of the thing the probe would have to carry, which is what the sentence below is about.
const PATHS: &str = "paths";
const MATCHERS: &str = "matchers";

/// The message a write that names an HTTP-only member on a probe speaking
/// `health_check_protocol` is refused with.
///
/// Fixed text whose variable parts are the member the request named and the protocol the
/// probe speaks — both taken from the request, or from the target group the request is
/// changing. Recorded behaviour for the path, the member a recorded create was refused
/// over; the matcher is the same sentence about the other member of the pair, since one
/// rule refuses both.
pub fn http_only_health_check_member_message(member: &str, health_check_protocol: &str) -> String {
    format!("Health check {member} are not supported for {health_check_protocol} health checks")
}

/// Whether a probe speaking `health_check_protocol` may carry the HTTP-only members this
/// request names — `path` and `matcher` say which of them it does — and, when it may not,
/// the message the write is refused with.
///
/// `health_check_protocol` is the EFFECTIVE protocol of the probe the write would leave
/// behind (see this module's header). `None` means a probe that reports no protocol at
/// all, which is a `lambda` target group's: the load balancer invokes the function rather
/// than connecting to a port, and that check is an HTTP one, so it carries both members.
///
/// Read by every operation that WRITES a health check — the create that stores the probe a
/// target group starts with and the modify that changes it — so one reading of the rule
/// serves both.
pub fn check_http_only_health_check_members(
    health_check_protocol: Option<&str>,
    path: bool,
    matcher: bool,
) -> Result<(), String> {
    let Some(health_check_protocol) = health_check_protocol else {
        return Ok(());
    };
    if is_http_protocol(Some(health_check_protocol)) {
        return Ok(());
    }
    for (named, member) in [(path, PATHS), (matcher, MATCHERS)] {
        if named {
            return Err(http_only_health_check_member_message(
                member,
                health_check_protocol,
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default the probe takes follows the target group's own protocol, for every
    /// protocol `ProtocolEnum` names.
    #[test]
    fn the_default_probe_protocol_follows_the_target_group_protocol() {
        // (the target group's protocol, the probe protocol it defaults to)
        let rows: &[(Option<&str>, &str)] = &[
            (Some("HTTP"), "HTTP"),
            (Some("HTTPS"), "HTTP"),
            (Some("TCP"), "TCP"),
            (Some("TLS"), "TCP"),
            (Some("UDP"), "TCP"),
            (Some("TCP_UDP"), "TCP"),
            (Some("QUIC"), "TCP"),
            (Some("TCP_QUIC"), "TCP"),
            (Some("GENEVE"), "TCP"),
            // A `lambda` target group carries no protocol at all.
            (None, "TCP"),
        ];
        for (protocol, expected) in rows {
            assert_eq!(
                default_health_check_protocol(*protocol),
                *expected,
                "case {protocol:?}: expected the documented default {expected}"
            );
        }
    }

    /// The rule, over every probe protocol a health check may speak and each member of
    /// the pair.
    #[test]
    fn an_http_only_member_is_carried_only_by_an_http_probe() {
        // (the probe protocol, whether the pair may be carried)
        let rows: &[(Option<&str>, bool)] = &[
            (Some("HTTP"), true),
            (Some("HTTPS"), true),
            (Some("TCP"), false),
            // No protocol at all is a `lambda` target group's probe, an HTTP one.
            (None, true),
        ];
        for (protocol, carried) in rows {
            for (path, matcher) in [(true, false), (false, true), (true, true)] {
                let answer = check_http_only_health_check_members(*protocol, path, matcher);
                assert_eq!(
                    answer.is_ok(),
                    *carried,
                    "case {protocol:?} path={path} matcher={matcher}: expected \
                     carried={carried}, got {answer:?}"
                );
            }
            // A request naming neither member is outside the rule for every protocol.
            let answer = check_http_only_health_check_members(*protocol, false, false);
            assert!(
                answer.is_ok(),
                "case {protocol:?} naming neither member: expected it to be accepted, got \
                 {answer:?}"
            );
        }
    }

    /// The refusal names the member and the protocol the probe speaks.
    #[test]
    fn the_refusal_names_the_member_and_the_probe_protocol() {
        assert_eq!(
            check_http_only_health_check_members(Some("TCP"), true, false),
            Err("Health check paths are not supported for TCP health checks".to_string()),
            "case a path on a TCP probe: unexpected refusal"
        );
        assert_eq!(
            check_http_only_health_check_members(Some("TCP"), false, true),
            Err("Health check matchers are not supported for TCP health checks".to_string()),
            "case a matcher on a TCP probe: unexpected refusal"
        );
        // Naming both is refused over the path, the member the sentence states first.
        assert_eq!(
            check_http_only_health_check_members(Some("TCP"), true, true),
            Err("Health check paths are not supported for TCP health checks".to_string()),
            "case both members on a TCP probe: unexpected refusal"
        );
    }
}
