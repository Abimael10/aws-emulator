// SPDX-License-Identifier: CC0-1.0

//! The ONE home for a service whose data plane is a SQL-wire database endpoint:
//! the listener a client dials, the server half of the engine's protocol, and
//! the in-process SQL engine the statements run against.
//!
//! A control plane that creates a database resource publishes an endpoint — an
//! `Endpoint{Address,Port}`, a `masterEndpoint`, a workgroup's address — and the
//! contract that member carries is the DNS address of the database and the port
//! it is listening on. So this module serves it: the CORE opens the listener
//! ([`::emulator_server::Listeners::tcp_plaintext`], which owns the socket, the
//! bind interface, the port range, the handshake deadline, the idle timeout and
//! the connection caps), the address and port the resource publishes are that
//! listener's own, and the connections accepted on it are served by the server
//! half of the engine's wire ([`mysql_wire`], [`pg_wire`]) over the database's
//! own engine ([`sql_engine`]).
//!
//! Both wires negotiate encryption IN BAND — a MySQL server sends its greeting
//! in the clear and upgrades on the client's `CLIENT_SSL` answer, and
//! PostgreSQL upgrades on its `SSLRequest` — so the listener is the plaintext
//! one, which hands the handler byte 0, and the handshake is run by the core's
//! own TLS upgrader. No service code binds a socket, chooses a port or holds a
//! certificate.
//!
//! What a session is admitted against is the OWNING SERVICE's stored resource as
//! it stands when the login arrives: the service implements
//! [`ServedDatabase::facts`] over its own store, so the master user and its
//! current password, the database name, the engine version and whether the
//! engine is running are read per login rather than captured when the listener
//! opened — and a password a later update rotated is the one the next login is
//! verified against.
//!
//! Services still carrying their own copy of this reading: this module
//! postdates the Amazon RDS engine endpoint
//! (`services/service-com-amazonaws-rds-amazonrdsv19/src/state/data_plane/`,
//! whose wires, confined engine and admission decision this module is the
//! service-agnostic statement of). That copy is to be MIGRATED onto this
//! module; it is left in place only because a change does not own that
//! service's issues, and because it carries a great deal more than this
//! module's surface (IAM database authentication, cluster and reader
//! endpoints, the S3 import statements, per-session transactions and row
//! locks). Nothing new writes a third copy — call this module.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use ::emulator_server::{ResourceListener, TlsUpgrader};
use sha1::{Digest, Sha1};

use crate::dialable_data_planes::{self, OpenedPlanes, SharedHandler};
use sql_engine::DatabaseEngine;

/// The server half of the MySQL client/server protocol.
pub mod mysql_wire;
/// The server half of the PostgreSQL frontend/backend protocol.
pub mod pg_wire;
/// What one session on either wire shares: its settings, its transaction and
/// the server's own commands.
pub mod session;
/// The in-process SQL engine one database's statements run against.
pub mod sql_engine;

/// The host a MySQL refusal names as the client's own.
///
/// MySQL's `ER_ACCESS_DENIED_ERROR` names the account AND the host the login
/// came from (`'user'@'host'`). The core accepts these connections on its own
/// loopback interface and does not hand a handler the peer address, and
/// `localhost` is what a client on that interface is.
const CLIENT_HOST: &str = "localhost";

/// The host a MySQL `ER_ACCESS_DENIED_ERROR` names as the client's.
#[must_use]
pub fn client_host() -> &'static str {
    CLIENT_HOST
}

/// Which wire protocol a database's engine speaks.
///
/// The OWNING SERVICE decides which one its resource's engine is — the engine
/// name a blueprint, an engine version or a cluster type gives it is that
/// service's own vocabulary — and this module serves the wire it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineWire {
    MySql,
    Postgres,
}

/// The address and port a database's data plane answers at — what the control
/// plane publishes as the resource's endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServedEndpoint {
    /// The host the listener is bound on, which is the address a client dials.
    pub address: String,
    /// The port the core assigned that listener.
    pub port: i32,
}

