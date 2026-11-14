// SPDX-License-Identifier: CC0-1.0

//! The Iceberg REST catalog's DISCOVERY endpoints: how an engine finds the
//! warehouse it was configured for, the namespaces in it and the tables in a
//! namespace.
//!
//! The rule under test: the catalog projects the SAME records the control plane
//! reads and writes. `GET /iceberg/v1/config` answers the configuration of the
//! warehouse the request names — the path prefix every later request of the
//! session is addressed under, which is that warehouse's own ARN as a path
//! segment — and a warehouse the store does not hold is reported as absent;
//! `GET /iceberg/v1/{prefix}/namespaces` answers exactly the namespaces
//! `ListNamespaces` lists; `GET /iceberg/v1/{prefix}/namespaces/{namespace}`
//! answers that namespace, and `HEAD` whether it is there; and
//! `GET /iceberg/v1/{prefix}/namespaces/{namespace}/tables` answers exactly the
//! tables `ListTables` lists.
//!
//! The rows span what the rule generalises over: how many namespaces a warehouse
//! holds (none, one, several), how many tables a namespace holds (none, one,
//! several), the region, and each coordinate being absent. The prefix the
//! configuration answers is not merely compared — it is USED to address the
//! endpoints beside it, which is what proves a client configured from this
//! answer reaches the warehouse.

use serde_json::{json, Value};

use crate::harness::{encode, expected_bucket_arn, Emulator};

/// One warehouse of this suite: a bucket, the namespaces in it and the tables in
/// each.
struct Warehouse {
    region: &'static str,
    bucket: &'static str,
    /// `(namespace, tables)`, in no particular order — the catalog's own order
    /// is what the assertions read.
    namespaces: Vec<(&'static str, Vec<&'static str>)>,
}

/// The warehouses this suite serves: an empty one, one with a single namespace
/// and a single table, and one with several namespaces of several (and no)
/// tables, in another region.
fn warehouses() -> Vec<Warehouse> {
    vec![
        Warehouse {
            region: "us-east-1",
            bucket: "catalog-discovery-empty",
            namespaces: Vec::new(),
        },
        Warehouse {
            region: "us-east-1",
            bucket: "catalog-discovery-one",
            namespaces: vec![("analytics", vec!["user_events"])],
        },
        Warehouse {
            region: "eu-west-2",
            bucket: "catalog-discovery-many",
            namespaces: vec![
                ("analytics", vec!["user_events", "page_views", "sessions"]),
                ("staging", vec!["landing"]),
                ("empty_namespace", Vec::new()),
            ],
        },
    ]
}

/// Create `warehouse` and answer its table bucket ARN.
async fn create(emulator: &Emulator, warehouse: &Warehouse) -> String {
    let created = emulator
        .create_table_bucket(warehouse.region, &json!({ "name": warehouse.bucket }))
        .await;
    assert_eq!(
        created.status, 200,
        "creating the bucket {bucket:?} must succeed, got {status} {body}",
        bucket = warehouse.bucket,
        status = created.status,
        body = created.body
    );
    let arn = created.text("arn");
    for (namespace, tables) in &warehouse.namespaces {
        let made = emulator
            .create_namespace(warehouse.region, &arn, &json!({ "namespace": [namespace] }))
            .await;
        assert_eq!(
            made.status, 200,
            "creating the namespace {namespace:?} must succeed, got {status} {body}",
            status = made.status,
            body = made.body
        );
        for table in tables {
            let made = emulator
                .create_table(
                    warehouse.region,
                    &arn,
                    namespace,
                    &json!({ "name": table, "format": "ICEBERG" }),
                )
                .await;
            assert_eq!(
                made.status, 200,
                "creating the table {table:?} must succeed, got {status} {body}",
                status = made.status,
                body = made.body
            );
        }
    }
    arn
}

/// A member of `body` as a sorted list of its JSON texts, so two listings of the
/// same resources compare equal whatever order they came in.
fn sorted(body: &Value, member: &str, case: &str) -> Vec<String> {
    let mut entries: Vec<String> = body
        .get(member)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: the answer has no {member} list: {body}"))
        .iter()
        .map(Value::to_string)
        .collect();
    entries.sort();
    entries
}

