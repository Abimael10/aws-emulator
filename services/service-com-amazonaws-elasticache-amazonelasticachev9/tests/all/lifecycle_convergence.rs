// SPDX-License-Identifier: CC0-1.0

//! What a REPORT answers while the clock still owes the resource a change it has already
//! scheduled, and how a client reaches the settled state.
//!
//! The rule under test: the call that STARTS work answers with the transient status the
//! service documents for that phase, and a REPORT issued while the change the clock owes is
//! still outstanding answers from the record as it stands — the same transient status, and
//! for a resource on its way out the resource itself, still there and `deleting`. No call of
//! this service waits for a change it has scheduled, and none advances one either: a report
//! materialises exactly what the clock had already made due. The clock then carries the
//! record to its settled state — a status, or the resource's absence — which a client reads
//! back.
//!
//! The span a transition is owed after is the CALL that started the work
//! ([`state::CLUSTER_TRANSITION`]), so each row holds the change out of reach while it
//! asserts the transient half: what a client's FIRST read after that call answers is the
//! settled state, which is the rule
//! [`crate::first_read_sees_the_settled_state`] is about.
//!
//! One resource has no such span to report: `DeleteCacheCluster` takes the cluster's record
//! away at the call, so there is nothing left to report it `deleting` from, and its
//! absence is asserted by the round trip below rather than by a row of the table.
//!
//! The tables below span what the rule generalises over — WHICH resource the report is
//! about (cache cluster, snapshot, replication group, serverless cache, RBAC user, user
//! group), WHICH phase it is in (being created, being changed, being restarted, going
//! away) and whether the settled state is a STATUS or the resource's ABSENCE. The snapshot
//! round trip of the recorded flow — create, copy, delete the source, delete the copy,
//! delete the cluster — is one row of that class, driven here the way a client drives it:
//! by polling each wait out.
//!
//! The transient half of each row is asserted with the record's due instant held out of
//! reach, so what it asserts does not depend on how fast the host runs; the settled half
//! then makes every outstanding change due and polls for it. Both halves go over the wire
//! through the same router the production emulator mounts.

use std::time::Duration;

use service_com_amazonaws_elasticache_amazonelasticachev9::state;

use crate::harness::{
    every_pending_change_due_in, params, poll_until, Emulator, Reply, HELD_OUT_OF_REACH, REGION,
};

/// What the report issued inside the span must show, and what the clock must then settle.
#[derive(Debug, Clone, Copy)]
enum Settled {
    /// The resource reports `transient` under `path` inside the span, and `value` once the
    /// clock has carried it.
    Status { path: &'static str, value: &'static str },
    /// The resource is gone once the clock has carried the removal: the lookup answers this
    /// HTTP status and error code. Inside the span it is still reported, under `path`, with
    /// the transient status the starting call answered.
    Absent {
        path: &'static str,
        status: u16,
        code: &'static str,
    },
    /// The resource is gone from a reader that answers a name it holds nothing for with an
    /// empty page rather than a fault: no node is reported under `absent_path` once the
    /// clock has carried the removal, while inside the span it still is, under `path`.
    AbsentFromPage {
        path: &'static str,
        absent_path: &'static str,
    },
}

/// One row: a call that starts work on a resource, the transient status its own answer must
/// carry, and the reports that follow it.
struct Phase {
    case: &'static str,
    /// The call that starts the work, and the request it is made with.
    start: &'static str,
    start_request: Vec<(String, String)>,
    /// Where that call's answer carries the resource's status, and the transient value it
    /// must report.
    transient_path: &'static str,
    transient: &'static str,
    /// The report issued after it, and what it must be answered with.
    report: &'static str,
    report_request: Vec<(String, String)>,
    settled: Settled,
}



