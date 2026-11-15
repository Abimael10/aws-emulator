// SPDX-License-Identifier: CC0-1.0

//! What a metadata configuration's tables really are: records of the Amazon S3 Tables store.
//!
//! The rule under test: **a table this service reports a metadata configuration as having is
//! created in the store of the service that OWNS tables — the managed table bucket under the
//! modelled `aws` bucket type, the general purpose bucket's own namespace inside it, and one
//! table per table the configuration provisions, each carrying the `tableARN` this service
//! publishes for it and the principal of the service that manages it. A table a caller then
//! deletes through the S3 Tables API that owns it is gone for this service too, so the
//! published `*AlreadyExists` provisioning failure stops applying to its name; and a table
//! whose creation had not yet fallen due goes with the configuration (or with the general
//! purpose bucket) that asked for it, because nothing a caller could have seen stood under
//! it.**
//!
//! The coordinates asserted here are the ones the configuration's own reader publishes
//! (`DestinationResult$TableBucketArn`, `DestinationResult$TableNamespace`,
//! `JournalTableConfigurationResult$TableName`/`TableArn`), read out of the response rather
//! than recomputed, so what is checked is that a client following them finds the table. The
//! S3 TABLES side of the same rule — that such a table is resolved and deleted over that
//! service's own wire by its `(tableBucketARN, namespace, name)` triple — is pinned in that
//! crate's own suite (`tests/all/service_managed_table_deletes.rs`).
//!
//! The rows differ along what the rule generalises over: which tables the configuration asks
//! for, the Region the general purpose bucket is created in, and the form of that bucket's
//! name — which is what the namespace holding its tables is derived from.

use std::sync::Arc;
use std::time::{Duration, Instant};

use service_com_amazonaws_s3_amazons3::state::{
    managing_service_principal, METADATA_TABLE_STATUS_ACTIVE,
};
use service_com_amazonaws_s3tables_s3tablebuckets::state as tables_store;
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{create_bucket, router_with_engine, send_in_region};
use super::bucket_metadata_configuration::{
    create_metadata_configuration, get_metadata_configuration, inside,
};
use super::bucket_metadata_configuration_deletion::{delete_metadata_configuration, payload};
use super::object_and_tagging_roundtrip::text;

/// How long a row waits for the configuration's tables to come into service before it gives
/// up: generous, since what it waits for is a span the store records and a loaded machine may
/// take much longer to get there.
const SETTLE_BUDGET: Duration = Duration::from_secs(30);

/// How often it looks again while waiting.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// One row: the configuration to provision, and where it is provisioned.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// What this row is, for the assertion messages.
    case: &'static str,
    /// The `LocationConstraint` the create carries, which is what places the bucket in a
    /// Region other than the one the unspecified constraint stands for.
    constraint: Option<&'static str>,
    /// That bucket's name — what the namespace holding its tables is derived from.
    bucket: &'static str,
    /// Whether the configuration asks for an inventory table beside the journal one.
    inventory: bool,
}

/// The rows, spanning the forms the rule generalises over.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the journal table alone",
            constraint: None,
            bucket: "backed-tables-journal",
            inventory: false,
        },
        Row {
            case: "a journal and an inventory table",
            constraint: None,
            bucket: "backed-tables-inventory",
            inventory: true,
        },
        Row {
            case: "a bucket of another Region",
            constraint: Some("us-west-2"),
            bucket: "backed-tables-region",
            inventory: true,
        },
        Row {
            case: "a bucket name carrying dots and hyphens",
            constraint: None,
            bucket: "backed.tables-2026",
            inventory: false,
        },
    ]
}

/// The containers a configuration asking for `inventory` or not publishes a table in.
fn containers(inventory: bool) -> Vec<&'static str> {
    match inventory {
        false => vec!["JournalTableConfigurationResult"],
        true => vec![
            "JournalTableConfigurationResult",
            "InventoryTableConfigurationResult",
        ],
    }
}

/// Where a configuration's tables are, as its own reader publishes it: the managed table
/// bucket's ARN, the namespace inside it, and one `(container, table name, table ARN)` per
/// table.
#[derive(Debug)]
struct Published {
    table_bucket_arn: String,
    namespace: String,
    tables: Vec<(String, String, String)>,
}

