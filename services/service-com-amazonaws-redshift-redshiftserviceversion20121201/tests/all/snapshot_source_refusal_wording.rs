// SPDX-License-Identifier: CC0-1.0

//! What an operation that PLANS work from a snapshot says when the snapshot its
//! request points at is one this account does not hold, end to end over the
//! awsQuery wire.
//!
//! The RULE asserted here: `ClusterSnapshotNotFoundFault` has one status (404)
//! and one `awsQueryError` code (`ClusterSnapshotNotFound`), which the model
//! fixes, but the fixed TEXT of its message belongs to the OPERATION — the
//! operations that evaluate or plan work FROM a snapshot word it
//! `Snapshot not found: <identifier>`, while the operations that act on the
//! snapshot record itself word it with the modelled shape's own name,
//! `ClusterSnapshot not found: <identifier>` — and the only variable part of
//! either sentence is the locator value the request carried, whatever form it
//! takes and whichever member it arrives on.
//!
//! The rows differ along what the rule generalises over: the operation, the
//! `ActionType` it plans for, the member the snapshot locator arrives on
//! (`SnapshotIdentifier` or `SnapshotArn`) and the form and length of the value.
//! The expected sentences are written out literally here, so a change to either
//! fixed text fails a case rather than passing one computed from the code under
//! test. The harness is shared with [`crate::cluster_lifecycle`], which owns it.

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{Answer, Emulator};

/// Which family an operation's `ClusterSnapshotNotFoundFault` sentence comes
/// from.
#[derive(Debug, Clone, Copy)]
enum Family {
    /// The operations that plan work FROM the snapshot they are pointed at:
    /// `Snapshot not found: <identifier>`.
    PlansFromTheSnapshot,
    /// The operations that act on the snapshot record itself:
    /// `ClusterSnapshot not found: <identifier>`.
    ActsOnTheSnapshot,
}

/// The sentence `family` words for the locator value `locator`.
fn expected_message(family: Family, locator: &str) -> String {
    match family {
        Family::PlansFromTheSnapshot => format!("Snapshot not found: {locator}"),
        Family::ActsOnTheSnapshot => format!("ClusterSnapshot not found: {locator}"),
    }
}

/// One row of the operation table: the case, the action, the member the snapshot
/// locator arrives on, the other members the operation's model requires, and the
/// family its sentence comes from.
type OperationRow = (
    &'static str,
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
    Family,
);

/// Call `action` with `locator` on `member_name` plus `extra`.
async fn refuse(
    emulator: &Emulator,
    action: &str,
    member_name: &str,
    locator: &str,
    extra: &[(&str, &str)],
) -> Answer {
    let mut params: Vec<(&str, &str)> = vec![(member_name, locator)];
    params.extend_from_slice(extra);
    emulator.call(action, &params).await
}

/// Assert one refusal: the status, the `<Code>` the model gives the fault, and
/// the sentence the operation's family words.
fn assert_refusal(case: &str, answer: &Answer, expected: &str) {
    assert_eq!(
        (answer.status, answer.error_code().as_deref()),
        (404, Some("ClusterSnapshotNotFound")),
        "case {case}: expected 404 ClusterSnapshotNotFound, got {} / {:?} with body {}",
        answer.status,
        answer.error_code(),
        answer.body
    );
    assert_eq!(
        answer.text("Message").as_deref(),
        Some(expected),
        "case {case}: expected the refusal {expected:?}, got {:?} with body {}",
        answer.text("Message"),
        answer.body
    );
}

/// Every operation that plans work from a snapshot words the snapshot-not-found
/// refusal its family's way, for every form of identifier — and the operations
/// that act on the snapshot record keep their own sentence.
#[tokio::test]
async fn a_snapshot_source_refusal_is_worded_for_the_operation_that_plans_from_it() {
    let emulator = Emulator::new();

    // The operations, each paired with the family of its sentence. The
    // `ActionType` values are the published ones
    // (`smithy_query(query="shape", shape="ActionType")`), reached here through
    // the constants this service already names them by.
    let operations: [OperationRow; 7] = [
        (
            "a restore planned from a snapshot",
            "DescribeNodeConfigurationOptions",
            "SnapshotIdentifier",
            &[("ActionType", state::ACTION_RESTORE_CLUSTER)],
            Family::PlansFromTheSnapshot,
        ),
        (
            "a recommendation planned from a snapshot",
            "DescribeNodeConfigurationOptions",
            "SnapshotIdentifier",
            &[("ActionType", state::ACTION_RECOMMEND_NODE_CONFIG)],
            Family::PlansFromTheSnapshot,
        ),
        (
            "a restore planned from a snapshot and a page size",
            "DescribeNodeConfigurationOptions",
            "SnapshotIdentifier",
            &[
                ("ActionType", state::ACTION_RESTORE_CLUSTER),
                ("MaxRecords", "100"),
            ],
            Family::PlansFromTheSnapshot,
        ),
        (
            "a cluster restored from a snapshot",
            "RestoreFromClusterSnapshot",
            "SnapshotIdentifier",
            &[("ClusterIdentifier", "snapshot-source-restored")],
            Family::PlansFromTheSnapshot,
        ),
        (
            "an exchange planned from a snapshot",
            "GetReservedNodeExchangeConfigurationOptions",
            "SnapshotIdentifier",
            &[("ActionType", state::EXCHANGE_ACTION_RESTORE_CLUSTER)],
            Family::PlansFromTheSnapshot,
        ),
        (
            "a delete of the snapshot record",
            "DeleteClusterSnapshot",
            "SnapshotIdentifier",
            &[],
            Family::ActsOnTheSnapshot,
        ),
        (
            "a copy of the snapshot record",
            "CopyClusterSnapshot",
            "SourceSnapshotIdentifier",
            &[("TargetSnapshotIdentifier", "snapshot-source-copy-target")],
            Family::ActsOnTheSnapshot,
        ),
    ];

    // The locator rows differ along the FORM and the LENGTH of the value the
    // request names, which neither sentence depends on.
    let longest = "s".repeat(255);
    let locators: [(&str, &str); 4] = [
        ("a plain name", "snapshot-source-absent"),
        ("a single character", "s"),
        ("a name carrying digits and dashes", "snap-2024-07-01-0001"),
        ("a name of the longest form the service admits", &longest),
    ];

    for (what, action, member_name, extra, family) in operations {
        for (form, locator) in locators {
            let case = format!("{action} for {what}, naming {form}");
            let answer = refuse(&emulator, action, member_name, locator, extra).await;
            assert_refusal(&case, &answer, &expected_message(family, locator));
        }
    }
}

