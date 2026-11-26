// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of what a replication leaves in a DESTINATION table
//! bucket, and of the `DeleteTableBucket` lifecycle that follows from it.
//!
//! The rule under test: a table bucket delete is refused only for children the
//! bucket really holds, and naming a table bucket as a replication destination
//! puts nothing in it — the call that configures replication answers before
//! anything has been copied anywhere, and the replica a destination holds is
//! what the replication itself leaves there once the replication of the table's
//! state to that destination has been carried out. So while no replication to a
//! destination has happened, that destination table bucket holds no namespace
//! and no table of this service's making, and its delete is the modelled empty
//! 204 — including after the configuration that named it was removed, which is
//! the lifecycle an AWS client's teardown follows. Once a replication HAS been
//! carried out, the destination holds the replica under the source table's own
//! namespace and name, and its delete is the modelled `BadRequestException`
//! (HTTP 400) stated with the one fixed not-empty sentence of this service until
//! the replica and its namespace are removed.
//!
//! The model is what bounds the shapes the rows span
//! (`smithy_query(query="operation", operation="DeleteTableBucket")` and
//! `smithy_query(query="closure", operation="PutTableReplication")`):
//! `DeleteTableBucket` is `DELETE /buckets/{tableBucketARN}` with success code
//! 204 and an empty output, `TableReplicationRules` is `@length(1, 1)` and a
//! rule's `destinations` `@length(1, 5)`. So the rows differ in the region, which
//! level the configuration that replicates the table sits at, how many
//! destinations its one rule names — one, several and the modelled maximum of
//! five — whether the source table carries a metadata file, and whether the
//! configuration is removed before the destinations are deleted. None of those
//! may change what a destination holds before a replication reached it.

use serde_json::{json, Value};

use super::harness::{await_replication_completed, listed_namespace_names, Emulator, Reply};
use super::wire_forms::create_table_request;

/// The fixed sentence this service states the delete of a table bucket that
/// still holds something with.
const BUCKET_NOT_EMPTY: &str = "The bucket that you tried to delete is not empty.";

/// The fixed sentence this service states a table bucket the store does not hold
/// with.
const NO_BUCKET: &str = "The specified bucket does not exist.";

/// The `ReplicationStatus` value a destination the replication has reached carries,
/// from `smithy_query(query="shape",shape="ReplicationStatus")`.
const COMPLETED: &str = "completed";

/// The IAM role every configuration in this suite replicates under, of the form
/// the modelled `IAMRole` pattern accepts (`^arn:.+:iam::[0-9]{12}:role/.+$`).
const ROLE: &str = "arn:aws:iam::000000000000:role/s3tables-replication-destinations";

/// Which level the configuration that replicates the table sits at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// The TABLE's own configuration (`PutTableReplication`).
    Table,
    /// The configuration of the table bucket holding it
    /// (`PutTableBucketReplication`), whose rules cover that bucket's tables.
    Bucket,
}