/// Whether the resource's engine is running, as its OWNING SERVICE's lifecycle
/// states it.
///
/// Which of the two non-serving answers a login gets is the protocol's: a
/// database that has not finished coming up and one that is going down are
/// different conditions, and each wire has its own error for each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServingState {
    /// The engine is running and admits sessions.
    Serving,
    /// It has not finished starting: the resource is being created, restored or
    /// started.
    ComingUp,
    /// It is stopping, stopped or restarting.
    GoingDown,
}

/// What the stored resource says about a login and the session it opens.
#[derive(Clone)]
pub struct DatabaseFacts {
    /// The resource's own name, as its control plane reports it.
    pub name: String,
    /// Whether its engine is running.
    pub serving: ServingState,
    /// The master user, the one account its engine authenticates.
    pub master_username: String,
    /// That user's CURRENT password.
    pub master_user_password: String,
    /// The database created inside the engine, which is the one a session uses.
    pub master_database_name: String,
    /// The engine version the resource publishes, which the greeting and
    /// `version()` report.
    pub server_version: String,
}

/// Written by hand rather than derived: the master password is a secret and no
/// formatter writes it.
impl std::fmt::Debug for DatabaseFacts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DatabaseFacts")
            .field("name", &self.name)
            .field("serving", &self.serving)
            .field("master_username", &self.master_username)
            .field("master_database_name", &self.master_database_name)
            .field("server_version", &self.server_version)
            .finish_non_exhaustive()
    }
}

impl DatabaseFacts {
    /// Whether the engine admits a session at all.
    #[must_use]
    pub fn is_serving(&self) -> bool {
        self.serving == ServingState::Serving
    }
}

/// The stored resource one listener belongs to, as its OWNING SERVICE reads it.
///
/// Implemented by the service that opened the plane, over its own store: every
/// value a session needs that can CHANGE — the credentials, the database name,
/// the engine version and the lifecycle state — is read through here when it is
/// needed rather than captured when the listener opened.
pub trait ServedDatabase: Send + Sync + 'static {
    /// The resource as it stands NOW, or `None` when it is gone: a database
    /// that has been deleted serves no session, and the client is told the
    /// server is going away rather than served from a record nobody holds.
    fn facts(&self) -> Option<DatabaseFacts>;
}

/// What one session on a database's endpoint is served from.
#[derive(Clone)]
pub struct SessionContext {
    /// The stored resource a login is admitted against.
    served: Arc<dyn ServedDatabase>,
    /// The engine behind it.
    database: Arc<DatabaseEngine>,
    /// The core's TLS server, for the in-band upgrade each wire negotiates.
    /// `None` where this emulator has none prepared, and then a client that
    /// asks for encryption is told so rather than handed a broken stream.
    upgrader: Option<TlsUpgrader>,
    /// The connection ids this endpoint has handed out, counted on from the
    /// last one.
    next_connection_id: Arc<AtomicU32>,
}

impl SessionContext {
    /// The id the next session on this endpoint is given, counted on from the
    /// last one so two sessions open at once do not share one.
    #[must_use]
    pub fn take_connection_id(&self) -> u32 {
        self.next_connection_id.fetch_add(1, Ordering::Relaxed)
    }

    /// The core's TLS server, for the wire's own in-band upgrade.
    #[must_use]
    pub fn upgrader(&self) -> Option<&TlsUpgrader> {
        self.upgrader.as_ref()
    }

    /// The engine behind this endpoint.
    #[must_use]
    pub fn engine(&self) -> &Arc<DatabaseEngine> {
        &self.database
    }

    /// The stored resource as it stands now ([`ServedDatabase::facts`]).
    #[must_use]
    pub fn facts(&self) -> Option<DatabaseFacts> {
        self.served.facts()
    }
}

/// How a login proves the master user's password, which differs per wire: the
/// PostgreSQL startup carries the password itself, and the MySQL handshake
/// carries a digest over the server's own nonce.
pub enum PasswordProof<'a> {
    /// The password as the client sent it.
    Cleartext(&'a str),
    /// `mysql_native_password`: `SHA1(password) XOR SHA1(nonce ++
    /// SHA1(SHA1(password)))`, which is the only form a native-password login
    /// can be verified in.
    MysqlNative {
        nonce: &'a [u8],
        response: &'a [u8],
    },
}

