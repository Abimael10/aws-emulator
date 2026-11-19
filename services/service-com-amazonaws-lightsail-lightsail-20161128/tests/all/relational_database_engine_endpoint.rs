// SPDX-License-Identifier: CC0-1.0

//! The managed relational database's ENGINE ENDPOINT: the `masterEndpoint` a
//! database publishes, and the MySQL and PostgreSQL sessions a client opens on
//! it.
//!
//! The rule under test: the `masterEndpoint` a database publishes is an address
//! this emulator answers on and a port where the blueprint's own engine is
//! served — a client resolves and dials that address, completes the engine's
//! own handshake as the database's master user, and round-trips SQL whose
//! answers come from that database's own storage. A login that is not the
//! master user's, a database the engine does not hold, and a database whose
//! engine is not running are each refused with that protocol's own error; and
//! deleting the database closes the endpoint it published.
//!
//! Every client here is written against the wire itself (the MySQL greeting and
//! command phase, the PostgreSQL startup and simple query protocol), so what
//! these tests drive is the protocol a stock driver speaks rather than an
//! in-process shortcut — and everything runs in ONE process: the listeners are
//! opened by the control-plane calls below, on the core's own loopback
//! interface.
//!
//! The rows span what the rule generalises over: both engines the blueprint
//! catalog publishes, more than one major version of one of them, a session in
//! the clear and one that negotiates TLS in band on the same port, and the
//! credential and database cases a login is refused for.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::instance_lifecycle::Emulator;

/// The master user every database below is created with.
const MASTER_USER: &str = "dbadmin";

/// Its password. A test credential, and the only place in this suite it
/// appears.
const MASTER_PASSWORD: &str = "Pw9#xZq!emulator";

/// The database created inside each engine, which is what a session uses.
const MASTER_DATABASE: &str = "roundtripdb";

/// The address every published endpoint carries: this emulator's own loopback
/// interface, which is where the core binds its listeners by default and so the
/// only address a client of this suite can reach.
const LOOPBACK: &str = "127.0.0.1";

/// How long a case waits for a database to reach `available` before it gives
/// up: far longer than the span this service owes that transition in, so the
/// wait is on the CONDITION and not on a clock.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long a case waits for a closed port to stop accepting connections.
const CLOSE_DEADLINE: Duration = Duration::from_secs(120);

/// How often a wait looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Which wire a blueprint's engine speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wire {
    MySql,
    Postgres,
}

/// How a session is opened: in the clear, or with the wire's own in-band TLS
/// upgrade on the same port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    Plaintext,
    Tls,
}

/// One endpoint case: the blueprint a database is created from, the wire its
/// engine speaks, and how the session is opened.
struct Row {
    case: &'static str,
    database_name: &'static str,
    blueprint_id: &'static str,
    wire: Wire,
    transport: Transport,
}

/// The endpoint cases. The blueprint ids name entries of this service's own
/// published catalog (`GetRelationalDatabaseBlueprints`: `mysql_8_4` is the
/// MySQL engine default, `postgres_18` the PostgreSQL one, and `postgres_13`
/// another published major version).
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the default MySQL blueprint, a session in the clear",
            database_name: "wire-mysql-plain",
            blueprint_id: "mysql_8_4",
            wire: Wire::MySql,
            transport: Transport::Plaintext,
        },
        Row {
            case: "the default MySQL blueprint, TLS negotiated in band",
            database_name: "wire-mysql-tls",
            blueprint_id: "mysql_8_4",
            wire: Wire::MySql,
            transport: Transport::Tls,
        },
        Row {
            case: "the default PostgreSQL blueprint, a session in the clear",
            database_name: "wire-postgres-plain",
            blueprint_id: "postgres_18",
            wire: Wire::Postgres,
            transport: Transport::Plaintext,
        },
        Row {
            case: "another PostgreSQL major version, TLS negotiated in band",
            database_name: "wire-postgres-tls",
            blueprint_id: "postgres_13",
            wire: Wire::Postgres,
            transport: Transport::Tls,
        },
    ]
}

