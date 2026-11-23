// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH REGION a snapshot copy looks its SOURCE up in.
//!
//! Every call goes over the wire through `db_instances::emulator`, as a real awsQuery form
//! POST carrying the SigV4 credential scope the router keys on, so each row really is a
//! request signed for the Region it names.
//!
//! The rule under test: the source of a snapshot copy is resolved in the Region the FORM of
//! the reference names — "If the source snapshot is in the same Amazon Web Services Region
//! as the copy, specify a valid DB snapshot identifier … If the source snapshot is in a
//! different Amazon Web Services Region than the copy, specify a valid DB snapshot ARN" — so
//! an ARN is looked up in the Region written in the ARN, whether or not that is the Region
//! the request was signed for, and only an unqualified identifier means the request's own
//! Region. A reference that resolves to no snapshot is the copy's own not-found fault,
//! naming the Region it was looked for in.
//!
//! The table below spans what the rule generalises over: both snapshot kinds a copy exists
//! for (a DB snapshot and a DB CLUSTER snapshot), the two forms a source may be named in
//! (an identifier and an ARN), the ARN whose Region IS the calling Region and the one whose
//! Region is another, and an ARN in a Region that holds no such snapshot.
//!
//! Nothing here waits out a span: every state a transition this service owes leads to is
//! reached by polling a REPORTING call through `lifecycle_poll_loops::poll_until`.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call_in, emulator, expect_error, expect_ok, optional_text, text};
use crate::lifecycle_poll_loops::poll_until;

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The Region the sources below are created in, and the Region the copies are signed for:
/// a cross-Region copy is a request to ONE Region naming a source in ANOTHER.
const SOURCE_REGION: &str = "us-west-2";
const COPY_REGION: &str = "eu-central-1";

/// A Region this suite stores nothing in, for the row whose ARN names one.
const EMPTY_REGION: &str = "ap-south-1";

/// The master user the source instances are created with, and its password: test
/// credentials, and the only place in this suite they appear.
const MASTER_USER: &str = "emuadmin";
const MASTER_PASSWORD: &str = "emu-Secret-99";

/// The status a snapshot reports once it holds the data it was taken of.
const AVAILABLE: &str = "available";

/// The status a copy reports while it is being made.
const COPYING: &str = "copying";

/// The `awsQueryError` codes of the two not-found faults the copies answer.
const DB_SNAPSHOT_NOT_FOUND: &str = "DBSnapshotNotFound";
const DB_CLUSTER_SNAPSHOT_NOT_FOUND: &str = "DBClusterSnapshotNotFoundFault";

/// WHICH snapshot kind a row copies: the two the service keeps, each with its own source
/// resource, its own copy operation and its own report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A DB snapshot, taken of a DB instance.
    DbSnapshot,
    /// A DB CLUSTER snapshot, taken of a DB cluster.
    ClusterSnapshot,
}

impl Kind {
    /// The operation that copies this kind.
    fn copy_action(self) -> &'static str {
        match self {
            Self::DbSnapshot => "CopyDBSnapshot",
            Self::ClusterSnapshot => "CopyDBClusterSnapshot",
        }
    }

    /// The member the copy names its SOURCE in, and the member it names the TARGET in.
    fn source_member(self) -> &'static str {
        match self {
            Self::DbSnapshot => "SourceDBSnapshotIdentifier",
            Self::ClusterSnapshot => "SourceDBClusterSnapshotIdentifier",
        }
    }

    fn target_member(self) -> &'static str {
        match self {
            Self::DbSnapshot => "TargetDBSnapshotIdentifier",
            Self::ClusterSnapshot => "TargetDBClusterSnapshotIdentifier",
        }
    }

    /// The element each kind's reports and copies answer with, and the member carrying its
    /// own ARN.
    fn element(self) -> &'static str {
        match self {
            Self::DbSnapshot => "DBSnapshot",
            Self::ClusterSnapshot => "DBClusterSnapshot",
        }
    }

    fn arn_member(self) -> &'static str {
        match self {
            Self::DbSnapshot => "DBSnapshotArn",
            Self::ClusterSnapshot => "DBClusterSnapshotArn",
        }
    }

    /// The `awsQueryError` code a source this service does not hold is refused with.
    fn not_found_code(self) -> &'static str {
        match self {
            Self::DbSnapshot => DB_SNAPSHOT_NOT_FOUND,
            Self::ClusterSnapshot => DB_CLUSTER_SNAPSHOT_NOT_FOUND,
        }
    }
}