impl PasswordProof<'_> {
    /// Whether this proof is the one `password` would produce.
    fn proves(&self, password: &str) -> bool {
        match self {
            Self::Cleartext(given) => {
                // Compared over the bytes, with no early exit on the first
                // difference, so a failed login says nothing about how much of
                // the password was right.
                let given = given.as_bytes();
                let held = password.as_bytes();
                let mut difference = u8::from(given.len() != held.len());
                for index in 0..given.len().max(held.len()) {
                    difference |= given.get(index).copied().unwrap_or(0)
                        ^ held.get(index).copied().unwrap_or(0);
                }
                difference == 0
            }
            Self::MysqlNative { nonce, response } => {
                response.len() == NATIVE_PROOF_BYTES
                    && *response == native_password_proof(password, nonce)
            }
        }
    }

    /// Whether the client sent a credential at all, which MySQL's own refusal
    /// states.
    fn carries_a_password(&self) -> bool {
        match self {
            Self::Cleartext(given) => !given.is_empty(),
            Self::MysqlNative { response, .. } => !response.is_empty(),
        }
    }
}

/// How long a `mysql_native_password` proof is: the SHA-1 digest it is built
/// from.
const NATIVE_PROOF_BYTES: usize = 20;

/// The `mysql_native_password` proof of `password` over `nonce`.
fn native_password_proof(password: &str, nonce: &[u8]) -> Vec<u8> {
    let first = Sha1::digest(password.as_bytes());
    let second = Sha1::digest(first);
    let mut hasher = Sha1::new();
    hasher.update(nonce);
    hasher.update(second);
    let scrambled = hasher.finalize();
    first
        .iter()
        .zip(scrambled.iter())
        .map(|(left, right)| left ^ right)
        .collect()
}

/// Why a login was refused, which each wire reports under its own protocol's
/// error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginRefusal {
    /// The engine is not running: the resource is still being created, or it
    /// has been stopped, restarted or deleted.
    NotServing { serving: ServingState },
    /// The session asked for a database this engine does not hold.
    UnknownDatabase { database: String },
    /// The account or the password is not the master user's.
    AuthenticationFailed { user: String, password_given: bool },
}

/// Admit one login against the stored resource, answering the database the
/// session is on.
///
/// The ONE admission decision both wires make, so a MySQL login and a
/// PostgreSQL login are held to the same rules: the engine has to be running,
/// the account has to be the resource's master user with its current password,
/// and the database asked for has to be the one the engine holds — a login that
/// names none is served the master database, which is the database the create
/// made inside the engine.
pub fn admit(
    facts: &DatabaseFacts,
    user: &str,
    database: Option<&str>,
    proof: &PasswordProof<'_>,
) -> Result<String, LoginRefusal> {
    if !facts.is_serving() {
        return Err(LoginRefusal::NotServing {
            serving: facts.serving,
        });
    }
    if user != facts.master_username || !proof.proves(&facts.master_user_password) {
        return Err(LoginRefusal::AuthenticationFailed {
            user: user.to_string(),
            password_given: proof.carries_a_password(),
        });
    }
    match database {
        None | Some("") => Ok(facts.master_database_name.clone()),
        Some(named) if named == facts.master_database_name => Ok(named.to_string()),
        Some(named) => Err(LoginRefusal::UnknownDatabase {
            database: named.to_string(),
        }),
    }
}

/// One database's open data plane: where it answers, what answers there, and
/// the engine behind it.
///
/// Dropping it closes the port and severs every session on it, and drops the
/// database's engine with its rows — which is what deleting the resource does.
pub struct OpenedSqlPlane {
    endpoint: ServedEndpoint,
    wire: EngineWire,
    database: Arc<DatabaseEngine>,
    /// The listener the core opened. Held, because dropping it closes the port.
    _listener: ResourceListener,
    /// The record that makes the published address dialable by an IN-PROCESS
    /// client of this emulator, withdrawn with this plane.
    _dialable: Option<OpenedPlanes>,
}

/// Written by hand rather than derived: a listener and a live engine carry no
/// `Debug`, and the store this is held on derives one.
impl std::fmt::Debug for OpenedSqlPlane {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenedSqlPlane")
            .field("endpoint", &self.endpoint)
            .field("wire", &self.wire)
            .finish_non_exhaustive()
    }
}

