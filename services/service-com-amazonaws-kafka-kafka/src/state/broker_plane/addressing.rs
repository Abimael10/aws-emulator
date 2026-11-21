// SPDX-License-Identifier: CC0-1.0

//! WHERE one BROKER NODE of a cluster is published: the host that node is named by, and
//! the DNS zone this emulator may name it in.
//!
//! Amazon MSK gives each broker node a host of its OWN —
//! `b-<id>.<cluster>.<suffix>.kafka.<region>.…`, the name `ListNodes` reports under
//! `BrokerNodeInfo$Endpoints` — and publishes the whole cluster's brokers for one ACCESS
//! METHOD on a single PORT: a bootstrap string lists one `host:port` pair per node, every
//! pair carrying the port that method is served on, so the port says which access method
//! a client is reaching and the host says which broker it is reaching
//! ([`super::AccessPath`]).
//!
//! The zone that host sits in is the one this emulator may announce a name of its own in
//! ([`::service_common::data_plane_addressing::announced_zone_of_authority`]), never the
//! zone AWS serves its own names in: a name a service hands a client is a name the client
//! RESOLVES and then dials, so it has to lead back to this process. It is read off the
//! address the CORE bound the broker socket on, which is the interface the name has to
//! resolve to — the loopback interface unless the operator named another, and `localhost`
//! and every name under it are reserved for that interface (RFC 6761).

/// The host Amazon MSK names the broker node `id` of a cluster by:
/// `b-<id>.<cluster>.<suffix>.kafka.<region>.amazonaws.com`, where `dns_suffix` is the
/// opaque cluster id and cell every endpoint of that cluster sits under.
///
/// The ONE statement of that form: a node's stored endpoint host
/// ([`crate::state::BrokerNode::endpoint_host`]) is composed here, and a cluster that
/// publishes no broker nodes of its own stands behind one node whose host is composed here
/// too, so both name the same thing the same way.
#[must_use]
pub fn broker_endpoint_host(region: &str, name: &str, dns_suffix: &str, id: i32) -> String {
    format!("b-{id}.{name}.{dns_suffix}.kafka.{region}.amazonaws.com")
}

/// The DNS zone a broker host this emulator publishes sits in, read off the `host:port` the
/// core bound the broker socket at: the interface a client that resolves the name then
/// dials.
#[must_use]
pub fn published_zone(listener_authority: &str) -> String {
    ::service_common::data_plane_addressing::announced_zone_of_authority(listener_authority)
}

