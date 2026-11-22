// SPDX-License-Identifier: CC0-1.0

//! The CHAINS a client drives with a poll loop: a resource this service provisions FROM
//! another resource of its own — a snapshot of a DB instance, a copy of a snapshot, a member
//! of a DB cluster, an export of a snapshot — and the statuses a polling client reads off
//! each level of such a chain.
//!
//! The rule these pin: the span the clock owes a resource's provisioning runs from the
//! instant the resource it WAITS ON became ready, so a chain costs one span per DEPENDENCY
//! LEVEL counted from its root rather than one span per call. A client that polls each level
//! to its terminal status therefore reaches the end of the chain in bounded time — by the
//! time it has polled one level ready, which costs it a poll of its own, the next level's
//! span has already elapsed — while a caller that creates a source and its derivative back to
//! back still sees the derivative in its transient status and gets the state errors that go
//! with it.
//!
//! An export TASK is the one piece of WORK here rather than a resource provisioned from
//! another: the source it reads is already available when the export is accepted, so its
//! progression is charged from the CALL that started it — one span per step of it — which is
//! what leaves a client polling the task a transient status to see.
//!
//! Beside them, the two reports a client reads a settled resource's own outcome off:
//!
//! * `PendingModifiedValues` is reported on EVERY DB instance, carrying what the instance
//!   still has pending and EMPTY — the form that identifies no change — once it has nothing
//!   pending, so a client that applied a modification can tell an applied change from a
//!   parked one.
//! * a not-found refusal of an automated backup names the SOURCE INSTANCE's `DbiResourceId`,
//!   the id a backup is identified by, whichever member the request named the backup by: the
//!   id is read back out of the automated-backups ARN, which is a function of it.
//!
//! The rows span what the rules generalise over: each kind of source a derived resource waits
//! on (a DB instance, a DB snapshot, a DB cluster, a DB cluster snapshot), both engine
//! families of a cluster, the deferred and the applied-at-once forms of a modification, and
//! both members a request names an automated backup by.
//!
//! Every wait here is a poll loop over a REPORTING call
//! ([`crate::lifecycle_poll_loops::poll_until`]) — nothing sleeps and no assertion is on how
//! fast the host runs — and every call goes over the wire through the harness
//! `db_instances.rs` keeps, so each assertion reads the XML the emulator itself answered.

use std::sync::Arc;
use std::time::Instant;

use service_com_amazonaws_rds_amazonrdsv19::state as st;
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{
    call, element_is_empty, emulator_with_engine, expect_error, expect_ok, subtrees, text, texts,
    REGION,
};
use crate::lifecycle_poll_loops::poll_until;

/// The master user every resource below is created with, and its password — test credentials,
/// and the only place in this suite they appear.
const MASTER_USER: &str = "emuadmin";
const MASTER_PASSWORD: &str = "emu-Secret-99";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// This service's store, as the handlers reach it: where the instant the clock makes a
/// resource ready is recorded.
fn store(
    engine: &Arc<engine::InMemoryEngine>,
) -> engine::ServiceState<st::ComAmazonawsRdsAmazonrdsv19State> {
    st::service_state(&**engine).expect("this service's store")
}

/// Which stored record a chain level is, so one table can read the ready instant of any of
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Instance,
    Snapshot,
    Cluster,
    ClusterSnapshot,
}

/// The instant the clock makes — or made — the record `identifier` names ready.
fn ready_at(
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    level: Level,
    identifier: &str,
) -> Instant {
    let state = store(engine);
    let store = state.lock();
    let key = (REGION.to_string(), identifier.to_string());
    match level {
        Level::Instance => store.db_instances.get(&key).map(|record| record.ready_at),
        Level::Snapshot => store.db_snapshots.get(&key).map(|record| record.ready_at),
        Level::Cluster => store.db_clusters.get(&key).map(|record| record.ready_at),
        Level::ClusterSnapshot => store
            .db_cluster_snapshots
            .get(&key)
            .map(|record| record.ready_at),
    }
    .unwrap_or_else(|| panic!("case {case:?}: the store holds {identifier} as a {level:?}"))
}