/// Drive one row: the starting call, a report inside the span, then the report a client
/// polls until the clock has settled the record.
async fn assert_reports_the_transient_phase_then_settles(emulator: &Emulator, phase: &Phase) {
    let started = emulator.call(phase.start, &phase.start_request).await;
    assert_eq!(
        started.status, 200,
        "case {}: {} must answer 200, got {} {}",
        phase.case, phase.start, started.status, started.body
    );
    assert_eq!(
        started.text(phase.transient_path),
        Some(phase.transient),
        "case {}: {} must answer with the transient status {:?}, got {:?}",
        phase.case,
        phase.start,
        phase.transient,
        started.text(phase.transient_path)
    );

    // Inside the span: the report answers from the record as it stands. The due instant is
    // held out of reach first, so this half of the row is about the rule and not about how
    // fast the host runs.
    every_pending_change_due_in(emulator, HELD_OUT_OF_REACH);
    let inside = emulator.call(phase.report, &phase.report_request).await;
    assert_eq!(
        inside.status, 200,
        "case {}: a {} inside the span must answer 200, got {} {}",
        phase.case, phase.report, inside.status, inside.body
    );
    let transient_in_report = match phase.settled {
        Settled::Status { path, .. } => path,
        Settled::Absent { path, .. } | Settled::AbsentFromPage { path, .. } => path,
    };
    assert_eq!(
        inside.text(transient_in_report),
        Some(phase.transient),
        "case {}: a {} inside the span must report the record as it stands ({:?}), got {:?} \
         in {}",
        phase.case,
        phase.report,
        phase.transient,
        inside.text(transient_in_report),
        inside.body
    );

    // And then the clock's own change, which a client reaches by looking again.
    every_pending_change_due_in(emulator, Duration::ZERO);
    match phase.settled {
        Settled::Status { path, value } => {
            let reported = poll_until(
                emulator,
                REGION,
                phase.case,
                &format!("the resource reporting {value:?}"),
                phase.report,
                &phase.report_request,
                |reply| reply.text(path) == Some(value),
            )
            .await;
            assert_eq!(
                reported.text(path),
                Some(value),
                "case {}: the clock must carry the resource to {value:?}, got {:?}",
                phase.case,
                reported.text(path)
            );
        }
        Settled::Absent { status, code, .. } => {
            let reported = poll_until(
                emulator,
                REGION,
                phase.case,
                &format!("the resource answering {status} {code}"),
                phase.report,
                &phase.report_request,
                |reply| reply.status == status,
            )
            .await;
            assert_eq!(
                (reported.status, reported.error_code()),
                (status, code),
                "case {}: the clock must take the resource away ({status} {code}), got {} {}",
                phase.case,
                reported.status,
                reported.body
            );
        }
        Settled::AbsentFromPage { absent_path, .. } => {
            let reported = poll_until(
                emulator,
                REGION,
                phase.case,
                "the resource dropping out of the page",
                phase.report,
                &phase.report_request,
                |reply| reply.all(absent_path).is_empty(),
            )
            .await;
            assert!(
                reported.all(absent_path).is_empty(),
                "case {}: the clock must take the resource away, got {}",
                phase.case,
                reported.body
            );
        }
    }
}

/// Create a cluster and poll it until the clock has settled it `available`, which is the
/// state the rows below act from.
async fn available_cluster(emulator: &Emulator, id: &str, extra: &[(&str, &str)]) {
    let mut request = params(&[
        ("CacheClusterId", id),
        ("Engine", "redis"),
        ("CacheNodeType", "cache.t4g.micro"),
        ("NumCacheNodes", "1"),
    ]);
    request.extend(params(extra));
    let created = emulator.call("CreateCacheCluster", &request).await;
    assert_eq!(
        created.status, 200,
        "the cluster {id} must be creatable, got {} {}",
        created.status, created.body
    );
    poll_until(
        emulator,
        REGION,
        id,
        "the cluster settling at available",
        "DescribeCacheClusters",
        &params(&[("CacheClusterId", id)]),
        |reply| reply.text("CacheCluster/CacheClusterStatus") == Some(state::STATUS_AVAILABLE),
    )
    .await;
}

/// Create one RBAC user with a password, asserting the create succeeded.
async fn create_user(emulator: &Emulator, id: &str, password: &str) {
    let created = emulator
        .call(
            "CreateUser",
            &params(&[
                ("UserId", id),
                ("UserName", id),
                ("Engine", "redis"),
                ("AccessString", "on ~* +@all"),
                ("Passwords.member.1", password),
            ]),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the user {id} must be creatable, got {} {}",
        created.status, created.body
    );
}

