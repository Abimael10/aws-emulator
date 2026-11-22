// SPDX-License-Identifier: CC0-1.0

//! WHERE a cluster's BROKERS are published: the name Amazon MSK gives one broker node, and
//! the address THIS emulator answers that node's connections at.
//!
//! Amazon MSK gives each broker node a host of its OWN —
//! `b-<id>.<cluster>.<suffix>.kafka.<region>.…`, the name `ListNodes` reports under
//! `BrokerNodeInfo$Endpoints` — and publishes the whole cluster's brokers for one ACCESS
//! METHOD on a single PORT: a bootstrap string lists `host:port` pairs that all carry the
//! port that method is served on ([`super::AccessPath`]).
//!
//! What this emulator may PUBLISH for a client to dial is narrower than what AWS names.
//! An address a service hands a client is an address the client then connects to, so it has
//! to lead back to this process: it is the `host:port` the CORE bound the broker socket at
//! — the loopback interface unless the operator named another — and never a name of AWS's
//! own, nor a name composed under some zone, which resolves to nothing a client on this
//! machine can reach. Every broker node of one access method is reached at that one
//! address, and `Metadata` reports each of them there.

/// The host Amazon MSK names the broker node `id` of a cluster by:
/// `b-<id>.<cluster>.<suffix>.kafka.<region>.amazonaws.com`, where `dns_suffix` is the
/// opaque cluster id and cell every endpoint of that cluster sits under.
///
/// The ONE statement of that form: a node's stored endpoint host
/// ([`crate::state::BrokerNode::endpoint_host`]), which `ListNodes` reports, is composed
/// here. It is the name AWS gives the node, not an address this emulator serves — what a
/// client dials is [`served_broker_host`] on the access method's port.
#[must_use]
pub fn broker_endpoint_host(region: &str, name: &str, dns_suffix: &str, id: i32) -> String {
    format!("b-{id}.{name}.{dns_suffix}.kafka.{region}.amazonaws.com")
}

/// The host this emulator publishes a cluster's brokers at, read off the `host:port` the
/// core bound the broker socket at: the address a client that takes a published pair then
/// dials, which is the interface the core is accepting on.
///
/// Read through the one shared reading of a listener's address
/// ([`::service_common::data_plane_addressing::host_only`]), so a wildcard bind is
/// published as the loopback address the core says the socket is dialable at and an IPv6
/// bind keeps the brackets its own colons live in.
#[must_use]
pub fn served_broker_host(listener_authority: &str) -> String {
    ::service_common::data_plane_addressing::host_only(listener_authority).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the host a cluster's brokers are published at is the one the core bound the
    /// socket on, so the published address leads back to this process rather than to a
    /// name it does not serve.
    #[test]
    fn the_published_broker_host_is_the_address_the_core_bound() {
        // The rows span the forms a listener's address comes in: a loopback bind, an IPv6
        // literal (whose brackets carry its own colons), a bind on a routable address, an
        // emulator reached under a name of its own, and an authority naming no port.
        let rows: &[(&str, &str, &str)] = &[
            ("a loopback-bound socket", "127.0.0.1:38211", "127.0.0.1"),
            ("an IPv6 literal", "[::1]:40127", "[::1]"),
            ("a routable address", "10.0.0.7:9092", "10.0.0.7"),
            (
                "an emulator reached under a name of its own",
                "emulator.internal:9297",
                "emulator.internal",
            ),
            ("an authority naming no port", "127.0.0.1", "127.0.0.1"),
        ];
        for (case, authority, expected) in rows {
            let published = served_broker_host(authority);
            assert_eq!(
                &published, expected,
                "case {case:?}: expected the host {expected:?}, got {published:?}"
            );
            assert!(
                ::service_common::data_plane_addressing::is_host_shaped(&published),
                "case {case:?}: a published broker host must be host-shaped, got {published:?}"
            );
            assert!(
                !::service_common::data_plane_addressing::addresses_an_aws_endpoint(&published),
                "case {case:?}: a published broker host must name no zone of AWS's own, got \
                 {published:?}"
            );
        }
    }

    /// RULE: a node's Amazon MSK name carries its own id, its cluster's name and that
    /// cluster's endpoint suffix and Region, so one cluster's nodes differ only in the
    /// `b-<id>` label and no two clusters share a name.
    #[test]
    fn a_brokers_endpoint_host_names_its_id_cluster_and_region() {
        // The rows span the ids a cluster lays out and the clusters they belong to.
        let rows: &[(&str, &str, &str, &str, i32)] = &[
            ("the first node", "us-west-2", "demo", "a1b2c3.c4", 1),
            ("the fifteenth node", "us-west-2", "demo", "a1b2c3.c4", 15),
            ("another cluster's node", "eu-west-1", "orders", "9f8e7d.c2", 1),
        ];
        let mut seen: Vec<String> = Vec::new();
        for (case, region, name, dns_suffix, id) in rows {
            let host = broker_endpoint_host(region, name, dns_suffix, *id);
            assert!(
                host.starts_with(&format!("b-{id}.{name}."))
                    && host.ends_with(&format!(".kafka.{region}.amazonaws.com")),
                "case {case:?}: expected a b-{id} host of {name} in {region}, got {host:?}"
            );
            assert!(
                !seen.contains(&host),
                "case {case:?}: two broker nodes must not share a host, got {host:?} again \
                 among {seen:?}"
            );
            seen.push(host);
        }
    }
}
