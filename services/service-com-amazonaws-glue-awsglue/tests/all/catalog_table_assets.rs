// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the Data Catalog entities the BUSINESS CATALOG indexes as assets:
//! `GetAsset` and `AssociateGlossaryTerms` against a catalog table's own ARN.
//!
//! RULE: a Data Catalog entity carries business context, so the business catalog indexes it as
//! an ASSET addressed by that entity's own ARN — `AssetId`'s pattern
//! `^[a-zA-Z0-9\-\:\/\.\_\*]+$` admits one (`smithy_query operation=GetAsset`) — so an asset
//! read of a table's ARN reports that table as an asset even though no `PutAsset` ever stored
//! it, and an association against that ARN attaches its terms while a disassociation removes
//! them, which the asset's own read then reports. An ARN naming no entity this catalog holds
//! resolves to nothing and is refused with the modelled `EntityNotFoundException`, exactly as
//! any other unknown asset identifier is.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour over state the catalog really holds.
//!
//! The rows differ along what the rule generalises over: how many terms one association
//! names, how many associations are stacked onto one table, which database the table lives in,
//! and how an ARN fails to name a held entity (an unknown table, an unknown database, an ARN
//! of another resource kind).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, emulator, error_code, error_message, ok, ACCOUNT_ID, REGION};

/// The `AssetTypeId` a read must report for a Data Catalog TABLE the business catalog has
/// indexed as an asset, written out here rather than read from the service's own constant, so
/// a change to the projected asset type is caught by this suite instead of followed by it.
const CATALOG_TABLE_ASSET_TYPE_ID: &str = "catalog::Table";

/// The database the tables of this suite live in.
const DATABASE: &str = "asset_projection_db";

/// The ARN of the catalog table `table` of `database`, whose resource portion carries both
/// names — the address the business catalog indexes that table under.
fn table_arn(database: &str, table: &str) -> String {
    format!("arn:aws:glue:{REGION}:{ACCOUNT_ID}:table/{database}/{table}")
}

/// Register the suite's database.
async fn create_database(router: &MultiServiceRouter, name: &str) {
    ok(
        router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": name } }),
    )
    .await;
}

/// Register a table, with the description the catalog's own form on an asset reports.
async fn create_table(
    router: &MultiServiceRouter,
    database: &str,
    table: &str,
    description: Option<&str>,
) {
    let mut input = json!({ "Name": table, "TableType": "EXTERNAL_TABLE" });
    if let Some(description) = description {
        input["Description"] = json!(description);
    }
    ok(
        router,
        "CreateTable",
        &json!({ "DatabaseName": database, "TableInput": input }),
    )
    .await;
}

/// Create a business glossary and return the identifier it minted.
async fn create_glossary(router: &MultiServiceRouter, name: &str) -> String {
    let created = ok(router, "CreateGlossary", &json!({ "Name": name })).await;
    created
        .get("Id")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("CreateGlossary must mint an Id, got {created}"))
        .to_string()
}

/// Create a glossary term and return the identifier it minted.
async fn create_term(router: &MultiServiceRouter, glossary: &str, name: &str) -> String {
    let created = ok(
        router,
        "CreateGlossaryTerm",
        &json!({ "GlossaryIdentifier": glossary, "Name": name }),
    )
    .await;
    created
        .get("Id")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("CreateGlossaryTerm must mint an Id, got {created}"))
        .to_string()
}

/// The `GlossaryTerms` a body reports, which is always a list.
fn reported_terms(case: &str, body: &Value) -> Vec<String> {
    let listed = body
        .get("GlossaryTerms")
        .unwrap_or_else(|| panic!("case {case}: the answer must carry GlossaryTerms, got {body}"));
    listed
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: GlossaryTerms must be a list, got {body}"))
        .iter()
        .filter_map(|held| held.as_str().map(str::to_string))
        .collect()
}

// ---------------------------------------------------------------------------
// A catalog table reads as an asset
// ---------------------------------------------------------------------------