#[tokio::test]
async fn a_report_inside_a_transition_span_answers_from_the_record_and_then_the_clock_settles() {
    let emulator = Emulator::new();

    // The resources the phase rows act on, each already settled.
    available_cluster(&emulator, "converge-modify", &[]).await;
    available_cluster(&emulator, "converge-reboot", &[]).await;
    available_cluster(&emulator, "converge-snapshot", &[]).await;
    create_user(&emulator, "converge-user", "a-password-of-sixteen-plus").await;
    create_user(&emulator, "converge-doomed", "another-password-of-sixteen").await;
    let group = emulator
        .call(
            "CreateUserGroup",
            &params(&[
                ("UserGroupId", "converge-group"),
                ("Engine", "redis"),
                ("UserIds.member.1", "default"),
            ]),
        )
        .await;
    assert_eq!(
        group.status, 200,
        "the user group must be creatable, got {} {}",
        group.status, group.body
    );
    poll_until(
        &emulator,
        REGION,
        "converge-group",
        "the user group settling at active",
        "DescribeUserGroups",
        &params(&[("UserGroupId", "converge-group")]),
        |reply| reply.text("Status") == Some(state::USER_STATUS_ACTIVE),
    )
    .await;
    // The snapshot the copy row copies and the delete row deletes.
    let taken = emulator
        .call(
            "CreateSnapshot",
            &params(&[
                ("CacheClusterId", "converge-snapshot"),
                ("SnapshotName", "converge-source"),
            ]),
        )
        .await;
    assert_eq!(
        taken.status, 200,
        "the source snapshot must be creatable, got {} {}",
        taken.status, taken.body
    );
    poll_until(
        &emulator,
        REGION,
        "converge-source",
        "the snapshot settling at available",
        "DescribeSnapshots",
        &params(&[("SnapshotName", "converge-source")]),
        |reply| reply.text("Snapshot/SnapshotStatus") == Some(state::STATUS_AVAILABLE),
    )
    .await;
    // The cluster the source snapshot was taken of is `snapshotting` until the clock
    // carries it back, and the rows below take their own snapshots of it.
    poll_until(
        &emulator,
        REGION,
        "converge-snapshot",
        "the snapshotted cluster settling at available",
        "DescribeCacheClusters",
        &params(&[("CacheClusterId", "converge-snapshot")]),
        |reply| reply.text("CacheCluster/CacheClusterStatus") == Some(state::STATUS_AVAILABLE),
    )
    .await;

    // The axis: the resource the report is about, the phase it is in, and whether the
    // settled state is a status or the resource's absence.
    let phases = vec![
        Phase {
            case: "a cache cluster being created",
            start: "CreateCacheCluster",
            start_request: params(&[
                ("CacheClusterId", "converge-create"),
                ("Engine", "redis"),
                ("CacheNodeType", "cache.t4g.micro"),
                ("NumCacheNodes", "1"),
            ]),
            transient_path: "CacheCluster/CacheClusterStatus",
            transient: "creating",
            report: "DescribeCacheClusters",
            report_request: params(&[("CacheClusterId", "converge-create")]),
            settled: Settled::Status {
                path: "CacheCluster/CacheClusterStatus",
                value: "available",
            },
        },
        Phase {
            case: "a cache cluster being changed",
            start: "ModifyCacheCluster",
            start_request: params(&[
                ("CacheClusterId", "converge-modify"),
                ("CacheNodeType", "cache.m7g.large"),
                ("ApplyImmediately", "true"),
            ]),
            transient_path: "CacheCluster/CacheClusterStatus",
            transient: "modifying",
            report: "DescribeCacheClusters",
            report_request: params(&[("CacheClusterId", "converge-modify")]),
            settled: Settled::Status {
                path: "CacheCluster/CacheClusterStatus",
                value: "available",
            },
        },
        Phase {
            case: "a cache cluster being restarted",
            start: "RebootCacheCluster",
            start_request: params(&[
                ("CacheClusterId", "converge-reboot"),
                ("CacheNodeIdsToReboot.CacheNodeId.1", "0001"),
            ]),
            transient_path: "CacheCluster/CacheClusterStatus",
            transient: "rebooting cluster nodes",
            report: "DescribeCacheClusters",
            report_request: params(&[("CacheClusterId", "converge-reboot")]),
            settled: Settled::Status {
                path: "CacheCluster/CacheClusterStatus",
                value: "available",
            },
        },
        Phase {
            case: "a snapshot being created",
            start: "CreateSnapshot",
            start_request: params(&[
                ("CacheClusterId", "converge-snapshot"),
                ("SnapshotName", "converge-taken"),
            ]),
            transient_path: "Snapshot/SnapshotStatus",
            transient: "creating",
            report: "DescribeSnapshots",
            report_request: params(&[("SnapshotName", "converge-taken")]),
            settled: Settled::Status {
                path: "Snapshot/SnapshotStatus",
                value: "available",
            },
        },
        Phase {
            case: "a snapshot being copied",
            start: "CopySnapshot",
            start_request: params(&[
                ("SourceSnapshotName", "converge-source"),
                ("TargetSnapshotName", "converge-copy"),
            ]),
            transient_path: "Snapshot/SnapshotStatus",
            transient: "creating",
            report: "DescribeSnapshots",
            report_request: params(&[("SnapshotName", "converge-copy")]),
            settled: Settled::Status {
                path: "Snapshot/SnapshotStatus",
                value: "available",
            },
        },
        Phase {
            case: "a snapshot going away",
            start: "DeleteSnapshot",
            start_request: params(&[("SnapshotName", "converge-source")]),
            transient_path: "Snapshot/SnapshotStatus",
            transient: "deleting",
            report: "DescribeSnapshots",
            report_request: params(&[("SnapshotName", "converge-source")]),
            settled: Settled::AbsentFromPage {
                path: "Snapshot/SnapshotStatus",
                absent_path: "Snapshot/SnapshotName",
            },
        },
        Phase {
            case: "a replication group being created",
            start: "CreateReplicationGroup",
            start_request: params(&[
                ("ReplicationGroupId", "converge-rg"),
                ("ReplicationGroupDescription", "the convergence suite"),
                ("Engine", "redis"),
                ("NumCacheClusters", "1"),
            ]),
            transient_path: "ReplicationGroup/Status",
            transient: "creating",
            report: "DescribeReplicationGroups",
            report_request: params(&[("ReplicationGroupId", "converge-rg")]),
            settled: Settled::Status {
                path: "ReplicationGroup/Status",
                value: "available",
            },
        },
        Phase {
            case: "a serverless cache being created",
            start: "CreateServerlessCache",
            start_request: params(&[
                ("ServerlessCacheName", "converge-serverless"),
                ("Engine", "valkey"),
            ]),
            transient_path: "Status",
            transient: "creating",
            report: "DescribeServerlessCaches",
            report_request: params(&[("ServerlessCacheName", "converge-serverless")]),
            settled: Settled::Status {
                path: "Status",
                value: "available",
            },
        },
        Phase {
            case: "a serverless cache going away",
            start: "DeleteServerlessCache",
            start_request: params(&[("ServerlessCacheName", "converge-serverless")]),
            transient_path: "Status",
            transient: "deleting",
            report: "DescribeServerlessCaches",
            report_request: params(&[("ServerlessCacheName", "converge-serverless")]),
            settled: Settled::Absent {
                path: "Status",
                status: 404,
                code: "ServerlessCacheNotFoundFault",
            },
        },
        Phase {
            case: "an RBAC user being changed",
            start: "ModifyUser",
            start_request: params(&[
                ("UserId", "converge-user"),
                ("AccessString", "on ~app:* +@read"),
            ]),
            transient_path: "Status",
            transient: "modifying",
            report: "DescribeUsers",
            report_request: params(&[("UserId", "converge-user")]),
            settled: Settled::Status {
                path: "Status",
                value: "active",
            },
        },
        Phase {
            case: "a user group taking a membership change",
            start: "ModifyUserGroup",
            start_request: params(&[
                ("UserGroupId", "converge-group"),
                ("UserIdsToAdd.member.1", "converge-user"),
            ]),
            transient_path: "Status",
            transient: "modifying",
            report: "DescribeUserGroups",
            report_request: params(&[("UserGroupId", "converge-group")]),
            settled: Settled::Status {
                path: "Status",
                value: "active",
            },
        },
        Phase {
            case: "an RBAC user going away",
            start: "DeleteUser",
            start_request: params(&[("UserId", "converge-doomed")]),
            transient_path: "Status",
            transient: "deleting",
            report: "DescribeUsers",
            report_request: params(&[("UserId", "converge-doomed")]),
            settled: Settled::Absent {
                path: "Status",
                status: 404,
                code: "UserNotFound",
            },
        },
    ];

    for phase in &phases {
        assert_reports_the_transient_phase_then_settles(&emulator, phase).await;
    }

    // The membership change the user-group row started is applied together with the
    // status, so the settled group holds the member the change named and reports nothing
    // outstanding — the convergence is of the whole change, not of a status word.
    let described = emulator
        .call(
            "DescribeUserGroups",
            &params(&[("UserGroupId", "converge-group")]),
        )
        .await;
    assert!(
        described.all("UserIds/member").contains(&"converge-user"),
        "the settled group must hold the member the change added, got {}",
        described.body
    );
    assert!(
        !described.body.contains("PendingChanges"),
        "a settled group reports nothing outstanding, got {}",
        described.body
    );
}