/// Poll a REPORTING call until `settled` holds of the one `<element>` subtree it answers
/// with, and hand that subtree back.
async fn poll_element(
    router: &MultiServiceRouter,
    case: &str,
    what: &str,
    action: &str,
    request: &[(&str, String)],
    element: &str,
    settled: impl Fn(&str) -> bool,
) -> String {
    let (body, _polls) = poll_until(
        case,
        what,
        || call(router, action, request),
        |status, body| {
            status == 200
                && subtrees(body, element)
                    .first()
                    .is_some_and(|reported| settled(reported))
        },
    )
    .await;
    subtrees(&body, element)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: {what} answered no {element}"))
}

/// Poll a DB instance to `available`.
async fn available_instance(router: &MultiServiceRouter, case: &str, identifier: &str) {
    let request = params!["DBInstanceIdentifier" => identifier];
    poll_element(
        router,
        case,
        &format!("the DB instance {identifier} reaching available"),
        "DescribeDBInstances",
        &request,
        "DBInstance",
        |instance| text(instance, "DBInstanceStatus") == st::DB_INSTANCE_STATUS_AVAILABLE,
    )
    .await;
}

/// Poll a DB cluster to `available`.
async fn available_cluster(router: &MultiServiceRouter, case: &str, identifier: &str) {
    let request = params!["DBClusterIdentifier" => identifier];
    poll_element(
        router,
        case,
        &format!("the DB cluster {identifier} reaching available"),
        "DescribeDBClusters",
        &request,
        "DBCluster",
        |cluster| text(cluster, "Status") == st::DB_CLUSTER_STATUS_AVAILABLE,
    )
    .await;
}

/// Poll a DB snapshot to `available`.
async fn available_snapshot(router: &MultiServiceRouter, case: &str, identifier: &str) {
    let request = params!["DBSnapshotIdentifier" => identifier];
    poll_element(
        router,
        case,
        &format!("the DB snapshot {identifier} reaching available"),
        "DescribeDBSnapshots",
        &request,
        "DBSnapshot",
        |snapshot| text(snapshot, "Status") == st::SNAPSHOT_STATUS_AVAILABLE,
    )
    .await;
}

/// Poll a DB cluster snapshot to `available`.
async fn available_cluster_snapshot(router: &MultiServiceRouter, case: &str, identifier: &str) {
    let request = params!["DBClusterSnapshotIdentifier" => identifier];
    poll_element(
        router,
        case,
        &format!("the DB cluster snapshot {identifier} reaching available"),
        "DescribeDBClusterSnapshots",
        &request,
        "DBClusterSnapshot",
        |snapshot| text(snapshot, "Status") == st::SNAPSHOT_STATUS_AVAILABLE,
    )
    .await;
}

/// `CreateDBInstance` for a standalone DB instance of `engine`.
async fn create_instance(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    engine: &str,
    class: &str,
) {
    expect_ok(
        case,
        call(
            router,
            "CreateDBInstance",
            &params![
                "DBInstanceIdentifier" => identifier,
                "Engine" => engine,
                "DBInstanceClass" => class,
                "AllocatedStorage" => 20,
                "MasterUsername" => MASTER_USER,
                "MasterUserPassword" => MASTER_PASSWORD,
            ],
        )
        .await,
    );
}

/// `CreateDBCluster` for a DB cluster of `engine`.
async fn create_cluster(router: &MultiServiceRouter, case: &str, identifier: &str, engine: &str) {
    expect_ok(
        case,
        call(
            router,
            "CreateDBCluster",
            &params![
                "DBClusterIdentifier" => identifier,
                "Engine" => engine,
                "MasterUsername" => MASTER_USER,
                "MasterUserPassword" => MASTER_PASSWORD,
            ],
        )
        .await,
    );
}

/// One row of the derived-resource table: a SOURCE this service provisions from, and the
/// resource provisioned from it.
struct Derived {
    case: &'static str,
    /// The engine the chain runs, which decides which kind of source it has.
    engine: &'static str,
    /// The instance class the chain's instances are ordered in.
    class: &'static str,
    /// What the chain's root is, and what the derivative is.
    source: Level,
    derivative: Level,
    /// The identifiers the chain's two levels are created under.
    source_identifier: &'static str,
    derivative_identifier: &'static str,
}

