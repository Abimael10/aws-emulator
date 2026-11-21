// SPDX-License-Identifier: CC0-1.0

//! What the FIRST read issued after the call that started work on a resource answers.
//!
//! The rule under test: a transition this service owes a resource is owed from the call
//! that started the work, so the transient status the service documents for that phase is
//! what THAT call's own answer carries, and the next request — a client's first poll —
//! reads the resource in its settled state (a status, or, for a removal, the resource's
//! absence). A client written against AWS polls a lifecycle status on an interval sized for
//! AWS, tens of seconds; a poll answered with the transient status costs it that whole
//! interval, so a flow that chains several waits would take minutes and be
//! indistinguishable from a resource that never settles.
//!
//! The rows span what the rule generalises over: WHICH resource (cache cluster, replication
//! group, serverless cache, user group, RBAC user, snapshot, snapshot copy, Global
//! datastore, reserved cache node), WHICH phase (being created, being changed, being
//! restarted, being removed) and whether the settled state is a STATUS or the resource's
//! ABSENCE. Nothing here sleeps and nothing polls: ONE read follows each starting call, and
//! what it answers is the assertion. Everything goes over the wire through the same router
//! the production emulator mounts.
//!
//! The companion of [`crate::lifecycle_convergence`], which asserts the other half: a
//! report issued while the change is held out of reach still answers the transient status,
//! and the state gates that go with it still refuse a change.

use crate::harness::{params, Emulator, Reply, REGION};

/// One row: the call that starts work on a resource, the transient status its own answer
/// must carry, and the single read that follows it.
struct Phase {
    case: &'static str,
    /// The call that starts the work, with the request it is made with.
    start: &'static str,
    start_request: Vec<(String, String)>,
    /// Where that call's answer carries the resource's status, and the transient value it
    /// must report there.
    transient_path: &'static str,
    transient: &'static str,
    /// The read issued straight after it, and what it must answer.
    read: &'static str,
    read_request: Vec<(String, String)>,
    settled: Settled,
}

