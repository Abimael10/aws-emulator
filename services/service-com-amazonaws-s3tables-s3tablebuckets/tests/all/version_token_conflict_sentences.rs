// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of WHICH sentence a version-token conflict carries.
//!
//! The rule under test: a request presenting a `versionToken` that is not the
//! current one is refused with the modelled `ConflictException` (HTTP 409)
//! stated by the ONE fixed sentence this service words that refusal with for
//! the operation at hand, and nothing is written. This service words it two
//! ways, and the two are not interchangeable: an operation whose refusal is
//! stated against the TABLE the request addresses — `DeleteTable`,
//! `RenameTable`, `UpdateTableMetadataLocation` and the delete of a table's own
//! replication record, `DeleteTableReplication` — carries the table-version-token
//! sentence, while an operation that writes a replication CONFIGURATION under a
//! precondition (`PutTableReplication`, `PutTableBucketReplication`) and the
//! delete of a table BUCKET's configuration carry the configuration sentence.
//! Both sentences are entirely fixed text: neither the presented token, the
//! current one, the table nor the bucket appears in either.
//!
//! The model is what bounds the shapes the rows span
//! (`smithy_query(query="operation", operation="DeleteTableReplication")` and
//! `smithy_query(query="closure", operation="PutTableReplication")`):
//! `versionToken` is a `@required` query member of the table-replication delete
//! and an optional one of the puts, `TableReplicationRules` is `@length(1, 1)`
//! and a rule's `destinations` `@length(1, 5)`. So the rows differ in the region
//! the table lives in, the role the configuration names, how many destinations
//! its one rule carries, how many accepted writes precede the probe — which
//! decides how many superseded tokens there are — and which form the presented
//! token takes: one a later write superseded, one minted for another record of
//! the same table, and one nothing ever minted.

use serde_json::{json, Value};
use service_com_amazonaws_s3tables_s3tablebuckets::state::{
    REPLICATION_CONFIGURATION_NOT_FOUND_MESSAGE, TABLE_VERSION_TOKEN_MISMATCH_MESSAGE,
    VERSION_TOKEN_MISMATCH_MESSAGE,
};

use crate::harness::{Emulator, Reply};
use crate::table_record_expiration::create_table;

/// A token this service never minted for anything.
const UNMINTED_TOKEN: &str = "not_a_version_token_this_service_minted";

