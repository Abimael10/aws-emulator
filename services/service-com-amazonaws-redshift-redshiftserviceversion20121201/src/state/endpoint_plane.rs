// SPDX-License-Identifier: CC0-1.0

//! The DATA plane of one provisioned cluster: the connection endpoint
//! `Cluster$Endpoint` publishes, and what admits a session on it.
//!
//! `Endpoint$Address` is "the DNS address of the cluster" and `Endpoint$Port`
//! "the port that the database engine is listening on"
//! (`smithy_query(query="shape", shape="Endpoint")`), and a client that reads
//! them opens a PostgreSQL session there with the credentials the control plane
//! handed it. So this emulator SERVES that endpoint, on the one shared
//! statement of a SQL-wire data plane ([`::service_common::sql_wire_planes`]):
//! the core opens the listener, the address and port a cluster publishes are
//! that listener's own, and the connections accepted on it are served by the
//! server half of the PostgreSQL frontend/backend protocol over the cluster's
//! own in-process SQL engine.
//!
//! What this module adds is the Redshift half of it:
//!
//! * which cluster one listener belongs to — named by its NAMESPACE ARN, which
//!   identifies the cluster's data and survives a rename, so a `ModifyCluster`
//!   that renames the cluster leaves its sessions exactly where they were;
//! * the STORED cluster a login is admitted against ([`ClusterFacts`]) — the
//!   admin user and its current password, the database the create named, the
//!   engine version, and the lifecycle state, each read out of the store when a
//!   login or a statement arrives rather than captured when the listener
//!   opened, so a cluster that is still being provisioned or has been paused
//!   refuses a session exactly as its engine would;
//! * the TEMPORARY credentials `GetClusterCredentials` and
//!   `GetClusterCredentialsWithIAM` minted for that cluster's database
//!   ([`IssuedDatabaseCredential`]), which log on until they expire — the
//!   lifetime those operations promise, made real.
//!
//! The listener is held on the cluster's own record
//! ([`StoredCluster::endpoint_plane`]), so the port stays open for as long as
//! the cluster does and closes with it: a `DeleteCluster` drops that record and
//! with it the listener and the engine.
//!
//! WHAT THIS PLANE'S ENGINE IS NOT: a cluster's databases as the Redshift Data
//! API runs statements on them live in that service's own engine
//! ([`super::cluster_data`]), which is also what a snapshot captures and a
//! restore writes back. A session on this endpoint therefore reads and writes
//! the engine behind THIS listener, not that one: a table created here is not
//! visible to `ExecuteStatement` and is not in a snapshot, and a restored
//! cluster's copied data is not visible here. Joining them needs a
//! service-agnostic statement-runner seam in
//! [`::service_common::sql_wire_planes`] — whose session type is bound to its
//! own engine — so it is a change to that shared module rather than to this
//! service, and is left to its own issue.

use std::sync::Arc;

use ::service_common::sql_wire_planes::{
    self, DatabaseFacts, EngineWire, IssuedCredential, ServedDatabase, ServingState,
};

use super::*;

/// The SigV4 signing name of this service, which is what names the service
/// answering at an address it opened for an in-process client.
const SIGNING_NAME: &str = "redshift";

/// The `server_version` a cluster's database endpoint reports, which is what a
/// client reads to decide what the backend understands.
///
/// Source: the Amazon Redshift Database Developer Guide, "Amazon Redshift and
/// PostgreSQL" — "Amazon Redshift is based on PostgreSQL 8.0.2" — which is the
/// version a Redshift backend reports for itself. The cluster's own
/// `ClusterVersion` ("Only version 1.0 is currently available",
/// [`DEFAULT_CLUSTER_VERSION`]) is the Redshift engine version and not a
/// PostgreSQL one, so it is not what this parameter carries.
pub const CLUSTER_SERVER_VERSION: &str = "8.0.2";

/// One cluster's open data plane, as this store holds it: where it answers,
/// what answers there, and the SQL engine behind it.
///
/// Dropping it closes the port and severs every session on it.
pub type ClusterEndpointPlane = sql_wire_planes::OpenedSqlPlane;

