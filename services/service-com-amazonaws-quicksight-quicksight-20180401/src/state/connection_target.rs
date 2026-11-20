// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS a data source's parameters declare, and whether the service can route a
//! connection to it.
//!
//! QuickSight does not register a data source whose declared connection target it
//! cannot reach: the create (and the update that re-states the target) validates the
//! address the submitted `DataSourceParameters` carry BEFORE anything is stored, and an
//! address outside the globally routable unicast space is refused synchronously as a
//! parameter whose value is not valid.
//!
//! What makes an address unreachable is a property of the address itself, so it is
//! decided here rather than by trying to connect: an IP literal reserved for a private
//! network, for the loopback or link-local scope, for documentation or benchmarking, or
//! for multicast and the reserved blocks, names nothing a service on the public network
//! can route to — and an address that names an IPv4 host in IPv6 form (mapped,
//! compatible or 6to4) is read as that host, so the form it was written in cannot change
//! the answer. An address in one of those blocks IS reachable through a VPC connection,
//! which is what QuickSight documents a caller must supply to reach a source inside a
//! VPC, so a request that supplies one declares a route and is not refused here.
//!
//! A target named by anything other than a bare address — a DNS name, a URL, a domain,
//! a cluster id, a project id or a resource ARN — carries no address to classify:
//! nothing in this emulator resolves names, so there is no routing failure to report
//! for one, exactly as [`super::connection_test`] reports none for a source outside the
//! emulator.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::ops::types::{DataSourceParameters, VpcConnectionProperties};

/// The refusal a write whose declared connection target cannot be routed to reports:
/// fixed text and the address the request carried, with no trailing period.
pub fn unroutable_host_message(host: &str) -> String {
    format!("Unable to route to the host address {host}")
}

/// The address `parameters` declare as the host of the connection, when the parameter
/// set names one.
///
/// Source: smithy_query(query="shape", shape="DataSourceParameters") and the member
/// shapes it targets — every parameter set below declares a `Host` ("the host" /
/// "the host name" of the source); the rest name their target by a domain, a URL, a
/// cluster or instance id, a project id, a workgroup or an ARN, none of which is an
/// address.
pub fn declared_host(parameters: &DataSourceParameters) -> Option<&str> {
    match parameters {
        DataSourceParameters::AuroraParameters(parameters) => Some(parameters.host.as_str()),
        DataSourceParameters::AuroraPostgreSqlParameters(parameters) => {
            Some(parameters.host.as_str())
        }
        DataSourceParameters::DatabricksParameters(parameters) => Some(parameters.host.as_str()),
        DataSourceParameters::ExasolParameters(parameters) => Some(parameters.host.as_str()),
        DataSourceParameters::ImpalaParameters(parameters) => Some(parameters.host.as_str()),
        DataSourceParameters::MariaDbParameters(parameters) => Some(parameters.host.as_str()),
        DataSourceParameters::MySqlParameters(parameters) => Some(parameters.host.as_str()),
        DataSourceParameters::OracleParameters(parameters) => Some(parameters.host.as_str()),
        DataSourceParameters::PostgreSqlParameters(parameters) => Some(parameters.host.as_str()),
        DataSourceParameters::PrestoParameters(parameters) => Some(parameters.host.as_str()),
        // `Host` is optional here: a Redshift connection may name the cluster instead,
        // and a cluster id is not an address.
        DataSourceParameters::RedshiftParameters(parameters) => parameters.host.as_deref(),
        DataSourceParameters::SnowflakeParameters(parameters) => Some(parameters.host.as_str()),
        DataSourceParameters::SparkParameters(parameters) => Some(parameters.host.as_str()),
        DataSourceParameters::SqlServerParameters(parameters) => Some(parameters.host.as_str()),
        DataSourceParameters::StarburstParameters(parameters) => Some(parameters.host.as_str()),
        DataSourceParameters::TeradataParameters(parameters) => Some(parameters.host.as_str()),
        DataSourceParameters::TrinoParameters(parameters) => Some(parameters.host.as_str()),
        _ => None,
    }
}

