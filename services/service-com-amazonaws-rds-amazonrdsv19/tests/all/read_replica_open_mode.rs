// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the OPEN MODE a read replica runs in: `ReplicaMode`.
//!
//! The rule under test: `ReplicaMode` is "the open mode of a Db2 or an Oracle read replica.
//! The default is `open-read-only`", and "this attribute is only supported in RDS for Db2,
//! RDS for Oracle, and RDS Custom for Oracle"
//! (`smithy_query(query="doc",shape="DBInstance",member="ReplicaMode",
//! model_path="services/service-com-amazonaws-rds-amazonrdsv19/smithy-model/com-amazonaws-rds-amazonrdsv19.json")`).
//! So a DB instance that REPLICATES another and runs one of those engine families reports the
//! mode its request asked for — the documented default where it asked for none — on every
//! read of it, every other instance reports no mode at all, a value outside the two the member
//! publishes is refused with the Query API's `InvalidParameterValue`, `ModifyDBInstance`
//! changes the mode of an existing replica, and an instance a promotion or a switchover has
//! made primary replicates nothing and so is in no replica mode.
//!
//! The tables below span what the rule generalises over: the engine families whose replicas
//! HAVE an open mode (Oracle enterprise, Oracle multi-tenant, Oracle standard edition 2 and
//! Db2) and families whose replicas have none (PostgreSQL and MySQL), each value the member
//! publishes plus the request that names none, the two operations that set it — including a
//! valid mode named for an instance that HAS none, which is served and stores nothing — and
//! the two role changes that end the replication. One row per TEST, each on its own emulator,
//! so no single test carries the whole class's provisioning work.
//!
//! Every call goes over the wire through `db_instances::emulator`, the one mounting helper this
//! test target has, as a real awsQuery form POST carrying the SigV4 credential scope the router
//! keys on, and every assertion reads the XML the emulator answered with. Nothing sleeps for a
//! transition: each wait is a poll loop on a REPORTING call through `lifecycle_poll_loops`.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, emulator, expect_error, expect_ok, optional_text, text};
use crate::lifecycle_poll_loops::poll_until;

/// The open mode a replica that asked for none runs in, as the member documents it.
const DEFAULT_MODE: &str = "open-read-only";

/// The other value `ReplicaMode` publishes.
const MOUNTED: &str = "mounted";

/// `CreateDBInstance` for a source instance, with the automated backups a read replica of it
/// is built from.
async fn create_source(router: &MultiServiceRouter, case: &str, identifier: &str, engine: &str) {
    expect_ok(
        case,
        call(
            router,
            "CreateDBInstance",
            &[
                ("DBInstanceIdentifier", identifier),
                ("Engine", engine),
                ("DBInstanceClass", "db.m5.large"),
                ("AllocatedStorage", "20"),
                ("BackupRetentionPeriod", "1"),
                ("MasterUsername", "appadmin"),
                ("MasterUserPassword", "an-example-password"),
            ],
        )
        .await,
    );
}

/// `CreateDBInstanceReadReplica` of `source`, naming `mode` where the row asks for one.
async fn create_replica(
    router: &MultiServiceRouter,
    identifier: &str,
    source: &str,
    mode: Option<&str>,
) -> (u16, String) {
    let mut params: Vec<(&str, String)> = vec![
        ("DBInstanceIdentifier", identifier.to_string()),
        ("SourceDBInstanceIdentifier", source.to_string()),
    ];
    if let Some(mode) = mode {
        params.push(("ReplicaMode", mode.to_string()));
    }
    call(router, "CreateDBInstanceReadReplica", &params).await
}

/// `DescribeDBInstances` naming one instance, polled until it reports `available` — the loop a
/// client's waiter runs, making only this REPORTING call.
async fn awaited(router: &MultiServiceRouter, case: &str, identifier: &str) -> String {
    poll_until(
        case,
        &format!("DB instance {identifier}"),
        || async {
            call(
                router,
                "DescribeDBInstances",
                &[("DBInstanceIdentifier", identifier)],
            )
            .await
        },
        |status, body| status == 200 && text(body, "DBInstanceStatus") == "available",
    )
    .await
    .0
}

