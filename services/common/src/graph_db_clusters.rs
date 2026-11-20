// SPDX-License-Identifier: CC0-1.0

//! The GRAPH DB CLUSTERS this emulator has provisioned: the engine each one runs and the
//! configuration a client gave it, written by the graph control plane and read by the graph
//! DATA plane.
//!
//! A graph database is two services: a control plane that CREATES a cluster (an engine
//! version, whether IAM database authentication is on, which logs the cluster publishes) and
//! a data plane addressed at that cluster's own endpoint, whose engine-status endpoint
//! REPORTS the engine the cluster was provisioned with and the features that configuration
//! turned on. The data plane configures nothing: it answers for a cluster somebody else
//! provisioned, so the only way it can report that cluster's engine and feature
//! configuration is for the control plane to have recorded it.
//!
//! This is that record: ONE directory in the shared engine, keyed by the cluster identifier
//! the control plane stores. Nothing here names a wire shape of either model — it holds the
//! configuration, and a reader projects it onto the members ITS model declares.
//!
//! The directory is a record of what this emulator PROVISIONED, so it is partial by
//! construction: a data plane that finds no cluster at all reports its own engine defaults
//! rather than inventing a cluster. It is BOUNDED (see [`MAX_PROVISIONED_CLUSTERS`]), so a
//! caller looping on create cannot grow it without end; the oldest registration is dropped
//! first, which keeps the cluster a live workflow just created resolvable.
//!
//! LOCK ORDER: this directory is taken LAST. A control plane holds its own store while it
//! records a cluster, and nothing here takes any other lock, so this store is never held
//! while another is taken.

use std::collections::{BTreeMap, VecDeque};
use std::time::SystemTime;

/// How many provisioned clusters the directory holds at once.
///
/// An EMULATOR ceiling, not an AWS quota: this directory lives in memory for the lifetime of
/// the process, so without a bound a caller creating clusters in a loop would grow it
/// without end. Past it the OLDEST registration is dropped — never a newer one — so the
/// cluster a running workflow just created is always still resolvable.
pub const MAX_PROVISIONED_CLUSTERS: usize = 1_000;

/// What one graph DB cluster was provisioned with, as its control plane recorded it.
///
/// Every field is a value the CREATE request settled, so a data plane reporting on this
/// cluster reports the configuration the cluster really has rather than a constant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphClusterConfiguration {
    /// The engine version the control plane provisioned (`DBCluster$EngineVersion`), exactly
    /// as it stored it.
    pub engine_version: String,
    /// Whether the cluster accepts IAM database authentication
    /// (`DBCluster$IAMDatabaseAuthenticationEnabled`).
    pub iam_database_authentication_enabled: bool,
    /// Whether the cluster publishes its AUDIT log, which is the audit-log configuration a
    /// create carries (`EnableCloudwatchLogsExports`).
    pub audit_log_enabled: bool,
    /// Whether the cluster publishes its SLOW QUERY log, from the same configuration.
    pub slow_query_log_enabled: bool,
    /// When the control plane provisioned the cluster.
    pub provisioned_at: SystemTime,
}

/// The directory itself: cluster identifier -> what it was provisioned with.
///
/// Registered in the shared engine under THIS type, so the control plane that provisions a
/// cluster and the data plane that answers for it read one directory instead of each keeping
/// its own copy.
#[derive(Debug, Default)]
pub struct GraphDbClusterDirectory {
    /// The configurations, by the identifier the control plane keys a cluster under.
    clusters: BTreeMap<String, GraphClusterConfiguration>,
    /// Registration order, so the ceiling drops the oldest entry first.
    order: VecDeque<String>,
}

impl GraphDbClusterDirectory {
    /// Record that the cluster `identifier` was provisioned with `configuration`.
    ///
    /// Re-recording an identifier REPLACES its configuration — which is what a cluster whose
    /// configuration was modified needs — and does not move its place in the eviction order.
    pub fn provision(&mut self, identifier: &str, configuration: GraphClusterConfiguration) {
        if self
            .clusters
            .insert(identifier.to_string(), configuration)
            .is_none()
        {
            self.order.push_back(identifier.to_string());
            while self.order.len() > MAX_PROVISIONED_CLUSTERS {
                if let Some(oldest) = self.order.pop_front() {
                    self.clusters.remove(&oldest);
                }
            }
        }
    }

    /// Drop the record of `identifier`: the cluster it named is gone, so the directory no
    /// longer claims an engine for it.
    pub fn forget(&mut self, identifier: &str) {
        if self.clusters.remove(identifier).is_some() {
            self.order.retain(|held| held != identifier);
        }
    }

    /// What the cluster `identifier` was provisioned with, or `None` when the directory holds
    /// no record of it.
    #[must_use]
    pub fn get(&self, identifier: &str) -> Option<&GraphClusterConfiguration> {
        self.clusters.get(identifier)
    }