/// One row: a table in `bucket` whose own replication configuration names
/// `role` and `destinations` destination table buckets and is written `writes`
/// times before the probe; the table BUCKET holding it carries a configuration
/// of its own besides, so a token minted for one record can be presented to the
/// other.
#[derive(Debug)]
struct Case {
    label: &'static str,
    region: &'static str,
    /// The source table bucket's name — lowercase letters, digits and hyphens,
    /// 3..63 characters, as `CreateTableBucket` accepts one.
    bucket: &'static str,
    /// The table's name (`^[0-9a-z_]*$`).
    table: &'static str,
    /// An IAM role ARN of the form the modelled `IAMRole` pattern accepts
    /// (`^arn:.+:iam::[0-9]{12}:role/.+$`).
    role: &'static str,
    /// How many destination table buckets the one rule carries;
    /// `ReplicationDestinations` is `@length(min: 1, max: 5)`.
    destinations: usize,
    /// How many accepted writes precede the probe, so some rows have one
    /// superseded token to present and others several.
    writes: usize,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "one destination, a single write",
            region: "us-east-1",
            bucket: "token-sentence-east",
            table: "orders_east",
            role: "arn:aws:iam::000000000000:role/s3tables-token-sentence",
            destinations: 1,
            writes: 1,
        },
        Case {
            label: "three destinations, three writes, a role under a path",
            region: "eu-west-1",
            bucket: "token-sentence-eu",
            table: "clicks_eu",
            role: "arn:aws:iam::000000000000:role/sentence/eu/nested-path",
            destinations: 3,
            writes: 3,
        },
        Case {
            label: "the modelled maximum of five destinations, two writes",
            region: "ap-south-1",
            bucket: "token-sentence-ap",
            table: "sensor_readings",
            role: "arn:aws:iam::000000000000:role/sentence-ap",
            destinations: 5,
            writes: 2,
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

/// Create `count` destination table buckets named after `prefix`, and answer
/// their ARNs.
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

/// What one row set up: the table and its coordinates, the configuration its
/// writes applied, the token of the write that is current, every token an
/// earlier write handed back, and the tokens the OTHER records of the same
/// table carry — the table's own and its bucket's configuration's.
struct Written {
    label: &'static str,
    region: &'static str,
    role: &'static str,
    namespace: String,
    table: &'static str,
    table_arn: String,
    bucket_arn: String,
    configuration: Value,
    warehouse: String,
    current: String,
    superseded: Vec<String>,
    bucket_token: String,
    table_version_token: String,
}

/// Apply one row: the table, its replication configuration (written `writes`
/// times, chaining the token the previous write reported into the next) and a
/// configuration on the table bucket holding it.
async fn write_row(emulator: &Emulator, case: &Case) -> Written {
    let label = case.label;
    let table_arn = create_table(emulator, case.region, case.bucket, case.table).await;
    let bucket_arn = table_arn
        .split("/table/")
        .next()
        .expect("a table ARN carries its bucket's")
        .to_string();
    let namespace = format!("{table}_ns", table = case.table);
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
    let current = tokens.pop().expect("every row performs at least one write");

    let for_bucket = destination_buckets(
        emulator,
        case.region,
        &format!("{bucket}-bkt", bucket = case.bucket),
        1,
        label,
    )
    .await;
    let bucket_put = emulator
        .put_table_bucket_replication(
            case.region,
            &[("tableBucketARN", bucket_arn.clone())],
            &configuration(case.role, &for_bucket),
        )
        .await;
    assert_eq!(
        bucket_put.status, 200,
        "case {label:?}: the bucket's own put must be accepted, got {status} {body}",
        status = bucket_put.status,
        body = bucket_put.body
    );

    let read = emulator
        .get_table_metadata_location(case.region, &bucket_arn, &namespace, case.table)
        .await;
    assert_eq!(
        read.status, 200,
        "case {label:?}: the table's metadata location must be served, got {status} {body}",
        status = read.status,
        body = read.body
    );

    Written {
        label,
        region: case.region,
        role: case.role,
        namespace,
        table: case.table,
        table_arn,
        bucket_arn,
        configuration: settings,
        warehouse: read.text("warehouseLocation"),
        current,
        superseded: tokens,
        bucket_token: bucket_put.text("versionToken"),
        table_version_token: read.text("versionToken"),
    }
}

/// Assert `reply` is the modelled conflict stated by `message`, with no value of
/// the request or of the store in the sentence.
fn assert_conflict(reply: &Reply, message: &str, row: &Written, presented: &str, case: &str) {
    assert_eq!(
        reply.status, 409,
        "case {case:?}: expected status 409, got {actual} {body}",
        actual = reply.status,
        body = reply.body
    );
    assert_eq!(
        reply.error_code.as_deref(),
        Some("ConflictException"),
        "case {case:?}: expected ConflictException, got {actual:?}",
        actual = reply.error_code
    );
    assert_eq!(
        reply.message(),
        message,
        "case {case:?}: expected the fixed sentence {message:?}, got {actual:?}",
        actual = reply.message()
    );
    for value in [
        presented,
        row.current.as_str(),
        row.table_arn.as_str(),
        row.table,
    ] {
        assert!(
            !reply.message().contains(value),
            "case {case:?}: the sentence must carry no value of the request or the store, got \
             {value:?} in {actual:?}",
            actual = reply.message()
        );
    }
}

/// The role a replication read reports.
fn reported_role(reply: &Reply, case: &str) -> String {
    reply
        .body
        .get("configuration")
        .and_then(|configuration| configuration.get("role"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the read must report the configuration's role, got {body}",
                body = reply.body
            )
        })
        .to_string()
}