/// What the first read that follows the starting call must answer.
#[derive(Debug, Clone, Copy)]
enum Settled {
    /// The resource reports `value` under `path`.
    Status { path: &'static str, value: &'static str },
    /// The resource is gone: the lookup answers this HTTP status and `awsQuery` code.
    Absent { status: u16, code: &'static str },
    /// The resource is gone from a reader that answers a name it holds nothing for with an
    /// empty page rather than a fault: no node is reported under `path`.
    AbsentFromPage { path: &'static str },
}

/// Drive one row: the starting call, then the ONE read that follows it.
async fn assert_first_read_is_settled(emulator: &Emulator, phase: &Phase) {
    let started = emulator.call(phase.start, &phase.start_request).await;
    assert_eq!(
        started.status, 200,
        "case {:?}: {} must answer 200, got {} {}",
        phase.case, phase.start, started.status, started.body
    );
    assert_eq!(
        started.text(phase.transient_path),
        Some(phase.transient),
        "case {:?}: {} must answer with the transient status {:?}, got {:?}",
        phase.case,
        phase.start,
        phase.transient,
        started.text(phase.transient_path)
    );

    let read = emulator.call(phase.read, &phase.read_request).await;
    match phase.settled {
        Settled::Status { path, value } => {
            assert_eq!(
                read.status, 200,
                "case {:?}: the first {} after {} must answer 200, got {} {}",
                phase.case, phase.read, phase.start, read.status, read.body
            );
            assert_eq!(
                read.text(path),
                Some(value),
                "case {:?}: the FIRST {} after {} must report {value:?}, got {:?} in {}",
                phase.case,
                phase.read,
                phase.start,
                read.text(path),
                read.body
            );
        }
        Settled::Absent { status, code } => {
            assert_eq!(
                (read.status, read.error_code()),
                (status, code),
                "case {:?}: the FIRST {} after {} must answer {status} {code}, got {} {}",
                phase.case,
                phase.read,
                phase.start,
                read.status,
                read.body
            );
        }
        Settled::AbsentFromPage { path } => {
            assert_eq!(
                read.status, 200,
                "case {:?}: the first {} after {} must answer 200, got {} {}",
                phase.case, phase.read, phase.start, read.status, read.body
            );
            assert!(
                read.all(path).is_empty(),
                "case {:?}: the FIRST {} after {} must report no {path}, got {}",
                phase.case,
                phase.read,
                phase.start,
                read.body
            );
        }
    }
}

/// A cluster of this account, read back until the create has settled it — the prerequisite
/// of every row whose starting call acts on an existing cluster.
async fn available_cluster(emulator: &Emulator, id: &str) {
    let created = emulator
        .call(
            "CreateCacheCluster",
            &params(&[
                ("CacheClusterId", id),
                ("Engine", "redis"),
                ("CacheNodeType", "cache.t3.micro"),
                ("NumCacheNodes", "1"),
            ]),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the cluster {id} must be created, got {} {}",
        created.status, created.body
    );
    // The rule under test, used as the setup: the read that follows the create reports the
    // cluster settled, so there is nothing to wait for.
    let described = emulator
        .call("DescribeCacheClusters", &params(&[("CacheClusterId", id)]))
        .await;
    assert_eq!(
        described.text("CacheClusterStatus"),
        Some("available"),
        "the cluster {id} must read back available, got {}",
        described.body
    );
}

/// An RBAC user of this account, with no password, in the one status a create leaves it in.
async fn user(emulator: &Emulator, id: &str) -> Reply {
    let created = emulator
        .call(
            "CreateUser",
            &params(&[
                ("UserId", id),
                ("UserName", id),
                ("Engine", "redis"),
                ("AccessString", "on ~* +@all"),
                ("NoPasswordRequired", "true"),
            ]),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the user {id} must be created, got {} {}",
        created.status, created.body
    );
    created
}

/// A user group holding the built-in `default` user, read back until it is active.
async fn active_user_group(emulator: &Emulator, id: &str) {
    let created = emulator
        .call(
            "CreateUserGroup",
            &params(&[
                ("UserGroupId", id),
                ("Engine", "redis"),
                ("UserIds.member.1", "default"),
            ]),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the user group {id} must be created, got {} {}",
        created.status, created.body
    );
    let described = emulator
        .call("DescribeUserGroups", &params(&[("UserGroupId", id)]))
        .await;
    assert_eq!(
        described.text("Status"),
        Some("active"),
        "the user group {id} must read back active, got {}",
        described.body
    );
}

#[tokio::test]
async fn a_created_resource_reads_back_settled_to_the_first_call_after_the_create() {
    let emulator = Emulator::new();

    // The axis: which resource was created. Each row's settled status is the one that
    // resource's own status member documents for a resource that is ready — `available` for
    // a cluster, a replication group and a serverless cache, `active` for a user group.
    let rows = [
        Phase {
            case: "a cache cluster",
            start: "CreateCacheCluster",
            start_request: params(&[
                ("CacheClusterId", "settles-cluster"),
                ("Engine", "redis"),
                ("CacheNodeType", "cache.t3.micro"),
                ("NumCacheNodes", "1"),
            ]),
            transient_path: "CacheClusterStatus",
            transient: "creating",
            read: "DescribeCacheClusters",
            read_request: params(&[("CacheClusterId", "settles-cluster")]),
            settled: Settled::Status {
                path: "CacheClusterStatus",
                value: "available",
            },
        },
        Phase {
            case: "a replication group",
            start: "CreateReplicationGroup",
            start_request: params(&[
                ("ReplicationGroupId", "settles-group"),
                ("ReplicationGroupDescription", "one wait, one poll"),
                ("Engine", "redis"),
                ("CacheNodeType", "cache.t3.micro"),
            ]),
            transient_path: "Status",
            transient: "creating",
            read: "DescribeReplicationGroups",
            read_request: params(&[("ReplicationGroupId", "settles-group")]),
            settled: Settled::Status {
                path: "Status",
                value: "available",
            },
        },
        Phase {
            case: "a serverless cache",
            start: "CreateServerlessCache",
            start_request: params(&[
                ("ServerlessCacheName", "settles-serverless"),
                ("Engine", "valkey"),
            ]),
            transient_path: "Status",
            transient: "creating",
            read: "DescribeServerlessCaches",
            read_request: params(&[("ServerlessCacheName", "settles-serverless")]),
            settled: Settled::Status {
                path: "Status",
                value: "available",
            },
        },
        Phase {
            case: "a user group",
            start: "CreateUserGroup",
            start_request: params(&[
                ("UserGroupId", "settles-rbac"),
                ("Engine", "redis"),
                ("UserIds.member.1", "default"),
            ]),
            transient_path: "Status",
            transient: "creating",
            read: "DescribeUserGroups",
            read_request: params(&[("UserGroupId", "settles-rbac")]),
            settled: Settled::Status {
                path: "Status",
                value: "active",
            },
        },
    ];

    for row in &rows {
        assert_first_read_is_settled(&emulator, row).await;
    }
}

#[tokio::test]
async fn a_changed_resource_reads_back_settled_to_the_first_call_after_the_change() {
    let emulator = Emulator::new();
    available_cluster(&emulator, "changed-cluster").await;
    available_cluster(&emulator, "rebooted-cluster").await;
    user(&emulator, "changed-user").await;
    active_user_group(&emulator, "changed-rbac").await;

    // The axis: which KIND of change the clock owes the resource — node work on a cluster, a
    // restart of its nodes, a change to a user's own settings, and a membership change to a
    // user group — and so which transient status the change's own answer carries.
    let rows = [
        Phase {
            case: "node work on a cache cluster",
            start: "ModifyCacheCluster",
            start_request: params(&[
                ("CacheClusterId", "changed-cluster"),
                ("EngineVersion", "7.0"),
                ("ApplyImmediately", "true"),
            ]),
            transient_path: "CacheClusterStatus",
            transient: "modifying",
            read: "DescribeCacheClusters",
            read_request: params(&[("CacheClusterId", "changed-cluster")]),
            settled: Settled::Status {
                path: "CacheClusterStatus",
                value: "available",
            },
        },
        Phase {
            case: "a reboot of a cluster's nodes",
            start: "RebootCacheCluster",
            start_request: params(&[
                ("CacheClusterId", "rebooted-cluster"),
                ("CacheNodeIdsToReboot.CacheNodeId.1", "0001"),
            ]),
            transient_path: "CacheClusterStatus",
            transient: "rebooting cluster nodes",
            read: "DescribeCacheClusters",
            read_request: params(&[("CacheClusterId", "rebooted-cluster")]),
            settled: Settled::Status {
                path: "CacheClusterStatus",
                value: "available",
            },
        },
        Phase {
            case: "a change to an RBAC user",
            start: "ModifyUser",
            start_request: params(&[("UserId", "changed-user"), ("Engine", "valkey")]),
            transient_path: "Status",
            transient: "modifying",
            read: "DescribeUsers",
            read_request: params(&[("UserId", "changed-user")]),
            settled: Settled::Status {
                path: "Status",
                value: "active",
            },
        },
        Phase {
            case: "a membership change to a user group",
            start: "ModifyUserGroup",
            start_request: params(&[
                ("UserGroupId", "changed-rbac"),
                ("UserIdsToAdd.member.1", "changed-user"),
            ]),
            transient_path: "Status",
            transient: "modifying",
            read: "DescribeUserGroups",
            read_request: params(&[("UserGroupId", "changed-rbac")]),
            settled: Settled::Status {
                path: "Status",
                value: "active",
            },
        },
    ];

    for row in &rows {
        assert_first_read_is_settled(&emulator, row).await;
    }
}

#[tokio::test]
async fn a_snapshot_reads_back_settled_to_the_first_call_after_the_call_that_made_it() {
    let emulator = Emulator::new();
    available_cluster(&emulator, "snapped-cluster").await;

    // The axis: how the snapshot came about — taken of a cluster, or copied from another
    // snapshot — and then its way out, where the settled state is the record's ABSENCE
    // rather than a status.
    let rows = [
        Phase {
            case: "a snapshot taken of a cluster",
            start: "CreateSnapshot",
            start_request: params(&[
                ("SnapshotName", "taken-snap"),
                ("CacheClusterId", "snapped-cluster"),
            ]),
            transient_path: "SnapshotStatus",
            transient: "creating",
            read: "DescribeSnapshots",
            read_request: params(&[("SnapshotName", "taken-snap")]),
            settled: Settled::Status {
                path: "SnapshotStatus",
                value: "available",
            },
        },
        Phase {
            case: "a copy of that snapshot",
            start: "CopySnapshot",
            start_request: params(&[
                ("SourceSnapshotName", "taken-snap"),
                ("TargetSnapshotName", "copied-snap"),
            ]),
            transient_path: "SnapshotStatus",
            transient: "creating",
            read: "DescribeSnapshots",
            read_request: params(&[("SnapshotName", "copied-snap")]),
            settled: Settled::Status {
                path: "SnapshotStatus",
                value: "available",
            },
        },
        Phase {
            case: "the copy on its way out",
            start: "DeleteSnapshot",
            start_request: params(&[("SnapshotName", "copied-snap")]),
            transient_path: "SnapshotStatus",
            transient: "deleting",
            read: "DescribeSnapshots",
            read_request: params(&[("SnapshotName", "copied-snap")]),
            settled: Settled::AbsentFromPage {
                path: "Snapshot/SnapshotName",
            },
        },
    ];

    for row in &rows {
        assert_first_read_is_settled(&emulator, row).await;
    }

    // The snapshot no call deleted is still there, and still settled: the removal the
    // clock carried through was the copy's alone.
    let left = emulator
        .call("DescribeSnapshots", &params(&[("SnapshotName", "taken-snap")]))
        .await;
    assert_eq!(
        left.text("SnapshotStatus"),
        Some("available"),
        "the snapshot nothing deleted must still read back available, got {}",
        left.body
    );
}

#[tokio::test]
async fn a_removed_resource_is_gone_to_the_first_call_after_the_delete() {
    let emulator = Emulator::new();
    user(&emulator, "doomed-user").await;
    active_user_group(&emulator, "doomed-rbac").await;
    let cache = emulator
        .call(
            "CreateServerlessCache",
            &params(&[
                ("ServerlessCacheName", "doomed-serverless"),
                ("Engine", "valkey"),
            ]),
        )
        .await;
    assert_eq!(
        cache.status, 200,
        "the serverless cache must be created, got {} {}",
        cache.status, cache.body
    );

    // The axis: which resource is going away, and so which modelled not-found its reader
    // answers once the clock has carried the removal through.
    let rows = [
        Phase {
            case: "an RBAC user",
            start: "DeleteUser",
            start_request: params(&[("UserId", "doomed-user")]),
            transient_path: "Status",
            transient: "deleting",
            read: "DescribeUsers",
            read_request: params(&[("UserId", "doomed-user")]),
            settled: Settled::Absent {
                status: 404,
                code: "UserNotFound",
            },
        },
        Phase {
            case: "a user group",
            start: "DeleteUserGroup",
            start_request: params(&[("UserGroupId", "doomed-rbac")]),
            transient_path: "Status",
            transient: "deleting",
            read: "DescribeUserGroups",
            read_request: params(&[("UserGroupId", "doomed-rbac")]),
            settled: Settled::Absent {
                status: 404,
                code: "UserGroupNotFound",
            },
        },
        Phase {
            case: "a serverless cache",
            start: "DeleteServerlessCache",
            start_request: params(&[("ServerlessCacheName", "doomed-serverless")]),
            transient_path: "Status",
            transient: "deleting",
            read: "DescribeServerlessCaches",
            read_request: params(&[("ServerlessCacheName", "doomed-serverless")]),
            settled: Settled::Absent {
                status: 404,
                code: "ServerlessCacheNotFoundFault",
            },
        },
    ];

    for row in &rows {
        assert_first_read_is_settled(&emulator, row).await;
    }
}

#[tokio::test]
async fn a_global_datastore_reads_back_settled_to_the_first_call_after_the_create() {
    let emulator = Emulator::new();
    let primary = emulator
        .call(
            "CreateReplicationGroup",
            &params(&[
                ("ReplicationGroupId", "global-primary"),
                ("ReplicationGroupDescription", "the datastore's primary"),
                ("Engine", "redis"),
                ("CacheNodeType", "cache.m5.large"),
            ]),
        )
        .await;
    assert_eq!(
        primary.status, 200,
        "the primary must be created, got {} {}",
        primary.status, primary.body
    );
    // One read, and the primary is ready to be built on — which is what the rule buys a
    // client driving a multi-step flow.
    let available = emulator
        .call(
            "DescribeReplicationGroups",
            &params(&[("ReplicationGroupId", "global-primary")]),
        )
        .await;
    assert_eq!(
        available.text("Status"),
        Some("available"),
        "the primary must read back available, got {}",
        available.body
    );

    // The id a Global datastore is stored under is the region's own prefix and the suffix
    // the request named; it is read off the create's answer rather than written here.
    let created = emulator
        .call(
            "CreateGlobalReplicationGroup",
            &params(&[
                ("GlobalReplicationGroupIdSuffix", "settles"),
                ("PrimaryReplicationGroupId", "global-primary"),
            ]),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the Global datastore must be created, got {} {}",
        created.status, created.body
    );
    assert_eq!(
        created.text("Status"),
        Some("creating"),
        "the create must answer with the transient status, got {}",
        created.body
    );
    let id = created
        .text("GlobalReplicationGroupId")
        .expect("the create must answer with the id the datastore is stored under")
        .to_string();

    // A datastore that has only the primary it was built around settles at `primary-only`,
    // which is what that status means — and the first read after the create answers it.
    let described = emulator
        .call_in(
            REGION,
            "DescribeGlobalReplicationGroups",
            &params(&[("GlobalReplicationGroupId", id.as_str())]),
        )
        .await;
    assert_eq!(
        described.text("Status"),
        Some("primary-only"),
        "case a Global datastore: the FIRST DescribeGlobalReplicationGroups after the create \
         must report the settled status, got {:?} in {}",
        described.text("Status"),
        described.body
    );
}

#[tokio::test]
async fn a_purchased_reservation_reads_back_settled_to_the_first_call_after_the_purchase() {
    let emulator = Emulator::new();
    // The offering is asked of the catalog rather than written here: the id is this
    // service's to mint.
    let offerings = emulator
        .call("DescribeReservedCacheNodesOfferings", &params(&[]))
        .await;
    assert_eq!(
        offerings.status, 200,
        "the offering catalog must be readable, got {} {}",
        offerings.status, offerings.body
    );
    let offering = offerings
        .text("ReservedCacheNodesOfferingId")
        .expect("the catalog must publish at least one offering")
        .to_string();

    let row = Phase {
        case: "a reserved cache node",
        start: "PurchaseReservedCacheNodesOffering",
        start_request: params(&[
            ("ReservedCacheNodesOfferingId", offering.as_str()),
            ("ReservedCacheNodeId", "settles-reservation"),
        ]),
        transient_path: "State",
        transient: "payment-pending",
        read: "DescribeReservedCacheNodes",
        read_request: params(&[("ReservedCacheNodeId", "settles-reservation")]),
        settled: Settled::Status {
            path: "State",
            value: "active",
        },
    };
    assert_first_read_is_settled(&emulator, &row).await;
}