/// A temporary database credential this service ISSUED for one cluster's
/// database, and that the cluster's endpoint therefore authenticates until it
/// expires.
///
/// `GetClusterCredentials` "returns a database user name and temporary password
/// with temporary authorization to log on to an Amazon Redshift database", and
/// `Expiration` is "the date and time the password in DbPassword expires": the
/// pair is a LOGIN for that long, so what was issued is held here and the wire
/// admits it ([`ClusterFacts::issued_credentials`]). The record is dropped once
/// its lifetime has run out, which the clock owes
/// ([`ComAmazonawsRedshiftRedshiftserviceversion20121201State::settle`]).
#[derive(Clone)]
pub struct IssuedDatabaseCredential {
    /// The database user the credential logs on as, exactly as the response
    /// reported it (with the prefix the operation documents).
    pub db_user: String,
    /// The temporary password the response carried, which is the secret a login
    /// presents. Never reported by any read of this store and never logged.
    pub db_password: String,
    /// The database the request scoped the credential to
    /// (`GetClusterCredentials$DbName`), and `None` for a request that named
    /// none — "if DbName is not specified, DbUser can log on to any existing
    /// database".
    pub db_name: Option<String>,
    /// When the credential expires, which is the `Expiration` the response
    /// reported, measured on the emulator's own clock ([`Clock`]).
    pub expires_at: Instant,
}

/// Written by hand rather than derived: the temporary password is a secret and
/// no formatter writes it.
impl std::fmt::Debug for IssuedDatabaseCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IssuedDatabaseCredential")
            .field("db_user", &self.db_user)
            .field("db_name", &self.db_name)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// The stored cluster one listener belongs to.
///
/// Holds the store and the cluster's KEY rather than the record, so every login
/// and every statement is admitted against the cluster as it stands at that
/// moment — with every clock-owed transition already applied, exactly as a
/// control-plane read sees it.
pub struct ClusterFacts {
    state: ::engine::ServiceState<ComAmazonawsRedshiftRedshiftserviceversion20121201State>,
    clock: ::engine::ServiceState<Clock>,
    key: String,
}

impl ClusterFacts {
    /// Run `body` against the cluster this listener belongs to, with every
    /// clock-owed transition already applied — the same view a control-plane
    /// read of that cluster gets.
    fn with_cluster<R>(&self, body: impl FnOnce(&StoredCluster) -> R) -> Option<R> {
        let now = self.clock.lock().now();
        let mut store = self.state.lock();
        store.settle(now);
        locate_cluster(&store, &self.key).map(body)
    }
}

/// The cluster a plane's key names: the cluster whose NAMESPACE this ARN names,
/// or — for a record that carries no namespace ARN — the one with this
/// identifier.
///
/// A namespace ARN and a cluster identifier can never be the same string (an
/// ARN carries colons, which no identifier may), so one chain resolves both
/// forms unambiguously.
fn locate_cluster<'a>(
    store: &'a ComAmazonawsRedshiftRedshiftserviceversion20121201State,
    key: &str,
) -> Option<&'a StoredCluster> {
    store
        .cluster_with_namespace_arn(key)
        .or_else(|| store.cluster(key))
}

impl ServedDatabase for ClusterFacts {
    fn facts(&self) -> Option<DatabaseFacts> {
        self.with_cluster(|stored| DatabaseFacts {
            name: stored
                .cluster
                .cluster_identifier
                .clone()
                .unwrap_or_default(),
            serving: serving_state(stored.cluster.cluster_status.as_deref().unwrap_or_default()),
            master_username: stored.cluster.master_username.clone().unwrap_or_default(),
            // The admin password as the store holds it NOW, so a password a
            // `ModifyCluster` replaced is the one the next login is verified
            // against. A cluster whose admin password this service does not
            // hold (`ManageMasterPassword`) reports none, and the wire then
            // admits no admin login on it — only the temporary credentials
            // below.
            master_user_password: stored.master_user_password.clone().unwrap_or_default(),
            master_database_name: stored.cluster.db_name.clone().unwrap_or_default(),
            server_version: CLUSTER_SERVER_VERSION.to_string(),
        })
    }