/// Create the source resource of `kind` in [`SOURCE_REGION`], take a snapshot of it, and
/// answer that snapshot's identifier and the ARN the service publishes for it.
async fn a_source_snapshot(router: &MultiServiceRouter, kind: Kind) -> (String, String) {
    let case = format!("{kind:?}: the source snapshot");
    let snapshot = match kind {
        Kind::DbSnapshot => {
            let instance = "emu-xrcopy-src-inst";
            expect_ok(
                &case,
                call_in(
                    router,
                    SOURCE_REGION,
                    "CreateDBInstance",
                    &params![
                        "DBInstanceIdentifier" => instance,
                        "Engine" => "postgres",
                        "DBInstanceClass" => "db.t3.micro",
                        "AllocatedStorage" => 20,
                        "MasterUsername" => MASTER_USER,
                        "MasterUserPassword" => MASTER_PASSWORD,
                    ],
                )
                .await,
            );
            let describe_instance_request =
                params!["DBInstanceIdentifier" => instance].to_vec();
            poll_until(
                &case,
                "the source instance reaching available",
                || {
                    call_in(
                        router,
                        SOURCE_REGION,
                        "DescribeDBInstances",
                        &describe_instance_request,
                    )
                },
                |status, body| {
                    status == 200
                        && optional_text(body, "DBInstanceStatus").as_deref() == Some(AVAILABLE)
                },
            )
            .await;
            let snapshot = "emu-xrcopy-src-snap";
            expect_ok(
                &case,
                call_in(
                    router,
                    SOURCE_REGION,
                    "CreateDBSnapshot",
                    &params![
                        "DBInstanceIdentifier" => instance,
                        "DBSnapshotIdentifier" => snapshot,
                    ],
                )
                .await,
            );
            snapshot.to_string()
        }
        Kind::ClusterSnapshot => {
            let cluster = "emu-xrcopy-src-cluster";
            expect_ok(
                &case,
                call_in(
                    router,
                    SOURCE_REGION,
                    "CreateDBCluster",
                    &params![
                        "DBClusterIdentifier" => cluster,
                        "Engine" => "aurora-postgresql",
                        "MasterUsername" => MASTER_USER,
                        "MasterUserPassword" => MASTER_PASSWORD,
                    ],
                )
                .await,
            );
            let describe_cluster_request =
                params!["DBClusterIdentifier" => cluster].to_vec();
            poll_until(
                &case,
                "the source cluster reaching available",
                || {
                    call_in(
                        router,
                        SOURCE_REGION,
                        "DescribeDBClusters",
                        &describe_cluster_request,
                    )
                },
                |status, body| {
                    status == 200 && optional_text(body, "Status").as_deref() == Some(AVAILABLE)
                },
            )
            .await;
            let snapshot = "emu-xrcopy-src-cluster-snap";
            expect_ok(
                &case,
                call_in(
                    router,
                    SOURCE_REGION,
                    "CreateDBClusterSnapshot",
                    &params![
                        "DBClusterIdentifier" => cluster,
                        "DBClusterSnapshotIdentifier" => snapshot,
                    ],
                )
                .await,
            );
            snapshot.to_string()
        }
    };
    // The snapshot becomes available on its own, and only an available snapshot can be
    // copied: the CONDITION is polled for.
    let (reported, _) = poll_until(
        &case,
        "the source snapshot reaching available",
        || describe_snapshot(router, SOURCE_REGION, kind, &snapshot),
        |status, body| status == 200 && optional_text(body, "Status").as_deref() == Some(AVAILABLE),
    )
    .await;
    let arn = text(&reported, kind.arn_member());
    (snapshot, arn)
}

