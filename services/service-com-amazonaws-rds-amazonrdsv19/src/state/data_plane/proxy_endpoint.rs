// SPDX-License-Identifier: CC0-1.0

//! The DB PROXY data plane: the address `DBProxy$Endpoint` publishes, and the sessions a
//! client opens on it.
//!
//! "The endpoint that you can use to connect to the DB proxy" is AN ADDRESS A CLIENT
//! CONNECTS TO with its ordinary driver and then speaks the database's own protocol on —
//! "you can connect to the proxy endpoint the same way you connect to the DB instance
//! endpoint" — so for every `EngineFamily` whose wire this emulator speaks
//! ([`proxy_engine_wire`]) `CreateDBProxy` opens a listener through the CORE and the control
//! plane publishes THAT listener's address, exactly as [`super::open_engine_endpoint`] does
//! for a DB instance and a DB cluster. An address this emulator does not answer on would be
//! no endpoint at all: a client connects to exactly the string the control plane hands it.
//! A proxy of an engine family this emulator serves no wire for keeps the DNS-form endpoint
//! this service has always reported and gets no listener, for the same reason an instance of
//! such an engine does.
//!
//! A proxy runs NO ENGINE and holds no data: "RDS Proxy ... pools and shares database
//! connections", and the database it pools them to is the one its target group holds
//! (`RegisterDBProxyTargets`). So a session on a proxy's endpoint is served against the
//! TARGET's own record and the TARGET's own storage, resolved when the session is opened —
//! a proxy is created before any target is registered, and a registration made later is
//! what gives the endpoint a database to serve. Everything a session needs beyond that is
//! shared with the other endpoints of this module tree: the two wires, the SQL engine and
//! the admission decision ([`super::wire_session`]).
//!
//! What the proxy itself decides about a session is what the proxy's own record states:
//!
//! * `RequireTLS` — "specifies whether Transport Layer Security (TLS) encryption is
//!   required for connections to the proxy. By enabling this setting, you can enforce
//!   encrypted TLS connections to the proxy" — so a session in the clear on a proxy that
//!   requires it is refused before any credential is read, which is the refusal both wires
//!   already word for an engine that requires an encrypted session
//!   ([`super::wire_session::LoginRefusal::PlaintextRefused`]);
//! * the ADDRESS the session arrived on, which is the proxy's own endpoint rather than the
//!   target database's: that is the pair a client generates an authentication token for when
//!   it authenticates to the proxy with IAM, and the one a refusal names.
//!
//! Everything else a login is checked against is the database behind the proxy: a proxy
//! "uses the credentials in the secret" its `Auth` names to log in to that database, so the
//! credentials that admit a session through the proxy are the database's own.
//!
//! The endpoint is the RESOURCE, as it is for a DB instance: dropping the stored
//! [`DbProxyEndpoint`] closes the port and severs every session on it, which is what
//! `DeleteDBProxy` does once the clock takes the proxy away.

use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::watch;

use super::wire_session::{self, ServedSession, SessionTarget};
use super::{pg_sessions, EndpointRole, EngineWire, ServedEndpoint};
use crate::state::{
    ComAmazonawsRdsAmazonrdsv19State, DB_PROXY_STATUS_AVAILABLE, ENGINE_FAMILY_MYSQL,
    ENGINE_FAMILY_POSTGRESQL,
};

/// The wire a proxy of `engine_family` speaks, or `None` for a family this emulator serves
/// no data plane for.
///
/// `EngineFamily` is exactly the question this answers: "this value determines which
/// database network protocol the proxy recognizes when it interprets network traffic to and
/// from the database". The two families this emulator speaks the protocol of are the two
/// wires it serves everywhere else ([`super::engine_wire`]); `SQLSERVER`
/// ([`crate::state::ENGINE_FAMILY_SQLSERVER`]) is deliberately absent, because this emulator
/// runs no engine for it and a port answering nothing would be worse than a name.
#[must_use]
pub fn proxy_engine_wire(engine_family: &str) -> Option<EngineWire> {
    match engine_family.to_ascii_uppercase().as_str() {
        ENGINE_FAMILY_MYSQL => Some(EngineWire::MySql),
        ENGINE_FAMILY_POSTGRESQL => Some(EngineWire::Postgres),
        _ => None,
    }
}

