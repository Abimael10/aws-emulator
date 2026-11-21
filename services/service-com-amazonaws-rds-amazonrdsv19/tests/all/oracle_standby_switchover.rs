// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `SwitchoverReadReplica`, and for the lifecycle an Oracle Data Guard
//! topology is built through.
//!
//! The rules under test:
//!
//! * a switchover is the ORACLE Data Guard role transition, so a DB instance running any
//!   other database engine names a value the operation does not serve: HTTP 400 with the
//!   Query API's `InvalidParameterValue` and a message naming the instance from the request
//!   and the engine from that instance's own record — never a 501, and never a state fault,
//!   since no waiting changes the engine an instance runs;
//! * on a supported engine the call EXCHANGES the two instances' roles: the standby it names
//!   replicates nothing from then on and reports the former primary among its read replicas,
//!   while that former primary reports the standby as its source — read back through
//!   `DescribeDBInstances` on both instances;
//! * what the operation needs beyond the engine is the modelled pair: an identifier that
//!   names a DB instance of the region (else HTTP 404 `DBInstanceNotFound`) which is a read
//!   replica whose engine is running (else HTTP 400 `InvalidDBInstanceState`);
//! * a DB instance of an engine this emulator serves no data plane for, and a read replica of
//!   it, each reach `available` with nothing inside their `PendingModifiedValues`, and a
//!   modification applied immediately settles them back there — the advance-to-terminal-state
//!   contract every waiter and provisioning step depends on.
//!
//! The tables span what the rules generalise over: the engine families a switchover does not
//! apply to (three different products), the Oracle engines it does (the plain and the
//! multi-tenant edition), and the states an instance can be addressed in.
//!
//! Every call goes over the wire: real awsQuery form POSTs through a `MultiServiceRouter`
//! carrying the SigV4 credential scope the router keys on, and every assertion reads the XML
//! the emulator answered with. Nothing sleeps for a transition: each wait is a poll loop on a
//! REPORTING call, through the one `poll_until` helper this test target has.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{
    call, emulator, expect_error, expect_ok, optional_text, subtrees, text, texts, REGION,
};
use crate::lifecycle_poll_loops::poll_until;

/// The one `StatusInfos` entry a DB instance reports while it REPLICATES another, as
/// `(StatusType, Status)` — or `None` for an instance that replicates nothing, which reports
/// the member blank.
///
/// Read off the entry's own subtree rather than off the whole answer: a `DBInstance` carries
/// a `Status` inside its security-group and option-group memberships too, so "the one
/// `<Status>`" is only unambiguous inside the entry.
fn replication_status(xml: &str) -> Option<(String, String)> {
    let entries = subtrees(xml, "DBInstanceStatusInfo");
    assert!(
        entries.len() <= 1,
        "expected at most one DBInstanceStatusInfo in {xml}, found {}",
        entries.len()
    );
    entries
        .first()
        .map(|entry| (text(entry, "StatusType"), text(entry, "Status")))
}

/// `CreateDBInstance` for a source instance, with the automated backups a read replica of it
/// has to be built from.
async fn primary(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    engine: &str,
    extra: &[(&str, &str)],
) {
    let mut params: Vec<(&str, String)> = vec![
        ("DBInstanceIdentifier", identifier.to_string()),
        ("Engine", engine.to_string()),
        ("DBInstanceClass", "db.m5.large".to_string()),
        ("AllocatedStorage", "20".to_string()),
        ("BackupRetentionPeriod", "1".to_string()),
        ("MasterUsername", "appadmin".to_string()),
        ("MasterUserPassword", "an-example-password".to_string()),
    ];
    for (name, value) in extra {
        params.push((name, (*value).to_string()));
    }
    expect_ok(case, call(router, "CreateDBInstance", &params).await);
}

/// `CreateDBInstanceReadReplica` of `source`.
async fn replica(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    source: &str,
    extra: &[(&str, &str)],
) {
    let mut params: Vec<(&str, String)> = vec![
        ("DBInstanceIdentifier", identifier.to_string()),
        ("SourceDBInstanceIdentifier", source.to_string()),
    ];
    for (name, value) in extra {
        params.push((name, (*value).to_string()));
    }
    expect_ok(
        case,
        call(router, "CreateDBInstanceReadReplica", &params).await,
    );
}

