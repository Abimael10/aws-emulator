// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests that `TestMigration` is answered FROM the replication group this
//! service holds, rather than from anything the request carried.
//!
//! The rule under test: an operation whose output member is a resource reports the SAME
//! projection of that resource every read of it reports, and leaves the stored resource as
//! it was — so the answer tracks a later change to the group, two calls in a row answer
//! identically, and the only trace the call leaves is the event it records under the group
//! it tested. `TestMigration` ("Async API to test connection between source and target
//! replication group") has exactly one output member, `ReplicationGroup`, so that
//! projection is its whole answer.
//!
//! The suite says nothing about WHICH requests are refused — that is
//! [`crate::test_migration`]'s table — and nothing here reaches into the state struct: the
//! group is created, changed and read back over the wire through the same router the
//! production emulator mounts, so an answer that is not computed from the persisted group
//! cannot pass.

use service_com_amazonaws_elasticache_amazonelasticachev9::state;

use crate::harness::{params, Emulator, Reply};
use crate::test_migration::{
    await_available_group, create_settled_group, describe_group, test_migration,
};

/// One source endpoint a row tests the connection from: `(Address, Port)`.
type Source = (&'static str, i32);

/// One row of the "a change shows up" table: the case, the group it builds, the form pairs
/// that change that group, and the member path and value the next test must then report.
type ChangedCase = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
    &'static str,
    &'static str,
);

/// The `ReplicationGroup` an answer reports, as `(path under the member, text)` pairs in
/// document order.
///
/// The path is taken relative to the `ReplicationGroup` element, so the projection a
/// single-resource answer carries (`TestMigrationResult/ReplicationGroup/…`) and the one a
/// describe carries (`…/ReplicationGroups/ReplicationGroup/…`) are directly comparable.
fn reported_group(case: &str, reply: &Reply) -> Vec<(String, String)> {
    let projection: Vec<(String, String)> = reply
        .nodes
        .iter()
        .filter_map(|(path, text)| {
            let mut segments = path.split('/');
            segments
                .by_ref()
                .position(|segment| segment == "ReplicationGroup")?;
            let relative: Vec<&str> = segments.collect();
            if relative.is_empty() {
                return None;
            }
            Some((relative.join("/"), text.clone()))
        })
        .collect();
    assert!(
        !projection.is_empty(),
        "case {case:?}: expected a ReplicationGroup member in the answer, got {}",
        reply.body
    );
    projection
}

/// What one projection says for the member at `path`, if it says anything.
fn member<'a>(projection: &'a [(String, String)], path: &str) -> Option<&'a str> {
    projection
        .iter()
        .find(|(key, _)| key == path)
        .map(|(_, value)| value.as_str())
}