/// One row of the engine-family table.
struct ModeCase {
    case: &'static str,
    engine: &'static str,
    source: &'static str,
    replica: &'static str,
    /// What `CreateDBInstanceReadReplica` asks for, and `None` for a request that names no
    /// mode at all.
    asked: Option<&'static str>,
    /// The mode the replica then reports, and `None` for an engine family whose replicas have
    /// no open mode.
    reported: Option<&'static str>,
}

/// The engine families whose replicas have an open mode, each value the member publishes, and
/// two families whose replicas have none.
const MODES: &[ModeCase] = &[
    ModeCase {
        case: "a mounted oracle-ee replica",
        engine: "oracle-ee",
        source: "mode-src-oracle-ee",
        replica: "mode-rep-oracle-ee",
        asked: Some(MOUNTED),
        reported: Some(MOUNTED),
    },
    ModeCase {
        case: "an oracle-ee-cdb replica opened read-only",
        engine: "oracle-ee-cdb",
        source: "mode-src-oracle-cdb",
        replica: "mode-rep-oracle-cdb",
        asked: Some(DEFAULT_MODE),
        reported: Some(DEFAULT_MODE),
    },
    ModeCase {
        case: "an oracle-se2 replica that asked for no mode",
        engine: "oracle-se2",
        source: "mode-src-oracle-se2",
        replica: "mode-rep-oracle-se2",
        asked: None,
        reported: Some(DEFAULT_MODE),
    },
    ModeCase {
        case: "a mounted db2-se replica",
        engine: "db2-se",
        source: "mode-src-db2",
        replica: "mode-rep-db2",
        asked: Some(MOUNTED),
        reported: Some(MOUNTED),
    },
    ModeCase {
        case: "a postgres replica, whose engine has no open mode",
        engine: "postgres",
        source: "mode-src-postgres",
        replica: "mode-rep-postgres",
        asked: None,
        reported: None,
    },
    ModeCase {
        case: "a mysql replica, whose engine has no open mode",
        engine: "mysql",
        source: "mode-src-mysql",
        replica: "mode-rep-mysql",
        asked: None,
        reported: None,
    },
];

/// Drive one row of the table: build the source and the replica the row names and read the
/// open mode back, from the create's own answer and from a later describe of each instance.
///
/// One row per TEST below rather than one test over the whole table, so no single test carries
/// the whole class's provisioning work.
async fn one_row(row: &ModeCase) {
    let router = emulator();
    let case = row.case;
    create_source(&router, case, row.source, row.engine).await;
    awaited(&router, case, row.source).await;
    let created = expect_ok(
        case,
        create_replica(&router, row.replica, row.source, row.asked).await,
    );
    assert_eq!(
        optional_text(&created, "ReplicaMode").as_deref(),
        row.reported,
        "case {case:?}: the create must answer the open mode the replica runs in, got \
         {created}"
    );
    // ROUND TRIP: the same mode on a later read of the settled replica, and no mode at
    // all on the SOURCE, which replicates nothing.
    let settled = awaited(&router, case, row.replica).await;
    assert_eq!(
        optional_text(&settled, "ReplicaMode").as_deref(),
        row.reported,
        "case {case:?}: a describe of the replica must report the same mode, got {settled}"
    );
    let source = awaited(&router, case, row.source).await;
    assert_eq!(
        optional_text(&source, "ReplicaMode"),
        None,
        "case {case:?}: an instance that replicates nothing is in no replica mode, got \
         {source}"
    );
}

#[tokio::test]
async fn a_mounted_oracle_replica_reports_its_open_mode() {
    one_row(&MODES[0]).await;
}

#[tokio::test]
async fn a_multi_tenant_oracle_replica_reports_the_mode_it_was_opened_in() {
    one_row(&MODES[1]).await;
}

#[tokio::test]
async fn an_oracle_replica_that_asked_for_no_mode_reports_the_documented_default() {
    one_row(&MODES[2]).await;
}

#[tokio::test]
async fn a_mounted_db2_replica_reports_its_open_mode() {
    one_row(&MODES[3]).await;
}

