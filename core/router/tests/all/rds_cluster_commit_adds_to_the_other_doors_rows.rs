// SPDX-License-Identifier: CC0-1.0

//! What a transaction's COMMIT does to a row ANOTHER writer committed while it was open.
//!
//! RULE: a transaction's commit carries over the rows it changed, and a row it CREATED is
//! ADDED to the database — the row id of a new row is the engine's to assign, so a row another
//! writer committed at that same row id since the transaction's snapshot is a row of its own
//! and not one the commit may take the place of. Only a row that was already there when the
//! transaction began is replaced in place, which is what changing it means; a row the
//! transaction created and then withdrew leaves the database alone; and where the row id is
//! the CLIENT's own value (a declared `INTEGER PRIMARY KEY`), two writers' rows of the same key
//! meet the engine's unique violation rather than one of them being dropped. So nothing a
//! writer committed is lost because another transaction happened to be open over it.
//!
//! This is the rule the sibling suite [`crate::rds_cluster_both_doors_at_once`] drives both
//! doors of a cluster into by RACING them: whether the other door's write landed before or
//! after the transaction's snapshot was whatever the host's interleaving made it, so the row it
//! lost was lost only on some runs. Here the same meeting is driven one statement at a time,
//! in the order that used to lose the row, so the outcome does not depend on how fast the host
//! runs.
//!
//! The rows span what the rule generalises over: WHICH writer committed meanwhile (the
//! cluster's Data API HTTP endpoint and a second session on its engine endpoint, each on a
//! cluster of its own), WHAT the transaction did to the table (created one row, created
//! several, created and withdrew one, updated a row that was there, deleted one, and contested
//! one the other writer changed as well), and WHOSE value the row id is (the engine's own and a
//! declared `INTEGER PRIMARY KEY`, with distinct keys and with the same key from both writers).
//! Every row is read back through BOTH doors, because the cluster is one database behind them.
//!
//! Its harness is the sibling suite's: the same cluster create (with the Data API HTTP endpoint
//! on and a Secrets Manager credential), the same `ExecuteStatement` client and the same
//! PostgreSQL session, reused rather than copied. Every control-plane call goes over the wire on
//! a router with request authentication off, as every suite in this crate runs it, and the whole
//! router is assembled on a sized thread (see `boot_serve.rs`).

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use smithy_server_runtime::MultiServiceRouter;

use super::rds_cluster_one_database_behind_both_doors::{
    api_column, execute_statement, provisioned, Provisioned, Session, BOOT_STACK_SIZE, DATABASE,
};

/// One meeting of a transaction's commit with a row another writer committed while it was open.
struct Meanwhile {
    /// What this row is a case of.
    case: &'static str,
    /// The table the row's statements name, which no other row names.
    declaration: &'static str,
    /// What is in the table before the transaction begins, committed by the other writer.
    seeded: &'static [&'static str],
    /// What the transaction runs between its `BEGIN` and its `COMMIT`.
    inside: &'static [&'static str],
    /// What the OTHER writer commits while that transaction is open.
    meanwhile: &'static str,
    /// The SQLSTATE the `COMMIT` is refused with, or `None` for a commit that is served.
    refused_with: Option<&'static str>,
    /// What the table holds afterwards, as the row's own read reports it.
    read: &'static str,
    /// The one column that read selects, in the order it selects them.
    holders: &'static [&'static str],
}