/// One row: a table in `bucket` replicated by a configuration at `scope` naming
/// `destinations` destination table buckets.
#[derive(Debug)]
struct Case {
    label: &'static str,
    region: &'static str,
    /// The source table bucket's name — lowercase letters, numbers and hyphens,
    /// 3..63 characters, as `CreateTableBucket` accepts one.
    bucket: &'static str,
    /// The namespace the source table lives in (`^[0-9a-z_]*$`).
    namespace: &'static str,
    /// The source table's name.
    table: &'static str,
    /// Whether the create writes the table a metadata file — and so an update a
    /// replication can copy.
    metadata: bool,
    scope: Scope,
    /// How many destination table buckets the one rule names;
    /// `ReplicationDestinations` is `@length(min: 1, max: 5)`.
    destinations: usize,
    /// Whether the configuration that named the destinations is removed before
    /// they are deleted, which is the teardown order a client follows.
    remove_configuration: bool,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "one destination, the table's own configuration, kept",
            region: "us-east-1",
            bucket: "repl-dst-east",
            namespace: "ns_east",
            table: "orders_east",
            metadata: true,
            scope: Scope::Table,
            destinations: 1,
            remove_configuration: false,
        },
        Case {
            label: "one destination, the table's own configuration, removed first",
            region: "us-west-2",
            bucket: "repl-dst-west",
            namespace: "ns_west",
            table: "orders_west",
            metadata: true,
            scope: Scope::Table,
            destinations: 1,
            remove_configuration: true,
        },
        Case {
            label: "two destinations, no metadata file, the configuration removed first",
            region: "eu-west-1",
            bucket: "repl-dst-eu",
            namespace: "ns_eu",
            table: "clicks_eu",
            metadata: false,
            scope: Scope::Table,
            destinations: 2,
            remove_configuration: true,
        },
        Case {
            label: "the modelled maximum of five destinations, the table's own configuration",
            region: "ap-south-1",
            bucket: "repl-dst-ap",
            namespace: "ns_ap",
            table: "sensor_readings",
            metadata: true,
            scope: Scope::Table,
            destinations: 5,
            remove_configuration: false,
        },
        Case {
            label: "the bucket's configuration, one destination",
            region: "us-east-2",
            bucket: "repl-dst-central",
            namespace: "ns_central",
            table: "ledger",
            metadata: true,
            scope: Scope::Bucket,
            destinations: 1,
            remove_configuration: false,
        },
        Case {
            label: "the bucket's configuration, three destinations, no metadata file",
            region: "eu-central-1",
            bucket: "repl-dst-frankfurt",
            namespace: "ns_frankfurt",
            table: "events",
            metadata: false,
            scope: Scope::Bucket,
            destinations: 3,
            remove_configuration: false,
        },
    ]
}

/// A configuration whose ONE rule — both replication rule lists are
/// `@length(1, 1)` — replicates to `destinations`.
fn configuration(destinations: &[String]) -> Value {
    json!({
        "role": ROLE,
        "rules": [{
            "destinations": destinations
                .iter()
                .map(|arn| json!({ "destinationTableBucketARN": arn }))
                .collect::<Vec<Value>>(),
        }],
    })
}

/// Create a table bucket, a namespace and one table in it, and answer the
/// bucket's ARN with the table's ARN.
async fn create_table(emulator: &Emulator, case: &Case) -> (String, String) {
    let label = case.label;
    let created = emulator
        .create_table_bucket(case.region, &json!({ "name": case.bucket }))
        .await;
    assert_eq!(
        created.status, 200,
        "case {label:?}: creating the bucket {bucket:?} must succeed, got {status} {body}",
        bucket = case.bucket,
        status = created.status,
        body = created.body
    );
    let bucket_arn = created.text("arn");
    let namespace = emulator
        .create_namespace(
            case.region,
            &bucket_arn,
            &json!({ "namespace": [case.namespace] }),
        )
        .await;
    assert_eq!(
        namespace.status, 200,
        "case {label:?}: creating the namespace must succeed, got {status} {body}",
        status = namespace.status,
        body = namespace.body
    );
    let table = emulator
        .create_table(
            case.region,
            &bucket_arn,
            case.namespace,
            &create_table_request(case.table, case.metadata),
        )
        .await;
    assert_eq!(
        table.status, 200,
        "case {label:?}: creating the table must succeed, got {status} {body}",
        status = table.status,
        body = table.body
    );
    (bucket_arn, table.text("tableARN"))
}

