// SPDX-License-Identifier: CC0-1.0

//! What a field a domain's index MAPS AS A DATE accepts, stores and compares — driven over
//! the real wire path: a signed request to the endpoint a domain publishes, through the
//! multi-service router, into the mounted service.
//!
//! One rule, asserted over tables of inputs rather than one recorded call:
//!
//! > a field mapped as a date accepts every value of the format set its mapping declares —
//! > the engine's default pair where it declares none, an ISO-8601 instant whose time part is
//! > optional or a count of epoch milliseconds — whichever form the document writes it in;
//! > the document's `_source` is answered exactly as it was sent; and every comparison on
//! > such a field (a term, a range bound, an order) is taken on the INSTANT each side parses
//! > to and not on the characters it was written with. A value outside the format set is
//! > refused, and so is a bound outside it.
//!
//! Asserted beside it, because the same searches are what reach a date field at all: the
//! clauses that ask a question of several fields or of a field pattern — `exists`,
//! `multi_match`, and the query a request states in its own query string (`?q=…`).
//!
//! The harness is [`super::domain_lifecycle`]'s and the data-plane request helper is
//! [`super::domain_rest_data_plane`]'s; this suite adds no second copy of either.

use serde_json::{json, Value};

use super::domain_lifecycle::{Answer, Emulator};
use super::domain_rest_data_plane::rest_prefixed;

/// The domain whose published endpoint every request below is addressed to.
const DOMAIN: &str = "date-plane";

/// The index every row is read over: the shape a client that writes ISO dates declares — two
/// analyzed text fields, a date and a number.
const INDEX: &str = "catalog";

/// The mapping that index is created with.
fn catalog_mapping() -> Value {
    json!({
        "settings": { "index": { "number_of_shards": 1, "number_of_replicas": 0 } },
        "mappings": { "properties": {
            "title": { "type": "text" },
            "body": { "type": "text" },
            "published": { "type": "date" },
            "rating": { "type": "integer" },
        } }
    })
}

/// The three documents the comparison tables are read over: one dating itself with a plain
/// date, one with epoch milliseconds, and one with a full ISO instant — so a comparison that
/// read the characters rather than the instant would answer differently from one that did.
///
/// `d1`'s instant is one second BEFORE `d0`'s, which is what makes a bound at `d0`'s day
/// separate them.
fn documents() -> Vec<(&'static str, Value)> {
    vec![
        (
            "d0",
            json!({
                "title": "red apple",
                "body": "fresh fruit",
                "published": "2026-01-01",
                "rating": 5,
            }),
        ),
        (
            "d1",
            json!({
                "title": "green pear",
                "body": "stale fruit",
                "published": 1_767_225_599_000_i64,
                "rating": 3,
            }),
        ),
        (
            "d2",
            json!({
                "title": "blue plum",
                "body": "fresh fruit",
                "published": "2026-02-15T12:30:00Z",
            }),
        ),
    ]
}

/// The instant `d0` is dated at: `2026-01-01T00:00:00Z` in epoch milliseconds.
const D0_INSTANT: i64 = 1_767_225_600_000;

/// `d1`'s, one second before it.
const D1_INSTANT: i64 = 1_767_225_599_000;

/// `d2`'s: `2026-02-15T12:30:00Z`.
const D2_INSTANT: i64 = 1_771_158_600_000;

/// `CreateDomain`, then the index and its documents.
async fn given_indexed(emulator: &Emulator) {
    given_domain(emulator).await;
    let index = put(emulator, &format!("/{INDEX}"), catalog_mapping()).await;
    assert_eq!(
        index.status, 200,
        "case PUT /{INDEX}: expected 200, got {} with {:?}",
        index.status, index.body
    );
    for (id, source) in documents() {
        let written = put(emulator, &format!("/{INDEX}/_doc/{id}"), source.clone()).await;
        assert_eq!(
            (written.status, written.body["result"].as_str()),
            (201, Some("created")),
            "case write {INDEX}/{id} with published={:?}: expected 201 created, got {} with {:?}",
            source["published"],
            written.status,
            written.body
        );
    }
}

