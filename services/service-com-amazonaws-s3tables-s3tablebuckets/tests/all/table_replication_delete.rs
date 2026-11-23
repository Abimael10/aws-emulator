// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `DeleteTableReplication`: the delete of a TABLE's
//! own replication configuration.
//!
//! The rule under test: a table's replication configuration is a record of that
//! table, and the delete of one is preconditioned on the `versionToken` the
//! configuration currently carries — the member is `@required` on this
//! operation's input. A delete presenting that token removes the record and
//! answers the modelled 204 with no body, after which the table has no
//! configuration of its own: the read of it, and any further delete, is the
//! modelled not-found stated with the one sentence this service states that
//! class with. Every other token is the modelled conflict (HTTP 409) and removes
//! nothing, whether it was superseded by a later write or never minted at all. A
//! delete addressed to a table the store does not hold is the modelled
//! not-found and changes nothing. What the delete removes is the TABLE's own
//! record alone: the configuration the table bucket holding it carries is left
//! exactly as it was, and the replicas the write that set the configuration
//! provisioned at the destinations remain, since "existing replicated copies
//! will remain in destination buckets" (`DeleteTableReplication`).
//!
//! The model is what bounds the shapes the rows span
//! (`smithy_query(query="operation", operation="DeleteTableReplication")` and
//! `smithy_query(query="closure", operation="PutTableReplication")`):
//! `versionToken` is a `@required` query member, `TableReplicationRules` is
//! `@length(1, 1)` and a rule's `destinations` `@length(1, 5)`. So the rows
//! differ in the region the table lives in, the role the configuration names,
//! how many destinations its one rule carries — one, several and the modelled
//! maximum of five — how many successive writes precede the delete, and whether
//! the table bucket holding the table carries a configuration of its own
//! besides. None of those may change what the delete removes or what it leaves.

use serde_json::{json, Value};

use super::harness::{Emulator, Reply, ACCOUNT};
use super::table_record_expiration::create_table;

/// The fixed sentence this service states a table that has no replication
/// configuration with.
const NO_REPLICATION_CONFIGURATION: &str = "The specified replication configuration does not exist.";

/// The fixed sentence this service states a table the store does not hold with.
const NO_TABLE: &str = "The specified table does not exist.";

/// The fixed sentence this service states a presented version token that is not
/// the current one with when the refusal is stated against the TABLE the delete
/// addresses, as `DeleteTableReplication`'s is.
const STALE_VERSION_TOKEN: &str = "Provided version token does not match the table version token.";

/// One row: a table in `bucket` whose own configuration names `role` and
/// `destinations` destination table buckets and is written `writes` times before
/// the delete.
#[derive(Debug)]
struct Case {
    label: &'static str,
    region: &'static str,
    /// The source table bucket's name — lowercase letters, numbers and hyphens,
    /// 3..63 characters, as `CreateTableBucket` accepts one.
    bucket: &'static str,
    /// The table's name (`^[0-9a-z_]*$`).
    table: &'static str,
    /// An IAM role ARN of the form the modelled `IAMRole` pattern accepts
    /// (`^arn:.+:iam::[0-9]{12}:role/.+$`, length 20..2048).
    role: &'static str,
    /// How many destination table buckets the one rule carries;
    /// `ReplicationDestinations` is `@length(min: 1, max: 5)`.
    destinations: usize,
    /// How many accepted writes precede the delete, so the token the delete
    /// presents is the first write's in some rows and a later one's in others —
    /// and the rows with more than one write have a superseded token to probe.
    writes: usize,
    /// Whether the table BUCKET holding the table also carries a replication
    /// configuration, which the delete of the table's own must leave alone.
    bucket_configuration: bool,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "one destination, a single write",
            region: "us-east-1",
            bucket: "table-repl-del-east",
            table: "orders_east",
            role: "arn:aws:iam::000000000000:role/s3tables-replication-delete",
            destinations: 1,
            writes: 1,
            bucket_configuration: false,
        },
        Case {
            label: "two destinations, two writes, the bucket configured as well",
            region: "us-west-2",
            bucket: "table-repl-del-west",
            table: "orders_west",
            role: "arn:aws:iam::000000000000:role/replication-delete-west",
            destinations: 2,
            writes: 2,
            bucket_configuration: true,
        },
        Case {
            label: "three destinations, three writes, a role under a path",
            region: "eu-west-1",
            bucket: "table-repl-del-eu",
            table: "clicks_eu",
            role: "arn:aws:iam::000000000000:role/replication/eu/nested-path",
            destinations: 3,
            writes: 3,
            bucket_configuration: false,
        },
        Case {
            label: "the modelled maximum of five destinations, two writes, the bucket configured",
            region: "ap-south-1",
            bucket: "table-repl-del-ap",
            table: "sensor_readings",
            role: "arn:aws:iam::000000000000:role/replication-delete-ap",
            destinations: 5,
            writes: 2,
            bucket_configuration: true,
        },
    ]
}

