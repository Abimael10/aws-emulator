// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `SearchAssets`, the search over the assets `PutAsset` writes and
//! `GetAsset` reads back.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is the HTTP behaviour and not a direct call into a handler.
//!
//! The rule under test: a search reports the stored assets its request selects — the ones
//! carrying every term of its `SearchText` in their own searchable text, and satisfying every
//! comparison its `FilterClause` makes against one attribute of them — ordered by the
//! attribute its `Sort` names or, with no `Sort`, by relevance, one `MaxResults`-sized page at
//! a time over that one total order; and a request that names neither text nor filter, or one
//! that names an attribute no asset carries, is refused rather than answered.
//!
//! Each suite is a table whose rows differ along what the rule generalises over: where in the
//! asset a term is carried, how many terms the text carries, which attribute a filter
//! compares and by which operator, how the clauses nest, which attribute orders the hits and
//! in which direction, and how a page size divides the hits.

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, emulator, error_code, error_message, ok};

/// The asset type every asset staged here is an instance of: one name, so the asset type is
/// never what distinguishes two rows.
const ASSET_TYPE: &str = "DataTable";

/// Register a form type and return the identifier the catalog assigned it.
async fn form_type(router: &MultiServiceRouter, name: &str) -> String {
    let stored = ok(
        router,
        "PutFormType",
        &json!({ "Name": name, "Schema": format!("structure {name} {{ note: String }}") }),
    )
    .await;
    stored
        .get("Id")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("PutFormType must report an Id, got {stored}"))
        .to_string()
}

/// Register the one asset type these suites use, over one form type, and report the form
/// type's identifier — which the assets below name in their own forms.
async fn given_asset_type(router: &MultiServiceRouter) -> String {
    let form = form_type(router, "TableForm").await;
    let mut forms = Map::new();
    forms.insert(
        "Details".to_string(),
        json!({ "FormTypeIdentifier": &form }),
    );
    ok(
        router,
        "PutAssetType",
        &json!({ "Name": ASSET_TYPE, "Forms": Value::Object(forms) }),
    )
    .await;
    form
}

/// One asset to stage: its identifier, its name, the description it carries (if any) and the
/// JSON content of the one form it holds (if any).
struct Staged<'a> {
    id: &'a str,
    name: &'a str,
    description: Option<&'a str>,
    content: Option<&'a str>,
}

/// Store one asset, an instance of [`ASSET_TYPE`], with the form content it carries held under
/// the form name `Details`.
async fn put_asset(router: &MultiServiceRouter, form_type_id: &str, asset: &Staged<'_>) {
    let mut forms = Map::new();
    if let Some(content) = asset.content {
        forms.insert(
            "Details".to_string(),
            json!({ "FormTypeId": form_type_id, "Content": content }),
        );
    }
    let mut body = json!({
        "Identifier": asset.id,
        "AssetTypeId": ASSET_TYPE,
        "Name": asset.name,
        "Forms": Value::Object(forms),
    });
    if let Some(description) = asset.description {
        body["Description"] = json!(description);
    }
    ok(router, "PutAsset", &body).await;
}

/// Search, asserting the modelled 200, and report the answer.
async fn search(router: &MultiServiceRouter, case: &str, request: &Value) -> Value {
    let (status, body) = call(router, "SearchAssets", request).await;
    assert_eq!(
        status, 200,
        "case {case}: SearchAssets {request} must answer 200, got {status} with {body}"
    );
    body
}

/// The identifiers of the hits an answer reports, in the order the service reported them.
fn hit_ids(case: &str, body: &Value) -> Vec<String> {
    body.get("Items")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case}: every answer must report Items, got {body}"))
        .iter()
        .map(|item| {
            item.get("Id")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case}: every hit carries an Id, got {item}"))
                .to_string()
        })
        .collect()
}

/// The same, sorted — for a rule about WHICH assets are selected rather than about their
/// order.
fn hit_id_set(case: &str, body: &Value) -> Vec<String> {
    let mut ids = hit_ids(case, body);
    ids.sort();
    ids
}

/// Assert one refusal: HTTP 400, `InvalidInputException`, and `expected` as the whole
/// `Message`.
async fn assert_refused(router: &MultiServiceRouter, case: &str, request: &Value, expected: &str) {
    let (status, body) = call(router, "SearchAssets", request).await;
    assert_eq!(
        status, 400,
        "case {case}: SearchAssets {request} must be refused with 400, got {status} with {body}"
    );
    assert!(
        error_code(&body).contains("InvalidInputException"),
        "case {case}: expected InvalidInputException, got {}",
        error_code(&body)
    );
    assert_eq!(
        error_message(&body),
        expected,
        "case {case}: expected Message {expected:?}, got {:?}",
        error_message(&body)
    );
}

