// SPDX-License-Identifier: CC0-1.0

//! What a service's `DnsConfig` may declare, and which instance attributes the records it
//! declares require.
//!
//! Both rules come from one place in the model — the documentation on `DnsRecord$Type` and
//! on `RegisterInstanceRequest$Attributes` — and both are checked by more than one
//! operation (`CreateService` and `UpdateService` decide what may be stored;
//! `RegisterInstance` decides what may be registered against what was stored), so they live
//! here once rather than in each handler.

use std::collections::HashMap;

/// `AWS_INSTANCE_IPV4`: "If the service configuration includes an `A` record, the IPv4
/// address that you want Route 53 to return in response to DNS queries"
/// (`RegisterInstanceRequest$Attributes`).
pub const ATTRIBUTE_IPV4: &str = "AWS_INSTANCE_IPV4";

/// `AWS_INSTANCE_IPV6`, which carries an `AAAA` record's address for the same reason.
pub const ATTRIBUTE_IPV6: &str = "AWS_INSTANCE_IPV6";

/// `AWS_INSTANCE_CNAME`: "If the service configuration includes a `CNAME` record, the domain
/// name that you want Route 53 to return in response to DNS queries".
pub const ATTRIBUTE_CNAME: &str = "AWS_INSTANCE_CNAME";

/// `AWS_INSTANCE_PORT`: "If the service includes an `SRV` record, the value that you want
/// Route 53 to return for the port", and "If the service includes `HealthCheckConfig`, the
/// port on the endpoint that you want Route 53 to send requests to."
pub const ATTRIBUTE_PORT: &str = "AWS_INSTANCE_PORT";

/// `AWS_ALIAS_DNS_NAME`: the DNS name of an Elastic Load Balancing load balancer Cloud Map
/// points an ALIAS record at instead of an address of its own. Documented as the alternative
/// to the address attributes — "If you specify a value for `AWS_ALIAS_DNS_NAME`, don't
/// specify values for any of the `AWS_INSTANCE` attributes" — for a service whose
/// "configuration ... must include settings for an `A` record, an `AAAA` record, or both".
pub const ATTRIBUTE_ALIAS_DNS_NAME: &str = "AWS_ALIAS_DNS_NAME";

/// The `RecordType` whose value is an IPv4 address.
pub const RECORD_A: &str = "A";
/// The `RecordType` whose value is an IPv6 address.
pub const RECORD_AAAA: &str = "AAAA";
/// The `RecordType` whose value is a domain name.
pub const RECORD_CNAME: &str = "CNAME";
/// The `RecordType` whose value is a priority/weight/port/hostname tuple.
pub const RECORD_SRV: &str = "SRV";

/// The `RoutingPolicy` a `CNAME` service must use: "You must specify `WEIGHTED` for the value
/// of `RoutingPolicy`" (`DnsRecord$Type`, under `CNAME`).
pub const ROUTING_POLICY_WEIGHTED: &str = "WEIGHTED";

/// Why a `DnsConfig` declaring `record_types`, with `routing_policy` and a Route 53 health
/// check as they would be stored, is not a configuration Cloud Map registers records for —
/// or `None` when it is.
///
/// Both clauses are sentences `DnsRecord$Type` states about a `CNAME` record:
///
/// * "You must specify `WEIGHTED` for the value of `RoutingPolicy`";
/// * "You can't specify both `CNAME` for `Type` and settings for `HealthCheckConfig`. If you
///   do, the request will fail with an `InvalidInput` error."
///
/// `CNAME` is the type with constraints of its own because a `CNAME` answer is a domain
/// NAME: one query can be answered with only one of them, so a policy that answers with
/// several instances has nothing to answer with, and there is no endpoint of the instance's
/// own for a health check to probe.
///
/// `record_types` is the whole set the configuration declares (at most one entry per type,
/// which each caller refuses separately) and `route53_health_checked` says whether the
/// configuration carries a `HealthCheckConfig`.
pub fn dns_config_violation(
    record_types: &[&str],
    routing_policy: &str,
    route53_health_checked: bool,
) -> Option<String> {
    if record_types.contains(&RECORD_CNAME) {
        if routing_policy != ROUTING_POLICY_WEIGHTED {
            return Some(format!(
                "{ROUTING_POLICY_WEIGHTED} routing policy must be used with DNS Record Type \
                 {RECORD_CNAME}"
            ));
        }
        if route53_health_checked {
            return Some(format!(
                "You can't specify both {RECORD_CNAME} for Type and settings for \
                 HealthCheckConfig."
            ));
        }
    }
    None
}

