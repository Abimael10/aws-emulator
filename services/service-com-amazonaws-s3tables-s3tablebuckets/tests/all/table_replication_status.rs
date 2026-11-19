// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `GetTableReplicationStatus`: what replication of
//! a table to each of its destinations reports.
//!
//! The rule under test: the read reports one entry per destination of the
//! configuration that replicates the table — the table's OWN configuration once
//! a put sets one on it, else the configuration of the table bucket holding it,
//! whose rules cover that bucket's tables — in the order that configuration's
//! rule holds them, and every member of an entry is derived from state and the
//! clock: a destination is pending until it holds the replica of the table and
//! the replication of the table's current state to it is due, completed once
//! both hold — reporting the metadata document it replicated, the instant it
//! completed and the replica's own ARN — and failed when the store holds no
//! such destination table bucket at all, stated with the sentence this service
//! states that class with. A table nothing replicates has no destination to
//! report — the modelled `destinations` is required and `@length(min: 1, max:
//! 5)` — so the read is refused with the modelled not-found, and so is a read
//! addressed to a table the store does not hold: which of the two resources a
//! `tableArn` names is missing — the table bucket or the table inside it — is
//! which resource that refusal is stated about. The read writes nothing, so two
//! reads of a table nobody reconfigured answer the same.
//!
//! The shapes the rows span are the model's own
//! (`smithy_query(query="closure", operation="GetTableReplicationStatus")`):
//! `ReplicationDestinationStatuses` is `@length(1, 5)` and `ReplicationStatus`
//! declares `pending`, `completed` and `failed`. So the rows differ in the
//! region, which level the configuration that replicates the table sits at, how
//! many destinations it names — one, several and the modelled maximum of five —
//! how many of those destination buckets the store holds, and whether the table
//! has a metadata file to replicate at all.
//! `replication_to_a_destination_holding_the_replica_completes_and_the_read_reports_it`
//! is where the progression to completed is pinned, by polling for it.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use super::harness::{timestamp_seconds, Emulator, Reply, ACCOUNT};
use super::wire_forms::create_table_request;

/// The fixed sentence this service states a table nothing replicates with.
const NO_REPLICATION_CONFIGURATION: &str = "The specified replication configuration does not exist.";

/// The fixed sentence this service states a table the store does not hold with.
const NO_TABLE: &str = "The specified table does not exist.";

/// The fixed sentence this service states a table bucket the store does not hold
/// with, which is what a destination that cannot be replicated to states.
const NO_BUCKET: &str = "The specified bucket does not exist.";

/// The `ReplicationStatus` values this emulator's destinations reach, from
/// `smithy_query(query="shape",shape="ReplicationStatus")`.
const PENDING: &str = "pending";
const COMPLETED: &str = "completed";
const FAILED: &str = "failed";

/// How long a case waits for a due replication to complete before giving up.
/// GENEROUS on purpose: the test polls for the condition and asserts nothing
/// about how long it took, so a loaded host only makes it poll more often.
const COMPLETION_DEADLINE: Duration = Duration::from_secs(120);

/// How long a case waits between two polls of a destination's state.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The IAM role every configuration in this suite replicates under, of the form
/// the modelled `IAMRole` pattern accepts (`^arn:.+:iam::[0-9]{12}:role/.+$`).
const ROLE: &str = "arn:aws:iam::000000000000:role/s3tables-replication-status";

/// Which level the configuration that replicates the table sits at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// The TABLE's own configuration (`PutTableReplication`).
    Table,
    /// The configuration of the table bucket holding it
    /// (`PutTableBucketReplication`), which covers that bucket's tables.
    Bucket,
}

