// SPDX-License-Identifier: CC0-1.0

//! The data a DB cluster holds THROUGH THE RDS DATA API, as a snapshot of that cluster
//! carries it.
//!
//! A DB cluster snapshot is "a storage volume snapshot of your DB cluster", so it holds the
//! cluster's DATA — every database the cluster serves — and a cluster restored from it
//! serves exactly that data. One of the surfaces a client writes an Aurora cluster's
//! databases through is the RDS Data API, whose service OWNS the databases it serves: they
//! are its state, keyed by the ARN of the cluster a call addresses. So a snapshot taken
//! here has to carry those databases too, and a restore has to install them on the cluster
//! it creates — otherwise a client that wrote its rows through the Data API restores a
//! cluster that serves none of them.
//!
//! This module is that hand-off, and nothing else: the copy is MADE and INSTALLED by the
//! service that owns those databases
//! (`service_com_amazonaws_rdsdata_rdsdataservice::state::cluster_copies`, with the engine's
//! own copy), on the SAME shared engine this service's state lives in, and what travels
//! across is the opaque copy that service hands back. This service neither reads nor writes
//! a database of it.
//!
//! What this side DOES say is WHERE a copy is installed ([`DataApiClusterPlacement`]): a DB
//! cluster's databases are held in the cluster's own volume, which every surface of that
//! cluster opens a connection onto, and this service's control plane is what knows the
//! restored cluster's resource id and `DatabaseName` — the two facts that name it. That is
//! what makes the restored rows rows every door of the restored cluster serves, the Data API's
//! HTTP endpoint and the cluster's own engine endpoints alike.
//!
//! Both calls are AWAITED on a blocking task, exactly as the capture of a cluster's own
//! storage is ([`super::data_plane::capture_cluster_storage`]): copying a database is engine
//! work, so it belongs off the runtime's worker threads — and neither may be called with
//! this service's own store locked, because each takes the Data API's store. `Err` carries
//! what went wrong, which a handler answers as a failure of this emulator rather than
//! storing a snapshot that holds less than the cluster did.
//!
//! An emulator assembled WITHOUT the Data API has no such databases, and a cluster nobody
//! ran a Data API statement against holds none: both answer `Ok(None)`, which is what a
//! snapshot of such a cluster carries.

use std::sync::Arc;

/// The copy itself, as the service that owns those databases hands it over: opaque here, and
/// kept on the snapshot record that holds it ([`super::DbClusterSnapshotRecord`]).
pub type DataApiClusterData =
    ::service_com_amazonaws_rdsdata_rdsdataservice::state::CapturedClusterData;

/// WHERE such a copy is installed: the volume of the cluster it is installed ON, and the
/// database that cluster was created with.
///
/// A DB cluster's databases are held in one backing per database that every surface of the
/// cluster opens a connection onto ([`::service_common::cluster_sql_volumes`]) — this
/// service's own engine endpoints and the Data API's HTTP endpoint alike — so a restore
/// installs the snapshot's databases THERE. That is what makes the restored rows rows every
/// door of the restored cluster serves, exactly as the cluster's own wire storage is loaded
/// into the volume its endpoint was opened on ([`super::data_plane::sql_engine::InstanceDatabase::load_from`]).
pub type DataApiClusterPlacement =
    ::service_com_amazonaws_rdsdata_rdsdataservice::state::ClusterPlacement;

/// A COPY of every database the Data API holds for the DB cluster `cluster_arn` names,
/// right now — what a DB cluster snapshot of that cluster carries.
///
/// `Ok(None)` for a cluster the Data API holds no database for, which is a snapshot that
/// carries none. Call it BEFORE this service's store is locked, as the storage capture
/// beside it is.
pub async fn capture_data_api_cluster_data<Eng: Send + Sync + 'static>(
    engine: &Eng,
    cluster_arn: &str,
) -> Result<Option<Arc<DataApiClusterData>>, String> {
    let Some(store) = ::service_com_amazonaws_rdsdata_rdsdataservice::state::service_state(engine)
    else {
        return Ok(None);
    };
    let cluster_arn = cluster_arn.to_string();
    let captured = ::tokio::task::spawn_blocking(move || {
        store.lock().capture_cluster_data(&cluster_arn)
    })
    .await
    .map_err(|error| format!("the DB cluster's Data API databases could not be copied: {error}"))?;
    match captured {
        Ok(captured) => Ok(captured.map(Arc::new)),
        Err(error) => Err(format!(
            "the DB cluster's Data API databases could not be copied: {}",
            error.message()
        )),
    }
}

/// Make the databases the Data API serves for the DB cluster `cluster_arn` names be exactly
/// what `captured` holds — a fresh copy of each, and no other.
///
/// This is the restore half: a cluster restored from a snapshot serves the snapshot's
/// databases, and one restored from a snapshot that carries none serves none. `captured` is
/// left intact, so the same snapshot restores again. Call it with this service's store
/// unlocked.
///
/// `resource_id` is the restored cluster's own `DbClusterResourceId` and `default_database`
/// its `DatabaseName`: together they name the storage the copy is installed in
/// ([`DataApiClusterPlacement`]), so the restored rows are rows every surface of that
/// cluster reads — the Data API's HTTP endpoint and the cluster's engine endpoints.
pub async fn install_data_api_cluster_data<Eng: Send + Sync + 'static>(
    engine: &Eng,
    cluster_arn: &str,
    resource_id: &str,
    default_database: Option<&str>,
    captured: Option<Arc<DataApiClusterData>>,
) -> Result<(), String> {
    let Some(store) = ::service_com_amazonaws_rdsdata_rdsdataservice::state::service_state(engine)
    else {
        // Nothing serves those databases in this emulator, so there is nothing to install
        // them on — and a snapshot that carries none asks for nothing either way.
        return Ok(());
    };
    // The cluster's own volume, named by the resource id of the cluster the copy is installed
    // on — the same naming its engine endpoint's databases are opened under, so the two
    // surfaces address one storage.
    let placement = DataApiClusterPlacement {
        volume: ::service_common::cluster_sql_volumes::ClusterVolumes::of(engine)
            .map(|volumes| volumes.cluster(resource_id)),
        default_database: default_database.map(ToString::to_string),
    };
    let cluster_arn = cluster_arn.to_string();
    let installed = ::tokio::task::spawn_blocking(move || {
        store
            .lock()
            .install_cluster_data(&cluster_arn, captured.as_deref(), &placement)
    })
    .await
    .map_err(|error| {
        format!("the DB cluster's Data API databases could not be restored: {error}")
    })?;
    installed.map_err(|error| {
        format!(
            "the DB cluster's Data API databases could not be restored: {}",
            error.message()
        )
    })
}