/// Create `count` destination table buckets named after the row's own bucket,
/// and answer their ARNs.
async fn destination_buckets(emulator: &Emulator, case: &Case) -> Vec<String> {
    let label = case.label;
    let mut arns = Vec::with_capacity(case.destinations);
    for index in 0..case.destinations {
        let name = format!("{bucket}-dst-{index}", bucket = case.bucket);
        let created = emulator
            .create_table_bucket(case.region, &json!({ "name": name }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: creating the destination bucket {name:?} must succeed, got {status} \
             {body}",
            status = created.status,
            body = created.body
        );
        arns.push(created.text("arn"));
    }
    arns
}

/// Set the configuration that replicates the table at the row's scope, and
/// answer the version token the accepted put minted.
async fn configure(
    emulator: &Emulator,
    case: &Case,
    bucket_arn: &str,
    table_arn: &str,
    destinations: &[String],
) -> String {
    let label = case.label;
    let settings = configuration(destinations);
    let put = match case.scope {
        Scope::Table => {
            emulator
                .put_table_replication(
                    case.region,
                    &[("tableArn", table_arn.to_string())],
                    &settings,
                )
                .await
        }
        Scope::Bucket => {
            emulator
                .put_table_bucket_replication(
                    case.region,
                    &[("tableBucketARN", bucket_arn.to_string())],
                    &settings,
                )
                .await
        }
    };
    assert_eq!(
        put.status, 200,
        "case {label:?}: the put must be accepted, got {status} {body}",
        status = put.status,
        body = put.body
    );
    put.text("versionToken")
}

/// Remove the configuration the row set, at the level it sits at.
async fn remove_configuration(
    emulator: &Emulator,
    case: &Case,
    bucket_arn: &str,
    table_arn: &str,
    token: &str,
) {
    let label = case.label;
    let removed = match case.scope {
        Scope::Table => {
            emulator
                .delete_table_replication(
                    case.region,
                    &[
                        ("tableArn", table_arn.to_string()),
                        ("versionToken", token.to_string()),
                    ],
                )
                .await
        }
        Scope::Bucket => {
            emulator
                .delete_table_bucket_replication(
                    case.region,
                    &[
                        ("tableBucketARN", bucket_arn.to_string()),
                        ("versionToken", token.to_string()),
                    ],
                )
                .await
        }
    };
    assert_eq!(
        removed.status, 204,
        "case {label:?}: removing the configuration must be accepted, got {status} {body}",
        status = removed.status,
        body = removed.body
    );
}

/// Assert `reply` is the modelled empty 204 of an accepted delete.
fn assert_deleted(reply: &Reply, case: &str) {
    assert_eq!(
        reply.status, 204,
        "case {case:?}: an accepted delete answers the modelled 204, got {status} {body}",
        status = reply.status,
        body = reply.body
    );
    assert!(
        reply.body.get("__type").is_none() && reply.body.get("message").is_none(),
        "case {case:?}: the modelled output is empty, got {body}",
        body = reply.body
    );
}

/// Assert `reply` is the modelled refusal `status`/`code`/`message` names.
fn assert_refused(reply: &Reply, status: u16, code: &str, message: &str, case: &str) {
    assert_eq!(
        reply.status, status,
        "case {case:?}: expected status {status}, got {actual} {body}",
        actual = reply.status,
        body = reply.body
    );
    assert_eq!(
        reply.error_code.as_deref(),
        Some(code),
        "case {case:?}: expected {code}, got {actual:?}",
        actual = reply.error_code
    );
    assert_eq!(
        reply.message(),
        message,
        "case {case:?}: expected the fixed sentence {message:?}, got {actual:?}",
        actual = reply.message()
    );
}

#[tokio::test]
async fn a_destination_no_replication_reached_holds_nothing_and_is_deleted() {
    let emulator = Emulator::new();
    for case in cases() {
        let label = case.label;
        let (bucket_arn, table_arn) = create_table(&emulator, &case).await;
        let destinations = destination_buckets(&emulator, &case).await;
        let token = configure(&emulator, &case, &bucket_arn, &table_arn, &destinations).await;
        if case.remove_configuration {
            remove_configuration(&emulator, &case, &bucket_arn, &table_arn, &token).await;
        }

        for destination in &destinations {
            // Configuring the replication copied nothing, so the destination
            // holds no namespace of this service's making...
            let held = listed_namespace_names(&emulator, case.region, destination, label).await;
            assert!(
                held.is_empty(),
                "case {label:?}: no replication to {destination} has been carried out, so it must \
                 hold no namespace, got {held:?}"
            );
            // ... and nothing in it stands in the way of its delete, which is
            // the modelled empty 204.
            let deleted = emulator.delete_table_bucket(case.region, destination).await;
            assert_deleted(&deleted, &format!("{label}, deleting {destination}"));
            // The bucket is gone, so a read of it is the modelled not-found:
            // the delete really removed it rather than answering 204 and
            // leaving it standing.
            assert_refused(
                &emulator.get_table_bucket(case.region, destination).await,
                404,
                "NotFoundException",
                NO_BUCKET,
                &format!("{label}, reading {destination} afterwards"),
            );
        }

        // And the source's own lifecycle runs to its end bottom-up, which is
        // the teardown an AWS client performs: the table, then the namespace it
        // lived in, then the table bucket.
        assert_deleted(
            &emulator
                .delete_table(case.region, &bucket_arn, case.namespace, case.table, &[])
                .await,
            &format!("{label}, deleting the source table"),
        );
        assert_deleted(
            &emulator
                .delete_namespace(case.region, &bucket_arn, case.namespace)
                .await,
            &format!("{label}, deleting the source namespace"),
        );
        assert_deleted(
            &emulator.delete_table_bucket(case.region, &bucket_arn).await,
            &format!("{label}, deleting the source bucket"),
        );
    }
}

#[tokio::test]
async fn a_destination_the_replication_reached_holds_the_replica_and_refuses_the_delete() {
    let emulator = Emulator::new();
    // Both levels a configuration sits at, because what the replication leaves
    // at a destination does not depend on which level configured it.
    for case in cases()
        .into_iter()
        .filter(|case| !case.remove_configuration && case.destinations == 1)
    {
        let label = case.label;
        let (bucket_arn, table_arn) = create_table(&emulator, &case).await;
        let destinations = destination_buckets(&emulator, &case).await;
        let destination = destinations[0].clone();
        configure(&emulator, &case, &bucket_arn, &table_arn, &destinations).await;
        let reported =
            await_replication_completed(&emulator, case.region, &table_arn, 1, label).await;
        assert_eq!(
            reported
                .first()
                .and_then(|entry| entry.get("replicationStatus"))
                .and_then(Value::as_str),
            Some(COMPLETED),
            "case {label:?}: the destination the replication reached reports it, got {reported:?}"
        );

        // The replication left the replica under the source table's own
        // namespace and name, so the destination holds both now.
        let held = listed_namespace_names(&emulator, case.region, &destination, label).await;
        assert!(
            held.contains(&case.namespace.to_string()),
            "case {label:?}: the replication to {destination} was carried out, so it holds the \
             source table's namespace {namespace:?}, got {held:?}",
            namespace = case.namespace
        );
        let replica = emulator
            .get_table(case.region, &destination, case.namespace, case.table)
            .await;
        assert_eq!(
            replica.status, 200,
            "case {label:?}: the replica in {destination} must resolve, got {status} {body}",
            status = replica.status,
            body = replica.body
        );

        // A bucket that holds something is not deleted: the refusal is the
        // modelled client-input error, stated with the one fixed sentence this
        // service states that class with, and it leaves the bucket standing.
        assert_refused(
            &emulator
                .delete_table_bucket(case.region, &destination)
                .await,
            400,
            "BadRequestException",
            BUCKET_NOT_EMPTY,
            &format!("{label}, deleting the destination that holds the replica"),
        );
        let still_there = emulator.get_table_bucket(case.region, &destination).await;
        assert_eq!(
            still_there.status, 200,
            "case {label:?}: the refused delete leaves {destination} standing, got {status} {body}",
            status = still_there.status,
            body = still_there.body
        );

        // Emptied bottom-up, the same bucket is deleted: the refusal was about
        // what the bucket held, not about the request.
        assert_deleted(
            &emulator
                .delete_table(case.region, &destination, case.namespace, case.table, &[])
                .await,
            &format!("{label}, deleting the replica"),
        );
        assert_refused(
            &emulator
                .delete_table_bucket(case.region, &destination)
                .await,
            400,
            "BadRequestException",
            BUCKET_NOT_EMPTY,
            &format!("{label}, deleting the destination that still holds the namespace"),
        );
        assert_deleted(
            &emulator
                .delete_namespace(case.region, &destination, case.namespace)
                .await,
            &format!("{label}, deleting the replica's namespace"),
        );
        assert_deleted(
            &emulator
                .delete_table_bucket(case.region, &destination)
                .await,
            &format!("{label}, deleting the emptied destination"),
        );
    }
}
