// SPDX-License-Identifier: CC0-1.0

//! BOTH doors of a DB CLUSTER serve statements AT THE SAME TIME.
//!
//! RULE: a DB cluster is one database behind several doors, and the engine behind them
//! SERIALISES the statements that arrive together: a statement issued while another door is
//! writing the cluster's database waits for that storage and is then served, so neither door
//! refuses a statement because the other door was using the database, and every write both
//! doors made is in the database afterwards. A database server admits concurrent connections
//! and orders their work; it does not answer the second one "the database is locked".
//!
//! The sibling suite [`crate::rds_cluster_one_database_behind_both_doors`] pins what one door
//! reads back of what the OTHER door committed, one statement at a time. This one pins what
//! happens when the two doors are in flight TOGETHER — which is the case a client reaches as
//! soon as two of its own processes are, and the case whose outcome used to depend on how the
//! host happened to interleave them.
//!
//! The rows span what the rule generalises over: WHICH door writes, whether the other door
//! reads the very table being written or writes it as well, whether it writes a different
//! table of the same database, and whether its work is one statement or a transaction of
//! several. Each row drives both doors over the same rounds and asserts that every statement
//! of both was served, and then that the rows both doors wrote are all there — read back
//! through each door in turn. The second case takes the same rule to the window a client can
//! actually see: one door in the MIDDLE of a write that inserts as many rows as its table
//! already holds, with the other door asking again and again for as long as it lasts.
//!
//! Its harness is the sibling suite's: the same cluster create (with the Data API HTTP
//! endpoint on and a Secrets Manager credential), the same `ExecuteStatement` client and the
//! same PostgreSQL session, reused rather than copied. Every control-plane call goes over the
//! wire on a router with request authentication off, as every suite in this crate runs it, and
//! the whole router is assembled on a sized thread (see `boot_serve.rs`).

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};

use super::rds_cluster_one_database_behind_both_doors::{
    api_column, execute_statement, provisioned, Session, BOOT_STACK_SIZE, DATABASE,
};

/// How many rounds one row drives: enough that the two doors' statements overlap, few enough
/// that a row costs a fraction of a second of engine work.
const ROUNDS: usize = 20;

/// One pairing of the two doors: what each of them runs in every round of it.
struct Together {
    /// What this row is a case of.
    case: &'static str,
    /// The table the row's statements name, which no other row names.
    table: &'static str,
    /// What the DATA API runs each round.
    through_the_api: &'static str,
    /// What the NATIVE session runs each round.
    through_the_session: &'static str,
    /// How many rows of [`Self::table`] the round's two statements add, which is what the
    /// table holds at the end of the row.
    rows_per_round: usize,
}

/// The pairings: each door as the writer, each kind of work the other door brings.
const TOGETHER: &[Together] = &[
    Together {
        case: "the Data API writes while a session reads the same table",
        table: "api_writes_rows",
        through_the_api: "INSERT INTO api_writes_rows (holder) VALUES ('the Data API')",
        through_the_session: "SELECT count(*) FROM api_writes_rows",
        rows_per_round: 1,
    },
    Together {
        case: "a session writes while the Data API reads the same table",
        table: "session_writes_rows",
        through_the_api: "SELECT count(*) FROM session_writes_rows",
        through_the_session: "INSERT INTO session_writes_rows (holder) VALUES ('the session')",
        rows_per_round: 1,
    },
    Together {
        case: "both doors write the same table",
        table: "both_write_rows",
        through_the_api: "INSERT INTO both_write_rows (holder) VALUES ('the Data API')",
        through_the_session: "INSERT INTO both_write_rows (holder) VALUES ('the session')",
        rows_per_round: 2,
    },
    Together {
        case: "a session runs a transaction of several statements while the Data API writes",
        table: "transacted_rows",
        through_the_api: "INSERT INTO transacted_rows (holder) VALUES ('the Data API')",
        through_the_session: "BEGIN; \
                              INSERT INTO transacted_rows (holder) VALUES ('the session'); \
                              UPDATE transacted_rows SET holder = 'committed' \
                                WHERE holder = 'the session'; \
                              COMMIT",
        rows_per_round: 2,
    },
];

