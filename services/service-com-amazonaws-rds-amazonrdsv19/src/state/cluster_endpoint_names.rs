// SPDX-License-Identifier: CC0-1.0

//! The DNS HOST NAMES a DB cluster MINTS for its endpoints, for the members that publish one.
//!
//! `DBCluster$Endpoint` is "the connection endpoint for the primary instance of the DB
//! cluster" and `DBCluster$ReaderEndpoint` "the reader endpoint for the DB cluster … [which]
//! load-balances connections across the Aurora Replicas that are available in a DB cluster"
//! (`smithy_query(query="shape",shape="DBCluster")`). Both are HOSTS and neither carries a
//! port: the cluster has ONE `Port` member, "the port that the database engine is listening
//! on", so `Endpoint` + `Port` and `ReaderEndpoint` + `Port` are the two pairs a client dials.
//!
//! A name composed here is minted by the CLUSTER, from its own identifier and the one
//! server-assigned label it was created with:
//!
//! * `<identifier>.cluster-<label>.<region>.<suffix>` for the writer, and
//! * `<identifier>.cluster-ro-<label>.<region>.<suffix>` for the reader,
//!
//! over the SAME suffix. Neither is read off a member instance, so a cluster with no members
//! at all publishes its own — a serverless cluster nobody has created an instance in is
//! reached at it — and the discriminating `cluster-`/`cluster-ro-` label is what tells a read
//! path from a write path.
//!
//! WHICH of a cluster's endpoint members is published as a minted name at all, and under
//! which suffix, depends on where that cluster's data plane is served, because a name a
//! service hands a client is a name the client RESOLVES and then dials:
//!
//! * a cluster whose engine wire this emulator serves is reached on a listener of its own
//!   ([`super::data_plane::cluster_endpoint`]), so its `Endpoint` — the member a session is
//!   opened on, beside the one `Port` that listener answers at — is that listener's own HOST
//!   and no name at all ([`super::DbClusterRecord::endpoint`]). Its `ReaderEndpoint` keeps the
//!   minted `cluster-ro-` name, which is the only thing that tells the read path from the
//!   write path, under the engine's own service label in the zone a resolver answers with the
//!   interface that listener is bound on ([`served_endpoint_suffix`]) — `localhost` and every
//!   name under it being reserved for the loopback interface the core binds on (RFC 6761);
//! * a cluster this emulator serves no data plane for publishes the AWS form for BOTH, under
//!   the DNS domain of the control plane that owns its engine ([`super::endpoint_domain`]):
//!   there is no local plane for such a cluster to lead a client to.
//!
//! A cluster whose data plane is an AWS API served at this emulator's own endpoint
//! ([`super::engine_data_plane_is_an_api`] — an Amazon Neptune graph cluster) publishes no
//! minted name at all, for either member: that plane is HTTP on this emulator's own front
//! door, which admits a request only when its `Host` names the interface the core bound
//! (`emulator_server::is_loopback_host`, the DNS-rebinding guard), so a name of this
//! emulator's own — in any zone — is refused there and would be no connection endpoint at
//! all. Such a cluster therefore publishes the HOST this process is reached at for both of its
//! endpoint members, which is the only address its plane answers on
//! ([`super::api_plane_address`]).

/// The label a cluster's WRITER endpoint carries in front of its server-assigned one:
/// `<identifier>.cluster-<label>.…`, the published form of "the connection endpoint for the
/// primary instance of the DB cluster".
pub const CLUSTER_WRITER_LABEL: &str = "cluster-";

/// The label a cluster's READER endpoint carries in the same position:
/// `<identifier>.cluster-ro-<label>.…`, the one discriminator between the two names a
/// read/write-splitting client reads.
pub const CLUSTER_READER_LABEL: &str = "cluster-ro-";

/// The label a CUSTOM endpoint of a cluster carries in the same position:
/// `<endpoint identifier>.cluster-custom-<label>.…`, the published form of a name a caller
/// created in front of the cluster's data plane.
pub const CLUSTER_CUSTOM_LABEL: &str = "cluster-custom-";

/// One cluster endpoint host name: the cluster's `identifier`, the endpoint's `role` label
/// ([`CLUSTER_WRITER_LABEL`] or [`CLUSTER_READER_LABEL`]) with the cluster's own
/// server-assigned `label` behind it, the `region` the cluster was created in, and the DNS
/// `suffix` both of its endpoints share.
///
/// A HOST and nothing else: no port, no scheme.
#[must_use]
pub fn cluster_endpoint_host(
    identifier: &str,
    role: &str,
    label: &str,
    region: &str,
    suffix: &str,
) -> String {
    format!("{identifier}.{role}{label}.{region}.{suffix}")
}