/// A configuration whose ONE rule — `TableReplicationRules` is `@length(1, 1)` —
/// replicates to `destinations`.
fn configuration(role: &str, destinations: &[String]) -> Value {
    json!({
        "role": role,
        "rules": [{
            "destinations": destinations
                .iter()
                .map(|arn| json!({ "destinationTableBucketARN": arn }))
                .collect::<Vec<Value>>(),
        }],
    })
}

/// Create `count` destination table buckets named after `prefix` in `region`, and
/// answer their ARNs.
async fn destination_buckets(
    emulator: &Emulator,
    region: &str,
    prefix: &str,
    count: usize,
    case: &str,
) -> Vec<String> {
    let mut arns = Vec::with_capacity(count);
    for index in 0..count {
        let name = format!("{prefix}-{index}");
        let created = emulator
            .create_table_bucket(region, &json!({ "name": name }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: creating the destination bucket {name:?} must succeed, got {status} \
             {body}",
            status = created.status,
            body = created.body
        );
        arns.push(created.text("arn"));
    }
    arns
}

/// Assert `reply` is the modelled refusal `status`/`code`/`message` names, and
/// that it reports nothing of a configuration.
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
    assert!(
        reply.body.get("configuration").is_none(),
        "case {case:?}: a refusal reports nothing of the configuration, got {body}",
        body = reply.body
    );
}

/// Assert the modelled 204 of an accepted delete, which carries no body members.
fn assert_deleted(reply: &Reply, case: &str) {
    assert_eq!(
        reply.status, 204,
        "case {case:?}: an accepted delete answers the modelled 204, got {status} {body}",
        status = reply.status,
        body = reply.body
    );
    assert!(
        reply.body.get("versionToken").is_none() && reply.body.get("configuration").is_none(),
        "case {case:?}: the modelled output is empty, got {body}",
        body = reply.body
    );
}

/// The version token the read of `table_arn`'s configuration reports.
async fn reported_token(emulator: &Emulator, region: &str, table_arn: &str, case: &str) -> String {
    let read = emulator.get_table_replication(region, table_arn).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: the read must be served, got {status} {body}",
        status = read.status,
        body = read.body
    );
    read.text("versionToken")
}

/// What one row set up: the table, the configuration its writes applied, every
/// token an earlier write handed back and the token that is current, beside the
/// destinations and the bucket's own configuration when the row carries one.
struct Written {
    label: &'static str,
    region: &'static str,
    table: &'static str,
    table_arn: String,
    bucket_arn: String,
    destinations: Vec<String>,
    configuration: Value,
    current: String,
    superseded: Vec<String>,
    bucket_token: Option<String>,
}