/// RULE: the endpoint a database publishes is an address this emulator answers
/// on where the blueprint's engine is served, and a client that dials it
/// round-trips SQL against that database's own storage.
#[tokio::test]
async fn the_published_endpoint_serves_the_blueprints_engine() {
    let emulator = Emulator::new();
    for row in rows() {
        let case = row.case;
        let database = available_database(&emulator, row.database_name, row.blueprint_id).await;
        let (address, port) = endpoint_of(case, &database);
        let engine_version = database["engineVersion"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: expected an engineVersion, got {database:?}"))
            .to_string();

        // CREATE TABLE / INSERT / SELECT, the round trip a client provisions a
        // database for, plus a value the ENGINE computes rather than echoes.
        let statements = [
            "CREATE TABLE sim2real_roundtrip (id INT PRIMARY KEY, val VARCHAR(64))",
            "INSERT INTO sim2real_roundtrip VALUES (1, 'hello-lightsail')",
            "INSERT INTO sim2real_roundtrip VALUES (2, 'second-row')",
        ];
        match row.wire {
            Wire::MySql => {
                let mut session = mysql_login(
                    case,
                    &address,
                    port,
                    row.transport,
                    MASTER_USER,
                    MASTER_PASSWORD,
                    Some(MASTER_DATABASE),
                )
                .await
                .unwrap_or_else(|refusal| {
                    panic!("case {case}: the master user must be admitted, got {refusal:?}")
                });
                assert_eq!(
                    session.server_version, engine_version,
                    "case {case}: expected the greeting to name the blueprint's engine version, \
                     got {:?}",
                    session.server_version
                );
                for statement in statements {
                    session.query(statement).await.unwrap_or_else(|refusal| {
                        panic!("case {case}: {statement} must run, got {refusal:?}")
                    });
                }
                let answer = session
                    .query("SELECT val FROM sim2real_roundtrip WHERE id = 1")
                    .await
                    .unwrap_or_else(|refusal| {
                        panic!("case {case}: the SELECT must run, got {refusal:?}")
                    });
                assert_eq!(
                    answer.only(),
                    Some("hello-lightsail"),
                    "case {case}: expected the row the INSERT wrote, got {answer:?}"
                );
                let counted = session
                    .query("SELECT COUNT(*) FROM sim2real_roundtrip")
                    .await
                    .unwrap_or_else(|refusal| {
                        panic!("case {case}: the aggregate must run, got {refusal:?}")
                    });
                assert_eq!(
                    counted.only(),
                    Some("2"),
                    "case {case}: expected the engine's own count of the rows, got {counted:?}"
                );
                let named = session
                    .query("SELECT database()")
                    .await
                    .unwrap_or_else(|refusal| {
                        panic!("case {case}: database() must run, got {refusal:?}")
                    });
                assert_eq!(
                    named.only(),
                    Some(MASTER_DATABASE),
                    "case {case}: expected the master database the create made, got {named:?}"
                );
            }
            Wire::Postgres => {
                let mut session = postgres_login(
                    case,
                    &address,
                    port,
                    row.transport,
                    MASTER_USER,
                    MASTER_PASSWORD,
                    Some(MASTER_DATABASE),
                )
                .await
                .unwrap_or_else(|refusal| {
                    panic!("case {case}: the master user must be admitted, got {refusal:?}")
                });
                assert_eq!(
                    session.parameter("server_version").as_deref(),
                    Some(engine_version.as_str()),
                    "case {case}: expected the backend to report the blueprint's engine version, \
                     got {:?}",
                    session.parameter("server_version")
                );
                for statement in statements {
                    session.query(statement).await.unwrap_or_else(|refusal| {
                        panic!("case {case}: {statement} must run, got {refusal:?}")
                    });
                }
                let answer = session
                    .query("SELECT val FROM sim2real_roundtrip WHERE id = 1")
                    .await
                    .unwrap_or_else(|refusal| {
                        panic!("case {case}: the SELECT must run, got {refusal:?}")
                    });
                assert_eq!(
                    answer.only(),
                    Some("hello-lightsail"),
                    "case {case}: expected the row the INSERT wrote, got {answer:?}"
                );
                let counted = session
                    .query("SELECT COUNT(*) FROM sim2real_roundtrip")
                    .await
                    .unwrap_or_else(|refusal| {
                        panic!("case {case}: the aggregate must run, got {refusal:?}")
                    });
                assert_eq!(
                    counted.only(),
                    Some("2"),
                    "case {case}: expected the engine's own count of the rows, got {counted:?}"
                );
                let named = session
                    .query("SELECT current_database()")
                    .await
                    .unwrap_or_else(|refusal| {
                        panic!("case {case}: current_database() must run, got {refusal:?}")
                    });
                assert_eq!(
                    named.only(),
                    Some(MASTER_DATABASE),
                    "case {case}: expected the master database the create made, got {named:?}"
                );
            }
        }
    }
}

/// RULE: a session is admitted only as the database's own master user with its
/// current password, using a database that engine holds — every other login is
/// refused with the protocol's own error for it.
#[tokio::test]
async fn a_session_is_admitted_only_with_the_databases_own_credentials() {
    let emulator = Emulator::new();
    // (case, the wire, the user, the password, the database asked for, the
    // error the protocol refuses it with)
    let mysql = available_database(&emulator, "login-mysql", "mysql_8_4").await;
    let postgres = available_database(&emulator, "login-postgres", "postgres_18").await;
    let (mysql_address, mysql_port) = endpoint_of("login-mysql", &mysql);
    let (postgres_address, postgres_port) = endpoint_of("login-postgres", &postgres);

    let mysql_cases: &[(&str, &str, &str, Option<&str>, &str)] = &[
        (
            "the wrong password",
            MASTER_USER,
            "not-the-password",
            Some(MASTER_DATABASE),
            "1045",
        ),
        (
            "no password at all",
            MASTER_USER,
            "",
            Some(MASTER_DATABASE),
            "1045",
        ),
        (
            "another user",
            "someone_else",
            MASTER_PASSWORD,
            Some(MASTER_DATABASE),
            "1045",
        ),
        (
            "a database this engine does not hold",
            MASTER_USER,
            MASTER_PASSWORD,
            Some("another_db"),
            "1049",
        ),
    ];
    for (case, user, password, database, code) in mysql_cases {
        let refusal = mysql_login(
            case,
            &mysql_address,
            mysql_port,
            Transport::Plaintext,
            user,
            password,
            *database,
        )
        .await
        .err()
        .unwrap_or_else(|| panic!("case {case}: the login must be refused"));
        assert_eq!(
            refusal.code, *code,
            "case {case}: expected the MySQL error {code}, got {refusal:?}"
        );
    }
    // The master user's own credentials are admitted on that same endpoint, so
    // the refusals above are the credential's and not the endpoint's.
    mysql_login(
        "the master user",
        &mysql_address,
        mysql_port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(MASTER_DATABASE),
    )
    .await
    .unwrap_or_else(|refusal| {
        panic!("case the master user: the master credentials must be admitted, got {refusal:?}")
    });

    let postgres_cases: &[(&str, &str, &str, Option<&str>, &str)] = &[
        (
            "the wrong password",
            MASTER_USER,
            "not-the-password",
            Some(MASTER_DATABASE),
            "28P01",
        ),
        (
            "another user",
            "someone_else",
            MASTER_PASSWORD,
            Some(MASTER_DATABASE),
            "28P01",
        ),
        (
            "a database this engine does not hold",
            MASTER_USER,
            MASTER_PASSWORD,
            Some("another_db"),
            "3D000",
        ),
    ];
    for (case, user, password, database, code) in postgres_cases {
        let refusal = postgres_login(
            case,
            &postgres_address,
            postgres_port,
            Transport::Plaintext,
            user,
            password,
            *database,
        )
        .await
        .err()
        .unwrap_or_else(|| panic!("case {case}: the login must be refused"));
        assert_eq!(
            refusal.code, *code,
            "case {case}: expected the PostgreSQL SQLSTATE {code}, got {refusal:?}"
        );
    }
    postgres_login(
        "the master user",
        &postgres_address,
        postgres_port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(MASTER_DATABASE),
    )
    .await
    .unwrap_or_else(|refusal| {
        panic!("case the master user: the master credentials must be admitted, got {refusal:?}")
    });
}

/// RULE: a database whose engine is not running serves no session — a stopped
/// database refuses the login with the protocol's own "the server is not
/// available" error, and serves again once it has been started.
#[tokio::test]
async fn a_database_that_is_not_available_refuses_a_session() {
    let emulator = Emulator::new();
    let case = "a stopped database";
    let database = available_database(&emulator, "stopped-mysql", "mysql_8_4").await;
    let (address, port) = endpoint_of(case, &database);
    emulator
        .ok(
            "StopRelationalDatabase",
            json!({"relationalDatabaseName": "stopped-mysql"}),
        )
        .await;
    wait_for_state(&emulator, "stopped-mysql", "stopped").await;

    let refusal = mysql_login(
        case,
        &address,
        port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(MASTER_DATABASE),
    )
    .await
    .err()
    .unwrap_or_else(|| panic!("case {case}: a stopped database must refuse the login"));
    assert_eq!(
        refusal.code, "1053",
        "case {case}: expected the MySQL server-shutdown error, got {refusal:?}"
    );

    emulator
        .ok(
            "StartRelationalDatabase",
            json!({"relationalDatabaseName": "stopped-mysql"}),
        )
        .await;
    let started = wait_for_state(&emulator, "stopped-mysql", "available").await;
    assert_eq!(
        endpoint_of(case, &started),
        (address.clone(), port),
        "case {case}: a database keeps the endpoint it published across a stop"
    );
    mysql_login(
        case,
        &address,
        port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(MASTER_DATABASE),
    )
    .await
    .unwrap_or_else(|refusal| {
        panic!("case {case}: a started database must serve again, got {refusal:?}")
    });
}

/// RULE: each database serves its OWN storage, and deleting a database closes
/// the endpoint it published.
#[tokio::test]
async fn each_database_serves_its_own_storage_and_a_delete_closes_its_endpoint() {
    let emulator = Emulator::new();
    let case = "two databases of one account";
    let first = available_database(&emulator, "own-storage-one", "mysql_8_4").await;
    let second = available_database(&emulator, "own-storage-two", "mysql_8_4").await;
    let (first_address, first_port) = endpoint_of(case, &first);
    let (second_address, second_port) = endpoint_of(case, &second);
    assert_ne!(
        (first_address.clone(), first_port),
        (second_address.clone(), second_port),
        "case {case}: two databases are served at two endpoints"
    );

    let mut writer = mysql_login(
        case,
        &first_address,
        first_port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(MASTER_DATABASE),
    )
    .await
    .unwrap_or_else(|refusal| panic!("case {case}: the first login must be admitted, got {refusal:?}"));
    for statement in [
        "CREATE TABLE only_in_the_first (id INT PRIMARY KEY)",
        "INSERT INTO only_in_the_first VALUES (7)",
    ] {
        writer
            .query(statement)
            .await
            .unwrap_or_else(|refusal| panic!("case {case}: {statement} must run, got {refusal:?}"));
    }

    let mut other = mysql_login(
        case,
        &second_address,
        second_port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(MASTER_DATABASE),
    )
    .await
    .unwrap_or_else(|refusal| {
        panic!("case {case}: the second login must be admitted, got {refusal:?}")
    });
    let refusal = other
        .query("SELECT id FROM only_in_the_first")
        .await
        .err()
        .unwrap_or_else(|| {
            panic!("case {case}: the second database holds no table the first one created")
        });
    assert_eq!(
        refusal.code, "1146",
        "case {case}: expected the MySQL unknown-table error, got {refusal:?}"
    );
    let tables = other
        .query("SHOW TABLES")
        .await
        .unwrap_or_else(|refusal| panic!("case {case}: SHOW TABLES must run, got {refusal:?}"));
    assert_eq!(
        tables.rows.len(),
        0,
        "case {case}: the second database holds no table at all, got {tables:?}"
    );

    // The delete closes the port: the listener is the database's, so it goes
    // with it. Looked for until it holds, because the connections the delete
    // severs are closed by the core.
    emulator
        .ok(
            "DeleteRelationalDatabase",
            json!({
                "relationalDatabaseName": "own-storage-one",
                "skipFinalSnapshot": true,
            }),
        )
        .await;
    let deadline = Instant::now() + CLOSE_DEADLINE;
    loop {
        // The address was pinned to this emulator's own loopback interface when
        // the endpoint was read ([`endpoint_of`]), so the dial names it.
        let reachable = TcpStream::connect(("127.0.0.1", first_port)).await.is_ok();
        if !reachable {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case}: expected the endpoint {first_address}:{first_port} of the deleted \
             database to stop accepting connections"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    // The database that was not deleted still serves at its own endpoint.
    mysql_login(
        case,
        &second_address,
        second_port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(MASTER_DATABASE),
    )
    .await
    .unwrap_or_else(|refusal| {
        panic!("case {case}: the other database keeps serving, got {refusal:?}")
    });
}

// ---------------------------------------------------------------------------
// The control plane, as a client drives it
// ---------------------------------------------------------------------------

/// Create the database `name` from `blueprint_id`, wait for it to report
/// `available`, and answer the record a client reads.
async fn available_database(emulator: &Emulator, name: &str, blueprint_id: &str) -> Value {
    emulator
        .ok(
            "CreateRelationalDatabase",
            json!({
                "relationalDatabaseName": name,
                "relationalDatabaseBlueprintId": blueprint_id,
                "relationalDatabaseBundleId": "micro_2_0",
                "masterDatabaseName": MASTER_DATABASE,
                "masterUsername": MASTER_USER,
                "masterUserPassword": MASTER_PASSWORD,
                "publiclyAccessible": true,
            }),
        )
        .await;
    wait_for_state(emulator, name, "available").await
}

/// Read the database `name` until it reports `state`, and answer that read.
///
/// The wait is on the CONDITION — the state a client has to see before it
/// connects — looked for again until it holds or a deadline far longer than the
/// span this service owes the transition in has passed.
async fn wait_for_state(emulator: &Emulator, name: &str, state: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = emulator
            .ok(
                "GetRelationalDatabase",
                json!({"relationalDatabaseName": name}),
            )
            .await["relationalDatabase"]
            .clone();
        if read["state"].as_str() == Some(state) {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {name}: expected the database to reach {state}, last read {read:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The endpoint a database published, as a client reads it: an address this
/// emulator answers on and the port its engine listens on there.
fn endpoint_of(case: &str, database: &Value) -> (String, u16) {
    let endpoint = &database["masterEndpoint"];
    let address = endpoint["address"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: expected a masterEndpoint address, got {database:?}"))
        .to_string();
    let port = endpoint["port"]
        .as_u64()
        .unwrap_or_else(|| panic!("case {case}: expected a masterEndpoint port, got {database:?}"));
    // The address has to be one this emulator is REACHED on: the core binds its
    // listeners on the operator-chosen interface, loopback by default, so a
    // published address that is not an address of this machine is one no client
    // of the emulator can connect to.
    assert_eq!(
        address, LOOPBACK,
        "case {case}: expected an address this emulator answers on, got {address:?}"
    );
    (
        address,
        u16::try_from(port)
            .unwrap_or_else(|_| panic!("case {case}: expected a port, got {port}")),
    )
}

// ---------------------------------------------------------------------------
// The wires, as a client speaks them
// ---------------------------------------------------------------------------

/// A connection to an engine endpoint, in the clear or upgraded to TLS.
trait Connection: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Connection for T {}

/// What a client holds: the stream, whatever it is now.
type Client = Box<dyn Connection>;

/// A refusal a server sent: the code a client branches on and the sentence it
/// reports.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Refusal {
    code: String,
    message: String,
}

/// One result set a client read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Rows {
    columns: Vec<String>,
    rows: Vec<Vec<Option<String>>>,
}

impl Rows {
    /// The one value a single-row, single-column answer carries.
    fn only(&self) -> Option<&str> {
        self.rows
            .first()
            .and_then(|row| row.first())
            .and_then(Option::as_deref)
    }
}

/// Open a TCP connection to the endpoint a database published.
///
/// The address is this emulator's own loopback interface, pinned when the
/// endpoint was read out of the control plane ([`endpoint_of`]): the core binds
/// its listeners there, so what is dialled is a port this same process opened.
async fn connect(case: &str, address: &str, port: u16) -> Client {
    assert_eq!(
        address, LOOPBACK,
        "case {case}: the published endpoint must be this machine's loopback address, got \
         {address:?}"
    );
    let stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap_or_else(|error| {
            panic!(
                "case {case}: the published endpoint {address}:{port} must accept a connection, \
                 got {error}"
            )
        });
    Box::new(stream)
}

/// Run the client side of a TLS handshake on `conn`.
///
/// The verifier accepts the certificate the EMULATOR'S OWN core generated for
/// its listeners, which is self-signed and names loopback: this client is the
/// emulator's own test, connecting to a port the same process opened, so there
/// is no authority to check it against.
async fn upgrade_to_tls(case: &str, conn: Client) -> Client {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .expect("the provider supports the default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptTheEmulatorsCertificate(provider)))
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let server_name =
        rustls::pki_types::ServerName::try_from("localhost").expect("localhost is a server name");
    let stream = connector
        .connect(server_name, conn)
        .await
        .unwrap_or_else(|error| {
            panic!("case {case}: the server must complete the in-band TLS handshake, got {error}")
        });
    Box::new(stream)
}

/// See [`upgrade_to_tls`].
#[derive(Debug)]
struct AcceptTheEmulatorsCertificate(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AcceptTheEmulatorsCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0
            .signature_verification_algorithms
            .supported_schemes()
    }
}

// --- MySQL -----------------------------------------------------------------

/// The capability flags this client declares, from the protocol's own list.
const CLIENT_CONNECT_WITH_DB: u32 = 0x0000_0008;
const CLIENT_PROTOCOL_41: u32 = 0x0000_0200;
const CLIENT_SSL: u32 = 0x0000_0800;
const CLIENT_SECURE_CONNECTION: u32 = 0x0000_8000;
const CLIENT_PLUGIN_AUTH: u32 = 0x0008_0000;
const CLIENT_DEPRECATE_EOF: u32 = 0x0100_0000;

/// A MySQL session a client has opened, and the version the greeting named.
struct MySqlSession {
    conn: Client,
    server_version: String,
}

/// Open a MySQL session on `address:port`, as libmysqlclient does: read the
/// greeting, answer it (with the TLS upgrade in the middle when asked for), and
/// read the server's verdict.
async fn mysql_login(
    case: &str,
    address: &str,
    port: u16,
    transport: Transport,
    user: &str,
    password: &str,
    database: Option<&str>,
) -> Result<MySqlSession, Refusal> {
    let mut conn = connect(case, address, port).await;
    let (_, greeting) = mysql_read(case, &mut conn).await;
    assert_eq!(
        greeting[0], 10,
        "case {case}: the greeting must announce protocol version 10"
    );
    let (server_version, at) = read_cstring(&greeting, 1);
    // connection id (4), the first 8 nonce bytes, a filler, the lower
    // capability flags, the charset, the status flags, the upper capability
    // flags, the nonce length, 10 reserved bytes, then the rest of the nonce.
    let mut nonce = greeting[at + 4..at + 12].to_vec();
    nonce.extend_from_slice(&greeting[at + 31..at + 43]);
    let server_capabilities = u32::from(u16::from_le_bytes([greeting[at + 13], greeting[at + 14]]))
        | (u32::from(u16::from_le_bytes([greeting[at + 19], greeting[at + 20]])) << 16);
    assert!(
        server_capabilities & CLIENT_SSL != 0,
        "case {case}: the greeting must offer CLIENT_SSL, so a client can negotiate TLS in band"
    );

    let mut capabilities =
        CLIENT_PROTOCOL_41 | CLIENT_SECURE_CONNECTION | CLIENT_PLUGIN_AUTH | CLIENT_DEPRECATE_EOF;
    if database.is_some() {
        capabilities |= CLIENT_CONNECT_WITH_DB;
    }
    if transport == Transport::Tls {
        capabilities |= CLIENT_SSL;
        // The 32-byte handshake response that asks for TLS and nothing else.
        let mut request = Vec::new();
        request.extend_from_slice(&capabilities.to_le_bytes());
        request.extend_from_slice(&(16u32 * 1024 * 1024).to_le_bytes());
        request.push(255);
        request.extend_from_slice(&[0u8; 23]);
        mysql_write(case, &mut conn, 1, &request).await;
        conn = upgrade_to_tls(case, conn).await;
    }

    let mut response = Vec::new();
    response.extend_from_slice(&capabilities.to_le_bytes());
    response.extend_from_slice(&(16u32 * 1024 * 1024).to_le_bytes());
    response.push(255);
    response.extend_from_slice(&[0u8; 23]);
    put_cstring(&mut response, user);
    let proof = if password.is_empty() {
        Vec::new()
    } else {
        mysql_native_proof(password, &nonce)
    };
    response.push(u8::try_from(proof.len()).expect("the proof is at most 20 bytes"));
    response.extend_from_slice(&proof);
    if let Some(database) = database {
        put_cstring(&mut response, database);
    }
    put_cstring(&mut response, "mysql_native_password");
    let sequence = if transport == Transport::Tls { 2 } else { 1 };
    mysql_write(case, &mut conn, sequence, &response).await;

    let (_, verdict) = mysql_read(case, &mut conn).await;
    match verdict[0] {
        0x00 => Ok(MySqlSession {
            conn,
            server_version,
        }),
        0xff => Err(mysql_refusal(&verdict)),
        other => panic!("case {case}: unexpected answer {other:#x} to the handshake response"),
    }
}

impl MySqlSession {
    /// Run one statement through `COM_QUERY` and read everything the server
    /// answers.
    async fn query(&mut self, sql: &str) -> Result<Rows, Refusal> {
        let case = "a statement";
        let mut command = vec![0x03u8];
        command.extend_from_slice(sql.as_bytes());
        mysql_write(case, &mut self.conn, 0, &command).await;
        let (_, first) = mysql_read(case, &mut self.conn).await;
        match first[0] {
            0x00 => return Ok(Rows::default()),
            0xff => return Err(mysql_refusal(&first)),
            _ => {}
        }
        let (columns, _) = read_length_encoded(&first, 0);
        let mut answer = Rows::default();
        for _ in 0..columns {
            let (_, definition) = mysql_read(case, &mut self.conn).await;
            // catalog, schema, table, org_table, name, org_name: the name is
            // the fifth length-encoded string.
            let mut at = 0usize;
            let mut name = String::new();
            for field in 0..6 {
                let (value, next) = read_length_encoded_string(&definition, at);
                at = next;
                if field == 4 {
                    name = value;
                }
            }
            answer.columns.push(name);
        }
        loop {
            let (_, packet) = mysql_read(case, &mut self.conn).await;
            // The packet that ends a result set: an OK or EOF marker, never a
            // row, which always begins with a length-encoded value.
            if packet[0] == 0xfe && packet.len() < 9 {
                break;
            }
            if packet[0] == 0xff {
                return Err(mysql_refusal(&packet));
            }
            let mut row = Vec::new();
            let mut at = 0usize;
            while at < packet.len() {
                if packet[at] == 0xfb {
                    row.push(None);
                    at += 1;
                    continue;
                }
                let (value, next) = read_length_encoded_string(&packet, at);
                row.push(Some(value));
                at = next;
            }
            answer.rows.push(row);
        }
        Ok(answer)
    }
}

/// `SHA1(password) XOR SHA1(nonce || SHA1(SHA1(password)))` — the
/// `mysql_native_password` proof a client answers the greeting's nonce with.
fn mysql_native_proof(password: &str, nonce: &[u8]) -> Vec<u8> {
    let first = Sha1::digest(password.as_bytes());
    let stored = Sha1::digest(first);
    let mut hasher = Sha1::new();
    hasher.update(nonce);
    hasher.update(stored);
    let mask = hasher.finalize();
    first
        .iter()
        .zip(mask.iter())
        .map(|(value, mask)| value ^ mask)
        .collect()
}

/// Write one MySQL packet.
async fn mysql_write(case: &str, conn: &mut Client, sequence: u8, payload: &[u8]) {
    let length = u32::try_from(payload.len()).expect("the packet fits");
    let mut packet = length.to_le_bytes()[..3].to_vec();
    packet.push(sequence);
    packet.extend_from_slice(payload);
    conn.write_all(&packet)
        .await
        .unwrap_or_else(|error| panic!("case {case}: the packet is sent, got {error}"));
}

/// Read one MySQL packet.
async fn mysql_read(case: &str, conn: &mut Client) -> (u8, Vec<u8>) {
    let mut header = [0u8; 4];
    conn.read_exact(&mut header)
        .await
        .unwrap_or_else(|error| panic!("case {case}: the server answers, got {error}"));
    let length =
        usize::from(header[0]) | (usize::from(header[1]) << 8) | (usize::from(header[2]) << 16);
    let mut payload = vec![0u8; length];
    conn.read_exact(&mut payload)
        .await
        .unwrap_or_else(|error| panic!("case {case}: the server sends the whole packet, got {error}"));
    (header[3], payload)
}

/// The error number and the message an `ERR` packet carries.
fn mysql_refusal(payload: &[u8]) -> Refusal {
    let number = u16::from_le_bytes([payload[1], payload[2]]);
    // `#` then the five-character SQLSTATE, then the message.
    Refusal {
        code: number.to_string(),
        message: String::from_utf8_lossy(&payload[9..]).into_owned(),
    }
}

/// A length-encoded integer and the index after it.
fn read_length_encoded(payload: &[u8], at: usize) -> (u64, usize) {
    match payload[at] {
        0xfc => (
            u64::from(u16::from_le_bytes([payload[at + 1], payload[at + 2]])),
            at + 3,
        ),
        0xfd => (
            u64::from(u32::from_le_bytes([
                payload[at + 1],
                payload[at + 2],
                payload[at + 3],
                0,
            ])),
            at + 4,
        ),
        0xfe => {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&payload[at + 1..at + 9]);
            (u64::from_le_bytes(bytes), at + 9)
        }
        value => (u64::from(value), at + 1),
    }
}

/// A length-encoded string and the index after it.
fn read_length_encoded_string(payload: &[u8], at: usize) -> (String, usize) {
    let (length, next) = read_length_encoded(payload, at);
    let length = usize::try_from(length).expect("a value length fits");
    (
        String::from_utf8_lossy(&payload[next..next + length]).into_owned(),
        next + length,
    )
}

// --- PostgreSQL ------------------------------------------------------------

/// A PostgreSQL session a client has opened, and the parameters the backend
/// reported while it opened.
struct PostgresSession {
    conn: Client,
    parameters: Vec<(String, String)>,
}

impl PostgresSession {
    /// The value the backend reported for the parameter `name`.
    fn parameter(&self, name: &str) -> Option<String> {
        self.parameters
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, value)| value.clone())
    }
}

/// Open a PostgreSQL session on `address:port`, as libpq does: the optional
/// `SSLRequest` first, then the startup message, the password, and the
/// parameters the server reports.
async fn postgres_login(
    case: &str,
    address: &str,
    port: u16,
    transport: Transport,
    user: &str,
    password: &str,
    database: Option<&str>,
) -> Result<PostgresSession, Refusal> {
    let mut conn = connect(case, address, port).await;
    if transport == Transport::Tls {
        let mut request = Vec::new();
        request.extend_from_slice(&8i32.to_be_bytes());
        // The `SSLRequest` code, sent in the clear as the connection's first
        // bytes.
        request.extend_from_slice(&80_877_103i32.to_be_bytes());
        conn.write_all(&request)
            .await
            .unwrap_or_else(|error| panic!("case {case}: the request is sent, got {error}"));
        let mut answer = [0u8; 1];
        conn.read_exact(&mut answer)
            .await
            .unwrap_or_else(|error| panic!("case {case}: the server answers, got {error}"));
        assert_eq!(
            answer[0], b'S',
            "case {case}: the server must answer an SSLRequest with 'S' before any TLS"
        );
        conn = upgrade_to_tls(case, conn).await;
    }
    let mut body = Vec::new();
    // Protocol version 3.0.
    body.extend_from_slice(&196_608i32.to_be_bytes());
    put_cstring(&mut body, "user");
    put_cstring(&mut body, user);
    if let Some(database) = database {
        put_cstring(&mut body, "database");
        put_cstring(&mut body, database);
    }
    body.push(0);
    let mut startup = Vec::new();
    startup.extend_from_slice(
        &i32::try_from(body.len() + 4)
            .expect("the startup message fits")
            .to_be_bytes(),
    );
    startup.extend_from_slice(&body);
    conn.write_all(&startup)
        .await
        .unwrap_or_else(|error| panic!("case {case}: the startup is sent, got {error}"));

    let mut parameters = Vec::new();
    loop {
        let (kind, body) = postgres_read(case, &mut conn).await;
        match kind {
            // AuthenticationRequest.
            b'R' => {
                let code = i32::from_be_bytes([body[0], body[1], body[2], body[3]]);
                if code == 0 {
                    continue;
                }
                assert_eq!(
                    code, 3,
                    "case {case}: this endpoint asks for a cleartext password, got the \
                     authentication request {code}"
                );
                let mut message = Vec::new();
                put_cstring(&mut message, password);
                postgres_send(case, &mut conn, b'p', &message).await;
            }
            b'E' => return Err(postgres_refusal(&body)),
            // ParameterStatus.
            b'S' => {
                let (name, next) = read_cstring(&body, 0);
                let (value, _) = read_cstring(&body, next);
                parameters.push((name, value));
            }
            // ReadyForQuery: the session is open.
            b'Z' => return Ok(PostgresSession { conn, parameters }),
            // BackendKeyData, NoticeResponse.
            _ => {}
        }
    }
}

impl PostgresSession {
    /// Run one simple query and read everything the server answers for it.
    async fn query(&mut self, sql: &str) -> Result<Rows, Refusal> {
        let case = "a statement";
        let mut message = Vec::new();
        put_cstring(&mut message, sql);
        postgres_send(case, &mut self.conn, b'Q', &message).await;
        let mut answer = Rows::default();
        let mut refusal = None;
        loop {
            let Some((kind, body)) = postgres_try_read(&mut self.conn).await else {
                return Err(refusal.unwrap_or(Refusal {
                    code: "08006".to_string(),
                    message: "the server closed the connection".to_string(),
                }));
            };
            match kind {
                // RowDescription.
                b'T' => {
                    let count = usize::from(u16::from_be_bytes([body[0], body[1]]));
                    let mut at = 2usize;
                    answer.columns.clear();
                    for _ in 0..count {
                        let (name, next) = read_cstring(&body, at);
                        answer.columns.push(name);
                        // name, table oid, column number, type oid, size,
                        // modifier, format code.
                        at = next + 18;
                    }
                }
                // DataRow.
                b'D' => {
                    let count = usize::from(u16::from_be_bytes([body[0], body[1]]));
                    let mut at = 2usize;
                    let mut row = Vec::with_capacity(count);
                    for _ in 0..count {
                        let length =
                            i32::from_be_bytes([body[at], body[at + 1], body[at + 2], body[at + 3]]);
                        at += 4;
                        if length < 0 {
                            row.push(None);
                            continue;
                        }
                        let length = usize::try_from(length).expect("a value length fits");
                        row.push(Some(
                            String::from_utf8_lossy(&body[at..at + length]).into_owned(),
                        ));
                        at += length;
                    }
                    answer.rows.push(row);
                }
                b'E' => refusal = Some(postgres_refusal(&body)),
                // ReadyForQuery ends the exchange.
                b'Z' => break,
                // CommandComplete, EmptyQueryResponse, NoticeResponse.
                _ => {}
            }
        }
        match refusal {
            Some(refusal) => Err(refusal),
            None => Ok(answer),
        }
    }
}

/// Write one PostgreSQL message.
async fn postgres_send(case: &str, conn: &mut Client, kind: u8, body: &[u8]) {
    let mut message = vec![kind];
    message.extend_from_slice(
        &i32::try_from(body.len() + 4)
            .expect("the message fits")
            .to_be_bytes(),
    );
    message.extend_from_slice(body);
    conn.write_all(&message)
        .await
        .unwrap_or_else(|error| panic!("case {case}: the message is sent, got {error}"));
}

/// Read one PostgreSQL message, panicking where the server should have
/// answered.
async fn postgres_read(case: &str, conn: &mut Client) -> (u8, Vec<u8>) {
    postgres_try_read(conn)
        .await
        .unwrap_or_else(|| panic!("case {case}: the server answers"))
}

/// Read one PostgreSQL message, or `None` where the server closed the
/// connection.
async fn postgres_try_read(conn: &mut Client) -> Option<(u8, Vec<u8>)> {
    let mut header = [0u8; 5];
    conn.read_exact(&mut header).await.ok()?;
    let length = i32::from_be_bytes([header[1], header[2], header[3], header[4]]);
    let body_length = usize::try_from(length - 4).expect("a message length fits");
    let mut body = vec![0u8; body_length];
    conn.read_exact(&mut body).await.ok()?;
    Some((header[0], body))
}

/// The SQLSTATE and the message an `ErrorResponse` carries.
fn postgres_refusal(body: &[u8]) -> Refusal {
    let mut refusal = Refusal {
        code: String::new(),
        message: String::new(),
    };
    let mut at = 0usize;
    while at < body.len() && body[at] != 0 {
        let field = body[at];
        let (value, next) = read_cstring(body, at + 1);
        match field {
            b'C' => refusal.code = value,
            b'M' => refusal.message = value,
            _ => {}
        }
        at = next;
    }
    refusal
}

// --- shared framing --------------------------------------------------------

/// The NUL-terminated string at `at`, and the index after it.
fn read_cstring(payload: &[u8], at: usize) -> (String, usize) {
    let rest = &payload[at..];
    let end = rest.iter().position(|byte| *byte == 0).unwrap_or(rest.len());
    (
        String::from_utf8_lossy(&rest[..end]).into_owned(),
        at + end + 1,
    )
}

/// Append a NUL-terminated string.
fn put_cstring(payload: &mut Vec<u8>, text: &str) {
    payload.extend_from_slice(text.as_bytes());
    payload.push(0);
}