/// Each kind of source a derived resource of this service waits on, for both cluster engine
/// families: a snapshot of a DB instance, a copy of that snapshot, a member of a DB cluster,
/// a snapshot of a DB cluster and a copy of that cluster snapshot.
const DERIVED: &[Derived] = &[
    Derived {
        case: "a DB snapshot of a DB instance",
        engine: "mysql",
        class: "db.t3.micro",
        source: Level::Instance,
        derivative: Level::Snapshot,
        source_identifier: "emu-derived-instance",
        derivative_identifier: "emu-derived-snapshot",
    },
    Derived {
        case: "a copy of a DB snapshot",
        engine: "postgres",
        class: "db.t3.micro",
        source: Level::Snapshot,
        derivative: Level::Snapshot,
        source_identifier: "emu-derived-copy-source",
        derivative_identifier: "emu-derived-copy",
    },
    Derived {
        case: "a member of an Aurora MySQL DB cluster",
        engine: "aurora-mysql",
        class: "db.r5.large",
        source: Level::Cluster,
        derivative: Level::Instance,
        source_identifier: "emu-derived-mysql-cluster",
        derivative_identifier: "emu-derived-mysql-member",
    },
    Derived {
        case: "a snapshot of an Aurora PostgreSQL DB cluster",
        engine: "aurora-postgresql",
        class: "db.r5.large",
        source: Level::Cluster,
        derivative: Level::ClusterSnapshot,
        source_identifier: "emu-derived-pg-cluster",
        derivative_identifier: "emu-derived-pg-cluster-snapshot",
    },
    Derived {
        case: "a copy of a DB cluster snapshot",
        engine: "aurora-mysql",
        class: "db.r5.large",
        source: Level::ClusterSnapshot,
        derivative: Level::ClusterSnapshot,
        source_identifier: "emu-derived-cluster-copy-source",
        derivative_identifier: "emu-derived-cluster-copy",
    },
];

/// Stand the row's SOURCE up, polled to the status its derivative is provisioned from, and
/// answer the identifier the derivative is created from.
async fn source_of(router: &MultiServiceRouter, row: &Derived, index: usize) -> String {
    match row.source {
        Level::Instance => {
            create_instance(
                router,
                row.case,
                row.source_identifier,
                row.engine,
                row.class,
            )
            .await;
            available_instance(router, row.case, row.source_identifier).await;
        }
        Level::Cluster => {
            create_cluster(router, row.case, row.source_identifier, row.engine).await;
            available_cluster(router, row.case, row.source_identifier).await;
        }
        // A snapshot source is itself derived from the instance or cluster it is taken of,
        // which this stands up first: the chain's own root.
        Level::Snapshot => {
            let root = format!("emu-derived-root-{index}");
            create_instance(router, row.case, &root, row.engine, row.class).await;
            available_instance(router, row.case, &root).await;
            expect_ok(
                row.case,
                call(
                    router,
                    "CreateDBSnapshot",
                    &params![
                        "DBInstanceIdentifier" => root,
                        "DBSnapshotIdentifier" => row.source_identifier,
                    ],
                )
                .await,
            );
            available_snapshot(router, row.case, row.source_identifier).await;
        }
        Level::ClusterSnapshot => {
            let root = format!("emu-derived-root-cluster-{index}");
            create_cluster(router, row.case, &root, row.engine).await;
            available_cluster(router, row.case, &root).await;
            expect_ok(
                row.case,
                call(
                    router,
                    "CreateDBClusterSnapshot",
                    &params![
                        "DBClusterIdentifier" => root,
                        "DBClusterSnapshotIdentifier" => row.source_identifier,
                    ],
                )
                .await,
            );
            available_cluster_snapshot(router, row.case, row.source_identifier).await;
        }
    }
    row.source_identifier.to_string()
}