/// One DB proxy's endpoint: the live listener and the handle that severs its sessions.
///
/// It holds no database, because a proxy holds no data — the storage a session on it runs on
/// is the target's, read when the session is opened ([`proxy_session`]).
pub struct DbProxyEndpoint {
    /// Where the control plane publishes it, which is what `DBProxy$Endpoint` reports.
    pub endpoint: ServedEndpoint,
    /// Which protocol the listener speaks.
    pub wire: EngineWire,
    /// The proxy's run: held because dropping it is what severs every session open on this
    /// endpoint, which is what taking the proxy away does — a session serves only while the
    /// value it started under is current.
    generation: watch::Sender<u64>,
    /// Held only to keep the port open; the core owns the socket and its TLS.
    _listener: ::emulator_server::ResourceListener,
    /// The record that says this address is one this emulator has opened a plane at, so an
    /// in-process client of this emulator that was handed it — the code of a Lambda function
    /// attached to the proxy's VPC — can open a connection to it, served by the same handler
    /// as the listener itself. Dropping it withdraws the address, exactly as dropping the
    /// listener closes the port.
    _dialable: Option<::service_common::dialable_data_planes::OpenedPlanes>,
}

/// Written by hand rather than derived: the endpoint holds a core `ResourceListener`, which
/// is not `Debug`. What a reader needs is the address it published and the protocol it
/// speaks.
impl std::fmt::Debug for DbProxyEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbProxyEndpoint")
            .field("endpoint", &self.endpoint)
            .field("wire", &self.wire)
            .field("generation", &*self.generation.borrow())
            .finish()
    }
}

/// WHAT to open: which proxy's endpoint it is, on which wire, and the callback every session
/// on it is served with.
pub struct ProxyEndpointPlan {
    /// The region the proxy was created in; half of its key in the store.
    pub region: String,
    /// The server-assigned label the proxy's endpoint carries
    /// (`DbProxyRecord::endpoint_host`), which is what a session resolves the proxy by: it
    /// is minted once and survives the rename `ModifyDBProxy` performs.
    pub endpoint_label: String,
    /// The wire the proxy's `EngineFamily` speaks ([`proxy_engine_wire`]).
    pub wire: EngineWire,
    /// The in-process callback of the operation opening this endpoint
    /// (`OperationContext::callback`), which every session on it is served with: a session
    /// has no request context of its own.
    pub callback: ::smithy_server_runtime::EmbeddedEmulator,
}