/// The meetings: each thing a transaction can have done to the table, against a row the other
/// writer committed under it.
const MEANWHILE: &[Meanwhile] = &[
    Meanwhile {
        case: "a row the transaction created beside one the other writer committed",
        declaration: "CREATE TABLE added_rows (holder VARCHAR(40) NOT NULL)",
        seeded: &["INSERT INTO added_rows (holder) VALUES ('already there')"],
        inside: &["INSERT INTO added_rows (holder) VALUES ('the transaction')"],
        meanwhile: "INSERT INTO added_rows (holder) VALUES ('the other writer')",
        refused_with: None,
        read: "SELECT holder FROM added_rows ORDER BY holder",
        holders: &["already there", "the other writer", "the transaction"],
    },
    Meanwhile {
        case: "several rows the transaction created beside one the other writer committed",
        declaration: "CREATE TABLE several_rows (holder VARCHAR(40) NOT NULL)",
        seeded: &[],
        inside: &[
            "INSERT INTO several_rows (holder) VALUES ('the first')",
            "INSERT INTO several_rows (holder) VALUES ('the second')",
            "INSERT INTO several_rows (holder) VALUES ('the third')",
        ],
        meanwhile: "INSERT INTO several_rows (holder) VALUES ('the other writer')",
        refused_with: None,
        read: "SELECT holder FROM several_rows ORDER BY holder",
        holders: &["the first", "the other writer", "the second", "the third"],
    },
    Meanwhile {
        case: "a row the transaction created and then withdrew",
        declaration: "CREATE TABLE withdrawn_rows (holder VARCHAR(40) NOT NULL)",
        seeded: &[],
        inside: &[
            "INSERT INTO withdrawn_rows (holder) VALUES ('withdrawn')",
            "DELETE FROM withdrawn_rows WHERE holder = 'withdrawn'",
        ],
        meanwhile: "INSERT INTO withdrawn_rows (holder) VALUES ('the other writer')",
        refused_with: None,
        read: "SELECT holder FROM withdrawn_rows ORDER BY holder",
        holders: &["the other writer"],
    },
    Meanwhile {
        case: "a row that was already there, updated by the transaction",
        declaration: "CREATE TABLE updated_rows (holder VARCHAR(40) NOT NULL)",
        seeded: &["INSERT INTO updated_rows (holder) VALUES ('to be updated')"],
        inside: &["UPDATE updated_rows SET holder = 'the transaction' \
                   WHERE holder = 'to be updated'"],
        meanwhile: "INSERT INTO updated_rows (holder) VALUES ('the other writer')",
        refused_with: None,
        read: "SELECT holder FROM updated_rows ORDER BY holder",
        holders: &["the other writer", "the transaction"],
    },
    Meanwhile {
        case: "a row that was already there, deleted by the transaction",
        declaration: "CREATE TABLE deleted_rows (holder VARCHAR(40) NOT NULL)",
        seeded: &["INSERT INTO deleted_rows (holder) VALUES ('to be deleted')"],
        inside: &["DELETE FROM deleted_rows WHERE holder = 'to be deleted'"],
        meanwhile: "INSERT INTO deleted_rows (holder) VALUES ('the other writer')",
        refused_with: None,
        read: "SELECT holder FROM deleted_rows ORDER BY holder",
        holders: &["the other writer"],
    },
    Meanwhile {
        // The other writer's own statement is an UPDATE rather than an insert here, of a
        // NEIGHBOURING row: the row the transaction is holding is one a second session waits
        // for, which is the row-lock rule the sibling suite
        // [`crate::rds_cluster_one_database_behind_both_doors`] pins.
        case: "a row each of them changed, of the rows that were already there",
        declaration: "CREATE TABLE neighbouring_rows (holder VARCHAR(40) NOT NULL)",
        seeded: &[
            "INSERT INTO neighbouring_rows (holder) VALUES ('the transactions own')",
            "INSERT INTO neighbouring_rows (holder) VALUES ('the other writers own')",
        ],
        inside: &["UPDATE neighbouring_rows SET holder = 'the transaction' \
                   WHERE holder = 'the transactions own'"],
        meanwhile: "UPDATE neighbouring_rows SET holder = 'the other writer' \
                    WHERE holder = 'the other writers own'",
        refused_with: None,
        read: "SELECT holder FROM neighbouring_rows ORDER BY holder",
        holders: &["the other writer", "the transaction"],
    },
    Meanwhile {
        case: "rows of distinct declared keys from each of them",
        declaration: "CREATE TABLE keyed_rows (id INTEGER PRIMARY KEY, holder VARCHAR(40) NOT NULL)",
        seeded: &[],
        inside: &["INSERT INTO keyed_rows (id, holder) VALUES (1, 'the transaction')"],
        meanwhile: "INSERT INTO keyed_rows (id, holder) VALUES (2, 'the other writer')",
        refused_with: None,
        read: "SELECT id || ' ' || holder FROM keyed_rows ORDER BY id",
        holders: &["1 the transaction", "2 the other writer"],
    },
    Meanwhile {
        // The one case the commit cannot carry: the row id is the key the CLIENT chose, so
        // the engine's own unique violation is the answer, and the row the other writer
        // committed stands.
        case: "the same declared key from both of them",
        declaration:
            "CREATE TABLE clashing_rows (id INTEGER PRIMARY KEY, holder VARCHAR(40) NOT NULL)",
        seeded: &[],
        inside: &["INSERT INTO clashing_rows (id, holder) VALUES (1, 'the transaction')"],
        meanwhile: "INSERT INTO clashing_rows (id, holder) VALUES (1, 'the other writer')",
        // PostgreSQL's `unique_violation`, which is the class a client branches on.
        refused_with: Some("23505"),
        read: "SELECT id || ' ' || holder FROM clashing_rows ORDER BY id",
        holders: &["1 the other writer"],
    },
];

/// One writer that is NOT the session holding the transaction.
struct Door {
    /// What this door is called in a case name.
    name: &'static str,
    /// Whether it is a second SESSION on the cluster's engine endpoint; else it is the
    /// cluster's Data API HTTP endpoint.
    a_second_session: bool,
    /// The cluster it writes, which is its own so every row's table name serves both doors.
    cluster: &'static str,
    /// The secret carrying that cluster's master credential.
    secret: &'static str,
}

