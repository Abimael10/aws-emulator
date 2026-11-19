// SPDX-License-Identifier: CC0-1.0

//! A COPY of every database this service holds for ONE cluster, and the installation of such
//! a copy on another cluster.
//!
//! A DB cluster snapshot holds the data the cluster held when it was taken, and a cluster
//! restored from that snapshot serves exactly that data — the control plane that owns
//! snapshots (Amazon RDS) is what takes one and what restores it, and the databases a
//! cluster serves THROUGH THIS API are part of the data it holds. So this module is the
//! pair of operations that control plane needs: take a private copy of a cluster's
//! databases, and install such a copy as the databases another cluster holds.
//!
//! Both are COPIES made by the engine itself ([`Database::snapshot`], SQLite's own backup
//! API), so:
//!
//! * a copy is byte-identical to the database it was taken from, with the same schema, the
//!   same rows and the same sequence marks — nothing is re-rendered as SQL text and nothing
//!   is replayed;
//! * the copy is private: a statement run on the cluster after the copy was taken does not
//!   reach it, which is what makes a snapshot a snapshot;
//! * a copy can be installed any number of times, each time as a fresh copy, because
//!   restoring one snapshot twice creates two clusters whose later writes are their own;
//! * every bound a database here carries holds for a copy too — they are opened by the one
//!   confined constructor — and the per-cluster and per-database caps are enforced on the
//!   cluster a copy is installed on.
//!
//! A cluster this service holds nothing for yields no copy at all (`None`), which is what a
//! snapshot of a cluster nobody ran a Data API statement on holds: nothing.

use std::sync::Mutex;

use super::{
    Cluster, ComAmazonawsRdsdataRdsdataserviceState, Database, SqlError,
    MAX_DATABASES_PER_CLUSTER,
};

/// A private copy of every database one cluster holds, as a snapshot of that cluster carries
/// it.
///
/// Each database is kept behind its own lock: a copy is HELD by the control plane (on the
/// snapshot record it belongs to) and installed from there, possibly by several requests at
/// once, while the engine connection inside it is a single-threaded resource. Taking the
/// lock is what lets the copy be shared without being written to — every use of it below is
/// a read that produces a further copy.
#[derive(Debug)]
pub struct CapturedClusterData {
    /// The databases, each under the name the cluster holds it by (`""` is the default
    /// database, the one a request that names none addresses).
    databases: Vec<(String, Mutex<Database>)>,
}

impl ComAmazonawsRdsdataRdsdataserviceState {
    /// A private copy of every database this service holds for `resource_arn`, or `None`
    /// when it holds none at all.
    ///
    /// A READ of this store: the cluster is not created by being captured — an ARN nothing
    /// ever ran a statement against holds nothing, and a copy of nothing is nothing — so a
    /// capture leaves the store exactly as it was. The databases a transaction has not
    /// committed are NOT part of it: a snapshot holds what is committed, which is what every
    /// other reader of the cluster sees.
    pub fn capture_cluster_data(
        &self,
        resource_arn: &str,
    ) -> Result<Option<CapturedClusterData>, SqlError> {
        let Some(cluster) = self.clusters.get(resource_arn) else {
            return Ok(None);
        };
        let mut databases = Vec::new();
        for (name, database) in &cluster.databases {
            databases.push((name.clone(), Mutex::new(copy_of(database)?)));
        }
        if databases.is_empty() {
            return Ok(None);
        }
        Ok(Some(CapturedClusterData { databases }))
    }

    /// Make the databases `resource_arn` holds be exactly what `captured` carries: each one
    /// a fresh copy of the captured database of that name, and no other.
    ///
    /// This is a RESTORE, so it REPLACES: a cluster restored from a snapshot serves the
    /// snapshot's databases and nothing else, and one restored from a snapshot that holds
    /// nothing serves nothing. The copy itself is left intact, so the same snapshot can be
    /// restored again.
    ///
    /// The transactions open on the cluster are not touched: a transaction belongs to the
    /// cluster it was opened on, and the cluster this installs onto is one the control plane
    /// has just created.
    pub fn install_cluster_data(
        &mut self,
        resource_arn: &str,
        captured: Option<&CapturedClusterData>,
    ) -> Result<(), SqlError> {
        let databases = match captured {
            Some(captured) => &captured.databases[..],
            None => &[],
        };
        // The per-cluster cap holds for a copy as for anything else a request creates: a
        // copy larger than it is this emulator's own room running out, never the caller's
        // fault.
        if databases.len() > MAX_DATABASES_PER_CLUSTER {
            return Err(SqlError::Capacity(format!(
                "this emulator serves at most {MAX_DATABASES_PER_CLUSTER} databases per cluster"
            )));
        }
        // A cluster that would hold nothing is not brought into being: the store's own cap
        // on clusters is reached through `cluster_mut`, and naming one to install nothing on
        // it would take a capped slot for a cluster holding nothing.
        if databases.is_empty() && !self.clusters.contains_key(resource_arn) {
            return Ok(());
        }
        let mut restored = Vec::with_capacity(databases.len());
        for (name, database) in databases {
            let copy = copy_of(
                &database
                    .lock()
                    .unwrap_or_else(::std::sync::PoisonError::into_inner),
            )?;
            restored.push((name.clone(), copy));
        }
        let cluster: &mut Cluster = self.cluster_mut(resource_arn)?;
        cluster.databases.clear();
        for (name, database) in restored {
            cluster.databases.insert(name, database);
        }
        Ok(())
    }
}

/// One private copy of `database`: the engine's own copy of its schema and rows
/// ([`Database::snapshot`]), carrying over the one fact that copy does not — whether a
/// `CREATE DATABASE` brought the database into being, which is what makes a database the
/// cluster HAS even while it holds nothing.
fn copy_of(database: &Database) -> Result<Database, SqlError> {
    let mut copy = database.snapshot()?;
    if database.declared {
        copy.declare();
    }
    Ok(copy)
}
