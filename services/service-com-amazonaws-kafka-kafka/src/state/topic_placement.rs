// SPDX-License-Identifier: CC0-1.0

//! What Apache Kafka can place a topic's partition replicas ON.
//!
//! A partition is replicated onto as many brokers as its replication factor
//! names, and each of those replicas has to sit on a broker that is UP: Apache
//! Kafka's controller assigns the replicas of every partition it is asked to
//! place over the brokers available to it at that instant, and refuses an
//! assignment it cannot make. That is one rule over two counts — the factor
//! asked for and the brokers available — so it lives here, beside the state
//! module rather than in it, and every caller that places partitions applies it:
//! the control plane's `CreateTopic` and `UpdateTopic`, and the `CreateTopics`
//! and `CreatePartitions` the cluster's own brokers serve.
//!
//! How many brokers a cluster has available is the cluster record's own answer
//! ([`super::Cluster::available_broker_count`]), since a node a reboot operation
//! has in flight is not one the controller can place a replica on.

/// The sentence Apache Kafka refuses a replica assignment it cannot place with:
/// fixed text around the two counts the controller compared — the factor asked
/// for, and the brokers it had to place those replicas on.
///
/// Both numbers come from state (the factor the topic carries or the request
/// named, and the cluster's available broker count); nothing else of the request
/// is echoed.
pub fn replication_factor_shortfall_message(
    replication_factor: i32,
    available_brokers: i32,
) -> String {
    format!(
        "Replication factor: {replication_factor} larger than available brokers: \
         {available_brokers}."
    )
}

/// Why `replication_factor` replicas of a partition cannot be placed on the
/// brokers a cluster has available, or `None` when they can.
///
/// One replica per broker, so a factor above the number of brokers available
/// describes a placement Apache Kafka cannot make. `available_brokers` is `None`
/// for a cluster that publishes no broker nodes of its own — an Amazon MSK
/// Serverless cluster's brokers are the service's — and such a cluster bounds
/// nothing here: there is no count to compare against, so the KIND of cluster is
/// what decides that call rather than a count it does not report.
pub fn replication_factor_shortfall(
    replication_factor: i32,
    available_brokers: Option<i32>,
) -> Option<String> {
    available_brokers
        .filter(|available| replication_factor > *available)
        .map(|available| replication_factor_shortfall_message(replication_factor, available))
}