/// Create the row's DERIVATIVE from `source`.
async fn create_derivative(router: &MultiServiceRouter, row: &Derived, source: &str) {
    let (action, request) = match (row.source, row.derivative) {
        (Level::Instance, Level::Snapshot) => (
            "CreateDBSnapshot",
            params![
                "DBInstanceIdentifier" => source,
                "DBSnapshotIdentifier" => row.derivative_identifier,
            ]
            .to_vec(),
        ),
        (Level::Snapshot, Level::Snapshot) => (
            "CopyDBSnapshot",
            params![
                "SourceDBSnapshotIdentifier" => source,
                "TargetDBSnapshotIdentifier" => row.derivative_identifier,
            ]
            .to_vec(),
        ),
        (Level::Cluster, Level::Instance) => (
            "CreateDBInstance",
            params![
                "DBInstanceIdentifier" => row.derivative_identifier,
                "DBClusterIdentifier" => source,
                "Engine" => row.engine,
                "DBInstanceClass" => row.class,
            ]
            .to_vec(),
        ),
        (Level::Cluster, Level::ClusterSnapshot) => (
            "CreateDBClusterSnapshot",
            params![
                "DBClusterIdentifier" => source,
                "DBClusterSnapshotIdentifier" => row.derivative_identifier,
            ]
            .to_vec(),
        ),
        (Level::ClusterSnapshot, Level::ClusterSnapshot) => (
            "CopyDBClusterSnapshot",
            params![
                "SourceDBClusterSnapshotIdentifier" => source,
                "TargetDBClusterSnapshotIdentifier" => row.derivative_identifier,
            ]
            .to_vec(),
        ),
        (source, derivative) => {
            panic!(
                "case {:?}: this suite drives no {source:?} -> {derivative:?} chain",
                row.case
            )
        }
    };
    let request: Vec<(&str, String)> = request;
    expect_ok(row.case, call(router, action, &request).await);
}

/// Poll the row's derivative to the terminal status its own report publishes.
async fn settle_derivative(router: &MultiServiceRouter, row: &Derived) {
    match row.derivative {
        Level::Instance => available_instance(router, row.case, row.derivative_identifier).await,
        Level::Snapshot => available_snapshot(router, row.case, row.derivative_identifier).await,
        Level::Cluster => available_cluster(router, row.case, row.derivative_identifier).await,
        Level::ClusterSnapshot => {
            available_cluster_snapshot(router, row.case, row.derivative_identifier).await
        }
    }
}

/// Rule: a resource this service provisions FROM another resource of its own is ready one
/// span after that source was — the span runs from the instant the work it waits on became
/// available, so a chain costs one span per dependency level counted from its root rather
/// than one span per call — and a client that polls each level with reporting calls alone
/// reaches the terminal status of every level.
#[tokio::test]
async fn a_derived_resource_is_ready_one_span_after_the_source_it_waits_on() {
    for (index, row) in DERIVED.iter().enumerate() {
        // One emulator per row, so the identifiers of one chain cannot collide with another's.
        let (router, engine) = emulator_with_engine();
        let source = source_of(&router, row, index).await;
        let source_ready = ready_at(&engine, row.case, row.source, &source);

        create_derivative(&router, row, &source).await;
        let derivative_ready =
            ready_at(&engine, row.case, row.derivative, row.derivative_identifier);
        assert_eq!(
            derivative_ready,
            source_ready + st::STATUS_TRANSITION_DELAY,
            "case {:?}: the derivative must be ready one span after the source it waits on \
             ({source_ready:?} + {:?}), got {derivative_ready:?}",
            row.case,
            st::STATUS_TRANSITION_DELAY
        );
        // And a client that only REPORTS reaches the derivative's terminal status.
        settle_derivative(&router, row).await;
    }
}