/// Assert every record of the row is exactly as its last accepted write left
/// it: the table's configuration, the bucket's, and the table itself.
async fn assert_unchanged(emulator: &Emulator, row: &Written, case: &str) {
    let read = emulator
        .get_table_replication(row.region, &row.table_arn)
        .await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: the table's configuration must still be served, got {status} {body}",
        status = read.status,
        body = read.body
    );
    assert_eq!(
        read.text("versionToken"),
        row.current,
        "case {case:?}: the refusal must leave the configuration's token as it was"
    );
    assert_eq!(
        reported_role(&read, case),
        row.role,
        "case {case:?}: the refusal must leave the configuration's role as it was"
    );

    let bucket_read = emulator
        .get_table_bucket_replication(row.region, &row.bucket_arn)
        .await;
    assert_eq!(
        bucket_read.status, 200,
        "case {case:?}: the bucket's configuration must still be served, got {status} {body}",
        status = bucket_read.status,
        body = bucket_read.body
    );
    assert_eq!(
        bucket_read.text("versionToken"),
        row.bucket_token,
        "case {case:?}: the refusal must leave the bucket configuration's token as it was"
    );

    let table = emulator.get_table_by_arn(row.region, &row.table_arn).await;
    assert_eq!(
        table.status, 200,
        "case {case:?}: the table itself must still be served, got {status} {body}",
        status = table.status,
        body = table.body
    );
    assert_eq!(
        table.text("name"),
        row.table,
        "case {case:?}: the refusal must leave the table where it is"
    );
    assert_eq!(
        table.text("versionToken"),
        row.table_version_token,
        "case {case:?}: the refusal must leave the table's own token as it was"
    );
}

/// Which operation a row probes. The sentence each one states its version-token
/// conflict with is what [`sentence`] answers.
#[derive(Debug, Clone, Copy)]
enum Probe {
    DeleteTableReplication,
    DeleteTable,
    UpdateTableMetadataLocation,
    RenameTable,
    PutTableReplication,
    PutTableBucketReplication,
    DeleteTableBucketReplication,
}

/// Every operation of this service that preconditions a write on a presented
/// `versionToken`.
const PROBES: [Probe; 7] = [
    Probe::DeleteTableReplication,
    Probe::DeleteTable,
    Probe::UpdateTableMetadataLocation,
    Probe::RenameTable,
    Probe::PutTableReplication,
    Probe::PutTableBucketReplication,
    Probe::DeleteTableBucketReplication,
];

/// The sentence the refusal of `probe` carries: the table-version-token one
/// where the refusal is stated against the table the request addresses, and the
/// configuration one where it is stated against a replication configuration.
fn sentence(probe: Probe) -> &'static str {
    match probe {
        Probe::DeleteTableReplication
        | Probe::DeleteTable
        | Probe::UpdateTableMetadataLocation
        | Probe::RenameTable => TABLE_VERSION_TOKEN_MISMATCH_MESSAGE,
        Probe::PutTableReplication
        | Probe::PutTableBucketReplication
        | Probe::DeleteTableBucketReplication => VERSION_TOKEN_MISMATCH_MESSAGE,
    }
}

/// Call `probe` against `row` presenting `token`, with every other member of the
/// request one the operation accepts — so the token is the only reason the call
/// can be refused.
async fn probe(emulator: &Emulator, row: &Written, probe: Probe, token: &str) -> Reply {
    match probe {
        Probe::DeleteTableReplication => {
            emulator
                .delete_table_replication(
                    row.region,
                    &[
                        ("tableArn", row.table_arn.clone()),
                        ("versionToken", token.to_string()),
                    ],
                )
                .await
        }
        Probe::DeleteTable => {
            emulator
                .delete_table(
                    row.region,
                    &row.bucket_arn,
                    &row.namespace,
                    row.table,
                    &[("versionToken", token.to_string())],
                )
                .await
        }
        Probe::UpdateTableMetadataLocation => {
            emulator
                .update_table_metadata_location(
                    row.region,
                    &row.bucket_arn,
                    &row.namespace,
                    row.table,
                    &json!({
                        "versionToken": token,
                        "metadataLocation": format!(
                            "{warehouse}/metadata/00001-moved.metadata.json",
                            warehouse = row.warehouse,
                        ),
                    }),
                )
                .await
        }
        Probe::RenameTable => {
            emulator
                .rename_table(
                    row.region,
                    &row.bucket_arn,
                    &row.namespace,
                    row.table,
                    &json!({
                        "newName": format!("{table}_renamed", table = row.table),
                        "versionToken": token,
                    }),
                )
                .await
        }
        Probe::PutTableReplication => {
            emulator
                .put_table_replication(
                    row.region,
                    &[
                        ("tableArn", row.table_arn.clone()),
                        ("versionToken", token.to_string()),
                    ],
                    &row.configuration,
                )
                .await
        }
        Probe::PutTableBucketReplication => {
            emulator
                .put_table_bucket_replication(
                    row.region,
                    &[
                        ("tableBucketARN", row.bucket_arn.clone()),
                        ("versionToken", token.to_string()),
                    ],
                    &row.configuration,
                )
                .await
        }
        Probe::DeleteTableBucketReplication => {
            emulator
                .delete_table_bucket_replication(
                    row.region,
                    &[
                        ("tableBucketARN", row.bucket_arn.clone()),
                        ("versionToken", token.to_string()),
                    ],
                )
                .await
        }
    }
}