/// Open the proxy endpoint `plan` describes.
///
/// The core binds the socket and assigns the port; this only hands it the protocol handler
/// and reports back where it landed. The caller stores the returned endpoint in the state —
/// which is what keeps the port open — and publishes [`DbProxyEndpoint::endpoint`] as the
/// proxy's `Endpoint`.
///
/// `Err` when this service was mounted on an engine with no listener surface, or when the
/// core could not bind: the caller answers that with a modelled failure rather than
/// publishing an address nothing serves.
pub async fn open_proxy_endpoint<Eng: Send + Sync + 'static>(
    engine: &Eng,
    state: &::engine::ServiceState<ComAmazonawsRdsAmazonrdsv19State>,
    plan: ProxyEndpointPlan,
) -> Result<DbProxyEndpoint, String> {
    let ProxyEndpointPlan {
        region,
        endpoint_label,
        wire,
        callback,
    } = plan;
    let listeners = super::shared_engine(engine)
        .ok_or_else(|| "this emulator's engine has no listener surface".to_string())?
        .listeners();
    let (generation, generation_rx) = watch::channel(0u64);
    let context = wire_session::SessionContext {
        state: state.clone(),
        target: SessionTarget::DbProxy {
            region,
            endpoint_label,
        },
        // A proxy's one target group holds the database the proxy reaches for read/write
        // work (`DBProxyTarget$Role`), so a session on its endpoint is served in the
        // writing role of that database.
        role: EndpointRole::Writer,
        // No storage of its own: the target's is resolved per session ([`proxy_session`]).
        database: None,
        sessions: pg_sessions::SessionRegistry::default(),
        upgrader: listeners.tls_upgrader().map_err(|error| {
            format!("the proxy endpoint's TLS could not be prepared: {error}")
        })?,
        objects: super::mysql_s3_load::object_store(engine),
        iam: super::engine_s3_role::iam_store(engine),
        generation: generation_rx,
        next_connection_id: Arc::new(AtomicU32::new(1)),
        callback,
        // A session through a proxy runs its statements on the database of the target the
        // proxy resolved, so its activity is that TARGET's: the store is the same one, and
        // which data source a sample is filed under comes from the session's own facts
        // ([`super::activity_samples`]).
        activity: super::activity_samples::ActivityStore::of(engine),
    };
    let (listener, plane) = super::open_wire_listener(&listeners, wire, context).await?;
    let dialable = super::dialable_plane(engine, &listener, &plane);
    Ok(DbProxyEndpoint {
        endpoint: ServedEndpoint {
            host: listener.host(),
            port: i32::from(listener.port()),
        },
        wire,
        generation,
        _listener: listener,
        _dialable: dialable,
    })
}

impl ComAmazonawsRdsAmazonrdsv19State {
    /// The proxy in `region` whose endpoint carries the server-assigned label
    /// `endpoint_label`.
    ///
    /// A session on a proxy's endpoint resolves its proxy by this label rather than by
    /// `DBProxyName`, because `ModifyDBProxy` can rename a proxy under a live connection
    /// while the label its endpoint was minted with stays as it is. Reads one region's
    /// ordered range, which the region's own proxy quota bounds.
    #[must_use]
    pub fn db_proxy_by_endpoint_label(
        &self,
        region: &str,
        endpoint_label: &str,
    ) -> Option<&crate::state::DbProxyRecord> {
        self.db_proxies
            .range((region.to_string(), String::new())..)
            .take_while(|((stored, _), _)| stored == region)
            .map(|(_, record)| record)
            .find(|record| record.endpoint_host == endpoint_label)
    }

    /// The endpoint of the DB proxy `key` names, for the proxies that have one.
    #[must_use]
    pub fn proxy_endpoint(&self, key: &crate::state::RegionalKey) -> Option<&DbProxyEndpoint> {
        self.db_proxy_endpoints.get(key)
    }

    /// Move a proxy's endpoint with the proxy when `ModifyDBProxy` renames it, so the
    /// listener a client is connected to stays the endpoint the control plane publishes for
    /// that proxy.
    pub fn move_proxy_endpoint(
        &mut self,
        from: &crate::state::RegionalKey,
        to: &crate::state::RegionalKey,
    ) {
        if from == to {
            return;
        }
        if let Some(endpoint) = self.db_proxy_endpoints.remove(from) {
            self.db_proxy_endpoints.insert(to.clone(), endpoint);
        }
    }
}