/// One row: a table in `bucket` replicated by a configuration at `scope` naming
/// `held` destination table buckets that exist and `absent` that do not.
#[derive(Debug)]
struct Case {
    label: &'static str,
    region: &'static str,
    /// The source table bucket's name — lowercase letters, numbers and hyphens,
    /// 3..63 characters, as `CreateTableBucket` accepts one.
    bucket: &'static str,
    /// The namespace and the table's name (`^[0-9a-z_]*$`).
    namespace: &'static str,
    table: &'static str,
    /// Whether the create describes the table's metadata, which is what gives
    /// the table a metadata file — and so an update replication can copy.
    metadata: bool,
    scope: Scope,
    /// How many destination table buckets the row creates before configuring
    /// replication to them.
    held: usize,
    /// How many further destination table buckets the configuration names
    /// without any of them ever being created.
    absent: usize,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "one destination, the table's own configuration, no metadata",
            region: "us-east-1",
            bucket: "repl-status-east",
            namespace: "ns_east",
            table: "orders_east",
            metadata: false,
            scope: Scope::Table,
            held: 1,
            absent: 0,
        },
        Case {
            label: "three destinations, the table's own configuration, with metadata",
            region: "us-west-2",
            bucket: "repl-status-west",
            namespace: "ns_west",
            table: "orders_west",
            metadata: true,
            scope: Scope::Table,
            held: 3,
            absent: 0,
        },
        Case {
            label: "the modelled maximum of five destinations, the bucket's configuration",
            region: "eu-west-1",
            bucket: "repl-status-eu",
            namespace: "ns_eu",
            table: "clicks_eu",
            metadata: true,
            scope: Scope::Bucket,
            held: 5,
            absent: 0,
        },
        Case {
            label: "a destination table bucket the store does not hold",
            region: "ap-south-1",
            bucket: "repl-status-ap",
            namespace: "ns_ap",
            table: "sensor_readings",
            metadata: true,
            scope: Scope::Table,
            held: 1,
            absent: 1,
        },
        Case {
            label: "the bucket's configuration, one destination, no metadata",
            region: "us-east-2",
            bucket: "repl-status-central",
            namespace: "ns_central",
            table: "ledger",
            metadata: false,
            scope: Scope::Bucket,
            held: 1,
            absent: 0,
        },
    ]
}

/// Create a table bucket, a namespace and one table in it, and answer the
/// bucket's ARN with the table's ARN.
async fn create_table(
    emulator: &Emulator,
    region: &str,
    bucket: &str,
    namespace: &str,
    table: &str,
    metadata: bool,
) -> (String, String) {
    let created = emulator
        .create_table_bucket(region, &json!({ "name": bucket }))
        .await;
    assert_eq!(
        created.status, 200,
        "creating the bucket {bucket:?} must succeed, got {status} {body}",
        status = created.status,
        body = created.body
    );
    let bucket_arn = created.text("arn");
    let namespace_created = emulator
        .create_namespace(region, &bucket_arn, &json!({ "namespace": [namespace] }))
        .await;
    assert_eq!(
        namespace_created.status, 200,
        "creating the namespace {namespace:?} must succeed, got {status} {body}",
        status = namespace_created.status,
        body = namespace_created.body
    );
    let table_created = emulator
        .create_table(
            region,
            &bucket_arn,
            namespace,
            &create_table_request(table, metadata),
        )
        .await;
    assert_eq!(
        table_created.status, 200,
        "creating the table {table:?} must succeed, got {status} {body}",
        status = table_created.status,
        body = table_created.body
    );
    (bucket_arn, table_created.text("tableARN"))
}

/// Create `count` destination table buckets named after `prefix` in `region`,
/// and answer their ARNs.
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

