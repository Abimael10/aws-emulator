// SPDX-License-Identifier: CC0-1.0

//! `ListTypes` as a REPORTING call, end to end over HTTP: what it answers is cut from the
//! types the addressed API holds at the instant it is asked, and asking changes nothing a
//! later caller sees.
//!
//! The rules asserted here are stated over CLASSES of input rather than over the one
//! request that filed the issue, and each one is a dimension the crate's other type-listing
//! suites do not cover:
//!
//! 1. the page size a type listing accepts is the one `MaxResults` publishes: a value
//!    outside that range is refused with the operation's own client error — AppSync's
//!    `BadRequestException` at HTTP 400 — carrying the sentence the service states that
//!    member's range in, and every value inside it, each published bound included, is
//!    served. Asserted on BOTH operations that carry the member for a type listing
//!    (`ListTypes` and `ListTypesByAssociation`), which is where the rule holds and where
//!    `range_refusal_wording`'s table of page-size carriers stops;
//! 2. a listing reports the types the API HOLDS, so it never reports a type a schema
//!    creation the clock still owes has not installed: a listing that does report one is
//!    answering an API whose creation the clock has performed, and an empty listing carries
//!    no cursor;
//! 3. once the clock has performed the creation, the listing reports exactly the type system
//!    the document declared, each type carrying the body a single `GetType` answers with —
//!    so a type that arrived inside a schema document is listed on the same terms as one
//!    `CreateType` put there (which is what `type_listing` pins over the whole page-size and
//!    format matrix).
//!
//! The rows differ along what the rules generalise over: the operation carrying the page
//! size, which bound the value crosses and how far past it, how many types a document
//! declares (one, a few, and more than a page holds), the page size the listing is cut at
//! and the format the types are named in.
//!
//! The harness is the crate's shared one — the router, the API and type calls, the schema
//! calls with their poll-until wait, the listing walker and the association helper, and the
//! published bounds and refusal wording — reused rather than copied, so every assertion
//! goes through the router the production emulator mounts.

use serde_json::Value;
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;
use smithy_server_runtime::MultiServiceRouter;

use super::graphql_api_lifecycle::router;
use super::graphql_types::{call, create_api, get_type, Answer, FORMATS};
use super::range_refusal_wording::{
    page_size_sentence, BAD_REQUEST, BAD_REQUEST_STATUS, MAX_RESULTS_MAX, MAX_RESULTS_MIN,
};
use super::schema_creation::{await_creation, creation_status, start_creation};
use super::source_api_association::create_pair;
use super::type_listing::{associate, listed, list_types, walk};

/// One document under test: how it names itself in an assertion, the SDL it carries and
/// the types it declares.
struct Document {
    label: &'static str,
    definition: String,
    declared: Vec<String>,
}

/// A document declaring `count` object types, each holding one field of a built-in scalar
/// so it resolves on its own.
fn document(label: &'static str, count: usize) -> Document {
    let declared: Vec<String> = (0..count).map(|index| format!("Declared{index}")).collect();
    // `Query` is declared by every document, because a schema's query root is what makes
    // the type system one an API can hold.
    let mut definition = String::from("type Query {\n  held: String\n}\n");
    for name in &declared {
        definition.push_str(&format!("\ntype {name} {{\n  id: ID!\n}}\n"));
    }
    let mut all = vec!["Query".to_string()];
    all.extend(declared);
    Document {
        label,
        definition,
        declared: all,
    }
}

/// The documents rules 2 and 3 are asserted over: one type beside the query root, a few,
/// and more than the smallest page holds.
fn documents() -> Vec<Document> {
    vec![
        document("a document declaring one type", 1),
        document("a document declaring a few types", 3),
        document("a document declaring more types than a page holds", 6),
    ]
}