#[tokio::test]
async fn each_operation_states_its_version_token_conflict_with_its_own_fixed_sentence() {
    // The two sentences are distinct: an operation stating the other one is the
    // defect this suite guards, so conflating them must not pass unnoticed.
    assert_ne!(
        TABLE_VERSION_TOKEN_MISMATCH_MESSAGE, VERSION_TOKEN_MISMATCH_MESSAGE,
        "the service words the two version-token refusals differently"
    );

    let emulator = Emulator::new();
    for case in cases() {
        let row = write_row(&emulator, &case).await;
        for probed in PROBES {
            // Three forms of a token that is not the current one: the one the
            // first write handed back where the row has one, a token minted for
            // another record of the same table, and a token nothing minted.
            let mut presented: Vec<(String, String)> = row
                .superseded
                .iter()
                .enumerate()
                .map(|(index, stale)| {
                    (
                        format!("the token of write {index}"),
                        stale.clone(),
                    )
                })
                .collect();
            presented.push((
                "a token minted for another record of the table".to_string(),
                // The token of the record the probe does NOT precondition on:
                // the table's own token where the precondition is a replication
                // configuration's, and the table's configuration's token where
                // it is the table's or the bucket configuration's.
                match probed {
                    Probe::DeleteTableReplication | Probe::PutTableReplication => {
                        row.table_version_token.clone()
                    }
                    _ => row.current.clone(),
                },
            ));
            presented.push(("a token nothing minted".to_string(), UNMINTED_TOKEN.to_string()));

            for (form, token) in presented {
                let case = format!(
                    "{label} / {probed:?} / {form}",
                    label = row.label
                );
                let refused = probe(&emulator, &row, probed, &token).await;
                assert_conflict(&refused, sentence(probed), &row, &token, &case);
                assert_unchanged(&emulator, &row, &case).await;
            }
        }
    }
}

#[tokio::test]
async fn the_current_token_is_accepted_after_every_refused_table_replication_delete() {
    let emulator = Emulator::new();
    for case in cases() {
        let row = write_row(&emulator, &case).await;

        // Every stale form refused above leaves the configuration deletable,
        // which is what makes the refusal a precondition rather than a failure:
        // the caller re-reads the token and retries.
        for (form, token) in [
            ("a token nothing minted", UNMINTED_TOKEN.to_string()),
            (
                "the table's own version token",
                row.table_version_token.clone(),
            ),
            ("the bucket configuration's token", row.bucket_token.clone()),
        ] {
            let case = format!("{label} / {form}", label = row.label);
            let refused = probe(
                &emulator,
                &row,
                Probe::DeleteTableReplication,
                &token,
            )
            .await;
            assert_conflict(
                &refused,
                TABLE_VERSION_TOKEN_MISMATCH_MESSAGE,
                &row,
                &token,
                &case,
            );
        }

        let case = format!("{label} / the current token", label = row.label);
        let deleted = probe(
            &emulator,
            &row,
            Probe::DeleteTableReplication,
            &row.current,
        )
        .await;
        assert_eq!(
            deleted.status, 204,
            "case {case:?}: the delete presenting the current token answers the modelled 204, got \
             {status} {body}",
            status = deleted.status,
            body = deleted.body
        );

        let read = emulator
            .get_table_replication(row.region, &row.table_arn)
            .await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: the table must hold no configuration afterwards, got {status} {body}",
            status = read.status,
            body = read.body
        );
        assert_eq!(
            read.message(),
            REPLICATION_CONFIGURATION_NOT_FOUND_MESSAGE,
            "case {case:?}: expected the fixed sentence {REPLICATION_CONFIGURATION_NOT_FOUND_MESSAGE:?}, \
             got {actual:?}",
            actual = read.message()
        );
    }
}