/// Set the configuration that replicates the table at `scope`, and answer the
/// version token the accepted put minted.
async fn configure(
    emulator: &Emulator,
    region: &str,
    scope: Scope,
    bucket_arn: &str,
    table_arn: &str,
    destinations: &[String],
    case: &str,
) -> String {
    let put = match scope {
        Scope::Table => {
            emulator
                .put_table_replication(
                    region,
                    &[("tableArn", table_arn.to_string())],
                    &configuration(destinations),
                )
                .await
        }
        Scope::Bucket => {
            emulator
                .put_table_bucket_replication(
                    region,
                    &[("tableBucketARN", bucket_arn.to_string())],
                    &configuration(destinations),
                )
                .await
        }
    };
    assert_eq!(
        put.status, 200,
        "case {case:?}: the {scope:?}-level put must be served, got {status} {body}",
        status = put.status,
        body = put.body
    );
    put.text("versionToken")
}

/// The destination entries a status read reports, in the order it reports them.
fn reported(reply: &Reply, case: &str) -> Vec<Value> {
    reply
        .body
        .get("destinations")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the read must report its destinations, got {body}",
                body = reply.body
            )
        })
        .clone()
}

/// The destination table bucket ARN one reported entry names.
fn destination_arn(entry: &Value, case: &str) -> String {
    entry
        .get("destinationTableBucketArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: a reported destination names its table bucket ARN, got {entry}")
        })
        .to_string()
}

/// The `replicationStatus` one reported entry carries.
fn state(entry: &Value, case: &str) -> String {
    entry
        .get("replicationStatus")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: a reported destination carries its state, got {entry}")
        })
        .to_string()
}