/// The report of one snapshot of `kind` in `region`.
async fn describe_snapshot(
    router: &MultiServiceRouter,
    region: &str,
    kind: Kind,
    identifier: &str,
) -> (u16, String) {
    match kind {
        Kind::DbSnapshot => {
            call_in(
                router,
                region,
                "DescribeDBSnapshots",
                &params!["DBSnapshotIdentifier" => identifier],
            )
            .await
        }
        Kind::ClusterSnapshot => {
            call_in(
                router,
                region,
                "DescribeDBClusterSnapshots",
                &params!["DBClusterSnapshotIdentifier" => identifier],
            )
            .await
        }
    }
}

/// RULE: a copy whose source is named by ARN resolves that source in the Region the ARN
/// names — the calling Region or another — and the copy it makes is a snapshot of the Region
/// the request was signed for; a source named by a bare identifier is resolved in the
/// request's own Region.
#[tokio::test]
async fn a_copy_resolves_its_source_in_the_region_the_reference_names() {
    for kind in [Kind::DbSnapshot, Kind::ClusterSnapshot] {
        let router = emulator();
        let (snapshot, source_arn) = a_source_snapshot(&router, kind).await;

        // The rows span the forms a source may be named in and the Region each is resolved
        // in: `(case, the Region the copy is signed for, the reference, the target name)`.
        let rows: Vec<(&str, &str, String, String)> = vec![
            (
                "a bare identifier in the source's own Region",
                SOURCE_REGION,
                snapshot.clone(),
                format!("{snapshot}-by-identifier"),
            ),
            (
                "an ARN whose Region is the calling Region",
                SOURCE_REGION,
                source_arn.clone(),
                format!("{snapshot}-by-arn-in-region"),
            ),
            (
                "an ARN whose Region is another one",
                COPY_REGION,
                source_arn.clone(),
                format!("{snapshot}-by-arn-cross-region"),
            ),
        ];
        for (case, region, reference, target) in rows {
            let case = format!("{kind:?}: {case}");
            let copied = expect_ok(
                &case,
                call_in(
                    &router,
                    region,
                    kind.copy_action(),
                    &[
                        (kind.source_member(), reference.clone()),
                        (kind.target_member(), target.clone()),
                    ],
                )
                .await,
            );
            let status = text(&copied, "Status");
            assert_eq!(
                status, COPYING,
                "case {case:?}: an accepted copy is still being made when the call returns, \
                 got {status:?} in {copied}"
            );
            // The copy is a snapshot of the Region the REQUEST was signed for, whichever
            // Region its source lived in: its own ARN says so, and the Region it was signed
            // for is the one that reports it.
            let arn = text(&copied, kind.arn_member());
            assert!(
                arn.contains(&format!(":{region}:")),
                "case {case:?}: the copy belongs to the Region the request was signed for, \
                 got {arn}"
            );
            let (reported, _) = poll_until(
                &case,
                "the copy reaching available",
                || describe_snapshot(&router, region, kind, &target),
                |status, body| {
                    status == 200
                        && optional_text(body, "Status").as_deref() == Some(AVAILABLE)
                },
            )
            .await;
            assert!(
                reported.contains(kind.element()),
                "case {case:?}: the Region the copy was made in reports it, got {reported}"
            );
        }

        // And an ARN whose Region holds no such snapshot is the copy's own not-found fault,
        // naming the Region it was looked for in — which is the ARN's, not the caller's.
        let elsewhere = source_arn.replace(SOURCE_REGION, EMPTY_REGION);
        let case = format!("{kind:?}: an ARN in a Region holding no such snapshot");
        let refused = expect_error(
            &case,
            call_in(
                &router,
                SOURCE_REGION,
                kind.copy_action(),
                &[
                    (kind.source_member(), elsewhere.clone()),
                    (kind.target_member(), format!("{snapshot}-missing-source")),
                ],
            )
            .await,
            404,
            kind.not_found_code(),
        );
        let message = text(&refused, "Message");
        assert!(
            message.contains(&elsewhere) && message.contains(EMPTY_REGION),
            "case {case:?}: the refusal names the snapshot asked for and the Region it was \
             looked for in, got {message:?}"
        );
    }
}