/// The host THIS emulator publishes a broker node under: the name Amazon MSK gives that
/// node ([`broker_endpoint_host`]), in the zone this emulator may announce a name of its
/// own in ([`published_zone`]).
///
/// Every label but the zone is the node's own, so the published name identifies the broker:
/// two nodes of one cluster never share one, two clusters never share one, and a client
/// correlating a connection with the node the control plane reports reads the id back out
/// of the host. A host already sitting in that zone is kept as it stands, since it is
/// already a name this emulator may hand out.
///
/// Each label is taken as a DNS LABEL
/// ([`::service_common::data_plane_addressing::dns_label`], this tree's one reading of that
/// folding): a cluster's `ClusterName` is CALLER-SUPPLIED text that ends up in a host here —
/// the model bounds its length and nothing else — so a value carrying a host or path
/// delimiter cannot make the published address name an authority the caller chose.
#[must_use]
pub fn published_broker_host(endpoint_host: &str, zone: &str) -> String {
    let host = endpoint_host.trim_end_matches('.');
    let lowered = host.to_ascii_lowercase();
    let stem = ::service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES
        .iter()
        .find_map(|suffix| {
            lowered
                .ends_with(suffix)
                .then(|| &host[..host.len() - suffix.len()])
        })
        .unwrap_or(host);
    let named = stem
        .split('.')
        .map(::service_common::data_plane_addressing::dns_label)
        .filter(|label| !label.is_empty())
        .collect::<Vec<String>>()
        .join(".");
    if named.is_empty() {
        return zone.to_string();
    }
    if named == zone || named.ends_with(&format!(".{zone}")) {
        return named;
    }
    format!("{named}.{zone}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the host a broker node is published under is that node's own Amazon MSK name
    /// with the zone AWS serves replaced by the zone this emulator may announce in, so it
    /// names the broker and leads back to this process.
    #[test]
    fn a_published_broker_host_names_the_node_in_the_announced_zone() {
        // The rows span what the composition reads: the node ids a cluster lays out, the
        // zone a loopback-bound socket is named in, a socket bound on a routable address,
        // one bound on an IPv6 literal, an emulator reached under a name of its own, a host
        // already in the announced zone, and cluster names carrying characters no DNS label
        // admits.
        let rows: &[(&str, &str, &str, &str)] = &[
            (
                "the first node of a loopback-bound socket",
                "b-1.demo.a1b2c3.c4.kafka.us-west-2.amazonaws.com",
                "127.0.0.1:38211",
                "b-1.demo.a1b2c3.c4.kafka.us-west-2.localhost",
            ),
            (
                "another node of the same cluster",
                "b-12.demo.a1b2c3.c4.kafka.us-west-2.amazonaws.com",
                "127.0.0.1:38211",
                "b-12.demo.a1b2c3.c4.kafka.us-west-2.localhost",
            ),
            (
                "a socket bound on an IPv6 literal",
                "b-2.demo.a1b2c3.c4.kafka.eu-central-1.amazonaws.com",
                "[::1]:40127",
                "b-2.demo.a1b2c3.c4.kafka.eu-central-1.localhost",
            ),
            (
                "a socket bound on a routable address",
                "b-3.demo.a1b2c3.c4.kafka.ap-northeast-1.amazonaws.com",
                "10.0.0.7:9092",
                "b-3.demo.a1b2c3.c4.kafka.ap-northeast-1.localhost",
            ),
            (
                "an emulator reached under a name of its own",
                "b-1.demo.a1b2c3.c4.kafka.us-east-2.amazonaws.com",
                "emulator.internal:9297",
                "b-1.demo.a1b2c3.c4.kafka.us-east-2.emulator.internal",
            ),
            (
                "a host already sitting in the announced zone",
                "b-1.demo.a1b2c3.c4.kafka.us-west-2.localhost",
                "127.0.0.1:38211",
                "b-1.demo.a1b2c3.c4.kafka.us-west-2.localhost",
            ),
            (
                // `ClusterName` is caller-supplied text the model bounds only the LENGTH
                // of, so a name carrying a delimiter must not reach the authority.
                "a cluster name carrying a port delimiter",
                "b-1.demo:9999/other.a1b2c3.c4.kafka.us-west-2.amazonaws.com",
                "127.0.0.1:38211",
                "b-1.demo-9999-other.a1b2c3.c4.kafka.us-west-2.localhost",
            ),
            (
                "a cluster name carrying an underscore and a space",
                "b-1.my_demo cluster.a1b2c3.c4.kafka.us-west-2.amazonaws.com",
                "127.0.0.1:38211",
                "b-1.my-demo-cluster.a1b2c3.c4.kafka.us-west-2.localhost",
            ),
        ];
        for (case, endpoint_host, authority, expected) in rows {
            let published = published_broker_host(endpoint_host, &published_zone(authority));
            assert_eq!(
                &published, expected,
                "case {case:?}: expected the host {expected:?}, got {published:?}"
            );
            assert!(
                ::service_common::data_plane_addressing::is_host_shaped(&published)
                    && !published.contains(':')
                    && !published.contains('/'),
                "case {case:?}: a published broker host must carry no address delimiter, got \
                 {published:?}"
            );
            for aws_zone in ::service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES {
                assert!(
                    !published.ends_with(aws_zone),
                    "case {case:?}: a published broker host must name no zone of AWS's own \
                     ({aws_zone}), got {published:?}"
                );
            }
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
