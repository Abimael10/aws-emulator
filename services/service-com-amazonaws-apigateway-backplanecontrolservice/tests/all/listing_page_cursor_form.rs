// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the `position` CURSOR this control plane's paginated listings hand
//! back, over a span of its collections.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. [`Gateway`], `create_api`, `page_items`,
//! `string_member` and `assert_failure` are this crate's one wire-test harness, reused
//! rather than standing a second emulator up another way.
//!
//! The RULE under test, over a SET of collections rather than one recorded call: a listing's
//! `position` is the cursor the collection being walked is ordered by, in the form the
//! SERVICE issues it.
//!
//! * A collection ordered by the identifiers this service MINTED for its records hands back
//!   a continuation token of its own: it names the position the next page resumes at, spells
//!   no record of the collection, and no value a caller assembled — a record's own id among
//!   them — resumes a walk, because an identifier the service assigned is not a cursor. A
//!   token is bound to the listing that issued it, so neither another operation's listing nor
//!   the same operation's listing of ANOTHER parent's collection resumes on it. The rows span
//!   the account's api keys, its rest apis and its client certificates, and the resources of
//!   one api, so the rule is asserted on an account-level collection and on a collection held
//!   inside a parent alike.
//! * A collection ordered by an ADDRESS the caller composed — a model's name — hands that
//!   address back, which is what the caller already holds and what every other operation of
//!   that collection addresses the element by.
//!
//! What each collection is cursored by is a property of the collection, so each row states
//! it; the walk, the refusals and the stability of a cursor are asserted the same way for
//! every row.

use serde_json::{json, Value};

use crate::rest_api_lifecycle::{
    assert_failure, create_api, page_items, string_member, Answer, Gateway,
};

/// How many elements each row is stood up with: enough that a one-at-a-time walk hands out
/// several cursors and a two-at-a-time page is cut inside the collection.
const COUNT: usize = 4;

/// The sentence a listing refuses a `position` it did not issue with. Fixed text, naming
/// nothing of the request.
const INVALID_POSITION: &str = "Invalid position parameter";

/// One collection under test: how it is listed, the identity of each element in the order
/// the collection reports them, and whether its cursor IS that identity.
struct Collection {
    /// The operation that lists it, which every case names.
    listing: &'static str,
    /// The collection's URI.
    uri: String,
    /// The member each element is identified by on the page.
    identity: &'static str,
    /// Every element's identity, in the order the collection reports them.
    elements: Vec<String>,
    /// `true` where the cursor is the element's own address — a name or a location the
    /// caller composed — and `false` where the collection is walked by an identifier this
    /// service minted and the cursor is a token of the service's own.
    cursor_is_the_elements_address: bool,
}

/// A create, asserting the modelled 201 and answering the created record.
async fn created(gateway: &Gateway, uri: &str, request: Value, case: &str) -> Value {
    let answer = gateway.call("POST", uri, Some(request)).await;
    assert_eq!(
        answer.status, 201,
        "case {case}: expected 201, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body
}

/// One page of `collection` with `query` appended (`""` for none).
async fn page(gateway: &Gateway, collection: &Collection, query: &str) -> Answer {
    let uri = if query.is_empty() {
        collection.uri.clone()
    } else {
        format!("{}?{query}", collection.uri)
    };
    gateway.call("GET", &uri, None).await
}

/// One page of `collection`, asserting the 200 a page of a resolvable cursor answers.
async fn listed(gateway: &Gateway, collection: &Collection, query: &str, case: &str) -> Value {
    let answer = page(gateway, collection, query).await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected 200 from {}, got {} with {:?}",
        collection.listing, answer.status, answer.body
    );
    answer.body
}

/// The identity of every element on `body`, in page order.
fn reported(body: &Value, collection: &Collection, case: &str) -> Vec<String> {
    page_items(body, case)
        .iter()
        .map(|item| string_member(item, collection.identity, case))
        .collect()
}

/// The account's api keys, stood up with [`COUNT`] keys.
async fn api_keys(gateway: &Gateway) -> Collection {
    let mut elements = Vec::with_capacity(COUNT);
    for index in 0..COUNT {
        let case = format!("CreateApiKey(#{index})");
        let body = created(
            gateway,
            "/apikeys",
            json!({ "name": format!("cursor-key-{index}") }),
            &case,
        )
        .await;
        elements.push(string_member(&body, "id", &case));
    }
    Collection {
        listing: "GetApiKeys",
        uri: "/apikeys".to_string(),
        identity: "id",
        elements,
        cursor_is_the_elements_address: false,
    }
}