/// RULE: every statement of both doors is served while the other door is using the cluster's
/// database, and every row either of them wrote is in that database afterwards.
#[test]
fn neither_door_of_a_cluster_is_refused_while_the_other_is_using_the_database() {
    std::thread::Builder::new()
        .name("rds-cluster-both-doors".to_string())
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
                let cluster = provisioned(&router, "both-doors-cluster", "both-doors-secret").await;
                let mut session =
                    Session::open("both doors at once", &cluster.address, cluster.port, DATABASE)
                        .await;

                for row in TOGETHER {
                    let case = row.case;
                    let table = row.table;
                    // The table both doors of the row work on, created through one of them —
                    // which makes it a table of the cluster either way.
                    execute_statement(
                        &router,
                        &cluster.cluster_arn,
                        &cluster.secret_arn,
                        Some(DATABASE),
                        None,
                        &format!("CREATE TABLE {table} (holder VARCHAR(40) NOT NULL)"),
                    )
                    .await
                    .served(case);

                    // Every round issues BOTH doors' statements and waits for both: they are
                    // in flight together, so each one meets the other using the database.
                    for round in 0..ROUNDS {
                        let (through_the_api, through_the_session) = tokio::join!(
                            execute_statement(
                                &router,
                                &cluster.cluster_arn,
                                &cluster.secret_arn,
                                Some(DATABASE),
                                None,
                                row.through_the_api,
                            ),
                            session.query(case, row.through_the_session),
                        );
                        assert_eq!(
                            through_the_api.status,
                            200,
                            "case {case:?}: round {round} of the Data API's own door must be \
                             served while the other door is using the database, got HTTP {} \
                             with {}",
                            through_the_api.status,
                            through_the_api.body
                        );
                        assert!(
                            through_the_session.is_ok(),
                            "case {case:?}: round {round} of the engine endpoint must be served \
                             while the other door is using the database, got \
                             {through_the_session:?}"
                        );
                    }

                    // And every row both doors wrote is in the cluster's database: the count
                    // is what the rounds added, read back through EACH door.
                    let expected = ROUNDS * row.rows_per_round;
                    let through_the_api = execute_statement(
                        &router,
                        &cluster.cluster_arn,
                        &cluster.secret_arn,
                        Some(DATABASE),
                        None,
                        &format!("SELECT count(*) FROM {table}"),
                    )
                    .await
                    .served(case);
                    assert_eq!(
                        api_column(case, &through_the_api),
                        vec![expected.to_string()],
                        "case {case:?}: the Data API must read every row the rounds wrote, got \
                         {through_the_api}"
                    );
                    let through_the_session = session
                        .served(case, &format!("SELECT count(*) FROM {table}"))
                        .await;
                    assert_eq!(
                        through_the_session,
                        vec![vec![Some(expected.to_string())]],
                        "case {case:?}: the engine endpoint must read every row the rounds \
                         wrote, got {through_the_session:?}"
                    );
                }
            });
        })
        .expect("the suite's thread starts")
        .join()
        .expect("the suite's thread finishes");
}

/// How many times the long write of the second case below DOUBLES its table: each statement
/// inserts as many rows as the table already holds, so the later rounds are writes a reader
/// arrives in the MIDDLE of — which is the window the rule is about. Bounded well below what
/// one statement of this emulator may write.
const DOUBLINGS: usize = 15;

/// How many statements the OTHER door runs beside one such write: the write is one statement,
/// so the door that is not writing asks again and again for as long as it lasts.
const READS_PER_ROUND: usize = 20;

/// Which door is the one in the middle of the long write; the other one reads.
struct LongWrite {
    /// What this row is a case of.
    case: &'static str,
    /// The table the row's statements name, which no other row names.
    table: &'static str,
    /// Whether the DATA API is the door that writes; else the native session writes.
    the_api_writes: bool,
}

/// Each door as the one holding the storage for a whole statement.
const LONG_WRITES: &[LongWrite] = &[
    LongWrite {
        case: "the Data API in the middle of a long write",
        table: "api_bulk_rows",
        the_api_writes: true,
    },
    LongWrite {
        case: "a session in the middle of a long write",
        table: "session_bulk_rows",
        the_api_writes: false,
    },
];

