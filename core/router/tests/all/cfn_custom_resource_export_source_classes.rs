// SPDX-License-Identifier: CC0-1.0

//! WHICH resource a stack's `Custom::` resource provider names when it calls into another
//! service, and what the deployment settles at for each.
//!
//! RULE under test: **a deployment of a Lambda-backed `Custom::<Type>` resource always reaches a
//! TERMINAL stack status, whatever the call its provider makes names — `CREATE_COMPLETE` when the
//! owning service SERVES that call, with the `Data` the provider published resolving through
//! `Fn::GetAtt` into the stack's Outputs and the resource the call created really held by that
//! service, and `ROLLBACK_COMPLETE` carrying the provider's own reason on the custom resource's
//! `CREATE_FAILED` event when the service REFUSES it with one of the operation's modelled faults.
//! What the provider's call names decides WHICH answer it meets, never WHETHER it gets one, so
//! one deployment is a single row of that rule.**
//!
//! The rows differ along the class of source an export names, which is what `StartExportTask`
//! models a not-found fault of its own for and reports back as `ExportTask$SourceType`
//! (`smithy_query(query="operation",operation="StartExportTask")`, whose `ExportSourceType`
//! publishes `SNAPSHOT` and `CLUSTER`): a DB instance snapshot, a DB cluster snapshot and a DB
//! cluster, each one Amazon RDS HOLDS and each named under a value it holds nothing under — plus
//! an export task identifier the Region already holds a task under, which is the one refusal
//! that is about the REQUEST rather than about the source. What SENTENCE each fault is worded
//! with is the owning service's own suites' subject; what this suite asserts is that the answer
//! reached the provider, with the `awsQueryError` code its shape carries, and that the stack
//! settled on it.
//!
//! The deployment itself — the template, the provider's own python source, the stack poll and the
//! failure-event reader — is the one this crate already writes for this chain
//! ([`crate::cfn_custom_resource_rds_export_settles`]), reused rather than copied, so what varies
//! between the two suites is only what a trigger NAMES. Nothing here writes to a `ResponseURL`
//! and nothing here starts an export: the provider's own code makes that call through the SDK its
//! runtime supplies, so what each stack settles on is what the emulated services really answered.
//! Every wait is on the CONDITION the next step needs, looked at again until it holds or a
//! deadline far beyond this emulator's own spans passes, so a loaded host cannot decide any
//! outcome here.

use bytes::Bytes;
use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::cfn_custom_resource_rds_export_settles::{
    available_snapshot, deployed, emulator, params, rds, read_rds_until, reported_export_task,
    served, template, Role, Trigger, BUCKET, MASTER_PASSWORD, MASTER_USER, ROLLBACK_COMPLETE,
    TERMINAL_EXPORT_STATUSES, TRIGGER,
};
use crate::cloudformation_completed_stack_output_values::{
    authorization, dispatch, reported_failures, ACCOUNT, CREATE_COMPLETE, REGION,
};
use crate::query_wire::{element, opt_element, outputs_of};

/// The status every served row's export has to have reached once nothing more is owed on it.
const COMPLETE: &str = "COMPLETE";

/// Which class of resource a row's `SourceArn` names.
#[derive(Clone, Copy, Debug)]
enum SourceClass {
    /// A snapshot of a DB instance.
    InstanceSnapshot,
    /// A snapshot of a DB cluster.
    ClusterSnapshot,
    /// A DB cluster itself, read where it stands.
    Cluster,
}

/// The sources one test exports, each created once and polled to `available`: the three classes
/// `SourceArn` can name, as the ARNs Amazon RDS itself minted for them.
#[derive(Debug)]
struct Sources {
    db_snapshot: String,
    db_cluster_snapshot: String,
    db_cluster: String,
}

impl Sources {
    /// The ARN of the source of `class`.
    fn arn_of(&self, class: SourceClass) -> String {
        match class {
            SourceClass::InstanceSnapshot => self.db_snapshot.clone(),
            SourceClass::ClusterSnapshot => self.db_cluster_snapshot.clone(),
            SourceClass::Cluster => self.db_cluster.clone(),
        }
    }
}