/// Read that out of a `GetBucketMetadataConfiguration` body.
fn published(body: &str, wanted: &[&str], case: &str) -> Published {
    let destination = inside(body, "DestinationResult")
        .unwrap_or_else(|| panic!("case {case:?}: no DestinationResult in {body:?}"));
    let table_bucket_arn = inside(destination, "TableBucketArn")
        .unwrap_or_else(|| panic!("case {case:?}: no TableBucketArn in {body:?}"))
        .to_string();
    let namespace = inside(destination, "TableNamespace")
        .unwrap_or_else(|| panic!("case {case:?}: no TableNamespace in {body:?}"))
        .to_string();
    let mut tables = Vec::new();
    for container in wanted {
        let fragment = inside(body, container)
            .unwrap_or_else(|| panic!("case {case:?}: no {container} in {body:?}"));
        let name = inside(fragment, "TableName")
            .unwrap_or_else(|| panic!("case {case:?}: no TableName in {container}: {body:?}"))
            .to_string();
        let arn = inside(fragment, "TableArn")
            .unwrap_or_else(|| panic!("case {case:?}: no TableArn in {container}: {body:?}"))
            .to_string();
        tables.push(((*container).to_string(), name, arn));
    }
    Published {
        table_bucket_arn,
        namespace,
        tables,
    }
}

/// The `TableStatus` the container `container` of `body` reports, or `None` when the body
/// carries no such container.
fn status_of(body: &str, container: &str) -> Option<String> {
    inside(body, container).and_then(|fragment| inside(fragment, "TableStatus").map(str::to_string))
}