/// The state gate a resource's transient phase goes with: a MUTATION arriving inside the
/// span is refused with the status the record carries, which is only observable because no
/// report of this service waits a scheduled change out.
///
/// The rows span the resources that carry such a gate and the call each one refuses.
#[tokio::test]
async fn a_mutation_arriving_inside_the_span_is_refused_with_the_status_the_record_carries() {
    // Each row: the case, the create and its request, the gated call and its request, and
    // the error code the gate answers.
    type Row = (
        &'static str,
        &'static str,
        Vec<(String, String)>,
        &'static str,
        Vec<(String, String)>,
        &'static str,
    );
    let rows: Vec<Row> = vec![
        (
            "a cache cluster still being created",
            "CreateCacheCluster",
            params(&[
                ("CacheClusterId", "gated-cluster"),
                ("Engine", "redis"),
                ("CacheNodeType", "cache.t4g.micro"),
                ("NumCacheNodes", "1"),
            ]),
            "DeleteCacheCluster",
            params(&[("CacheClusterId", "gated-cluster")]),
            "InvalidCacheClusterState",
        ),
        (
            "a user group still being created",
            "CreateUserGroup",
            params(&[
                ("UserGroupId", "gated-group"),
                ("Engine", "redis"),
                ("UserIds.member.1", "default"),
            ]),
            "DeleteUserGroup",
            params(&[("UserGroupId", "gated-group")]),
            "InvalidUserGroupState",
        ),
        (
            "a serverless cache still being created",
            "CreateServerlessCache",
            params(&[
                ("ServerlessCacheName", "gated-serverless"),
                ("Engine", "valkey"),
            ]),
            "DeleteServerlessCache",
            params(&[("ServerlessCacheName", "gated-serverless")]),
            "InvalidServerlessCacheStateFault",
        ),
    ];

    for (case, create, create_request, gated, gated_request, code) in rows {
        // One emulator per row, so the row's gate is the only thing outstanding.
        let emulator = Emulator::new();
        let created = emulator.call(create, &create_request).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: {create} must answer 200, got {} {}",
            created.status, created.body
        );
        // Held out of reach, so the gated call below arrives inside the span however fast
        // or slow the host runs.
        every_pending_change_due_in(&emulator, HELD_OUT_OF_REACH);
        let refused = emulator.call(gated, &gated_request).await;
        assert_eq!(
            (refused.status, refused.error_code()),
            (400, code),
            "case {case:?}: {gated} inside the span must be refused 400 {code}, got {} {}",
            refused.status,
            refused.body
        );
        // And the same call is accepted once the clock has settled the record, which is
        // what makes the refusal the state rule speaking rather than a blanket refusal.
        every_pending_change_due_in(&emulator, Duration::ZERO);
        let accepted = poll_until(
            &emulator,
            REGION,
            case,
            &format!("{gated} being accepted once the record has settled"),
            gated,
            &gated_request,
            |reply: &Reply| reply.status == 200,
        )
        .await;
        assert_eq!(
            accepted.status, 200,
            "case {case:?}: {gated} must be accepted once the record has settled, got {} {}",
            accepted.status, accepted.body
        );
    }
}