/// The snapshot locator the refusal echoes is the one the request sent, on
/// either member the operation accepts it on.
#[tokio::test]
async fn the_refusal_echoes_whichever_locator_member_the_request_sent() {
    let emulator = Emulator::new();

    // `DescribeNodeConfigurationOptions` locates a snapshot by identifier or by
    // ARN (`smithy_query(query="operation", operation="DescribeNodeConfigurationOptions")`
    // — `SnapshotIdentifier` and `SnapshotArn`), and the sentence is the same for
    // both: only the value the request carried changes.
    let rows: [(&str, &str, &str); 3] = [
        (
            "a snapshot identifier",
            "SnapshotIdentifier",
            "locator-absent-snapshot",
        ),
        (
            "a snapshot ARN of this service's own form",
            "SnapshotArn",
            "arn:aws:redshift:us-west-2:000000000000:snapshot:nowhere/locator-absent-snapshot",
        ),
        (
            "a snapshot ARN carrying no snapshot resource at all",
            "SnapshotArn",
            "arn:aws:redshift:us-west-2:000000000000:cluster:nowhere",
        ),
    ];

    for (case, member_name, locator) in rows {
        let answer = refuse(
            &emulator,
            "DescribeNodeConfigurationOptions",
            member_name,
            locator,
            &[("ActionType", state::ACTION_RESTORE_CLUSTER)],
        )
        .await;
        assert_refusal(
            case,
            &answer,
            &expected_message(Family::PlansFromTheSnapshot, locator),
        );
    }
}

/// A snapshot this account DOES hold is planned from rather than refused, so the
/// refusals above are about the snapshot the request named and not about the
/// operation.
#[tokio::test]
async fn a_snapshot_this_account_holds_is_planned_from() {
    let emulator = Emulator::new();
    let cluster = "snapshot-source-cluster";
    let snapshot = "snapshot-source-snapshot";
    let created = emulator.create_minimal(cluster).await;
    assert_eq!(
        created.status, 200,
        "case seed cluster {cluster}: expected 200, got {} with body {}",
        created.status, created.body
    );
    emulator
        .poll_until(
            "the cluster is serving",
            "DescribeClusters",
            &[("ClusterIdentifier", cluster)],
            |answer| answer.text("ClusterStatus").as_deref() == Some(state::STATUS_AVAILABLE),
        )
        .await;
    let taken = emulator
        .call(
            "CreateClusterSnapshot",
            &[
                ("SnapshotIdentifier", snapshot),
                ("ClusterIdentifier", cluster),
            ],
        )
        .await;
    assert_eq!(
        taken.status, 200,
        "case seed snapshot {snapshot}: expected 200, got {} with body {}",
        taken.status, taken.body
    );
    emulator
        .poll_until(
            "the snapshot is available",
            "DescribeClusterSnapshots",
            &[("SnapshotIdentifier", snapshot)],
            |answer| answer.text("Status").as_deref() == Some(state::STATUS_AVAILABLE),
        )
        .await;

    let served = emulator
        .call(
            "DescribeNodeConfigurationOptions",
            &[
                ("ActionType", state::ACTION_RESTORE_CLUSTER),
                ("SnapshotIdentifier", snapshot),
            ],
        )
        .await;
    assert_eq!(
        served.status, 200,
        "case a snapshot this account holds: expected 200, got {} with body {}",
        served.status, served.body
    );
    assert_eq!(
        served.error_code(),
        None,
        "case a snapshot this account holds: expected no refusal, got {:?} with body {}",
        served.error_code(),
        served.body
    );

    // The read changed nothing a later caller sees: the same request is served
    // the same way again, and the snapshot is still held.
    let again = emulator
        .call(
            "DescribeNodeConfigurationOptions",
            &[
                ("ActionType", state::ACTION_RESTORE_CLUSTER),
                ("SnapshotIdentifier", snapshot),
            ],
        )
        .await;
    assert_eq!(
        again.status, 200,
        "case the same plan read twice: expected 200, got {} with body {}",
        again.status, again.body
    );
    let read_back = emulator
        .call(
            "DescribeClusterSnapshots",
            &[("SnapshotIdentifier", snapshot)],
        )
        .await;
    assert_eq!(
        read_back.status, 200,
        "case the snapshot read back: expected 200, got {} with body {}",
        read_back.status, read_back.body
    );
}
