// SPDX-License-Identifier: CC0-1.0

//! WHICH of a table restore's source names was wrong, and what the refusal
//! suggests instead — end to end over the awsQuery wire with the Redshift Data
//! API mounted beside the control plane on one engine.
//!
//! The RULE under test: `RestoreTableFromClusterSnapshot` refuses a source name
//! the snapshot does not hold with `InvalidTableRestoreArgument`, and the
//! sentence names the ONE value that was wrong — for a `SourceTableName` the
//! snapshot's database does not hold, the refusal quotes the name the request
//! sent and suggests the nearest table that database's own catalog does hold
//! (`Source table "<name>" doesn't exist in snapshot. Consider using:
//! "<candidate>"`), which is what makes it actionable for a client that cannot
//! read the snapshot's catalog itself. An unknown source DATABASE and an unknown
//! source SCHEMA are a different value and so a different sentence.
//!
//! The rows differ along what the rule generalises over: how far the name sent
//! is from the one the snapshot holds, which of the catalog's tables is the
//! nearest, the spelling (case) the request used, the schema the candidate is
//! read from (the default one and a named one), how many tables that schema
//! holds (several, exactly one, none at all) and which of the three source
//! members was wrong.
//!
//! The control-plane harness and the Data API harness are shared with
//! [`crate::cluster_lifecycle`] and [`crate::snapshot_data`], which own them.

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{all_text, Answer, Emulator};
use crate::snapshot_data::{create_cluster, create_snapshot, run_sql};

/// The database the snapshot's tables live in.
const DATABASE: &str = "devdb";

/// The cluster every case restores into, and the snapshot they restore from.
const CLUSTER: &str = "candidate-restore-cluster";
const SNAPSHOT: &str = "candidate-restore-snapshot";

/// The sentence a `SourceTableName` the snapshot does not hold is refused with,
/// spelled out here rather than read off the crate under test: the fixed text is
/// `Source table "`, `" doesn't exist in snapshot. Consider using: "` and `"`,
/// with no trailing period.
fn candidate_refusal(requested: &str, candidate: &str) -> String {
    format!(
        "Source table \"{requested}\" doesn't exist in snapshot. Consider using: \"{candidate}\""
    )
}

/// One row of the per-member table: the case, the members the restore carries,
/// and the sentence it must be refused with.
type MemberRow<'a> = (&'a str, Vec<(&'a str, &'a str)>, String);

/// `RestoreTableFromClusterSnapshot` with these members beside the cluster and
/// the snapshot every row names.
async fn restore_table(emulator: &Emulator, extra: &[(&str, &str)]) -> Answer {
    let mut params: Vec<(&str, &str)> = vec![
        ("ClusterIdentifier", CLUSTER),
        ("SnapshotIdentifier", SNAPSHOT),
    ];
    params.extend_from_slice(extra);
    emulator
        .call("RestoreTableFromClusterSnapshot", &params)
        .await
}

/// Assert one refusal: the status, the `<Code>` and the whole sentence.
fn assert_refused(case: &str, answer: &Answer, message: &str) {
    assert_eq!(
        (answer.status, answer.error_code().as_deref()),
        (400, Some("InvalidTableRestoreArgument")),
        "case {case}: expected 400 InvalidTableRestoreArgument, got {} / {:?} with body {}",
        answer.status,
        answer.error_code(),
        answer.body
    );
    assert_eq!(
        answer.text("Message").as_deref(),
        Some(message),
        "case {case}: expected the message {message:?}, got {:?}",
        answer.text("Message")
    );
}

/// An emulator holding [`CLUSTER`], the tables below in its database and an
/// AVAILABLE snapshot [`SNAPSHOT`] of it.
///
/// The default schema holds three tables, so the nearest one is a choice rather
/// than the only one; one named schema holds exactly one; and one named schema
/// holds none at all.
async fn seeded() -> Emulator {
    let emulator = Emulator::with_data_api();
    create_cluster(&emulator, "the candidate cases", CLUSTER, DATABASE).await;
    for sql in [
        "CREATE TABLE people (id INTEGER, name VARCHAR(16))",
        "INSERT INTO people (id, name) VALUES (1, 'ada')",
        "CREATE TABLE peoples (id INTEGER)",
        "CREATE TABLE totals (total INTEGER)",
        "CREATE SCHEMA analytics",
        "CREATE TABLE analytics.visits (day VARCHAR(10), hits INTEGER)",
        "CREATE SCHEMA staging",
    ] {
        run_sql(&emulator, "seed the tables", CLUSTER, DATABASE, sql).await;
    }
    create_snapshot(&emulator, "the candidate cases", CLUSTER, SNAPSHOT).await;
    // Both the cluster and the snapshot have to be serving before a restore is
    // admitted; the clock is ADVANCED across the span rather than waited out, so
    // no case here depends on how fast the host runs.
    emulator.advance_clock(state::PROVISIONING_SPAN);
    emulator
}