/// Rule: the chain `CopyDBSnapshot` sits at the end of converges for a client that drives it
/// with poll loops alone — the instance, the snapshot of it and the copy of that snapshot each
/// reach `available`, the copy is an independent resource with its own identifier and ARN, and
/// deleting the source leaves the copy available.
#[tokio::test]
async fn a_polling_client_drives_a_snapshot_copy_round_trip_to_its_end() {
    let case = "the CopyDBSnapshot round trip";
    let (router, _engine) = emulator_with_engine();
    create_instance(&router, case, "emu-round-trip", "mysql", "db.t3.micro").await;
    available_instance(&router, case, "emu-round-trip").await;

    expect_ok(
        case,
        call(
            &router,
            "CreateDBSnapshot",
            &params![
                "DBInstanceIdentifier" => "emu-round-trip",
                "DBSnapshotIdentifier" => "emu-round-trip-snapshot",
            ],
        )
        .await,
    );
    available_snapshot(&router, case, "emu-round-trip-snapshot").await;

    let answered = expect_ok(
        case,
        call(
            &router,
            "CopyDBSnapshot",
            &params![
                "SourceDBSnapshotIdentifier" => "emu-round-trip-snapshot",
                "TargetDBSnapshotIdentifier" => "emu-round-trip-copy",
            ],
        )
        .await,
    );
    // A copy AWS answers in progress is not reported finished: the status the copy carries
    // when the call answers is the transient one.
    let answered_status = text(&answered, "Status");
    assert_eq!(
        answered_status, st::SNAPSHOT_STATUS_COPYING,
        "case {case:?}: expected the accepted copy to answer the transient status, got \
         {answered_status:?}"
    );

    let copy = poll_element(
        &router,
        case,
        "the copy reaching available",
        "DescribeDBSnapshots",
        &params!["DBSnapshotIdentifier" => "emu-round-trip-copy"],
        "DBSnapshot",
        |snapshot| text(snapshot, "Status") == st::SNAPSHOT_STATUS_AVAILABLE,
    )
    .await;
    assert_eq!(
        text(&copy, "DBSnapshotIdentifier"),
        "emu-round-trip-copy",
        "case {case:?}: the copy is its own resource, got {copy}"
    );
    assert_eq!(
        text(&copy, "DBSnapshotArn"),
        format!("arn:aws:rds:{REGION}:000000000000:snapshot:emu-round-trip-copy"),
        "case {case:?}: the copy publishes an ARN of its own, got {copy}"
    );
    assert_eq!(
        text(&copy, "Engine"),
        "mysql",
        "case {case:?}: the copy is a snapshot of the same database, got {copy}"
    );

    // Deleting the SOURCE leaves the copy where it is: an independent snapshot.
    expect_ok(
        case,
        call(
            &router,
            "DeleteDBSnapshot",
            &params!["DBSnapshotIdentifier" => "emu-round-trip-snapshot"],
        )
        .await,
    );
    let gone_request = params!["DBSnapshotIdentifier" => "emu-round-trip-snapshot"];
    let (_gone, _polls) = poll_until(
        case,
        "the deleted source snapshot being gone",
        || call(&router, "DescribeDBSnapshots", &gone_request),
        |status, _body| status == 404,
    )
    .await;
    let still = poll_element(
        &router,
        case,
        "the copy staying available after the source is deleted",
        "DescribeDBSnapshots",
        &params!["DBSnapshotIdentifier" => "emu-round-trip-copy"],
        "DBSnapshot",
        |snapshot| text(snapshot, "Status") == st::SNAPSHOT_STATUS_AVAILABLE,
    )
    .await;
    assert_eq!(
        text(&still, "DBSnapshotIdentifier"),
        "emu-round-trip-copy",
        "case {case:?}: the copy survives its source, got {still}"
    );
}

/// Rule: a DB cluster whose members a client created converges back to `available` after a
/// failover, and a second failover issued while the first is still in flight is refused with
/// the modelled cluster state fault — exactly one of two concurrent failovers is performed.
#[tokio::test]
async fn a_cluster_a_client_failed_over_converges_back_to_available() {
    let case = "two failovers on one cluster";
    let (router, _engine) = emulator_with_engine();
    create_cluster(&router, case, "emu-failover-chain", "aurora-mysql").await;
    available_cluster(&router, case, "emu-failover-chain").await;
    for member in ["emu-failover-writer", "emu-failover-reader"] {
        expect_ok(
            case,
            call(
                &router,
                "CreateDBInstance",
                &params![
                    "DBInstanceIdentifier" => member,
                    "DBClusterIdentifier" => "emu-failover-chain",
                    "Engine" => "aurora-mysql",
                    "DBInstanceClass" => "db.r5.large",
                ],
            )
            .await,
        );
    }
    for member in ["emu-failover-writer", "emu-failover-reader"] {
        available_instance(&router, case, member).await;
    }

    let accepted = expect_ok(
        case,
        call(
            &router,
            "FailoverDBCluster",
            &params!["DBClusterIdentifier" => "emu-failover-chain"],
        )
        .await,
    );
    let status = text(&accepted, "Status");
    assert_eq!(
        status,
        st::DB_CLUSTER_STATUS_FAILING_OVER,
        "case {case:?}: expected the accepted failover to answer the transient status, got \
         {status:?}"
    );
    // A second failover while the first is in flight is refused: the cluster is not in a
    // state one can be performed in.
    let refused = expect_error(
        case,
        call(
            &router,
            "FailoverDBCluster",
            &params!["DBClusterIdentifier" => "emu-failover-chain"],
        )
        .await,
        400,
        "InvalidDBClusterStateFault",
    );
    let message = text(&refused, "Message");
    assert!(
        message.contains("emu-failover-chain")
            && message.contains(st::DB_CLUSTER_STATUS_FAILING_OVER),
        "case {case:?}: the refusal must name the cluster and the state it is in, got \
         {message:?}"
    );

    // And the cluster comes back on its own, for a client that only polls.
    available_cluster(&router, case, "emu-failover-chain").await;
}