/// RULE, at a window a client can actually see: a statement that arrives while the other door
/// is in the MIDDLE of a write waits for the storage and is served — it is not refused for the
/// duration of the other door's statement.
#[test]
fn a_door_is_served_while_the_other_is_in_the_middle_of_a_write() {
    std::thread::Builder::new()
        .name("rds-cluster-doors-overlap".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a current-thread runtime");
            runtime.block_on(async {
                let cluster =
                    provisioned(&router, "doors-overlap-cluster", "doors-overlap-secret").await;
                let mut session =
                    Session::open("doors overlapping", &cluster.address, cluster.port, DATABASE)
                        .await;

                for row in LONG_WRITES {
                    let case = row.case;
                    let table = row.table;
                    for statement in [
                        format!("CREATE TABLE {table} (holder VARCHAR(40) NOT NULL)"),
                        format!("INSERT INTO {table} (holder) VALUES ('the first row')"),
                    ] {
                        execute_statement(
                            &router,
                            &cluster.cluster_arn,
                            &cluster.secret_arn,
                            Some(DATABASE),
                            None,
                            &statement,
                        )
                        .await
                        .served(case);
                    }
                    // One statement that writes as many rows as the table holds, so what the
                    // other door meets is a write still in progress.
                    let doubling =
                        format!("INSERT INTO {table} (holder) SELECT holder FROM {table}");
                    let read = format!("SELECT count(*) FROM {table}");

                    for round in 0..DOUBLINGS {
                        // Both doors are issued and then waited for together, so the reads
                        // land while the write holds the storage.
                        let (wrote, read_back) = if row.the_api_writes {
                            let writing = execute_statement(
                                &router,
                                &cluster.cluster_arn,
                                &cluster.secret_arn,
                                Some(DATABASE),
                                None,
                                &doubling,
                            );
                            let reading = async {
                                let mut refused = Vec::new();
                                for _ in 0..READS_PER_ROUND {
                                    if let Err(refusal) = session.query(case, &read).await {
                                        refused.push(format!("{refusal:?}"));
                                    }
                                }
                                refused
                            };
                            let (writing, refused) = tokio::join!(writing, reading);
                            ((writing.status, writing.body), refused)
                        } else {
                            let writing = session.query(case, &doubling);
                            let reading = async {
                                let mut refused = Vec::new();
                                for _ in 0..READS_PER_ROUND {
                                    let answer = execute_statement(
                                        &router,
                                        &cluster.cluster_arn,
                                        &cluster.secret_arn,
                                        Some(DATABASE),
                                        None,
                                        &read,
                                    )
                                    .await;
                                    if answer.status != 200 {
                                        refused.push(format!(
                                            "HTTP {} with {}",
                                            answer.status, answer.body
                                        ));
                                    }
                                }
                                refused
                            };
                            let (writing, refused) = tokio::join!(writing, reading);
                            let status = match writing {
                                Ok(_) => (200, String::new()),
                                Err(refusal) => (400, format!("{refusal:?}")),
                            };
                            (status, refused)
                        };
                        let (status, body) = wrote;
                        assert_eq!(
                            status, 200,
                            "case {case:?}: round {round}'s write must be served while the other \
                             door reads, got {body}"
                        );
                        assert!(
                            read_back.is_empty(),
                            "case {case:?}: round {round}: every read of the door that is not \
                             writing must be served, got {} refusal(s), the first {:?}",
                            read_back.len(),
                            read_back.first()
                        );
                    }

                    // And the write landed in the cluster's database, whole: the table holds
                    // what the doublings wrote, read back through each door.
                    let expected = 1usize << DOUBLINGS;
                    let through_the_api = execute_statement(
                        &router,
                        &cluster.cluster_arn,
                        &cluster.secret_arn,
                        Some(DATABASE),
                        None,
                        &read,
                    )
                    .await
                    .served(case);
                    assert_eq!(
                        api_column(case, &through_the_api),
                        vec![expected.to_string()],
                        "case {case:?}: the Data API must read every row the writes added, got \
                         {through_the_api}"
                    );
                    let through_the_session = session.served(case, &read).await;
                    assert_eq!(
                        through_the_session,
                        vec![vec![Some(expected.to_string())]],
                        "case {case:?}: the engine endpoint must read every row the writes \
                         added, got {through_the_session:?}"
                    );
                }
            });
        })
        .expect("the suite's thread starts")
        .join()
        .expect("the suite's thread finishes");
}