#[tokio::test]
async fn the_configuration_answers_a_prefix_that_addresses_the_warehouse() {
    let emulator = Emulator::new();

    for warehouse in warehouses() {
        let arn = create(&emulator, &warehouse).await;
        let case = warehouse.bucket;

        let config = emulator
            .catalog_config(warehouse.region, &[("warehouse", arn.clone())])
            .await;
        assert_eq!(
            config.status, 200,
            "case {case:?}: the configuration must be served, got {status} {body}",
            status = config.status,
            body = config.body
        );
        let defaults = config
            .body
            .get("defaults")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("case {case:?}: the configuration has no defaults: {body}", body = config.body))
            .clone();
        // The prefix is the warehouse ARN as a path SEGMENT, so its `:` and `/`
        // are escaped: a client builds the path of its next request by putting
        // this value into one.
        let prefix = defaults
            .get("prefix")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            prefix,
            encode(&arn),
            "case {case:?}: the prefix must be the warehouse ARN, escaped for a path segment"
        );
        // Nothing a client configures is overridden.
        assert_eq!(
            config.body.get("overrides"),
            Some(&json!({})),
            "case {case:?}: the catalog must override nothing, got {actual:?}",
            actual = config.body.get("overrides")
        );

        // The prefix WORKS: the namespaces endpoint addressed under it answers
        // the warehouse's namespaces.
        let under_prefix = emulator
            .call(
                "GET",
                &format!("/iceberg/v1/{prefix}/namespaces"),
                warehouse.region,
                None,
            )
            .await;
        assert_eq!(
            under_prefix.status, 200,
            "case {case:?}: the prefix the configuration answered must address the warehouse, \
             got {status} {body}",
            status = under_prefix.status,
            body = under_prefix.body
        );
        assert_eq!(
            sorted(&under_prefix.body, "namespaces", case).len(),
            warehouse.namespaces.len(),
            "case {case:?}: the listing under the prefix must carry every namespace of the \
             warehouse, got {body}",
            body = under_prefix.body
        );
    }
}

#[tokio::test]
async fn a_warehouse_the_store_does_not_hold_has_no_configuration() {
    let emulator = Emulator::new();
    let region = "us-east-1";

    // A configuration request that names no warehouse states no table bucket to
    // answer for at all.
    let nameless = emulator.catalog_config(region, &[]).await;
    assert_eq!(
        nameless.status, 400,
        "a configuration request naming no warehouse must be refused, got {status} {body}",
        status = nameless.status,
        body = nameless.body
    );

    // One that names a warehouse this store does not hold is answered as absent,
    // whether the value could be a table bucket ARN or not.
    for case in [
        expected_bucket_arn(region, "never-created-warehouse"),
        "not-an-arn-at-all".to_string(),
    ] {
        let unknown = emulator
            .catalog_config(region, &[("warehouse", case.clone())])
            .await;
        assert_eq!(
            unknown.status, 404,
            "case {case:?}: an unknown warehouse must be reported as absent, got {status} {body}",
            status = unknown.status,
            body = unknown.body
        );
    }
}

#[tokio::test]
async fn the_catalog_and_the_control_plane_report_the_same_namespaces_and_tables() {
    let emulator = Emulator::new();

    for warehouse in warehouses() {
        let arn = create(&emulator, &warehouse).await;
        let case = warehouse.bucket;

        // The namespaces: the catalog spells each as the LIST of its levels, the
        // control plane as the same list under `namespace`, so the two compare
        // element for element.
        let catalog = emulator
            .catalog_namespaces(warehouse.region, &arn, &[])
            .await;
        assert_eq!(
            catalog.status, 200,
            "case {case:?}: the namespace listing must be served, got {status} {body}",
            status = catalog.status,
            body = catalog.body
        );
        let control_plane = emulator.list_namespaces(warehouse.region, &arn, &[]).await;
        let mut expected: Vec<String> = control_plane
            .list("namespaces")
            .iter()
            .map(|row| {
                row.get("namespace")
                    .unwrap_or(&Value::Null)
                    .to_string()
            })
            .collect();
        expected.sort();
        assert_eq!(
            sorted(&catalog.body, "namespaces", case),
            expected,
            "case {case:?}: the catalog must list the namespaces the control plane lists"
        );

        for (namespace, _) in &warehouse.namespaces {
            // The namespace itself, and whether it exists.
            let loaded = emulator
                .catalog_namespace("GET", warehouse.region, &arn, namespace)
                .await;
            assert_eq!(
                loaded.status, 200,
                "case {case:?}/{namespace:?}: the namespace must be served, got {status} {body}",
                status = loaded.status,
                body = loaded.body
            );
            assert_eq!(
                loaded.body.get("namespace"),
                Some(&json!([namespace])),
                "case {case:?}/{namespace:?}: the namespace must come back as its levels, got \
                 {body}",
                body = loaded.body
            );
            let exists = emulator
                .catalog_namespace("HEAD", warehouse.region, &arn, namespace)
                .await;
            assert_eq!(
                exists.status, 204,
                "case {case:?}/{namespace:?}: the existence check must answer that it is there, \
                 got {status}",
                status = exists.status
            );

            // The tables of that namespace, against the control plane's own
            // listing of them.
            let catalog_tables = emulator
                .catalog_tables(warehouse.region, &arn, namespace)
                .await;
            assert_eq!(
                catalog_tables.status, 200,
                "case {case:?}/{namespace:?}: the table listing must be served, got {status} \
                 {body}",
                status = catalog_tables.status,
                body = catalog_tables.body
            );
            let listed = emulator
                .list_tables(
                    warehouse.region,
                    &arn,
                    &[("namespace", (*namespace).to_string())],
                )
                .await;
            let mut expected: Vec<String> = listed
                .list("tables")
                .iter()
                .map(|row| {
                    json!({
                        "namespace": row.get("namespace").cloned().unwrap_or(Value::Null),
                        "name": row.get("name").cloned().unwrap_or(Value::Null),
                    })
                    .to_string()
                })
                .collect();
            expected.sort();
            assert_eq!(
                sorted(&catalog_tables.body, "identifiers", case),
                expected,
                "case {case:?}/{namespace:?}: the catalog must list the tables the control plane \
                 lists"
            );
        }
    }
}