/// Create one source of each class under names derived from `tag`, each polled to the state an
/// export reads it in, and answer the ARNs the service reports for them.
async fn sources(router: &MultiServiceRouter, tag: &str) -> Sources {
    let cluster = format!("{tag}-cluster");
    Sources {
        db_snapshot: available_snapshot(
            router,
            &format!("{tag}-instance"),
            &format!("{tag}-snapshot"),
        )
        .await,
        db_cluster: available_cluster(router, &cluster).await,
        db_cluster_snapshot: available_cluster_snapshot(
            router,
            &cluster,
            &format!("{tag}-cluster-snapshot"),
        )
        .await,
    }
}

/// Create the DB cluster `identifier`, poll it to `available`, and answer the ARN Amazon RDS
/// minted for it.
async fn available_cluster(router: &MultiServiceRouter, identifier: &str) -> String {
    let case = "the DB cluster a row exports";
    served(
        case,
        rds(
            router,
            "CreateDBCluster",
            &params![
                "DBClusterIdentifier" => identifier,
                "Engine" => "aurora-mysql",
                "MasterUsername" => MASTER_USER,
                "MasterUserPassword" => MASTER_PASSWORD,
            ],
        )
        .await,
    );
    let described = read_rds_until(
        router,
        case,
        "DescribeDBClusters",
        &params!["DBClusterIdentifier" => identifier],
        "Status",
        &["available"],
    )
    .await;
    let arn = element(&described, "DBClusterArn");
    assert!(
        arn.contains(":cluster:"),
        "case {case:?}: the service reports the cluster's own ARN, got {arn:?} in {described}"
    );
    arn
}

/// Take a snapshot of `cluster`, poll it to `available`, and answer the ARN Amazon RDS minted for
/// it.
async fn available_cluster_snapshot(
    router: &MultiServiceRouter,
    cluster: &str,
    snapshot: &str,
) -> String {
    let case = "the DB cluster snapshot a row exports";
    served(
        case,
        rds(
            router,
            "CreateDBClusterSnapshot",
            &params![
                "DBClusterIdentifier" => cluster,
                "DBClusterSnapshotIdentifier" => snapshot,
            ],
        )
        .await,
    );
    let described = read_rds_until(
        router,
        case,
        "DescribeDBClusterSnapshots",
        &params!["DBClusterSnapshotIdentifier" => snapshot],
        "Status",
        &["available"],
    )
    .await;
    let arn = element(&described, "DBClusterSnapshotArn");
    assert!(
        arn.contains(":cluster-snapshot:"),
        "case {case:?}: the service reports the snapshot's own ARN, got {arn:?} in {described}"
    );
    arn
}

/// How many keys Amazon S3 holds under `prefix` of `bucket`, read through its own listing.
///
/// `ListObjectsV2` reports `KeyCount` as "the number of keys returned with this request", so an
/// answer carrying none, or one carrying something that is no count, is a wire-shape regression
/// rather than an empty prefix: it is reported as itself.
async fn listed_key_count(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    prefix: &str,
) -> i64 {
    let listed = served(
        case,
        dispatch(
            router,
            "GET",
            &format!("s3.{REGION}.amazonaws.com"),
            &format!("/{bucket}?list-type=2&prefix={prefix}%2F"),
            &[("authorization", &authorization("s3"))],
            Bytes::new(),
        )
        .await,
    );
    let reported = opt_element(&listed, "KeyCount");
    reported
        .as_deref()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the listing to report KeyCount, got {listed}")
        })
        .parse()
        .unwrap_or_else(|error| {
            panic!(
                "case {case:?}: expected KeyCount to be a count, got {reported:?} ({error}) in \
                 {listed}"
            )
        })
}

