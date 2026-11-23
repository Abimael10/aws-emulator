// SPDX-License-Identifier: CC0-1.0

//! `ListGlossaries`: the business glossaries this account holds.
//!
//! RULE: the operation "lists business glossaries in Glue Data Catalog", so it reports
//! every glossary record this account holds — the records `CreateGlossary` wrote,
//! `UpdateGlossary` changed and `GetGlossary` reads back — projected to the three members
//! `GlossaryItem` declares, one `MaxResults`-sized page at a time in glossary-name order
//! with a `NextToken` that resumes the walk, and nothing the store no longer holds.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! The rows differ along what the rule generalises over: how many glossaries are stored,
//! the page size asked for (including the published bounds 1 and 1000, a size smaller than
//! the stored count and one past it), which members each stored glossary carries, and
//! whether a record was created, renamed or deleted between two listings.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::ComAmazonawsGlueAwsglueState;
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, emulator, emulator_on, ok};

/// Register one glossary with the members the row asks for, and return the `GlossaryId`
/// the create minted.
async fn given_glossary(router: &MultiServiceRouter, members: &Value) -> String {
    let created = ok(router, "CreateGlossary", members).await;
    created
        .get("Id")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("CreateGlossary must mint an Id, got {created}"))
        .to_string()
}

/// The entries of one answer, as a list.
fn items(page: &Value) -> Vec<Value> {
    page["Items"]
        .as_array()
        .unwrap_or_else(|| panic!("Items must be a list, got {page}"))
        .clone()
}

/// The `Name` of every entry in one answer, in the order the service returned them.
fn listed_names(page: &Value) -> Vec<String> {
    items(page)
        .iter()
        .map(|item| {
            item["Name"]
                .as_str()
                .unwrap_or_else(|| panic!("GlossaryItem.Name must be a string, got {item}"))
                .to_string()
        })
        .collect()
}

/// Walk every page the service hands out, following its own `NextToken`, and return the
/// entries in the order they were reported.
async fn walk(
    router: &MultiServiceRouter,
    case: &str,
    page_size: Option<i32>,
    limit: usize,
) -> Vec<Value> {
    let mut listed: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut request = json!({});
        if let Some(size) = page_size {
            request["MaxResults"] = json!(size);
        }
        if let Some(cursor) = &token {
            request["NextToken"] = json!(cursor);
        }
        let page = ok(router, "ListGlossaries", &request).await;
        let entries = items(&page);
        if let Some(size) = page_size {
            assert!(
                entries.len() <= usize::try_from(size).unwrap_or(usize::MAX),
                "case {case:?}: a page must not exceed MaxResults {size}, got {entries:?}"
            );
        }
        listed.extend(entries);
        token = page
            .get("NextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        if token.is_none() {
            break;
        }
        assert!(
            listed.len() <= limit,
            "case {case:?}: the walk must terminate, got {listed:?} for {limit} glossaries"
        );
    }
    listed
}

