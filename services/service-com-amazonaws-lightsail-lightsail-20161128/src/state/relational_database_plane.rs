// SPDX-License-Identifier: CC0-1.0

//! The DATA plane of one managed relational database: the endpoint a client
//! connects to, and what admits a session on it.
//!
//! A Lightsail managed database publishes a `masterEndpoint`, and the contract
//! that member carries is "the DNS address of the database" and "the port that
//! the database is listening on" — an address a client resolves and connects
//! to, where the blueprint's engine answers. So this emulator serves it, on the
//! ONE shared statement of a SQL-wire data plane
//! ([`::service_common::sql_wire_planes`]): the core opens the listener, the
//! address and port this database publishes are that listener's own, and the
//! connections accepted on it are served by the server half of the blueprint
//! engine's protocol over this database's own in-process SQL engine.
//!
//! What this module adds is the Lightsail half of it: which wire a blueprint's
//! engine speaks, and the STORED database a login is admitted against
//! ([`StoredDatabaseFacts`]) — the master user and its current password, the
//! master database name, the engine version the blueprint names, and the
//! lifecycle state, each read out of the store when a login or a statement
//! arrives rather than captured when the listener opened. So a database that
//! is still being created, or has been stopped, refuses a session exactly as
//! its engine would, and a password an `UpdateRelationalDatabase` has rotated
//! is the one the next login is verified against.
//!
//! The listener is the RESOURCE: the plane is held on the store beside the
//! database record, so the port stays open for as long as the database does and
//! closes with it (`DeleteRelationalDatabase` drops this record and with it the
//! listener and the engine).

use std::sync::Arc;

use ::service_common::sql_wire_planes::{
    self, DatabaseFacts, EngineWire, ServedDatabase, ServingState,
};

use super::*;

/// The SigV4 signing name of this service, which is what names the service
/// answering at an address it opened for an in-process client.
const SIGNING_NAME: &str = "lightsail";

/// One database's open data plane, as this store holds it: where it answers,
/// what answers there, and the engine behind it.
///
/// Dropping it closes the port and severs every session on it.
pub type RelationalDatabasePlane = sql_wire_planes::OpenedSqlPlane;

/// Where a database's data plane answers — what `masterEndpoint` publishes.
pub type ServedEndpoint = sql_wire_planes::ServedEndpoint;

/// The stored managed relational database one listener belongs to.
///
/// Holds the store and the database's NAME rather than the record, so every
/// login and every statement is admitted against the database as it stands at
/// that moment — with every clock-owed transition already applied, exactly as a
/// control-plane read sees it.
struct StoredDatabaseFacts {
    state: ::engine::ServiceState<ComAmazonawsLightsailLightsail20161128State>,
    name: String,
}

impl ServedDatabase for StoredDatabaseFacts {
    fn facts(&self) -> Option<DatabaseFacts> {
        with_held_state(&self.state, |store| {
            let database = store.relational_database(&self.name)?;
            Some(DatabaseFacts {
                name: database.name.clone(),
                serving: serving_state(database.state),
                master_username: database.master_username.clone(),
                master_user_password: database.master_user_password.password.clone(),
                master_database_name: database.master_database_name.clone(),
                server_version: database.blueprint.engine_version.to_string(),
            })
        })
    }
}

/// Whether a database in `state` is serving, as the wire reads it.
///
/// Only an `available` database accepts a session, which is what AWS reports a
/// client has to wait for. Of the states that do not, the wire tells a client
/// which KIND it is: a database being created or started has not finished
/// coming up, and one being stopped, stopped or restarting is going down.
fn serving_state(state: &str) -> ServingState {
    match state {
        RELATIONAL_DATABASE_STATE_AVAILABLE => ServingState::Serving,
        RELATIONAL_DATABASE_STATE_CREATING | RELATIONAL_DATABASE_STATE_STARTING => {
            ServingState::ComingUp
        }
        _ => ServingState::GoingDown,
    }
}

/// The wire the engine `engine` speaks, classified the way every other
/// per-engine helper of this service classifies it
/// ([`is_postgres_engine`]).
pub fn relational_database_wire(engine: &str) -> EngineWire {
    if is_postgres_engine(engine) {
        EngineWire::Postgres
    } else {
        EngineWire::MySql
    }
}

/// Open the data plane of the database `name`, whose blueprint engine is
/// `engine_name`.
///
/// The core binds the socket and assigns the port; this hands it the protocol
/// handler and reports back where it landed. The caller stores the plane — which
/// is what keeps the port open — and publishes
/// [`RelationalDatabasePlane::endpoint`] as the database's `masterEndpoint`.
///
/// `Err` when this emulator's engine has no store or no listener surface, when
/// the core could not bind, or when the SQL engine could not be started: the
/// caller answers that with a modelled failure rather than creating a database
/// whose published address answers nothing.
pub async fn open_relational_database_plane<Eng: Send + Sync + 'static>(
    engine: &Eng,
    name: &str,
    engine_name: &str,
) -> Result<RelationalDatabasePlane, String> {
    let state = service_state(engine)
        .ok_or_else(|| "this emulator's engine does not vend this service's store".to_string())?;
    let served = Arc::new(StoredDatabaseFacts {
        state,
        name: name.to_string(),
    });
    sql_wire_planes::open_sql_wire_plane(
        engine,
        relational_database_wire(engine_name),
        served,
        SIGNING_NAME,
    )
    .await
}
