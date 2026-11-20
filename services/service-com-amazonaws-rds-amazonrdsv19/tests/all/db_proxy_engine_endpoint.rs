// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the DB PROXY ENDPOINT: the address `DBProxy$Endpoint` publishes,
//! and the database sessions a client opens on it.
//!
//! The rule under test: a published endpoint is an address THIS EMULATOR ANSWERS ON, so for
//! every `EngineFamily` whose database protocol this emulator speaks the endpoint a proxy
//! publishes is its own listener's `host:port` — the string a client dials — and a session
//! opened on it is served by the database the proxy's target group holds, under the
//! encryption `RequireTLS` demands of connections to the proxy. A proxy of an engine family
//! this emulator speaks no protocol for gets no listener and keeps the DNS-form endpoint,
//! because a port that answers nothing would be worse than a name.
//!
//! Every control-plane call goes over the wire through `db_instances::emulator`, and every
//! database session is written against the protocol itself with the client this test target
//! already has for a DB instance's endpoint (`db_instance_engine_endpoint`), so what these
//! tests drive is what a stock driver speaks. Everything runs in ONE process: the listeners
//! are opened by the control-plane calls below, on the core's own loopback interface.
//!
//! The tables span what the rules generalise over: the engine families `EngineFamily` names
//! (including the one this emulator serves no protocol for), both states of `RequireTLS`,
//! encrypted and plaintext sessions, a target whose own engine refuses a plaintext login
//! and one whose engine admits it, and both directions of the round trip between the
//! proxy's endpoint and the database's own.

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use service_com_amazonaws_rds_amazonrdsv19::state::{
    DB_PROXY_STATUS_AVAILABLE, TARGET_STATE_AVAILABLE,
};
use smithy_server_runtime::MultiServiceRouter;
use tokio::net::TcpStream;

use crate::db_instance_engine_endpoint::{
    available_instance, mysql_login, mysql_read, postgres_login, Client, Refusal, Transport,
    DATABASE, MASTER_PASSWORD, MASTER_USER,
};
use crate::db_instances::{call, emulator, expect_ok, text, texts, REGION};
use crate::lifecycle_poll_loops::poll_until;

/// An IAM role ARN of the form `CreateDBProxy$RoleArn` takes.
const PROXY_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/rds-proxy";

/// A subnet id of the published form (`subnet-` plus eight hexadecimal characters).
const SUBNET_ID: &str = "subnet-0a1b2c3d";

/// The host the core binds its listeners on, which is therefore the host every endpoint
/// this emulator publishes answers at (`EMULATOR_BIND_ADDRESS`, loopback by default).
const LOOPBACK: &str = "127.0.0.1";

/// How long a wait for a condition this suite drives is given before it calls it
/// unobservable. Generous next to the spans this service owes its transitions in, so a
/// loaded test machine cannot fail the suite.
const WAIT_DEADLINE: Duration = Duration::from_secs(20);

/// The gap between two looks at a condition that is not a control-plane read.
const LOOK_INTERVAL: Duration = Duration::from_millis(25);

/// `CreateDBProxy` for a proxy of `engine_family`, answered with the `Endpoint` it
/// published, once the proxy reports itself ready to handle requests.
async fn available_proxy(
    router: &MultiServiceRouter,
    name: &str,
    engine_family: &str,
    require_tls: bool,
) -> String {
    let created = expect_ok(
        name,
        call(
            router,
            "CreateDBProxy",
            &[
                ("DBProxyName", name),
                ("EngineFamily", engine_family),
                ("RoleArn", PROXY_ROLE_ARN),
                ("VpcSubnetIds.member.1", SUBNET_ID),
                ("RequireTLS", if require_tls { "true" } else { "false" }),
            ],
        )
        .await,
    );
    let published = text(&created, "Endpoint");
    // The proxy comes up on its own, so what this waits for is the CONDITION a client has
    // to see before it connects, read again until it holds.
    let described_params = [("DBProxyName", name.to_string())];
    let (described, _polls) = poll_until(
        name,
        "the proxy",
        || call(router, "DescribeDBProxies", &described_params),
        |status, body| status == 200 && text(body, "Status") == DB_PROXY_STATUS_AVAILABLE,
    )
    .await;
    assert_eq!(
        text(&described, "Endpoint"),
        published,
        "case {name:?}: the endpoint the create published must be the endpoint the describe \
         reports"
    );
    published
}

