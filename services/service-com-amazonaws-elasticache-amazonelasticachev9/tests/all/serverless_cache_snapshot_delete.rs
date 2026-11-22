// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `DeleteServerlessCacheSnapshot`: the removal of a stored
//! serverless cache snapshot.
//!
//! Every assertion goes over the wire, through the same router the production
//! emulator mounts, using the shared `harness` request builder and XML reader, and
//! what the delete claims is read back through
//! `DescribeServerlessCacheSnapshots` — so a response that is not computed from
//! persisted state cannot pass.
//!
//! The rule under test: a delete BEGINS the removal of the snapshot the request
//! names (resolved by its stored, lowercased name) — the call answers with that
//! persisted snapshot reporting `deleting`, leaving everything else it reports as
//! it stands, the record is still readable while the removal is outstanding, and
//! once the clock has made that removal due the snapshot is gone from every read.
//! It is refused and removes nothing when the account holds no snapshot under the
//! name (the modelled not-found fault, HTTP 404) and when the snapshot is not
//! `available` — one still being written, or one an earlier delete is already
//! removing (the modelled snapshot-state fault, HTTP 400).
//!
//! The tests are tables: each row is one input along an axis the rule generalises
//! over (the engine the source cache runs, the case and the length of the
//! snapshot's name, the Region the flow runs in, and — for the refusals — every
//! state a snapshot cannot be deleted from), so no single recorded request is what
//! makes them pass.

use service_com_amazonaws_elasticache_amazonelasticachev9::state;

use crate::harness::{
    hold_pending_changes, params, poll_until, release_pending_changes, Emulator, Reply, REGION,
};

/// A second Region, for the row that runs the whole flow outside the Region every
/// other row signs for.
const OTHER_REGION: &str = "eu-west-1";

/// A 63-character snapshot name: `ServerlessCacheSnapshotName` "must be between 1
/// and 255 characters", so a long name is as good a name as a short one.
const LONG_SNAPSHOT_NAME: &str = "a-serverless-cache-snapshot-name-of-sixty-three-characters-long";

/// One row of the delete table: the cache to snapshot and the snapshot taken of
/// it.
#[derive(Debug, Clone, Copy)]
struct DeleteCase {
    /// What the row is called in a failure message.
    case: &'static str,
    /// The Region the cache and the snapshot live in.
    region: &'static str,
    /// The `ServerlessCacheName` the create sends.
    cache_request_name: &'static str,
    /// The engine the source cache runs.
    engine: &'static str,
    /// The `ServerlessCacheSnapshotName` the create and the delete send, and the
    /// identity it is stored under — the name "is stored as a lowercase string".
    snapshot_request_name: &'static str,
    snapshot_stored_name: &'static str,
}

/// The axis: the engine the source cache runs, the case and the length of the
/// snapshot's name, and the Region the flow runs in. The delete rule holds over
/// all of them.
const CASES: &[DeleteCase] = &[
    DeleteCase {
        case: "redis, plain names",
        region: REGION,
        cache_request_name: "redis-delete-cache",
        engine: "redis",
        snapshot_request_name: "redis-delete-snapshot",
        snapshot_stored_name: "redis-delete-snapshot",
    },
    DeleteCase {
        case: "valkey, the snapshot named in mixed case",
        region: REGION,
        cache_request_name: "Valkey-Delete-Cache",
        engine: "valkey",
        snapshot_request_name: "Valkey-Delete-Snapshot",
        snapshot_stored_name: "valkey-delete-snapshot",
    },
    DeleteCase {
        case: "memcached, a single-character snapshot name",
        region: REGION,
        cache_request_name: "memcached-delete-cache",
        engine: "memcached",
        snapshot_request_name: "X",
        snapshot_stored_name: "x",
    },
    DeleteCase {
        case: "redis in another Region, with a 63-character snapshot name",
        region: OTHER_REGION,
        cache_request_name: "other-region-delete-cache",
        engine: "redis",
        snapshot_request_name: LONG_SNAPSHOT_NAME,
        snapshot_stored_name: LONG_SNAPSHOT_NAME,
    },
];

/// Create one serverless cache in the row's Region and insist the create itself
/// succeeded.
async fn create_cache(emulator: &Emulator, case: &DeleteCase) {
    let created = emulator
        .call_in(
            case.region,
            "CreateServerlessCache",
            &params(&[
                ("ServerlessCacheName", case.cache_request_name),
                ("Engine", case.engine),
            ]),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {}: CreateServerlessCache must answer 200, got {} {}",
        case.case, created.status, created.body
    );
}

