// SPDX-License-Identifier: CC0-1.0

//! The DB PROXY data plane: the address `DBProxy$Endpoint` publishes, and the sessions a
//! client opens on it.
//!
//! "The endpoint that you can use to connect to the DB proxy" is AN ADDRESS A CLIENT
//! CONNECTS TO with its ordinary driver and then speaks the database's own protocol on —
//! "you can connect to the proxy endpoint the same way you connect to the DB instance
//! endpoint" — so for every `EngineFamily` whose wire this emulator speaks
//! ([`proxy_engine_wire`]) `CreateDBProxy` opens a listener through the CORE, exactly as
//! [`super::open_engine_endpoint`] does for a DB instance and a DB cluster. An address this
//! emulator does not answer on would be no endpoint at all: a client connects to exactly the
//! string the control plane hands it. A proxy of an engine family this emulator serves no wire
//! for keeps the DNS-form endpoint this service has always reported and gets no listener, for
//! the same reason an instance of such an engine does.
//!
//! WHERE that endpoint is published, and the addresses it answers at, follow from the one
//! thing `DBProxy` does not model: a PORT. An instance publishes `Endpoint.Address` beside its
//! own `Port`, so a client composes the pair from two members; a proxy publishes one member and
//! the client completes it with the port of the databases its family names
//! ([`proxy_engine_port`]) — "you include the endpoint value in the connection string for a
//! database client application". So the endpoint answers that family's own port as well as the
//! port the core assigned its listener ([`open_proxy_endpoint`] records both addresses, and one
//! handler serves them both), and a driver handed the published host alone reaches it.
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
    ComAmazonawsRdsAmazonrdsv19State, DB_PROXY_ENDPOINT_STATUS_AVAILABLE,
    DB_PROXY_STATUS_AVAILABLE, ENGINE_FAMILY_MYSQL, ENGINE_FAMILY_POSTGRESQL,
};

/// What a DB PROXY answers a login that arrives in the clear while `RequireTLS` is on:
/// "specifies whether Transport Layer Security (TLS) encryption is required for connections
/// to the proxy. By enabling this setting, you can enforce encrypted TLS connections to the
/// proxy."
///
/// The sentence is the PROXY's own and has no variable part: it names neither the client, the
/// user nor the database, because no database was reached — the proxy refused the client's
/// hop before opening a connection to the one its target group holds. It is what both wires
/// word [`super::wire_session::LoginRefusal::ProxyTlsRequired`] with, so a client reads the
/// same sentence whichever engine family the proxy announced.
pub const PROXY_REQUIRES_TLS_MESSAGE: &str = "This RDS Proxy requires TLS connections";

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

/// The port a client DIALS a proxy of `engine_family` on, or `None` for a family this
/// emulator serves no data plane for.
///
/// `DBProxy` models no port member at all: `Endpoint` is "the endpoint that you can use to
/// connect to the DB proxy. You include the endpoint value in the connection string for a
/// database client application", so what completes that connection string is the port of the
/// databases the family names — "this value determines which database network protocol the
/// proxy recognizes", and a proxy speaks that protocol on its own default port.
#[must_use]
pub fn proxy_engine_port(engine_family: &str) -> Option<u16> {
    Some(wire_engine_port(proxy_engine_wire(engine_family)?))
}

/// The same port, read off the WIRE the family resolved to, which is what the endpoint itself
/// holds once it is open ([`DbProxyEndpoint::wire`]).
///
/// The two values are the documented defaults of the engines each family names, which is the
/// one reading this crate holds of them ([`crate::state::default_port`]): the test below pins
/// each against it, so the port a proxy is dialled on and the port the databases behind it
/// publish for themselves cannot drift.
#[must_use]
pub fn wire_engine_port(wire: EngineWire) -> u16 {
    match wire {
        EngineWire::MySql => MYSQL_FAMILY_PORT,
        EngineWire::Postgres => POSTGRESQL_FAMILY_PORT,
    }
}

/// `CreateDBInstanceMessage$Port` publishes the default port of the engines
/// `ENGINE_FAMILY_MYSQL` names: "Default: … RDS for MariaDB — 3306, … RDS for MySQL — 3306".
const MYSQL_FAMILY_PORT: u16 = 3306;