/// Register `instance` in `proxy`'s one target group and wait for the registration to be in
/// force, which is when the proxy pools connections to it.
async fn registered_target(router: &MultiServiceRouter, case: &str, proxy: &str, instance: &str) {
    expect_ok(
        case,
        call(
            router,
            "RegisterDBProxyTargets",
            &[
                ("DBProxyName", proxy),
                ("DBInstanceIdentifiers.member.1", instance),
            ],
        )
        .await,
    );
    let target_params = [("DBProxyName", proxy.to_string())];
    poll_until(
        case,
        "the proxy's target",
        || call(router, "DescribeDBProxyTargets", &target_params),
        |status, body| {
            status == 200
                && texts(body, "State")
                    .iter()
                    .any(|state| state == TARGET_STATE_AVAILABLE)
        },
    )
    .await;
}

/// The PORT a published proxy endpoint spells, as a client reads it, with the assertion that
/// pins the host it carries.
///
/// The `DBProxy` shape carries no port member of its own, so the address a proxy publishes
/// carries one; the host half has to be the address the core bound the proxy's listener on —
/// this machine's loopback ([`LOOPBACK`]) — because a client connects to exactly the string
/// the control plane handed it.
fn dialled_port(case: &str, endpoint: &str) -> u16 {
    let (host, port) = endpoint.rsplit_once(':').unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected a published endpoint carrying the port a client dials, \
             got {endpoint:?}"
        )
    });
    assert_eq!(
        host, LOOPBACK,
        "case {case:?}: expected the published endpoint to be this machine's loopback \
         address, got {endpoint:?}"
    );
    let port: u16 = port.parse().unwrap_or_else(|_| {
        panic!("case {case:?}: expected a port in the published endpoint, got {endpoint:?}")
    });
    assert!(
        port != 0,
        "case {case:?}: expected the port the listener landed on, got {endpoint:?}"
    );
    port
}

/// The payload of the first packet a MySQL server sends on a connection, read without
/// logging in.
///
/// The login client of the sibling suite reads the GREETING; what this reads is the first
/// packet whatever it is, because a connection to a proxy with no database behind it is
/// answered an error packet in its place. The protocol's framing is that suite's one reading
/// of it ([`crate::db_instance_engine_endpoint::mysql_read`]).
async fn first_mysql_packet(case: &str, port: u16) -> Vec<u8> {
    // The emulator's own published address: the core binds its listeners on this machine
    // ([`LOOPBACK`]), and [`dialled_port`] has pinned the published host to it.
    let conn = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the published endpoint {LOOPBACK}:{port} must accept a connection, got {error}")
        });
    let mut conn: Client = Box::new(conn);
    let (_sequence, payload) = mysql_read(&mut conn).await;
    payload
}

/// One row of the engine-family table: the proxy created and whether this emulator speaks
/// the protocol its `EngineFamily` names.
struct FamilyCase {
    case: &'static str,
    proxy: &'static str,
    engine_family: &'static str,
    /// Whether the endpoint must be a listener of this emulator's own.
    served: bool,
}