/// Take a snapshot of the row's cache under `name` and insist the create
/// succeeded.
async fn create_snapshot(emulator: &Emulator, case: &DeleteCase, name: &str) {
    let created = emulator
        .call_in(
            case.region,
            "CreateServerlessCacheSnapshot",
            &params(&[
                ("ServerlessCacheSnapshotName", name),
                ("ServerlessCacheName", case.cache_request_name),
            ]),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {}: CreateServerlessCacheSnapshot must answer 200, got {} {}",
        case.case, created.status, created.body
    );
}

/// `DeleteServerlessCacheSnapshot` with the one member it takes.
async fn delete(emulator: &Emulator, region: &str, snapshot_name: &str) -> Reply {
    emulator
        .call_in(
            region,
            "DeleteServerlessCacheSnapshot",
            &params(&[("ServerlessCacheSnapshotName", snapshot_name)]),
        )
        .await
}

/// Read one snapshot back by name, whatever the answer is.
async fn describe(emulator: &Emulator, region: &str, snapshot_name: &str) -> Reply {
    emulator
        .call_in(
            region,
            "DescribeServerlessCacheSnapshots",
            &params(&[("ServerlessCacheSnapshotName", snapshot_name)]),
        )
        .await
}

/// Wait for a resource to reach the status the next step needs.
///
/// A create leaves a serverless cache and its snapshot in a transient status, and
/// the handlers never sleep, so the suite looks again until the CONDITION holds
/// rather than waiting a fixed time: what it asserts then does not depend on how
/// fast or how loaded the host is.
async fn await_settled_cache(emulator: &Emulator, case: &DeleteCase) {
    poll_until(
        emulator,
        case.region,
        case.case,
        &format!(
            "the serverless cache {} settling at available",
            case.cache_request_name
        ),
        "DescribeServerlessCaches",
        &params(&[("ServerlessCacheName", case.cache_request_name)]),
        |reply| reply.text("Status") == Some(state::STATUS_AVAILABLE),
    )
    .await;
}

/// See [`await_settled_cache`]: the same wait, for the snapshot a delete needs
/// settled.
async fn await_settled_snapshot(emulator: &Emulator, case: &DeleteCase, name: &str) {
    poll_until(
        emulator,
        case.region,
        case.case,
        &format!("the serverless cache snapshot {name} settling at available"),
        "DescribeServerlessCacheSnapshots",
        &params(&[("ServerlessCacheSnapshotName", name)]),
        |reply| reply.text("Status") == Some(state::STATUS_AVAILABLE),
    )
    .await;
}

/// The messages this service recorded under the snapshot stored as `stored_name`.
async fn snapshot_event_messages(
    emulator: &Emulator,
    region: &str,
    case: &str,
    stored_name: &str,
) -> Vec<String> {
    let events = emulator
        .call_in(
            region,
            "DescribeEvents",
            &params(&[
                ("SourceIdentifier", stored_name),
                ("SourceType", state::SOURCE_TYPE_SERVERLESS_CACHE_SNAPSHOT),
            ]),
        )
        .await;
    assert_eq!(
        events.status, 200,
        "case {case}: DescribeEvents must answer 200, got {} {}",
        events.status, events.body
    );
    events
        .all("Message")
        .into_iter()
        .map(str::to_string)
        .collect()
}