/// A table this catalog holds is readable as an asset under its own ARN: the read reports the
/// table's name as the asset's, the asset type a Data Catalog table is indexed under, and the
/// empty glossary term list of an entity nothing has associated a term with.
#[tokio::test]
async fn a_catalog_table_reads_as_an_asset() {
    let router = emulator();
    create_database(&router, DATABASE).await;

    // (case, the table, its description)
    let cases: Vec<(&str, &str, Option<&str>)> = vec![
        ("a table with no description", "orders", None),
        (
            "a table carrying a description",
            "customers",
            Some("one row per customer"),
        ),
        ("a table whose name carries digits", "events_2026", None),
    ];

    for (case, table, description) in &cases {
        create_table(&router, DATABASE, table, *description).await;
        let identifier = table_arn(DATABASE, table);

        let read = ok(&router, "GetAsset", &json!({ "Identifier": &identifier })).await;
        assert_eq!(
            read.get("Id").and_then(Value::as_str),
            Some(identifier.as_str()),
            "case {case}: the asset must be reported under the identifier it was read by, got \
             {read}"
        );
        assert_eq!(
            read.get("Name").and_then(Value::as_str),
            Some(*table),
            "case {case}: the asset's name must be the table's, got {read}"
        );
        assert_eq!(
            read.get("AssetTypeId").and_then(Value::as_str),
            Some(CATALOG_TABLE_ASSET_TYPE_ID),
            "case {case}: expected the asset type a catalog table is indexed under, got {read}"
        );
        assert_eq!(
            read.get("Description").and_then(Value::as_str),
            *description,
            "case {case}: the asset's description must be the table's, got {read}"
        );
        assert!(
            reported_terms(case, &read).is_empty(),
            "case {case}: a table nothing has associated a term with must report an empty \
             list, got {read}"
        );
    }
}

/// An ARN that names no entity this catalog holds resolves to nothing: the read is refused
/// with the modelled miss, worded the one way this surface words an asset miss.
#[tokio::test]
async fn an_arn_naming_no_held_entity_is_the_modelled_miss() {
    let router = emulator();
    create_database(&router, DATABASE).await;
    create_table(&router, DATABASE, "held", None).await;

    // (case, the identifier the read addresses)
    let misses: Vec<(&str, String)> = vec![
        (
            "a table the database does not hold",
            table_arn(DATABASE, "never_created"),
        ),
        (
            "a database nothing created",
            table_arn("no_such_database", "held"),
        ),
        (
            "an ARN of another resource kind",
            format!("arn:aws:glue:{REGION}:{ACCOUNT_ID}:database/{DATABASE}"),
        ),
        (
            "an ARN naming the table of another account",
            format!("arn:aws:glue:{REGION}:999999999999:table/{DATABASE}/held"),
        ),
        ("an identifier that is no ARN at all", "held".to_string()),
    ];

    for (case, identifier) in &misses {
        let (status, body) = call(&router, "GetAsset", &json!({ "Identifier": identifier })).await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} with {body}"
        );
        assert!(
            error_code(&body).contains("EntityNotFoundException"),
            "case {case}: expected EntityNotFoundException, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!(r#"{{"Message":"Asset not found: {identifier}"}}"#),
            "case {case}: expected the asset surface's own miss wording, got {body}"
        );
    }
}

// ---------------------------------------------------------------------------
// The association attaches terms to the table
// ---------------------------------------------------------------------------

/// An association against a table's ARN attaches the terms it names, reports the whole set,
/// and the asset read of the same ARN reports exactly that set — so a table is associable as
/// the asset it is, and a second association adds to what the first wrote.
#[tokio::test]
async fn terms_associate_with_a_catalog_table() {
    let router = emulator();
    create_database(&router, DATABASE).await;
    let glossary = create_glossary(&router, "table-asset-glossary").await;
    let first = create_term(&router, &glossary, "Revenue").await;
    let second = create_term(&router, &glossary, "Customer").await;
    let third = create_term(&router, &glossary, "Region").await;

    // One row of the association table.
    struct Association {
        /// What this row varies, named in every assertion it drives.
        case: &'static str,
        /// The table the associations are made against.
        table: &'static str,
        /// The term ids each association in turn names.
        rounds: Vec<Vec<String>>,
        /// The set the table must carry afterwards.
        expected: Vec<String>,
    }

    let cases: Vec<Association> = vec![
        Association {
            case: "one association naming one term",
            table: "sales",
            rounds: vec![vec![first.clone()]],
            expected: vec![first.clone()],
        },
        Association {
            case: "one association naming two terms",
            table: "ledger",
            rounds: vec![vec![first.clone(), second.clone()]],
            expected: vec![first.clone(), second.clone()],
        },
        Association {
            case: "two associations, the second adding a term",
            table: "invoices",
            rounds: vec![vec![first.clone()], vec![third.clone()]],
            expected: vec![first.clone(), third.clone()],
        },
        Association {
            case: "the same association sent twice, which is not repeated",
            table: "payments",
            rounds: vec![vec![second.clone()], vec![second.clone()]],
            expected: vec![second.clone()],
        },
    ];

    for row in &cases {
        let case = row.case;
        create_table(&router, DATABASE, row.table, None).await;
        let identifier = table_arn(DATABASE, row.table);

        for named in &row.rounds {
            let answered = ok(
                &router,
                "AssociateGlossaryTerms",
                &json!({
                    "AssetIdentifier": &identifier,
                    "GlossaryTermIdentifiers": named,
                }),
            )
            .await;
            assert_eq!(
                answered.get("AssetIdentifier").and_then(Value::as_str),
                Some(identifier.as_str()),
                "case {case}: the association must name the asset it wrote to, got {answered}"
            );
        }

        let read = ok(&router, "GetAsset", &json!({ "Identifier": &identifier })).await;
        assert_eq!(
            reported_terms(case, &read),
            row.expected,
            "case {case}: expected the table to carry {:?}, got {read}",
            row.expected
        );
    }
}