/// The DNS suffix the endpoints of a cluster whose data plane this emulator SERVES sit in:
/// the service label of the control plane that owns the cluster's engine, in the zone a
/// resolver answers with the interface the cluster's listener is bound on.
///
/// `endpoint_domain` is that control plane's own endpoint domain
/// (`rds.amazonaws.com`, `docdb.amazonaws.com`, `neptune.amazonaws.com`): its FIRST label is
/// the service, which is what keeps a database endpoint recognisable as one, and the zone AWS
/// publishes its endpoints in is replaced by the zone this emulator may announce a name of its
/// own in (`service_common::data_plane_addressing::announced_zone_of_authority`, the tree's
/// one reading of that question, read off the address the listener is bound to). A name in a
/// zone AWS serves resolves nowhere near this process or, worse, at genuine AWS
/// infrastructure, so this never announces one; a name in the loopback zone resolves to the
/// interface the listener was opened on.
#[must_use]
pub fn served_endpoint_suffix(endpoint_domain: &str, served_host: &str) -> String {
    let service = endpoint_domain
        .split('.')
        .next()
        .unwrap_or(endpoint_domain);
    let zone = ::service_common::data_plane_addressing::announced_zone_of_authority(served_host);
    format!("{service}.{zone}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the two names composed here for one cluster differ only in the
    /// `cluster-`/`cluster-ro-` label, are derived from the cluster's own identifier, and
    /// share one suffix.
    #[test]
    fn the_two_endpoint_names_share_a_suffix_and_differ_in_their_role_label() {
        // The rows span what the composition generalises over: the identifier (short, long,
        // hyphenated), the region, and the suffix (a served one and the published AWS form).
        let rows: &[(&str, &str, &str, &str, &str)] = &[
            ("a serverless cluster", "sim2real-aur-1757", "abcdefghijkl", "us-west-2", "rds.localhost"),
            ("a one-label identifier", "db", "mnopqrstuvwx", "us-east-1", "rds.localhost"),
            ("the published AWS form", "emu-docdb", "yz0123456789", "eu-central-1", "docdb.amazonaws.com"),
        ];
        for (case, identifier, label, region, suffix) in rows {
            let writer =
                cluster_endpoint_host(identifier, CLUSTER_WRITER_LABEL, label, region, suffix);
            let reader =
                cluster_endpoint_host(identifier, CLUSTER_READER_LABEL, label, region, suffix);
            assert_ne!(
                writer, reader,
                "case {case:?}: the two endpoints must be distinct, got {writer:?} for both"
            );
            for (which, name, role) in [
                ("writer", &writer, CLUSTER_WRITER_LABEL),
                ("reader", &reader, CLUSTER_READER_LABEL),
            ] {
                let expected_prefix = format!("{identifier}.{role}");
                assert!(
                    name.starts_with(&expected_prefix),
                    "case {case:?}: the {which} endpoint must begin {expected_prefix:?}, got \
                     {name:?}"
                );
                let expected_suffix = format!(".{region}.{suffix}");
                assert!(
                    name.ends_with(&expected_suffix),
                    "case {case:?}: the {which} endpoint must end {expected_suffix:?}, got \
                     {name:?}"
                );
                assert!(
                    !name.contains(':'),
                    "case {case:?}: an endpoint member carries no port, got {name:?}"
                );
            }
        }
    }

    /// RULE: the suffix of a SERVED cluster's endpoints is the engine's service label in the
    /// zone the address its listener is bound on names — never a zone AWS serves.
    #[test]
    fn a_served_clusters_suffix_names_the_zone_its_listener_is_reached_in() {
        // The rows span the endpoint domains the front door publishes and the addresses a
        // listener can be bound to: the loopback literals the core binds by default, a
        // routable address an operator bound, and a name the emulator is served under.
        let rows: &[(&str, &str, &str, &str)] = &[
            ("the default loopback bind", "rds.amazonaws.com", "127.0.0.1", "rds.localhost"),
            ("an IPv6 loopback bind", "rds.amazonaws.com", "::1", "rds.localhost"),
            ("a routable bind", "rds.amazonaws.com", "192.168.1.10", "rds.localhost"),
            ("a name the emulator is served under", "rds.amazonaws.com", "emulator.internal", "rds.emulator.internal"),
            ("another control plane's domain", "docdb.amazonaws.com", "127.0.0.1", "docdb.localhost"),
            ("the graph control plane's domain", "neptune.amazonaws.com", "127.0.0.1", "neptune.localhost"),
        ];
        for (case, endpoint_domain, served_host, expected) in rows {
            let suffix = served_endpoint_suffix(endpoint_domain, served_host);
            assert_eq!(
                suffix, *expected,
                "case {case:?}: expected the suffix {expected:?}, got {suffix:?}"
            );
            for aws_zone in ::service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES {
                assert!(
                    !suffix.ends_with(aws_zone),
                    "case {case:?}: a served cluster must announce no zone of AWS's own \
                     ({aws_zone}), got {suffix:?}"
                );
            }
        }
    }
}