/// Apply each row's writes, chaining the token the previous write reported into
/// the next one, and configure the table BUCKET too where the row says so.
async fn write_rows(emulator: &Emulator) -> Vec<Written> {
    let mut written = Vec::new();
    for case in cases() {
        let label = case.label;
        let table_arn = create_table(emulator, case.region, case.bucket, case.table).await;
        let bucket_arn = table_arn
            .split("/table/")
            .next()
            .expect("a table ARN carries its bucket's")
            .to_string();
        let destinations = destination_buckets(
            emulator,
            case.region,
            &format!("{bucket}-dst", bucket = case.bucket),
            case.destinations,
            label,
        )
        .await;
        let settings = configuration(case.role, &destinations);

        let mut tokens: Vec<String> = Vec::new();
        for write in 0..case.writes {
            let mut query = vec![("tableArn", table_arn.clone())];
            if let Some(held) = tokens.last() {
                query.push(("versionToken", held.clone()));
            }
            let put = emulator
                .put_table_replication(case.region, &query, &settings)
                .await;
            assert_eq!(
                put.status, 200,
                "case {label:?}: write {write} must be accepted, got {status} {body}",
                status = put.status,
                body = put.body
            );
            tokens.push(put.text("versionToken"));
        }

        // A configuration on the BUCKET, where the row carries one: it covers
        // the bucket's tables and is a record of its own, so deleting the
        // table's must leave it untouched.
        let bucket_token = if case.bucket_configuration {
            let for_bucket = destination_buckets(
                emulator,
                case.region,
                &format!("{bucket}-bkt", bucket = case.bucket),
                1,
                label,
            )
            .await;
            let put = emulator
                .put_table_bucket_replication(
                    case.region,
                    &[("tableBucketARN", bucket_arn.clone())],
                    &configuration(case.role, &for_bucket),
                )
                .await;
            assert_eq!(
                put.status, 200,
                "case {label:?}: the bucket's own put must be accepted, got {status} {body}",
                status = put.status,
                body = put.body
            );
            Some(put.text("versionToken"))
        } else {
            None
        };

        let current = tokens.pop().expect("every row performs at least one write");
        written.push(Written {
            label,
            region: case.region,
            table: case.table,
            table_arn,
            bucket_arn,
            destinations,
            configuration: settings,
            current,
            superseded: tokens,
            bucket_token,
        });
    }
    written
}

#[tokio::test]
async fn a_token_that_is_not_the_configurations_is_the_modelled_conflict_and_removes_nothing() {
    let emulator = Emulator::new();
    for row in &write_rows(&emulator).await {
        // Every token an earlier write handed back now names a configuration a
        // later write superseded, plus one token nothing ever minted.
        let mut probes: Vec<(String, String)> = row
            .superseded
            .iter()
            .enumerate()
            .map(|(index, stale)| {
                (
                    format!("{label} (the token of write {index})", label = row.label),
                    stale.clone(),
                )
            })
            .collect();
        probes.push((
            format!("{label} (a token nothing minted)", label = row.label),
            format!("{current}-x", current = row.current),
        ));

        for (case, presented) in probes {
            let refused = emulator
                .delete_table_replication(
                    row.region,
                    &[
                        ("tableArn", row.table_arn.clone()),
                        ("versionToken", presented),
                    ],
                )
                .await;
            assert_refused(
                &refused,
                409,
                "ConflictException",
                STALE_VERSION_TOKEN,
                &case,
            );
            assert_eq!(
                reported_token(&emulator, row.region, &row.table_arn, &case).await,
                row.current,
                "case {case:?}: the refused delete must leave the configuration in place"
            );
        }
    }
}