impl OpenedSqlPlane {
    /// Where this plane answers, which is what the control plane publishes.
    #[must_use]
    pub fn endpoint(&self) -> &ServedEndpoint {
        &self.endpoint
    }

    /// The wire its engine speaks.
    #[must_use]
    pub fn wire(&self) -> EngineWire {
        self.wire
    }

    /// The engine behind it, for a caller that reads what the database holds.
    #[must_use]
    pub fn database(&self) -> &Arc<DatabaseEngine> {
        &self.database
    }
}

/// Open the data plane of one database resource: its own engine, served on the
/// `wire` its engine speaks, on a listener the core opens.
///
/// The core binds the socket and assigns the port; this hands it the protocol
/// handler and reports back where it landed. The caller stores the plane — which
/// is what keeps the port open — and publishes [`OpenedSqlPlane::endpoint`] as
/// the resource's endpoint. `signing_name` is the SigV4 signing name of the
/// service that opened it, which names what answers at that address for an
/// in-process client ([`crate::dialable_data_planes`]).
///
/// `Err` when this emulator's engine has no listener surface, when the core
/// could not bind, or when the SQL engine could not be started: the caller
/// answers that with a modelled failure rather than creating a resource whose
/// published address answers nothing.
pub async fn open_sql_wire_plane<Eng: Send + Sync + 'static>(
    engine: &Eng,
    wire: EngineWire,
    served: Arc<dyn ServedDatabase>,
    signing_name: &str,
) -> Result<OpenedSqlPlane, String> {
    let listeners = shared_engine(engine)
        .ok_or_else(|| "this emulator's engine has no listener surface".to_string())?
        .listeners();
    let database = Arc::new(
        DatabaseEngine::open()
            .map_err(|error| format!("the SQL engine could not be started: {}", error.message))?,
    );
    let context = SessionContext {
        served,
        database: Arc::clone(&database),
        upgrader: listeners.tls_upgrader().ok(),
        next_connection_id: Arc::new(AtomicU32::new(1)),
    };
    // ONE protocol handler, served at both ends of the endpoint: the listener
    // the core accepts a client's connections on, and the connections an
    // IN-PROCESS client of this emulator opens to the address this endpoint
    // publishes. The wire negotiates its own encryption in band, so the plane
    // is handed byte 0 at both ends and upgrades it itself.
    let (listener, plane) = match wire {
        EngineWire::MySql => {
            let served = Arc::new(mysql_wire::MySqlWire::new(context));
            let listener = listeners
                .tcp_plaintext(SharedHandler(Arc::clone(&served)))
                .await
                .map_err(|error| format!("the database endpoint could not be opened: {error}"))?;
            (
                listener,
                dialable_data_planes::served_in_process(served, None),
            )
        }
        EngineWire::Postgres => {
            let served = Arc::new(pg_wire::PostgresWire::new(context));
            let listener = listeners
                .tcp_plaintext(SharedHandler(Arc::clone(&served)))
                .await
                .map_err(|error| format!("the database endpoint could not be opened: {error}"))?;
            (
                listener,
                dialable_data_planes::served_in_process(served, None),
            )
        }
    };
    let dialable = dialable_data_planes::DialablePlanes::of(engine).map(|planes| {
        let authority = crate::data_plane_addressing::listener_authority(&listener);
        planes.opening([authority], signing_name, &plane)
    });
    Ok(OpenedSqlPlane {
        endpoint: ServedEndpoint {
            address: listener.host(),
            port: i32::from(listener.port()),
        },
        wire,
        database,
        _listener: listener,
        _dialable: dialable,
    })
}

