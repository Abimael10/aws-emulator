// SPDX-License-Identifier: CC0-1.0

//! The STORAGE of each Amazon DocumentDB cluster, and the credential its endpoints
//! authenticate a client against — held ONCE per cluster, for every view of the one front
//! door that serves it.
//!
//! Amazon DocumentDB, Amazon Neptune and Amazon RDS are three views of one AWS front door
//! (`src/service.rs`): a request that names DocumentDB itself is served from this crate, and
//! an AWS SDK DocumentDB client signs `rds`, so its traffic is served from the Amazon RDS
//! model of the same door. Both create the SAME AWS resource, so the documents a cluster
//! holds and the master credential its endpoints check a login against belong to the CLUSTER
//! rather than to whichever view created it — one store per AWS resource, not one per crate.
//!
//! The records live in the SHARED engine, keyed by type, so each view reaches the same ones
//! ([`ClusterDocuments::of`]); they are keyed by the cluster's `DbClusterResourceId`, which
//! is "Region-unique, immutable", so a rename of the cluster does not move its storage and
//! two clusters can never share it.
//!
//! LOCK ORDER: this store is taken LAST. A writer holds its own service's store while it
//! records a cluster's storage, and nothing here takes any other lock.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::auth::ScramCredentials;
use super::SharedVolume;

/// What one cluster holds: its documents, and the credential its endpoints authenticate a
/// client against.
#[derive(Debug, Default)]
struct ClusterStorage {
    volume: SharedVolume,
    /// The master user's SCRAM credentials, or `None` for a cluster created with no master
    /// user — whose endpoints then have no credential to check a client against.
    credentials: Option<ScramCredentials>,
}

/// The documents and credentials of every Amazon DocumentDB cluster this emulator holds.
#[derive(Debug, Default)]
pub struct ClusterDocuments {
    clusters: BTreeMap<String, ClusterStorage>,
}

impl ClusterDocuments {
    /// This store in the shared engine: call it with a handler's `&*self.engine`.
    ///
    /// `None` on an engine this emulator did not build, which vends no such store — and then
    /// a cluster has no storage here, which is the position every cluster was in before one
    /// was recorded.
    #[must_use]
    pub fn of<Eng: Send + Sync + 'static>(
        engine: &Eng,
    ) -> Option<::engine::ServiceState<ClusterDocuments>> {
        (engine as &dyn ::std::any::Any)
            .downcast_ref::<::engine::InMemoryEngine>()
            .map(::engine::InMemoryEngine::state::<ClusterDocuments>)
    }

    /// The documents of the cluster whose `DbClusterResourceId` is `resource_id`, minted on
    /// first use: a cluster's storage comes into being with the first endpoint that serves it.
    pub fn volume(&mut self, resource_id: &str) -> SharedVolume {
        Arc::clone(&self.storage(resource_id).volume)
    }

    /// Record the credential the cluster's endpoints authenticate a client against, which its
    /// create derived from the master password and a modify of that password replaces.
    pub fn set_credentials(&mut self, resource_id: &str, credentials: Option<ScramCredentials>) {
        self.storage(resource_id).credentials = credentials;
    }

    /// The credential the cluster's endpoints authenticate a client against.
    #[must_use]
    pub fn credentials(&self, resource_id: &str) -> Option<ScramCredentials> {
        self.clusters
            .get(resource_id)
            .and_then(|storage| storage.credentials.clone())
    }

    /// Drop everything one cluster held, which the delete of the cluster does: the storage is
    /// the cluster's, so nothing of it outlives the cluster.
    pub fn forget(&mut self, resource_id: &str) {
        self.clusters.remove(resource_id);
    }

    /// How many clusters hold storage here, which is what bounds this map: one entry per
    /// cluster either view of the front door created.
    #[must_use]
    pub fn len(&self) -> usize {
        self.clusters.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.clusters.is_empty()
    }

    fn storage(&mut self, resource_id: &str) -> &mut ClusterStorage {
        self.clusters.entry(resource_id.to_string()).or_default()
    }
}