#[tokio::test]
async fn the_current_token_removes_the_configuration_and_leaves_nothing_to_delete() {
    let emulator = Emulator::new();
    for row in &write_rows(&emulator).await {
        let case = format!("{label} (the current token)", label = row.label);
        let deleted = emulator
            .delete_table_replication(
                row.region,
                &[
                    ("tableArn", row.table_arn.clone()),
                    ("versionToken", row.current.clone()),
                ],
            )
            .await;
        assert_deleted(&deleted, &case);

        // The record is gone, so the read of it is the not-found of a table
        // with no configuration of its own — which is how a client observes
        // that table replication is no longer configured.
        let read = emulator
            .get_table_replication(row.region, &row.table_arn)
            .await;
        assert_refused(
            &read,
            404,
            "NotFoundException",
            NO_REPLICATION_CONFIGURATION,
            &format!("{case}, the read afterwards"),
        );

        // And there is nothing left for a second delete to remove, whichever
        // token it presents.
        for (again, presented) in [
            ("the same token again", row.current.clone()),
            ("another token", format!("{current}-x", current = row.current)),
        ] {
            let refused = emulator
                .delete_table_replication(
                    row.region,
                    &[
                        ("tableArn", row.table_arn.clone()),
                        ("versionToken", presented),
                    ],
                )
                .await;
            assert_refused(
                &refused,
                404,
                "NotFoundException",
                NO_REPLICATION_CONFIGURATION,
                &format!("{case}, deleting again with {again}"),
            );
        }

        // A put that presents no token at all configures the table afresh, so
        // the delete removed the record rather than the table's ability to hold
        // one, and the token it mints is not the deleted configuration's.
        let recreated = emulator
            .put_table_replication(
                row.region,
                &[("tableArn", row.table_arn.clone())],
                &row.configuration,
            )
            .await;
        assert_eq!(
            recreated.status, 200,
            "case {case:?}: the table must be configurable again, got {status} {body}",
            status = recreated.status,
            body = recreated.body
        );
        let token = recreated.text("versionToken");
        assert_ne!(
            token, row.current,
            "case {case:?}: the recreated configuration must carry a fresh token"
        );
        assert_eq!(
            reported_token(&emulator, row.region, &row.table_arn, &case).await,
            token,
            "case {case:?}: the read must report the recreated configuration's token"
        );
    }
}

#[tokio::test]
async fn the_delete_removes_the_tables_own_record_and_nothing_else() {
    let emulator = Emulator::new();
    for row in &write_rows(&emulator).await {
        let case = format!("{label} (what the delete leaves)", label = row.label);
        let deleted = emulator
            .delete_table_replication(
                row.region,
                &[
                    ("tableArn", row.table_arn.clone()),
                    ("versionToken", row.current.clone()),
                ],
            )
            .await;
        assert_deleted(&deleted, &case);

        // The replica the write that set the configuration provisioned at each
        // destination stands where it stood: the delete stops further updates
        // being replicated, it does not remove what was replicated already.
        let namespace = format!("{table}_ns", table = row.table);
        for destination in &row.destinations {
            let replica = emulator
                .get_table(row.region, destination, &namespace, row.table)
                .await;
            assert_eq!(
                replica.status, 200,
                "case {case:?}: the replica in {destination:?} must remain, got {status} {body}",
                status = replica.status,
                body = replica.body
            );
        }

        // The table itself is untouched, and so is the configuration the table
        // BUCKET carries where the row set one: each record is deleted on its
        // own.
        let table = emulator
            .get_table_by_arn(row.region, &row.table_arn)
            .await;
        assert_eq!(
            table.status, 200,
            "case {case:?}: the table itself must remain, got {status} {body}",
            status = table.status,
            body = table.body
        );
        let bucket_read = emulator
            .get_table_bucket_replication(row.region, &row.bucket_arn)
            .await;
        match &row.bucket_token {
            Some(token) => {
                assert_eq!(
                    bucket_read.status, 200,
                    "case {case:?}: the bucket's own configuration must remain, got {status} \
                     {body}",
                    status = bucket_read.status,
                    body = bucket_read.body
                );
                assert_eq!(
                    bucket_read.text("versionToken"),
                    *token,
                    "case {case:?}: the bucket's configuration must carry the token its own write \
                     minted, got {actual:?}",
                    actual = bucket_read.text("versionToken")
                );
            }
            None => assert_refused(
                &bucket_read,
                404,
                "NotFoundException",
                NO_REPLICATION_CONFIGURATION,
                &format!("{case}, a bucket nothing configured"),
            ),
        }
    }
}