#[tokio::test]
async fn a_postgres_replica_reports_no_open_mode() {
    one_row(&MODES[4]).await;
}

#[tokio::test]
async fn a_mysql_replica_reports_no_open_mode() {
    one_row(&MODES[5]).await;
}

/// The values the member does NOT publish, each refused rather than stored.
const REJECTED: [(&str, &str); 3] = [
    ("a mode no engine has", "read-write"),
    ("the empty string", ""),
    ("a mode in another spelling", "Mounted-Standby"),
];

/// How a value outside `ReplicaMode`'s published list is refused: the enum constraint the
/// model declares for the member, in the sentence this front door words a constraint
/// violation with, naming the member and the value set.
fn not_a_replica_mode(value: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at 'replicaMode' failed to satisfy \
         constraint: Member must satisfy enum value set: [open-read-only, mounted]"
    )
}

#[tokio::test]
async fn a_replica_mode_outside_the_published_values_is_refused() {
    let router = emulator();
    let case = "the source of the refused replicas";
    create_source(&router, case, "mode-refusal-src", "oracle-ee").await;
    awaited(&router, case, "mode-refusal-src").await;

    for (case, asked) in REJECTED {
        let refused = expect_error(
            case,
            create_replica(&router, "mode-refusal-rep", "mode-refusal-src", Some(asked)).await,
            400,
            "InvalidParameterValue",
        );
        assert_eq!(
            text(&refused, "Message"),
            not_a_replica_mode(asked),
            "case {case:?}: got {refused}"
        );
        // Nothing was stored: the identifier the refused create named resolves to no
        // instance.
        let (status, body) = call(
            &router,
            "DescribeDBInstances",
            &[("DBInstanceIdentifier", "mode-refusal-rep")],
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: a refused create stores nothing, got {body}"
        );
        // And the same value is refused on the modify path of an existing instance.
        let refused = expect_error(
            case,
            call(
                &router,
                "ModifyDBInstance",
                &[
                    ("DBInstanceIdentifier", "mode-refusal-src"),
                    ("ReplicaMode", asked),
                ],
            )
            .await,
            400,
            "InvalidParameterValue",
        );
        assert_eq!(
            text(&refused, "Message"),
            not_a_replica_mode(asked),
            "case {case:?}: got {refused}"
        );
    }
}

#[tokio::test]
async fn a_mode_an_engine_family_does_not_have_is_not_stored() {
    let router = emulator();

    // A replica whose ENGINE family has no open mode: the create and the modify are served —
    // nothing about the request is invalid — and the replica reports no mode, before and
    // after.
    let case = "a postgres replica asked to run mounted";
    create_source(&router, case, "mode-none-postgres-src", "postgres").await;
    awaited(&router, case, "mode-none-postgres-src").await;
    expect_ok(
        case,
        create_replica(
            &router,
            "mode-none-postgres-rep",
            "mode-none-postgres-src",
            Some(MOUNTED),
        )
        .await,
    );
    let replica = awaited(&router, case, "mode-none-postgres-rep").await;
    assert_eq!(
        optional_text(&replica, "ReplicaMode"),
        None,
        "case {case:?}: got {replica}"
    );
    expect_ok(
        case,
        call(
            &router,
            "ModifyDBInstance",
            &[
                ("DBInstanceIdentifier", "mode-none-postgres-rep"),
                ("ReplicaMode", MOUNTED),
                ("ApplyImmediately", "true"),
            ],
        )
        .await,
    );
    let modified = awaited(&router, case, "mode-none-postgres-rep").await;
    assert_eq!(
        optional_text(&modified, "ReplicaMode"),
        None,
        "case {case:?}: an engine family with no open mode reports none after a modify that \
         named one, got {modified}"
    );
}