/// The same member for the other family this emulator speaks the wire of: "Default: … RDS for
/// PostgreSQL — 5432".
const POSTGRESQL_FAMILY_PORT: u16 = 5432;

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
    /// Whether this endpoint also answers at the PORT the proxy's engine family is dialled
    /// on, for the client that completes the published host with it ([`wire_engine_port`]).
    ///
    /// That port is one port of the one interface the core binds on, so only ONE endpoint can
    /// answer there: a proxy's own endpoint does, which is the address `DBProxy$Endpoint`
    /// publishes with no port of its own, while an endpoint a caller ADDED answers at the
    /// authority it publishes — which carries its port — so adding one does not take the
    /// family port away from the proxy that published it.
    pub answers_engine_family_port: bool,
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
        answers_engine_family_port,
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
    // WHERE the endpoint is published, and the addresses it answers at.
    //
    // The core owns the socket, the interface and the port it assigned the listener, so what
    // the control plane publishes is THAT listener's `host:port` — `DBProxy` models no port
    // member, so the published value carries one. A client puts that value in its connection
    // string, and the two ways it reads it are the two addresses recorded here: the whole
    // authority as it stands, and the HOST alone completed with the port of the engine family
    // the proxy announced ([`wire_engine_port`]) — which is what a driver dials when the member
    // it was handed carries no port of its own, and the address this endpoint would answer at
    // on AWS. Both are served by the SAME handler over the same session context as the listener
    // itself, so a session is served identically whichever of them it arrived on.
    //
    // That family port is ONE port of the ONE interface the core binds on, so it names no proxy
    // in particular: the proxy that published its endpoint last answers there, and once THAT
    // proxy is gone nothing does, while each proxy's own published authority — which carries the
    // port the core assigned it — keeps reaching that proxy throughout. Giving each proxy an
    // address of its own to answer that port at needs a core listener that takes an address or a
    // port (`vendor/server/src/listeners.rs`, whose `tcp_plaintext` takes neither), which is
    // recorded in
    // `issues/things-that-now-work/20261109T135215Z-lambda-rds-secretsmanager-db-proxy-endpoints-are-published.md`.
    //
    // The HOST is the one the core says the listener is dialable at
    // (`data_plane_addressing::listener_authority`, which publishes a WILDCARD bind as
    // loopback), so the address recorded here is the host the control plane publishes and not
    // the interface the socket happens to be bound to.
    let family = answers_engine_family_port.then(|| {
        ::service_common::data_plane_addressing::host_port_authority(
            ::service_common::data_plane_addressing::host_only(
                &::service_common::data_plane_addressing::listener_authority(&listener),
            ),
            wire_engine_port(wire),
        )
    });
    let dialable = super::dialable_plane_also_at(engine, &listener, family, &plane);
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

    /// The endpoint a caller ADDED whose own listener carries the label `endpoint_label`, in
    /// `region`.
    ///
    /// Each such endpoint is minted a label of its own
    /// ([`crate::state::DbProxyCustomEndpointRecord::endpoint_label`]), so the address a
    /// session arrived on says which endpoint it arrived on and therefore in which ROLE it is
    /// served. Reads one region's ordered range, which the region's own endpoint allowance
    /// bounds.
    #[must_use]
    pub fn proxy_custom_endpoint_by_label(
        &self,
        region: &str,
        endpoint_label: &str,
    ) -> Option<&crate::state::DbProxyCustomEndpointRecord> {
        self.db_proxy_custom_endpoints
            .range((region.to_string(), String::new())..)
            .take_while(|((stored, _), _)| stored == region)
            .map(|(_, record)| record)
            .find(|record| record.endpoint_label == endpoint_label)
    }

    /// WHICH proxy a session that arrived on the address labelled `endpoint_label` is served
    /// by, and the ADDED endpoint it arrived on where that label names one rather than a
    /// proxy's own default endpoint.
    ///
    /// The one resolution of an arriving session's endpoint, so what admits the session
    /// ([`proxy_session`]) and what serves its statements cannot disagree about which proxy —
    /// or which of its endpoints — the client reached.
    #[must_use]
    pub fn proxy_at_endpoint_label(
        &self,
        region: &str,
        endpoint_label: &str,
    ) -> Option<(
        &crate::state::DbProxyRecord,
        Option<&crate::state::DbProxyCustomEndpointRecord>,
    )> {
        if let Some(proxy) = self.db_proxy_by_endpoint_label(region, endpoint_label) {
            return Some((proxy, None));
        }
        let endpoint = self.proxy_custom_endpoint_by_label(region, endpoint_label)?;
        let proxy = self
            .db_proxies
            .get(&(region.to_string(), endpoint.db_proxy_name.clone()))?;
        Some((proxy, Some(endpoint)))
    }

    /// The endpoint of the DB proxy `key` names, for the proxies that have one.
    #[must_use]
    pub fn proxy_endpoint(&self, key: &crate::state::RegionalKey) -> Option<&DbProxyEndpoint> {
        self.db_proxy_endpoints.get(key)
    }

    /// Move a proxy's endpoints with the proxy when `ModifyDBProxy` renames it, so the
    /// listener a client is connected to stays the endpoint the control plane publishes for
    /// that proxy.
    ///
    /// The proxy's own default endpoint is keyed by the PROXY, so it moves with it; the
    /// endpoints a caller added are keyed by their own names and keep them, so each is
    /// retargeted at the name the proxy now has — "the identifier for the DB proxy that is
    /// associated with this DB proxy endpoint" (`DBProxyEndpoint$DBProxyName`) is the proxy as
    /// it stands, so none of them is left naming a proxy this store no longer holds.
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
        for ((region, _), record) in self.db_proxy_custom_endpoints.iter_mut() {
            if *region == from.0 && record.db_proxy_name == from.1 {
                record.db_proxy_name = to.1.clone();
            }
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
/// * the session arrived on an endpoint a caller ADDED and that endpoint is not ready
///   either: "a status of `available` means the endpoint is ready to handle requests"
///   (`DBProxyEndpoint$Status`), so one still being created or on its way out answers no
///   session;
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
    let (proxy, added) = store.proxy_at_endpoint_label(region, endpoint_label)?;
    if proxy.status != DB_PROXY_STATUS_AVAILABLE {
        return None;
    }
    // An endpoint a caller added is a resource of its own with a state of its own, so the
    // session is admitted on ITS readiness as well as the proxy's.
    if added.is_some_and(|endpoint| endpoint.status != DB_PROXY_ENDPOINT_STATUS_AVAILABLE) {
        return None;
    }
    let wire = proxy_engine_wire(&proxy.engine_family)?;
    // The address the session arrived on is the one the ENDPOINT it arrived on publishes:
    // that is the pair a client generates an IAM authentication token for, and the one a
    // refusal names.
    let served = match added {
        Some(endpoint) => endpoint.served_endpoint.clone()?,
        None => proxy.served_endpoint.clone()?,
    };
    let require_tls = proxy.require_tls;
    // "A value that indicates whether the DB proxy endpoint can be used for read/write or
    // read-only operations" (`DBProxyEndpoint$TargetRole`): a session that arrived on a
    // read-only endpoint writes nothing, whatever the database behind the proxy would have
    // admitted, and the default endpoint "always ha[s] read/write capability".
    let reads_only = added.is_some_and(crate::state::DbProxyCustomEndpointRecord::serves_reads_only);
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
    // session whatever the database behind it would have admitted — and that refusal is the
    // PROXY's own, worded as the proxy words it ([`PROXY_REQUIRES_TLS_MESSAGE`]), not the
    // engine's. A database that requires an encrypted session keeps requiring one, since the
    // proxy presents its traffic to that database as it stands.
    facts.tls_required_by_proxy = require_tls;
    // A read-only endpoint serves no writing statement, exactly as a reader endpoint of a DB
    // cluster does ([`super::read_only`]); an endpoint that serves read/write work leaves the
    // database's own answer as it stands, so a session on the writer of a cluster still
    // writes.
    facts.read_only = facts.read_only || reads_only;
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
    let (proxy, _) = store.proxy_at_endpoint_label(region, endpoint_label)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the port a proxy of a family is dialled on is the DOCUMENTED DEFAULT PORT of the
    /// engines that family names, so a client that completes the published host with it reaches
    /// the proxy on the same port the databases behind it publish for themselves.
    ///
    /// The rows are the families `EngineFamily` names, each with the engines it covers
    /// (`CreateDBProxy$EngineFamily`: "For Aurora MySQL, RDS for MariaDB, and RDS for MySQL
    /// databases, specify MYSQL. For Aurora PostgreSQL and RDS for PostgreSQL databases,
    /// specify POSTGRESQL. For RDS for Microsoft SQL Server, specify SQLSERVER"), and the one
    /// this emulator speaks no wire for.
    #[test]
    fn a_familys_port_is_the_documented_default_of_the_engines_it_names() {
        let rows: &[(&str, &[&str])] = &[
            (
                crate::state::ENGINE_FAMILY_MYSQL,
                &["mysql", "mariadb", "aurora-mysql"],
            ),
            (
                crate::state::ENGINE_FAMILY_POSTGRESQL,
                &["postgres", "aurora-postgresql"],
            ),
        ];
        for (family, engines) in rows {
            let port = proxy_engine_port(family);
            for engine in *engines {
                let documented = u16::try_from(crate::state::default_port(engine)).ok();
                assert_eq!(
                    port, documented,
                    "case {family:?}: expected the default port of {engine:?} ({documented:?}), \
                     got {port:?}"
                );
            }
            // The family is read however a request spells it, as the wire is
            // ([`proxy_engine_wire`]).
            let spelled = proxy_engine_port(&family.to_ascii_lowercase());
            assert_eq!(
                spelled, port,
                "case {family:?}: the family is read without regard to case, got {spelled:?}"
            );
        }
        let unserved = proxy_engine_port(crate::state::ENGINE_FAMILY_SQLSERVER);
        assert_eq!(
            unserved, None,
            "case {:?}: this emulator speaks no wire for that family, so it is dialled on no \
             port of this emulator's, got {unserved:?}",
            crate::state::ENGINE_FAMILY_SQLSERVER
        );
    }
}