/// `DescribeDBInstances` naming one instance, polled until it reports `available`: the loop a
/// client's waiter runs, making only this REPORTING call, so only a transition this service's
/// clock owes can end it.
async fn awaited_instance(router: &MultiServiceRouter, case: &str, identifier: &str) -> String {
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

/// `DescribeDBInstances` naming one instance, read once.
async fn describe(router: &MultiServiceRouter, case: &str, identifier: &str) -> String {
    expect_ok(
        case,
        call(
            router,
            "DescribeDBInstances",
            &[("DBInstanceIdentifier", identifier)],
        )
        .await,
    )
}

/// `SwitchoverReadReplica` naming one instance.
async fn switchover(router: &MultiServiceRouter, identifier: &str) -> (u16, String) {
    call(
        router,
        "SwitchoverReadReplica",
        &[("DBInstanceIdentifier", identifier)],
    )
    .await
}

/// The message Amazon RDS refuses a switchover of an instance whose engine the operation does
/// not apply to with, spelled out here so the wording is pinned by the test rather than by
/// the code under test: fixed text, with the instance from the request and the engine from
/// that instance's state.
fn unsupported_engine(identifier: &str, engine: &str) -> String {
    format!(
        "You can't switch over the DB instance {identifier} because the operation is not \
         supported for {engine} engine."
    )
}

/// One row of the unsupported-engine table: an engine a switchover does not apply to, and the
/// two instances the row builds in it.
struct EngineCase {
    case: &'static str,
    engine: &'static str,
    source: &'static str,
    standby: &'static str,
}

/// Three different database products a read replica can be built in, none of them Oracle.
const UNSUPPORTED: &[EngineCase] = &[
    EngineCase {
        case: "a postgres read replica",
        engine: "postgres",
        source: "switchover-src-postgres",
        standby: "switchover-rep-postgres",
    },
    EngineCase {
        case: "a mysql read replica",
        engine: "mysql",
        source: "switchover-src-mysql",
        standby: "switchover-rep-mysql",
    },
    EngineCase {
        case: "a mariadb read replica",
        engine: "mariadb",
        source: "switchover-src-mariadb",
        standby: "switchover-rep-mariadb",
    },
];

#[tokio::test]
async fn a_switchover_is_refused_for_every_engine_the_operation_does_not_apply_to() {
    let router = emulator();
    for row in UNSUPPORTED {
        let case = row.case;
        primary(&router, case, row.source, row.engine, &[]).await;
        awaited_instance(&router, case, row.source).await;
        replica(&router, case, row.standby, row.source, &[]).await;
        let described = awaited_instance(&router, case, row.standby).await;
        // The replica this row addresses really is one, and is replicating: the refusal below
        // is therefore about the ENGINE and nothing else.
        assert_eq!(
            (
                text(&described, "ReadReplicaSourceDBInstanceIdentifier"),
                replication_status(&described),
            ),
            (
                row.source.to_string(),
                Some(("read replication".to_string(), "replicating".to_string())),
            ),
            "case {case:?}: got {described}"
        );

        let refused = expect_error(
            case,
            switchover(&router, row.standby).await,
            400,
            "InvalidParameterValue",
        );
        assert_eq!(
            text(&refused, "Message"),
            unsupported_engine(row.standby, row.engine),
            "case {case:?}: got {refused}"
        );
        // The refusal changed nothing: the replica still replicates its source.
        let after = describe(&router, case, row.standby).await;
        assert_eq!(
            text(&after, "ReadReplicaSourceDBInstanceIdentifier"),
            row.source,
            "case {case:?}: got {after}"
        );
    }
}

/// One row of the supported-engine table: an Oracle engine, and the pair the row switches
/// over.
struct SwitchoverCase {
    case: &'static str,
    engine: &'static str,
    source: &'static str,
    standby: &'static str,
    /// What `CreateDBInstanceReadReplica` is given beyond the two identifiers.
    extra: &'static [(&'static str, &'static str)],
}

/// The Oracle engines a switchover applies to: the plain enterprise edition, and the
/// multi-tenant one, whose standby this row mounts.
const SUPPORTED: &[SwitchoverCase] = &[
    SwitchoverCase {
        case: "an oracle-ee standby",
        engine: "oracle-ee",
        source: "switchover-src-oracle",
        standby: "switchover-rep-oracle",
        extra: &[],
    },
    SwitchoverCase {
        case: "a mounted oracle-ee-cdb standby",
        engine: "oracle-ee-cdb",
        source: "switchover-src-oracle-cdb",
        standby: "switchover-rep-oracle-cdb",
        extra: &[("ReplicaMode", "mounted")],
    },
];

#[tokio::test]
async fn a_switchover_exchanges_the_roles_of_an_oracle_primary_and_its_standby() {
    let router = emulator();
    for row in SUPPORTED {
        let case = row.case;
        primary(
            &router,
            case,
            row.source,
            row.engine,
            &[("LicenseModel", "bring-your-own-license")],
        )
        .await;
        awaited_instance(&router, case, row.source).await;
        replica(&router, case, row.standby, row.source, row.extra).await;
        awaited_instance(&router, case, row.standby).await;

        let (status, body) = switchover(&router, row.standby).await;
        let answered = expect_ok(case, (status, body));
        // The answer is the instance the request named — the new primary — and it reports the
        // exchange this call made.
        assert_eq!(
            (
                text(&answered, "DBInstanceIdentifier"),
                optional_text(&answered, "ReadReplicaSourceDBInstanceIdentifier"),
                texts(&answered, "ReadReplicaDBInstanceIdentifier"),
            ),
            (
                row.standby.to_string(),
                None,
                vec![row.source.to_string()],
            ),
            "case {case:?}: got {answered}"
        );

        // ROUND TRIP: both instances report the exchanged roles on a later read, and both
        // settle back to `available` on their own.
        let new_primary = awaited_instance(&router, case, row.standby).await;
        assert_eq!(
            (
                optional_text(&new_primary, "ReadReplicaSourceDBInstanceIdentifier"),
                texts(&new_primary, "ReadReplicaDBInstanceIdentifier"),
                replication_status(&new_primary),
            ),
            (None, vec![row.source.to_string()], None),
            "case {case:?}: got {new_primary}"
        );
        let new_standby = awaited_instance(&router, case, row.source).await;
        assert_eq!(
            (
                text(&new_standby, "ReadReplicaSourceDBInstanceIdentifier"),
                texts(&new_standby, "ReadReplicaDBInstanceIdentifier"),
                replication_status(&new_standby),
            ),
            (
                row.standby.to_string(),
                Vec::new(),
                Some(("read replication".to_string(), "replicating".to_string())),
            ),
            "case {case:?}: got {new_standby}"
        );

        // And the exchange is repeatable: the new standby can be switched back over.
        let back = expect_ok(case, switchover(&router, row.source).await);
        assert_eq!(
            (
                optional_text(&back, "ReadReplicaSourceDBInstanceIdentifier"),
                texts(&back, "ReadReplicaDBInstanceIdentifier"),
            ),
            (None, vec![row.standby.to_string()]),
            "case {case:?}: got {back}"
        );
    }
}

#[tokio::test]
async fn a_switchover_refuses_what_it_cannot_be_performed_on() {
    let router = emulator();
    let case = "an identifier naming no DB instance";
    let refused = expect_error(
        case,
        switchover(&router, "switchover-absent").await,
        404,
        "DBInstanceNotFound",
    );
    assert_eq!(
        text(&refused, "Message"),
        "DBInstance switchover-absent not found.",
        "case {case:?}: got {refused}"
    );

    // An ORACLE instance that is no read replica: the engine applies, so the refusal is the
    // state fault.
    let case = "a standalone oracle instance";
    primary(
        &router,
        case,
        "switchover-standalone",
        "oracle-ee",
        &[("LicenseModel", "bring-your-own-license")],
    )
    .await;
    awaited_instance(&router, case, "switchover-standalone").await;
    let refused = expect_error(
        case,
        switchover(&router, "switchover-standalone").await,
        400,
        "InvalidDBInstanceState",
    );
    assert_eq!(
        text(&refused, "Message"),
        "DB Instance is not a read replica.",
        "case {case:?}: got {refused}"
    );

    // A standby whose engine is not running yet: the create answers `creating`, and a
    // switchover issued before the clock settles it is refused for the state the instance is
    // in rather than served.
    let case = "a standby that is still being created";
    replica(
        &router,
        case,
        "switchover-creating",
        "switchover-standalone",
        &[],
    )
    .await;
    let refused = expect_error(
        case,
        switchover(&router, "switchover-creating").await,
        400,
        "InvalidDBInstanceState",
    );
    assert_eq!(
        text(&refused, "Message"),
        "Instance switchover-creating is not in available state.",
        "case {case:?}: got {refused}"
    );
    // Once it is up the same call is served, which is what makes the refusal above a state
    // and not a parameter.
    awaited_instance(&router, case, "switchover-creating").await;
    expect_ok(case, switchover(&router, "switchover-creating").await);
}

#[tokio::test]
async fn an_oracle_primary_and_its_mounted_standby_each_reach_their_terminal_state() {
    let router = emulator();
    let case = "the oracle-ee primary and its mounted standby";
    // The engine version the catalogue publishes as the default for this engine, which is
    // what a client builds the topology with.
    let versions = expect_ok(
        case,
        call(
            &router,
            "DescribeDBEngineVersions",
            &[("Engine", "oracle-ee"), ("DefaultOnly", "true")],
        )
        .await,
    );
    let version = text(&versions, "EngineVersion");

    primary(
        &router,
        case,
        "lifecycle-oracle-primary",
        "oracle-ee",
        &[
            ("EngineVersion", version.as_str()),
            ("LicenseModel", "bring-your-own-license"),
            ("StorageType", "gp3"),
        ],
    )
    .await;
    let up = awaited_instance(&router, case, "lifecycle-oracle-primary").await;
    // `PendingModifiedValues` carries one subelement per pending change, so an instance
    // that is up carries the element with nothing inside it.
    assert_eq!(
        optional_text(&up, "PendingModifiedValues").as_deref(),
        Some(""),
        "case {case:?}: an instance that is up has nothing pending: {up}"
    );
    assert_eq!(
        text(&up, "DBInstanceArn"),
        format!("arn:aws:rds:{REGION}:000000000000:db:lifecycle-oracle-primary"),
        "case {case:?}: got {up}"
    );

    replica(
        &router,
        case,
        "lifecycle-oracle-standby",
        "lifecycle-oracle-primary",
        &[("ReplicaMode", "mounted")],
    )
    .await;
    let standby = awaited_instance(&router, case, "lifecycle-oracle-standby").await;
    assert_eq!(
        (
            optional_text(&standby, "PendingModifiedValues").as_deref(),
            text(&standby, "ReadReplicaSourceDBInstanceIdentifier"),
            replication_status(&standby),
        ),
        (
            Some(""),
            "lifecycle-oracle-primary".to_string(),
            Some(("read replication".to_string(), "replicating".to_string())),
        ),
        "case {case:?}: got {standby}"
    );

    // A modification applied immediately settles the standby back to `available` with the
    // automated backups it was given.
    expect_ok(
        case,
        call(
            &router,
            "ModifyDBInstance",
            &[
                ("DBInstanceIdentifier", "lifecycle-oracle-standby"),
                ("BackupRetentionPeriod", "1"),
                ("ApplyImmediately", "true"),
            ],
        )
        .await,
    );
    let settled = poll_until(
        case,
        "the standby's automated backups",
        || async {
            call(
                &router,
                "DescribeDBInstances",
                &[("DBInstanceIdentifier", "lifecycle-oracle-standby")],
            )
            .await
        },
        |status, body| {
            status == 200
                && text(body, "DBInstanceStatus") == "available"
                && text(body, "BackupRetentionPeriod") == "1"
        },
    )
    .await
    .0;
    assert_eq!(
        optional_text(&settled, "PendingModifiedValues").as_deref(),
        Some(""),
        "case {case:?}: a settled instance has nothing pending: {settled}"
    );
}