#[tokio::test]
async fn a_mode_an_instance_that_replicates_nothing_named_is_not_stored() {
    let router = emulator();

    // An Oracle instance that REPLICATES NOTHING: its engine family has an open mode but it
    // is in none, so the modify stores nothing — and a later switchover that makes it a
    // standby reports the documented default rather than a mode kept from that request.
    let case = "a standalone oracle-ee instance asked to run mounted";
    create_source(&router, case, "mode-none-oracle", "oracle-ee").await;
    awaited(&router, case, "mode-none-oracle").await;
    expect_ok(
        case,
        call(
            &router,
            "ModifyDBInstance",
            &[
                ("DBInstanceIdentifier", "mode-none-oracle"),
                ("ReplicaMode", MOUNTED),
                ("ApplyImmediately", "true"),
            ],
        )
        .await,
    );
    let standalone = awaited(&router, case, "mode-none-oracle").await;
    assert_eq!(
        optional_text(&standalone, "ReplicaMode"),
        None,
        "case {case:?}: an instance that replicates nothing is in no replica mode, got \
         {standalone}"
    );
    expect_ok(
        case,
        create_replica(&router, "mode-none-oracle-rep", "mode-none-oracle", None).await,
    );
    awaited(&router, case, "mode-none-oracle-rep").await;
    expect_ok(
        case,
        call(
            &router,
            "SwitchoverReadReplica",
            &[("DBInstanceIdentifier", "mode-none-oracle-rep")],
        )
        .await,
    );
    let now_a_standby = awaited(&router, case, "mode-none-oracle").await;
    assert_eq!(
        optional_text(&now_a_standby, "ReplicaMode").as_deref(),
        Some(DEFAULT_MODE),
        "case {case:?}: an instance a switchover made a standby runs in the documented \
         default, not in a mode an earlier modify named while it had none, got {now_a_standby}"
    );
}

#[tokio::test]
async fn a_modification_changes_the_open_mode_and_a_role_change_ends_it() {
    let router = emulator();
    let case = "an oracle-ee standby opened read-only and then mounted";
    create_source(&router, case, "mode-switch-primary", "oracle-ee").await;
    awaited(&router, case, "mode-switch-primary").await;
    expect_ok(
        case,
        create_replica(
            &router,
            "mode-switch-standby",
            "mode-switch-primary",
            Some(DEFAULT_MODE),
        )
        .await,
    );
    awaited(&router, case, "mode-switch-standby").await;

    // `ModifyDBInstance` changes the mode the replica runs in, and the change is applied as
    // soon as possible rather than staged for the maintenance window.
    expect_ok(
        case,
        call(
            &router,
            "ModifyDBInstance",
            &[
                ("DBInstanceIdentifier", "mode-switch-standby"),
                ("ReplicaMode", MOUNTED),
            ],
        )
        .await,
    );
    let mounted = awaited(&router, case, "mode-switch-standby").await;
    assert_eq!(
        optional_text(&mounted, "ReplicaMode").as_deref(),
        Some(MOUNTED),
        "case {case:?}: the modified replica must report the mode it was moved to, got {mounted}"
    );

    // A SWITCHOVER exchanges the roles: the instance it names replicates nothing from then on,
    // so it is in no replica mode, and the former primary — now the standby — reports the
    // documented default it has not been given a mode of its own.
    expect_ok(
        case,
        call(
            &router,
            "SwitchoverReadReplica",
            &[("DBInstanceIdentifier", "mode-switch-standby")],
        )
        .await,
    );
    let new_primary = awaited(&router, case, "mode-switch-standby").await;
    let new_standby = awaited(&router, case, "mode-switch-primary").await;
    assert_eq!(
        (
            optional_text(&new_primary, "ReplicaMode"),
            optional_text(&new_standby, "ReplicaMode").as_deref(),
        ),
        (None, Some(DEFAULT_MODE)),
        "case {case:?}: got new primary {new_primary} and new standby {new_standby}"
    );

    // And a PROMOTION ends it the same way: the promoted instance replicates nothing.
    expect_ok(
        case,
        call(
            &router,
            "PromoteReadReplica",
            &[("DBInstanceIdentifier", "mode-switch-primary")],
        )
        .await,
    );
    let promoted = awaited(&router, case, "mode-switch-primary").await;
    assert_eq!(
        (
            optional_text(&promoted, "ReplicaMode"),
            optional_text(&promoted, "ReadReplicaSourceDBInstanceIdentifier"),
        ),
        (None, None),
        "case {case:?}: a promoted instance is in no replica mode, got {promoted}"
    );
}