    /// The configuration of the cluster provisioned MOST RECENTLY, or `None` while the
    /// directory holds none.
    ///
    /// One emulator process answers every cluster's data plane at its own one address
    /// (`crate::data_plane_addressing`), so a data-plane request carries no cluster of its
    /// own to resolve: the cluster it is addressing is the one a caller has just provisioned
    /// and then connected to. Clusters provisioned in the same instant are ordered by
    /// identifier, so the answer is total and does not depend on map iteration order.
    #[must_use]
    pub fn most_recently_provisioned(&self) -> Option<&GraphClusterConfiguration> {
        self.clusters
            .iter()
            .max_by(|(left_id, left), (right_id, right)| {
                left.provisioned_at
                    .cmp(&right.provisioned_at)
                    .then_with(|| left_id.cmp(right_id))
            })
            .map(|(_, configuration)| configuration)
    }

    /// How many provisioned clusters the directory holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.clusters.len()
    }

    /// Whether the directory holds no cluster at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.clusters.is_empty()
    }
}

/// A handle on one engine's [`GraphDbClusterDirectory`].
///
/// The ONE place the directory is reached from an engine, so the control plane that
/// provisions a cluster and the data plane that answers for it share this reading instead of
/// each spelling the downcast itself. Clone-cheap (an `Arc` inside) and `'static`.
#[derive(Clone)]
pub struct GraphDbClusters(::engine::ServiceState<GraphDbClusterDirectory>);

/// Written by hand rather than derived: the state handle inside carries no `Debug`, and what
/// is diagnosable about this handle is how many clusters it holds.
impl std::fmt::Debug for GraphDbClusters {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GraphDbClusters")
            .field("provisioned", &self.0.lock().len())
            .finish()
    }
}