/// What a session arriving on the proxy `region`/`endpoint_label`'s endpoint is served
/// against: the facts of the database its target group holds, with what the PROXY decides
/// written over them.
///
/// `None` — which both wires answer as an engine that is not there — when:
///
/// * the store holds no such proxy any more (it was deleted while the listener was still
///   accepting);
/// * the proxy is not ready to pool connections yet: "a status of `available` means the
///   proxy is ready to handle requests", and one still coming up, applying a change or on
///   its way out is not;
/// * its target group holds no registration that is in force yet, so the proxy has no
///   database to pool connections to — a proxy is created with none, and a registration
///   comes into force a moment after it is made (`DBProxyTargetRecord::is_in_force`);
/// * the database behind that registration is gone, runs no engine this emulator serves, or
///   speaks another wire than the one this proxy's `EngineFamily` announced, so the proxy
///   cannot interpret traffic to it.
pub(super) fn proxy_session(
    store: &ComAmazonawsRdsAmazonrdsv19State,
    region: &str,
    endpoint_label: &str,
) -> Option<ServedSession> {
    let proxy = store.db_proxy_by_endpoint_label(region, endpoint_label)?;
    if proxy.status != DB_PROXY_STATUS_AVAILABLE {
        return None;
    }
    let wire = proxy_engine_wire(&proxy.engine_family)?;
    let served = proxy.served_endpoint.clone()?;
    let require_tls = proxy.require_tls;
    let target = proxied_session_target(store, region, endpoint_label)?;
    let (mut facts, database) = match &target {
        SessionTarget::DbInstance {
            region,
            resource_id,
        } => {
            let record = store.instance_by_resource_id(region, resource_id)?;
            let database = store
                .engine_endpoint(&(record.region.clone(), record.identifier.clone()))?
                .database();
            let facts = wire_session::instance_facts(store, region, resource_id, &database)?;
            (facts, database)
        }
        SessionTarget::DbCluster {
            region,
            resource_id,
        } => {
            let record = store.cluster_by_resource_id(region, resource_id)?;
            let database = store
                .cluster_engine_endpoint(&(record.region.clone(), record.identifier.clone()))?
                .database();
            let facts = super::cluster_endpoint::cluster_facts(
                store,
                region,
                resource_id,
                &database,
                EndpointRole::Writer,
            )?;
            (facts, database)
        }
        // [`proxied_session_target`] resolves a database, never another proxy.
        SessionTarget::DbProxy { .. } => return None,
    };
    if facts.wire != wire {
        return None;
    }
    // "By enabling this setting, you can enforce encrypted TLS connections to the proxy":
    // the client's hop ends at the proxy, so a proxy that requires TLS requires it of this
    // session whatever the database behind it would have admitted — and a database that
    // requires an encrypted session keeps requiring one, since the proxy presents its
    // traffic to that database as it stands.
    facts.requires_secure_transport = facts.requires_secure_transport || require_tls;
    // The address this session arrived on is the PROXY's own endpoint: that is the pair a
    // client generates an IAM authentication token for, and the one a refusal names.
    facts.endpoint_host = served.host;
    facts.endpoint_port = u16::try_from(served.port).unwrap_or_default();
    Some(ServedSession { facts, database })
}

/// WHICH database a session on the proxy `region`/`endpoint_label`'s endpoint is served by,
/// as the target of a session on that database's own endpoint.
///
/// The proxy's target group holds it: "currently, each target group is associated with
/// exactly one RDS DB instance or Aurora DB cluster", so the database is the first
/// registration in force — a registration still coming up is not one the proxy pools
/// connections to yet. Each database is named by the IMMUTABLE id its create minted, as
/// every other session target is, so a rename under a live connection leaves the session
/// pointing at the same database.
///
/// The one resolution of a proxy's database, so the facts a session is admitted against
/// ([`proxy_session`]) and the engine its statements are served by
/// (`wire_session::engine_now_of`) cannot be two different databases.
pub(super) fn proxied_session_target(
    store: &ComAmazonawsRdsAmazonrdsv19State,
    region: &str,
    endpoint_label: &str,
) -> Option<SessionTarget> {
    let proxy = store.db_proxy_by_endpoint_label(region, endpoint_label)?;
    let now = Instant::now();
    let target = proxy
        .targets
        .iter()
        .find(|target| target.is_in_force(now))?;
    if target.is_cluster {
        let cluster = store
            .db_clusters
            .get(&(region.to_string(), target.identifier.clone()))?;
        return Some(SessionTarget::DbCluster {
            region: region.to_string(),
            resource_id: cluster.resource_id.clone(),
        });
    }
    let instance = store
        .db_instances
        .get(&(region.to_string(), target.identifier.clone()))?;
    Some(SessionTarget::DbInstance {
        region: region.to_string(),
        resource_id: instance.dbi_resource_id.clone(),
    })
}