/// Rule: an export task's progression runs on the clock of the CALL that started it — the
/// step that has the task writing is due one span after `StartExportTask`, and the `COMPLETE`
/// it ends in one span after that — so a client that polls `DescribeExportTasks` straight
/// after the start still finds the task on its way rather than already finished, which is what
/// the progression a client polls for is for.
#[tokio::test]
async fn an_export_task_charges_its_steps_from_the_call_that_started_it() {
    let case = "an export of a settled snapshot";
    let (router, engine) = crate::rds_s3_integration::emulator();
    let bucket = "emu-derived-export-bucket";
    crate::rds_s3_integration::create_bucket(&router, bucket, REGION).await;
    let key_id = crate::rds_s3_integration::kms(&router)
        .create_key()
        .description("the export's encryption key")
        .send()
        .await
        .expect("the emulated KMS creates the export key")
        .key_metadata()
        .map(|metadata| metadata.key_id().to_string())
        .expect("the created key reports its id");
    let role_arn = crate::rds_s3_integration::role(
        &router,
        "emu-derived-export-role",
        st::EXPORT_SERVICE_PRINCIPAL,
        Some(crate::rds_s3_integration::export_role_policy(bucket)),
    )
    .await;

    create_instance(&router, case, "emu-derived-export", "mysql", "db.t3.micro").await;
    available_instance(&router, case, "emu-derived-export").await;
    expect_ok(
        case,
        call(
            &router,
            "CreateDBSnapshot",
            &params![
                "DBInstanceIdentifier" => "emu-derived-export",
                "DBSnapshotIdentifier" => "emu-derived-export-snapshot",
            ],
        )
        .await,
    );
    available_snapshot(&router, case, "emu-derived-export-snapshot").await;
    let snapshot_ready = ready_at(
        &engine,
        case,
        Level::Snapshot,
        "emu-derived-export-snapshot",
    );

    // The call is BRACKETED, so the instant its step is due is read against the instants the
    // call itself lies between rather than against a clock reading taken elsewhere.
    let before_the_call = Instant::now();
    let started = expect_ok(
        case,
        call(
            &router,
            "StartExportTask",
            &params![
                "ExportTaskIdentifier" => "emu-derived-export-task",
                "SourceArn" => format!(
                    "arn:aws:rds:{REGION}:000000000000:snapshot:emu-derived-export-snapshot"
                ),
                "S3BucketName" => bucket,
                "IamRoleArn" => role_arn,
                "KmsKeyId" => key_id,
            ],
        )
        .await,
    );
    let after_the_call = Instant::now();
    let answered = text(&started, "Status");
    assert_eq!(
        answered,
        st::EXPORT_TASK_STATUS_STARTING,
        "case {case:?}: expected the started task to answer {}, got {answered:?}",
        st::EXPORT_TASK_STATUS_STARTING
    );

    // The step the task waits on is due one span after the CALL that started it — not after
    // the snapshot it reads became ready, which is already behind it, and which would leave a
    // client that takes a span to ask its first question no transient status to see.
    let due_at = {
        let state = store(&engine);
        let store = state.lock();
        let record = store
            .export_tasks
            .get(&(REGION.to_string(), "emu-derived-export-task".to_string()))
            .unwrap_or_else(|| panic!("case {case:?}: the store holds the started export task"));
        record
            .pending_status
            .as_ref()
            .map(|pending| pending.due_at)
            .unwrap_or_else(|| panic!("case {case:?}: the started task owes the clock a step"))
    };
    assert!(
        due_at >= before_the_call + st::STATUS_TRANSITION_DELAY
            && due_at <= after_the_call + st::STATUS_TRANSITION_DELAY,
        "case {case:?}: the task's first step must be due one span ({:?}) after the call that \
         started it, which ran between {before_the_call:?} and {after_the_call:?}, got {due_at:?}",
        st::STATUS_TRANSITION_DELAY
    );
    assert!(
        due_at > snapshot_ready + st::STATUS_TRANSITION_DELAY,
        "case {case:?}: the step is charged from the call and not from the readiness of the \
         snapshot it reads ({snapshot_ready:?}), got {due_at:?}"
    );

    // And a client that only REPORTS reaches a status it can act on.
    let task_request = params!["ExportTaskIdentifier" => "emu-derived-export-task"];
    let acted_on: Vec<&str> = st::TERMINAL_STATUSES
        .iter()
        .copied()
        .chain([st::EXPORT_TASK_STATUS_IN_PROGRESS])
        .collect();
    let reported = poll_element(
        &router,
        case,
        "the export task leaving STARTING",
        "DescribeExportTasks",
        &task_request,
        "ExportTask",
        |task| acted_on.contains(&text(task, "Status").as_str()),
    )
    .await;
    let status = text(&reported, "Status");
    assert!(
        acted_on.contains(&status.as_str()),
        "case {case:?}: expected one of {acted_on:?}, got {status:?}"
    );
}