/// A disassociation removes the terms it names from the table and answers the ones that
/// remain, which the asset's own read then reports: the rows span removing one of several,
/// removing every one, removing an id the table does not carry, and removing a term whose own
/// record a delete has already removed.
#[tokio::test]
async fn terms_disassociate_from_a_catalog_table() {
    let router = emulator();
    create_database(&router, DATABASE).await;
    let glossary = create_glossary(&router, "table-disassociation-glossary").await;
    let first = create_term(&router, &glossary, "First").await;
    let second = create_term(&router, &glossary, "Second").await;
    let deleted = create_term(&router, &glossary, "Deleted").await;

    // One row of the disassociation table: what the table starts with, what the
    // disassociation names, and what it must be left with.
    struct Removal {
        /// What this row varies, named in every assertion it drives.
        case: &'static str,
        /// The term ids the table is associated with before the removal.
        start: Vec<String>,
        /// The ids the disassociation names.
        removed: Vec<String>,
        /// The ids the table must carry afterwards.
        expected: Vec<String>,
    }

    let cases: Vec<Removal> = vec![
        Removal {
            case: "one of two terms removed",
            start: vec![first.clone(), second.clone()],
            removed: vec![first.clone()],
            expected: vec![second.clone()],
        },
        Removal {
            case: "every term removed",
            start: vec![first.clone(), second.clone()],
            removed: vec![first.clone(), second.clone()],
            expected: Vec::new(),
        },
        Removal {
            case: "an id the table does not carry, which removes nothing",
            start: vec![first.clone()],
            removed: vec!["no-such-term".to_string()],
            expected: vec![first.clone()],
        },
        Removal {
            case: "a term whose own record a delete has removed",
            start: vec![first.clone(), deleted.clone()],
            removed: vec![deleted.clone()],
            expected: vec![first.clone()],
        },
    ];

    for (index, row) in cases.iter().enumerate() {
        let case = row.case;
        let table = format!("disassociating_{index}");
        create_table(&router, DATABASE, &table, None).await;
        let identifier = table_arn(DATABASE, &table);
        ok(
            &router,
            "AssociateGlossaryTerms",
            &json!({ "AssetIdentifier": &identifier, "GlossaryTermIdentifiers": row.start }),
        )
        .await;
        // The row that removes a DELETED term deletes it only now, so the association above
        // was made while the term was still held — which is the state a client reaches.
        if row.removed.contains(&deleted) {
            ok(
                &router,
                "DeleteGlossaryTerm",
                &json!({ "Identifier": &deleted }),
            )
            .await;
        }

        let answered = ok(
            &router,
            "DisassociateGlossaryTerms",
            &json!({ "AssetIdentifier": &identifier, "GlossaryTermIdentifiers": row.removed }),
        )
        .await;
        assert_eq!(
            reported_terms(case, &answered),
            row.expected,
            "case {case}: expected the remaining terms {:?}, got {answered}",
            row.expected
        );
        assert_eq!(
            answered.get("AssetIdentifier").and_then(Value::as_str),
            Some(identifier.as_str()),
            "case {case}: the disassociation must name the asset it wrote to, got {answered}"
        );

        let read = ok(&router, "GetAsset", &json!({ "Identifier": &identifier })).await;
        assert_eq!(
            reported_terms(case, &read),
            row.expected,
            "case {case}: a later read must report the remaining terms, got {read}"
        );
    }

    // An identifier naming no held entity is the modelled miss for a disassociation too.
    let (status, body) = call(
        &router,
        "DisassociateGlossaryTerms",
        &json!({
            "AssetIdentifier": table_arn(DATABASE, "never_created"),
            "GlossaryTermIdentifiers": [&first],
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case a table the database does not hold: expected 400, got {status} with {body}"
    );
    assert!(
        error_code(&body).contains("EntityNotFoundException"),
        "case a table the database does not hold: expected EntityNotFoundException, got {body}"
    );
}

/// The terms live on the TABLE, so they are the table's own: another table of the same
/// database carries none of them, and a table dropped and created again under the same name
/// starts from none.
#[tokio::test]
async fn the_terms_belong_to_the_table_they_were_associated_with() {
    let router = emulator();
    create_database(&router, DATABASE).await;
    let glossary = create_glossary(&router, "table-scope-glossary").await;
    let term = create_term(&router, &glossary, "Scoped").await;
    create_table(&router, DATABASE, "associated", None).await;
    create_table(&router, DATABASE, "untouched", None).await;

    let associated = table_arn(DATABASE, "associated");
    ok(
        &router,
        "AssociateGlossaryTerms",
        &json!({
            "AssetIdentifier": &associated,
            "GlossaryTermIdentifiers": [&term],
        }),
    )
    .await;

    let other = ok(
        &router,
        "GetAsset",
        &json!({ "Identifier": table_arn(DATABASE, "untouched") }),
    )
    .await;
    assert!(
        reported_terms("another table of the same database", &other).is_empty(),
        "case another table of the same database: it must carry no term, got {other}"
    );

    // Dropping the table drops what the catalog held against it, so the name is free of the
    // association when a table is created under it again.
    ok(
        &router,
        "DeleteTable",
        &json!({ "DatabaseName": DATABASE, "Name": "associated" }),
    )
    .await;
    let (status, body) = call(&router, "GetAsset", &json!({ "Identifier": &associated })).await;
    assert_eq!(
        status, 400,
        "case the table was dropped: its asset must resolve to nothing, got {status} with {body}"
    );
    create_table(&router, DATABASE, "associated", None).await;
    let again = ok(&router, "GetAsset", &json!({ "Identifier": &associated })).await;
    assert!(
        reported_terms("a table created again under the same name", &again).is_empty(),
        "case a table created again under the same name: it must carry no term, got {again}"
    );
}

/// An association against an ARN that names no held entity is refused with the modelled miss
/// and attaches nothing, and one naming a term the catalog holds no live term under is refused
/// as invalid input, leaving the table's own set as it was.
#[tokio::test]
async fn an_association_with_an_unresolvable_reference_is_refused() {
    let router = emulator();
    create_database(&router, DATABASE).await;
    create_table(&router, DATABASE, "guarded", None).await;
    let glossary = create_glossary(&router, "table-refusal-glossary").await;
    let held = create_term(&router, &glossary, "Held").await;
    let deleted = create_term(&router, &glossary, "Deleted").await;
    ok(
        &router,
        "DeleteGlossaryTerm",
        &json!({ "Identifier": &deleted }),
    )
    .await;

    let guarded = table_arn(DATABASE, "guarded");
    ok(
        &router,
        "AssociateGlossaryTerms",
        &json!({ "AssetIdentifier": &guarded, "GlossaryTermIdentifiers": [&held] }),
    )
    .await;

    // (case, the asset the association addresses, the terms it names, the error code)
    let refusals: Vec<(&str, String, Vec<String>, &str)> = vec![
        (
            "a table the database does not hold",
            table_arn(DATABASE, "never_created"),
            vec![held.clone()],
            "EntityNotFoundException",
        ),
        (
            "a database nothing created",
            table_arn("no_such_database", "guarded"),
            vec![held.clone()],
            "EntityNotFoundException",
        ),
        (
            "a term a delete removed",
            guarded.clone(),
            vec![deleted.clone()],
            "InvalidInputException",
        ),
        (
            "one held term beside one that resolves to nothing",
            guarded.clone(),
            vec![held.clone(), "no-such-term".to_string()],
            "InvalidInputException",
        ),
    ];

    for (case, identifier, terms, expected) in &refusals {
        let (status, body) = call(
            &router,
            "AssociateGlossaryTerms",
            &json!({ "AssetIdentifier": identifier, "GlossaryTermIdentifiers": terms }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} with {body}"
        );
        assert!(
            error_code(&body).contains(expected),
            "case {case}: expected {expected}, got {body}"
        );
    }

    let read = ok(&router, "GetAsset", &json!({ "Identifier": &guarded })).await;
    assert_eq!(
        reported_terms("after the refusals", &read),
        vec![held],
        "a refused association must change nothing, got {read}"
    );
}
