// SPDX-License-Identifier: CC0-1.0

//! `ListTables`, driven end-to-end over the wire: every call below is a real
//! `POST /` with `X-Amz-Target: RedshiftData.ListTables` dispatched through the
//! router.
//!
//! The RULE: the operation "lists the tables in a database" out of that
//! database's own catalogue — one entry per relation it holds, carrying the
//! schema that contains it, its name and what kind of relation it is — narrowed
//! by the two patterns the operation documents ("`%` means match any substring
//! of 0 or more characters and `_` means match any one character"), and paged
//! with an opaque `NextToken`. Only a database the warehouse does not hold is a
//! missing resource.

use serde_json::{json, Value};

use super::emulator::{error_code, list, Emulator};

/// The connection every request in this suite is made through.
fn cluster() -> Value {
    json!({"ClusterIdentifier": "my-cluster", "Database": "dev", "DbUser": "awsuser"})
}

/// The `Tables` a response reports, as `<schema>.<name> <type>` triples, which
/// is every member of each entry in one comparable string.
fn reported(response: &Value) -> Vec<String> {
    list(response, "Tables")
        .iter()
        .map(|table| {
            format!(
                "{}.{} {}",
                table["schema"].as_str().unwrap_or_default(),
                table["name"].as_str().unwrap_or_default(),
                table["type"].as_str().unwrap_or_default()
            )
        })
        .collect()
}

/// Create the relations every case below reads: tables of the default schema,
/// a table of a schema of its own, and a view.
async fn catalogued() -> Emulator {
    let emulator = Emulator::new();
    emulator.run("CREATE TABLE orders (id INTEGER)").await;
    emulator.run("CREATE TABLE order_items (id INTEGER)").await;
    emulator.run("CREATE TABLE people (id INTEGER)").await;
    emulator.run("CREATE SCHEMA sales").await;
    emulator
        .run("CREATE TABLE sales.regions (id INTEGER)")
        .await;
    emulator
        .run("CREATE VIEW recent AS SELECT id FROM orders")
        .await;
    emulator
}

/// Every relation the database holds is listed, each with the schema that
/// contains it, its name and its kind.
#[tokio::test]
async fn every_relation_the_database_holds_is_listed() {
    let emulator = catalogued().await;
    let listed = emulator.ok("ListTables", &cluster()).await;
    assert_eq!(
        reported(&listed),
        vec![
            "public.order_items TABLE".to_string(),
            "public.orders TABLE".to_string(),
            "public.people TABLE".to_string(),
            "public.recent VIEW".to_string(),
            "sales.regions TABLE".to_string(),
        ],
        "every relation of the database is listed with its schema and kind, got {listed}"
    );
    assert!(
        listed.get("NextToken").is_none(),
        "one page held every table, so no continuation token is issued, got {listed}"
    );

    // A database the warehouse holds that no statement has built anything in
    // holds no table — an empty list, not a refusal.
    let mut request = cluster();
    request["Database"] = json!("dev2");
    let (status, response) = emulator.call("ListTables", &request).await;
    assert_eq!(
        status, 404,
        "a database the warehouse does not hold is a missing resource, got {status} {response}"
    );
    assert_eq!(
        error_code(&response),
        "ResourceNotFoundException",
        "expected ResourceNotFoundException, got {response}"
    );
}

/// One row of the pattern table: the patterns a request carries, and the
/// `<schema>.<name>` of every relation they must match.
struct PatternRow {
    label: &'static str,
    schema_pattern: Option<&'static str>,
    table_pattern: Option<&'static str>,
    matched: &'static [&'static str],
}

