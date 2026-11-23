// SPDX-License-Identifier: CC0-1.0

//! The lifecycle of a Global datastore that has only its primary member, from the call that
//! mints it to the deletion that gives the primary back, as ONE sequence — the whole flow a
//! client driving a Global datastore runs.
//!
//! RULE 1 — a `ModifyGlobalReplicationGroup` that changes only how the datastore is DESCRIBED
//! is work the service does as it answers: the new `GlobalReplicationGroupDescription` is
//! there for the first read, and the datastore keeps the status it had settled at
//! (`primary-only` for a datastore with no secondary, which is what that status means). A
//! modify that changes the datastore's CONFIGURATION — the engine version it runs, the node
//! type its members run on, whether they fail over — is work ElastiCache carries out on the
//! nodes, so that one answers `modifying` and the clock owes the datastore its settled status
//! from the call. This is the reading `ModifyReplicationGroup` already takes of its own
//! request, and it is what keeps a client that reads a change back from being refused by the
//! next state-gated call it makes.
//!
//! RULE 2 — the rest of that flow converges on the FIRST read after each call: the primary
//! replication group reaches `available`, the datastore reaches `primary-only`,
//! `DisassociateGlobalReplicationGroup` naming the PRIMARY is refused
//! `InvalidParameterValue` with the sentence that states the remedy and leaves the primary in
//! `Members`, and `DeleteGlobalReplicationGroup(RetainPrimaryReplicationGroup=true)` clears
//! the primary's `GlobalReplicationGroupInfo` as it answers, after which the retained primary
//! is an ordinary replication group that deletes to `ReplicationGroupNotFoundFault`.
//!
//! The rows of rule 1 differ along what the rule generalises over: WHICH member the modify
//! names, and so whether the change is one the service applies as it answers or one it has to
//! carry out.
//!
//! Every assertion goes over the wire through the same router the production emulator mounts.

use crate::harness::{
    assert_refusal, params, poll_until, release_pending_changes, Emulator, Reply, REGION,
};

/// The primary replication group every test here builds its datastore around.
const PRIMARY: &str = "topology-primary";

/// The datastore's own suffix, and the id it is stored under: the Region's auto-applied
/// prefix and the suffix, as `CreateGlobalReplicationGroup` documents it.
const SUFFIX: &str = "topology";
const DATASTORE: &str = "ldgnf-topology";

/// The status a Global datastore with no secondary member settles at.
const PRIMARY_ONLY: &str = "primary-only";

/// How ElastiCache refuses a `DisassociateGlobalReplicationGroup` that names the datastore's
/// primary member. Two fixed sentences, neither of which names a resource: the second one is
/// the only place the service states the remedy.
const PRIMARY_NOT_DISASSOCIABLE: &str =
    "Cannot disassociate primary replication group. Primary Replication Group will be \
     disassociated when you delete the Global Replication Group.";

/// Create the primary replication group and look again until it reports `available`.
async fn available_primary(emulator: &Emulator, case: &str, id: &str) {
    let created = emulator
        .call(
            "CreateReplicationGroup",
            &params(&[
                ("ReplicationGroupId", id),
                ("ReplicationGroupDescription", "the datastore's primary"),
                ("Engine", "redis"),
                ("CacheNodeType", "cache.m5.large"),
                ("NumNodeGroups", "1"),
                ("ReplicasPerNodeGroup", "0"),
                ("AutomaticFailoverEnabled", "false"),
            ]),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: the primary must be created, got {} {}",
        created.status, created.body
    );
    poll_until(
        emulator,
        REGION,
        case,
        "the primary reaching available",
        "DescribeReplicationGroups",
        &params(&[("ReplicationGroupId", id)]),
        |reply| reply.text("ReplicationGroup/Status") == Some("available"),
    )
    .await;
}

/// Mint the Global datastore around `primary` and look again until it reports `primary-only`.
async fn settled_datastore(emulator: &Emulator, case: &str, suffix: &str, primary: &str) -> Reply {
    let created = emulator
        .call(
            "CreateGlobalReplicationGroup",
            &params(&[
                ("GlobalReplicationGroupIdSuffix", suffix),
                ("PrimaryReplicationGroupId", primary),
                ("GlobalReplicationGroupDescription", "the datastore as minted"),
            ]),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: the datastore must be created, got {} {}",
        created.status, created.body
    );
    poll_until(
        emulator,
        REGION,
        case,
        "the datastore settling at primary-only",
        "DescribeGlobalReplicationGroups",
        &datastore_report(),
        |reply| reply.text("GlobalReplicationGroup/Status") == Some(PRIMARY_ONLY),
    )
    .await
}

