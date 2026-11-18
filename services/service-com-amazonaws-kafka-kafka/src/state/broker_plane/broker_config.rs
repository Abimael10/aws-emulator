// SPDX-License-Identifier: CC0-1.0

//! The BROKER-level configuration a cluster's brokers run with.
//!
//! An Amazon MSK cluster carries an MSK CONFIGURATION — a `server.properties` bundle
//! stored as a revision of a configuration resource (`CreateConfiguration`,
//! `UpdateClusterConfiguration`) — and what that bundle states is what the cluster's
//! brokers run with: the properties it declares override Apache Kafka's own documented
//! broker defaults, and a property it does not declare keeps the default.
//!
//! That matters to a CLIENT, not only to a reader of the control plane: a broker applies
//! its own configuration wherever the protocol lets a client leave a decision to it — the
//! `num.partitions` and `default.replication.factor` a `CreateTopics` request asks the
//! broker to choose, and the values a `DescribeConfigs` on the broker resource reports.
//! So the bundle is read here, once, and the data plane answers from it.

use std::collections::BTreeMap;
use std::time::SystemTime;

use super::*;

/// The broker-level property that says how many partitions a topic created without a
/// partition count gets, and the one that says how many replicas of each partition such
/// a topic gets ("Broker Configs", Apache Kafka documentation).
pub const NUM_PARTITIONS_CONFIG: &str = "num.partitions";
/// See [`NUM_PARTITIONS_CONFIG`].
pub const DEFAULT_REPLICATION_FACTOR_CONFIG: &str = "default.replication.factor";

/// The configuration a cluster's brokers run with: the entries in force, and the ones the
/// cluster's own MSK configuration STATES — both halves, because a client that asks a
/// broker for its configuration is told which values it was configured with and which are
/// Apache Kafka's defaults.
#[derive(Debug, Clone, Default)]
pub struct BrokerConfiguration {
    /// Every entry in force, whether the cluster's configuration states it or it is a
    /// default.
    pub effective: BTreeMap<String, String>,
    /// Only the entries the cluster's configuration revision states.
    pub stated: BTreeMap<String, String>,
}

impl BrokerConfiguration {
    /// The value `name` is in force at, read as a number — the form every broker property
    /// this data plane acts on is stated in. `None` when nothing in force states a
    /// readable number for it.
    #[must_use]
    pub fn number(&self, name: &str) -> Option<i32> {
        self.effective
            .get(name)
            .and_then(|value| value.trim().parse::<i32>().ok())
    }
}

/// The configuration the brokers of `cluster` run with at `now`: Apache Kafka's
/// documented broker defaults ([`super::BROKER_CONFIG_DEFAULTS`]) overridden by the
/// properties of the MSK configuration revision the cluster carries.
///
/// A cluster that carries no configuration — or one whose configuration has left the
/// registry, or that names a revision it no longer holds — runs with the defaults alone,
/// which is what a cluster created without one runs with.
pub fn in_force(
    store: &ComAmazonawsKafkaKafkaState,
    cluster: &Cluster,
    now: SystemTime,
) -> BrokerConfiguration {
    let mut effective: BTreeMap<String, String> = BROKER_CONFIG_DEFAULTS
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect();
    let stated = stated_properties(store, cluster, now);
    effective.extend(stated.clone());
    BrokerConfiguration { effective, stated }
}

/// The properties the MSK configuration revision `cluster` carries declares, read from
/// the `server.properties` body the revision stored.
fn stated_properties(
    store: &ComAmazonawsKafkaKafkaState,
    cluster: &Cluster,
    now: SystemTime,
) -> BTreeMap<String, String> {
    let Some(carried) = cluster.configuration_info.as_ref() else {
        return BTreeMap::new();
    };
    let Some(configuration) = store.configuration_in(&cluster.region, &carried.arn, now) else {
        return BTreeMap::new();
    };
    let Some(revision) = configuration.revision(carried.revision) else {
        return BTreeMap::new();
    };
    // `server.properties` is the `name=value` form a Kafka configuration file is written
    // in, read by the same reader this service reads a submitted body with.
    ::java_properties::read(revision.server_properties.as_slice())
        .map(|read| read.into_iter().collect())
        .unwrap_or_default()
}
