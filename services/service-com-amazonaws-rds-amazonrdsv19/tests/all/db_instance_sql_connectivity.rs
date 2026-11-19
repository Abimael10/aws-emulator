// SPDX-License-Identifier: CC0-1.0

//! End-to-end test for the baseline provisioning path a client follows: `CreateDBInstance`,
//! poll to `available`, connect to the published `Endpoint` with the engine's own driver and
//! round-trip a row — for a PostgreSQL and a MySQL-family instance standing side by side in
//! ONE account and region.
//!
//! Every control-plane call goes over the wire through `db_instances::emulator` as a real
//! awsQuery form POST, and every session is opened over a real TCP connection to the address
//! and port the control plane published, speaking the engine's own protocol
//! (`db_instance_engine_endpoint`'s clients). So nothing here is an in-process shortcut: what
//! is exercised is the flow a provisioning script drives.
//!
//! The rule under test: for every engine whose native wire this emulator speaks, an instance
//! a create accepted reaches `DBInstanceStatus` `available` in bounded time, and the
//! `Endpoint` it publishes then accepts a session for that engine in which a row written is a
//! row read back. Several instances of DIFFERENT engines in one region each reach that state
//! and each serve their own data — the endpoint of one is not the endpoint of another, and a
//! row written through one is not visible through the other.
//!
//! The rows span what the rule generalises over: the two native wires (PostgreSQL, and MySQL
//! for both the MySQL and MariaDB engines), a session in the clear and one upgraded in band
//! to TLS, and an instance created WITH a `DBName` as well as one created without, which
//! connects to the engine's own default database.

use std::collections::BTreeSet;

use crate::db_instance_engine_endpoint::{
    available_instance, mysql_login, postgres_login, Rows, Transport, DATABASE, MASTER_PASSWORD,
    MASTER_USER,
};
use crate::db_instances::emulator;

/// Which wire a row's engine speaks, and so which client opens its session.
#[derive(Debug, Clone, Copy)]
enum Wire {
    Postgres,
    MySql,
}

/// One row of the connectivity table.
struct Case {
    case: &'static str,
    identifier: &'static str,
    engine: &'static str,
    wire: Wire,
    /// Whether the create names a `DBName`; a row that does not connects to the engine's own
    /// default database.
    with_database: bool,
    transport: Transport,
}

#[tokio::test]
async fn every_published_endpoint_serves_its_own_instance_after_the_create_settles() {
    let table: &[Case] = &[
        Case {
            case: "a postgres instance, TLS, with a database of its own",
            identifier: "emu-sqlc-postgres",
            engine: "postgres",
            wire: Wire::Postgres,
            with_database: true,
            transport: Transport::Tls,
        },
        Case {
            case: "a mysql instance, in the clear, on the engine's default database",
            identifier: "emu-sqlc-mysql",
            engine: "mysql",
            wire: Wire::MySql,
            with_database: false,
            transport: Transport::Plaintext,
        },
        Case {
            case: "a mariadb instance, TLS, with a database of its own",
            identifier: "emu-sqlc-mariadb",
            engine: "mariadb",
            wire: Wire::MySql,
            with_database: true,
            transport: Transport::Tls,
        },
    ];

    // ONE emulator for the whole table: the instances stand side by side in one account and
    // region, which is what the flow under test provisions.
    let router = emulator();
    let mut endpoints: Vec<(String, u16)> = Vec::new();
    for row in table {
        let case = row.case;
        let extra: Vec<(&str, String)> = match row.with_database {
            true => vec![("DBName", DATABASE.to_string())],
            false => Vec::new(),
        };
        // `available_instance` polls `DescribeDBInstances` until the instance reports
        // `available` — the condition a client waits for — and asserts that the endpoint the
        // create published is the endpoint the describe reports.
        let (address, port) = available_instance(&router, row.identifier, row.engine, &extra).await;
        endpoints.push((address.clone(), port));

        // The round trip: a table created, a row written, the row read back — through the
        // published endpoint, with the engine's own driver.
        let database = row.with_database.then_some(DATABASE);
        let read = match row.wire {
            Wire::Postgres => {
                let mut session = postgres_login(
                    &address,
                    port,
                    row.transport,
                    MASTER_USER,
                    MASTER_PASSWORD,
                    database,
                )
                .await
                .unwrap_or_else(|refusal| {
                    panic!(
                        "case {case:?}: the published endpoint must admit the master user, got \
                         {refusal:?}"
                    )
                });
                for statement in [
                    "CREATE TABLE connectivity (id integer, note text)",
                    "INSERT INTO connectivity (id, note) VALUES (1, 'round-trip')",
                ] {
                    session.query(statement).await.unwrap_or_else(|refusal| {
                        panic!("case {case:?}: {statement:?} must run, got {refusal:?}")
                    });
                }
                session
                    .query("SELECT note FROM connectivity WHERE id = 1")
                    .await
                    .unwrap_or_else(|refusal| {
                        panic!("case {case:?}: the read must run, got {refusal:?}")
                    })
            }
            Wire::MySql => {
                let mut session = mysql_login(
                    &address,
                    port,
                    row.transport,
                    MASTER_USER,
                    MASTER_PASSWORD,
                    database,
                )
                .await
                .unwrap_or_else(|refusal| {
                    panic!(
                        "case {case:?}: the published endpoint must admit the master user, got \
                         {refusal:?}"
                    )
                });
                for statement in [
                    "CREATE TABLE connectivity (id int, note varchar(32))",
                    "INSERT INTO connectivity (id, note) VALUES (1, 'round-trip')",
                ] {
                    session.query(statement).await.unwrap_or_else(|refusal| {
                        panic!("case {case:?}: {statement:?} must run, got {refusal:?}")
                    });
                }
                session
                    .query("SELECT note FROM connectivity WHERE id = 1")
                    .await
                    .unwrap_or_else(|refusal| {
                        panic!("case {case:?}: the read must run, got {refusal:?}")
                    })
            }
        };
        assert_eq!(
            read.only(),
            Some("round-trip"),
            "case {case:?}: the row written through the endpoint must read back, got {read:?}"
        );
    }

    // Each instance was served by its OWN engine: no two of them published the same address
    // and port, and the table one of them holds is a table the others do not have.
    let distinct: BTreeSet<&(String, u16)> = endpoints.iter().collect();
    assert_eq!(
        distinct.len(),
        endpoints.len(),
        "the instances must each publish an endpoint of their own, got {endpoints:?}"
    );
    let (address, port) = &endpoints[1];
    let mut session = mysql_login(
        address,
        *port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        None,
    )
    .await
    .expect("the mysql endpoint admits the master user");
    let read: Rows = session
        .query("SELECT count(*) FROM connectivity")
        .await
        .expect("the mysql instance's own table is readable");
    assert_eq!(
        read.only(),
        Some("1"),
        "the mysql instance must hold exactly the one row written through its own endpoint, \
         got {read:?}"
    );
}