#[tokio::test]
async fn a_delete_begins_the_removal_of_the_snapshot_the_request_names() {
    let emulator = Emulator::new();

    for case in CASES {
        create_cache(&emulator, case).await;
    }
    for case in CASES {
        await_settled_cache(&emulator, case).await;
        create_snapshot(&emulator, case, case.snapshot_request_name).await;
    }
    for case in CASES {
        await_settled_snapshot(&emulator, case, case.snapshot_request_name).await;
    }

    for case in CASES {
        let before = describe(&emulator, case.region, case.snapshot_request_name).await;
        assert_eq!(
            before.status, 200,
            "case {}: the snapshot must be readable before the delete, got {} {}",
            case.case, before.status, before.body
        );

        let deleted = delete(&emulator, case.region, case.snapshot_request_name).await;
        assert_eq!(
            deleted.status, 200,
            "case {}: DeleteServerlessCacheSnapshot must answer 200, got {} {}",
            case.case, deleted.status, deleted.body
        );
        // The removal this delete just scheduled is held out of reach straight
        // away, so the phase the delete answers with holds across the several
        // calls that follow instead of being raced against its span — what the
        // harness asks a suite that needs a transient phase over more than one
        // call to do.
        hold_pending_changes(&emulator);
        // The delete answers with the PERSISTED snapshot, reporting the phase
        // this call put it in and leaving everything else as it stands. The
        // three members carried over from the read before it are read out of
        // that reply first, so a row cannot pass by both replies omitting the
        // member.
        let carried_over = ["ARN", "CreateTime", "BytesUsedForCache"].map(|member| {
            let value = before.text(member).unwrap_or_else(|| {
                panic!(
                    "case {}: the read before the delete must report {member}, got {}",
                    case.case, before.body
                )
            });
            (member, Some(value))
        });
        for (member, expected) in [
            (
                "ServerlessCacheSnapshotName",
                Some(case.snapshot_stored_name),
            ),
            ("Status", Some(state::STATUS_DELETING)),
            ("SnapshotType", Some(state::SERVERLESS_SNAPSHOT_TYPE_MANUAL)),
            ("Engine", Some(case.engine)),
        ]
        .into_iter()
        .chain(carried_over)
        {
            assert_eq!(
                deleted.text(member),
                expected,
                "case {}: expected {member} {expected:?}, got {:?} ({})",
                case.case,
                deleted.text(member),
                deleted.body
            );
        }

        // The record is still there while the removal is outstanding: a read
        // answers the same phase, from the same stored record.
        let deleting = describe(&emulator, case.region, case.snapshot_request_name).await;
        assert_eq!(
            deleting.status, 200,
            "case {}: the snapshot must still be readable while it is {}, got {} {}",
            case.case,
            state::STATUS_DELETING,
            deleting.status,
            deleting.body
        );
        for member in ["Status", "ARN", "CreateTime"] {
            assert_eq!(
                deleting.text(member),
                deleted.text(member),
                "case {}: the read must report {member} as the delete did, got {:?} where the \
                 delete answered {:?}",
                case.case,
                deleting.text(member),
                deleted.text(member)
            );
        }

        // A second delete of a snapshot already on its way out is refused: there
        // is work in flight over it, which is what the snapshot-state fault is.
        let again = delete(&emulator, case.region, case.snapshot_request_name).await;
        assert_eq!(
            again.status, 400,
            "case {}: a second delete must answer 400 while the snapshot is {}, got {} {}",
            case.case,
            state::STATUS_DELETING,
            again.status,
            again.body
        );
        assert_eq!(
            again.error_code(),
            "InvalidServerlessCacheSnapshotStateFault",
            "case {}: expected the snapshot-state fault, got {:?} ({})",
            case.case,
            again.error_code(),
            again.body
        );
        for fragment in [case.snapshot_stored_name, state::STATUS_DELETING] {
            assert!(
                again.error_message().contains(fragment),
                "case {}: the state fault must mention {fragment:?}, got {:?}",
                case.case,
                again.error_message()
            );
        }

        // Once the clock has made the removal due, the snapshot is gone from
        // every read — the read words its not-found the way a READ does.
        release_pending_changes(&emulator);
        let gone = poll_until(
            &emulator,
            case.region,
            case.case,
            &format!(
                "the serverless cache snapshot {} being removed",
                case.snapshot_request_name
            ),
            "DescribeServerlessCacheSnapshots",
            &params(&[("ServerlessCacheSnapshotName", case.snapshot_request_name)]),
            |reply| reply.status == 404,
        )
        .await;
        assert_eq!(
            gone.error_code(),
            "ServerlessCacheSnapshotNotFoundFault",
            "case {}: expected the not-found code, got {:?} ({})",
            case.case,
            gone.error_code(),
            gone.body
        );

        // The source cache is not touched by the delete of a snapshot taken of
        // it: a snapshot is its own resource.
        let cache = emulator
            .call_in(
                case.region,
                "DescribeServerlessCaches",
                &params(&[("ServerlessCacheName", case.cache_request_name)]),
            )
            .await;
        assert_eq!(
            cache.text("Status"),
            Some(state::STATUS_AVAILABLE),
            "case {}: the source cache must be left alone, got {} {}",
            case.case,
            cache.status,
            cache.body
        );

        // The deletion is one of the events this service records, under the
        // snapshot itself.
        let messages = snapshot_event_messages(
            &emulator,
            case.region,
            case.case,
            case.snapshot_stored_name,
        )
        .await;
        assert!(
            messages.iter().any(|message| {
                message.contains("deleted") && message.contains(case.snapshot_stored_name)
            }),
            "case {}: the delete must be recorded in the event log, got {messages:?}",
            case.case
        );
    }
}