/// The `DescribeGlobalReplicationGroups` request a client polls with: the datastore, with its
/// members.
fn datastore_report() -> Vec<(String, String)> {
    params(&[
        ("GlobalReplicationGroupId", DATASTORE),
        ("ShowMemberInfo", "true"),
    ])
}

#[tokio::test]
async fn a_description_change_is_applied_as_the_modify_answers() {
    let emulator = Emulator::new();
    available_primary(&emulator, "the datastore's modify", PRIMARY).await;
    settled_datastore(&emulator, "the datastore's modify", SUFFIX, PRIMARY).await;

    // One row per MEMBER a modify can name: the pairs it sends beside the identity, the
    // status the datastore reports as the call answers, and the member a read confirms the
    // change by.
    struct Row {
        case: &'static str,
        pairs: &'static [(&'static str, &'static str)],
        answered_status: &'static str,
        read_back: &'static str,
        expected: &'static str,
    }
    let rows: &[Row] = &[
        Row {
            case: "the description alone, which the service applies as it answers",
            pairs: &[("GlobalReplicationGroupDescription", "the datastore re-described")],
            answered_status: PRIMARY_ONLY,
            read_back: "GlobalReplicationGroupDescription",
            expected: "the datastore re-described",
        },
        Row {
            case: "the engine version, which ElastiCache carries out on the nodes",
            pairs: &[("EngineVersion", "7.1")],
            answered_status: "modifying",
            read_back: "EngineVersion",
            expected: "7.1",
        },
        Row {
            case: "the node type, which ElastiCache carries out on the nodes",
            pairs: &[("CacheNodeType", "cache.m5.xlarge")],
            answered_status: "modifying",
            read_back: "CacheNodeType",
            expected: "cache.m5.xlarge",
        },
        Row {
            case: "a description change beside one the nodes carry out",
            pairs: &[
                ("GlobalReplicationGroupDescription", "re-described with a resize"),
                ("CacheNodeType", "cache.m5.2xlarge"),
            ],
            answered_status: "modifying",
            read_back: "GlobalReplicationGroupDescription",
            expected: "re-described with a resize",
        },
    ];

    for row in rows {
        let mut pairs = params(&[
            ("GlobalReplicationGroupId", DATASTORE),
            ("ApplyImmediately", "true"),
        ]);
        pairs.extend(params(row.pairs));
        let modified = emulator
            .call("ModifyGlobalReplicationGroup", &pairs)
            .await;
        assert_eq!(
            modified.status, 200,
            "case {:?}: the modify must answer 200, got {} {}",
            row.case, modified.status, modified.body
        );
        assert_eq!(
            modified.text("GlobalReplicationGroup/Status"),
            Some(row.answered_status),
            "case {:?}: the modify must answer the datastore {:?}, got {}",
            row.case,
            row.answered_status,
            modified.body
        );
        // The change is readable at once either way, and the status the FIRST read reports is
        // the one the call answered: a change the service applied as it answered leaves the
        // datastore settled, so a client needs no second read before its next call.
        let read = emulator
            .call("DescribeGlobalReplicationGroups", &datastore_report())
            .await;
        assert_eq!(
            read.text(row.read_back),
            Some(row.expected),
            "case {:?}: the first read must report {} {:?}, got {}",
            row.case,
            row.read_back,
            row.expected,
            read.body
        );
        assert_eq!(
            read.text("GlobalReplicationGroup/Status"),
            Some(row.answered_status),
            "case {:?}: the first read must report the datastore {:?}, got {}",
            row.case,
            row.answered_status,
            read.body
        );
        // Whatever the row left outstanding, the clock carries it through to the datastore's
        // settled status, and the change the modify made is still there.
        release_pending_changes(&emulator);
        let settled = poll_until(
            &emulator,
            REGION,
            row.case,
            "the datastore settling again",
            "DescribeGlobalReplicationGroups",
            &datastore_report(),
            |reply| reply.text("GlobalReplicationGroup/Status") == Some(PRIMARY_ONLY),
        )
        .await;
        assert_eq!(
            settled.text(row.read_back),
            Some(row.expected),
            "case {:?}: the settled datastore must keep {} {:?}, got {}",
            row.case,
            row.read_back,
            row.expected,
            settled.body
        );
    }
}

