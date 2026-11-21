// SPDX-License-Identifier: CC0-1.0

//! The APPLICATION PROTOCOLS a TLS-terminating listener offers the client during the
//! handshake, and which of them the load balancer then speaks.
//!
//! Application-Layer Protocol Negotiation is the client offering a list of protocols and
//! the server choosing one of them, so what a listener offers is a fact of its own
//! configuration — and two pieces of configuration state it, one per load balancer family:
//!
//! * a Network Load Balancer's TLS listener carries the policy itself: "[TLS listeners] The
//!   name of the Application-Layer Protocol Negotiation (ALPN) policy. You can specify one
//!   policy name. The following are the possible values: `HTTP1Only`, `HTTP2Only`,
//!   `HTTP2Optional`, `HTTP2Preferred`, `None`" (`CreateListenerInput$AlpnPolicy`) — a name
//!   that says which protocols are offered and in which order the server prefers them
//!   ([`offered_for_policy`]), with `None` offering nothing at all. A TLS listener carrying
//!   no policy negotiates nothing either, which is the listener a create set none on;
//! * an Application Load Balancer's HTTPS listener has no such member, and the protocol it
//!   serves is the load balancer's own attribute: "`routing.http2.enabled` - Indicates
//!   whether HTTP/2 is enabled… The default is true" (`LoadBalancerAttribute$Key`). So such
//!   a listener offers HTTP/2 ahead of HTTP/1.1 while the attribute is on — the preference
//!   a load balancer that serves HTTP/2 has — and HTTP/1.1 alone once a caller switches it
//!   off.
//!
//! A protocol is offered only where the load balancer SPEAKS it: a client that accepted a
//! selection the server cannot speak would hold a connection nothing can answer, so the
//! listener's socket serves the selected protocol on the connection it was selected on
//! ([`super::listener_socket`]).
//!
//! Nothing here is written: the protocols are read off the records a create or a modify
//! persisted, per handshake.

use crate::state::{ListenerRecord, LoadBalancerFamily, LoadBalancerRecord};

/// The two protocol identifiers an ALPN policy names, as the wire spells them (RFC 7540 §11.1
/// registers `h2`, RFC 7230 `http/1.1`).
pub(super) const HTTP_2: &str = "h2";
pub(super) const HTTP_1_1: &str = "http/1.1";

/// The attribute that decides whether an Application Load Balancer serves HTTP/2:
/// `LoadBalancerAttribute$Key`'s `routing.http2.enabled`.
const HTTP2_ENABLED_ATTRIBUTE: &str = "routing.http2.enabled";

/// The `AlpnPolicy` names, from that member's own list.
const HTTP1_ONLY: &str = "HTTP1Only";
const HTTP2_ONLY: &str = "HTTP2Only";
const HTTP2_OPTIONAL: &str = "HTTP2Optional";
const HTTP2_PREFERRED: &str = "HTTP2Preferred";

/// The protocols one ALPN policy offers, in the order the server prefers them: the first
/// entry is what a client offering both is selected.
///
/// `HTTP2Only` and `HTTP1Only` offer the one protocol they name; `HTTP2Optional` offers both
/// preferring HTTP/1.1, and `HTTP2Preferred` offers both preferring HTTP/2 — which is what
/// "optional" and "preferred" say about the same pair. Every other value — `None`, and a
/// name this service does not publish — offers nothing.
fn offered_for_policy(policy: &str) -> &'static [&'static str] {
    match policy.trim() {
        HTTP1_ONLY => &[HTTP_1_1],
        HTTP2_ONLY => &[HTTP_2],
        HTTP2_OPTIONAL => &[HTTP_1_1, HTTP_2],
        HTTP2_PREFERRED => &[HTTP_2, HTTP_1_1],
        _ => &[],
    }
}

/// Whether `record` serves HTTP/2: what [`HTTP2_ENABLED_ATTRIBUTE`] holds for it — a
/// caller's own write, else the value the attribute is carried at — read through the ONE
/// catalog of load balancer attributes, so what `DescribeLoadBalancerAttributes` reports and
/// what a handshake offers cannot drift.
///
/// A load balancer of a family that carries no such attribute serves no HTTP/2 of this
/// kind: the attribute is documented "supported by only Application Load Balancers", and a
/// Network Load Balancer's listener names its own policy instead.
fn serves_http_2(record: &LoadBalancerRecord) -> bool {
    let Some(spec) = crate::state::load_balancer_attribute(HTTP2_ENABLED_ATTRIBUTE) else {
        return false;
    };
    if !spec.applies_to(record) {
        return false;
    }
    record
        .attributes
        .get(HTTP2_ENABLED_ATTRIBUTE)
        .cloned()
        .or_else(|| spec.default_for(record))
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"))
}

/// The protocols the handshake of `listener` offers, in the server's own preference order.
///
/// The listener's own ALPN policy decides it where the listener carries one — "you can
/// specify one policy name", so the first entry is the policy — and the load balancer's
/// `routing.http2.enabled` decides it for an Application Load Balancer's HTTPS listener,
/// which carries no such member. Anything else offers nothing: a Network Load Balancer's
/// TLS listener with no policy, and a Gateway Load Balancer's listeners, which terminate no
/// TLS at all.
pub(super) fn offered(
    listener: &ListenerRecord,
    load_balancer: &LoadBalancerRecord,
) -> Vec<&'static str> {
    if let Some(policy) = listener
        .alpn_policy
        .iter()
        .find(|policy| !policy.trim().is_empty())
    {
        return offered_for_policy(policy).to_vec();
    }
    if load_balancer.family() == LoadBalancerFamily::Application && serves_http_2(load_balancer) {
        return vec![HTTP_2, HTTP_1_1];
    }
    if load_balancer.family() == LoadBalancerFamily::Application {
        return vec![HTTP_1_1];
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    //! The reading of one ALPN policy name. What a CLIENT negotiates is pinned end to end
    //! in `tests/all/alb_negotiated_application_protocol.rs`.

    use super::*;

    /// RULE: a policy name offers the protocols it names, in the order it prefers them, and
    /// a name that names none offers none.
    #[test]
    fn a_policy_name_offers_the_protocols_it_names() {
        // The rows are the WHOLE published value list of `CreateListenerInput$AlpnPolicy`,
        // plus a name the member does not list and the empty one.
        let rows: &[(&str, &[&str])] = &[
            ("HTTP1Only", &[HTTP_1_1]),
            ("HTTP2Only", &[HTTP_2]),
            ("HTTP2Optional", &[HTTP_1_1, HTTP_2]),
            ("HTTP2Preferred", &[HTTP_2, HTTP_1_1]),
            ("None", &[]),
            ("HTTP3Only", &[]),
            ("", &[]),
        ];
        for (policy, expected) in rows {
            let offered = offered_for_policy(policy);
            assert_eq!(
                offered, *expected,
                "case {policy:?}: expected {expected:?}, got {offered:?}"
            );
        }
    }
}