/// The names a collected listing reports, in the order it reported them.
fn names(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .map(|item| item["name"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// One type listing at `base` with the page size the case selects.
async fn list_at(router: &MultiServiceRouter, base: &str, max_results: i64) -> Answer {
    call(
        router,
        "GET",
        &format!(
            "{base}?format={}&maxResults={max_results}",
            state::SDL_FORMAT
        ),
        None,
    )
    .await
}

/// RULE 1: the page size a type listing accepts is the one the model publishes, and a value
/// outside it takes the service's sentence for that member.
#[tokio::test]
async fn a_type_listings_page_size_is_the_published_range() {
    let router = router();
    let api_id = create_api(&router, "list-types-page-size").await;
    let (source, merged) = create_pair(&router, "list-types-page-size-pair").await;
    let association_id = associate(&router, &source.id, &merged.id).await;
    let expected = page_size_sentence();

    // Both operations that cut a type listing into pages, as the path that would page it.
    let carriers = [
        ("ListTypes", format!("/v1/apis/{api_id}/types")),
        (
            "ListTypesByAssociation",
            format!(
                "/v1/mergedApis/{}/sourceApiAssociations/{association_id}/types",
                merged.id
            ),
        ),
    ];

    for (operation, base) in &carriers {
        // One below the minimum, one past the maximum, and values far past either bound.
        for value in [MAX_RESULTS_MIN - 1, MAX_RESULTS_MAX + 1, -99, 1_000] {
            let case = format!("{operation} with maxResults {value}");
            let answer = list_at(&router, base, value).await;
            assert_eq!(
                (answer.status, answer.error_type.as_str()),
                (BAD_REQUEST_STATUS, BAD_REQUEST),
                "case {case}: expected {BAD_REQUEST_STATUS} {BAD_REQUEST}, got {answer:?}"
            );
            assert_eq!(
                answer.body["message"].as_str().unwrap_or_default(),
                expected,
                "case {case}: expected {expected:?}, got {answer:?}"
            );
            assert!(
                answer.body["types"].is_null(),
                "case {case}: expected no types on a refusal, got {answer:?}"
            );
        }

        // Each published bound, and values between them, are served.
        for value in [MAX_RESULTS_MIN, MAX_RESULTS_MAX, 1, 7] {
            let case = format!("{operation} with maxResults {value}");
            let answer = list_at(&router, base, value).await;
            assert_eq!(
                answer.status, 200,
                "case {case}: expected the value inside the range to be served, got {answer:?}"
            );
        }
    }
}

/// RULE 2: a listing never reports a type a schema creation the clock still owes has not
/// installed — and RULE 3: once the clock has performed it, the listing reports exactly the
/// type system the document declared, each type carrying what a single read answers.
#[tokio::test]
async fn a_listing_reports_the_type_system_the_clock_has_installed() {
    let router = router();
    let documents = documents();
    let mut apis: Vec<String> = Vec::new();

    // Every creation is scheduled first, each on its own API, so the ONE wait further down
    // crosses the due instant of all of them: the transition is owed by the clock rather
    // than by a call.
    for document in &documents {
        let api_id = create_api(&router, document.label).await;
        let started = start_creation(&router, &api_id, document.definition.as_bytes()).await;
        assert_eq!(
            started.status, 200,
            "case {}: expected StartSchemaCreation 200, got {} {:?}",
            document.label, started.status, started.body
        );

        // A listing taken while the creation may still be owed reports only types the API
        // already holds: a listing that reports one of the document's types is answering an
        // API whose creation the clock has PERFORMED, which the status read right after it
        // has to agree with.
        let pending = list_types(&router, &api_id, state::SDL_FORMAT, None, None).await;
        assert_eq!(
            pending.status, 200,
            "case {}: expected ListTypes 200 while the creation is owed, got {pending:?}",
            document.label
        );
        let reported = names(&listed(&pending));
        let settled = creation_status(&router, &api_id).await;
        let status = settled.body["status"].as_str().unwrap_or_default();
        if reported.is_empty() {
            assert!(
                pending.body["nextToken"].is_null(),
                "case {}: expected no cursor on an empty listing, got {pending:?}",
                document.label
            );
            // An API whose creation is owed or performed holds a creation in one of those
            // two states, so an empty listing is never the answer of an API that never had
            // one or whose document was refused.
            assert!(
                status == state::SCHEMA_PROCESSING || status == state::SCHEMA_SUCCESS,
                "case {}: expected the creation to be owed or performed, got the status \
                 {status:?}",
                document.label
            );
        } else {
            assert_eq!(
                status,
                state::SCHEMA_SUCCESS,
                "case {}: expected a listing reporting {reported:?} to answer a creation the \
                 clock has performed, got the status {status:?}",
                document.label
            );
        }
        for name in &reported {
            assert!(
                document.declared.contains(name),
                "case {}: expected only the document's types listed, got {reported:?}",
                document.label
            );
        }

        apis.push(api_id);
    }

    for (document, api_id) in documents.iter().zip(apis.iter()) {
        await_creation(&router, api_id, document.label).await;

        let held = document.declared.len();
        let mut expected = document.declared.clone();
        expected.sort();

        // One type per page, which pages a type system that arrived inside a document, and
        // the unspecified page size in each format (`maxResults` defaults to 0, i.e.
        // "unspecified"). The whole page-size and format matrix is `type_listing`'s.
        let cuts: Vec<(&str, Option<usize>)> = FORMATS
            .iter()
            .map(|format| (*format, None))
            .chain(std::iter::once((state::SDL_FORMAT, Some(1))))
            .collect();
        let mut order: Option<Vec<String>> = None;
        for (format, size) in cuts {
            let case = format!("{} in {format} at page size {size:?}", document.label);
            let collected = walk(&router, api_id, format, size, held, &case).await;
            let reported = names(&collected);

            let mut sorted = reported.clone();
            sorted.sort();
            assert_eq!(
                sorted, expected,
                "case {case}: expected the document's type system listed, got {reported:?}"
            );

            // The order a caller pages over is the API's own, so it does not depend on the
            // cut the request makes or on the format it asks for.
            match &order {
                None => order = Some(reported.clone()),
                Some(first) => assert_eq!(
                    &reported, first,
                    "case {case}: expected the listing order {first:?}, got {reported:?}"
                ),
            }

            // A type read on its own and the same type read in a listing carry one body.
            for item in &collected {
                let name = item["name"].as_str().unwrap_or_default();
                let read = get_type(&router, api_id, name, format).await;
                assert_eq!(
                    read.status, 200,
                    "case {case}: expected GetType 200 for {name:?}, got {read:?}"
                );
                assert_eq!(
                    &read.body["type"], item,
                    "case {case}: expected the listing to report what GetType reports for \
                     {name:?}, got {item:?}"
                );
            }
        }
    }
}
