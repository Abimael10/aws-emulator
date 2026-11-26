// SPDX-License-Identifier: CC0-1.0

//! Which Region's clusters an endpoint of this service resolves.
//!
//! Amazon ECS is a REGIONAL service — "Amazon ECS clusters are Region specific" — so a
//! cluster belongs to the one Region's endpoint it was created against: it is the only
//! endpoint that lists it, describes it, or acts in it, and every ARN that endpoint reports
//! carries that Region. The store keys `clusters` by `(region, clusterName)` for exactly
//! that reason, the way it keys `task_definitions` by `(region, family, revision)`, and
//! every lookup of a cluster goes through the accessors here: one cluster name is a cluster
//! of each Region separately, so no handler assembles the key itself and none of them can
//! read a cluster of another Region.
//!
//! The Region an accessor is given is the request's own ([`super::request_region`]) wherever
//! a request is being answered. Where the lookup is made for a RECORD this store already
//! holds — a clock pass, a derived count — the Region is read off the ARN that record
//! carries ([`cluster_region_of_arn`]), which is the Region the record was created in.

use super::{
    ClusterRecord, ComAmazonawsEcsAmazonec2containerservicev20141113State, CLUSTER_INACTIVE,
    MAX_CLUSTERS,
};

/// The Region an ARN this service minted carries, for a lookup made on behalf of a stored
/// record rather than of a request: a task, a container instance or a service holds the ARN
/// of its cluster, and that ARN's Region segment is the Region the cluster lives in.
///
/// An empty Region for a value that is not an ARN, which no Region's clusters are keyed
/// under, so such a value resolves to no cluster rather than to some other Region's.
pub fn cluster_region_of_arn(arn: &str) -> &str {
    ::service_common::arn::parse(arn).map_or("", |parsed| parsed.region)
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// The cluster record `cluster_name` names in `region`, whatever its status — the one
    /// lookup of a stored cluster.
    pub fn cluster(&self, region: &str, cluster_name: &str) -> Option<&ClusterRecord> {
        self.clusters
            .get(&(region.to_string(), cluster_name.to_string()))
    }

    /// The same, for a write.
    pub fn cluster_mut(&mut self, region: &str, cluster_name: &str) -> Option<&mut ClusterRecord> {
        self.clusters
            .get_mut(&(region.to_string(), cluster_name.to_string()))
    }

    /// The cluster `cluster_arn` names: the Region it carries, and the name it ends in.
    ///
    /// For the readers that hold a cluster's ARN rather than the pair — a stored record's
    /// `cluster_arn`, a tagging call's `resourceArn` — so they resolve the cluster of the
    /// Region the ARN names and of no other.
    pub fn cluster_of_arn(&self, cluster_arn: &str) -> Option<&ClusterRecord> {
        let region = cluster_region_of_arn(cluster_arn);
        let name = super::cluster_name_from_reference(Some(cluster_arn));
        self.cluster(region, &name)
    }

    /// Whether the store already holds as many clusters as it may ([`MAX_CLUSTERS`]) — the
    /// bound a caller cannot grow the store past, asked before a cluster is written.
    ///
    /// The count is over the WHOLE map, every Region's clusters together (the same
    /// `clusters.len()` the create asks about before it writes), so making a call under
    /// another Region's credential scope adds a KEY to this one budget rather than a budget of
    /// its own: the number of stored records is bounded however many Regions a caller signs
    /// for. Reads never write, so no Region a caller names materialises anything on its own
    /// either ([`Self::cluster`] and its neighbours all take `&self`).
    pub fn cluster_capacity_reached(&self) -> bool {
        self.clusters.len() >= MAX_CLUSTERS
    }

    /// Store `record` as the cluster of `region` it names. The one write of the map, so the
    /// key a cluster is stored under is always the Region the ARN on the record carries, and
    /// nothing is stored once the store holds its bound ([`Self::cluster_capacity_reached`]),
    /// whichever Region the write is for.
    pub fn insert_cluster(&mut self, region: &str, record: ClusterRecord) {
        if self.cluster_capacity_reached()
            && !self
                .clusters
                .contains_key(&(region.to_string(), record.name.clone()))
        {
            return;
        }
        self.clusters
            .insert((region.to_string(), record.name.clone()), record);
    }

    /// Every cluster of `region`, whatever its status — what a listing of that Region's
    /// endpoint walks, and nothing of any other Region's.
    pub fn clusters_of_region(&self, region: &str) -> impl Iterator<Item = &ClusterRecord> {
        let region = region.to_string();
        self.clusters
            .iter()
            .filter(move |((held, _), _)| *held == region)
            .map(|(_, record)| record)
    }

    /// The cluster an operation may act IN: one that exists in the request's Region and has
    /// not been deleted.
    ///
    /// A tombstone (`status` [`CLUSTER_INACTIVE`]) is not one — the `Cluster$status` doc:
    /// "The cluster has been deleted" — so every operation that resolves a `cluster` to
    /// work in refuses it with the same error code it answers for a name that never
    /// existed, and with the sentence that condition carries
    /// ([`super::cluster_refusal_message`]). The three operations that own the tombstone
    /// read the record directly instead ([`Self::cluster`]): `DescribeClusters` reports it,
    /// `DeleteCluster` answers idempotently with it, and `CreateCluster` creates a new
    /// cluster over it — and a describe that reports its answers per NAME resolves it
    /// through [`Self::holds_cluster`], since a name it cannot resolve is a failure it
    /// reports rather than a call it refuses.
    pub fn active_cluster(&self, region: &str, cluster_name: &str) -> Option<&ClusterRecord> {
        self.cluster(region, cluster_name)
            .filter(|cluster| cluster.status != CLUSTER_INACTIVE)
    }

    /// Whether [`Self::active_cluster`] names one — the existence check an operation that
    /// only needs to know the cluster is there makes.
    pub fn has_active_cluster(&self, region: &str, cluster_name: &str) -> bool {
        self.active_cluster(region, cluster_name).is_some()
    }

    /// Whether this store HOLDS a cluster record for `cluster_name` in `region` at all,
    /// whatever status it is in — the existence check a READ of the records that cluster
    /// held makes.
    ///
    /// The tombstone a delete leaves is still a record this account holds in that Region
    /// (`Cluster$status`: "Clusters with an INACTIVE status may remain discoverable in
    /// your account for a period of time"), and a read of what it held reports per record:
    /// a describe answers each name it cannot resolve in its `failures` channel, and a
    /// listing answers with the records that are still there to list. Only a reference that
    /// resolves to NO record of this Region leaves such a read nothing to answer about,
    /// which is the condition `ClusterNotFoundException` reports
    /// ([`super::CLUSTER_NOT_FOUND_MESSAGE`]).
    pub fn holds_cluster(&self, region: &str, cluster_name: &str) -> bool {
        self.cluster(region, cluster_name).is_some()
    }
}