/// Look again until every one of `wanted` reports [`METADATA_TABLE_STATUS_ACTIVE`], or the
/// budget runs out — the CONDITION a row waits for, never a fixed pause.
async fn wait_until_in_service(
    router: &MultiServiceRouter,
    bucket: &str,
    wanted: &[&str],
    case: &str,
) -> String {
    let deadline = Instant::now() + SETTLE_BUDGET;
    loop {
        let body = text(&get_metadata_configuration(router, bucket, &[]).await);
        let in_service = wanted.iter().all(|container| {
            status_of(&body, container).as_deref() == Some(METADATA_TABLE_STATUS_ACTIVE)
        });
        if in_service {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the configuration's tables never came into service: {body:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// `DELETE /<bucket>` — `DeleteBucket`, in the Region the bucket lives in.
async fn delete_bucket(router: &MultiServiceRouter, region: &str, bucket: &str) -> u16 {
    send_in_region(router, region, "DELETE", &format!("/{bucket}"), &[], "")
        .await
        .status
}

/// Whether the S3 Tables store on `engine` holds a table under `name` in `namespace` of the
/// table bucket `arn`.
fn tables_holds(engine: &Arc<engine::InMemoryEngine>, arn: &str, namespace: &str, name: &str) -> bool {
    tables_store::service_state(&**engine)
        .expect("the S3 Tables store is reachable on the shared engine")
        .lock()
        .holds_table(arn, namespace, name)
}

#[tokio::test]
async fn every_table_a_configuration_provisions_is_a_table_of_the_s3_tables_store() {
    let (router, engine) = router_with_engine();

    for row in rows() {
        let Row {
            case,
            constraint,
            bucket,
            inventory,
        } = row;
        let wanted = containers(inventory);

        let created = create_bucket(&router, bucket, constraint, None).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the bucket must be created, got {} with body {:?}",
            created.status,
            text(&created)
        );
        let configured =
            create_metadata_configuration(&router, bucket, &[], &payload(inventory, false)).await;
        assert_eq!(
            configured.status, 200,
            "case {case:?}: the configuration must be created, got {} with body {:?}",
            configured.status,
            text(&configured)
        );
        // The tables come into service a span after the create; what this row asserts about
        // the store holds from the create onwards, and the status is read once they have.
        let body = wait_until_in_service(&router, bucket, &wanted, case).await;
        let published = published(&body, &wanted, case);

        // The managed table bucket is a table bucket of the OTHER service, and one Amazon Web
        // Services created on the account's behalf.
        let store = tables_store::service_state(&*engine)
            .expect("the S3 Tables store is reachable on the shared engine");
        {
            let guard = store.lock();
            let held = guard
                .table_buckets
                .get(&published.table_bucket_arn)
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: the S3 Tables store holds no table bucket {:?}",
                        published.table_bucket_arn
                    )
                });
            assert_eq!(
                held.bucket_type,
                tables_store::TableBucketType::Aws,
                "case {case:?}: expected the managed table bucket's type to be the modelled \
                 `aws`, got {:?}",
                held.bucket_type
            );
            let namespace = held.namespaces.get(&published.namespace).unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the managed table bucket holds no namespace {:?}, only {:?}",
                    published.namespace,
                    held.namespaces.keys().collect::<Vec<_>>()
                )
            });
            // Exactly the tables the configuration reports, and no others.
            let mut held_names: Vec<&String> = namespace.tables.keys().collect();
            held_names.sort();
            let mut expected: Vec<&String> =
                published.tables.iter().map(|(_, name, _)| name).collect();
            expected.sort();
            assert_eq!(
                held_names, expected,
                "case {case:?}: expected the namespace to hold {expected:?}, got {held_names:?}"
            );

            for (container, name, arn) in &published.tables {
                let table = namespace.tables.get(name).unwrap_or_else(|| {
                    panic!("case {case:?}: no {container} table {name:?} in the store")
                });
                assert_eq!(
                    &table.table_arn, arn,
                    "case {case:?}: expected the {container} table's stored ARN to be the one \
                     the configuration publishes ({arn:?}), got {:?}",
                    table.table_arn
                );
                assert_eq!(
                    table.table_type,
                    tables_store::TableType::Aws,
                    "case {case:?}: expected the {container} table's type to be the modelled \
                     `aws`, got {:?}",
                    table.table_type
                );
                assert_eq!(
                    table.managed_by_service.as_deref(),
                    Some(managing_service_principal().as_str()),
                    "case {case:?}: expected the {container} table to be managed by this \
                     service, got {:?}",
                    table.managed_by_service
                );
                // The ARN index of that store resolves the same table, so a client holding the
                // published ARN reaches it.
                assert!(
                    guard.table_by_arn(arn).is_some(),
                    "case {case:?}: the {container} table's ARN {arn:?} must resolve in the S3 \
                     Tables store"
                );
            }
            // And every table is reported in service, which is what makes the name it holds
            // one only a delete through that service can free.
            for container in &wanted {
                assert_eq!(
                    status_of(&body, container).as_deref(),
                    Some(METADATA_TABLE_STATUS_ACTIVE),
                    "case {case:?}: expected {container} to report \
                     {METADATA_TABLE_STATUS_ACTIVE}, got {body:?}"
                );
            }
        }

        // A table a caller deletes through the S3 Tables API that owns it is gone for this
        // service too: the configuration is deleted and created again, and the journal table
        // provisions afresh rather than reporting its name taken.
        let journal = published
            .tables
            .first()
            .map(|(_, name, _)| name.clone())
            .expect("every configuration has a journal table");
        let dropped = delete_metadata_configuration(&router, bucket, &[]).await;
        assert_eq!(
            dropped.status, 204,
            "case {case:?}: the configuration delete must succeed, got {} with body {:?}",
            dropped.status,
            text(&dropped)
        );
        // The delete left the table that came into service where it was, which is what makes
        // the create below collide with it.
        assert!(
            tables_holds(
                &engine,
                &published.table_bucket_arn,
                &published.namespace,
                &journal
            ),
            "case {case:?}: a table in service outlives the configuration that created it"
        );
        let colliding =
            create_metadata_configuration(&router, bucket, &[], &payload(inventory, false)).await;
        assert_eq!(
            colliding.status, 200,
            "case {case:?}: the second create must be accepted, got {} with body {:?}",
            colliding.status,
            text(&colliding)
        );
        let colliding_body = text(&get_metadata_configuration(&router, bucket, &[]).await);
        assert_eq!(
            status_of(&colliding_body, "JournalTableConfigurationResult").as_deref(),
            Some("FAILED"),
            "case {case:?}: the journal table's name is still held, so it must report FAILED, \
             got {colliding_body:?}"
        );

        // Now the caller deletes the leftover table the way the published sentence asks it to,
        // through the service that owns it.
        let deleted = store.lock().delete_service_managed_table(
            &published.table_bucket_arn,
            &published.namespace,
            &journal,
            &managing_service_principal(),
        );
        assert!(
            deleted,
            "case {case:?}: the leftover journal table must be deletable through its own \
             service"
        );
        let dropped_again = delete_metadata_configuration(&router, bucket, &[]).await;
        assert_eq!(
            dropped_again.status, 204,
            "case {case:?}: the second configuration delete must succeed, got {}",
            dropped_again.status
        );
        let afresh =
            create_metadata_configuration(&router, bucket, &[], &payload(inventory, false)).await;
        assert_eq!(
            afresh.status, 200,
            "case {case:?}: the third create must be accepted, got {} with body {:?}",
            afresh.status,
            text(&afresh)
        );
        let afresh_body = text(&get_metadata_configuration(&router, bucket, &[]).await);
        assert_ne!(
            status_of(&afresh_body, "JournalTableConfigurationResult").as_deref(),
            Some("FAILED"),
            "case {case:?}: the deleted table freed its name, so the journal table must \
             provision afresh, got {afresh_body:?}"
        );
    }
}

