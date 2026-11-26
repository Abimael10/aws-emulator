// SPDX-License-Identifier: CC0-1.0

//! The SORT KEY a search orders its hits by — the precision it compares at, which of several
//! values it reads, and what a document holding none sorts at — driven over the real wire
//! path: a signed request to the endpoint a domain publishes, through the multi-service
//! router, into the mounted service.
//!
//! The rule, over the set of values a key may read:
//!
//! > a sort key orders by the per-document values of the field it names, compared at the
//! > PRECISION those values hold — two whole numbers exactly, however large — picks one value
//! > out of several the way its `mode` says, and places a document holding NO value where its
//! > `missing` option says, carrying as its echoed sort value the one the order really used:
//! > the end of the long range for a key whose values are longs, the value the request stated
//! > where it stated one.
//!
//! Why the echoed value matters: live pagination feeds the sort values of the last hit back as
//! the cursor, so a value a client cannot resume from — or a key that ties two longs either
//! side of what a magnitude carries exactly — makes a paged walk skip or repeat documents.
//!
//! The harness is [`super::domain_lifecycle`]'s and the data-plane request helper is
//! [`super::domain_rest_data_plane`]'s; this suite adds no second copy of either.

use serde_json::{json, Value};

use super::domain_lifecycle::{Answer, Emulator};
use super::domain_rest_data_plane::rest_prefixed;

/// The domain whose published endpoint every request below is addressed to.
const DOMAIN: &str = "ordering-domain";

/// The index the table is read over: a `long` whose values straddle what a magnitude carries
/// exactly, a `long` holding SEVERAL values per document, and a `keyword` to break ties with.
const INDEX: &str = "ledger";

/// The three values either side of 2^53 the exact-order rows are read on, and the negative one
/// below them: a magnitude carries whole numbers exactly only up to 2^53, so these are the
/// values a comparison taken on one transposes.
const BELOW: i64 = 9_007_199_254_740_992;
const ABOVE: i64 = 9_007_199_254_740_993;
const HIGHEST: i64 = 9_007_199_254_740_995;
const NEGATIVE: i64 = -9_007_199_254_740_993;

/// The ends of the long range, which are the sentinels a key whose values are longs places a
/// document holding none at.
const GREATEST_LONG: i64 = i64::MAX;
const LEAST_LONG: i64 = i64::MIN;