/// The account's rest apis, stood up with [`COUNT`] apis.
async fn rest_apis(gateway: &Gateway) -> Collection {
    let mut elements = Vec::with_capacity(COUNT);
    for index in 0..COUNT {
        let (api, _root) = create_api(gateway, &format!("cursor-api-{index}")).await;
        elements.push(api);
    }
    Collection {
        listing: "GetRestApis",
        uri: "/restapis".to_string(),
        identity: "id",
        elements,
        cursor_is_the_elements_address: false,
    }
}

/// The account's client certificates, stood up with [`COUNT`] certificates.
async fn client_certificates(gateway: &Gateway) -> Collection {
    let mut elements = Vec::with_capacity(COUNT);
    for index in 0..COUNT {
        let case = format!("GenerateClientCertificate(#{index})");
        let body = created(
            gateway,
            "/clientcertificates",
            json!({ "description": format!("cursor certificate {index}") }),
            &case,
        )
        .await;
        elements.push(string_member(&body, "clientCertificateId", &case));
    }
    Collection {
        listing: "GetClientCertificates",
        uri: "/clientcertificates".to_string(),
        identity: "clientCertificateId",
        elements,
        cursor_is_the_elements_address: false,
    }
}

/// The resources of ONE api: the root resource every api is created with, plus the children
/// that bring the collection to [`COUNT`] elements — a collection held inside a parent
/// rather than by the account.
async fn resources(gateway: &Gateway) -> Collection {
    let (api, root) = create_api(gateway, "cursor-resources").await;
    let mut elements = vec![root.clone()];
    for index in 1..COUNT {
        let case = format!("CreateResource(#{index})");
        let body = created(
            gateway,
            &format!("/restapis/{api}/resources/{root}"),
            json!({ "pathPart": format!("part{index}") }),
            &case,
        )
        .await;
        elements.push(string_member(&body, "id", &case));
    }
    Collection {
        listing: "GetResources",
        uri: format!("/restapis/{api}/resources"),
        identity: "id",
        elements,
        cursor_is_the_elements_address: false,
    }
}

/// The models of ONE api: the models an api is created with, plus [`COUNT`] of this suite's
/// own — the contrast row, whose cursor is the NAME the caller gave each model.
async fn models(gateway: &Gateway) -> Collection {
    let (api, _root) = create_api(gateway, "cursor-models").await;
    let uri = format!("/restapis/{api}/models");
    for index in 0..COUNT {
        let case = format!("CreateModel(#{index})");
        created(
            gateway,
            &uri,
            json!({
                "name": format!("CursorModel{index:03}"),
                "contentType": "application/json",
            }),
            &case,
        )
        .await;
    }
    // The api was created holding models of its own, so the order is read back off the
    // collection rather than assumed from the creates.
    let collection = Collection {
        listing: "GetModels",
        uri,
        identity: "name",
        elements: Vec::new(),
        cursor_is_the_elements_address: true,
    };
    let body = listed(gateway, &collection, "limit=500", "GetModels(all)").await;
    let elements = reported(&body, &collection, "GetModels(all)");
    Collection {
        elements,
        ..collection
    }
}

/// Every collection this suite drives, each on its OWN emulator — a row's collection holds
/// exactly the elements that row stood up, which is what every case asserts the walk
/// against.
async fn collections() -> Vec<(Gateway, Collection)> {
    let mut rows = Vec::new();
    for stand_up in [
        "GetApiKeys",
        "GetRestApis",
        "GetClientCertificates",
        "GetResources",
        "GetModels",
    ] {
        let gateway = Gateway::new();
        let collection = match stand_up {
            "GetApiKeys" => api_keys(&gateway).await,
            "GetRestApis" => rest_apis(&gateway).await,
            "GetClientCertificates" => client_certificates(&gateway).await,
            "GetResources" => resources(&gateway).await,
            "GetModels" => models(&gateway).await,
            other => panic!("case {other}: this suite drives no such collection"),
        };
        rows.push((gateway, collection));
    }
    rows
}

