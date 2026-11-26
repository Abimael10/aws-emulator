// SPDX-License-Identifier: CC0-1.0

//! The DNS HOST NAMES a DB cluster MINTS for its endpoints, for the clusters that publish one.
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
//! * `<identifier>.cluster-<label>.<region>.<endpoint domain>` for the writer, and
//! * `<identifier>.cluster-ro-<label>.<region>.<endpoint domain>` for the reader,
//!
//! over the SAME domain — that of the control plane which owns the cluster's engine
//! ([`super::endpoint_domain`]). Neither is read off a member instance, so a cluster with no
//! members at all publishes its own, and the discriminating `cluster-`/`cluster-ro-` label is
//! what tells a read path from a write path.
//!
//! WHICH clusters publish a name composed here: only those this emulator serves NO data plane
//! for. An endpoint member a client DIALS has to be a host that leads back to the plane
//! answering the session, and the only hosts that reach this process are its OWN — the
//! interface the core bound a listener on, and the endpoint this emulator is served at. A name
//! this service announced beneath one of them is a host of nobody's: a client that resolves it
//! reaches no listener, and a client that checks what it was handed is being given an address
//! outside this emulator. So a cluster whose engine wire this emulator serves on a listener of
//! its own ([`super::data_plane::cluster_endpoint`]), and one whose data plane it serves as an
//! AWS API at its own front door ([`super::engine_data_plane_is_an_api`] — an Amazon Neptune
//! graph cluster), publish that address for every endpoint in front of that plane — the two
//! cluster endpoints and each CUSTOM endpoint ([`CLUSTER_CUSTOM_LABEL`]) — which is what
//! [`super::DbClusterRecord::endpoint`] reads. A cluster with no local plane at all has nothing
//! to lead a client to and keeps the published AWS form composed here.

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

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the two names composed here for one cluster differ only in the
    /// `cluster-`/`cluster-ro-` label, are derived from the cluster's own identifier, and
    /// share one suffix.
    #[test]
    fn the_two_endpoint_names_share_a_suffix_and_differ_in_their_role_label() {
        // The rows span what the composition generalises over: the identifier (short, long,
        // hyphenated), the region, and the endpoint domain of each control plane that owns an
        // engine `CreateDBCluster` accepts.
        let rows: &[(&str, &str, &str, &str, &str)] = &[
            ("an Aurora cluster", "sim2real-aur-1757", "abcdefghijkl", "us-west-2", "rds.amazonaws.com"),
            ("a one-label identifier", "db", "mnopqrstuvwx", "us-east-1", "rds.amazonaws.com"),
            ("another control plane's domain", "emu-docdb", "yz0123456789", "eu-central-1", "docdb.amazonaws.com"),
            ("the graph control plane's domain", "emu-neptune", "0123456789ab", "ap-south-1", "neptune.amazonaws.com"),
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
}