/// One served row: the class of source the trigger names, and what the export Amazon RDS then
/// holds reports about it.
struct Served {
    case: &'static str,
    stack: &'static str,
    task: &'static str,
    source: SourceClass,
    /// `ExportTask$SourceType` for that class: `ExportSourceType` publishes `SNAPSHOT` and
    /// `CLUSTER`, so a snapshot of either kind is exported as the former and a cluster read where
    /// it stands as the latter.
    source_type: &'static str,
}

/// One row per class of source a provider's export can name and the owning service holds.
const SERVED: &[Served] = &[
    Served {
        case: "a snapshot of a DB instance",
        stack: "cfn-export-class-db-snapshot",
        task: "export-class-db-snapshot",
        source: SourceClass::InstanceSnapshot,
        source_type: "SNAPSHOT",
    },
    Served {
        case: "a snapshot of a DB cluster",
        stack: "cfn-export-class-cluster-snapshot",
        task: "export-class-cluster-snapshot",
        source: SourceClass::ClusterSnapshot,
        source_type: "SNAPSHOT",
    },
    Served {
        case: "a DB cluster read where it stands",
        stack: "cfn-export-class-cluster",
        task: "export-class-cluster",
        source: SourceClass::Cluster,
        source_type: "CLUSTER",
    },
];

/// RULE (the served leg, over every class of source): whatever class of resource the provider's
/// export names, the deployment settles at `CREATE_COMPLETE`, the `Data` the provider published
/// is what `Fn::GetAtt` resolves into the stack's Outputs, and the export is one Amazon RDS
/// really holds — reported against the source it was given, reaching a terminal status of its
/// own, with the objects it wrote in the destination bucket.
#[tokio::test]
async fn every_class_of_export_source_completes_its_stack_and_is_held_by_the_service() {
    let router = emulator();
    let sources = sources(&router, "served-class").await;
    for row in SERVED {
        let case = row.case;
        let source = sources.arn_of(row.source);
        let (settled, described) = deployed(
            &router,
            case,
            row.stack,
            template(&Trigger {
                task: row.task.to_string(),
                source: source.clone(),
                bucket: json!({"Ref": BUCKET}),
                role: Role::Export,
            }),
        )
        .await;
        if settled != CREATE_COMPLETE {
            let failures = reported_failures(&router, row.stack).await;
            panic!(
                "case {case:?}: expected the stack to settle at {CREATE_COMPLETE}, got \
                 {settled:?}; the failures it reported: {failures:?}; last answer {described}"
            );
        }

        // 1. The stack's Outputs: the bucket it created, and the attribute the PROVIDER
        //    published, which is the destination the service answered its call with.
        let outputs = outputs_of(&described);
        let bucket = outputs.get("BucketName").cloned().unwrap_or_else(|| {
            panic!("case {case:?}: the stack publishes its bucket, got {outputs:?}")
        });
        assert_eq!(
            outputs.get("ResolvedS3Arn").map(String::as_str),
            Some(format!("arn:aws:s3:::{bucket}").as_str()),
            "case {case:?}: expected Fn::GetAtt of the published attribute to resolve to the ARN \
             of the bucket the export writes into, got {outputs:?}"
        );

        // 2. The export the provider started is one Amazon RDS HOLDS, against the source it was
        //    given and under the class that source belongs to, and it reaches a terminal status
        //    of its own: a client that polls it gets an answer.
        let reported = read_rds_until(
            &router,
            case,
            "DescribeExportTasks",
            &params!["ExportTaskIdentifier" => row.task],
            "Status",
            &TERMINAL_EXPORT_STATUSES,
        )
        .await;
        assert_eq!(
            element(&reported, "Status"),
            COMPLETE,
            "case {case:?}: expected the export the provider started to settle at {COMPLETE}, \
             got {reported}"
        );
        assert_eq!(
            (
                element(&reported, "SourceArn").as_str(),
                element(&reported, "SourceType").as_str(),
            ),
            (source.as_str(), row.source_type),
            "case {case:?}: expected the stored export to report the source the provider's call \
             named and the class it belongs to, got {reported}"
        );
        let held = reported_export_task(&router, case, row.task).await;
        assert_eq!(
            (
                held.get("ExportTaskIdentifier").map(String::as_str),
                held.get("S3Bucket").map(String::as_str),
                held.get("S3Prefix").map(String::as_str),
                held.get("FailureCause").map(String::as_str),
            ),
            (
                Some(row.task),
                Some(bucket.as_str()),
                Some(row.task),
                None,
            ),
            "case {case:?}: expected the stored export to report the identifier, destination and \
             prefix the provider's call named and no failure cause, got {held:?}"
        );

        // 3. ...and the objects that export wrote are in the destination bucket, read through
        //    the service that owns them.
        let key_count = listed_key_count(&router, case, &bucket, row.task).await;
        assert!(
            key_count > 0,
            "case {case:?}: expected the export's own prefix to hold the objects it wrote, got \
             KeyCount {key_count}"
        );
    }
}