/// The two patterns narrow the listing on the name each filters, with `%`
/// matching any substring and `_` any one character; a pattern the request does
/// not carry filters nothing.
#[tokio::test]
async fn the_patterns_narrow_the_listing() {
    let emulator = catalogued().await;

    // The rows differ in which pattern is given, in which wild card it uses and
    // where, and in how many relations it leaves.
    let rows: &[PatternRow] = &[
        PatternRow {
            label: "neither pattern",
            schema_pattern: None,
            table_pattern: None,
            matched: &[
                "public.order_items",
                "public.orders",
                "public.people",
                "public.recent",
                "sales.regions",
            ],
        },
        PatternRow {
            label: "a table pattern with a trailing wild card",
            schema_pattern: None,
            table_pattern: Some("order%"),
            matched: &["public.order_items", "public.orders"],
        },
        PatternRow {
            label: "a table pattern with a leading wild card",
            schema_pattern: None,
            table_pattern: Some("%s"),
            matched: &["public.order_items", "public.orders", "sales.regions"],
        },
        PatternRow {
            label: "a table pattern of exactly one name",
            schema_pattern: None,
            table_pattern: Some("people"),
            matched: &["public.people"],
        },
        PatternRow {
            label: "the single-character wild card",
            schema_pattern: None,
            table_pattern: Some("_eople"),
            matched: &["public.people"],
        },
        PatternRow {
            label: "a schema pattern",
            schema_pattern: Some("sal%"),
            table_pattern: None,
            matched: &["sales.regions"],
        },
        PatternRow {
            label: "both patterns at once",
            schema_pattern: Some("pub%"),
            table_pattern: Some("order_"),
            matched: &["public.orders"],
        },
        PatternRow {
            label: "a pattern nothing matches",
            schema_pattern: None,
            table_pattern: Some("absent%"),
            matched: &[],
        },
        PatternRow {
            label: "a schema pattern nothing matches",
            schema_pattern: Some("other"),
            table_pattern: None,
            matched: &[],
        },
    ];

    for row in rows {
        let label = row.label;
        let mut request = cluster();
        if let Some(pattern) = row.schema_pattern {
            request["SchemaPattern"] = json!(pattern);
        }
        if let Some(pattern) = row.table_pattern {
            request["TablePattern"] = json!(pattern);
        }
        let listed = emulator.ok("ListTables", &request).await;
        let matched: Vec<String> = list(&listed, "Tables")
            .iter()
            .map(|table| {
                format!(
                    "{}.{}",
                    table["schema"].as_str().unwrap_or_default(),
                    table["name"].as_str().unwrap_or_default()
                )
            })
            .collect();
        assert_eq!(
            matched,
            row.matched
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<String>>(),
            "case {label}: expected {:?}, got {listed}",
            row.matched
        );
    }
}

/// The listing is paged with an opaque `NextToken`: every page carries at most
/// `MaxResults` entries, the pages together carry the whole listing exactly
/// once, and a token this listing did not issue is refused.
#[tokio::test]
async fn the_listing_is_paged_with_an_opaque_token() {
    let emulator = catalogued().await;
    let whole = reported(&emulator.ok("ListTables", &cluster()).await);

    let mut collected: Vec<String> = Vec::new();
    let mut next_token: Option<String> = None;
    let mut pages = 0_usize;
    loop {
        let mut request = cluster();
        request["MaxResults"] = json!(2);
        if let Some(token) = &next_token {
            request["NextToken"] = json!(token);
        }
        let page = emulator.ok("ListTables", &request).await;
        let entries = reported(&page);
        assert!(
            entries.len() <= 2,
            "page {pages}: MaxResults 2 holds at most two entries, got {page}"
        );
        collected.extend(entries);
        pages += 1;
        match page.get("NextToken").and_then(Value::as_str) {
            Some(token) => next_token = Some(token.to_string()),
            None => break,
        }
        assert!(
            pages < 10,
            "the listing must run out of pages: {collected:?}"
        );
    }
    assert_eq!(
        collected, whole,
        "the pages together carry the whole listing, in the same order"
    );
    assert_eq!(
        pages, 3,
        "five entries at two per page take three pages, got {pages}"
    );

    // A token this operation never issued is not a token.
    for token in ["not-hexadecimal", "6162636465"] {
        let mut request = cluster();
        request["NextToken"] = json!(token);
        let (status, response) = emulator.call("ListTables", &request).await;
        assert_eq!(
            status, 400,
            "case {token:?}: expected 400, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {token:?}: expected ValidationException, got {response}"
        );
    }
}