/// The cursors `collection` hands out, read off a one-at-a-time walk: `(the number of
/// elements reported before it, the cursor)`.
///
/// This is the only way to hold the cursor of a position for a collection walked by minted
/// identifiers, and it reads the same for one walked by the element's own address — so one
/// helper serves every row.
async fn cursors(gateway: &Gateway, collection: &Collection) -> Vec<(usize, String)> {
    let case = format!("{}(limit=1, the cursors it hands out)", collection.listing);
    let mut handed_out: Vec<(usize, String)> = Vec::new();
    let mut reported_count = 0usize;
    let mut cursor: Option<String> = None;
    loop {
        let query = match &cursor {
            None => "limit=1".to_string(),
            Some(position) => format!("limit=1&position={position}"),
        };
        let body = listed(gateway, collection, &query, &case).await;
        reported_count += reported(&body, collection, &case).len();
        assert!(
            reported_count <= collection.elements.len(),
            "case {case}: the walk reported more than the {} element(s) the collection holds",
            collection.elements.len()
        );
        match body["position"].as_str() {
            None => break,
            Some(position) => {
                handed_out.push((reported_count, position.to_string()));
                cursor = Some(position.to_string());
            }
        }
    }
    assert_eq!(
        reported_count,
        collection.elements.len(),
        "case {case}: the walk must reach every element of {:?}",
        collection.elements
    );
    handed_out
}

#[tokio::test]
async fn a_cursor_names_the_position_the_next_page_resumes_at() {
    for (gateway, collection) in collections().await {
        let total = collection.elements.len();
        let handed_out = cursors(&gateway, &collection).await;
        assert_eq!(
            handed_out.len(),
            total - 1,
            "case {}: expected a cursor for every position past the first of {total}, got \
             {handed_out:?}",
            collection.listing
        );
        for (start, cursor) in &handed_out {
            let case = format!(
                "{}(position=<the cursor of position {start}>, limit=2)",
                collection.listing
            );
            // The page resumes AT the position the cursor names, and carries on at the
            // cursor of the position it stopped at — the same value whichever page hands it
            // out, since a cursor is a function of the position and not of the read.
            let body = listed(&gateway, &collection, &format!("position={cursor}&limit=2"), &case)
                .await;
            let page = reported(&body, &collection, &case);
            let end = (start + 2).min(total);
            assert_eq!(
                page,
                collection.elements[*start..end].to_vec(),
                "case {case}: expected {:?}, got {page:?}",
                &collection.elements[*start..end]
            );
            let next = handed_out
                .iter()
                .find(|(position, _)| *position == end)
                .map(|(_, cursor)| cursor.clone());
            assert_eq!(
                body["position"].as_str().map(str::to_owned),
                next,
                "case {case}: expected the cursor of position {end} ({next:?}), got {:?}",
                body["position"]
            );
        }
    }
}

#[tokio::test]
async fn a_minted_cursor_spells_no_record_and_an_address_cursor_spells_its_element() {
    for (gateway, collection) in collections().await {
        let handed_out = cursors(&gateway, &collection).await;
        for (start, cursor) in &handed_out {
            let case = format!(
                "{}(the cursor of position {start})",
                collection.listing
            );
            if collection.cursor_is_the_elements_address {
                assert_eq!(
                    cursor, &collection.elements[*start],
                    "case {case}: a collection addressed by the caller hands that address \
                     back, expected {:?}, got {cursor:?}",
                    collection.elements[*start]
                );
            } else {
                assert!(
                    collection
                        .elements
                        .iter()
                        .all(|element| !cursor.contains(element.as_str())),
                    "case {case}: a cursor of the service's own spells no record of the \
                     collection {:?}, got {cursor:?}",
                    collection.elements
                );
            }
        }
    }
}

#[tokio::test]
async fn a_value_the_listing_did_not_issue_as_a_cursor_names_no_position() {
    for (gateway, collection) in collections().await {
        let handed_out = cursors(&gateway, &collection).await;
        let (position, minted) = handed_out[0].clone();
        // Rows differ along the form of the value a caller could send: a word, a value of a
        // minted id's shape, a minted cursor with its last character dropped, and — for a
        // collection walked by minted identifiers — the identity of the element the cursor
        // names, which is the value the collection must NOT accept, because an identifier
        // this service assigned is not a cursor.
        let mut refused: Vec<(String, String)> = vec![
            ("a word no cursor takes".to_string(), "nosuchcursor".to_string()),
            ("a value of an id's shape".to_string(), "0000000000".to_string()),
            (
                "a minted cursor with its last character dropped".to_string(),
                minted[..minted.len() - 1].to_string(),
            ),
        ];
        if !collection.cursor_is_the_elements_address {
            refused.push((
                format!("the identity of the element at position {position}"),
                collection.elements[position].clone(),
            ));
        }
        for (row, value) in refused {
            let case = format!("{}({row})", collection.listing);
            let answer = page(&gateway, &collection, &format!("position={value}")).await;
            assert_failure(&answer, 400, "BadRequestException", &case);
            assert_eq!(
                answer.body["message"].as_str(),
                Some(INVALID_POSITION),
                "case {case}: expected the refusal {INVALID_POSITION:?}, got {:?}",
                answer.body["message"]
            );
        }
    }
}