/// What a refused row's trigger names as its `SourceArn`.
#[derive(Clone, Copy, Debug)]
enum Named {
    /// A DB cluster snapshot ARN of this Region under a name the account holds nothing under.
    NoSuchDbClusterSnapshot,
    /// A DB cluster ARN of this Region under a name the account holds nothing under.
    NoSuchDbCluster,
    /// The DB instance snapshot the account DOES hold — the row where only the task identifier
    /// is already taken.
    TheHeldDbSnapshot,
}

/// What Amazon RDS has to hold for the refused row's identifier afterwards.
#[derive(Clone, Copy, Debug)]
enum Aftermath {
    /// Nothing: a refused export leaves no task behind, so nothing downstream reports a success
    /// that never happened.
    NoExportTask,
    /// The export that identifier already stood for, exactly as the deployment that started it
    /// left it: a refused call changes nothing about it.
    TheExportItAlreadyStoodFor,
}

/// One refused row: what the trigger names, the `awsQueryError` code the fault that provokes
/// carries, and what the owning service holds for the identifier afterwards.
struct Refused {
    case: &'static str,
    stack: &'static str,
    /// The export task identifier the trigger names.
    task: &'static str,
    source: Named,
    /// The `awsQueryError` code of the fault's own shape, which is what the refusal reaching the
    /// provider carries.
    code: &'static str,
    aftermath: Aftermath,
}

/// The export the first deployment of the refusal test starts, whose identifier the last row
/// then names again.
const TAKEN_TASK: &str = "export-class-taken";

/// One row per refusal a provider's export meets that the served table's sources do not: the
/// two source classes the operation models a not-found fault of its own for, and an identifier
/// the Region already holds a task under.
const REFUSED: &[Refused] = &[
    Refused {
        case: "a DB cluster snapshot this account holds nothing under",
        stack: "cfn-export-class-absent-cluster-snapshot",
        task: "export-class-absent-cluster-snapshot",
        source: Named::NoSuchDbClusterSnapshot,
        code: "DBClusterSnapshotNotFoundFault",
        aftermath: Aftermath::NoExportTask,
    },
    Refused {
        case: "a DB cluster this account holds nothing under",
        stack: "cfn-export-class-absent-cluster",
        task: "export-class-absent-cluster",
        source: Named::NoSuchDbCluster,
        code: "DBClusterNotFoundFault",
        aftermath: Aftermath::NoExportTask,
    },
    Refused {
        case: "an identifier this Region already holds an export under",
        stack: "cfn-export-class-taken-identifier",
        task: TAKEN_TASK,
        source: Named::TheHeldDbSnapshot,
        code: "ExportTaskAlreadyExists",
        aftermath: Aftermath::TheExportItAlreadyStoodFor,
    },
];