/// `CreateDomain`, then the index and its documents — one of which holds NO value for the
/// numeric keys at all.
async fn given_indexed(emulator: &Emulator) {
    let created = emulator
        .create(json!({
            "DomainName": DOMAIN,
            "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 1 },
            "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "case CreateDomain({DOMAIN:?}): expected 200, got {} with {:?}",
        created.status, created.body
    );
    let index = rest_prefixed(
        emulator,
        DOMAIN,
        "PUT",
        &format!("/{INDEX}"),
        Some(json!({
            "settings": { "number_of_shards": 1, "number_of_replicas": 0 },
            "mappings": { "properties": {
                "big": { "type": "long" },
                "scores": { "type": "long" },
                "name": { "type": "keyword" },
                "label": { "type": "keyword" },
            } },
        })),
    )
    .await;
    assert_eq!(
        index.status, 200,
        "case \"PUT /{INDEX}\": expected 200, got {} with {:?}",
        index.status, index.body
    );
    // Written in an order no key below puts them in, so an answer in insertion order is
    // distinguishable from an ordered one. `d3` holds neither numeric key and no `label`,
    // which is the document every `missing` row is about.
    let documents: [(&str, Value); 5] = [
        (
            "d1",
            json!({ "big": ABOVE, "scores": [2, 4], "name": "delta", "label": "one" }),
        ),
        (
            "d2",
            json!({ "big": BELOW, "scores": [3], "name": "alpha", "label": "two" }),
        ),
        ("d3", json!({ "name": "echo" })),
        (
            "d4",
            json!({ "big": HIGHEST, "scores": [1, 9], "name": "bravo", "label": "three" }),
        ),
        (
            "d5",
            json!({ "big": NEGATIVE, "scores": [6, 6, 7], "name": "charlie", "label": "four" }),
        ),
    ];
    for (id, source) in documents {
        let written = rest_prefixed(
            emulator,
            DOMAIN,
            "PUT",
            &format!("/{INDEX}/_doc/{id}"),
            Some(source),
        )
        .await;
        assert!(
            written.status == 200 || written.status == 201,
            "case \"write {INDEX}/{id}\": expected 200 or 201, got {} with {:?}",
            written.status,
            written.body
        );
    }
}

/// One row of the sentinel table: the case, the sort the request names, and the (id, sort
/// values) pairs it must be answered with.
type OrderRow = (&'static str, Value, Vec<(&'static str, Value)>);

/// One row of the mode table: the case, the mode the key names, and the (id, value) each
/// document is ordered by under it.
type ModeRow = (&'static str, Option<&'static str>, Vec<(&'static str, i64)>);

/// One `POST /{INDEX}/_search` asking for `sort`.
async fn sorted(emulator: &Emulator, sort: Value) -> Answer {
    rest_prefixed(
        emulator,
        DOMAIN,
        "POST",
        &format!("/{INDEX}/_search"),
        Some(json!({ "size": 10, "sort": sort })),
    )
    .await
}

/// The (id, sort values) pairs an answer carries, in the order it carries them.
fn ordered(answer: &Answer) -> Vec<(String, Value)> {
    answer.body["hits"]["hits"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .map(|hit| {
            (
                hit["_id"].as_str().unwrap_or_default().to_string(),
                hit["sort"].clone(),
            )
        })
        .collect()
}

/// RULE: whole numbers are compared exactly and the document holding none carries the sentinel
/// its key and direction put it at.
#[tokio::test]
async fn a_numeric_key_orders_exactly_and_places_the_document_holding_none() {
    let emulator = Emulator::new();
    given_indexed(&emulator).await;

    // (case, the sort the request names, the (id, sort values) it must answer) — rows spanning
    // both directions, both places `missing` names, a value it states itself, and the
    // query-string form of the same key.
    let rows: [OrderRow; 5] = [
        (
            "ascending, with the document holding no value last by default",
            json!([{ "big": { "order": "asc" } }]),
            vec![
                ("d5", json!([NEGATIVE])),
                ("d2", json!([BELOW])),
                ("d1", json!([ABOVE])),
                ("d4", json!([HIGHEST])),
                ("d3", json!([GREATEST_LONG])),
            ],
        ),
        (
            "descending, with that document still last",
            json!([{ "big": { "order": "desc" } }]),
            vec![
                ("d4", json!([HIGHEST])),
                ("d1", json!([ABOVE])),
                ("d2", json!([BELOW])),
                ("d5", json!([NEGATIVE])),
                ("d3", json!([LEAST_LONG])),
            ],
        ),
        (
            "ascending with missing _first, which moves it to the front",
            json!([{ "big": { "order": "asc", "missing": "_first" } }]),
            vec![
                ("d3", json!([LEAST_LONG])),
                ("d5", json!([NEGATIVE])),
                ("d2", json!([BELOW])),
                ("d1", json!([ABOVE])),
                ("d4", json!([HIGHEST])),
            ],
        ),
        (
            "descending with missing _first, where the sentinel is the other end",
            json!([{ "big": { "order": "desc", "missing": "_first" } }]),
            vec![
                ("d3", json!([GREATEST_LONG])),
                ("d4", json!([HIGHEST])),
                ("d1", json!([ABOVE])),
                ("d2", json!([BELOW])),
                ("d5", json!([NEGATIVE])),
            ],
        ),
        (
            "a missing value the request states itself, which orders it as if held",
            json!([{ "big": { "order": "asc", "missing": 0 } }]),
            vec![
                ("d5", json!([NEGATIVE])),
                ("d3", json!([0])),
                ("d2", json!([BELOW])),
                ("d1", json!([ABOVE])),
                ("d4", json!([HIGHEST])),
            ],
        ),
    ];
    for (case, sort, expected) in rows {
        let answered = sorted(&emulator, sort.clone()).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected 200, got {} with {:?}",
            answered.status, answered.body
        );
        let answered = ordered(&answered);
        let expected: Vec<(String, Value)> = expected
            .into_iter()
            .map(|(id, sort)| (id.to_string(), sort))
            .collect();
        assert_eq!(
            answered, expected,
            "case {case:?}: sort {sort:?} returned (id, sort) {answered:?}, expected {expected:?}"
        );
    }
}

/// RULE (the same key, paged): the page is taken out of that one order, so paging through it
/// reports every document exactly once — which is what an exact comparison is for.
#[tokio::test]
async fn paging_through_the_exact_order_reports_each_document_once() {
    let emulator = Emulator::new();
    given_indexed(&emulator).await;

    let mut paged: Vec<String> = Vec::new();
    for from in 0..5 {
        let page = rest_prefixed(
            &emulator,
            DOMAIN,
            "POST",
            &format!("/{INDEX}/_search"),
            Some(json!({ "from": from, "size": 1, "sort": [{ "big": "asc" }] })),
        )
        .await;
        assert_eq!(
            page.status, 200,
            "case \"page {from}\": expected 200, got {} with {:?}",
            page.status, page.body
        );
        paged.extend(ordered(&page).into_iter().map(|(id, _)| id));
    }
    assert_eq!(
        paged,
        vec!["d5", "d2", "d1", "d4", "d3"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<String>>(),
        "case \"paging through one exact order\": expected each document exactly once in \
         order, got {paged:?}"
    );
}

/// RULE: a field holding SEVERAL values orders the document by the one its `mode` names, and
/// the value the hit carries is that one.
#[tokio::test]
async fn a_multi_valued_key_orders_by_the_value_its_mode_names() {
    let emulator = Emulator::new();
    given_indexed(&emulator).await;

    // (case, the mode the key names, the (id, value) each document is ordered by) — the
    // documents hold [2,4], [3], none, [1,9] and [6,6,7], so each mode picks a different
    // value and a different order out of the same documents. A mode over whole numbers
    // answers a whole number, the mean rounded to the nearest with a half going up.
    let rows: [ModeRow; 6] = [
        (
            "no mode at all, which reads the smallest ascending",
            None,
            vec![("d4", 1), ("d1", 2), ("d2", 3), ("d5", 6)],
        ),
        (
            "min",
            Some("min"),
            vec![("d4", 1), ("d1", 2), ("d2", 3), ("d5", 6)],
        ),
        (
            "max",
            Some("max"),
            vec![("d2", 3), ("d1", 4), ("d5", 7), ("d4", 9)],
        ),
        (
            "sum",
            Some("sum"),
            vec![("d2", 3), ("d1", 6), ("d4", 10), ("d5", 19)],
        ),
        (
            "avg, rounded to a whole number",
            Some("avg"),
            vec![("d2", 3), ("d1", 3), ("d4", 5), ("d5", 6)],
        ),
        (
            "median",
            Some("median"),
            vec![("d2", 3), ("d1", 3), ("d4", 5), ("d5", 6)],
        ),
    ];
    for (case, mode, expected) in rows {
        let mut key = json!({ "order": "asc" });
        if let (Some(mode), Some(members)) = (mode, key.as_object_mut()) {
            members.insert("mode".to_string(), json!(mode));
        }
        // The name breaks a tie between two documents the mode gives one value, so one set of
        // documents reports one order on every read.
        let sort = json!([{ "scores": key }, { "name": "asc" }]);
        let answered = sorted(&emulator, sort.clone()).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected 200, got {} with {:?}",
            answered.status, answered.body
        );
        let answered = ordered(&answered);
        // The document holding no value for the key is last whatever the mode, carrying the
        // sentinel of a key whose values are longs.
        let mut wanted: Vec<(String, Value)> = expected
            .into_iter()
            .map(|(id, value)| (id.to_string(), json!([value])))
            .collect();
        for entry in &mut wanted {
            let name = match entry.0.as_str() {
                "d1" => "delta",
                "d2" => "alpha",
                "d4" => "bravo",
                _ => "charlie",
            };
            entry.1 = json!([entry.1[0].clone(), name]);
        }
        wanted.push(("d3".to_string(), json!([GREATEST_LONG, "echo"])));
        assert_eq!(
            answered, wanted,
            "case {case:?}: sort {sort:?} returned (id, sort) {answered:?}, expected {wanted:?}"
        );
    }
}

/// RULE (the other half of the sentinel rule): a key whose values are NOT longs has no such
/// sentinel — the document holding none carries no value at all — and the `missing` option
/// still decides where it sorts.
#[tokio::test]
async fn a_keyword_key_places_the_document_holding_none_without_a_sentinel() {
    let emulator = Emulator::new();
    given_indexed(&emulator).await;

    // (case, the sort, the ids in order, the sort values the document holding none carries)
    let rows: [(&str, Value, Vec<&str>, Value); 3] = [
        (
            "ascending, last by default",
            json!([{ "label": { "order": "asc" } }]),
            vec!["d5", "d1", "d4", "d2", "d3"],
            Value::Null,
        ),
        (
            "descending, still last",
            json!([{ "label": { "order": "desc" } }]),
            vec!["d2", "d4", "d1", "d5", "d3"],
            Value::Null,
        ),
        (
            "missing _first, which moves it to the front",
            json!([{ "label": { "order": "asc", "missing": "_first" } }]),
            vec!["d3", "d5", "d1", "d4", "d2"],
            Value::Null,
        ),
    ];
    for (case, sort, expected, missing) in rows {
        let answered = sorted(&emulator, sort.clone()).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected 200, got {} with {:?}",
            answered.status, answered.body
        );
        let answered = ordered(&answered);
        let ids: Vec<String> = answered.iter().map(|(id, _)| id.clone()).collect();
        assert_eq!(
            ids, expected,
            "case {case:?}: sort {sort:?} returned the hits {ids:?}, expected {expected:?}"
        );
        let carried = answered
            .iter()
            .find(|(id, _)| id == "d3")
            .map(|(_, sort)| sort[0].clone())
            .unwrap_or(Value::Null);
        assert_eq!(
            carried, missing,
            "case {case:?}: expected the document holding no value to carry {missing:?}, got \
             {carried:?}"
        );
    }
}