impl GraphDbClusters {
    /// The directory of the SAME shared engine the services' own stores live in, or `None`
    /// for an engine that vends none — which is an engine nobody provisioned a cluster on.
    #[must_use]
    pub fn of<Eng: Send + Sync + 'static>(engine: &Eng) -> Option<Self> {
        (engine as &dyn ::std::any::Any)
            .downcast_ref::<::engine::InMemoryEngine>()
            .map(|engine| Self(engine.state::<GraphDbClusterDirectory>()))
    }

    /// Record that the cluster `identifier` was provisioned with `configuration`; see
    /// [`GraphDbClusterDirectory::provision`].
    pub fn provision(&self, identifier: &str, configuration: GraphClusterConfiguration) {
        self.0.lock().provision(identifier, configuration);
    }

    /// Drop the record of `identifier`; see [`GraphDbClusterDirectory::forget`].
    pub fn forget(&self, identifier: &str) {
        self.0.lock().forget(identifier);
    }

    /// What the cluster `identifier` was provisioned with, or `None` when the directory holds
    /// no record of it. Cloned out from under the lock, so nothing holds the directory while
    /// the caller reads the configuration.
    #[must_use]
    pub fn configuration(&self, identifier: &str) -> Option<GraphClusterConfiguration> {
        self.0.lock().get(identifier).cloned()
    }

    /// The configuration of the cluster a DATA-PLANE request is answering for; see
    /// [`GraphDbClusterDirectory::most_recently_provisioned`]. Cloned out from under the lock.
    #[must_use]
    pub fn answering_cluster(&self) -> Option<GraphClusterConfiguration> {
        self.0.lock().most_recently_provisioned().cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A configuration provisioned `seconds` after the epoch, with every switch off.
    fn provisioned(engine_version: &str, seconds: u64) -> GraphClusterConfiguration {
        GraphClusterConfiguration {
            engine_version: engine_version.to_string(),
            iam_database_authentication_enabled: false,
            audit_log_enabled: false,
            slow_query_log_enabled: false,
            provisioned_at: SystemTime::UNIX_EPOCH + Duration::from_secs(seconds),
        }
    }

    /// RULE: a cluster resolves to the configuration it was provisioned with until the
    /// control plane forgets it, and a cluster the directory never saw resolves to nothing.
    #[test]
    fn a_cluster_resolves_to_the_configuration_it_was_provisioned_with() {
        let mut directory = GraphDbClusterDirectory::default();
        directory.provision("graph-one", provisioned("1.3.2.1", 10));
        directory.provision(
            "graph-two",
            GraphClusterConfiguration {
                iam_database_authentication_enabled: true,
                audit_log_enabled: true,
                ..provisioned("1.4.8.0", 20)
            },
        );

        let rows: &[(&str, &str, Option<&str>)] = &[
            ("the first cluster", "graph-one", Some("1.3.2.1")),
            ("the second cluster", "graph-two", Some("1.4.8.0")),
            ("a cluster nothing provisioned", "graph-three", None),
        ];
        for (case, identifier, expected) in rows {
            let answered = directory
                .get(identifier)
                .map(|configuration| configuration.engine_version.clone());
            assert_eq!(
                answered.as_deref(),
                *expected,
                "case {case:?}: expected {expected:?}, got {answered:?}"
            );
        }

        let configuration = directory
            .get("graph-two")
            .expect("case \"the second cluster\": the directory holds it");
        assert_eq!(
            (
                configuration.iam_database_authentication_enabled,
                configuration.audit_log_enabled,
                configuration.slow_query_log_enabled
            ),
            (true, true, false),
            "case \"the second cluster\": expected the switches it was provisioned with, got \
             {configuration:?}"
        );

        directory.forget("graph-two");
        let answered = directory.get("graph-two");
        assert!(
            answered.is_none(),
            "case \"a deleted cluster\": expected nothing, got {answered:?}"
        );
    }

    /// RULE: the cluster provisioned most recently is the one a data plane answers for, and
    /// a tie is broken by identifier so the answer is total.
    #[test]
    fn the_most_recently_provisioned_cluster_is_the_one_answered_for() {
        let mut directory = GraphDbClusterDirectory::default();
        assert!(
            directory.most_recently_provisioned().is_none(),
            "case \"an empty directory\": expected nothing to answer for"
        );

        // Rows: the clusters in the directory, oldest first, and the engine version the
        // reading must answer. The middle row is the one provisioned latest, so provisioning
        // order and identifier order disagree on purpose.
        directory.provision("c-early", provisioned("1.3.2.1", 10));
        directory.provision("b-latest", provisioned("1.4.8.0", 30));
        directory.provision("a-middle", provisioned("1.4.5.0", 20));
        let answered = directory
            .most_recently_provisioned()
            .map(|configuration| configuration.engine_version.clone());
        assert_eq!(
            answered.as_deref(),
            Some("1.4.8.0"),
            "case \"three clusters\": expected the newest one's engine, got {answered:?}"
        );

        // Two clusters provisioned in the same instant: the identifier breaks the tie.
        directory.provision("z-tied", provisioned("1.4.9.0", 30));
        let answered = directory
            .most_recently_provisioned()
            .map(|configuration| configuration.engine_version.clone());
        assert_eq!(
            answered.as_deref(),
            Some("1.4.9.0"),
            "case \"a tie on the instant\": expected the later identifier's engine, got \
             {answered:?}"
        );
    }

    /// The directory is bounded: past the ceiling the OLDEST registration is dropped, and the
    /// cluster just provisioned is still resolvable.
    #[test]
    fn the_directory_is_bounded_and_drops_the_oldest_registration() {
        let mut directory = GraphDbClusterDirectory::default();
        for index in 0..MAX_PROVISIONED_CLUSTERS + 5 {
            directory.provision(&format!("graph-{index:05}"), provisioned("1.4.8.0", 1));
        }
        assert_eq!(
            directory.len(),
            MAX_PROVISIONED_CLUSTERS,
            "case \"past the ceiling\": expected {MAX_PROVISIONED_CLUSTERS} records, got {}",
            directory.len()
        );
        assert!(
            directory.get("graph-00000").is_none(),
            "case \"the oldest registration\": expected it to have been dropped"
        );
        let newest = format!("graph-{:05}", MAX_PROVISIONED_CLUSTERS + 4);
        assert!(
            directory.get(&newest).is_some(),
            "case \"the newest registration\": expected {newest} to still resolve"
        );
    }

    /// An engine this emulator did not build vends no directory, so a data plane reports its
    /// own defaults rather than taking the caller down.
    #[test]
    fn an_engine_that_vends_no_directory_resolves_no_cluster() {
        struct Elsewhere;
        assert!(
            GraphDbClusters::of(&Elsewhere).is_none(),
            "expected no directory for an engine of another kind"
        );
    }

    /// The handle every service reaches the directory through reads and writes the records of
    /// ONE engine, which is what lets the control plane that provisions a cluster and the
    /// data plane that answers for it be in two different crates.
    #[test]
    fn the_handle_reads_the_records_of_one_shared_engine() {
        let engine = ::engine::InMemoryEngine::new();
        let control_plane = GraphDbClusters::of(&engine).expect("the engine vends the directory");
        assert!(
            control_plane.answering_cluster().is_none(),
            "case \"nothing provisioned\": expected no cluster to answer for"
        );
        control_plane.provision("graph-one", provisioned("1.3.2.0", 10));
        control_plane.provision("graph-two", provisioned("1.4.8.0", 20));

        // A handle built again over the same engine reads the same records.
        let data_plane = GraphDbClusters::of(&engine).expect("the engine vends the directory");
        let rows: [(&str, Option<String>, Option<&str>); 2] = [
            (
                "the cluster provisioned first, by identifier",
                data_plane
                    .configuration("graph-one")
                    .map(|configuration| configuration.engine_version),
                Some("1.3.2.0"),
            ),
            (
                "the cluster a data-plane request answers for",
                data_plane
                    .answering_cluster()
                    .map(|configuration| configuration.engine_version),
                Some("1.4.8.0"),
            ),
        ];
        for (case, answered, expected) in rows {
            assert_eq!(
                answered.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {answered:?}"
            );
        }

        // Withdrawn with the cluster: the data plane answers for the one that is left.
        control_plane.forget("graph-two");
        let answered = data_plane
            .answering_cluster()
            .map(|configuration| configuration.engine_version);
        assert_eq!(
            answered.as_deref(),
            Some("1.3.2.0"),
            "case \"a deleted cluster\": expected the cluster that is left, got {answered:?}"
        );
    }
}