/// RULE (the refused leg, over the refusals a source or an identifier can provoke): a deployment
/// whose provider is REFUSED still settles — at `ROLLBACK_COMPLETE`, with the provider's own
/// reason on the custom resource's `CREATE_FAILED` event — and what Amazon RDS holds for the
/// identifier afterwards is what the refusal says: nothing where no export was started, and the
/// export already under it, untouched, where that is why the call was refused.
#[tokio::test]
async fn every_refused_provider_call_rolls_its_stack_back_and_changes_nothing_it_was_refused_for() {
    let router = emulator();
    // Only one source class is needed here: two rows name a source this account holds nothing
    // under, and the third is about the identifier rather than the source.
    let held_snapshot =
        available_snapshot(&router, "refused-class-instance", "refused-class-snapshot").await;

    // The export the last row's identifier is already taken by: started by a deployment of its
    // own, exactly as a client would have it, since nothing in this suite starts an export
    // itself.
    let first = "cfn-export-class-first";
    let (settled, described) = deployed(
        &router,
        "the deployment that takes the identifier",
        first,
        template(&Trigger {
            task: TAKEN_TASK.to_string(),
            source: held_snapshot.clone(),
            bucket: json!({"Ref": BUCKET}),
            role: Role::Export,
        }),
    )
    .await;
    if settled != CREATE_COMPLETE {
        let failures = reported_failures(&router, first).await;
        panic!(
            "setup: expected {first:?} to settle at {CREATE_COMPLETE}, got {settled:?}; the \
             failures it reported: {failures:?}; last answer {described}"
        );
    }
    // Read once it has reached a terminal status of its own, so what the rows below are compared
    // against is the export as it finally stands rather than a status it was still walking
    // through.
    let setup = "the export the identifier stands for";
    read_rds_until(
        &router,
        setup,
        "DescribeExportTasks",
        &params!["ExportTaskIdentifier" => TAKEN_TASK],
        "Status",
        &TERMINAL_EXPORT_STATUSES,
    )
    .await;
    let taken = reported_export_task(&router, setup, TAKEN_TASK).await;
    assert!(
        !taken.is_empty(),
        "setup: expected the first deployment to leave an export under {TAKEN_TASK:?}, got \
         {taken:?}"
    );

    for row in REFUSED {
        let case = row.case;
        let (settled, described) = deployed(
            &router,
            case,
            row.stack,
            template(&Trigger {
                task: row.task.to_string(),
                source: match row.source {
                    Named::NoSuchDbClusterSnapshot => format!(
                        "arn:aws:rds:{REGION}:{ACCOUNT}:cluster-snapshot:export-class-no-such"
                    ),
                    Named::NoSuchDbCluster => {
                        format!("arn:aws:rds:{REGION}:{ACCOUNT}:cluster:export-class-no-such")
                    }
                    Named::TheHeldDbSnapshot => held_snapshot.clone(),
                },
                bucket: json!({"Ref": BUCKET}),
                role: Role::Export,
            }),
        )
        .await;

        // 1. It settles, and on the status a failed create rolls back to — not on a status it is
        //    still working through, and not on no answer at all.
        assert_eq!(
            settled, ROLLBACK_COMPLETE,
            "case {case:?}: expected the stack to settle at {ROLLBACK_COMPLETE}, got {settled:?} \
             with {described}"
        );

        // 2. The reason the custom resource's failure carries is the PROVIDER's, which is how
        //    the refusal the service gave reaches the client that deployed the stack. The shared
        //    reader reports each failure as `<logical id> <status>: <reason>`, so the one this
        //    row asserts on is the custom resource's own.
        let failures = reported_failures(&router, row.stack).await;
        assert!(
            failures
                .iter()
                .any(|failure| failure.starts_with(TRIGGER) && failure.contains(row.code)),
            "case {case:?}: expected the custom resource's own failure to carry the code the \
             refusal was answered with ({}), got {failures:?}",
            row.code
        );

        // 3. And what the owning service holds for the identifier is what the refusal says.
        let reported = reported_export_task(&router, case, row.task).await;
        match row.aftermath {
            Aftermath::NoExportTask => assert!(
                reported.is_empty(),
                "case {case:?}: expected the refused export to leave no export task behind, got \
                 {reported:?}"
            ),
            Aftermath::TheExportItAlreadyStoodFor => assert_eq!(
                reported, taken,
                "case {case:?}: expected the export the identifier already stood for to be \
                 exactly as the deployment that started it left it ({taken:?}), got {reported:?}"
            ),
        }
    }
}
