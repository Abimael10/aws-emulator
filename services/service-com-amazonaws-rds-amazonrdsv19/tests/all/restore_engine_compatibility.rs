// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ENGINE a restore may ask for.
//!
//! Every call goes over the wire through `db_instances::emulator`, as a real awsQuery form
//! POST carrying the SigV4 credential scope the router keys on, so the assertions read the
//! XML the emulator itself answered with.
//!
//! The rule under test: `RestoreDBInstanceFromDBSnapshot$Engine` is "the database engine to
//! use for the new instance … Default: The same as source. Constraint: Must be compatible
//! with the engine of the source. For example, you can restore a MariaDB 10.1 DB instance
//! from a MySQL 5.6 snapshot." A snapshot holds the data of the engine it was taken of, so
//! the engines that may run it are that engine and the engines that ARE that engine with
//! different management around them (the MySQL-compatible family, the PostgreSQL-compatible
//! family); every other engine is a different database, and the restore is refused BEFORE
//! any instance is created rather than producing an instance whose engine cannot read its
//! data.
//!
//! The table below spans what the rule generalises over: a request that names no engine at
//! all, one that names the source's own, one that names another engine of the same family
//! (in both directions), and the incompatible pairs across the two families and into an
//! engine family this emulator serves no wire for.
//!
//! Nothing here waits out a span: every state a transition this service owes leads to is
//! reached by polling a REPORTING call through `lifecycle_poll_loops::poll_until`.

use crate::db_instance_automated_backups::{available_instance, describe_instance};
use crate::db_instances::{call, emulator, expect_error, expect_ok, optional_text, text};
use crate::lifecycle_poll_loops::poll_until;

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The `awsQueryError` code the Query API answers members that cannot hold together with.
const INVALID_PARAMETER_COMBINATION: &str = "InvalidParameterCombination";

/// One row: the engine the snapshot was taken of, the engine the restore asks for, and
/// whether the restore is served.
struct EngineCase {
    case: &'static str,
    /// The engine of the source instance the snapshot is taken of.
    source_engine: &'static str,
    /// What the restore sends for `Engine`, or `None` to leave the member out.
    requested_engine: Option<&'static str>,
    /// Whether the restore is served; `false` is the documented constraint refusing it.
    served: bool,
}

const TABLE: &[EngineCase] = &[
    EngineCase {
        case: "no engine named at all, which takes the source's",
        source_engine: "postgres",
        requested_engine: None,
        served: true,
    },
    EngineCase {
        case: "the source's own engine",
        source_engine: "postgres",
        requested_engine: Some("postgres"),
        served: true,
    },
    EngineCase {
        case: "the source's own engine in another case",
        source_engine: "mysql",
        requested_engine: Some("MySQL"),
        served: true,
    },
    EngineCase {
        case: "MariaDB from a MySQL snapshot, the example the member documents",
        source_engine: "mysql",
        requested_engine: Some("mariadb"),
        served: true,
    },
    EngineCase {
        case: "MySQL from a MariaDB snapshot, the same family the other way",
        source_engine: "mariadb",
        requested_engine: Some("mysql"),
        served: true,
    },
    EngineCase {
        case: "PostgreSQL asked of a MySQL snapshot",
        source_engine: "mysql",
        requested_engine: Some("postgres"),
        served: false,
    },
    EngineCase {
        case: "MySQL asked of a PostgreSQL snapshot",
        source_engine: "postgres",
        requested_engine: Some("mysql"),
        served: false,
    },
    EngineCase {
        case: "an engine of another family entirely",
        source_engine: "postgres",
        requested_engine: Some("oracle-se2"),
        served: false,
    },
];

/// The sentence the refusal carries: fixed text, with the engine the request asked for and
/// the engine the snapshot was taken of as its variable parts.
fn expected_message(requested: &str, source: &str) -> String {
    format!(
        "The engine {requested} isn't compatible with the engine of the source DB snapshot \
         ({source})."
    )
}

#[tokio::test]
async fn a_restore_takes_only_an_engine_the_snapshots_own_engine_is_compatible_with() {
    for (index, row) in TABLE.iter().enumerate() {
        // Each row gets its own emulator, so one row's records cannot carry into the next.
        let router = emulator();
        let source = format!("emu-reng-src-{index}");
        let snapshot = format!("{source}-snap");
        let target = format!("emu-reng-restored-{index}");
        available_instance(&router, row.case, &source, row.source_engine, &[]).await;
        expect_ok(
            row.case,
            call(
                &router,
                "CreateDBSnapshot",
                &params![
                    "DBInstanceIdentifier" => &source,
                    "DBSnapshotIdentifier" => &snapshot,
                ],
            )
            .await,
        );
        // Only an available snapshot can be restored, and it becomes available on its own:
        // the CONDITION is polled for.
        let describe_snapshot_request = params!["DBSnapshotIdentifier" => &snapshot].to_vec();
        poll_until(
            row.case,
            &format!("{snapshot} reaching available"),
            || call(&router, "DescribeDBSnapshots", &describe_snapshot_request),
            |status, body| {
                status == 200 && optional_text(body, "Status").as_deref() == Some("available")
            },
        )
        .await;

        let mut request: Vec<(&str, String)> = params![
            "DBInstanceIdentifier" => &target,
            "DBSnapshotIdentifier" => &snapshot,
        ]
        .to_vec();
        if let Some(engine) = row.requested_engine {
            request.push(("Engine", engine.to_string()));
        }
        let answer = call(&router, "RestoreDBInstanceFromDBSnapshot", &request).await;
        if !row.served {
            let requested = row
                .requested_engine
                .expect("a refused row names the engine it asked for");
            let refused =
                expect_error(row.case, answer, 400, INVALID_PARAMETER_COMBINATION);
            let message = text(&refused, "Message");
            assert_eq!(
                message,
                expected_message(requested, row.source_engine),
                "case {:?}: expected the compatibility refusal, got {message:?}",
                row.case
            );
            // Refused BEFORE anything was created: nothing answers to the target name.
            let (status, body) = describe_instance(&router, &target).await;
            assert_eq!(
                status, 404,
                "case {:?}: a refused restore creates no instance, got HTTP {status} with \
                 {body}",
                row.case
            );
            continue;
        }

        let restored = expect_ok(row.case, answer);
        let reported_engine = text(&restored, "Engine");
        let expected_engine = row.requested_engine.unwrap_or(row.source_engine);
        assert!(
            reported_engine.eq_ignore_ascii_case(expected_engine),
            "case {:?}: expected the restored instance to run {expected_engine:?}, got \
             {reported_engine:?}",
            row.case
        );
        // And it really is a DB instance of this service: the read-back reports it under
        // the same engine once it has come up.
        let (described, _) = poll_until(
            row.case,
            &format!("{target} reaching available"),
            || describe_instance(&router, &target),
            |status, body| {
                status == 200
                    && optional_text(body, "DBInstanceStatus").as_deref() == Some("available")
            },
        )
        .await;
        let read_back = text(&described, "Engine");
        assert!(
            read_back.eq_ignore_ascii_case(expected_engine),
            "case {:?}: expected the stored instance to run {expected_engine:?}, got \
             {read_back:?}",
            row.case
        );
    }
}
