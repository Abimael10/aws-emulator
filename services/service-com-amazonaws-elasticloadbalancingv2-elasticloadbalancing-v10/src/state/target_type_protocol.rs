// SPDX-License-Identifier: CC0-1.0

//! Which target-group PROTOCOLS a given `TargetType` can be routed to over, and the
//! `TargetTypeEnum` values the rules below and their callers name.
//!
//! `TargetType` says what a target group's members ARE — "`instance` - Register targets by
//! instance ID … `ip` - Register targets by IP address … `lambda` - Register a single
//! Lambda function as a target … `alb` - Register a single Application Load Balancer as a
//! target" — and `Protocol` says how traffic reaches them: "For Application Load
//! Balancers, the supported protocols are HTTP and HTTPS. For Network Load Balancers, the
//! supported protocols are TCP, TLS, UDP, TCP_UDP, QUIC, or TCP_QUIC. For Gateway Load
//! Balancers, the supported protocol is GENEVE. … If the target is a Lambda function, this
//! parameter does not apply."
//!
//! The two members are therefore not independent: a target of a given KIND can only be
//! reached over a protocol Elastic Load Balancing can forward to that kind of target. An
//! Application Load Balancer registered as a target is itself the thing that terminates
//! HTTP, so the load balancer in front of it passes connections through to it: the pairing
//! Elastic Load Balancing admits for that target type is the TCP protocol alone. An
//! `instance` or `ip` target is a host, which every protocol a target group can carry
//! reaches, and a `lambda` target is invoked rather than connected to, so neither puts a
//! bound of its own on the member.
//!
//! The model declares the two members separately and states no dependency between them, so
//! the boundary validator passes every combination and this is the only gate. It is read
//! here ONCE, so the operation that mints a target group and anything that later judges a
//! stored one agree on which pairings exist.

use super::Refusal;

/// The target type that holds "a single Application Load Balancer as a target", from
/// `TargetTypeEnum`'s own values.
pub const LOAD_BALANCER_TARGET_TYPE: &str = "alb";

/// The target type that holds "a single Lambda function as a target", from the same values:
/// the one target type whose target is INVOKED rather than connected to, which is what
/// several members and one whole attribute family key on.
pub const LAMBDA_TARGET_TYPE: &str = "lambda";

/// The protocols a target group of `target_type` may carry, or `None` when the target type
/// puts no bound of its own on the protocol — which is every target type whose targets are
/// hosts (`instance`, `ip`) or are invoked rather than connected to (`lambda`).
pub fn protocols_for_target_type(target_type: &str) -> Option<&'static [&'static str]> {
    match target_type {
        // An Application Load Balancer target terminates the application protocol itself,
        // so the load balancer in front of it forwards TCP connections to it.
        LOAD_BALANCER_TARGET_TYPE => Some(&["TCP"]),
        _ => None,
    }
}

/// Whether a target group of `target_type` can carry `protocol`, and — when it cannot —
/// the rule it broke.
///
/// A request that names no protocol is judged by neither rule here: what protocol such a
/// target group carries is settled elsewhere, and this is only about the PAIRING of the
/// two members the request did carry.
pub fn check_target_type_protocol(
    target_type: &str,
    protocol: Option<&str>,
) -> Result<(), Refusal> {
    let (Some(admitted), Some(protocol)) = (protocols_for_target_type(target_type), protocol)
    else {
        return Ok(());
    };
    if admitted.contains(&protocol) {
        return Ok(());
    }
    // Elastic Load Balancing words this refusal about the combination, naming the target
    // type the request asked for and the protocol it cannot be reached over; the protocols
    // that WOULD serve it are not restated in it.
    Err(Refusal::Configuration(format!(
        "Target type '{target_type}' is not supported for target groups with {protocol} protocol"
    )))
}
