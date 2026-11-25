// SPDX-License-Identifier: CC0-1.0

//! The DATA plane of one workgroup: the connection endpoint `Workgroup$endpoint`
//! publishes, and what admits a session on it.
//!
//! `Endpoint$address` is "the DNS address of the VPC endpoint" and `Endpoint$port`
//! "the port that Amazon Redshift Serverless listens on"
//! (`smithy_query(query="shape", shape="Endpoint")`), and a client that reads them
//! opens a Redshift session there with a stock PostgreSQL driver. So this emulator
//! SERVES that endpoint, on the one shared statement of a SQL-wire data plane
//! ([`::service_common::sql_wire_planes`]): the core opens the listener (it owns the
//! socket, the bind interface, the port range, the handshake deadline, the idle
//! timeout and the connection caps), the ADDRESS a workgroup publishes is the host
//! that listener is bound on — this emulator's own, never a public AWS DNS name it
//! does not serve — and the connections accepted on it are served by the server half
//! of the PostgreSQL frontend/backend protocol over the workgroup's own in-process
//! SQL engine. The PORT it publishes is the one the workgroup itself listens on
//! (`Workgroup$port`, "the custom port to use when connecting to a workgroup", whose
//! default the model documents), and the published pair is recorded as an address
//! this plane answers at too, so a client of this emulator that dials what the
//! control plane handed it reaches this workgroup's wire
//! ([`::service_common::dialable_data_planes`]).
//!
//! What this module adds is the Redshift Serverless half of it:
//!
//! * the STORED workgroup a session is admitted against ([`WorkgroupFacts`]) — read
//!   out of the store, with every clock-owed transition already applied, when a login
//!   or a statement arrives rather than captured when the listener opened. So a
//!   workgroup that is still being created refuses a session, one being deleted
//!   refuses one, and a workgroup that has gone away ends the sessions on it, exactly
//!   as its own status says;
//! * WHOSE credentials log on: a workgroup is the compute of a NAMESPACE, and the
//!   namespace carries the administrator of the first database created in it
//!   (`Namespace$adminUsername`, `Namespace$dbName`), so the account a session logs on
//!   as and the database it uses are read off that namespace as it stands now — a
//!   password a later `UpdateNamespace` replaced is the one the next login is verified
//!   against. A namespace whose admin password this service does not hold
//!   (`manageAdminPassword`, whose password lives in the Secrets Manager secret the
//!   namespace reports) has none to compare a login with, so the wire admits none on
//!   it rather than admitting any;
//! * which DIALECT of the PostgreSQL protocol the endpoint answers in
//!   ([`::service_common::sql_wire_planes::EngineWire::Redshift`]): the server behind
//!   it is a data warehouse and not a PostgreSQL server, so `version()` names the
//!   warehouse.
//!
//! The listener is held on the workgroup's own record
//! ([`crate::state::WorkgroupRecord::endpoint_plane`]), so the port stays open for as
//! long as the workgroup does and closes with it: a `DeleteWorkgroup` drops that
//! record and with it the listener, the sessions on it and the engine.
//!
//! WHAT THIS PLANE'S ENGINE IS NOT: a namespace's databases as the Redshift Data API
//! runs statements on them live in that service's own engine (the one
//! [`crate::state::capture_namespace_data`] copies), which is also what a snapshot
//! holds and a restore writes back. A session on this endpoint therefore reads and
//! writes the engine behind THIS listener, not that one. Joining them needs a
//! service-agnostic statement-runner seam in
//! [`::service_common::sql_wire_planes`] — whose session type is bound to its own
//! engine — so it is a change to that shared module rather than to this service, and
//! is left to its own issue; the provisioned Amazon Redshift endpoint states the same
//! boundary for the same reason.

use std::sync::Arc;

use ::service_common::sql_wire_planes::{
    self, DatabaseFacts, EngineWire, ServedDatabase, ServingState,
};

use crate::state::{
    ComAmazonawsRedshiftserverlessRedshiftserverlessState, NamespaceRecord, WorkgroupRecord,
    ARN_NAMESPACE, WORKGROUP_AVAILABLE, WORKGROUP_CREATING, WORKGROUP_MODIFYING,
};

/// The `server_version` a workgroup's endpoint reports, which is what a client reads
/// to decide what the backend understands.
///
/// Source: the Amazon Redshift Database Developer Guide, "Amazon Redshift and
/// PostgreSQL" — "Amazon Redshift is based on PostgreSQL 8.0.2" — which is the version
/// a Redshift backend reports for itself. The workgroup's own `workgroupVersion` is
/// the Amazon Redshift Serverless release and not a PostgreSQL one, so it is not what
/// this parameter carries.
pub const WORKGROUP_SERVER_VERSION: &str = "8.0.2";

/// One workgroup's open data plane, as this store holds it: where it answers, what
/// answers there, and the SQL engine behind it.
///
/// Dropping it closes the port and severs every session on it.
pub type WorkgroupDataPlane = sql_wire_planes::OpenedSqlPlane;

/// Where a workgroup's data plane is SERVED: the host the core bound its listener on —
/// the address the workgroup's `Endpoint` publishes — and the port the core assigned it.
pub type ServedEndpoint = sql_wire_planes::ServedEndpoint;

