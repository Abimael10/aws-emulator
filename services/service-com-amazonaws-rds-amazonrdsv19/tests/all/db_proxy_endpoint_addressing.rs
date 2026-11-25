// SPDX-License-Identifier: CC0-1.0

//! WHICH ADDRESSES a DB PROXY's endpoint answers at, beside the `host:port` it publishes.
//!
//! The rule under test: `DBProxy$Endpoint` is "the endpoint that you can use to connect to the
//! DB proxy. You include the endpoint value in the connection string for a database client
//! application" and `DBProxy` models NO PORT MEMBER at all, so a client completes the address
//! it was handed with the port of the engine family the proxy announced — "this value
//! determines which database network protocol the proxy recognizes" — rather than with a port
//! the member told it. A proxy endpoint of this emulator must therefore answer that family's
//! own port at the host it publishes, as well as the port the core assigned its listener.
//!
//! That address is on the LOOPBACK interface, so the client that reaches it is a client in this
//! process: a proxy endpoint is reachable only from inside the proxy's VPC, and the in-VPC
//! clients of this emulator are the ones it runs itself. Such a client opens its connection
//! through the register the owning service recorded the address in
//! (`service_common::dialable_data_planes`), which is what this suite drives; the whole path
//! from a VPC-attached function is driven end to end in
//! `service-com-amazonaws-lambda-awsgirapiservice`'s `handler_rds_proxy_native_session`.
//!
//! The rows span what the rule generalises over: both engine families whose wire this emulator
//! speaks, with their own ports and their own protocols, and both ways a client reads the
//! published value — the host completed with the family's own port, and the whole authority as
//! it stands. One port of one interface can name only ONE proxy, so the suite also pins which:
//! the proxy that published its endpoint last answers there, each proxy keeps answering at the
//! authority it published, and the family's port goes away with the proxy that held it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use service_com_amazonaws_rds_amazonrdsv19::state::{
    proxy_engine_port, DB_PROXY_STATUS_AVAILABLE, TARGET_STATE_AVAILABLE,
};
use service_common::dialable_data_planes::DialablePlanes;
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instance_engine_endpoint::{
    available_instance, mysql_login, mysql_login_on, postgres_login, postgres_login_on, Client,
    Transport, DATABASE, MASTER_PASSWORD, MASTER_USER,
};
use crate::db_instances::{call, emulator_with_engine, expect_ok, text, texts};
use crate::lifecycle_poll_loops::poll_until;

/// An IAM role ARN of the form `CreateDBProxy$RoleArn` takes.
const PROXY_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/rds-proxy";

/// A subnet id of the published form (`subnet-` plus eight hexadecimal characters).
const SUBNET_ID: &str = "subnet-0a1b2c3d";

/// How long a wait for a condition this suite drives is given before it calls it unobservable.
/// Generous next to the spans this service owes its transitions in, so a loaded test machine
/// cannot decide the outcome.
const WAIT_DEADLINE: Duration = Duration::from_secs(20);

/// The gap between two looks at a condition that is not a control-plane read.
const LOOK_INTERVAL: Duration = Duration::from_millis(25);

/// The statement every session runs: the row one write put in the TARGET's own database, so an
/// answer proves the session reached that database and not a store of its own.
const READ_THE_ROW: &str = "SELECT note FROM through_the_proxy";

/// One engine family's row: the family `CreateDBProxy` takes, the engine of the database behind
/// the proxies, the version that database runs, and the two proxies the row creates in front of
/// it.
struct FamilyRow {
    case: &'static str,
    engine_family: &'static str,
    engine: &'static str,
    version: &'static str,
    instance: &'static str,
    proxies: [&'static str; 2],
    /// How a session on this family's wire is opened, which is that wire's own handshake.
    transport: Transport,
}

/// The families this emulator speaks the protocol of, each with TWO proxies in front of one
/// database: two proxies of ONE family are what the family-port rule has to be stated for,
/// because both are dialled on that family's one port.
const FAMILY_ROWS: &[FamilyRow] = &[
    FamilyRow {
        case: "two MySQL proxies in front of one database",
        engine_family: "MYSQL",
        engine: "mysql",
        version: "8.0.39",
        instance: "emu-proxy-addr-my-target",
        proxies: ["emu-proxy-addr-my-one", "emu-proxy-addr-my-two"],
        transport: Transport::Plaintext,
    },
    FamilyRow {
        case: "two PostgreSQL proxies in front of one database",
        engine_family: "POSTGRESQL",
        engine: "postgres",
        version: "14.13",
        instance: "emu-proxy-addr-pg-target",
        proxies: ["emu-proxy-addr-pg-one", "emu-proxy-addr-pg-two"],
        transport: Transport::Tls,
    },
];

/// `CreateDBProxy` for a proxy of `engine_family`, answered with the `Endpoint` it published
/// once the proxy reports itself ready to handle requests.
async fn available_proxy(router: &MultiServiceRouter, name: &str, engine_family: &str) -> String {
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
            ],
        )
        .await,
    );
    let published = text(&created, "Endpoint");
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