/// The SHARED engine the service is mounted on, which owns the listener
/// surface: call it with a handler's `&*self.engine`.
fn shared_engine<Eng: Send + Sync + 'static>(engine: &Eng) -> Option<&::engine::InMemoryEngine> {
    (engine as &dyn ::std::any::Any).downcast_ref::<::engine::InMemoryEngine>()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The facts of a serving database with one master user.
    fn serving_facts() -> DatabaseFacts {
        DatabaseFacts {
            name: "db".to_string(),
            serving: ServingState::Serving,
            master_username: "dbadmin".to_string(),
            master_user_password: "Pw9#xZq!shared".to_string(),
            master_database_name: "appdb".to_string(),
            server_version: "8.4.11".to_string(),
        }
    }

    /// One admission case: the user, the proof it presented, the database it
    /// asked for, and what the decision answers.
    type AdmissionCase<'a> = (
        &'a str,
        &'a str,
        PasswordProof<'a>,
        Option<&'a str>,
        Result<&'a str, LoginRefusal>,
    );

    /// RULE: a login is admitted only as the master user with its current
    /// password, on a database the engine holds — and the refusal says which of
    /// those failed. The rows span both proof forms and every member the
    /// decision reads.
    #[test]
    fn only_the_master_credentials_on_a_held_database_are_admitted() {
        let facts = serving_facts();
        let password = facts.master_user_password.clone();
        let nonce = b"a-nonce-of-twenty-ch";
        let proof = native_password_proof(&password, nonce);
        // (case, the user, the proof, the database asked for, what is admitted)
        let rows: Vec<AdmissionCase<'_>> = vec![
            (
                "the master user, cleartext",
                "dbadmin",
                PasswordProof::Cleartext(&password),
                Some("appdb"),
                Ok("appdb"),
            ),
            (
                "the master user, a native-password proof",
                "dbadmin",
                PasswordProof::MysqlNative {
                    nonce,
                    response: &proof,
                },
                Some("appdb"),
                Ok("appdb"),
            ),
            (
                "no database named, so the master database",
                "dbadmin",
                PasswordProof::Cleartext(&password),
                None,
                Ok("appdb"),
            ),
            (
                "a database this engine does not hold",
                "dbadmin",
                PasswordProof::Cleartext(&password),
                Some("other"),
                Err(LoginRefusal::UnknownDatabase {
                    database: "other".to_string(),
                }),
            ),
            (
                "the wrong password",
                "dbadmin",
                PasswordProof::Cleartext("not-it"),
                Some("appdb"),
                Err(LoginRefusal::AuthenticationFailed {
                    user: "dbadmin".to_string(),
                    password_given: true,
                }),
            ),
            (
                "no password at all",
                "dbadmin",
                PasswordProof::Cleartext(""),
                Some("appdb"),
                Err(LoginRefusal::AuthenticationFailed {
                    user: "dbadmin".to_string(),
                    password_given: false,
                }),
            ),
            (
                "another user with the master password",
                "someone",
                PasswordProof::Cleartext(&password),
                Some("appdb"),
                Err(LoginRefusal::AuthenticationFailed {
                    user: "someone".to_string(),
                    password_given: true,
                }),
            ),
            (
                "a native-password proof over another nonce",
                "dbadmin",
                PasswordProof::MysqlNative {
                    nonce: b"another-nonce-of-20c",
                    response: &proof,
                },
                Some("appdb"),
                Err(LoginRefusal::AuthenticationFailed {
                    user: "dbadmin".to_string(),
                    password_given: true,
                }),
            ),
        ];
        for (case, user, proof, database, want) in rows {
            let answer = admit(&facts, user, database, &proof);
            let want = want.map(str::to_string);
            assert_eq!(
                answer, want,
                "case {case:?}: expected {want:?}, got {answer:?}"
            );
        }
    }

    /// RULE: a database whose engine is not running admits nothing, and the
    /// refusal carries WHICH way it is not running, so each wire can report the
    /// condition its protocol has for it.
    #[test]
    fn a_database_that_is_not_serving_admits_nothing() {
        for serving in [ServingState::ComingUp, ServingState::GoingDown] {
            let facts = DatabaseFacts {
                serving,
                ..serving_facts()
            };
            let password = facts.master_user_password.clone();
            let answer = admit(
                &facts,
                &facts.master_username,
                Some(&facts.master_database_name),
                &PasswordProof::Cleartext(&password),
            );
            assert_eq!(
                answer,
                Err(LoginRefusal::NotServing { serving }),
                "case {serving:?}: expected the refusal to carry the state, got {answer:?}"
            );
        }
    }
}