#[tokio::test]
async fn a_delete_that_cannot_be_performed_is_refused_and_removes_nothing() {
    let emulator = Emulator::new();
    let case = &CASES[0];
    // A second snapshot of the same cache, to show a refused delete — and a
    // served one — reaches only the snapshot it names.
    let bystander = "redis-delete-bystander";
    create_cache(&emulator, case).await;
    await_settled_cache(&emulator, case).await;
    create_snapshot(&emulator, case, bystander).await;
    await_settled_snapshot(&emulator, case, bystander).await;

    // A name the account holds no snapshot under: the modelled not-found fault,
    // whatever case the name is in and whatever its length.
    for missing in [
        "no-such-snapshot",
        "No-Such-Snapshot",
        "x",
        LONG_SNAPSHOT_NAME,
    ] {
        let refused = delete(&emulator, case.region, missing).await;
        assert_eq!(
            refused.status, 404,
            "case {missing}: expected HTTP 404, got {} {}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            "ServerlessCacheSnapshotNotFoundFault",
            "case {missing}: expected the not-found code, got {:?} ({})",
            refused.error_code(),
            refused.body
        );
        assert_eq!(
            refused.error_message(),
            // The class's own fixed sentence: it states only that no snapshot is
            // held under the specified name, and names nothing the request sent.
            "Serverless cache snapshot with specified name does not exists.",
            "case {missing}: the fault must state the class's own sentence, got {:?}",
            refused.error_message()
        );
    }

    // A snapshot that has not settled yet is still being written, so there is
    // nothing finished to remove: the modelled snapshot-state fault, naming the
    // state it is in. The clock owes the snapshot `available` from the call that
    // took it, so that state is held out of reach rather than raced for.
    create_snapshot(&emulator, case, case.snapshot_request_name).await;
    hold_pending_changes(&emulator);
    let unsettled = delete(&emulator, case.region, case.snapshot_request_name).await;
    assert_eq!(
        unsettled.status, 400,
        "case {}: expected HTTP 400 while the snapshot is {}, got {} {}",
        case.case,
        state::STATUS_CREATING,
        unsettled.status,
        unsettled.body
    );
    assert_eq!(
        unsettled.error_code(),
        "InvalidServerlessCacheSnapshotStateFault",
        "case {}: expected the snapshot-state fault, got {:?} ({})",
        case.case,
        unsettled.error_code(),
        unsettled.body
    );
    for fragment in [case.snapshot_stored_name, state::STATUS_CREATING] {
        assert!(
            unsettled.error_message().contains(fragment),
            "case {}: the state fault must mention {fragment:?}, got {:?}",
            case.case,
            unsettled.error_message()
        );
    }

    // Nothing was removed by any of those refusals: both snapshots are still
    // readable, and no deletion was recorded for either.
    release_pending_changes(&emulator);
    await_settled_snapshot(&emulator, case, case.snapshot_request_name).await;
    for name in [bystander, case.snapshot_stored_name] {
        let read = describe(&emulator, case.region, name).await;
        assert_eq!(
            read.status, 200,
            "case {name}: a refused delete must leave the snapshot readable, got {} {}",
            read.status, read.body
        );
        let messages = snapshot_event_messages(&emulator, case.region, name, name).await;
        assert!(
            !messages.iter().any(|message| message.contains("deleted")),
            "case {name}: no refused delete may be recorded as one, got {messages:?}"
        );
    }

    // The same snapshot IS deleted once it is available, and the other snapshot
    // of the same cache stays — so the refusals above are the rule's, not a
    // blanket refusal.
    let served = delete(&emulator, case.region, case.snapshot_request_name).await;
    assert_eq!(
        served.status, 200,
        "case {}: the delete of the settled snapshot must answer 200, got {} {}",
        case.case, served.status, served.body
    );
    let bystander_read = describe(&emulator, case.region, bystander).await;
    assert_eq!(
        bystander_read.text("Status"),
        Some(state::STATUS_AVAILABLE),
        "case {bystander}: the delete must reach only the snapshot it names, got {} {}",
        bystander_read.status,
        bystander_read.body
    );
}