#[tokio::test]
async fn a_namespace_the_warehouse_does_not_hold_is_reported_as_absent() {
    let emulator = Emulator::new();
    let warehouse = Warehouse {
        region: "us-east-1",
        bucket: "catalog-discovery-absent",
        namespaces: vec![("analytics", vec!["user_events"])],
    };
    let arn = create(&emulator, &warehouse).await;
    let absent_bucket = expected_bucket_arn(warehouse.region, "never-created-warehouse");

    // A namespace that was never created is answered as absent rather than as
    // unimplemented, by each of the three endpoints that address one.
    for case in ["never_created"] {
        let loaded = emulator
            .catalog_namespace("GET", warehouse.region, &arn, case)
            .await;
        assert_eq!(
            loaded.status, 404,
            "case {case:?}: the namespace must be reported as absent, got {status} {body}",
            status = loaded.status,
            body = loaded.body
        );
        let exists = emulator
            .catalog_namespace("HEAD", warehouse.region, &arn, case)
            .await;
        assert_eq!(
            exists.status, 404,
            "case {case:?}: the existence check must answer that it is not there, got {status}",
            status = exists.status
        );
        let tables = emulator
            .catalog_tables(warehouse.region, &arn, case)
            .await;
        assert_eq!(
            tables.status, 404,
            "case {case:?}: the table listing must report the namespace as absent, got {status} \
             {body}",
            status = tables.status,
            body = tables.body
        );
    }

    // A namespace of MORE levels than this service has — the Iceberg REST
    // catalog spells a multi-level namespace with the unit separator — names no
    // namespace of this catalog, even where its first level does.
    let multi_level = emulator
        .call(
            "GET",
            &format!("/iceberg/v1/{}/namespaces/analytics%1Fchild", encode(&arn)),
            warehouse.region,
            None,
        )
        .await;
    assert_eq!(
        multi_level.status, 404,
        "a namespace of two levels must be reported as absent, got {status} {body}",
        status = multi_level.status,
        body = multi_level.body
    );

    let foreign = emulator
        .catalog_namespaces(warehouse.region, &absent_bucket, &[])
        .await;
    assert_eq!(
        foreign.status, 404,
        "a warehouse that does not exist must be reported as absent, got {status} {body}",
        status = foreign.status,
        body = foreign.body
    );

    // A listing scoped to a parent: a namespace of this service has one level,
    // so a parent it holds has no children, and a parent it does not hold is
    // absent.
    let under_resident = emulator
        .catalog_namespaces(warehouse.region, &arn, &[("parent", "analytics".to_string())])
        .await;
    assert_eq!(
        under_resident.status, 200,
        "a resident parent must be served, got {status} {body}",
        status = under_resident.status,
        body = under_resident.body
    );
    assert_eq!(
        under_resident.body.get("namespaces"),
        Some(&json!([])),
        "a namespace of this service has no children, got {body}",
        body = under_resident.body
    );
    let under_absent = emulator
        .catalog_namespaces(
            warehouse.region,
            &arn,
            &[("parent", "never_created".to_string())],
        )
        .await;
    assert_eq!(
        under_absent.status, 404,
        "a parent that does not exist must be reported as absent, got {status} {body}",
        status = under_absent.status,
        body = under_absent.body
    );
}