#[tokio::test]
async fn a_cursor_resumes_only_the_listing_that_issued_it() {
    // Two collections of the account, each walked by identifiers this service minted: a
    // cursor one of them issued names a position in that collection alone, so the other
    // refuses it rather than resuming its own walk somewhere.
    let gateway = Gateway::new();
    let keys = api_keys(&gateway).await;
    let apis = rest_apis(&gateway).await;

    let key_cursor = cursors(&gateway, &keys).await[0].1.clone();
    let api_cursor = cursors(&gateway, &apis).await[0].1.clone();

    for (collection, foreign, row) in [
        (&keys, &api_cursor, "a cursor GetRestApis issued"),
        (&apis, &key_cursor, "a cursor GetApiKeys issued"),
    ] {
        let case = format!("{}({row})", collection.listing);
        let answer = page(&gateway, collection, &format!("position={foreign}")).await;
        assert_failure(&answer, 400, "BadRequestException", &case);
        assert_eq!(
            answer.body["message"].as_str(),
            Some(INVALID_POSITION),
            "case {case}: expected the refusal {INVALID_POSITION:?}, got {:?}",
            answer.body["message"]
        );
    }

    // Its own cursor still resumes its own walk, so the refusals above took nothing away.
    for collection in [&keys, &apis] {
        let own = cursors(&gateway, collection).await[0].1.clone();
        let case = format!("{}(its own cursor)", collection.listing);
        let body = listed(&gateway, collection, &format!("position={own}"), &case).await;
        let page = reported(&body, collection, &case);
        assert_eq!(
            page,
            collection.elements[1..].to_vec(),
            "case {case}: expected {:?}, got {page:?}",
            &collection.elements[1..]
        );
    }
}

#[tokio::test]
async fn a_cursor_resumes_only_the_parents_collection_it_was_issued_for() {
    // The SAME operation over two parents: a collection held inside one api is not the one
    // held inside another, so a cursor issued for one names no position in the other even
    // though both sequences are of ids of the same kind.
    let gateway = Gateway::new();
    let here = resources(&gateway).await;
    let there = resources(&gateway).await;

    let here_cursor = cursors(&gateway, &here).await[0].1.clone();
    let there_cursor = cursors(&gateway, &there).await[0].1.clone();

    for (collection, foreign, row) in [
        (&here, &there_cursor, "the other api's cursor"),
        (&there, &here_cursor, "the first api's cursor"),
    ] {
        let case = format!("{}({row})", collection.listing);
        let answer = page(&gateway, collection, &format!("position={foreign}")).await;
        assert_failure(&answer, 400, "BadRequestException", &case);
        assert_eq!(
            answer.body["message"].as_str(),
            Some(INVALID_POSITION),
            "case {case}: expected the refusal {INVALID_POSITION:?}, got {:?}",
            answer.body["message"]
        );
    }

    // And each api's own cursor still resumes that api's own collection.
    for (collection, cursor) in [(&here, &here_cursor), (&there, &there_cursor)] {
        let case = format!("{}(its own api's cursor)", collection.listing);
        let body = listed(&gateway, collection, &format!("position={cursor}"), &case).await;
        let reported_ids = reported(&body, collection, &case);
        assert_eq!(
            reported_ids,
            collection.elements[1..].to_vec(),
            "case {case}: expected {:?}, got {reported_ids:?}",
            &collection.elements[1..]
        );
    }
}

#[tokio::test]
async fn walking_a_collection_changes_nothing_a_later_caller_sees() {
    for (gateway, collection) in collections().await {
        let case = format!("{}(before the walk)", collection.listing);
        let before = listed(&gateway, &collection, "limit=500", &case).await;
        // Every shape of read a cursor takes part in: the whole collection, a one-at-a-time
        // walk of it, a page from each cursor, and a value the listing did not issue.
        let handed_out = cursors(&gateway, &collection).await;
        for (_, cursor) in &handed_out {
            let _ = page(&gateway, &collection, &format!("position={cursor}&limit=2")).await;
        }
        let _ = page(&gateway, &collection, "position=nosuchcursor").await;
        let case = format!("{}(after the walk)", collection.listing);
        let after = listed(&gateway, &collection, "limit=500", &case).await;
        assert_eq!(
            after, before,
            "case {case}: a walk must change nothing a later caller sees, got {after:?}"
        );
        // And the cursors are the same values after the walk as during it: a cursor is a
        // function of the position it names, so reading a collection does not re-mint them.
        let again = cursors(&gateway, &collection).await;
        assert_eq!(
            again, handed_out,
            "case {case}: expected the same cursors {handed_out:?}, got {again:?}"
        );
    }
}