/// The declared host a connection cannot be routed to, or `None` when the write's
/// target is one this service may accept.
///
/// `None` as well when no parameters were submitted (there is no target to reach), when
/// the parameter set names no address, when the address is one the public network
/// routes to, and when the request supplies `VpcConnectionProperties` — the route that
/// makes an address inside a VPC reachable.
pub fn unroutable_host<'a>(
    parameters: Option<&'a DataSourceParameters>,
    vpc_connection_properties: Option<&VpcConnectionProperties>,
) -> Option<&'a str> {
    if vpc_connection_properties.is_some() {
        return None;
    }
    let host = declared_host(parameters?)?.trim();
    let address: IpAddr = host.parse().ok()?;
    if is_routable(address) {
        None
    } else {
        Some(host)
    }
}

/// Whether a connection to `address` can leave the service at all: true only for an
/// address in the globally routable unicast space.
fn is_routable(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_routable_v4(address),
        IpAddr::V6(address) => is_routable_v6(address),
    }
}

/// [`is_routable`] for an IPv4 address: every block reserved for something other than
/// global unicast is unreachable.
fn is_routable_v4(address: Ipv4Addr) -> bool {
    let [first, second, third, _] = address.octets();
    !(address.is_unspecified()
        || address.is_loopback()
        // 10/8, 172.16/12 and 192.168/16: a private network.
        || address.is_private()
        || address.is_link_local()
        || address.is_broadcast()
        || address.is_documentation()
        || address.is_multicast()
        // 0/8: "this network", which names no host to reach.
        || first == 0
        // 100.64/10: shared address space, reachable only inside one carrier.
        || (first == 100 && (64..128).contains(&second))
        // 192.0.0/24: addresses assigned to IETF protocols rather than to hosts.
        || (first == 192 && second == 0 && third == 0)
        // 198.18/15: reserved for benchmarking, so routed nowhere.
        || (first == 198 && (second == 18 || second == 19))
        // 240/4 upwards: reserved, which includes the broadcast address.
        || first >= 240)
}

/// [`is_routable`] for an IPv6 address: the same reading of the reserved blocks.
///
/// An address that NAMES an IPv4 host is classified as that host rather than by the
/// IPv6 blocks, so the same target is read the same way whichever form it was written
/// in.
fn is_routable_v6(address: Ipv6Addr) -> bool {
    if address.is_unspecified() || address.is_loopback() || address.is_multicast() {
        return false;
    }
    if let Some(embedded) = embedded_ipv4(address) {
        return is_routable_v4(embedded);
    }
    let segments = address.segments();
    !(
        // fc00::/7: a unique local address, routed only inside one site.
        segments[0] & 0xfe00 == 0xfc00
        // fe80::/10: link-local unicast, routed only on one link.
        || segments[0] & 0xffc0 == 0xfe80
        // 2001:db8::/32: reserved for documentation.
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        // 100::/64: the discard-only block, which drops whatever is sent to it.
        || (segments[0] == 0x0100 && segments[1..4].iter().all(|segment| *segment == 0))
    )
}

/// The IPv4 address an IPv6 literal names, when it names one.
///
/// Three forms carry one: the mapped form `::ffff:a.b.c.d`, the compatible form
/// `::a.b.c.d`, and the 6to4 prefix `2002:a.b.c.d::/48`, whose address is the IPv4 host
/// the traffic is tunnelled to. The caller has already answered the loopback and
/// unspecified addresses, so neither reaches the compatible-form reading here.
fn embedded_ipv4(address: Ipv6Addr) -> Option<Ipv4Addr> {
    let segments = address.segments();
    if segments[0] == 0x2002 {
        return Some(ipv4_of(segments[1], segments[2]));
    }
    if let Some(mapped) = address.to_ipv4_mapped() {
        return Some(mapped);
    }
    if segments[..6].iter().all(|segment| *segment == 0) {
        return Some(ipv4_of(segments[6], segments[7]));
    }
    None
}

/// The IPv4 address two consecutive IPv6 segments spell, high half first.
fn ipv4_of(high: u16, low: u16) -> Ipv4Addr {
    let [first, second] = high.to_be_bytes();
    let [third, fourth] = low.to_be_bytes();
    Ipv4Addr::new(first, second, third, fourth)
}
