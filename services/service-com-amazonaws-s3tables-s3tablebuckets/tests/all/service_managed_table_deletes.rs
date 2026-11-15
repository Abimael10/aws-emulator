// SPDX-License-Identifier: CC0-1.0

//! A table another Amazon Web Services service provisioned in a table bucket is an ordinary
//! table of this store, so this service's own reads resolve it and its deletes take it away.
//!
//! The rule under test: **a table bucket, a namespace and a table that a service created on
//! the account's behalf are resolved and deleted exactly as a caller's own are. `GetTable` and
//! `ListTables` answer about such a table by its `(tableBucketARN, namespace, name)` triple
//! and by its `tableARN`; `DeleteTable` on that triple answers the modelled 204 with an empty
//! body and the table is gone (a second delete answering the modelled `NotFoundException`,
//! 404), a stale `versionToken` is the modelled `ConflictException` (409) and deletes nothing;
//! and `DeleteNamespace` answers 204 with an empty body once that namespace holds no table,
//! while a namespace that still holds one is refused and keeps its tables.** Nothing about
//! WHICH service asked for the table changes any of that: a client that was handed a table's
//! coordinates by the service that provisioned it can read and delete the table with them.
//!
//! The rows differ along what the rule generalises over: how many tables the namespace holds,
//! the Region the table bucket is in, the spelling of the namespace (one a provisioning
//! service derived from its own resource's name, including characters a name this service
//! would CREATE cannot carry), whether the table's ARN carries the id the provisioning
//! service publishes or one this store minted, and the order the tables are deleted in.
//!
//! WHICH code the refusal of a delete of a namespace that still holds a table carries is the
//! non-empty-container rule's own ([`super::non_empty_container_delete_class`]); what this
//! suite asserts about that call is that it is refused and that the tables are still there
//! afterwards.

use std::time::SystemTime;

use serde_json::Value;
use service_com_amazonaws_s3tables_s3tablebuckets::state::service_state;

use super::harness::{Emulator, ACCOUNT};

/// The service principal a row provisions its tables under: a service that creates tables in
/// a table bucket it keeps for the account, which is what `managedByService` reports.
const MANAGING_SERVICE: &str = "s3.amazonaws.com";

/// One table a row provisions: its name, and the id its ARN is to carry — `None` for an id
/// this store mints.
#[derive(Debug, Clone, Copy)]
struct Provisioned {
    name: &'static str,
    published_id: Option<&'static str>,
}

/// One row: a service-managed table bucket and namespace, the tables in it, and how they are
/// then taken away.
#[derive(Debug)]
struct Row {
    /// What this row is, for the assertion messages.
    case: &'static str,
    /// The Region the table bucket is created in.
    region: &'static str,
    /// The table bucket's name, which is the one AWS keeps for the provisioning service.
    bucket: &'static str,
    /// The namespace the provisioning service puts its tables in.
    namespace: &'static str,
    /// The tables it provisions there.
    tables: &'static [Provisioned],
    /// Whether the row presents a stale `versionToken` on the first delete before deleting
    /// for real.
    stale_token: bool,
    /// Whether the tables are deleted in the order they were provisioned or in reverse.
    reversed: bool,
}

/// The rows, spanning the forms the rule generalises over.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one table under the id its service publishes",
            region: "us-east-1",
            bucket: "aws-s3",
            namespace: "b_managed_journal",
            tables: &[Provisioned {
                name: "journal",
                published_id: Some("b_managed_journal_journal"),
            }],
            stale_token: false,
            reversed: false,
        },
        Row {
            case: "two tables in another Region, deleted in reverse",
            region: "us-west-2",
            bucket: "aws-s3",
            namespace: "b_managed_inventory",
            tables: &[
                Provisioned {
                    name: "journal",
                    published_id: Some("b_managed_inventory_journal"),
                },
                Provisioned {
                    name: "inventory",
                    published_id: Some("b_managed_inventory_inventory"),
                },
            ],
            stale_token: false,
            reversed: true,
        },
        Row {
            case: "three tables with minted ids, after a stale version token",
            region: "eu-west-1",
            bucket: "managed-tables-minted",
            namespace: "b_managed_every_table",
            tables: &[
                Provisioned {
                    name: "journal",
                    published_id: None,
                },
                Provisioned {
                    name: "inventory",
                    published_id: None,
                },
                Provisioned {
                    name: "annotation",
                    published_id: None,
                },
            ],
            stale_token: true,
            reversed: false,
        },
        Row {
            // A namespace a provisioning service derived from a resource name of its own,
            // carrying characters a namespace this service would CREATE cannot: the delete
            // still addresses the namespace that is there rather than refusing its spelling.
            case: "a namespace spelled outside the create-time character set",
            region: "us-east-1",
            bucket: "aws-s3",
            namespace: "b_managed.tables-2026",
            tables: &[Provisioned {
                name: "journal",
                published_id: Some("b_managed_tables_2026_journal"),
            }],
            stale_token: false,
            reversed: false,
        },
    ]
}