#[tokio::test]
async fn the_primary_only_datastore_flow_converges_on_every_first_read() {
    let case = "the primary-only topology";
    let emulator = Emulator::new();
    available_primary(&emulator, case, PRIMARY).await;
    let minted = settled_datastore(&emulator, case, SUFFIX, PRIMARY).await;
    assert_eq!(
        minted.all("GlobalReplicationGroupMember/ReplicationGroupId"),
        vec![PRIMARY],
        "case {case:?}: the datastore must report its primary member, got {}",
        minted.body
    );

    // The description sticks, and the datastore stays settled: the first read after the
    // modify is the one a client acts on.
    let described = "the datastore after the modify";
    let modified = emulator
        .call(
            "ModifyGlobalReplicationGroup",
            &params(&[
                ("GlobalReplicationGroupId", DATASTORE),
                ("ApplyImmediately", "true"),
                ("GlobalReplicationGroupDescription", described),
            ]),
        )
        .await;
    assert_eq!(
        modified.status, 200,
        "case {case:?}: the modify must answer 200, got {} {}",
        modified.status, modified.body
    );
    let read = emulator
        .call("DescribeGlobalReplicationGroups", &datastore_report())
        .await;
    assert_eq!(
        (
            read.text("GlobalReplicationGroupDescription"),
            read.text("GlobalReplicationGroup/Status"),
        ),
        (Some(described), Some(PRIMARY_ONLY)),
        "case {case:?}: the first read after the modify must report the new description on a \
         settled datastore, got {}",
        read.body
    );

    // The PRIMARY cannot be disassociated: the refusal states the remedy, and the membership
    // is left exactly as it was.
    let refused = emulator
        .call(
            "DisassociateGlobalReplicationGroup",
            &params(&[
                ("GlobalReplicationGroupId", DATASTORE),
                ("ReplicationGroupId", PRIMARY),
                ("ReplicationGroupRegion", REGION),
            ]),
        )
        .await;
    assert_refusal(
        case,
        &refused,
        400,
        "InvalidParameterValue",
        PRIMARY_NOT_DISASSOCIABLE,
    );
    let still = emulator
        .call("DescribeGlobalReplicationGroups", &datastore_report())
        .await;
    assert_eq!(
        still.all("GlobalReplicationGroupMember/ReplicationGroupId"),
        vec![PRIMARY],
        "case {case:?}: the refused disassociation must leave the primary in Members, got {}",
        still.body
    );
    assert_eq!(
        still.text("GlobalReplicationGroup/Status"),
        Some(PRIMARY_ONLY),
        "case {case:?}: the refused disassociation must leave the datastore settled, got {}",
        still.body
    );

    // Deleting the datastore while RETAINING the primary: the primary stops reporting a
    // datastore as the call answers, and the datastore itself is gone.
    let deleted = emulator
        .call(
            "DeleteGlobalReplicationGroup",
            &params(&[
                ("GlobalReplicationGroupId", DATASTORE),
                ("RetainPrimaryReplicationGroup", "true"),
            ]),
        )
        .await;
    assert_eq!(
        deleted.status, 200,
        "case {case:?}: the delete must answer 200, got {} {}",
        deleted.status, deleted.body
    );
    let primary = emulator
        .call(
            "DescribeReplicationGroups",
            &params(&[("ReplicationGroupId", PRIMARY)]),
        )
        .await;
    assert!(
        primary
            .text("GlobalReplicationGroupInfo/GlobalReplicationGroupId")
            .is_none(),
        "case {case:?}: the retained primary must report no datastore once it is deleted, got {}",
        primary.body
    );
    assert_eq!(
        primary.text("ReplicationGroup/Status"),
        Some("available"),
        "case {case:?}: the retained primary must still be available, got {}",
        primary.body
    );
    let gone = emulator
        .call("DescribeGlobalReplicationGroups", &datastore_report())
        .await;
    assert_eq!(
        (gone.status, gone.error_code()),
        (404, "GlobalReplicationGroupNotFoundFault"),
        "case {case:?}: the deleted datastore must not be reported, got {} {}",
        gone.status,
        gone.body
    );

    // And the retained primary is an ordinary replication group from here on: it deletes, and
    // the teardown the delete begins carries through to the fault a client polls for.
    let deleted = emulator
        .call(
            "DeleteReplicationGroup",
            &params(&[("ReplicationGroupId", PRIMARY)]),
        )
        .await;
    assert_eq!(
        deleted.status, 200,
        "case {case:?}: the primary must delete, got {} {}",
        deleted.status, deleted.body
    );
    assert_eq!(
        deleted.text("ReplicationGroup/Status"),
        Some("deleting"),
        "case {case:?}: the delete must answer the group deleting, got {}",
        deleted.body
    );
    let gone = poll_until(
        &emulator,
        REGION,
        case,
        "the deleted primary reaching not-found",
        "DescribeReplicationGroups",
        &params(&[("ReplicationGroupId", PRIMARY)]),
        |reply| reply.status == 404,
    )
    .await;
    assert_eq!(
        gone.error_code(),
        "ReplicationGroupNotFoundFault",
        "case {case:?}: the deleted primary must be answered the modelled fault, got {}",
        gone.body
    );
}