/// One row of the pending-values table: what a modification asks for, and whether the
/// instance is left with something pending once it is settled.
struct Modification {
    case: &'static str,
    identifier: &'static str,
    engine: &'static str,
    /// The members the modification carries beyond `DBInstanceIdentifier`.
    request: &'static [(&'static str, &'static str)],
    /// Whether the settled instance still has a change outstanding.
    leaves_pending: bool,
}

/// The two forms of a modification, plus the instance nothing has modified at all.
const MODIFICATIONS: &[Modification] = &[
    Modification {
        case: "an instance nothing has modified",
        identifier: "emu-pending-untouched",
        engine: "postgres",
        request: &[],
        leaves_pending: false,
    },
    Modification {
        case: "a modification applied at once",
        identifier: "emu-pending-at-once",
        engine: "postgres",
        request: &[
            ("DBInstanceClass", "db.t3.small"),
            ("AllocatedStorage", "25"),
            ("ApplyImmediately", "true"),
        ],
        leaves_pending: false,
    },
    Modification {
        case: "a modification left for the maintenance window",
        identifier: "emu-pending-deferred",
        engine: "mysql",
        request: &[
            ("DBInstanceClass", "db.t3.small"),
            ("ApplyImmediately", "false"),
        ],
        leaves_pending: true,
    },
];

/// Rule: `PendingModifiedValues` is reported on EVERY DB instance — the element is always
/// there, carrying the changes the instance still has outstanding and EMPTY, the form that
/// identifies no change, once it has none. A modification applied at once leaves it empty; one
/// parked for the maintenance window leaves the parked settings in it.
#[tokio::test]
async fn every_db_instance_reports_what_it_has_pending_and_the_empty_element_when_it_has_none() {
    let (router, _engine) = emulator_with_engine();
    for row in MODIFICATIONS {
        create_instance(&router, row.case, row.identifier, row.engine, "db.t3.micro").await;
        available_instance(&router, row.case, row.identifier).await;
        if !row.request.is_empty() {
            let mut request = params!["DBInstanceIdentifier" => row.identifier].to_vec();
            for (member, value) in row.request {
                request.push((member, (*value).to_string()));
            }
            expect_ok(row.case, call(&router, "ModifyDBInstance", &request).await);
        }
        // The instance is read back once it is settled again, through reporting calls alone.
        let instance = poll_element(
            &router,
            row.case,
            &format!("{} reaching available again", row.identifier),
            "DescribeDBInstances",
            &params!["DBInstanceIdentifier" => row.identifier],
            "DBInstance",
            |instance| text(instance, "DBInstanceStatus") == st::DB_INSTANCE_STATUS_AVAILABLE,
        )
        .await;
        let reported = subtrees(&instance, "PendingModifiedValues");
        assert_eq!(
            reported.len(),
            1,
            "case {:?}: every DB instance reports the pending element, got {instance}",
            row.case
        );
        let pending = reported.into_iter().next().expect("checked just above");
        let identifies_a_change = !element_is_empty(&pending, "PendingModifiedValues");
        assert_eq!(
            identifies_a_change, row.leaves_pending,
            "case {:?}: expected the pending element to identify a change: \
             {}, got {pending:?}",
            row.case, row.leaves_pending
        );
        if row.leaves_pending {
            let staged = text(&pending, "DBInstanceClass");
            assert_eq!(
                staged, "db.t3.small",
                "case {:?}: expected the parked class in the pending element, got {pending:?}",
                row.case
            );
        }
        // A change applied at once is also IN FORCE on the instance itself.
        if row.request.iter().any(|(member, _)| *member == "AllocatedStorage") {
            let storage = text(&instance, "AllocatedStorage");
            assert_eq!(
                storage, "25",
                "case {:?}: expected the applied storage on the instance, got {instance}",
                row.case
            );
        }
    }
}