/// The other half of the rule: a table whose creation had not yet fallen due goes with the
/// configuration — or with the general purpose bucket — that asked for it, because nothing a
/// caller could have seen stood under it.
#[tokio::test]
async fn a_table_that_never_came_into_service_goes_with_what_asked_for_it() {
    // The rows differ in what carries the still-being-created table away, and in which tables
    // the configuration had.
    let cases: [(&str, &str, bool, bool); 4] = [
        (
            "the configuration deleted while the journal table is being created",
            "unprovisioned-config-journal",
            false,
            false,
        ),
        (
            "the configuration deleted while both tables are being created",
            "unprovisioned-config-both",
            true,
            false,
        ),
        (
            "the general purpose bucket deleted while the journal table is being created",
            "unprovisioned-bucket-journal",
            false,
            true,
        ),
        (
            "the general purpose bucket deleted while both tables are being created",
            "unprovisioned-bucket-both",
            true,
            true,
        ),
    ];
    let region = "us-east-1";

    for (case, bucket, inventory, delete_the_bucket) in cases {
        let (router, engine) = router_with_engine();
        let wanted = containers(inventory);
        assert_eq!(
            create_bucket(&router, bucket, None, None).await.status,
            200,
            "case {case:?}: the bucket must be created"
        );
        assert_eq!(
            create_metadata_configuration(&router, bucket, &[], &payload(inventory, false))
                .await
                .status,
            200,
            "case {case:?}: the configuration must be created"
        );
        // Read WITHOUT waiting, so the tables are still being created — the one state in which
        // taking the configuration away carries them off.
        let body = text(&get_metadata_configuration(&router, bucket, &[]).await);
        let published = published(&body, &wanted, case);
        for (container, name, _) in &published.tables {
            assert!(
                tables_holds(
                    &engine,
                    &published.table_bucket_arn,
                    &published.namespace,
                    name
                ),
                "case {case:?}: the {container} table {name:?} must be in the S3 Tables store \
                 from the create onwards"
            );
        }

        match delete_the_bucket {
            false => {
                let dropped = delete_metadata_configuration(&router, bucket, &[]).await;
                assert_eq!(
                    dropped.status, 204,
                    "case {case:?}: the configuration delete must succeed, got {}",
                    dropped.status
                );
            }
            true => {
                let status = delete_bucket(&router, region, bucket).await;
                assert_eq!(
                    status, 204,
                    "case {case:?}: the bucket delete must succeed, got {status}"
                );
            }
        }

        for (container, name, _) in &published.tables {
            assert!(
                !tables_holds(
                    &engine,
                    &published.table_bucket_arn,
                    &published.namespace,
                    name
                ),
                "case {case:?}: the {container} table {name:?} never came into service, so it \
                 must be gone from the S3 Tables store"
            );
        }
    }
}