/// `DescribeSnapshots` for one snapshot by name.
async fn snapshot(emulator: &Emulator, name: &str) -> Reply {
    emulator
        .call("DescribeSnapshots", &params(&[("SnapshotName", name)]))
        .await
}

/// Poll `DescribeSnapshots` for `name` until it reports `status`.
async fn snapshot_reporting(emulator: &Emulator, name: &str, status: &str) -> Reply {
    poll_until(
        emulator,
        REGION,
        name,
        &format!("the snapshot reporting {status:?}"),
        "DescribeSnapshots",
        &params(&[("SnapshotName", name)]),
        |reply| reply.text("Snapshot/SnapshotStatus") == Some(status),
    )
    .await
}

#[tokio::test]
async fn the_snapshot_round_trip_converges_by_polling_each_wait_out() {
    let emulator = Emulator::new();
    // The recorded flow, driven the way a client drives it: each wait is polled out.
    available_cluster(&emulator, "round-trip", &[("SnapshotRetentionLimit", "0")]).await;

    let source = emulator
        .call(
            "CreateSnapshot",
            &params(&[
                ("CacheClusterId", "round-trip"),
                ("SnapshotName", "round-trip-source"),
            ]),
        )
        .await;
    assert_eq!(
        source.status, 200,
        "CreateSnapshot must answer 200, got {} {}",
        source.status, source.body
    );
    let settled_source = snapshot_reporting(&emulator, "round-trip-source", "available").await;
    assert_eq!(
        settled_source.text("Snapshot/SnapshotStatus"),
        Some("available"),
        "the source snapshot must settle available, got {}",
        settled_source.body
    );
    // A snapshot of a cluster records the cluster it was taken from, with one
    // `NodeSnapshot` per node of it.
    assert_eq!(
        settled_source.text("Snapshot/CacheClusterId"),
        Some("round-trip"),
        "the snapshot must record the cluster it was taken from, got {}",
        settled_source.body
    );
    assert_eq!(
        settled_source.all("NodeSnapshot/CacheNodeId").len(),
        1,
        "a single-node cluster's snapshot holds exactly one NodeSnapshot, got {}",
        settled_source.body
    );

    // The copy is an independent snapshot of its own, which records the SOURCE cluster.
    let copied = emulator
        .call(
            "CopySnapshot",
            &params(&[
                ("SourceSnapshotName", "round-trip-source"),
                ("TargetSnapshotName", "round-trip-copy"),
            ]),
        )
        .await;
    assert_eq!(
        copied.status, 200,
        "CopySnapshot must answer 200, got {} {}",
        copied.status, copied.body
    );
    assert!(
        copied.text("Snapshot/SnapshotStatus") == Some("creating"),
        "the copy's answer must report the transient status this service gives a copy it \
         has started ({:?} of the modelled creating | copying | available), got {}",
        copied.text("Snapshot/SnapshotStatus"),
        copied.body
    );
    let settled_copy = snapshot_reporting(&emulator, "round-trip-copy", "available").await;
    assert_eq!(
        settled_copy.text("Snapshot/SnapshotStatus"),
        Some("available"),
        "the copy must settle available, got {}",
        settled_copy.body
    );
    assert_eq!(
        settled_copy.text("Snapshot/CacheClusterId"),
        Some("round-trip"),
        "the copy must record the SOURCE cluster, got {}",
        settled_copy.body
    );
    assert_eq!(
        settled_copy.all("NodeSnapshot/CacheNodeId").len(),
        1,
        "the copy carries the source's node snapshots, got {}",
        settled_copy.body
    );

    // Both snapshots coexist: the copy is not a second view of the source.
    let listed = emulator.call("DescribeSnapshots", &params(&[])).await;
    let names = listed.all("Snapshot/SnapshotName");
    for name in ["round-trip-source", "round-trip-copy"] {
        assert!(
            names.contains(&name),
            "both snapshots must be listed, got {names:?}"
        );
    }

    // Deleting the source removes it, and the copy survives it untouched.
    let deleted = emulator
        .call(
            "DeleteSnapshot",
            &params(&[("SnapshotName", "round-trip-source")]),
        )
        .await;
    assert_eq!(
        deleted.text("Snapshot/SnapshotStatus"),
        Some("deleting"),
        "DeleteSnapshot must answer with the snapshot on its way out, got {}",
        deleted.body
    );
    let gone = poll_until(
        &emulator,
        REGION,
        "round-trip-source",
        "the snapshot being removed by the clock",
        "DescribeSnapshots",
        &params(&[("SnapshotName", "round-trip-source")]),
        |reply| reply.all("Snapshot/SnapshotName").is_empty(),
    )
    .await;
    assert!(
        gone.all("Snapshot/SnapshotName").is_empty(),
        "once the clock has removed it, a read must not find the source, got {} {}",
        gone.status,
        gone.body
    );
    let survivor = snapshot(&emulator, "round-trip-copy").await;
    assert_eq!(
        survivor.text("Snapshot/SnapshotStatus"),
        Some("available"),
        "the copy must survive its source's deletion, still available, got {}",
        survivor.body
    );
    assert_eq!(
        survivor.text("Snapshot/CacheClusterId"),
        Some("round-trip"),
        "the survivor still records the cluster it was taken from, got {}",
        survivor.body
    );

    // And the way out: the copy, then the cluster.
    let deleted_copy = emulator
        .call(
            "DeleteSnapshot",
            &params(&[("SnapshotName", "round-trip-copy")]),
        )
        .await;
    assert_eq!(
        deleted_copy.text("Snapshot/SnapshotStatus"),
        Some("deleting"),
        "DeleteSnapshot must answer with the copy on its way out, got {}",
        deleted_copy.body
    );
    let copy_gone = poll_until(
        &emulator,
        REGION,
        "round-trip-copy",
        "the copy being removed by the clock",
        "DescribeSnapshots",
        &params(&[("SnapshotName", "round-trip-copy")]),
        |reply| reply.all("Snapshot/SnapshotName").is_empty(),
    )
    .await;
    assert!(
        copy_gone.all("Snapshot/SnapshotName").is_empty(),
        "once the clock has removed it, a read must not find the copy, got {} {}",
        copy_gone.status,
        copy_gone.body
    );
    // A delete needs the cluster settled, and taking a snapshot of it left it
    // `snapshotting` for a span.
    poll_until(
        &emulator,
        REGION,
        "round-trip",
        "the cluster settling at available",
        "DescribeCacheClusters",
        &params(&[("CacheClusterId", "round-trip")]),
        |reply| reply.text("CacheCluster/CacheClusterStatus") == Some(state::STATUS_AVAILABLE),
    )
    .await;
    let removed = emulator
        .call(
            "DeleteCacheCluster",
            &params(&[("CacheClusterId", "round-trip")]),
        )
        .await;
    assert_eq!(
        removed.text("CacheCluster/CacheClusterStatus"),
        Some("deleting"),
        "DeleteCacheCluster must answer with the cluster on its way out, got {}",
        removed.body
    );
    let cluster_gone = poll_until(
        &emulator,
        REGION,
        "round-trip",
        "the cluster being removed by the clock",
        "DescribeCacheClusters",
        &params(&[("CacheClusterId", "round-trip")]),
        |reply| reply.status == 404,
    )
    .await;
    assert_eq!(
        (cluster_gone.status, cluster_gone.error_code()),
        (404, "CacheClusterNotFound"),
        "once the clock has removed it, a read must not find the cluster, got {} {}",
        cluster_gone.status,
        cluster_gone.body
    );
}