#[tokio::test]
async fn an_unknown_source_table_is_refused_with_the_nearest_table_the_snapshot_holds() {
    let emulator = seeded().await;

    // (case, the SourceSchemaName the request carries, the SourceTableName it
    // sends, the candidate the refusal must suggest)
    let rows: Vec<(&str, Option<&str>, &str, &str)> = vec![
        (
            "a name one character short of a held table",
            None,
            "peopl",
            "people",
        ),
        (
            "a name one character short of another held table",
            None,
            "total",
            "totals",
        ),
        (
            "the same name in another case, so the request's own spelling is echoed",
            None,
            "Peopl",
            "people",
        ),
        (
            "a name of a named schema's only table",
            Some("analytics"),
            "visit",
            "visits",
        ),
        (
            "a name nothing in that schema resembles",
            Some("analytics"),
            "nothing_like_it",
            "visits",
        ),
    ];

    for (case, schema, requested, candidate) in rows {
        let mut members: Vec<(&str, &str)> = vec![
            ("SourceDatabaseName", DATABASE),
            ("SourceTableName", requested),
            ("NewTableName", "restored_copy"),
        ];
        if let Some(schema) = schema {
            members.push(("SourceSchemaName", schema));
        }
        let refused = restore_table(&emulator, &members).await;
        assert_refused(case, &refused, &candidate_refusal(requested, candidate));

        // A refused restore records nothing and creates nothing.
        let listed = emulator
            .call(
                "DescribeTableRestoreStatus",
                &[("ClusterIdentifier", CLUSTER)],
            )
            .await;
        assert!(
            all_text(&listed.body, "TableRestoreRequestId").is_empty(),
            "case {case}: expected the refused restore to record nothing, got body {}",
            listed.body
        );
    }

    // The other half of the rule: the table the snapshot DOES hold is restored,
    // so the refusals above are about the name and not about the operation —
    // and the candidate the refusal suggested is a name that works.
    let served = restore_table(
        &emulator,
        &[
            ("SourceDatabaseName", DATABASE),
            ("SourceTableName", "people"),
            ("NewTableName", "people_restored"),
        ],
    )
    .await;
    assert_eq!(
        served.status, 200,
        "case the candidate the refusal suggested: expected the restore to be served, got {} \
         with body {}",
        served.status, served.body
    );
}

#[tokio::test]
async fn each_wrong_source_name_is_refused_with_its_own_sentence() {
    let emulator = seeded().await;

    // (case, the members the row carries, the sentence it must be refused with)
    let rows: Vec<MemberRow> = vec![
        (
            "a source database the snapshot does not hold",
            vec![
                ("SourceDatabaseName", "nowheredb"),
                ("SourceTableName", "people"),
                ("NewTableName", "restored_copy"),
            ],
            "The value specified for SourceDatabaseName doesn't exist in the snapshot."
                .to_string(),
        ),
        (
            "a source schema the snapshot's database does not hold",
            vec![
                ("SourceDatabaseName", DATABASE),
                ("SourceSchemaName", "nowhere_schema"),
                ("SourceTableName", "people"),
                ("NewTableName", "restored_copy"),
            ],
            "The value specified for SourceSchemaName doesn't exist in the snapshot.".to_string(),
        ),
        (
            "a source table of a schema that holds none at all",
            vec![
                ("SourceDatabaseName", DATABASE),
                ("SourceSchemaName", "staging"),
                ("SourceTableName", "people"),
                ("NewTableName", "restored_copy"),
            ],
            // Nothing to consider, so the refusal falls back to the fault's own
            // documented sentence rather than suggesting a name no catalog
            // answered with.
            "The value specified for the sourceDatabaseName, sourceSchemaName, or \
             sourceTableName parameter, or a combination of these, doesn't exist in the snapshot."
                .to_string(),
        ),
        (
            "a source table the snapshot's database does not hold",
            vec![
                ("SourceDatabaseName", DATABASE),
                ("SourceTableName", "peopl"),
                ("NewTableName", "restored_copy"),
            ],
            candidate_refusal("peopl", "people"),
        ),
    ];

    for (case, members, message) in rows {
        let refused = restore_table(&emulator, &members).await;
        assert_refused(case, &refused, &message);
    }

    // The sentences are DISTINGUISHABLE: the three source members are refused
    // three ways rather than collapsed onto one template.
    let mut answered: Vec<String> = Vec::new();
    for members in [
        vec![
            ("SourceDatabaseName", "nowheredb"),
            ("SourceTableName", "people"),
            ("NewTableName", "restored_copy"),
        ],
        vec![
            ("SourceDatabaseName", DATABASE),
            ("SourceSchemaName", "nowhere_schema"),
            ("SourceTableName", "people"),
            ("NewTableName", "restored_copy"),
        ],
        vec![
            ("SourceDatabaseName", DATABASE),
            ("SourceTableName", "peopl"),
            ("NewTableName", "restored_copy"),
        ],
    ] {
        let refused = restore_table(&emulator, &members).await;
        answered.push(refused.text("Message").unwrap_or_else(|| {
            panic!(
                "case the three source members: every refusal carries a message, got body {}",
                refused.body
            )
        }));
    }
    let mut distinct = answered.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        answered.len(),
        "case the three source members: expected a sentence per wrong member, got {answered:?}"
    );
}
