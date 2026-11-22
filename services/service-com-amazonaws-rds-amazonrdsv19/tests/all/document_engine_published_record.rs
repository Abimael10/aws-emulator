// SPDX-License-Identifier: CC0-1.0

//! What an ORDERABLE OPTION of an engine whose CLUSTER owns the storage publishes about that
//! storage, and which classes such an engine is offered in.
//!
//! The rule under test: an option of an engine of another control plane carries the storage
//! facts THAT control plane publishes. Amazon DocumentDB's cluster owns its members' storage
//! — its `CreateDBInstance` models no allocation at all — so an option of it publishes the
//! storage TYPE an order picks and no per-instance storage bounds and no storage autoscaling,
//! where an option of an engine whose DB instance keeps its own storage publishes all three.
//! The classes it is offered in are that control plane's own published table
//! (`service_common::docdb_engine_catalog::INSTANCE_FAMILIES`), which is also the list the
//! DocumentDB view of this one front door answers from.
//!
//! The rows span what the rule generalises over: a memory-optimised class and a burstable one
//! of the document engine, on each storage type it is ordered with, beside an option of one of
//! this door's own engines as the control case; and the class table is read on a class the
//! issue that prompted this suite did not cite (`db.r7g.16xlarge`) as well as on the one it
//! did (`db.r5.24xlarge`), plus a class of a generation the engine is not offered in.
//!
//! Every call goes over the wire through the harness in `db_instances.rs`, so each assertion
//! reads the XML this emulator itself answered with.

use crate::db_instances::{call, emulator, expect_ok};

/// The engine of the control plane whose record this suite is about, as its own `Engine`
/// member publishes it ("Valid values: docdb").
const DOCUMENT_ENGINE: &str = "docdb";

/// An engine of THIS door's own control plane, read as the control case: its DB instance
/// keeps its own storage.
const OWN_ENGINE: &str = "mysql";

/// The members an orderable option of an engine whose CLUSTER owns the storage does not
/// carry: there is no per-instance allocation to bound or to raise.
const CLUSTER_MANAGED_STORAGE_MEMBERS: &[&str] = &[
    "SupportsStorageAutoscaling",
    "MinStorageSize",
    "MaxStorageSize",
];

/// The storage types an option of the document engine is offered with, in the order its own
/// `CreateDBCluster$StorageType` publishes them ("standard | iopt1").
const DOCUMENT_STORAGE_TYPES: [&str; 2] = ["standard", "iopt1"];

/// One class read against the document engine's published table, and whether that table names
/// it.
///
/// Source: the "Amazon DocumentDB instance classes" section of the Amazon DocumentDB
/// Developer Guide, held in `service_common::docdb_engine_catalog::INSTANCE_FAMILIES` — the
/// two burstable `medium` classes, the memory-optimised `r5` line up to `24xlarge` and the
/// Graviton `r6g` and `r7g` lines up to `16xlarge`. `db.r6i.large` and `db.serverless` are
/// classes of this door's OWN cluster list, which that engine is not offered in.
const CLASS_ROWS: &[(&str, bool)] = &[
    ("db.r5.24xlarge", true),
    ("db.r7g.16xlarge", true),
    ("db.t4g.medium", true),
    ("db.r6i.large", false),
    ("db.serverless", false),
    ("db.m5.large", false),
];

/// The `<tag>…</tag>` subtrees of `xml`, in document order. The reports below nest no tag
/// inside another of the same name, so the first closing tag ends the subtree.
fn subtrees(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut found = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start..];
        let Some(end) = after.find(&close) else { break };
        found.push(after[..end + close.len()].to_string());
        rest = &after[end + close.len()..];
    }
    found
}

/// Every text carried by `<tag>` elements of `xml`, in document order.
fn texts(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut found = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.find(&close) else { break };
        found.push(after[..end].to_string());
        rest = &after[end + close.len()..];
    }
    found
}

/// Whether `xml` carries a `<tag>` element at all, open-and-closed or self-closing.
fn carries(xml: &str, tag: &str) -> bool {
    xml.contains(&format!("<{tag}>")) || xml.contains(&format!("<{tag}/>"))
}