/// The `host` and `port` of a published `host:port` authority, as a client reads them off the
/// string the control plane handed it.
fn authority_of(case: &str, published: &str) -> (String, u16) {
    let (host, port) = published.rsplit_once(':').unwrap_or_else(|| {
        panic!("case {case:?}: expected a published host:port authority, got {published:?}")
    });
    let port = port
        .parse()
        .unwrap_or_else(|_| panic!("case {case:?}: expected a numeric port in {published:?}"));
    (host.to_string(), port)
}

/// Open a connection to `host`:`port` the way an IN-PROCESS client of this emulator does: the
/// plane the owning service recorded under that address serves it, and `None` when this
/// emulator has opened no plane there at all.
async fn dial_in_process(
    engine: &Arc<engine::InMemoryEngine>,
    host: &str,
    port: u16,
) -> Option<Client> {
    let planes = DialablePlanes::of(&**engine)?;
    let dialed = planes.dialable(host, port)?;
    let conn = dialed.plane.dial(None).await.unwrap_or_else(|error| {
        panic!("the plane at {host}:{port} must accept a connection: {error}")
    });
    Some(Box::new(conn))
}

/// [`dial_in_process`] with the panic a row that expects an answer wants.
async fn must_dial(
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    host: &str,
    port: u16,
) -> Client {
    dial_in_process(engine, host, port)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: this emulator must answer at {host}:{port}"))
}

/// The two statements that put [`READ_THE_ROW`]'s row in the TARGET database, run through that
/// database's OWN endpoint on its own wire — so what a session through a proxy reads is a row
/// no proxy wrote.
async fn seed_the_row(row: &FamilyRow, host: &str, port: u16) {
    const DDL: &str = "CREATE TABLE through_the_proxy (note VARCHAR(32))";
    const WRITE: &str = "INSERT INTO through_the_proxy (note) VALUES ('pooled')";
    match row.engine {
        "postgres" => {
            let mut session = postgres_login(
                host,
                port,
                row.transport,
                MASTER_USER,
                MASTER_PASSWORD,
                Some(DATABASE),
            )
            .await
            .expect("the instance's own endpoint admits its master user");
            for statement in [DDL, WRITE] {
                session
                    .query(statement)
                    .await
                    .expect("the instance's endpoint runs the statement");
            }
        }
        _ => {
            let mut session = mysql_login(
                host,
                port,
                row.transport,
                MASTER_USER,
                MASTER_PASSWORD,
                Some(DATABASE),
            )
            .await
            .expect("the instance's own endpoint admits its master user");
            for statement in [DDL, WRITE] {
                session
                    .query(statement)
                    .await
                    .expect("the instance's endpoint runs the statement");
            }
        }
    }
}

/// Run [`READ_THE_ROW`] on a session opened over `conn`, and answer the one value it read.
async fn row_through(conn: Client, row: &FamilyRow, case: &str) -> Option<String> {
    match row.engine {
        "postgres" => {
            let mut session = postgres_login_on(
                conn,
                row.transport,
                MASTER_USER,
                MASTER_PASSWORD,
                Some(DATABASE),
            )
            .await
            .unwrap_or_else(|refusal| {
                panic!(
                    "case {case:?}: the endpoint must admit the database's master user, got \
                     {refusal:?}"
                )
            });
            let answered = session.query(READ_THE_ROW).await.unwrap_or_else(|refusal| {
                panic!("case {case:?}: the session must run the statement, got {refusal:?}")
            });
            answered.only().map(str::to_string)
        }
        _ => {
            let mut session = mysql_login_on(
                conn,
                row.transport,
                MASTER_USER,
                MASTER_PASSWORD,
                Some(DATABASE),
            )
            .await
            .unwrap_or_else(|refusal| {
                panic!(
                    "case {case:?}: the endpoint must admit the database's master user, got \
                     {refusal:?}"
                )
            });
            let answered = session.query(READ_THE_ROW).await.unwrap_or_else(|refusal| {
                panic!("case {case:?}: the session must run the statement, got {refusal:?}")
            });
            answered.only().map(str::to_string)
        }
    }
}