/// Assert `reply` is the modelled 404 stated with `message`.
fn assert_not_found(reply: &Reply, message: &str, case: &str) {
    assert_eq!(
        reply.status, 404,
        "case {case:?}: expected status 404, got {status} {body}",
        status = reply.status,
        body = reply.body
    );
    assert_eq!(
        reply.error_code.as_deref(),
        Some("NotFoundException"),
        "case {case:?}: expected NotFoundException, got {actual:?}",
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
async fn every_destination_of_the_configuration_that_replicates_the_table_is_reported() {
    let emulator = Emulator::new();
    for case in cases() {
        let label = case.label;
        // The instant the table could not have been replicated before, so an
        // instant a row does see reported is one of THIS table's replication.
        let created_at = super::harness::now_seconds();
        let (bucket_arn, table_arn) = create_table(
            &emulator,
            case.region,
            case.bucket,
            case.namespace,
            case.table,
            case.metadata,
        )
        .await;

        // Nothing replicates the table until a configuration says so, so the
        // read is refused rather than answered with an empty list.
        assert_not_found(
            &emulator
                .get_table_replication_status(case.region, &table_arn)
                .await,
            NO_REPLICATION_CONFIGURATION,
            &format!("{label}, before any configuration"),
        );

        let held = destination_buckets(
            &emulator,
            case.region,
            &format!("{bucket}-dst", bucket = case.bucket),
            case.held,
            label,
        )
        .await;
        // The destinations the configuration names without the store holding
        // them: ARNs of the form the modelled `TableBucketARN` pattern accepts
        // for buckets nothing ever created.
        let absent: Vec<String> = (0..case.absent)
            .map(|index| {
                format!(
                    "arn:aws:s3tables:{region}:{ACCOUNT}:bucket/{bucket}-never-{index}",
                    region = case.region,
                    bucket = case.bucket
                )
            })
            .collect();
        let mut named = held.clone();
        named.extend(absent.iter().cloned());
        configure(
            &emulator,
            case.region,
            case.scope,
            &bucket_arn,
            &table_arn,
            &named,
            label,
        )
        .await;

        let read = emulator
            .get_table_replication_status(case.region, &table_arn)
            .await;
        assert_eq!(
            read.status, 200,
            "case {label:?}: the read must be served, got {status} {body}",
            status = read.status,
            body = read.body
        );
        assert_eq!(
            read.text("sourceTableArn"),
            table_arn,
            "case {label:?}: expected the source table's own ARN, got {actual:?}",
            actual = read.text("sourceTableArn")
        );
        let entries = reported(&read, label);
        assert_eq!(
            entries
                .iter()
                .map(|entry| destination_arn(entry, label))
                .collect::<Vec<String>>(),
            named,
            "case {label:?}: expected one entry per configured destination, in the order the \
             rule holds them, got {body}",
            body = read.body
        );

        for entry in &entries {
            let destination = destination_arn(entry, label);
            let actual = state(entry, label);
            if absent.contains(&destination) {
                // A destination table bucket the store does not hold cannot be
                // replicated to, so its state never moves off failed and it
                // states why.
                assert_eq!(
                    actual, FAILED,
                    "case {label:?}: expected {destination} to report {FAILED}, got {entry}"
                );
                assert_eq!(
                    entry.get("failureMessage").and_then(Value::as_str),
                    Some(NO_BUCKET),
                    "case {label:?}: expected the fixed sentence {NO_BUCKET:?} for a destination \
                     nothing holds, got {entry}"
                );
                assert!(
                    entry.get("lastSuccessfulReplicatedUpdate").is_none(),
                    "case {label:?}: nothing was replicated to {destination}, so it must report \
                     no update, got {entry}"
                );
                assert!(
                    entry.get("destinationTableArn").is_none(),
                    "case {label:?}: {destination} holds no replica, so no table ARN may be \
                     reported for it, got {entry}"
                );
                continue;
            }
            // A destination the store holds took the replica the put
            // provisioned there, so the entry names it and nothing refuses it:
            // the replication of the table's current state to it is pending
            // until it is due, and a host slow enough for it to have become due
            // in between reports it completed — never failed.
            assert!(
                entry.get("failureMessage").is_none(),
                "case {label:?}: nothing refuses {destination}, so it must carry no \
                 failureMessage, got {entry}"
            );
            let replica = entry
                .get("destinationTableArn")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!(
                        "case {label:?}: {destination} holds the replica, so the entry must name \
                         its table ARN, got {entry}"
                    )
                });
            assert!(
                replica.starts_with(&format!("{destination}/table/")),
                "case {label:?}: expected the replica to stand under {destination}, got \
                 {replica:?}"
            );
            // The ARN is one a read of that table answers for, rather than a
            // claim about a table nothing holds.
            let held = emulator.get_table_by_arn(case.region, replica).await;
            assert_eq!(
                held.status, 200,
                "case {label:?}: the replica {replica} must resolve, got {status} {body}",
                status = held.status,
                body = held.body
            );
            match actual.as_str() {
                PENDING => assert!(
                    entry.get("lastSuccessfulReplicatedUpdate").is_none(),
                    "case {label:?}: no replication to {destination} has completed, so it must \
                     report no update, got {entry}"
                ),
                COMPLETED => match entry.get("lastSuccessfulReplicatedUpdate") {
                    // A table with no metadata file has no document to
                    // replicate, so a completed destination of it reports no
                    // update either.
                    None => assert!(
                        !case.metadata,
                        "case {label:?}: the table carries metadata, so a completed destination \
                         must report the update it replicated, got {entry}"
                    ),
                    Some(update) => {
                        assert!(
                            case.metadata,
                            "case {label:?}: the table carries no metadata file, so no update \
                             may be reported, got {entry}"
                        );
                        let replicated = update
                            .get("metadataLocation")
                            .and_then(Value::as_str)
                            .unwrap_or_else(|| {
                                panic!(
                                    "case {label:?}: a reported update names the metadata it \
                                     replicated, got {update}"
                                )
                            });
                        assert!(
                            replicated.starts_with("s3://"),
                            "case {label:?}: expected the replicated metadata's S3 location, got \
                             {replicated:?}"
                        );
                        let at = timestamp_seconds(update, "timestamp", label);
                        assert!(
                            at >= created_at && at <= super::harness::now_seconds(),
                            "case {label:?}: expected the completion instant to fall between the \
                             table's creation ({created_at}) and now, got {at}"
                        );
                    }
                },
                other => panic!(
                    "case {label:?}: expected {destination} to report {PENDING} (or the \
                     replication it completed), got {other:?} in {entry}"
                ),
            }
        }
    }
}