/// One row of the automated-backup table: which member names the backup.
struct BackupReference {
    case: &'static str,
    /// Whether the request names the backup by its ARN rather than by the resource id.
    by_arn: bool,
}

/// Both members a request names ONE automated backup by.
const BACKUP_REFERENCES: &[BackupReference] = &[
    BackupReference {
        case: "named by DbiResourceId",
        by_arn: false,
    },
    BackupReference {
        case: "named by DBInstanceAutomatedBackupsArn",
        by_arn: true,
    },
];

/// Rule: the retained automated backup of a deleted DB instance is describable and deletable,
/// and a not-found refusal of one names the SOURCE INSTANCE's `DbiResourceId` — the id a
/// backup is identified by — whichever member the request named it by, since the
/// automated-backups ARN is a function of that one id.
#[tokio::test]
async fn an_automated_backup_refusal_names_the_source_instances_resource_id() {
    for (index, row) in BACKUP_REFERENCES.iter().enumerate() {
        let (router, _engine) = emulator_with_engine();
        let identifier = format!("emu-retained-backup-{index}");
        create_instance(&router, row.case, &identifier, "mysql", "db.t3.micro").await;
        available_instance(&router, row.case, &identifier).await;
        let described = poll_element(
            &router,
            row.case,
            "the instance reporting its resource id",
            "DescribeDBInstances",
            &params!["DBInstanceIdentifier" => identifier],
            "DBInstance",
            |instance| !text(instance, "DbiResourceId").is_empty(),
        )
        .await;
        let resource_id = text(&described, "DbiResourceId");

        // The delete KEEPS the automated backup, and the instance then converges out of
        // existence on its own.
        expect_ok(
            row.case,
            call(
                &router,
                "DeleteDBInstance",
                &params![
                    "DBInstanceIdentifier" => identifier,
                    "SkipFinalSnapshot" => true,
                    "DeleteAutomatedBackups" => false,
                ],
            )
            .await,
        );
        let gone_request = params!["DBInstanceIdentifier" => identifier];
        let (_gone, _polls) = poll_until(
            row.case,
            "the deleted instance being gone",
            || call(&router, "DescribeDBInstances", &gone_request),
            |status, _body| status == 404,
        )
        .await;

        // The retained backup is the one the deleted instance left behind.
        let backup = poll_element(
            &router,
            row.case,
            "the retained automated backup appearing",
            "DescribeDBInstanceAutomatedBackups",
            &params!["DbiResourceId" => resource_id],
            "DBInstanceAutomatedBackup",
            |backup| text(backup, "DbiResourceId") == resource_id,
        )
        .await;
        let backups_arn = text(&backup, "DBInstanceAutomatedBackupsArn");
        assert_eq!(
            backups_arn,
            st::instance_automated_backups_arn(REGION, &resource_id),
            "case {:?}: the retained backup publishes the ARN of the source instance's \
             resource id, got {backup}",
            row.case
        );

        // Deleting it by either member removes it, and a repeat is the modelled not-found
        // refusal — worded with the source instance's resource id either way.
        let named = if row.by_arn {
            params!["DBInstanceAutomatedBackupsArn" => backups_arn].to_vec()
        } else {
            params!["DbiResourceId" => resource_id].to_vec()
        };
        expect_ok(
            row.case,
            call(&router, "DeleteDBInstanceAutomatedBackup", &named).await,
        );
        let refused = expect_error(
            row.case,
            call(&router, "DeleteDBInstanceAutomatedBackup", &named).await,
            404,
            "DBInstanceAutomatedBackupNotFound",
        );
        let message = text(&refused, "Message");
        assert_eq!(
            message,
            format!("Automated backups could not be found for: {resource_id}"),
            "case {:?}: expected the refusal to name the source instance's resource id, got \
             {message:?}",
            row.case
        );
        // And the report no longer lists it.
        let empty = expect_ok(
            row.case,
            call::<&str, &str>(&router, "DescribeDBInstanceAutomatedBackups", &[]).await,
        );
        assert!(
            !texts(&empty, "DbiResourceId").contains(&resource_id),
            "case {:?}: a deleted automated backup is not listed, got {empty}",
            row.case
        );
    }
}