/// The messages of the events recorded under the replication group `id`, newest first.
async fn group_events(emulator: &Emulator, case: &str, id: &str) -> Vec<String> {
    let events = emulator
        .call(
            "DescribeEvents",
            &params(&[
                ("SourceType", state::SOURCE_TYPE_REPLICATION_GROUP),
                ("SourceIdentifier", id),
            ]),
        )
        .await;
    assert_eq!(
        events.status, 200,
        "case {case:?}: the event read must answer 200, got {} {}",
        events.status, events.body
    );
    events
        .all("Event/Message")
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// A test reports the group the store holds — the same projection a describe of it reports
/// — leaves it exactly there, and records the test it ran under that group.
#[tokio::test]
async fn a_test_reports_the_stored_group_and_only_logs_that_it_ran() {
    // The axis: how the group is built (how many shards, with how many replicas each) and
    // which source the connection is tested from — the two things the request and the
    // stored group can differ along. `CustomerNodeEndpoint$Port` is a TCP port, so a row
    // sits on its lowest value too.
    let rows: &[(&str, &str, i32, Source)] = &[
        (
            "a cluster mode disabled group tested from an address",
            "stored-unsharded",
            1,
            ("10.0.0.31", 6379),
        ),
        (
            "a cluster mode enabled group tested from a host name",
            "stored-sharded",
            3,
            ("source.example.internal", 7000),
        ),
        (
            "a two-shard group tested from the lowest port a source can answer on",
            "stored-two-shards",
            2,
            ("192.0.2.77", 1),
        ),
    ];

    for (case, id, shards, source) in rows {
        let emulator = Emulator::new();
        create_settled_group(&emulator, case, id, *shards).await;
        let before = group_events(&emulator, case, id).await;

        let tested = test_migration(&emulator, id, &[*source]).await;
        assert_eq!(
            tested.status, 200,
            "case {case:?}: expected the test to be served, got {} {}",
            tested.status, tested.body
        );
        let reported = reported_group(case, &tested);

        // The answer is the stored group: every member of it, not just the id, is what the
        // operation that READS this group reports for it.
        let described = describe_group(&emulator, id).await;
        assert_eq!(
            described.status, 200,
            "case {case:?}: the read-back must answer 200, got {} {}",
            described.status, described.body
        );
        assert_eq!(
            reported,
            reported_group(case, &described),
            "case {case:?}: expected the tested group to be reported exactly as a describe \
             reports it, got {} against {}",
            tested.body,
            described.body
        );
        // …and it is a group of substance, not an empty shell: its own id, the status it is
        // in, its ARN and one node group per shard it was built with.
        assert_eq!(
            member(&reported, "ReplicationGroupId"),
            Some(*id),
            "case {case:?}: expected the group it tested to be named, got {}",
            tested.body
        );
        assert_eq!(
            member(&reported, "Status"),
            Some(state::STATUS_AVAILABLE),
            "case {case:?}: a test starts no work on the group, so it stays available, got {}",
            tested.body
        );
        let arn = member(&reported, "ARN").unwrap_or_default();
        assert!(
            arn.ends_with(&format!(":replicationgroup:{id}")),
            "case {case:?}: expected the stored group's own ARN, got {arn:?}"
        );
        assert_eq!(
            tested.all("NodeGroups/NodeGroup/NodeGroupId").len(),
            usize::try_from(*shards).expect("a row's shard count is a count"),
            "case {case:?}: expected one node group per shard, got {}",
            tested.body
        );

        // Nothing was consumed: the same test runs again and answers the same group.
        let again = test_migration(&emulator, id, &[*source]).await;
        assert_eq!(
            again.status, 200,
            "case {case:?}: expected a repeated test to be served, got {} {}",
            again.status, again.body
        );
        assert_eq!(
            reported_group(case, &again),
            reported,
            "case {case:?}: expected the repeated test to report the same group, got {}",
            again.body
        );

        // The one trace the two calls leave is the two events they recorded under the
        // group; the log they were added to is untouched.
        let after = group_events(&emulator, case, id).await;
        assert_eq!(
            after.len(),
            before.len() + 2,
            "case {case:?}: expected the two tests to record one event each, got {after:?} \
             after {before:?}"
        );
        assert_eq!(
            after[2..],
            before[..],
            "case {case:?}: expected the group's earlier events to be left as they were, got \
             {after:?} after {before:?}"
        );
        assert_eq!(
            after[0], after[1],
            "case {case:?}: expected both tests to record the same thing, got {after:?}"
        );
        let logged = after[0].to_ascii_lowercase();
        assert!(
            logged.contains("migration") && logged.contains("test"),
            "case {case:?}: expected an event recording the migration test, got {:?}",
            after[0]
        );
    }
}

/// What a test reports follows the group: a change made to the group after the first test
/// shows up in the next one, because the answer is read from the stored record.
#[tokio::test]
async fn a_test_reports_the_group_as_it_is_when_it_runs() {
    // The axis: WHICH member of the group is changed between the two tests. Each row names
    // a member `ModifyReplicationGroup` applies at once (the pending shape carries none of
    // them), so the next test has a different group to report.
    let rows: &[ChangedCase] = &[
        (
            "the description",
            "changed-description",
            &[("ReplicationGroupDescription", "the re-described target")],
            "Description",
            "the re-described target",
        ),
        (
            "the backup window",
            "changed-window",
            &[("SnapshotWindow", "07:00-08:00")],
            "SnapshotWindow",
            "07:00-08:00",
        ),
        (
            "how long a backup is kept",
            "changed-retention",
            &[("SnapshotRetentionLimit", "3")],
            "SnapshotRetentionLimit",
            "3",
        ),
    ];

    for (case, id, change, path, expected) in rows {
        let emulator = Emulator::new();
        create_settled_group(&emulator, case, id, 1).await;

        let first = test_migration(&emulator, id, &[("10.0.0.41", 6379)]).await;
        assert_eq!(
            first.status, 200,
            "case {case:?}: expected the first test to be served, got {} {}",
            first.status, first.body
        );
        let before = reported_group(case, &first);
        assert_ne!(
            member(&before, path),
            Some(*expected),
            "case {case:?}: the row must change {path} to a value the group did not already \
             carry, got {}",
            first.body
        );

        let mut pairs = params(&[("ReplicationGroupId", id)]);
        pairs.extend(params(change));
        let modified = emulator.call("ModifyReplicationGroup", &pairs).await;
        assert_eq!(
            modified.status, 200,
            "case {case:?}: the change must be accepted, got {} {}",
            modified.status, modified.body
        );
        // A change leaves the group busy for the span this service publishes; a test needs
        // it settled, so the suite waits for THAT condition rather than for a time.
        await_available_group(&emulator, case, id).await;

        let second = test_migration(&emulator, id, &[("10.0.0.41", 6379)]).await;
        assert_eq!(
            second.status, 200,
            "case {case:?}: expected the second test to be served, got {} {}",
            second.status, second.body
        );
        let after = reported_group(case, &second);
        assert_eq!(
            member(&after, path),
            Some(*expected),
            "case {case:?}: expected the tested group to carry the change to {path}, got {}",
            second.body
        );
        assert_eq!(
            after,
            reported_group(case, &describe_group(&emulator, id).await),
            "case {case:?}: expected the tested group to match the stored group after the \
             change, got {}",
            second.body
        );
    }
}