#[tokio::test]
async fn every_stored_glossary_is_listed_and_paged() {
    // (case, how many glossaries are stored, the page size asked for)
    let rows: &[(&str, usize, Option<i32>)] = &[
        ("no glossaries at all", 0, None),
        ("one glossary, no page size", 1, None),
        // The published lower bound of `MaxResults` (`PageSize` range 1..1000).
        ("the smallest page the model accepts", 5, Some(1)),
        ("a page smaller than the stored count", 7, Some(3)),
        ("a page exactly the stored count", 4, Some(4)),
        ("a page larger than the stored count", 3, Some(50)),
        // The published upper bound.
        ("the largest page the model accepts", 6, Some(1_000)),
    ];

    for (case, stored, page_size) in rows {
        let router = emulator();
        // Created out of name order, so the listing's own order is what is asserted.
        let expected: Vec<String> = (0..*stored)
            .map(|index| format!("glossary-{index:02}"))
            .collect();
        for name in expected.iter().rev() {
            given_glossary(&router, &json!({ "Name": name })).await;
        }

        let listed = walk(&router, case, *page_size, *stored).await;
        let names: Vec<String> = listed
            .iter()
            .map(|item| item["Name"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            names, expected,
            "case {case:?}: expected every stored glossary in name order, got {names:?}"
        );

        // A glossary the listing reports is one `GetGlossary` resolves, with the same
        // record: one record, two reads.
        for item in &listed {
            let id = item["Id"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: the entry must carry an Id, got {item}"));
            let read = ok(&router, "GetGlossary", &json!({ "Identifier": id })).await;
            assert_eq!(
                read.get("Name"),
                item.get("Name"),
                "case {case:?}: GetGlossary({id}) must report the listed glossary, got {read} for \
                 {item}"
            );
            assert_eq!(
                read.get("Description"),
                item.get("Description"),
                "case {case:?}: GetGlossary({id}) must report the listed description, got {read} \
                 for {item}"
            );
        }
    }
}

#[tokio::test]
async fn a_listed_glossary_carries_the_summary_members_the_create_stored() {
    let router = emulator();

    // The rows differ along which members the stored glossary carries: the required name
    // alone, a description, and the shortest and longest values the model accepts for a
    // name (`GlossaryName`, len 1..256) and a description (`MetadataDescription`, len
    // 1..2048).
    let rows: &[(&str, Value)] = &[
        ("the required member only", json!({ "Name": "aaa-bare" })),
        (
            "a description too",
            json!({ "Name": "bbb-described", "Description": "Terms the finance team owns" }),
        ),
        (
            "a one-character name and description",
            json!({ "Name": "c", "Description": "d" }),
        ),
        (
            "values at the modelled length bounds",
            json!({
                "Name": format!("ddd{}", "n".repeat(253)),
                "Description": "d".repeat(2048),
            }),
        ),
    ];

    for (_, members) in rows {
        given_glossary(&router, members).await;
    }

    let listed = walk(&router, "summary members", None, rows.len()).await;
    assert_eq!(
        listed.len(),
        rows.len(),
        "case summary members: expected {} glossaries, got {listed:?}",
        rows.len()
    );

    // The rows were created in name order, which is the order the listing reports.
    for ((case, members), item) in rows.iter().zip(listed.iter()) {
        assert_eq!(
            item.get("Name"),
            members.get("Name"),
            "case {case:?}: expected the stored name, got {item}"
        );
        assert_eq!(
            item.get("Description"),
            members.get("Description"),
            "case {case:?}: expected the stored description, got {item}"
        );
        // `GlossaryItem` is a summary of three members (`smithy_query shape=GlossaryItem`):
        // the two instants a glossary carries are reported by `GetGlossary` and not here.
        for instant in ["CreatedAt", "UpdatedAt"] {
            assert!(
                item.get(instant).is_none(),
                "case {case:?}: a listed glossary must report no {instant}, got {item}"
            );
        }
        let id = item["Id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the entry must carry an Id, got {item}"));
        let read = ok(&router, "GetGlossary", &json!({ "Identifier": id })).await;
        assert!(
            read.get("CreatedAt").is_some(),
            "case {case:?}: GetGlossary must report the instants, got {read}"
        );
    }
}

#[tokio::test]
async fn the_listing_reflects_what_the_store_holds_now() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());

    // Nothing stored yet: the listing is empty rather than a refusal.
    let empty = ok(&router, "ListGlossaries", &json!({})).await;
    assert_eq!(
        listed_names(&empty),
        Vec::<String>::new(),
        "case nothing stored: expected an empty listing, got {empty}"
    );

    let mut ids = Vec::new();
    for name in ["kept-one", "kept-two", "removed"] {
        ids.push(given_glossary(&router, &json!({ "Name": name })).await);
    }

    let before = listed_names(&ok(&router, "ListGlossaries", &json!({})).await);
    assert_eq!(
        before,
        vec![
            "kept-one".to_string(),
            "kept-two".to_string(),
            "removed".to_string()
        ],
        "case after the creates: expected every stored glossary, got {before:?}"
    );

    // Listing is reporting only: two listings in a row agree, so a read changed nothing a
    // later caller sees.
    let again = listed_names(&ok(&router, "ListGlossaries", &json!({})).await);
    assert_eq!(
        again, before,
        "case listed twice: a listing must change nothing, got {again:?} then {before:?}"
    );

    // A rename moves the record within the listing, because the update changed the member
    // the listing reports and orders by.
    ok(
        &router,
        "UpdateGlossary",
        &json!({
            "Identifier": ids.first().expect("three glossaries were created"),
            "Name": "zzz-renamed",
        }),
    )
    .await;
    let renamed = listed_names(&ok(&router, "ListGlossaries", &json!({})).await);
    assert_eq!(
        renamed,
        vec![
            "kept-two".to_string(),
            "removed".to_string(),
            "zzz-renamed".to_string()
        ],
        "case after the rename: expected the updated name, got {renamed:?}"
    );

    // A glossary the store no longer holds is not listed. The record is dropped through
    // `DeleteGlossary`, the operation that owns that removal.
    {
        let removed = ids.last().expect("three glossaries were created").clone();
        ok(&router, "DeleteGlossary", &json!({ "Identifier": &removed })).await;
        assert!(
            !engine
                .state::<ComAmazonawsGlueAwsglueState>()
                .lock()
                .glossaries
                .contains_key(&removed),
            "case after the delete: the store must no longer hold {removed}"
        );
    }

    let after = listed_names(&ok(&router, "ListGlossaries", &json!({})).await);
    assert_eq!(
        after,
        vec!["kept-two".to_string(), "zzz-renamed".to_string()],
        "case after the record is dropped: a glossary the store no longer holds must not be \
         listed, got {after:?}"
    );
}