#[tokio::test]
async fn replication_provisions_the_replica_and_the_read_reports_the_completion() {
    let region = "us-east-1";
    let namespace = "ns_replica";
    let table = "replicated";
    let case = "the replica";
    let emulator = Emulator::new();
    let before = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs_f64();
    let (source_bucket, table_arn) =
        create_table(&emulator, region, "repl-status-source", namespace, table, true).await;
    // An EMPTY destination table bucket: the put is what gives it the namespace
    // and the replica, which is the state a replication leaves behind there.
    let destinations =
        destination_buckets(&emulator, region, "repl-status-replica", 1, case).await;
    let destination_bucket = destinations[0].clone();
    configure(
        &emulator,
        region,
        Scope::Table,
        &source_bucket,
        &table_arn,
        &destinations,
        case,
    )
    .await;

    // The namespace the source table lives in now stands at the destination
    // too, because that is where the replica has to go.
    let namespaces = emulator
        .list_namespaces(region, &destination_bucket, &[])
        .await;
    assert_eq!(
        namespaces.status, 200,
        "case {case:?}: the destination's namespace listing must be served, got {status} {body}",
        status = namespaces.status,
        body = namespaces.body
    );
    let listed: Vec<String> = namespaces
        .list("namespaces")
        .iter()
        .filter_map(|row| {
            row.get("namespace")
                .and_then(Value::as_array)
                .and_then(|names| names.first())
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    assert!(
        listed.contains(&namespace.to_string()),
        "case {case:?}: expected the replicated namespace {namespace:?} at the destination, got \
         {listed:?}"
    );

    // Poll for the CONDITION — the replication having completed — rather than
    // for a span, and give it a deadline far longer than it needs.
    let deadline = SystemTime::now() + COMPLETION_DEADLINE;
    let completed = loop {
        let reply = emulator
            .get_table_replication_status(region, &table_arn)
            .await;
        let entries = reported(&reply, case);
        assert_eq!(
            entries.len(),
            1,
            "case {case:?}: one destination was configured, got {body}",
            body = reply.body
        );
        let entry = entries[0].clone();
        if state(&entry, case) == COMPLETED {
            break entry;
        }
        assert_eq!(
            state(&entry, case),
            PENDING,
            "case {case:?}: a destination the store holds is pending until its replication \
             completes, got {entry}"
        );
        assert!(
            SystemTime::now() < deadline,
            "case {case:?}: the replication never completed: {body}",
            body = reply.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };

    // A completed destination names the replica it holds, under the source
    // table's own name — and that ARN is one a read of the table answers for
    // rather than a claim about a table nothing holds.
    let replica_arn = completed
        .get("destinationTableArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: a completed destination names its replica, got {completed}")
        })
        .to_string();
    assert!(
        replica_arn.starts_with(&format!("{destination_bucket}/table/")),
        "case {case:?}: expected the replica to stand under {destination_bucket}, got \
         {replica_arn:?}"
    );
    let replica = emulator.get_table_by_arn(region, &replica_arn).await;
    assert_eq!(
        replica.status, 200,
        "case {case:?}: the reported table ARN must resolve, got {status} {body}",
        status = replica.status,
        body = replica.body
    );
    assert_eq!(
        replica.text("name"),
        table,
        "case {case:?}: expected the replica to carry the source table's name, got {body}",
        body = replica.body
    );

    // What a completed replication reports is the source table's own metadata
    // document — the one `GetTableMetadataLocation` answers with — and the
    // instant the replication of it completed.
    let location = emulator
        .get_table_metadata_location(region, &source_bucket, namespace, table)
        .await;
    assert_eq!(
        location.status, 200,
        "case {case:?}: the metadata location read must be served, got {status} {body}",
        status = location.status,
        body = location.body
    );
    let update = completed
        .get("lastSuccessfulReplicatedUpdate")
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: a completed destination reports the update it replicated, got \
                 {completed}"
            )
        });
    assert_eq!(
        update.get("metadataLocation").and_then(Value::as_str),
        location.optional_text("metadataLocation").as_deref(),
        "case {case:?}: expected the table's own metadata location, got {update}"
    );
    let at = timestamp_seconds(update, "timestamp", case);
    assert!(
        at >= before && at <= super::harness::now_seconds(),
        "case {case:?}: expected the completion instant to fall between the table's creation \
         ({before}) and now, got {at}"
    );
    assert!(
        completed.get("failureMessage").is_none(),
        "case {case:?}: nothing refused this destination, so it must carry no failureMessage, \
         got {completed}"
    );

    // The read is a read: asking again answers the same completed replication,
    // and does not move it on.
    let again = emulator
        .get_table_replication_status(region, &table_arn)
        .await;
    assert_eq!(
        reported(&again, case)[0],
        completed,
        "case {case:?}: two reads of a table nobody reconfigured must answer the same, got \
         {completed:?} then {second:?}",
        second = reported(&again, case)[0]
    );
}