    fn issued_credentials(&self) -> Vec<IssuedCredential> {
        self.with_cluster(|stored| {
            stored
                .issued_credentials
                .iter()
                .map(|issued| IssuedCredential {
                    user: issued.db_user.clone(),
                    password: issued.db_password.clone(),
                    database: issued.db_name.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
    }
}

/// Whether a cluster in `status` is serving, as the wire reads it.
///
/// Only a cluster that is available for queries accepts a session, which is
/// what AWS reports a client has to wait for — every `ClusterStatus` that
/// starts with `available` ([`availability_status`] classifies the published
/// vocabulary the same way). Of the states that do not, the wire tells a client
/// which KIND it is: a cluster the service is still bringing up or changing
/// ends that work serving, so a client that waits is right to; one that is
/// paused, deleting or broken does not.
pub fn serving_state(status: &str) -> ServingState {
    if status.starts_with(STATUS_AVAILABLE) {
        return ServingState::Serving;
    }
    match status {
        STATUS_CREATING | STATUS_MODIFYING | STATUS_REBOOTING | STATUS_ROTATING_KEYS => {
            ServingState::ComingUp
        }
        _ => ServingState::GoingDown,
    }
}

/// Open the data plane of the cluster `cluster_identifier`: its own SQL engine,
/// served on the PostgreSQL wire, on a listener the core opens.
///
/// The core binds the socket and assigns the port; this hands it the protocol
/// handler and reports back where it landed. The caller stores the plane — which
/// is what keeps the port open — and publishes [`ClusterEndpointPlane::endpoint`]
/// as the cluster's `Endpoint` ([`attach_cluster_endpoint`]).
///
/// `Err` when this emulator's engine has no store or no listener surface, when
/// the core could not bind, or when the SQL engine could not be started: the
/// caller answers that with a modelled failure rather than creating a cluster
/// whose published address answers nothing.
pub async fn open_cluster_endpoint_plane<Eng: Send + Sync + 'static>(
    engine: &Eng,
    cluster_key: &str,
) -> Result<ClusterEndpointPlane, String> {
    let state = service_state(engine)
        .ok_or_else(|| "this emulator's engine does not vend this service's store".to_string())?;
    let clock = ::service_common::clock::clock_store(engine)
        .ok_or_else(|| "this emulator's engine does not vend the shared clock".to_string())?;
    let served = Arc::new(ClusterFacts {
        state,
        clock,
        key: cluster_key.to_string(),
    });
    sql_wire_planes::open_sql_wire_plane(engine, EngineWire::Postgres, served, SIGNING_NAME).await
}

/// Open the data plane of the cluster `cluster_identifier` and publish it as
/// that cluster's connection endpoint.
///
/// Called by every operation that ALLOCATES a cluster (`CreateCluster`,
/// `RestoreFromClusterSnapshot`), with the store's lock released, because
/// binding is asynchronous and the core owns the socket, the bind interface and
/// the port. From here on the cluster's stored endpoint is the listener's own
/// address and port, which is what a read publishes once the cluster is serving
/// (`StoredCluster::publish_endpoint`) — so no cluster of this service ever
/// announces an address nothing answers at.
///
/// `Err` carries why no endpoint could be opened; the caller then removes the
/// cluster it had allocated and answers a modelled failure.
pub async fn attach_cluster_endpoint<Eng: Send + Sync + 'static>(
    engine: &Eng,
    cluster_identifier: &str,
) -> Result<(), String> {
    let state = service_state(engine)
        .ok_or_else(|| "this emulator's engine does not vend this service's store".to_string())?;
    // The key the plane reads its cluster back by: the cluster's own namespace
    // ARN, which a rename leaves exactly as it was minted.
    let key = {
        let store = state.lock();
        let stored = store.cluster(cluster_identifier).ok_or_else(|| {
            format!("the cluster {cluster_identifier} was gone before its endpoint was opened")
        })?;
        stored
            .cluster
            .cluster_namespace_arn
            .clone()
            .unwrap_or_else(|| cluster_identifier.to_string())
    };
    let plane = Arc::new(open_cluster_endpoint_plane(engine, &key).await?);
    let served = plane.endpoint().clone();
    let mut store = state.lock();
    let Some(position) = store.cluster_position(cluster_identifier) else {
        return Err(format!(
            "the cluster {cluster_identifier} was gone before its endpoint could be published"
        ));
    };
    if let Some(stored) = store.clusters.get_mut(position) {
        // The plane answers on the port the CORE assigned it, so that is the
        // port the cluster publishes and the one a client dials; the port a
        // request asked for names no socket this emulator serves.
        stored.endpoint.address = Some(served.address.clone());
        stored.endpoint.port = Some(served.port);
        stored.endpoint_plane = Some(plane);
        // A cluster that is already serving publishes the endpoint it is served
        // on from now on — including one the clock settled while this listener
        // was being opened, which published none because there was nothing yet
        // to announce. One still coming up publishes none until it is up.
        if serving_state(stored.cluster.cluster_status.as_deref().unwrap_or_default())
            == ServingState::Serving
        {
            stored.publish_endpoint();
        }
    }
    Ok(())
}