/// The ARN of the table bucket `name` in `region` — the same total function of the two that
/// every answer of this service computes it from.
fn table_bucket_arn(region: &str, name: &str) -> String {
    format!("arn:aws:s3tables:{region}:{ACCOUNT}:bucket/{name}")
}

#[tokio::test]
async fn a_service_provisioned_table_is_resolved_and_deleted_like_any_other() {
    for row in rows() {
        let Row {
            case,
            region,
            bucket,
            namespace,
            tables,
            stale_token,
            reversed,
        } = row;
        // One emulator per row, so no row depends on what another left behind.
        let emulator = Emulator::new();
        let now = SystemTime::now();

        // The provisioning itself, through the one seam a service reaches this store by.
        let state = service_state(&*emulator.engine).expect("the S3 Tables store is mounted");
        let bucket_arn = state
            .lock()
            .ensure_service_managed_namespace(region, bucket, namespace, now)
            .unwrap_or_else(|refusal| {
                panic!("case {case:?}: the managed namespace must be created: {refusal:?}")
            });
        assert_eq!(
            bucket_arn,
            table_bucket_arn(region, bucket),
            "case {case:?}: expected the table bucket's own ARN, got {bucket_arn:?}"
        );
        // Idempotent: asking again creates nothing a second time and answers the same ARN.
        let again = state
            .lock()
            .ensure_service_managed_namespace(region, bucket, namespace, now)
            .unwrap_or_else(|refusal| {
                panic!("case {case:?}: the second ask must answer the same bucket: {refusal:?}")
            });
        assert_eq!(
            again, bucket_arn,
            "case {case:?}: expected the same table bucket ARN, got {again:?}"
        );

        let mut provisioned: Vec<(Provisioned, String)> = Vec::new();
        for table in tables {
            let arn = state
                .lock()
                .create_service_managed_table_with_id(
                    &bucket_arn,
                    namespace,
                    table.name,
                    MANAGING_SERVICE,
                    table.published_id,
                    now,
                )
                .unwrap_or_else(|refusal| {
                    panic!(
                        "case {case:?}: the table {:?} must be created: {refusal:?}",
                        table.name
                    )
                });
            if let Some(id) = table.published_id {
                assert_eq!(
                    arn,
                    format!("{bucket_arn}/table/{id}"),
                    "case {case:?}: expected the table's ARN to carry the published id {id:?}, \
                     got {arn:?}"
                );
            }
            provisioned.push((*table, arn));
        }

        // The table bucket a service's tables live in is a table bucket of this service, and
        // one Amazon Web Services created on the account's behalf.
        let read = emulator.get_table_bucket(region, &bucket_arn).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the managed table bucket must read back: {read:?}"
        );
        assert_eq!(
            read.text("type"),
            "aws",
            "case {case:?}: expected the bucket type `aws`, got {:?}",
            read.body
        );
        let namespace_read = emulator.get_namespace(region, &bucket_arn, namespace).await;
        assert_eq!(
            namespace_read.status, 200,
            "case {case:?}: the managed namespace must read back: {namespace_read:?}"
        );

        // Every table reads back by its triple and by its ARN, reporting the service that
        // manages it.
        for (table, arn) in &provisioned {
            let reply = emulator
                .get_table(region, &bucket_arn, namespace, table.name)
                .await;
            assert_eq!(
                reply.status, 200,
                "case {case:?}: the table {:?} must read back by its triple: {reply:?}",
                table.name
            );
            assert_eq!(
                reply.text("tableARN"),
                *arn,
                "case {case:?}: expected the table ARN {arn:?}, got {:?}",
                reply.body
            );
            assert_eq!(
                reply.text("type"),
                "aws",
                "case {case:?}: expected the table type `aws`, got {:?}",
                reply.body
            );
            assert_eq!(
                reply.text("managedByService"),
                MANAGING_SERVICE,
                "case {case:?}: expected the managing service {MANAGING_SERVICE:?}, got {:?}",
                reply.body
            );
            let by_arn = emulator.get_table_by_arn(region, arn).await;
            assert_eq!(
                by_arn.status, 200,
                "case {case:?}: the table {:?} must resolve through its ARN: {by_arn:?}",
                table.name
            );
        }

        // And the namespace lists exactly them.
        let listed = emulator
            .list_tables(
                region,
                &bucket_arn,
                &[("namespace", namespace.to_string())],
            )
            .await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: ListTables must answer for the namespace: {listed:?}"
        );
        let mut listed_names: Vec<String> = listed
            .list("tables")
            .iter()
            .filter_map(|table| table.get("name").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        listed_names.sort();
        let mut expected_names: Vec<String> = provisioned
            .iter()
            .map(|(table, _)| table.name.to_string())
            .collect();
        expected_names.sort();
        assert_eq!(
            listed_names, expected_names,
            "case {case:?}: expected the namespace to list {expected_names:?}, got {:?}",
            listed.body
        );

        let mut order: Vec<(Provisioned, String)> = provisioned.clone();
        if reversed {
            order.reverse();
        }
        let mut left = order.len();
        for (table, _) in &order {
            // A namespace that still holds a table is refused and keeps it — which code that
            // refusal carries is the non-empty rule's own (see this suite's header).
            let refused = emulator.delete_namespace(region, &bucket_arn, namespace).await;
            assert!(
                (400..500).contains(&refused.status),
                "case {case:?}: a namespace still holding {left} table(s) must be refused: \
                 {refused:?}"
            );

            if stale_token {
                // A token that is not the table's own is the modelled write conflict, and
                // deletes nothing.
                let conflicted = emulator
                    .delete_table(
                        region,
                        &bucket_arn,
                        namespace,
                        table.name,
                        &[("versionToken", "not-this-tables-token".to_string())],
                    )
                    .await;
                assert_eq!(
                    conflicted.status, 409,
                    "case {case:?}: a stale versionToken must be refused 409: {conflicted:?}"
                );
                assert_eq!(
                    emulator
                        .get_table(region, &bucket_arn, namespace, table.name)
                        .await
                        .status,
                    200,
                    "case {case:?}: the refused delete must leave the table {:?} where it is",
                    table.name
                );
            }

            // The table's own token, read back from the record, is accepted.
            let token = emulator
                .get_table(region, &bucket_arn, namespace, table.name)
                .await
                .text("versionToken");
            let deleted = emulator
                .delete_table(
                    region,
                    &bucket_arn,
                    namespace,
                    table.name,
                    &[("versionToken", token)],
                )
                .await;
            assert_eq!(
                deleted.status, 204,
                "case {case:?}: deleting the table {:?} must answer 204: {deleted:?}",
                table.name
            );
            assert!(
                deleted.body.is_null(),
                "case {case:?}: a delete answers an empty body, got {:?}",
                deleted.body
            );

            let gone = emulator
                .get_table(region, &bucket_arn, namespace, table.name)
                .await;
            assert_eq!(
                gone.status, 404,
                "case {case:?}: the deleted table {:?} must be gone: {gone:?}",
                table.name
            );
            let again = emulator
                .delete_table(region, &bucket_arn, namespace, table.name, &[])
                .await;
            assert_eq!(
                again.status, 404,
                "case {case:?}: a second delete of {:?} must answer the modelled not-found: \
                 {again:?}",
                table.name
            );
            assert_eq!(
                again.error_code.as_deref(),
                Some("NotFoundException"),
                "case {case:?}: expected NotFoundException, got {again:?}"
            );
            left -= 1;
        }

        // The emptied namespace deletes too, and is gone afterwards.
        let emptied = emulator.delete_namespace(region, &bucket_arn, namespace).await;
        assert_eq!(
            emptied.status, 204,
            "case {case:?}: deleting the emptied namespace must answer 204: {emptied:?}"
        );
        assert!(
            emptied.body.is_null(),
            "case {case:?}: the namespace delete answers an empty body, got {:?}",
            emptied.body
        );
        let absent = emulator.get_namespace(region, &bucket_arn, namespace).await;
        assert_eq!(
            absent.status, 404,
            "case {case:?}: the deleted namespace must be gone: {absent:?}"
        );
    }
}