#[tokio::test]
async fn a_tables_own_configuration_replicates_it_and_its_buckets_covers_it_otherwise() {
    let region = "us-east-1";
    let emulator = Emulator::new();
    let (bucket_arn, table_arn) = create_table(
        &emulator,
        region,
        "repl-status-precedence",
        "ns_precedence",
        "precedence",
        true,
    )
    .await;
    let for_bucket = destination_buckets(
        &emulator,
        region,
        "repl-status-precedence-bucket",
        1,
        "precedence",
    )
    .await;
    let for_table = destination_buckets(
        &emulator,
        region,
        "repl-status-precedence-table",
        2,
        "precedence",
    )
    .await;

    // The bucket's configuration covers the tables of that bucket, so the
    // table's status is reported against the bucket's destinations.
    configure(
        &emulator,
        region,
        Scope::Bucket,
        &bucket_arn,
        &table_arn,
        &for_bucket,
        "the bucket's configuration",
    )
    .await;
    let under_bucket = emulator
        .get_table_replication_status(region, &table_arn)
        .await;
    assert_eq!(
        reported(&under_bucket, "the bucket's configuration")
            .iter()
            .map(|entry| destination_arn(entry, "the bucket's configuration"))
            .collect::<Vec<String>>(),
        for_bucket,
        "case \"the bucket's configuration\": expected the bucket's destinations, got {body}",
        body = under_bucket.body
    );

    // A configuration on the TABLE is what replicates it once it has one, so
    // its own destinations are what the status reports — never a mix of the
    // two.
    configure(
        &emulator,
        region,
        Scope::Table,
        &bucket_arn,
        &table_arn,
        &for_table,
        "the table's own configuration",
    )
    .await;
    let under_table = emulator
        .get_table_replication_status(region, &table_arn)
        .await;
    assert_eq!(
        reported(&under_table, "the table's own configuration")
            .iter()
            .map(|entry| destination_arn(entry, "the table's own configuration"))
            .collect::<Vec<String>>(),
        for_table,
        "case \"the table's own configuration\": expected the table's own destinations, got \
         {body}",
        body = under_table.body
    );

    // And a SIBLING table of the same bucket, which carries no configuration of
    // its own, is still covered by the bucket's: the two records are
    // independent, so each table's status is reported against the configuration
    // that replicates THAT table.
    let sibling_namespace = "ns_sibling";
    let created = emulator
        .create_namespace(
            region,
            &bucket_arn,
            &json!({ "namespace": [sibling_namespace] }),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case \"the sibling table\": creating its namespace must succeed, got {status} {body}",
        status = created.status,
        body = created.body
    );
    let sibling = emulator
        .create_table(
            region,
            &bucket_arn,
            sibling_namespace,
            &create_table_request("sibling", true),
        )
        .await;
    assert_eq!(
        sibling.status, 200,
        "case \"the sibling table\": creating it must succeed, got {status} {body}",
        status = sibling.status,
        body = sibling.body
    );
    let sibling_arn = sibling.text("tableARN");
    let under_sibling = emulator
        .get_table_replication_status(region, &sibling_arn)
        .await;
    assert_eq!(
        reported(&under_sibling, "the sibling table")
            .iter()
            .map(|entry| destination_arn(entry, "the sibling table"))
            .collect::<Vec<String>>(),
        for_bucket,
        "case \"the sibling table\": expected the bucket's destinations, got {body}",
        body = under_sibling.body
    );
    // The configured table still reports its own, so neither read changed the
    // other's.
    let again = emulator
        .get_table_replication_status(region, &table_arn)
        .await;
    assert_eq!(
        reported(&again, "the configured table, read again")
            .iter()
            .map(|entry| destination_arn(entry, "the configured table, read again"))
            .collect::<Vec<String>>(),
        for_table,
        "case \"the configured table, read again\": expected the table's own destinations, got \
         {body}",
        body = again.body
    );
}