/// `CreateDomain` alone, for a case that creates its own index.
async fn given_domain(emulator: &Emulator) {
    let created = emulator
        .create(json!({
            "DomainName": DOMAIN,
            "EngineVersion": "OpenSearch_2.11",
            "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 1 },
            "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "case CreateDomain({DOMAIN:?}): expected 200, got {} with {:?}",
        created.status, created.body
    );
}

/// One `PUT` to the domain's own data plane.
async fn put(emulator: &Emulator, path: &str, body: Value) -> Answer {
    rest_prefixed(emulator, DOMAIN, "PUT", path, Some(body)).await
}

/// One `GET` on it.
async fn get(emulator: &Emulator, path: &str) -> Answer {
    rest_prefixed(emulator, DOMAIN, "GET", path, None).await
}

/// One `POST /{index}/_search`.
async fn search(emulator: &Emulator, body: Value) -> Answer {
    rest_prefixed(
        emulator,
        DOMAIN,
        "POST",
        &format!("/{INDEX}/_search"),
        Some(body),
    )
    .await
}

/// The ids the hits of an answer carry, in the order the answer lists them.
fn hit_ids(answer: &Answer) -> Vec<String> {
    answer.body["hits"]["hits"]
        .as_array()
        .map(|hits| {
            hits.iter()
                .map(|hit| hit["_id"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Those ids SORTED, for a row that states which documents a query selects rather than the
/// order they come back in.
fn selected(answer: &Answer) -> Vec<String> {
    let mut ids = hit_ids(answer);
    ids.sort();
    ids
}

/// RULE: a date field takes every form of its default format set, and the `_source` a read
/// answers is the document exactly as it was sent — the value is not rewritten into one of
/// those forms.
///
/// The rows span the levels the ISO form may stop at, the offsets it may carry, the fraction
/// of a second, both spellings of epoch milliseconds, and an array of values of the field.
#[tokio::test]
async fn a_date_field_takes_every_form_of_its_format_set() {
    let emulator = Emulator::new();
    given_domain(&emulator).await;
    let index = put(&emulator, &format!("/{INDEX}"), catalog_mapping()).await;
    assert_eq!(
        index.status, 200,
        "case PUT /{INDEX}: expected 200, got {} with {:?}",
        index.status, index.body
    );

    // (the case, the id, the value the document's date field holds)
    // The forms are the ones `strict_date_optional_time||epoch_millis` admits: the date with
    // its time part stopped at every level, an offset in either spelling, and the number.
    let rows: &[(&str, &str, Value)] = &[
        ("a plain date", "f0", json!("2026-01-01")),
        ("a date and a clock", "f1", json!("2026-01-01T00:00:00")),
        ("a date, a clock and the zero offset", "f2", json!("2026-01-01T00:00:00Z")),
        ("a clock without seconds", "f3", json!("2026-01-01T06:30Z")),
        ("a fraction of a second", "f4", json!("2026-01-01T00:00:00.123Z")),
        ("an offset east of UTC", "f5", json!("2026-01-01T01:00:00+01:00")),
        ("an offset written without a colon", "f6", json!("2026-01-01T01:00:00+0100")),
        ("a year and a month", "f7", json!("2026-01")),
        ("a year alone", "f8", json!("2026")),
        ("epoch milliseconds as a number", "f9", json!(D0_INSTANT)),
        ("epoch milliseconds as text", "f10", json!("1767225600000")),
        ("the epoch itself", "f11", json!(0)),
        ("a leap day", "f12", json!("2024-02-29")),
        ("an instant before the epoch", "f13", json!("1969-12-31T23:59:59Z")),
        ("no value at all", "f14", Value::Null),
        (
            "two values of the field, written differently",
            "f15",
            json!(["2026-01-01", D0_INSTANT]),
        ),
    ];
    for (case, id, value) in rows {
        let source = json!({ "title": "a title", "published": value.clone(), "rating": 1 });
        let written = put(&emulator, &format!("/{INDEX}/_doc/{id}"), source.clone()).await;
        assert_eq!(
            (written.status, written.body["result"].as_str()),
            (201, Some("created")),
            "case {case:?}: expected 201 created, got {} with {:?}",
            written.status,
            written.body
        );
        // The stored source round-trips VERBATIM: the same JSON, with the value in the form
        // and of the kind it was sent in.
        let read = get(&emulator, &format!("/{INDEX}/_doc/{id}")).await;
        assert_eq!(
            (read.status, &read.body["_source"]),
            (200, &source),
            "case {case:?}: expected the source back as it was sent, got {} with {:?}",
            read.status,
            read.body
        );
        // And writing the same id again UPDATES it rather than creating it a second time.
        let again = put(&emulator, &format!("/{INDEX}/_doc/{id}"), source).await;
        assert_eq!(
            (again.status, again.body["result"].as_str()),
            (200, Some("updated")),
            "case {case:?} written again: expected 200 updated, got {} with {:?}",
            again.status,
            again.body
        );
    }
}

/// RULE: a value a date field's format set reads no instant from is refused — the engine's
/// `mapper_parsing_exception`, naming the field, the type it is mapped as, the document and
/// the value.
///
/// The rows span the ways a value can fall outside that set: text that is no date, a
/// component nothing numbers, the lenient spellings the STRICT form does not admit, and
/// values of another kind entirely.
#[tokio::test]
async fn a_value_outside_the_format_set_is_refused() {
    let emulator = Emulator::new();
    given_domain(&emulator).await;
    put(&emulator, &format!("/{INDEX}"), catalog_mapping()).await;

    // (the case, the value the document's date field holds)
    let rows: &[(&str, Value)] = &[
        ("text that is no date at all", json!("not-a-date")),
        ("a month nothing numbers", json!("2026-13-01")),
        ("a day the month has not", json!("2026-02-30")),
        ("an hour nothing clocks", json!("2026-01-01T24:00:00Z")),
        ("a year written in two digits", json!("26-01-01")),
        ("a month written in one digit", json!("2026-1-01")),
        ("a date and a clock joined by a space", json!("2026-01-01 00:00:00")),
        ("an empty value", json!("")),
        ("a value of another kind", json!(true)),
        ("an object", json!({ "at": "2026-01-01" })),
        ("one value of an array outside the set", json!(["2026-01-01", "soon"])),
    ];
    for (case, value) in rows {
        let id = "refused";
        let written = put(
            &emulator,
            &format!("/{INDEX}/_doc/{id}"),
            json!({ "title": "a title", "published": value.clone() }),
        )
        .await;
        let reason = written.body["error"]["reason"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            (written.status, written.body["error"]["type"].as_str()),
            (400, Some("mapper_parsing_exception")),
            "case {case:?}: expected the parse refusal of a value outside the format set, got \
             {} with {:?}",
            written.status,
            written.body
        );
        assert!(
            reason.starts_with(&format!(
                "failed to parse field [published] of type [date] in document with id '{id}'. \
                 Preview of field's value: '"
            )),
            "case {case:?}: expected the refusal to name the field, the type and the document, \
             got {reason:?}"
        );
        // And nothing was stored: the refused write left the index without the document.
        let read = get(&emulator, &format!("/{INDEX}/_doc/{id}")).await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: expected the refused document not to be held, got {} with {:?}",
            read.status, read.body
        );
    }
}

/// RULE: every comparison on a date field is taken on the INSTANT each side parses to, so a
/// bound, a term or an order written in one form reaches a document written in another.
///
/// The rows span the four bounds a `range` states, both forms a bound may be written in, the
/// bounds combined, and a term naming the same instant in another form.
#[tokio::test]
async fn a_date_field_compares_on_the_instant() {
    let emulator = Emulator::new();
    given_indexed(&emulator).await;

    // (the case, the query, the ids it must select)
    let rows: &[(&str, Value, &[&str])] = &[
        (
            "a lower bound written as a date",
            json!({ "range": { "published": { "gte": "2026-01-01" } } }),
            &["d0", "d2"],
        ),
        (
            "that same bound written as epoch milliseconds",
            json!({ "range": { "published": { "gte": D0_INSTANT } } }),
            &["d0", "d2"],
        ),
        (
            "that same bound written as a full ISO instant",
            json!({ "range": { "published": { "gte": "2026-01-01T00:00:00Z" } } }),
            &["d0", "d2"],
        ),
        (
            "an exclusive lower bound at the same instant",
            json!({ "range": { "published": { "gt": "2026-01-01" } } }),
            &["d2"],
        ),
        (
            "an upper bound below it",
            json!({ "range": { "published": { "lt": "2026-01-01" } } }),
            &["d1"],
        ),
        (
            "an inclusive upper bound at it",
            json!({ "range": { "published": { "lte": D0_INSTANT } } }),
            &["d0", "d1"],
        ),
        (
            "both bounds, naming one month",
            json!({ "range": { "published": { "gte": "2026-01-01", "lt": "2026-02-01" } } }),
            &["d0"],
        ),
        (
            "bounds that name no instant between them",
            json!({ "range": { "published": { "gte": "2026-03-01", "lt": "2026-01-01" } } }),
            &[],
        ),
        (
            "a term naming the instant in another form",
            json!({ "term": { "published": "2026-01-01T00:00:00Z" } }),
            &["d0"],
        ),
        (
            "a term naming it as epoch milliseconds",
            json!({ "term": { "published": D0_INSTANT } }),
            &["d0"],
        ),
        (
            "a term naming the instant a document wrote as a number",
            json!({ "term": { "published": "2025-12-31T23:59:59Z" } }),
            &["d1"],
        ),
        (
            "a term naming no instant any document holds",
            json!({ "term": { "published": "2026-01-02" } }),
            &[],
        ),
        (
            "a range composed with another clause",
            json!({ "bool": {
                "filter": [{ "range": { "published": { "gte": "2026-01-01" } } }],
                "must_not": [{ "term": { "body": "stale" } }],
            } }),
            &["d0", "d2"],
        ),
        (
            "a range on the number field, which compares magnitudes",
            json!({ "range": { "rating": { "gte": 4 } } }),
            &["d0"],
        ),
    ];
    for (case, query, expected) in rows {
        let answered = search(&emulator, json!({ "query": query.clone(), "size": 10 })).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected the search to be served, got {} with {:?}",
            answered.status, answered.body
        );
        let ids = selected(&answered);
        assert_eq!(
            ids,
            expected.iter().map(|id| (*id).to_string()).collect::<Vec<_>>(),
            "case {case:?}: expected {expected:?}, got {ids:?} from {:?}",
            answered.body["hits"]["total"]
        );
        assert_eq!(
            answered.body["hits"]["total"]["value"].as_u64(),
            Some(expected.len() as u64),
            "case {case:?}: expected the total to be {}, got {:?}",
            expected.len(),
            answered.body["hits"]["total"]
        );
    }

    // An ORDER on the field is taken on the same instants, and the sort value a hit carries
    // is that instant in epoch milliseconds.
    let ordered = search(&emulator, json!({ "sort": [{ "published": "asc" }] })).await;
    assert_eq!(
        hit_ids(&ordered),
        vec!["d1", "d0", "d2"],
        "case \"ascending on the date field\": expected the instants in order, got {:?}",
        ordered.body
    );
    assert_eq!(
        ordered.body["hits"]["hits"][0]["sort"],
        json!([D1_INSTANT]),
        "case \"ascending on the date field\": expected the first hit's sort value to be the \
         instant, got {:?}",
        ordered.body["hits"]["hits"][0]
    );
    let descending = search(&emulator, json!({ "sort": [{ "published": "desc" }] })).await;
    assert_eq!(
        (hit_ids(&descending), &descending.body["hits"]["hits"][0]["sort"]),
        (
            vec!["d2".to_string(), "d0".to_string(), "d1".to_string()],
            &json!([D2_INSTANT])
        ),
        "case \"descending on the date field\": expected the instants in reverse, got {:?}",
        descending.body
    );
}

/// RULE: a bound a date field reads no instant from states no limit at all, so the search is
/// refused rather than answered from a comparison nothing could take.
#[tokio::test]
async fn a_bound_outside_the_format_set_refuses_the_search() {
    let emulator = Emulator::new();
    given_indexed(&emulator).await;

    // (the case, the bound)
    let rows: &[(&str, Value)] = &[
        ("text that is no date at all", json!("yesterday")),
        ("a month nothing numbers", json!("2026-13-01")),
        ("a value of another kind", json!(true)),
    ];
    for (case, bound) in rows {
        let answered = search(
            &emulator,
            json!({ "query": { "range": { "published": { "gte": bound.clone() } } } }),
        )
        .await;
        let reason = answered.body["error"]["root_cause"][0]["reason"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            (
                answered.status,
                answered.body["error"]["root_cause"][0]["type"].as_str()
            ),
            (400, Some("parse_exception")),
            "case {case:?}: expected the refusal of a bound outside the format set, got {} with \
             {:?}",
            answered.status,
            answered.body
        );
        assert!(
            reason.starts_with("failed to parse date field [")
                && reason.ends_with(
                    "] with format [strict_date_optional_time||epoch_millis]"
                ),
            "case {case:?}: expected the refusal to name the value and the format set, got \
             {reason:?}"
        );
    }

    // A member of a `range` this surface does not read is refused too, rather than dropped —
    // a bound compared under a reading the caller did not ask for is a wrong answer.
    let unread = search(
        &emulator,
        json!({ "query": { "range": { "published": { "gte": "2026-01-01", "format": "yyyy" } } } }),
    )
    .await;
    assert_eq!(
        (unread.status, unread.body["error"]["type"].as_str()),
        (400, Some("parsing_exception")),
        "case \"a range member this surface does not read\": expected a refusal, got {} with \
         {:?}",
        unread.status,
        unread.body
    );
}

/// RULE: a clause that asks its question of SEVERAL fields asks it of each of them — the
/// fields it names, the fields a pattern it names expands to, or every field the index maps
/// where it names none — and `exists` asks whether the document holds a value at a field at
/// all.
#[tokio::test]
async fn a_clause_over_several_fields_asks_each_of_them() {
    let emulator = Emulator::new();
    given_indexed(&emulator).await;

    // (the case, the query, the ids it must select)
    let rows: &[(&str, Value, &[&str])] = &[
        (
            "exists on a field two documents hold",
            json!({ "exists": { "field": "rating" } }),
            &["d0", "d1"],
        ),
        (
            "exists on a field every document holds",
            json!({ "exists": { "field": "published" } }),
            &["d0", "d1", "d2"],
        ),
        (
            "exists on a field the index maps and nothing holds",
            json!({ "exists": { "field": "shelf" } }),
            &[],
        ),
        (
            "a multi_match over two named fields",
            json!({ "multi_match": { "query": "red", "fields": ["title", "body"] } }),
            &["d0"],
        ),
        (
            "a multi_match whose term only the second field carries",
            json!({ "multi_match": { "query": "stale", "fields": ["title", "body"] } }),
            &["d1"],
        ),
        (
            "a multi_match naming no field at all",
            json!({ "multi_match": { "query": "fruit" } }),
            &["d0", "d1", "d2"],
        ),
        (
            "a multi_match over a field pattern",
            json!({ "multi_match": { "query": "plum", "fields": ["tit*"] } }),
            &["d2"],
        ),
        (
            "a multi_match naming a field weight",
            json!({ "multi_match": { "query": "apple", "fields": ["title^3", "body"] } }),
            &["d0"],
        ),
        (
            "a multi_match asking for a phrase",
            json!({
                "multi_match": {
                    "query": "fresh fruit",
                    "fields": ["body"],
                    "type": "phrase",
                }
            }),
            &["d0", "d2"],
        ),
    ];
    for (case, query, expected) in rows {
        let answered = search(&emulator, json!({ "query": query.clone(), "size": 10 })).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected the search to be served, got {} with {:?}",
            answered.status, answered.body
        );
        let ids = selected(&answered);
        assert_eq!(
            ids,
            expected.iter().map(|id| (*id).to_string()).collect::<Vec<_>>(),
            "case {case:?}: expected {expected:?}, got {ids:?}"
        );
    }
}

/// RULE: a search that carries no body query reads the query its own QUERY STRING states —
/// terms and phrases asked of the field each names or of every field the index maps, placed
/// by the prefixes and operators it carries — and a query string carrying syntax this plane
/// does not read is refused rather than answered as though it did.
#[tokio::test]
async fn a_search_reads_the_query_its_uri_states() {
    let emulator = Emulator::new();
    given_indexed(&emulator).await;

    // (the case, the query string, the ids it must select)
    let rows: &[(&str, &str, &[&str])] = &[
        ("a term asked of a text field", "q=title:apple", &["d0"]),
        ("a term asked of every field", "q=red", &["d0"]),
        (
            "a term every document carries somewhere",
            "q=fruit",
            &["d0", "d1", "d2"],
        ),
        ("a term asked of a date field", "q=published:2026-01-01", &["d0"]),
        (
            "that same instant written in another form",
            "q=published:2026-01-01T00:00:00Z",
            &["d0"],
        ),
        (
            "an instant a document wrote as a number",
            "q=published:2025-12-31T23:59:59Z",
            &["d1"],
        ),
        ("a term asked of a number field", "q=rating:5", &["d0"]),
        (
            "two terms, joined by the OR the request defaults to",
            "q=title:apple%20title:pear",
            &["d0", "d1"],
        ),
        (
            "two terms joined by AND",
            "q=title:apple%20AND%20body:fresh",
            &["d0"],
        ),
        (
            "two terms joined by nothing under the AND default",
            "q=title:apple%20body:fresh&default_operator=AND",
            &["d0"],
        ),
        (
            "a required and a prohibited clause",
            "q=%2Bbody:fruit%20-body:stale",
            &["d0", "d2"],
        ),
        (
            "a clause after NOT",
            "q=body:fruit%20NOT%20body:stale",
            &["d0", "d2"],
        ),
        (
            "a phrase",
            "q=body:%22fresh%20fruit%22",
            &["d0", "d2"],
        ),
        (
            "a term asked of the default field the request names",
            "q=plum&df=title",
            &["d2"],
        ),
        ("a term nothing carries", "q=title:quince", &[]),
        (
            "the term that asks for every document",
            "q=%2A",
            &["d0", "d1", "d2"],
        ),
        (
            "that same term written in full",
            "q=%2A:%2A",
            &["d0", "d1", "d2"],
        ),
        (
            "a term asking for the presence of a value",
            "q=rating:%2A",
            &["d0", "d1"],
        ),
    ];
    for (case, query, expected) in rows {
        let answered = get(&emulator, &format!("/{INDEX}/_search?{query}&size=10")).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected the search to be served, got {} with {:?}",
            answered.status, answered.body
        );
        let ids = selected(&answered);
        assert_eq!(
            ids,
            expected.iter().map(|id| (*id).to_string()).collect::<Vec<_>>(),
            "case {case:?}: expected {expected:?}, got {ids:?}"
        );
    }

    // The same syntax in the BODY's own `query_string` clause selects the same documents, so
    // a client has one reading of it whichever form it writes it in.
    let clause = search(
        &emulator,
        json!({ "query": { "query_string": { "query": "title:apple" } } }),
    )
    .await;
    assert_eq!(
        selected(&clause),
        vec!["d0".to_string()],
        "case \"the same query as a body clause\": expected d0, got {:?}",
        clause.body
    );

    // And syntax this plane does not read is refused with the engine's own parse failure.
    // (the case, the query string)
    let refused: &[(&str, &str)] = &[
        ("a wildcard term", "q=title:app%2A"),
        ("a grouping", "q=%28title:apple%29"),
        ("a bracketed range", "q=published:%5B2026-01-01%20TO%202026-12-31%5D"),
        ("a fuzziness", "q=title:apple~2"),
    ];
    for (case, query) in refused {
        let answered = get(&emulator, &format!("/{INDEX}/_search?{query}")).await;
        assert_eq!(
            (answered.status, answered.body["error"]["type"].as_str()),
            (400, Some("query_shard_exception")),
            "case {case:?}: expected the refusal of syntax this plane does not read, got {} \
             with {:?}",
            answered.status,
            answered.body
        );
    }
}

/// RULE: a `_count` and a `_delete_by_query` select by the same clauses a `_search` does, so
/// a date bound reaches the same documents whichever of them asks it.
#[tokio::test]
async fn one_reading_of_a_query_serves_every_route_that_selects() {
    let emulator = Emulator::new();
    given_indexed(&emulator).await;

    let counted = rest_prefixed(
        &emulator,
        DOMAIN,
        "POST",
        &format!("/{INDEX}/_count"),
        Some(json!({ "query": { "range": { "published": { "gte": "2026-01-01" } } } })),
    )
    .await;
    assert_eq!(
        (counted.status, counted.body["count"].as_u64()),
        (200, Some(2)),
        "case \"a count over a date bound\": expected 2, got {} with {:?}",
        counted.status,
        counted.body
    );

    let removed = rest_prefixed(
        &emulator,
        DOMAIN,
        "POST",
        &format!("/{INDEX}/_delete_by_query"),
        Some(json!({ "query": { "range": { "published": { "lt": "2026-01-01" } } } })),
    )
    .await;
    assert_eq!(
        (removed.status, removed.body["deleted"].as_u64()),
        (200, Some(1)),
        "case \"a delete over a date bound\": expected one document removed, got {} with {:?}",
        removed.status,
        removed.body
    );
    let left = get(&emulator, &format!("/{INDEX}/_search?q=fruit&size=10")).await;
    assert_eq!(
        selected(&left),
        vec!["d0".to_string(), "d2".to_string()],
        "case \"the documents left after the delete\": expected d0 and d2, got {:?}",
        left.body
    );
}