/// The attributes a service declaring `record_types` requires of every instance registered
/// against it, in the order a refusal names them.
///
/// From `RegisterInstanceRequest$Attributes`, one group per record the service declares: an
/// `A` record requires `AWS_INSTANCE_IPV4` ("This value is required if the service specified
/// by `ServiceId` includes settings for an `A` record"), an `AAAA` record
/// `AWS_INSTANCE_IPV6`, a `CNAME` record `AWS_INSTANCE_CNAME`, and an `SRV` record
/// `AWS_INSTANCE_PORT` — "the value that you want Route 53 to return for the port" — plus an
/// address: "you must specify a value for `AWS_INSTANCE_IPV4`, `AWS_INSTANCE_IPV6`, or
/// both", which is why the addresses are one group that any single member satisfies.
fn required_attribute_groups(record_types: &[&str]) -> Vec<&'static [&'static str]> {
    let mut groups: Vec<&'static [&'static str]> = Vec::new();
    if record_types.contains(&RECORD_A) {
        groups.push(&[ATTRIBUTE_IPV4]);
    }
    if record_types.contains(&RECORD_AAAA) {
        groups.push(&[ATTRIBUTE_IPV6]);
    }
    if record_types.contains(&RECORD_CNAME) {
        groups.push(&[ATTRIBUTE_CNAME]);
    }
    if record_types.contains(&RECORD_SRV) {
        groups.push(&[ATTRIBUTE_IPV4, ATTRIBUTE_IPV6]);
        groups.push(&[ATTRIBUTE_PORT]);
    }
    groups
}

/// Why `attributes` cannot register an instance against a service declaring `record_types`,
/// or `None` when every attribute those records require is there.
///
/// `AWS_ALIAS_DNS_NAME` is the documented alternative to the address attributes of an
/// alias-record registration — "If you specify a value for `AWS_ALIAS_DNS_NAME`, don't
/// specify values for any of the `AWS_INSTANCE` attributes" — so a registration that carries
/// it is not missing the address Route 53 takes from the load balancer instead.
///
/// The message names every attribute the registration is missing, so a caller learns about
/// all of them from one refusal.
pub fn missing_required_attributes(
    record_types: &[&str],
    attributes: &HashMap<String, String>,
) -> Option<String> {
    let aliased = attributes.contains_key(ATTRIBUTE_ALIAS_DNS_NAME);
    let mut missing: Vec<&str> = Vec::new();
    for group in required_attribute_groups(record_types) {
        // An alias record carries no address of its own, so the attributes Route 53 would
        // have answered from are not required of a registration that names one.
        if aliased && group.iter().all(|key| is_address_attribute(key)) {
            continue;
        }
        if group.iter().any(|key| attributes.contains_key(*key)) {
            continue;
        }
        for key in group {
            if !missing.contains(key) {
                missing.push(key);
            }
        }
    }
    (!missing.is_empty())
        .then(|| format!("Required attribute(s) {} are missing.", missing.join(", ")))
}

/// Whether `key` is one of the attributes an `A`/`AAAA` record's value is read from, which
/// an `AWS_ALIAS_DNS_NAME` registration supplies through the load balancer instead.
fn is_address_attribute(key: &str) -> bool {
    key == ATTRIBUTE_IPV4 || key == ATTRIBUTE_IPV6
}