#[tokio::test]
async fn a_page_resumes_at_the_entry_after_the_token() {
    let router = emulator();
    let names = ["alpha", "bravo", "charlie", "delta"];
    for name in names {
        given_glossary(&router, &json!({ "Name": name })).await;
    }

    // (case, the page size, the names of the first page, the token it must carry)
    let rows: &[(&str, i32, &[&str], Option<&str>)] = &[
        ("one at a time", 1, &["alpha"], Some("alpha")),
        ("two at a time", 2, &["alpha", "bravo"], Some("bravo")),
        (
            "three at a time",
            3,
            &["alpha", "bravo", "charlie"],
            Some("charlie"),
        ),
        (
            "every glossary in one page",
            4,
            &["alpha", "bravo", "charlie", "delta"],
            None,
        ),
    ];

    for (case, page_size, first, token) in rows {
        let page = ok(
            &router,
            "ListGlossaries",
            &json!({ "MaxResults": page_size }),
        )
        .await;
        assert_eq!(
            listed_names(&page),
            first
                .iter()
                .map(|name| (*name).to_string())
                .collect::<Vec<_>>(),
            "case {case:?}: expected the first {page_size} glossaries in name order, got {page}"
        );
        assert_eq!(
            page.get("NextToken").and_then(Value::as_str),
            *token,
            "case {case:?}: expected the token {token:?}, got {page}"
        );

        // Continuing from the token reports the REST of the glossaries, each exactly once.
        if let Some(cursor) = token {
            let next = ok(&router, "ListGlossaries", &json!({ "NextToken": cursor })).await;
            let rest: Vec<String> = names
                .iter()
                .filter(|name| !first.contains(name))
                .map(|name| (*name).to_string())
                .collect();
            assert_eq!(
                listed_names(&next),
                rest,
                "case {case:?}: expected the glossaries after {cursor}, got {next}"
            );
        }
    }
}

#[tokio::test]
async fn a_page_size_outside_the_published_range_is_refused() {
    let router = emulator();
    given_glossary(&router, &json!({ "Name": "bounded" })).await;

    // `MaxResults` is `PageSize`, range 1..1000: one past each published bound is refused
    // by the modelled constraint rather than served as some other page size.
    for (case, max_results) in [("below the minimum", 0), ("above the maximum", 1_001)] {
        let (status, body) = call(
            &router,
            "ListGlossaries",
            &json!({ "MaxResults": max_results }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: MaxResults={max_results} must be refused, got {status} {body}"
        );
    }

    // Each published bound itself is served.
    for (case, max_results) in [("the minimum", 1), ("the maximum", 1_000)] {
        let page = ok(
            &router,
            "ListGlossaries",
            &json!({ "MaxResults": max_results }),
        )
        .await;
        assert_eq!(
            listed_names(&page),
            vec!["bounded".to_string()],
            "case {case:?}: MaxResults={max_results} must be served, got {page}"
        );
    }
}