/// Each writer the rule has to hold for: the cluster's other door, and another session on the
/// same one.
const DOORS: &[Door] = &[
    Door {
        name: "the Data API",
        a_second_session: false,
        cluster: "meanwhile-api-cluster",
        secret: "meanwhile-api-secret",
    },
    Door {
        name: "a second session",
        a_second_session: true,
        cluster: "meanwhile-session-cluster",
        secret: "meanwhile-session-secret",
    },
];

/// Run `sql` through the writer that is not the one holding the transaction, and assert it was
/// served: `other` is a second session on the cluster's engine endpoint, and `None` is the
/// cluster's Data API HTTP endpoint.
async fn through_the_other_writer(
    router: &MultiServiceRouter,
    cluster: &Provisioned,
    other: Option<&mut Session>,
    case: &str,
    sql: &str,
) {
    match other {
        Some(session) => {
            session.served(case, sql).await;
        }
        None => {
            execute_statement(
                router,
                &cluster.cluster_arn,
                &cluster.secret_arn,
                Some(DATABASE),
                None,
                sql,
            )
            .await
            .served(case);
        }
    }
}

/// RULE: a commit ADDS the rows the transaction created to what another writer committed while
/// it was open, and replaces only the rows that were there when it began.
#[test]
fn a_commit_adds_its_rows_to_what_another_writer_committed_meanwhile() {
    std::thread::Builder::new()
        .name("rds-cluster-commit-meanwhile".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            // `enable_all`, which is what the emulator's own boot path builds: a cluster
            // create opens the cluster's engine listener through the core, and binding a
            // socket needs the runtime's I/O driver.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a current-thread runtime");
            runtime.block_on(async {
                for door in DOORS {
                    let cluster = provisioned(&router, door.cluster, door.secret).await;
                    let mut session =
                        Session::open(door.name, &cluster.address, cluster.port, DATABASE).await;
                    let mut other = if door.a_second_session {
                        Some(
                            Session::open(door.name, &cluster.address, cluster.port, DATABASE)
                                .await,
                        )
                    } else {
                        None
                    };

                    for row in MEANWHILE {
                        let case = format!("{} meeting {}", row.case, door.name);
                        let case = case.as_str();
                        for statement in
                            std::iter::once(row.declaration).chain(row.seeded.iter().copied())
                        {
                            through_the_other_writer(
                                &router,
                                &cluster,
                                other.as_mut(),
                                case,
                                statement,
                            )
                            .await;
                        }

                        // The transaction: its own statements run against the database as it
                        // stood when it began.
                        session.served(case, "BEGIN").await;
                        for statement in row.inside {
                            session.served(case, statement).await;
                        }
                        // And the other writer commits a row of its own while it is open.
                        through_the_other_writer(
                            &router,
                            &cluster,
                            other.as_mut(),
                            case,
                            row.meanwhile,
                        )
                        .await;

                        let committed = session.query(case, "COMMIT").await;
                        match row.refused_with {
                            None => assert!(
                                committed.is_ok(),
                                "case {case:?}: the commit must be served, got {committed:?}"
                            ),
                            Some(sqlstate) => {
                                let refusal = committed.expect_err(&format!(
                                    "case {case:?}: the commit must be refused, got a commit"
                                ));
                                assert_eq!(
                                    refusal.code, sqlstate,
                                    "case {case:?}: expected SQLSTATE {sqlstate}, got {refusal:?}"
                                );
                            }
                        }

                        // What the cluster's database holds afterwards, read back through the
                        // session that ran the transaction and through the Data API: one
                        // database behind both doors.
                        let expected: Vec<Vec<Option<String>>> = row
                            .holders
                            .iter()
                            .map(|holder| vec![Some((*holder).to_string())])
                            .collect();
                        let through_the_session = session.served(case, row.read).await;
                        assert_eq!(
                            through_the_session, expected,
                            "case {case:?}: the engine endpoint must read every row either of \
                             them committed, got {through_the_session:?}"
                        );
                        let through_the_api = execute_statement(
                            &router,
                            &cluster.cluster_arn,
                            &cluster.secret_arn,
                            Some(DATABASE),
                            None,
                            row.read,
                        )
                        .await
                        .served(case);
                        assert_eq!(
                            api_column(case, &through_the_api),
                            row.holders
                                .iter()
                                .map(|holder| (*holder).to_string())
                                .collect::<Vec<String>>(),
                            "case {case:?}: the Data API must read every row either of them \
                             committed, got {through_the_api}"
                        );
                    }
                }
            });
        })
        .expect("the suite's thread starts")
        .join()
        .expect("the suite's thread finishes");
}