// ---------------------------------------------------------------------------
// Free-text search
// ---------------------------------------------------------------------------

/// The assets the text suite searches over: a term is carried in a different place in each,
/// which is what its rows differ along.
fn text_corpus() -> Vec<Staged<'static>> {
    vec![
        Staged {
            id: "orders-raw",
            name: "Orders Raw",
            description: Some("Raw order events landed hourly"),
            content: Some(r#"{"tableName":"orders_raw","owner":"finance"}"#),
        },
        Staged {
            id: "orders-curated",
            name: "Orders Curated",
            description: Some("Curated orders for analytics"),
            content: None,
        },
        Staged {
            id: "customers",
            name: "Customer Profiles",
            description: None,
            content: Some(r#"{"tableName":"customer_profiles"}"#),
        },
        Staged {
            id: "shipments-eu",
            name: "Shipments EU",
            description: Some("Shipment events for Europe"),
            content: None,
        },
    ]
}

/// A `SearchText` selects the assets whose own searchable text carries EVERY term of it: a
/// term is carried by the asset's name, identifier, description or metadata, matched against
/// the beginning of one of that value's words and without regard to case.
///
/// The rows differ along where the term is carried, how it is spelled relative to the stored
/// value (whole word, recased, a prefix of a word, one part of a punctuated value) and how
/// many terms the text carries.
#[tokio::test]
async fn a_text_search_selects_the_assets_carrying_every_term() {
    let router = emulator();
    let form = given_asset_type(&router).await;
    for asset in text_corpus() {
        put_asset(&router, &form, &asset).await;
    }

    // (case, search text, the identifiers that must be reported)
    let rows: Vec<(&str, &str, Vec<&str>)> = vec![
        ("a whole word of the name", "Curated", vec!["orders-curated"]),
        ("the same word recased", "cURATED", vec!["orders-curated"]),
        ("a prefix of a word", "ship", vec!["shipments-eu"]),
        ("a word of the description", "hourly", vec!["orders-raw"]),
        ("a word of a form's content", "finance", vec!["orders-raw"]),
        ("a word of the identifier", "customers", vec!["customers"]),
        (
            "a word shared by two assets",
            "orders",
            vec!["orders-curated", "orders-raw"],
        ),
        (
            "two terms, which one asset carries together",
            "orders analytics",
            vec!["orders-curated"],
        ),
        (
            "two terms carried by different assets",
            "hourly analytics",
            vec![],
        ),
        ("a word no asset carries", "nonexistent", vec![]),
        (
            "one part of a punctuated identifier",
            "eu",
            vec!["shipments-eu"],
        ),
        (
            "the asset type every asset is an instance of",
            ASSET_TYPE,
            vec!["customers", "orders-curated", "orders-raw", "shipments-eu"],
        ),
    ];

    for (case, text, expected) in rows {
        let body = search(&router, case, &json!({ "SearchText": text })).await;
        assert_eq!(
            hit_id_set(case, &body),
            expected,
            "case {case}: searching for {text:?} must report {expected:?}, got {body}"
        );
    }
}

/// With no `Sort`, the hits are RELEVANCE-ranked: the asset whose name carries the term comes
/// before the one whose identifier does, which comes before the one whose description does,
/// which comes before the one that carries it only in a form's content.
#[tokio::test]
async fn hits_are_relevance_ranked_when_no_sort_is_named() {
    let router = emulator();
    let form = given_asset_type(&router).await;
    let staged = [
        Staged {
            id: "metrics-a",
            name: "Beacon Metrics",
            description: None,
            content: None,
        },
        Staged {
            id: "beacon-b",
            name: "Signals Daily",
            description: None,
            content: None,
        },
        Staged {
            id: "events-c",
            name: "Events Hourly",
            description: Some("Beacon derived events"),
            content: None,
        },
        Staged {
            id: "lineage-d",
            name: "Lineage",
            description: None,
            content: Some(r#"{"source":"beacon"}"#),
        },
    ];
    for asset in &staged {
        put_asset(&router, &form, asset).await;
    }

    let case = "one term carried in four different places";
    let body = search(&router, case, &json!({ "SearchText": "beacon" })).await;
    assert_eq!(
        hit_ids(case, &body),
        vec![
            "metrics-a".to_string(),
            "beacon-b".to_string(),
            "events-c".to_string(),
            "lineage-d".to_string(),
        ],
        "case {case}: a name hit must rank above an identifier hit, above a description hit, \
         above a metadata hit, got {body}"
    );
}

/// Every member of a hit is the one the asset's own read reports, so a client working from a
/// search is told what a `GetAsset` of that identifier would tell it.
#[tokio::test]
async fn a_hit_reports_the_members_the_assets_own_read_reports() {
    let router = emulator();
    let form = given_asset_type(&router).await;
    // The rows differ along whether the asset carries a description, which is the one member
    // of a hit an asset may hold no value for.
    let staged = [
        Staged {
            id: "described-asset",
            name: "Described Asset",
            description: Some("Carries a description"),
            content: None,
        },
        Staged {
            id: "bare-asset",
            name: "Bare Asset",
            description: None,
            content: None,
        },
    ];
    for asset in &staged {
        put_asset(&router, &form, asset).await;
    }

    for asset in &staged {
        let case = asset.id;
        let body = search(
            &router,
            case,
            &json!({
                "FilterClause": {
                    "AttributeFilter": {
                        "Attribute": "Id",
                        "Operator": "equals",
                        "Value": { "StringValue": asset.id },
                    },
                },
            }),
        )
        .await;
        let items = body
            .get("Items")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case}: the answer must report Items, got {body}"));
        assert_eq!(
            items.len(),
            1,
            "case {case}: exactly one asset carries that identifier, got {body}"
        );
        let hit = &items[0];
        let read = ok(&router, "GetAsset", &json!({ "Identifier": asset.id })).await;
        assert_eq!(
            hit.get("Id"),
            read.get("Id"),
            "case {case}: the hit must report the asset's own identifier, got {hit} for {read}"
        );
        assert_eq!(
            hit.get("AssetName"),
            read.get("Name"),
            "case {case}: the hit must report the name the read reports, got {hit} for {read}"
        );
        assert_eq!(
            hit.get("AssetDescription"),
            read.get("Description"),
            "case {case}: the hit must report the description the read reports, got {hit} for \
             {read}"
        );
        assert_eq!(
            hit.get("AssetTypeId"),
            read.get("AssetTypeId"),
            "case {case}: the hit must report the asset type the read reports, got {hit} for \
             {read}"
        );
        assert_eq!(
            hit.get("UpdatedAt"),
            read.get("UpdatedAt"),
            "case {case}: the hit must report the instant the read reports, got {hit} for {read}"
        );
    }
}

// ---------------------------------------------------------------------------
// Filters
// ---------------------------------------------------------------------------

/// A `FilterClause` selects the assets whose values for the attribute it names satisfy its
/// comparison, and the two clause lists compose those comparisons.
///
/// The rows differ along which attribute is compared (identifier, name, description, asset
/// type, the glossary terms, the forms map), by which operator, under which spelling of the
/// attribute, and how the clauses nest.
#[tokio::test]
async fn a_filter_selects_the_assets_whose_attribute_values_satisfy_it() {
    let router = emulator();
    let form = given_asset_type(&router).await;
    let staged = [
        Staged {
            id: "alpha-asset",
            name: "Alpha",
            description: Some("The first one"),
            content: Some(r#"{"zone":"eu-west-1"}"#),
        },
        Staged {
            id: "mu-asset",
            name: "Mu",
            description: None,
            content: None,
        },
        Staged {
            id: "zeta-asset",
            name: "Zeta",
            description: Some("The last one"),
            content: None,
        },
    ];
    for asset in &staged {
        put_asset(&router, &form, asset).await;
    }
    // One asset carries a glossary term, so a filter on the list attribute has something to
    // select on.
    let glossary = ok(&router, "CreateGlossary", &json!({ "Name": "SearchGlossary" })).await;
    let glossary_id = glossary
        .get("Id")
        .and_then(Value::as_str)
        .expect("CreateGlossary must mint an Id")
        .to_string();
    let term = ok(
        &router,
        "CreateGlossaryTerm",
        &json!({ "GlossaryIdentifier": &glossary_id, "Name": "Confidential" }),
    )
    .await;
    let term_id = term
        .get("Id")
        .and_then(Value::as_str)
        .expect("CreateGlossaryTerm must mint an Id")
        .to_string();
    ok(
        &router,
        "AssociateGlossaryTerms",
        &json!({ "AssetIdentifier": "mu-asset", "GlossaryTermIdentifiers": [&term_id] }),
    )
    .await;

    // (case, filter clause, the identifiers that must be reported)
    let rows: Vec<(&str, Value, Vec<&str>)> = vec![
        (
            "equals on the identifier",
            json!({ "AttributeFilter": {
                "Attribute": "Id",
                "Operator": "equals",
                "Value": { "StringValue": "mu-asset" },
            }}),
            vec!["mu-asset"],
        ),
        (
            "equals on the name",
            json!({ "AttributeFilter": {
                "Attribute": "Name",
                "Operator": "equals",
                "Value": { "StringValue": "Zeta" },
            }}),
            vec!["zeta-asset"],
        ),
        (
            "equals on the name under the search's own spelling of it",
            json!({ "AttributeFilter": {
                "Attribute": "assetName",
                "Operator": "equals",
                "Value": { "StringValue": "Zeta" },
            }}),
            vec!["zeta-asset"],
        ),
        (
            "equals on the asset type every asset is an instance of",
            json!({ "AttributeFilter": {
                "Attribute": "AssetTypeId",
                "Operator": "equals",
                "Value": { "StringValue": ASSET_TYPE },
            }}),
            vec!["alpha-asset", "mu-asset", "zeta-asset"],
        ),
        (
            "greaterThan on the name",
            json!({ "AttributeFilter": {
                "Attribute": "Name",
                "Operator": "greaterThan",
                "Value": { "StringValue": "Mu" },
            }}),
            vec!["zeta-asset"],
        ),
        (
            "lessThanOrEquals on the name",
            json!({ "AttributeFilter": {
                "Attribute": "Name",
                "Operator": "lessThanOrEquals",
                "Value": { "StringValue": "Mu" },
            }}),
            vec!["alpha-asset", "mu-asset"],
        ),
        (
            "notExists on the description",
            json!({ "AttributeFilter": { "Attribute": "Description", "Operator": "notExists" }}),
            vec!["mu-asset"],
        ),
        (
            "notExists on a member every asset carries",
            json!({ "AttributeFilter": { "Attribute": "Name", "Operator": "notExists" }}),
            vec![],
        ),
        (
            "equals on one of the glossary terms the asset carries",
            json!({ "AttributeFilter": {
                "Attribute": "GlossaryTerms",
                "Operator": "equals",
                "Value": { "StringValue": &term_id },
            }}),
            vec!["mu-asset"],
        ),
        (
            "greaterThanOrEquals on an instant before every asset was stored",
            json!({ "AttributeFilter": {
                "Attribute": "CreatedAt",
                "Operator": "greaterThanOrEquals",
                "Value": { "StringValue": "1970-01-01T00:00:00Z" },
            }}),
            vec!["alpha-asset", "mu-asset", "zeta-asset"],
        ),
        (
            "lessThan the same instant, as epoch seconds",
            json!({ "AttributeFilter": {
                "Attribute": "UpdatedAt",
                "Operator": "lessThan",
                "Value": { "LongValue": 0 },
            }}),
            vec![],
        ),
        (
            "a map filter on the content of a form the asset holds",
            json!({ "MapFilter": {
                "Attribute": "Forms",
                "Key": "Details",
                "Value": { "StringValue": r#"{"zone":"eu-west-1"}"# },
            }}),
            vec!["alpha-asset"],
        ),
        (
            "a map filter naming a form name no asset holds",
            json!({ "MapFilter": {
                "Attribute": "Forms",
                "Key": "NoSuchForm",
                "Value": { "StringValue": "anything" },
            }}),
            vec![],
        ),
        (
            "every filter of a conjunction",
            json!({ "AndAllFilters": [
                { "AttributeFilter": {
                    "Attribute": "AssetTypeId",
                    "Operator": "equals",
                    "Value": { "StringValue": ASSET_TYPE },
                }},
                { "AttributeFilter": { "Attribute": "Description", "Operator": "notExists" }},
            ]}),
            vec!["mu-asset"],
        ),
        (
            "any filter of a disjunction",
            json!({ "OrAnyFilters": [
                { "AttributeFilter": {
                    "Attribute": "Id",
                    "Operator": "equals",
                    "Value": { "StringValue": "alpha-asset" },
                }},
                { "AttributeFilter": {
                    "Attribute": "Id",
                    "Operator": "equals",
                    "Value": { "StringValue": "zeta-asset" },
                }},
            ]}),
            vec!["alpha-asset", "zeta-asset"],
        ),
        (
            "a disjunction nested inside a conjunction",
            json!({ "AndAllFilters": [
                { "OrAnyFilters": [
                    { "AttributeFilter": {
                        "Attribute": "Name",
                        "Operator": "equals",
                        "Value": { "StringValue": "Alpha" },
                    }},
                    { "AttributeFilter": {
                        "Attribute": "Name",
                        "Operator": "equals",
                        "Value": { "StringValue": "Mu" },
                    }},
                ]},
                { "AttributeFilter": { "Attribute": "Description", "Operator": "notExists" }},
            ]}),
            vec!["mu-asset"],
        ),
    ];

    for (case, clause, expected) in rows {
        let body = search(&router, case, &json!({ "FilterClause": clause })).await;
        assert_eq!(
            hit_id_set(case, &body),
            expected,
            "case {case}: the filter must select {expected:?}, got {body}"
        );
    }

    // A text search and a filter NARROW each other: the hits are the assets that satisfy both.
    let case = "a filter beside a search text";
    let body = search(
        &router,
        case,
        &json!({
            "SearchText": "the",
            "FilterClause": { "AttributeFilter": {
                "Attribute": "Name",
                "Operator": "equals",
                "Value": { "StringValue": "Zeta" },
            }},
        }),
    )
    .await;
    assert_eq!(
        hit_id_set(case, &body),
        vec!["zeta-asset"],
        "case {case}: text and filter must both hold, got {body}"
    );
}

// ---------------------------------------------------------------------------
// Sorting
// ---------------------------------------------------------------------------

/// A `Sort` orders the hits by the attribute it names, ascending unless the request asks for
/// `DESCENDING`, with the assets carrying no value for that attribute last in the ascending
/// order.
///
/// The rows differ along which attribute orders the hits — text, identifier, instant, and one
/// an asset may carry no value for — and along the direction.
#[tokio::test]
async fn a_sort_orders_the_hits_by_the_attribute_it_names() {
    let router = emulator();
    let form = given_asset_type(&router).await;
    // Stored in identifier order, so an ordering by the instants they were stored at is the
    // same sequence whether or not two writes land on the same millisecond: the tie is broken
    // by the identifier.
    let staged = [
        Staged {
            id: "a-1",
            name: "Zeta",
            description: Some("last letter"),
            content: None,
        },
        Staged {
            id: "b-2",
            name: "Alpha",
            description: None,
            content: None,
        },
        Staged {
            id: "c-3",
            name: "Mu",
            description: Some("middle letter"),
            content: None,
        },
    ];
    for asset in &staged {
        put_asset(&router, &form, asset).await;
    }

    // (case, sort, the identifiers in the order they must be reported)
    let rows: Vec<(&str, Value, Vec<&str>)> = vec![
        (
            "by name, ascending by default",
            json!({ "Attribute": "Name" }),
            vec!["b-2", "c-3", "a-1"],
        ),
        (
            "by name, ascending",
            json!({ "Attribute": "Name", "Order": "ASCENDING" }),
            vec!["b-2", "c-3", "a-1"],
        ),
        (
            "by name, descending",
            json!({ "Attribute": "Name", "Order": "DESCENDING" }),
            vec!["a-1", "c-3", "b-2"],
        ),
        (
            "by identifier",
            json!({ "Attribute": "Id", "Order": "ASCENDING" }),
            vec!["a-1", "b-2", "c-3"],
        ),
        (
            "by the instant of the last write",
            json!({ "Attribute": "UpdatedAt", "Order": "ASCENDING" }),
            vec!["a-1", "b-2", "c-3"],
        ),
        (
            "by the instant of the last write, descending",
            json!({ "Attribute": "UpdatedAt", "Order": "DESCENDING" }),
            vec!["c-3", "b-2", "a-1"],
        ),
        (
            "by a description one asset carries none of, which sorts last",
            json!({ "Attribute": "Description", "Order": "ASCENDING" }),
            vec!["a-1", "c-3", "b-2"],
        ),
    ];

    for (case, sort, expected) in rows {
        let body = search(
            &router,
            case,
            &json!({ "SearchText": ASSET_TYPE, "Sort": sort }),
        )
        .await;
        assert_eq!(
            hit_ids(case, &body),
            expected,
            "case {case}: the hits must be ordered {expected:?}, got {body}"
        );
    }
}

// ---------------------------------------------------------------------------
// Paging
// ---------------------------------------------------------------------------

/// Paging walks every hit exactly once, in the search's own order, however the page size
/// divides them, and the last page carries no `NextToken`.
///
/// The rows differ along the page size relative to the number of hits — one at a time, a
/// divisor of it, a size that leaves a short last page, exactly the number of hits, and more
/// than there are.
#[tokio::test]
async fn paging_walks_every_hit_once_whatever_the_page_size() {
    let router = emulator();
    let form = given_asset_type(&router).await;
    for index in 0..7 {
        put_asset(
            &router,
            &form,
            &Staged {
                id: &format!("paged-{index}"),
                name: &format!("Paged Asset {index}"),
                description: None,
                content: None,
            },
        )
        .await;
    }

    // The whole answer, in one page, is the order every paged walk below has to reproduce.
    let whole = search(
        &router,
        "one page",
        &json!({ "SearchText": ASSET_TYPE, "Sort": { "Attribute": "Id" } }),
    )
    .await;
    let expected = hit_ids("one page", &whole);
    assert_eq!(
        expected.len(),
        7,
        "the search must report every staged asset, got {whole}"
    );
    assert!(
        whole.get("NextToken").is_none(),
        "a page holding every hit must carry no NextToken, got {whole}"
    );

    for page_size in [1, 2, 3, 7, 100] {
        let case = format!("pages of {page_size}");
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        // One more iteration than the hits, so a token that never cleared is caught rather
        // than looped on.
        for _ in 0..=expected.len() {
            let mut request = json!({
                "SearchText": ASSET_TYPE,
                "Sort": { "Attribute": "Id" },
                "MaxResults": page_size,
            });
            if let Some(token) = token.as_deref() {
                request["NextToken"] = json!(token);
            }
            let body = search(&router, &case, &request).await;
            let page = hit_ids(&case, &body);
            assert!(
                page.len() <= page_size,
                "case {case}: a page must hold no more than {page_size} hits, got {body}"
            );
            walked.extend(page);
            token = body
                .get("NextToken")
                .and_then(Value::as_str)
                .map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            token, None,
            "case {case}: the walk must end with no continuation token left"
        );
        assert_eq!(
            walked, expected,
            "case {case}: paging must report every hit once, in the search's order"
        );
    }
}

/// A `NextToken` is opaque and bound to the ordering it was minted for: a value this store
/// did not mint for that ordering names no position and is refused.
///
/// The rows differ along how the token misses: a value a caller assembled, a truncated minted
/// token, and a token minted for a DIFFERENT ordering of the same assets.
#[tokio::test]
async fn a_continuation_token_this_search_did_not_mint_is_refused() {
    let router = emulator();
    let form = given_asset_type(&router).await;
    for index in 0..3 {
        put_asset(
            &router,
            &form,
            &Staged {
                id: &format!("token-{index}"),
                name: &format!("Token Asset {index}"),
                description: None,
                content: None,
            },
        )
        .await;
    }

    // A token this store DID mint, for the identifier-ordered search.
    let sorted = search(
        &router,
        "a minted token",
        &json!({
            "SearchText": ASSET_TYPE,
            "Sort": { "Attribute": "Id" },
            "MaxResults": 1,
        }),
    )
    .await;
    let minted = sorted
        .get("NextToken")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("a first page of three hits must carry a NextToken, got {sorted}"))
        .to_string();
    // It resumes its OWN search, which is what makes the refusals below about the token and
    // not about paging.
    let resumed = search(
        &router,
        "the minted token on its own search",
        &json!({
            "SearchText": ASSET_TYPE,
            "Sort": { "Attribute": "Id" },
            "MaxResults": 1,
            "NextToken": &minted,
        }),
    )
    .await;
    assert_eq!(
        hit_ids("the minted token on its own search", &resumed),
        vec!["token-1".to_string()],
        "a minted token must resume the search that minted it, got {resumed}"
    );

    let rows: Vec<(&str, Value)> = vec![
        (
            "a value a caller assembled",
            json!({ "SearchText": ASSET_TYPE, "NextToken": "token-0" }),
        ),
        (
            "a minted token with its last byte dropped",
            json!({
                "SearchText": ASSET_TYPE,
                "Sort": { "Attribute": "Id" },
                "NextToken": minted[..minted.len() - 2].to_string(),
            }),
        ),
        (
            "a token minted for another ordering",
            json!({ "SearchText": ASSET_TYPE, "NextToken": &minted }),
        ),
        (
            "a token minted for the other direction of the same attribute",
            json!({
                "SearchText": ASSET_TYPE,
                "Sort": { "Attribute": "Id", "Order": "DESCENDING" },
                "NextToken": &minted,
            }),
        ),
    ];
    for (case, request) in rows {
        assert_refused(&router, case, &request, "Invalid pagination token.").await;
    }
}

// ---------------------------------------------------------------------------
// The search follows the store
// ---------------------------------------------------------------------------

/// A search reports what the store holds NOW: an asset is found as soon as a put stored it,
/// reported with the members that put stored, and is no longer found once a delete removed it
/// — whichever way the request selects it.
///
/// The rows differ along how the search names the asset: by its text, by a filter on its
/// identifier, by a filter on its name, and by a map filter on a form it holds.
#[tokio::test]
async fn a_search_finds_an_asset_until_it_is_deleted() {
    let router = emulator();
    let form = given_asset_type(&router).await;
    let asset = Staged {
        id: "lifecycle-asset",
        name: "Lifecycle Asset",
        description: Some("Stored, found, then deleted"),
        content: Some(r#"{"zone":"us-east-1"}"#),
    };

    let rows: Vec<(&str, Value)> = vec![
        ("by a word of its name", json!({ "SearchText": "Lifecycle" })),
        (
            "by a filter on its identifier",
            json!({ "FilterClause": { "AttributeFilter": {
                "Attribute": "Id",
                "Operator": "equals",
                "Value": { "StringValue": "lifecycle-asset" },
            }}}),
        ),
        (
            "by a filter on its name",
            json!({ "FilterClause": { "AttributeFilter": {
                "Attribute": "AssetName",
                "Operator": "equals",
                "Value": { "StringValue": "Lifecycle Asset" },
            }}}),
        ),
        (
            "by a map filter on the form it holds",
            json!({ "FilterClause": { "MapFilter": {
                "Attribute": "Forms",
                "Key": "Details",
                "Value": { "StringValue": r#"{"zone":"us-east-1"}"# },
            }}}),
        ),
    ];

    for (case, request) in rows {
        // Nothing is stored yet, so the search that will find the asset finds nothing first.
        let before = search(&router, case, &request).await;
        assert_eq!(
            hit_ids(case, &before),
            Vec::<String>::new(),
            "case {case}: no asset is stored, so the search must report none, got {before}"
        );

        put_asset(&router, &form, &asset).await;
        let found = search(&router, case, &request).await;
        assert_eq!(
            hit_ids(case, &found),
            vec![asset.id.to_string()],
            "case {case}: the stored asset must be found, got {found}"
        );
        // The asset the search reports is one the read resolves.
        let read = ok(&router, "GetAsset", &json!({ "Identifier": asset.id })).await;
        assert_eq!(
            read.get("Id").and_then(Value::as_str),
            Some(asset.id),
            "case {case}: a hit must be an asset the read resolves, got {read}"
        );

        ok(&router, "DeleteAsset", &json!({ "Identifier": asset.id })).await;
        let after = search(&router, case, &request).await;
        assert_eq!(
            hit_ids(case, &after),
            Vec::<String>::new(),
            "case {case}: the deleted asset must no longer be found, got {after}"
        );
    }
}

/// A search changes nothing: the same request repeated reports the same hits, and the asset a
/// search reported reads back exactly as it did before.
#[tokio::test]
async fn a_search_changes_nothing_a_later_caller_sees() {
    let router = emulator();
    let form = given_asset_type(&router).await;
    for asset in text_corpus() {
        put_asset(&router, &form, &asset).await;
    }
    let before: Vec<Value> = {
        let mut reads = Vec::new();
        for asset in text_corpus() {
            reads.push(ok(&router, "GetAsset", &json!({ "Identifier": asset.id })).await);
        }
        reads
    };

    let requests: Vec<(&str, Value)> = vec![
        ("a text search", json!({ "SearchText": "orders" })),
        (
            "a filtered search",
            json!({ "FilterClause": { "AttributeFilter": {
                "Attribute": "AssetTypeId",
                "Operator": "equals",
                "Value": { "StringValue": ASSET_TYPE },
            }}}),
        ),
        (
            "a sorted, paged search",
            json!({
                "SearchText": ASSET_TYPE,
                "Sort": { "Attribute": "Name", "Order": "DESCENDING" },
                "MaxResults": 2,
            }),
        ),
    ];
    for (case, request) in &requests {
        let first = search(&router, case, request).await;
        let again = search(&router, case, request).await;
        assert_eq!(
            first, again,
            "case {case}: the same search must answer the same way, got {first} then {again}"
        );
    }

    for (asset, before) in text_corpus().iter().zip(before.iter()) {
        let after = ok(&router, "GetAsset", &json!({ "Identifier": asset.id })).await;
        assert_eq!(
            &after, before,
            "case {}: a search must leave the asset exactly as it was",
            asset.id
        );
    }
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// A request this search cannot evaluate is REFUSED with the operation's own
/// `InvalidInputException`, rather than answered with an empty page the service never
/// searched for.
///
/// The rows differ along what the request got wrong: it names no search at all, it names an
/// attribute no asset carries, it compares a map attribute as a single value or a single value
/// as a map, it compares text to a number or a timestamp to text that is not one, and it names
/// an operator without the value that operator compares.
#[tokio::test]
async fn a_request_this_search_cannot_evaluate_is_refused() {
    let router = emulator();
    let form = given_asset_type(&router).await;
    put_asset(
        &router,
        &form,
        &Staged {
            id: "refusal-asset",
            name: "Refusal Asset",
            description: None,
            content: None,
        },
    )
    .await;

    let rows: Vec<(&str, Value, String)> = vec![
        (
            "neither text nor filter",
            json!({ "MaxResults": 10 }),
            "At least one of searchText or filterClause must be provided.".to_string(),
        ),
        (
            "a filter on an attribute no asset carries",
            json!({ "FilterClause": { "AttributeFilter": {
                "Attribute": "Owner",
                "Operator": "equals",
                "Value": { "StringValue": "finance" },
            }}}),
            "Attribute Owner is not a searchable attribute of an asset".to_string(),
        ),
        (
            "a sort by an attribute no asset carries",
            json!({
                "SearchText": ASSET_TYPE,
                "Sort": { "Attribute": "relevance" },
            }),
            "Attribute relevance is not a searchable attribute of an asset".to_string(),
        ),
        (
            "a map filter on an attribute holding a single value",
            json!({ "FilterClause": { "MapFilter": {
                "Attribute": "Name",
                "Key": "Details",
                "Value": { "StringValue": "Refusal Asset" },
            }}}),
            "Attribute Name holds a single value, so it is compared by an AttributeFilter and \
             not by a MapFilter"
                .to_string(),
        ),
        (
            "an attribute filter on an attribute holding a map",
            json!({ "FilterClause": { "AttributeFilter": {
                "Attribute": "Forms",
                "Operator": "equals",
                "Value": { "StringValue": "Details" },
            }}}),
            "Attribute Forms holds a map of values, so it is compared by a MapFilter naming \
             the key to compare and not by an AttributeFilter"
                .to_string(),
        ),
        (
            "a sort by an attribute holding a map",
            json!({ "SearchText": ASSET_TYPE, "Sort": { "Attribute": "Attachments" } }),
            "Attribute Attachments holds a map of values, so the results cannot be sorted by it"
                .to_string(),
        ),
        (
            "a number compared against an attribute holding text",
            json!({ "FilterClause": { "AttributeFilter": {
                "Attribute": "Name",
                "Operator": "equals",
                "Value": { "LongValue": 7 },
            }}}),
            "Value is a LongValue, which the attribute Name is not compared against: it holds \
             text"
                .to_string(),
        ),
        (
            "text that is no timestamp compared against an instant",
            json!({ "FilterClause": { "AttributeFilter": {
                "Attribute": "UpdatedAt",
                "Operator": "lessThan",
                "Value": { "StringValue": "yesterday" },
            }}}),
            "Value yesterday is not a timestamp, which the attribute UpdatedAt is compared \
             against"
                .to_string(),
        ),
        (
            "a comparison with no value to compare",
            json!({ "FilterClause": { "AttributeFilter": {
                "Attribute": "Name",
                "Operator": "equals",
            }}}),
            "Value is required on an AttributeFilter whose Operator is equals".to_string(),
        ),
    ];

    for (case, request, expected) in rows {
        assert_refused(&router, case, &request, &expected).await;
    }
}