#[tokio::test]
async fn an_orderable_option_of_a_cluster_managed_engine_reports_no_instance_storage() {
    let router = emulator();

    // Both a memory-optimised and a burstable class of the document engine: the storage of
    // either is its cluster's, whatever the class.
    for class in ["db.r5.24xlarge", "db.t3.medium"] {
        let body = expect_ok(
            class,
            call(
                &router,
                "DescribeOrderableDBInstanceOptions",
                &[
                    ("Engine", DOCUMENT_ENGINE),
                    ("DBInstanceClass", class),
                    ("MaxRecords", "20"),
                ],
            )
            .await,
        );
        let options = subtrees(&body, "OrderableDBInstanceOption");
        assert!(
            !options.is_empty(),
            "case {class:?}: expected the class to be orderable for this engine, got {body}"
        );
        for option in &options {
            for member in CLUSTER_MANAGED_STORAGE_MEMBERS {
                assert!(
                    !carries(option, member),
                    "case {class:?}: expected {member} to be absent from an option whose \
                     cluster owns the storage, got {option}"
                );
            }
            // What the option DOES publish about its storage is the type the order picks.
            assert!(
                carries(option, "StorageType"),
                "case {class:?}: expected the option to publish the storage type it is \
                 ordered with, got {option}"
            );
        }
        // One option per type the engine is ordered with, in the order its own value list
        // publishes them, for each release the report describes.
        let reported: Vec<String> = texts(&body, "StorageType");
        let published: Vec<String> = DOCUMENT_STORAGE_TYPES
            .iter()
            .map(|storage_type| (*storage_type).to_string())
            .collect();
        assert!(
            !reported.is_empty() && reported.len().is_multiple_of(published.len()),
            "case {class:?}: expected one option per published storage type, got {reported:?}"
        );
        for chunk in reported.chunks(published.len()) {
            assert_eq!(
                chunk,
                published.as_slice(),
                "case {class:?}: expected the storage types in their published order, got \
                 {reported:?}"
            );
        }
    }

    // The control case: an option of an engine whose DB instance keeps its own storage
    // publishes the autoscaling answer and the bounds, so the rule reads the engine rather
    // than dropping the members from the report.
    let own = expect_ok(
        "this door's own engine",
        call(
            &router,
            "DescribeOrderableDBInstanceOptions",
            &[
                ("Engine", OWN_ENGINE),
                ("DBInstanceClass", "db.r5.large"),
                ("MaxRecords", "20"),
            ],
        )
        .await,
    );
    let own_options = subtrees(&own, "OrderableDBInstanceOption");
    assert!(
        !own_options.is_empty(),
        "case \"this door's own engine\": expected its own options, got {own}"
    );
    for option in &own_options {
        for member in CLUSTER_MANAGED_STORAGE_MEMBERS {
            assert!(
                carries(option, member),
                "case \"this door's own engine\": expected {member} on an option whose \
                 instance owns its storage, got {option}"
            );
        }
    }
}

#[tokio::test]
async fn the_document_engine_is_orderable_in_the_classes_its_own_table_publishes() {
    let router = emulator();

    // The whole report for one release, held to the engine's OWN published table: this view
    // of the door reports the classes that table names and no others, so the table this
    // handler reads cannot drift from the catalog the engine's own control plane publishes.
    let version = ::service_common::docdb_engine_catalog::DEFAULT_ENGINE_VERSION;
    let storage_types = ::service_common::docdb_engine_catalog::STORAGE_TYPES.len();
    let published: Vec<String> = ::service_common::docdb_engine_catalog::instance_classes();
    let whole = expect_ok(
        "the whole report",
        call(
            &router,
            "DescribeOrderableDBInstanceOptions",
            &[
                ("Engine", DOCUMENT_ENGINE),
                ("EngineVersion", version),
                ("MaxRecords", "100"),
            ],
        )
        .await,
    );
    let mut reported: Vec<String> = texts(&whole, "DBInstanceClass");
    let option_count = reported.len();
    reported.sort_unstable();
    reported.dedup();
    let mut expected = published.clone();
    expected.sort_unstable();
    assert_eq!(
        reported, expected,
        "case \"the whole report\": expected the engine's own published classes, got \
         {reported:?}"
    );
    assert_eq!(
        option_count,
        published.len() * storage_types,
        "case \"the whole report\": expected each class once per published storage type, got \
         {option_count} option(s)"
    );

    for (class, offered) in CLASS_ROWS {
        let body = expect_ok(
            class,
            call(
                &router,
                "DescribeOrderableDBInstanceOptions",
                &[
                    ("Engine", DOCUMENT_ENGINE),
                    ("DBInstanceClass", class),
                    ("MaxRecords", "20"),
                ],
            )
            .await,
        );
        let named: Vec<String> = texts(&body, "DBInstanceClass");
        assert_eq!(
            !named.is_empty(),
            *offered,
            "case {class:?}: expected the engine's own table to {} this class, got {named:?}",
            if *offered { "name" } else { "exclude" }
        );
        for reported in &named {
            assert_eq!(
                reported, class,
                "case {class:?}: a class filter selects only the class it names, got \
                 {named:?}"
            );
        }
    }
}