/// RULE: a proxy's endpoint answers the port its ENGINE FAMILY is dialled on at the host it
/// publishes — the port a client completes a connection string with, because the published
/// member carries none — as well as the port that member spells; a session on either is served
/// by the database the proxy's target group holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_proxy_endpoint_answers_the_port_its_engine_family_is_dialled_on() {
    let (router, engine) = emulator_with_engine();
    for row in FAMILY_ROWS {
        let (instance_host, instance_port) = available_instance(
            &router,
            row.instance,
            row.engine,
            &[("EngineVersion", row.version.to_string())],
        )
        .await;
        // The row every session below reads, written through the DATABASE's own endpoint: an
        // answer through a proxy therefore proves the session reached that database.
        seed_the_row(row, &instance_host, instance_port).await;

        // The port this family is dialled on, which is the one the databases behind the proxy
        // publish for themselves — not a value of this suite's own.
        let family_port = proxy_engine_port(row.engine_family).unwrap_or_else(|| {
            panic!(
                "case {:?}: this emulator speaks {}'s wire, so that family has a port",
                row.case, row.engine_family
            )
        });
        assert_ne!(
            family_port, instance_port,
            "case {:?}: the family's own port is not the port the core assigned a listener",
            row.case
        );

        let mut published = Vec::new();
        for proxy in row.proxies {
            let endpoint = available_proxy(&router, proxy, row.engine_family).await;
            registered_target(&router, row.case, proxy, row.instance).await;
            let (host, port) = authority_of(row.case, &endpoint);
            assert_ne!(
                port, family_port,
                "case {:?}: the published authority spells the port the core assigned the \
                 proxy's listener, got {endpoint:?}",
                row.case
            );
            published.push((proxy, host, port));
        }

        // Each proxy answers at the authority it published: that address names THAT proxy, so
        // every one of them keeps answering however many proxies the family has.
        for (proxy, host, port) in &published {
            let case = format!("{} ({proxy} at the authority it published)", row.case);
            let conn = must_dial(&engine, &case, host, *port).await;
            let answered = row_through(conn, row, &case).await;
            assert_eq!(
                answered.as_deref(),
                Some("pooled"),
                "case {case:?}: expected the row the target database holds, got {answered:?}"
            );
        }

        // And the published HOST answers the family's own port: the address a driver dials when
        // the member it was handed carries no port. One port of one interface names no proxy in
        // particular, so it is the proxy that published its endpoint LAST that answers there —
        // which the diagnosis below asserts by reading the session's own server port.
        let (last, host, last_port) = published
            .last()
            .cloned()
            .unwrap_or_else(|| panic!("case {:?}: the row creates two proxies", row.case));
        let case = format!("{} ({last} at its family's own port)", row.case);
        let conn = must_dial(&engine, &case, &host, family_port).await;
        let answered = row_through(conn, row, &case).await;
        assert_eq!(
            answered.as_deref(),
            Some("pooled"),
            "case {case:?}: expected the row the target database holds, got {answered:?}"
        );
        let served_by = DialablePlanes::of(&*engine)
            .and_then(|planes| planes.dialable(&host, family_port))
            .map(|dialed| dialed.authority);
        assert_eq!(
            served_by.as_deref(),
            Some(
                ::service_common::data_plane_addressing::host_port_authority(&host, family_port)
                    .as_str()
            ),
            "case {case:?}: expected the family's port at the published host to be answered as \
             its own address, got {served_by:?}"
        );
        assert_ne!(
            last_port, family_port,
            "case {case:?}: the two addresses one endpoint answers at are different addresses"
        );

        // The addresses are the ENDPOINT: deleting the proxy withdraws the ones it holds, so
        // nothing of this emulator answers there any more.
        expect_ok(
            row.case,
            call(&router, "DeleteDBProxy", &[("DBProxyName", last)]).await,
        );
        let gone_params = [("DBProxyName", last.to_string())];
        poll_until(
            row.case,
            "the deleted proxy",
            || call(&router, "DescribeDBProxies", &gone_params),
            |status, _body| status == 404,
        )
        .await;
        let deadline = Instant::now() + WAIT_DEADLINE;
        loop {
            if dial_in_process(&engine, &host, last_port).await.is_none() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {:?}: expected nothing to answer at the address {last:?} published once \
                 it is gone",
                row.case
            );
            tokio::time::sleep(LOOK_INTERVAL).await;
        }

        // The proxy BESIDE it is untouched: the authority it published still reaches it.
        let (other, host, port) = published
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("case {:?}: the row creates two proxies", row.case));
        let case = format!("{} ({other} after the other is deleted)", row.case);
        let conn = must_dial(&engine, &case, &host, port).await;
        let answered = row_through(conn, row, &case).await;
        assert_eq!(
            answered.as_deref(),
            Some("pooled"),
            "case {case:?}: expected the row the target database holds, got {answered:?}"
        );

        // The family's port, though, was the DELETED proxy's: it is one port of the one
        // interface the core binds on, it named that proxy, and it went away with it — the
        // surviving proxy does not take it over, because a client holding an endpoint reaches
        // the proxy that published it and nothing hands one proxy's address to another. An
        // address per proxy, which would let both answer that port at once, needs a core
        // listener that takes an address or a port (see the module docs of
        // `state::data_plane::proxy_endpoint`).
        let dialled = dial_in_process(&engine, &host, family_port).await;
        assert!(
            dialled.is_none(),
            "case {case:?}: expected the family's port to go with the proxy that published it"
        );
    }
}