#[tokio::test]
async fn a_delete_addressed_to_a_table_the_store_does_not_hold_changes_nothing() {
    let region = "us-east-1";
    let role = "arn:aws:iam::000000000000:role/replication-delete-absent";
    let emulator = Emulator::new();
    let table_arn = create_table(&emulator, region, "table-repl-del-absent", "held_table").await;
    let destinations =
        destination_buckets(&emulator, region, "table-repl-del-absent-dst", 1, "setup").await;
    let put = emulator
        .put_table_replication(
            region,
            &[("tableArn", table_arn.clone())],
            &configuration(role, &destinations),
        )
        .await;
    assert_eq!(
        put.status, 200,
        "the setup put must be served, got {status} {body}",
        status = put.status,
        body = put.body
    );
    let token = put.text("versionToken");

    let bucket_arn = table_arn
        .split("/table/")
        .next()
        .expect("a table ARN carries its bucket's")
        .to_string();
    // Table ids of the form the modelled `TableARN` pattern accepts
    // (`[a-zA-Z0-9-_]{1,255}`) that nothing was ever minted for: one in the
    // bucket that is there, one in a bucket that never was.
    let rows: Vec<(&str, String)> = vec![
        (
            "a table id the bucket holds no table under",
            format!("{bucket_arn}/table/11111111-2222-4333-8444-555555555555"),
        ),
        (
            "a table in a bucket that was never created",
            format!(
                "arn:aws:s3tables:{region}:{ACCOUNT}:bucket/table-repl-del-never/table/\
                 66666666-7777-4888-8999-aaaaaaaaaaaa"
            ),
        ),
    ];

    for (case, addressed) in rows {
        let refused = emulator
            .delete_table_replication(
                region,
                &[
                    ("tableArn", addressed.clone()),
                    ("versionToken", token.clone()),
                ],
            )
            .await;
        assert_refused(&refused, 404, "NotFoundException", NO_TABLE, case);
        // The configuration of the table that IS held is untouched: a delete
        // addressed elsewhere removes nothing.
        assert_eq!(
            reported_token(&emulator, region, &table_arn, case).await,
            token,
            "case {case:?}: the held table's configuration must be left in place"
        );
    }
}

#[tokio::test]
async fn a_table_configured_only_by_its_bucket_has_no_record_of_its_own_to_delete() {
    let region = "us-east-1";
    let role = "arn:aws:iam::000000000000:role/replication-delete-bucket-only";
    let emulator = Emulator::new();
    let table_arn = create_table(&emulator, region, "table-repl-del-bkt", "bucket_only").await;
    let bucket_arn = table_arn
        .split("/table/")
        .next()
        .expect("a table ARN carries its bucket's")
        .to_string();
    let destinations =
        destination_buckets(&emulator, region, "table-repl-del-bkt-dst", 1, "setup").await;
    let put = emulator
        .put_table_bucket_replication(
            region,
            &[("tableBucketARN", bucket_arn.clone())],
            &configuration(role, &destinations),
        )
        .await;
    assert_eq!(
        put.status, 200,
        "the bucket's own put must be served, got {status} {body}",
        status = put.status,
        body = put.body
    );
    let bucket_token = put.text("versionToken");

    // The bucket's configuration is not the table's own record, so there is
    // nothing for the table's delete to remove — whichever token it presents,
    // the bucket's included.
    for (case, presented) in [
        ("the bucket configuration's own token", bucket_token.clone()),
        ("a token nothing minted", format!("{bucket_token}-x")),
    ] {
        let refused = emulator
            .delete_table_replication(
                region,
                &[
                    ("tableArn", table_arn.clone()),
                    ("versionToken", presented),
                ],
            )
            .await;
        assert_refused(
            &refused,
            404,
            "NotFoundException",
            NO_REPLICATION_CONFIGURATION,
            case,
        );
        let bucket_read = emulator
            .get_table_bucket_replication(region, &bucket_arn)
            .await;
        assert_eq!(
            bucket_read.status, 200,
            "case {case:?}: the bucket's configuration must remain, got {status} {body}",
            status = bucket_read.status,
            body = bucket_read.body
        );
        assert_eq!(
            bucket_read.text("versionToken"),
            bucket_token,
            "case {case:?}: the bucket's configuration must keep its own token, got {actual:?}",
            actual = bucket_read.text("versionToken")
        );
    }
}