/// Every value `EngineFamily` takes (`smithy_query(query="shape", shape="EngineFamily")`:
/// `MYSQL | POSTGRESQL | SQLSERVER`). The rows differ along exactly what the rule
/// generalises over: whether this emulator speaks the database protocol that family names.
const FAMILY_CASES: &[FamilyCase] = &[
    FamilyCase {
        case: "a MySQL proxy",
        proxy: "emu-proxy-ep-mysql",
        engine_family: "MYSQL",
        served: true,
    },
    FamilyCase {
        case: "a PostgreSQL proxy",
        proxy: "emu-proxy-ep-postgres",
        engine_family: "POSTGRESQL",
        served: true,
    },
    FamilyCase {
        case: "a SQL Server proxy, whose protocol this emulator does not speak",
        proxy: "emu-proxy-ep-sqlserver",
        engine_family: "SQLSERVER",
        served: false,
    },
];

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_proxy_publishes_an_address_this_emulator_answers_on_for_every_protocol_it_speaks() {
    let router = emulator();
    for row in FAMILY_CASES {
        let published = available_proxy(&router, row.proxy, row.engine_family, false).await;
        if !row.served {
            // No listener, so the endpoint is the DNS-form name this service has always
            // reported — and it carries no port, since nothing of this emulator answers at
            // one.
            let expected_suffix = format!(".{REGION}.rds.amazonaws.com");
            assert!(
                published.ends_with(&expected_suffix) && !published.contains(':'),
                "case {:?}: expected a DNS-form endpoint ending in {expected_suffix:?}, got \
                 {published:?}",
                row.case
            );
            continue;
        }
        let port = dialled_port(row.case, &published);
        // And the address really answers: a connection to exactly the address the control
        // plane published — this machine's loopback, on the port the listener landed on — is
        // accepted.
        let accepted = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await;
        assert!(
            accepted.is_ok(),
            "case {:?}: expected the published endpoint {published} to accept a connection, \
             got {:?}",
            row.case,
            accepted.err()
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_on_a_mysql_proxy_endpoint_is_served_by_the_database_its_group_holds() {
    let router = emulator();
    let (instance_host, instance_port) = available_instance(
        &router,
        "emu-proxy-my-target",
        "mysql",
        &[("EngineVersion", "8.0.39".to_string())],
    )
    .await;
    let published = available_proxy(&router, "emu-proxy-my-session", "MYSQL", false).await;
    registered_target(
        &router,
        "the MySQL proxy's target",
        "emu-proxy-my-session",
        "emu-proxy-my-target",
    )
    .await;
    let proxy_port = dialled_port("the MySQL proxy", &published);

    // What one session writes through the DATABASE's own endpoint is what a session through
    // the PROXY reads, which is what makes the proxy's endpoint an address onto that
    // database rather than a store of its own.
    let mut direct = mysql_login(
        &instance_host,
        instance_port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .expect("the instance's own endpoint admits its master user");
    direct
        .query("CREATE TABLE through_the_database (note VARCHAR(32))")
        .await
        .expect("the instance's endpoint runs the statement");
    direct
        .query("INSERT INTO through_the_database (note) VALUES ('written directly')")
        .await
        .expect("the instance's endpoint runs the statement");

    let mut pooled = mysql_login(
        LOOPBACK,
        proxy_port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .expect("the proxy's endpoint admits the database's master user");
    let read = pooled
        .query("SELECT note FROM through_the_database")
        .await
        .expect("the proxy's endpoint runs the statement");
    assert_eq!(
        read.only(),
        Some("written directly"),
        "case \"a row written through the database's own endpoint\": expected the proxy to \
         read it, got {read:?}"
    );

    // And the other direction: what a session through the proxy writes is in the database.
    pooled
        .query("INSERT INTO through_the_database (note) VALUES ('written through the proxy')")
        .await
        .expect("the proxy's endpoint runs the statement");
    let read_back = direct
        .query("SELECT note FROM through_the_database ORDER BY note")
        .await
        .expect("the instance's endpoint runs the statement");
    assert_eq!(
        read_back.rows.len(),
        2,
        "case \"a row written through the proxy\": expected the database to hold both rows, \
         got {read_back:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_on_a_postgres_proxy_endpoint_is_served_by_the_database_its_group_holds() {
    let router = emulator();
    let (instance_host, instance_port) =
        available_instance(&router, "emu-proxy-pg-target", "postgres", &[]).await;
    let published = available_proxy(&router, "emu-proxy-pg-session", "POSTGRESQL", true).await;
    registered_target(
        &router,
        "the PostgreSQL proxy's target",
        "emu-proxy-pg-session",
        "emu-proxy-pg-target",
    )
    .await;
    let proxy_port = dialled_port("the PostgreSQL proxy", &published);

    let mut direct = postgres_login(
        &instance_host,
        instance_port,
        Transport::Tls,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .expect("the instance's own endpoint admits its master user");
    direct
        .query("CREATE TABLE through_the_database (note text)")
        .await
        .expect("the instance's endpoint runs the statement");
    direct
        .query("INSERT INTO through_the_database (note) VALUES ('written directly')")
        .await
        .expect("the instance's endpoint runs the statement");

    let mut pooled = postgres_login(
        LOOPBACK,
        proxy_port,
        Transport::Tls,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .expect("the proxy's endpoint admits the database's master user");
    let read = pooled
        .query("SELECT note FROM through_the_database")
        .await
        .expect("the proxy's endpoint runs the statement");
    assert_eq!(
        read.only(),
        Some("written directly"),
        "case \"a row written through the database's own endpoint\": expected the proxy to \
         read it, got {read:?}"
    );
    // The session is served by the database behind the proxy, so the engine it reports is
    // that database's own.
    let served_by = pooled
        .query("SELECT current_database()")
        .await
        .expect("the proxy's endpoint runs the statement");
    assert_eq!(
        served_by.only(),
        Some(DATABASE),
        "case \"the database a session through the proxy lands in\": expected {DATABASE}, got \
         {served_by:?}"
    );
}

/// One row of the encryption table: how a session through a proxy is opened, what the proxy
/// and its database require of it, and what the server answers.
struct EncryptionCase {
    case: &'static str,
    proxy: &'static str,
    engine_family: &'static str,
    /// The database the proxy's target group holds.
    instance: &'static str,
    engine: &'static str,
    /// The engine version that database runs, which is what its own secure-transport
    /// default follows.
    version: &'static str,
    /// `CreateDBProxy$RequireTLS`.
    require_tls: bool,
    transport: Transport,
    /// `Some(code)` is the refusal the server must answer the login with.
    refused_with: Option<&'static str>,
}

/// The rows differ along exactly what the rule generalises over: the protocol the proxy
/// speaks, both states of `RequireTLS`, both transports a client opens a session with, and
/// a database whose own engine refuses a plaintext login beside one whose engine admits it
/// (`rds.force_ssl`, whose published default is 1 from RDS for PostgreSQL 15 and 0 before
/// it).
const ENCRYPTION_CASES: &[EncryptionCase] = &[
    EncryptionCase {
        case: "a MySQL proxy that requires TLS refuses a plaintext login",
        proxy: "emu-proxy-tls-my-on",
        engine_family: "MYSQL",
        instance: "emu-proxy-tls-my-target",
        engine: "mysql",
        version: "8.0.39",
        require_tls: true,
        transport: Transport::Plaintext,
        refused_with: Some("3159"),
    },
    EncryptionCase {
        case: "a MySQL proxy that requires TLS admits an encrypted session",
        proxy: "emu-proxy-tls-my-on-tls",
        engine_family: "MYSQL",
        instance: "emu-proxy-tls-my-target-tls",
        engine: "mysql",
        version: "8.0.39",
        require_tls: true,
        transport: Transport::Tls,
        refused_with: None,
    },
    EncryptionCase {
        case: "a MySQL proxy that does not require TLS admits a plaintext login its \
               database admits",
        proxy: "emu-proxy-tls-my-off",
        engine_family: "MYSQL",
        instance: "emu-proxy-tls-my-target-off",
        engine: "mysql",
        version: "8.0.39",
        require_tls: false,
        transport: Transport::Plaintext,
        refused_with: None,
    },
    EncryptionCase {
        case: "a PostgreSQL proxy that requires TLS refuses a plaintext login",
        proxy: "emu-proxy-tls-pg-on",
        engine_family: "POSTGRESQL",
        instance: "emu-proxy-tls-pg-target",
        engine: "postgres",
        version: "14.13",
        require_tls: true,
        transport: Transport::Plaintext,
        refused_with: Some("28000"),
    },
    EncryptionCase {
        case: "a PostgreSQL proxy that requires TLS admits an encrypted session",
        proxy: "emu-proxy-tls-pg-on-tls",
        engine_family: "POSTGRESQL",
        instance: "emu-proxy-tls-pg-target-tls",
        engine: "postgres",
        version: "14.13",
        require_tls: true,
        transport: Transport::Tls,
        refused_with: None,
    },
    EncryptionCase {
        case: "a PostgreSQL proxy that does not require TLS admits a plaintext login its \
               database admits",
        proxy: "emu-proxy-tls-pg-off",
        engine_family: "POSTGRESQL",
        instance: "emu-proxy-tls-pg-target-off",
        engine: "postgres",
        version: "14.13",
        require_tls: false,
        transport: Transport::Plaintext,
        refused_with: None,
    },
    EncryptionCase {
        case: "a PostgreSQL proxy that does not require TLS still does not admit a \
               plaintext login its database refuses",
        proxy: "emu-proxy-tls-pg-forced",
        engine_family: "POSTGRESQL",
        instance: "emu-proxy-tls-pg-target-forced",
        engine: "postgres",
        version: "17.2",
        require_tls: false,
        transport: Transport::Plaintext,
        refused_with: Some("28000"),
    },
];

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn whether_a_plaintext_session_through_a_proxy_is_admitted_follows_require_tls() {
    let router = emulator();
    for row in ENCRYPTION_CASES {
        available_instance(
            &router,
            row.instance,
            row.engine,
            &[("EngineVersion", row.version.to_string())],
        )
        .await;
        let published = available_proxy(&router, row.proxy, row.engine_family, row.require_tls)
            .await;
        registered_target(&router, row.case, row.proxy, row.instance).await;
        let port = dialled_port(row.case, &published);
        let outcome: Result<(), Refusal> = match row.engine {
            "postgres" => postgres_login(
                LOOPBACK,
                port,
                row.transport,
                MASTER_USER,
                MASTER_PASSWORD,
                Some(DATABASE),
            )
            .await
            .map(|_| ()),
            _ => mysql_login(
                LOOPBACK,
                port,
                row.transport,
                MASTER_USER,
                MASTER_PASSWORD,
                Some(DATABASE),
            )
            .await
            .map(|_| ()),
        };
        match (row.refused_with, &outcome) {
            (None, Ok(())) => {}
            (Some(code), Err(refusal)) => assert_eq!(
                refusal.code, code,
                "case {:?}: expected the refusal {code}, got {refusal:?}",
                row.case
            ),
            (expected, actual) => panic!(
                "case {:?}: expected {expected:?}, got {actual:?}",
                row.case
            ),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_proxy_endpoint_serves_a_session_only_while_it_has_a_database_and_a_proxy() {
    let router = emulator();
    let published = available_proxy(&router, "emu-proxy-life", "MYSQL", false).await;
    let port = dialled_port("the proxy's own endpoint", &published);

    // A proxy is created with NO target, so there is no database to pool connections to:
    // the server answers the protocol's own error packet (`0xff`) rather than a greeting
    // onto a database it does not have.
    let answered = first_mysql_packet("a proxy with no registered target", port).await;
    assert_eq!(
        answered.first().copied(),
        Some(0xff),
        "case \"a proxy with no registered target\": expected an error packet, got {answered:?}"
    );

    // With a database registered, the same address serves a session.
    available_instance(
        &router,
        "emu-proxy-life-target",
        "mysql",
        &[("EngineVersion", "8.0.39".to_string())],
    )
    .await;
    registered_target(
        &router,
        "the registered database",
        "emu-proxy-life",
        "emu-proxy-life-target",
    )
    .await;
    let session = mysql_login(
        LOOPBACK,
        port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await;
    assert!(
        session.is_ok(),
        "case \"a proxy whose group holds a database\": expected a session, got {:?}",
        session.err()
    );

    // The endpoint is the proxy's: a delete takes it away, and nothing answers there once
    // this service has taken the proxy itself away.
    expect_ok(
        "the delete",
        call(
            &router,
            "DeleteDBProxy",
            &[("DBProxyName", "emu-proxy-life")],
        )
        .await,
    );
    let gone_params = [("DBProxyName", "emu-proxy-life".to_string())];
    poll_until(
        "the deleted proxy",
        "the proxy",
        || call(&router, "DescribeDBProxies", &gone_params),
        |status, _body| status == 404,
    )
    .await;
    // The port closes with the record that held it; looked at again until it does or a
    // deadline far longer than the span this service owes the removal in has passed.
    let deadline = Instant::now() + WAIT_DEADLINE;
    loop {
        let accepted = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await;
        match accepted {
            Err(_closed) => break,
            Ok(open) => {
                drop(open);
                assert!(
                    Instant::now() < deadline,
                    "case \"a deleted proxy\": expected nothing to answer at {published} once \
                     the proxy is gone"
                );
                tokio::time::sleep(LOOK_INTERVAL).await;
            }
        }
    }
}