#[tokio::test]
async fn a_read_of_a_table_nothing_replicates_or_of_no_table_at_all_is_refused() {
    let region = "us-east-1";
    let emulator = Emulator::new();
    let (bucket_arn, table_arn) = create_table(
        &emulator,
        region,
        "repl-status-refused",
        "ns_refused",
        "refused",
        false,
    )
    .await;

    // A table id of the form the modelled `TableARN` pattern accepts
    // (`[a-zA-Z0-9-_]{1,255}`) that nothing was ever minted for, in a bucket
    // the store holds, in one it never held, and in one it held and no longer
    // does.
    let absent_in_bucket = format!("{bucket_arn}/table/11111111-2222-4333-8444-555555555555");
    let absent_bucket = format!(
        "arn:aws:s3tables:{region}:{ACCOUNT}:bucket/repl-status-never/table/\
         66666666-7777-4888-8999-aaaaaaaaaaaa"
    );
    let emptied = emulator
        .create_table_bucket(region, &json!({ "name": "repl-status-deleted" }))
        .await;
    assert_eq!(
        emptied.status, 200,
        "creating the bucket to delete must succeed, got {status} {body}",
        status = emptied.status,
        body = emptied.body
    );
    let deleted_bucket_arn = emptied.text("arn");
    let deleted = emulator
        .delete_table_bucket(region, &deleted_bucket_arn)
        .await;
    assert_eq!(
        deleted.status, 204,
        "deleting the bucket must succeed, got {status} {body}",
        status = deleted.status,
        body = deleted.body
    );
    let in_deleted_bucket =
        format!("{deleted_bucket_arn}/table/77777777-8888-4999-8aaa-bbbbbbbbbbbb");

    let rows: Vec<(&str, String, &str)> = vec![
        (
            "a table nothing replicates",
            table_arn.clone(),
            NO_REPLICATION_CONFIGURATION,
        ),
        (
            "a table id the bucket holds no table under",
            absent_in_bucket,
            NO_TABLE,
        ),
        (
            "a table in a bucket that was never created",
            absent_bucket,
            NO_BUCKET,
        ),
        (
            "a table in a bucket that has since been deleted",
            in_deleted_bucket,
            NO_BUCKET,
        ),
    ];

    for (case, addressed, message) in rows {
        let read = emulator
            .get_table_replication_status(region, &addressed)
            .await;
        assert_not_found(&read, message, case);
        assert!(
            read.body.get("destinations").is_none(),
            "case {case:?}: a refused read reports no destinations, got {body}",
            body = read.body
        );
    }
}