/// The stored workgroup one listener belongs to.
///
/// Holds the store and the workgroup's NAME rather than the record, so every login and
/// every statement is admitted against the workgroup as it stands at that moment —
/// with every clock-owed transition already applied, exactly as a control-plane read
/// sees it.
pub struct WorkgroupFacts {
    state: ::engine::ServiceState<ComAmazonawsRedshiftserverlessRedshiftserverlessState>,
    workgroup_name: String,
}

impl WorkgroupFacts {
    /// Run `body` against the workgroup this listener belongs to and the namespace it
    /// is the compute of, with every clock-owed transition already applied — the same
    /// view a control-plane read of that workgroup gets.
    ///
    /// `None` when the store holds no such workgroup: a deleted workgroup serves no
    /// session, and the client is told the server is going away rather than served
    /// from a record nobody holds.
    fn with_workgroup<R>(
        &self,
        body: impl FnOnce(&WorkgroupRecord, Option<&NamespaceRecord>) -> R,
    ) -> Option<R> {
        let mut store = self.state.lock();
        store.settle_due_transitions();
        let record = store.workgroups.get(&self.workgroup_name)?;
        let namespace = record
            .workgroup
            .namespace_name
            .as_deref()
            .and_then(|name| store.namespaces.get(name));
        Some(body(record, namespace))
    }
}

impl ServedDatabase for WorkgroupFacts {
    fn facts(&self) -> Option<DatabaseFacts> {
        self.with_workgroup(|record, namespace| DatabaseFacts {
            name: record
                .workgroup
                .workgroup_name
                .clone()
                .unwrap_or_else(|| self.workgroup_name.clone()),
            serving: serving_state(record.workgroup.status.as_deref().unwrap_or_default()),
            // The namespace's administrator, its CURRENT password and the first
            // database created in the namespace, all read as the store holds them
            // now: the workgroup is that namespace's compute, so what logs on to the
            // workgroup is what logs on to the namespace's database. A namespace this
            // service holds no admin password for admits no login here.
            master_username: namespace
                .and_then(|namespace| namespace.namespace.admin_username.clone())
                .unwrap_or_default(),
            master_user_password: namespace
                .and_then(|namespace| namespace.admin_user_password.clone())
                .unwrap_or_default(),
            master_database_name: namespace
                .and_then(|namespace| namespace.namespace.db_name.clone())
                .unwrap_or_default(),
            server_version: WORKGROUP_SERVER_VERSION.to_string(),
        })
    }
}

/// Whether a workgroup in `status` is serving, as the wire reads it.
///
/// Only a workgroup the control plane reports as available admits a session, which is
/// what a client that creates one waits for. Of the statuses that do not, the wire
/// tells a client which KIND it is: a workgroup the service is still bringing up or
/// changing ends that work serving, so a client that waits is right to; one being
/// deleted does not.
pub fn serving_state(status: &str) -> ServingState {
    match status {
        WORKGROUP_AVAILABLE => ServingState::Serving,
        WORKGROUP_CREATING | WORKGROUP_MODIFYING => ServingState::ComingUp,
        _ => ServingState::GoingDown,
    }
}

/// Open the data plane of the workgroup `workgroup_name`: its own SQL engine, served
/// on the PostgreSQL wire a Redshift warehouse speaks, on a listener the core opens.
///
/// The core binds the socket and assigns the port; this hands it the protocol handler
/// and reports back where it landed. The caller stores the plane on the workgroup's
/// record — which is what keeps the port open — and publishes the workgroup's
/// `Endpoint` from it, so no workgroup of this service announces an address nothing
/// answers at.
///
/// `published_port` is the port the workgroup REPORTS it listens on — its own
/// configured `port` — which the shared plane records as a second address this one
/// answers at, on the same host: a client of this emulator that takes the pair the
/// control plane published reaches this workgroup's wire at exactly that pair. Two
/// workgroups configured on one port share that one address, as two servers on one
/// host and port would, and the one published there last is the one it reaches; each
/// also answers at the address the core assigned it.
///
/// `Err` when this emulator's engine has no store or no listener surface, when the
/// core could not bind, or when the SQL engine could not be started: the caller
/// answers that with a modelled failure rather than creating a workgroup whose
/// published address answers nothing.
///
/// A `published_port` outside the ports a workgroup may listen on is not reachable from
/// this API: every operation that takes one refuses a value outside the documented ranges
/// ([`crate::state::port_in_documented_range`]) before a plane is opened, so only a port
/// inside them — all of which a port number holds — ever reaches this.
pub async fn open_workgroup_data_plane<Eng: Send + Sync + 'static>(
    engine: &Eng,
    workgroup_name: &str,
    published_port: i32,
) -> Result<WorkgroupDataPlane, String> {
    let state = crate::state::service_state(engine)
        .ok_or_else(|| "this emulator's engine does not vend this service's store".to_string())?;
    let served = Arc::new(WorkgroupFacts {
        state,
        workgroup_name: workgroup_name.to_string(),
    });
    // The SigV4 signing name of this service, which is what names the service
    // answering at an address it opened for an IN-PROCESS client of this emulator.
    sql_wire_planes::open_sql_wire_plane_published_on(
        engine,
        EngineWire::Redshift,
        served,
        ARN_NAMESPACE,
        u16::try_from(published_port).ok(),
    )
    .await
}
