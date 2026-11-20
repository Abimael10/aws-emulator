// SPDX-License-Identifier: CC0-1.0

//! The Places plane of this service, end to end over the wire:
//! `SearchPlaceIndexForText` geocoding free-form text against the data provider a
//! place index was created with, and `GetPlace` resolving the identifiers that
//! search hands out.
//!
//! Every call goes through the shared harness (`crate::harness`), so each one is a
//! separate HTTP request against one shared engine: a place index created by one
//! call is searchable by the next because it was PERSISTED, and an identifier
//! resolves through a LATER request because it is derived from the place and the
//! provider rather than remembered per call.
//!
//! The tables span what each rule generalises over — the forms a query can take
//! (a point-of-interest name, a street address, a locality, a postal code, a name
//! in another language, a query that names nothing), every optional filter and
//! several values of each, the whole documented provider set, the result-count
//! limit at its bounds, and the request shapes the two mutually exclusive
//! members can make. The recorded call of the issue report — a search on one index
//! and a `GetPlace` through another index of the same provider — is one row of the
//! identifier-scope table.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, emulator, encode, Answer};

/// The geospatial data providers a place index may be created with, written out
/// here rather than read from the crate: the source is the
/// `CreatePlaceIndexRequest$DataSource` member doc ("Valid values include: Esri …
/// Grab … Here").
const PROVIDERS: [&str; 3] = ["Esri", "Grab", "Here"];

/// The providers whose place indexes report a `PlaceId` on a TEXT search. The
/// source is the `SearchForTextResult$PlaceId` doc: "For
/// `SearchPlaceIndexForText` operations, the `PlaceId` is returned only by place
/// indexes that use HERE or Grab as a data provider."
const PROVIDERS_MINTING_TEXT_SEARCH_IDS: [&str; 2] = ["Grab", "Here"];

/// The sentence a search naming both a bias position and a bounding box is refused
/// with, written out as a fixed string: the two member names in single quotes and
/// the rule they broke.
const MUTUALLY_EXCLUSIVE: &str = "'BiasPosition' and 'FilterBBox' are mutually exclusive.";

/// The sentence an identifier the index's own provider never minted is refused
/// with.
const INVALID_PLACE_ID: &str = "'PlaceId' must be a valid ID.";

/// Create the place index `name` against `data_source`, asserting the create was
/// served — the step every case here starts from.
async fn create_index(router: &MultiServiceRouter, name: &str, data_source: &str) {
    let created = call(
        router,
        "POST",
        "/places/v0/indexes",
        Some(json!({ "IndexName": name, "DataSource": data_source })),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {name:?}/{data_source:?}: the place index must be created, got {} {:?}",
        created.status, created.body
    );
}

/// One text search against `index`.
async fn search_text(router: &MultiServiceRouter, index: &str, body: Value) -> Answer {
    call(
        router,
        "POST",
        &format!("/places/v0/indexes/{index}/search/text"),
        Some(body),
    )
    .await
}

/// One `GetPlace` against `index`, with the identifier in the URI label the way an
/// SDK puts it there.
async fn get_place(router: &MultiServiceRouter, index: &str, place_id: &str) -> Answer {
    call(
        router,
        "GET",
        &format!(
            "/places/v0/indexes/{index}/places/{}",
            encode(place_id)
        ),
        None,
    )
    .await
}

/// A served search's `Results`.
fn results(case: &str, answer: &Answer) -> Vec<Value> {
    assert_eq!(
        answer.status, 200,
        "case {case:?}: the search must be served, got {} {:?}",
        answer.status, answer.body
    );
    answer
        .json()
        .get("Results")
        .and_then(|results| results.as_array())
        .unwrap_or_else(|| panic!("case {case:?}: a search answers a Results list, got {:?}", answer.body))
        .clone()
}

/// A served search's `Summary`.
fn summary(case: &str, answer: &Answer) -> Value {
    answer
        .json()
        .get("Summary")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: a search answers a Summary, got {:?}", answer.body))
}

/// The name each result names its place by: the first component of the assembled
/// `Label`, which is the place's own name.
fn place_names(case: &str, answer: &Answer) -> Vec<String> {
    results(case, answer)
        .iter()
        .map(|result| {
            let label = label_of(case, result);
            label
                .split(',')
                .next()
                .unwrap_or_default()
                .trim()
                .to_string()
        })
        .collect()
}

/// One result's `Place.Label`.
fn label_of(case: &str, result: &Value) -> String {
    result
        .pointer("/Place/Label")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: every result carries a Place.Label, got {result}"))
        .to_string()
}

/// One result's `Place.Geometry.Point`, as `[longitude, latitude]`.
fn point_of(case: &str, result: &Value) -> [f64; 2] {
    let point = result
        .pointer("/Place/Geometry/Point")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!("case {case:?}: every result carries a Place.Geometry.Point, got {result}")
        });
    assert_eq!(
        point.len(),
        2,
        "case {case:?}: a Point is [longitude, latitude], got {point:?}"
    );
    [
        point[0].as_f64().expect("longitude is a number"),
        point[1].as_f64().expect("latitude is a number"),
    ]
}

/// One result's `Relevance`.
fn relevance_of(case: &str, result: &Value) -> f64 {
    result
        .get("Relevance")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("case {case:?}: every result carries a Relevance, got {result}"))
}

/// Assert the invariants EVERY served search answers with, whatever the query:
/// relevance runs from highest to lowest and stays inside the modelled range, and
/// the summary's `ResultBBox` fully contains the results it was answered with.
fn assert_search_invariants(case: &str, answer: &Answer) {
    let results = results(case, answer);
    let mut previous = f64::INFINITY;
    for result in &results {
        let relevance = relevance_of(case, result);
        assert!(
            relevance > 0.0 && relevance <= 1.0,
            "case {case:?}: Relevance is modelled range 0..1, got {relevance}"
        );
        assert!(
            relevance <= previous,
            "case {case:?}: results run from highest to lowest relevance, got {relevance} after {previous}"
        );
        previous = relevance;
    }

    let summary = summary(case, answer);
    let result_box = summary.get("ResultBBox").and_then(Value::as_array);
    match result_box {
        None => assert!(
            results.is_empty(),
            "case {case:?}: a search with results reports the box containing them, got {summary}"
        ),
        Some(bounds) => {
            assert_eq!(
                bounds.len(),
                4,
                "case {case:?}: ResultBBox is [west, south, east, north], got {bounds:?}"
            );
            let corners: Vec<f64> = bounds
                .iter()
                .map(|value| value.as_f64().expect("a bound is a number"))
                .collect();
            for result in &results {
                let [longitude, latitude] = point_of(case, result);
                assert!(
                    longitude >= corners[0]
                        && longitude <= corners[2]
                        && latitude >= corners[1]
                        && latitude <= corners[3],
                    "case {case:?}: ResultBBox {corners:?} must contain the result at {:?}",
                    [longitude, latitude]
                );
            }
        }
    }
}

/// RULE: a text search geocodes the request's free-form text against the provider
/// dataset and answers the places it names, with a summary that echoes the search
/// text and reports the provider of the index that served it.
///
/// The rows span the forms free-form text takes — a point-of-interest name, a
/// street address, a locality, a postal code, a name written in another language,
/// a category word, and text that names no place at all.
#[tokio::test]
async fn a_text_search_answers_the_places_the_query_names() {
    let router = emulator();
    create_index(&router, "text-search", "Here").await;

    // (query, the place the query names, whether anything else may be answered too)
    let rows: [(&str, Option<&str>); 10] = [
        ("Space Needle", Some("Space Needle")),
        ("400 Broad Street Seattle", Some("Space Needle")),
        ("Pike Place Market, Seattle", Some("Pike Place Market")),
        ("105-0011", Some("Tokyo Tower")),
        ("M5V 3L9", Some("CN Tower")),
        ("Tour Eiffel", Some("Eiffel Tower")),
        ("東京タワー", Some("Tokyo Tower")),
        ("coffee", Some("Starbucks Reserve Roastery")),
        ("Zzzyzx Nowhere Boulevard", None),
        ("Broad Street Zzzyzx Qqqq Wwww", None),
    ];

    for (query, expected) in rows {
        let answer = search_text(&router, "text-search", json!({ "Text": query })).await;
        assert_search_invariants(query, &answer);
        let names = place_names(query, &answer);
        match expected {
            Some(name) => assert_eq!(
                names.first().map(String::as_str),
                Some(name),
                "case {query:?}: expected {name:?} first, got {names:?}"
            ),
            None => assert!(
                names.is_empty(),
                "case {query:?}: text that names no place answers no results, got {names:?}"
            ),
        }

        let summary = summary(query, &answer);
        assert_eq!(
            summary.get("Text").and_then(Value::as_str),
            Some(query),
            "case {query:?}: the summary echoes the search text, got {summary}"
        );
        assert_eq!(
            summary.get("DataSource").and_then(Value::as_str),
            Some("Here"),
            "case {query:?}: the summary reports the index's own provider, got {summary}"
        );
        for result in results(query, &answer) {
            assert!(
                result.get("Distance").is_none(),
                "case {query:?}: Distance is answered only for a biased search, got {result}"
            );
        }
    }
}

/// RULE: a result's relevance is the share of the query the place accounted for,
/// so a query every term of which names something about the place scores 1 and one
/// that also names something else scores lower.
///
/// The rows span a one-term query, a two-term query and a fully spelled-out
/// address query, each with a place that accounted for all of it and a place that
/// accounted for part.
#[tokio::test]
async fn relevance_is_the_share_of_the_query_a_place_accounted_for() {
    let router = emulator();
    create_index(&router, "relevance", "Esri").await;

    // (query, the place that accounts for all of it, a place that accounts for part)
    let rows: [(&str, &str, &str); 3] = [
        ("Tokyo Tower", "Tokyo Tower", "CN Tower"),
        ("Eiffel Tower Paris France", "Eiffel Tower", "Louvre Museum"),
        ("Pike Street Seattle 98101", "Pike Place Market", "Space Needle"),
    ];

    for (query, exact, partial) in rows {
        let answer = search_text(&router, "relevance", json!({ "Text": query })).await;
        assert_search_invariants(query, &answer);
        let served = results(query, &answer);
        let scored: Vec<(String, f64)> = served
            .iter()
            .map(|result| {
                let name = label_of(query, result)
                    .split(',')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                (name, relevance_of(query, result))
            })
            .collect();

        let exact_score = scored
            .iter()
            .find(|(name, _)| name == exact)
            .map(|(_, score)| *score)
            .unwrap_or_else(|| panic!("case {query:?}: expected {exact:?} among {scored:?}"));
        assert_eq!(
            exact_score, 1.0,
            "case {query:?}: {exact:?} accounts for the whole query, got {exact_score} in {scored:?}"
        );

        let partial_score = scored
            .iter()
            .find(|(name, _)| name == partial)
            .map(|(_, score)| *score)
            .unwrap_or_else(|| panic!("case {query:?}: expected {partial:?} among {scored:?}"));
        assert!(
            partial_score < 1.0,
            "case {query:?}: {partial:?} accounts for part of the query, got {partial_score} in {scored:?}"
        );
    }
}

/// RULE: each optional filter LIMITS the results to the places that satisfy it —
/// a country in the list, a position inside the bounding box, any one of the
/// categories — and a filter no place satisfies narrows the answer to nothing.
///
/// Every row searches the same query, so what differs between rows is only the
/// filter: its member, its value count, and whether anything satisfies it. The
/// query matches four places in four countries, which is what makes each
/// narrowing visible.
#[tokio::test]
async fn every_filter_narrows_the_results_it_is_given() {
    let router = emulator();
    create_index(&router, "filters", "Here").await;

    // (case, the filter members, the places that survive it)
    let rows: [(&str, Value, &[&str]); 9] = [
        ("no filter", json!({}), &["CN Tower", "Eiffel Tower", "Tokyo Tower", "Tower Bridge"]),
        ("one country", json!({ "FilterCountries": ["JPN"] }), &["Tokyo Tower"]),
        (
            "two countries",
            json!({ "FilterCountries": ["FRA", "GBR"] }),
            &["Eiffel Tower", "Tower Bridge"],
        ),
        (
            "a country no match is in",
            json!({ "FilterCountries": ["BRA"] }),
            &[],
        ),
        (
            "a box over western Europe",
            json!({ "FilterBBox": [-10.0, 35.0, 30.0, 60.0] }),
            &["Eiffel Tower", "Tower Bridge"],
        ),
        (
            "a box over one city",
            json!({ "FilterBBox": [139.0, 35.0, 140.0, 36.0] }),
            &["Tokyo Tower"],
        ),
        (
            "a box over open ocean",
            json!({ "FilterBBox": [-30.0, -40.0, -20.0, -30.0] }),
            &[],
        ),
        (
            "one category",
            json!({ "FilterCategories": ["monument"] }),
            &["Eiffel Tower"],
        ),
        (
            "a category no place carries",
            json!({ "FilterCategories": ["not_a_published_category"] }),
            &[],
        ),
    ];

    for (case, filter, expected) in rows {
        let mut body = json!({ "Text": "tower" });
        for (member, value) in filter.as_object().expect("the row's filter is an object") {
            body[member] = value.clone();
        }
        let answer = search_text(&router, "filters", body.clone()).await;
        assert_search_invariants(case, &answer);

        let mut names = place_names(case, &answer);
        names.sort();
        let mut wanted: Vec<String> = expected.iter().map(|name| (*name).to_string()).collect();
        wanted.sort();
        assert_eq!(
            names, wanted,
            "case {case:?}: expected {wanted:?}, got {names:?} for {body}"
        );

        // The summary echoes the filter the request carried, member for member.
        let summary = summary(case, &answer);
        for (member, value) in filter.as_object().expect("the row's filter is an object") {
            assert_eq!(
                summary.get(member),
                Some(value),
                "case {case:?}: the summary echoes {member}, got {summary}"
            );
        }
    }
}

/// RULE: `MaxResults` caps how many of the matched places are answered, and the
/// summary echoes the limit only when the request named one; an absent limit
/// answers every match up to the documented default of 50.
///
/// The rows run the limit from its modelled minimum through the match count to its
/// modelled maximum, plus the request that names no limit at all.
#[tokio::test]
async fn max_results_caps_the_answer() {
    let router = emulator();
    create_index(&router, "limits", "Here").await;

    // The query matches these four places, so a limit below four is visible as a
    // shorter answer and one above it changes nothing.
    let matched = 4;
    let rows: [(Option<i64>, usize); 6] = [
        (Some(1), 1),
        (Some(2), 2),
        (Some(3), 3),
        (Some(4), matched),
        (Some(50), matched),
        (None, matched),
    ];

    for (limit, expected) in rows {
        let case = format!("MaxResults {limit:?}");
        let body = match limit {
            Some(most) => json!({ "Text": "tower", "MaxResults": most }),
            None => json!({ "Text": "tower" }),
        };
        let answer = search_text(&router, "limits", body).await;
        assert_search_invariants(&case, &answer);
        let names = place_names(&case, &answer);
        assert_eq!(
            names.len(),
            expected,
            "case {case:?}: expected {expected} results, got {names:?}"
        );
        assert_eq!(
            summary(&case, &answer).get("MaxResults").and_then(Value::as_i64),
            limit,
            "case {case:?}: the summary echoes the limit the request named"
        );
    }
}

/// RULE: a bias position makes every result report the great-circle distance in
/// metres between that position and the place, and orders places the query
/// matched equally well nearest first.
///
/// The rows bias the same query toward three of its own matches, so each row
/// expects a different place first; the distance assertion is checked against the
/// real-world great-circle distance between two of them.
#[tokio::test]
async fn a_bias_position_reports_distance_and_prefers_nearer_places() {
    let router = emulator();
    create_index(&router, "bias", "Here").await;

    // (case, the bias position, the place it is at)
    let rows: [(&str, [f64; 2], &str); 3] = [
        ("at Tokyo Tower", [139.7454, 35.6586], "Tokyo Tower"),
        ("at Tower Bridge", [-0.0754, 51.5055], "Tower Bridge"),
        ("at the CN Tower", [-79.3871, 43.6426], "CN Tower"),
    ];

    for (case, bias, nearest) in rows {
        let answer = search_text(
            &router,
            "bias",
            json!({ "Text": "tower", "BiasPosition": bias }),
        )
        .await;
        assert_search_invariants(case, &answer);
        let names = place_names(case, &answer);
        assert_eq!(
            names.first().map(String::as_str),
            Some(nearest),
            "case {case:?}: the nearest equally relevant place comes first, got {names:?}"
        );

        let served = results(case, &answer);
        let mut previous = 0.0_f64;
        for result in &served {
            let distance = result
                .get("Distance")
                .and_then(Value::as_f64)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: a biased search reports Distance, got {result}")
                });
            assert!(
                distance >= previous,
                "case {case:?}: equally relevant results run nearest first, got {distance} after {previous}"
            );
            previous = distance;
        }
        let first = served.first().expect("the query matched places");
        let at_bias = first
            .get("Distance")
            .and_then(Value::as_f64)
            .expect("a biased search reports Distance");
        assert!(
            at_bias < 100.0,
            "case {case:?}: the place AT the bias position is metres away, got {at_bias}"
        );
        assert_eq!(
            summary(case, &answer).get("BiasPosition"),
            Some(&json!(bias)),
            "case {case:?}: the summary echoes the bias position"
        );
    }

    // The arc is the real one: Tokyo Tower to the Eiffel Tower is about 9 713 km
    // of great-circle distance, so the metres reported for the far match are that
    // arc rather than a number of the emulator's own.
    let answer = search_text(
        &router,
        "bias",
        json!({ "Text": "tower", "BiasPosition": [139.7454, 35.6586] }),
    )
    .await;
    let eiffel = results("Tokyo to Paris", &answer)
        .into_iter()
        .find(|result| label_of("Tokyo to Paris", result).starts_with("Eiffel Tower"))
        .expect("the query matched the Eiffel Tower");
    let metres = eiffel
        .get("Distance")
        .and_then(Value::as_f64)
        .expect("a biased search reports Distance");
    assert!(
        (9_400_000.0..10_000_000.0).contains(&metres),
        "case \"Tokyo to Paris\": expected about 9 713 km of great-circle arc, got {metres} m"
    );
}

/// RULE: the two members that each say WHERE to look cannot both be given, and a
/// coordinate outside the WGS 84 domain or a box whose corners are the wrong way
/// round is refused — each as the modelled 400 `ValidationException` naming the
/// member at fault.
///
/// The rows span both mutually exclusive members together, each member's own
/// domain, both axes of that domain and both corner orders.
#[tokio::test]
async fn a_search_that_cannot_hold_together_is_refused() {
    let router = emulator();
    create_index(&router, "refusals", "Here").await;

    // (case, the request body, the member named, the sentence)
    let rows: [(&str, Value, &str, &str); 6] = [
        (
            "both ways of saying where",
            json!({
                "Text": "tower",
                "BiasPosition": [2.2945, 48.8584],
                "FilterBBox": [-10.0, 35.0, 30.0, 60.0]
            }),
            "BiasPosition",
            MUTUALLY_EXCLUSIVE,
        ),
        (
            "a bias longitude past the meridian bound",
            json!({ "Text": "tower", "BiasPosition": [180.5, 48.8584] }),
            "BiasPosition",
            "longitude must be between -180 and 180 degrees",
        ),
        (
            "a bias latitude past the pole",
            json!({ "Text": "tower", "BiasPosition": [2.2945, 90.5] }),
            "BiasPosition",
            "latitude must be between -90 and 90 degrees",
        ),
        (
            "a box corner outside the domain",
            json!({ "Text": "tower", "FilterBBox": [-10.0, 35.0, 181.0, 60.0] }),
            "FilterBBox",
            "longitude must be between -180 and 180 degrees",
        ),
        (
            "a box running east to west",
            json!({ "Text": "tower", "FilterBBox": [30.0, 35.0, -10.0, 60.0] }),
            "FilterBBox",
            "'FilterBBox' must give the southwest corner before the northeast corner.",
        ),
        (
            "a box running north to south",
            json!({ "Text": "tower", "FilterBBox": [-10.0, 60.0, 30.0, 35.0] }),
            "FilterBBox",
            "'FilterBBox' must give the southwest corner before the northeast corner.",
        ),
    ];

    for (case, body, member, sentence) in rows {
        let refused = search_text(&router, "refusals", body.clone()).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected a 400 refusal, got {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {:?} ({:?})",
            refused.error_code(),
            refused.body
        );
        assert_eq!(
            refused.message(),
            sentence,
            "case {case:?}: expected {sentence:?}, got {:?}",
            refused.message()
        );
        let field_list = refused.json().get("fieldList").cloned().unwrap_or_default();
        assert_eq!(
            field_list,
            json!([{ "name": member, "message": sentence }]),
            "case {case:?}: the refusal names the member at fault, got {field_list}"
        );
    }
}

/// RULE: a Places call against a place index the account does not hold is refused
/// as a missing resource, whichever of the two operations it is and whether or not
/// anything else about the request is resolvable.
#[tokio::test]
async fn a_call_on_an_unheld_place_index_is_not_found() {
    let router = emulator();
    create_index(&router, "held", "Here").await;
    let identifier = {
        let answer = search_text(&router, "held", json!({ "Text": "Space Needle" })).await;
        results("minting", &answer)[0]
            .get("PlaceId")
            .and_then(Value::as_str)
            .expect("a HERE index reports a PlaceId")
            .to_string()
    };

    let rows: [(&str, Answer); 2] = [
        (
            "a search",
            search_text(&router, "not-held", json!({ "Text": "Space Needle" })).await,
        ),
        (
            "a place read",
            get_place(&router, "not-held", &identifier).await,
        ),
    ];
    for (case, refused) in rows {
        assert_eq!(
            refused.status, 404,
            "case {case:?}: expected a 404 refusal, got {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {:?} ({:?})",
            refused.error_code(),
            refused.body
        );
        assert_eq!(
            refused.message(),
            "Place index not found: not-held.",
            "case {case:?}: got {:?}",
            refused.message()
        );
    }
}

/// RULE: which members a place reports is decided by the data provider of the
/// index that served it — the model says so member by member — while the place
/// itself (its name, its position, its address) is the same whichever provider
/// reports it.
///
/// The rows are every documented provider, read for two places that carry every
/// provider-dependent member between them.
#[tokio::test]
async fn the_provider_decides_which_members_a_place_reports() {
    let router = emulator();
    for provider in PROVIDERS {
        create_index(&router, &format!("by-{provider}"), provider).await;
    }

    // (query, the member, the providers that report it)
    // Sources: `Place$TimeZone` "Returned only when using HERE or Grab as the
    // selected partner"; `Place$UnitType` "Returned only for a place index that
    // uses Esri as a data provider"; `Place$UnitNumber` "Returned only for a place
    // index that uses Esri or Grab as a data provider"; `Place$SubMunicipality`
    // "This property supported by Esri and OpenData".
    let rows: [(&str, &str, &[&str]); 4] = [
        ("Marina Bay Sands", "TimeZone", &["Grab", "Here"]),
        ("Marina Bay Sands", "UnitType", &["Esri"]),
        ("Marina Bay Sands", "UnitNumber", &["Esri", "Grab"]),
        ("Empire State Building", "SubMunicipality", &["Esri"]),
    ];

    for (query, member, reporting) in rows {
        for provider in PROVIDERS {
            let case = format!("{query} on {provider}: {member}");
            let answer =
                search_text(&router, &format!("by-{provider}"), json!({ "Text": query })).await;
            let served = results(&case, &answer);
            let place = served
                .first()
                .map(|result| result.get("Place").cloned().unwrap_or_default())
                .unwrap_or_else(|| panic!("case {case:?}: expected the query to match a place"));
            assert_eq!(
                place.get(member).is_some(),
                reporting.contains(&provider),
                "case {case:?}: {member} is reported by {reporting:?} and no other provider, got {place}"
            );
        }
    }

    // The place itself does not change with the provider, and the provider-only
    // members carry the record's own values.
    let mut seen: Vec<(String, Value)> = Vec::new();
    for provider in PROVIDERS {
        let case = format!("Marina Bay Sands on {provider}");
        let answer = search_text(
            &router,
            &format!("by-{provider}"),
            json!({ "Text": "Marina Bay Sands" }),
        )
        .await;
        let place = results(&case, &answer)[0]
            .get("Place")
            .cloned()
            .expect("a result carries its Place");
        for member in ["Label", "Geometry", "Country", "Municipality", "PostalCode"] {
            if let Some((other, value)) = seen
                .iter()
                .find(|(other_member, _)| other_member == member)
                .map(|(_, value)| (provider, value.clone()))
            {
                assert_eq!(
                    place.get(member),
                    Some(&value),
                    "case {case:?}: {member} is the place's own, not the provider's (differs from {other})"
                );
            } else {
                seen.push((
                    member.to_string(),
                    place.get(member).cloned().unwrap_or_default(),
                ));
            }
        }
        if provider == "Here" || provider == "Grab" {
            assert_eq!(
                place.pointer("/TimeZone/Name").and_then(Value::as_str),
                Some("Asia/Singapore"),
                "case {case:?}: the time zone is the place's own, got {place}"
            );
            assert_eq!(
                place.pointer("/TimeZone/Offset").and_then(Value::as_i64),
                Some(28_800),
                "case {case:?}: the offset is that zone's, got {place}"
            );
        }
    }
}

/// RULE: a `PlaceId` is scoped to the data provider that minted it, not to the
/// index it was minted through — the `GetPlace` doc requires the account, the
/// Region and "Data provider specified in the place index resource" to be the
/// same, and nothing else — so a second index of the same provider resolves it to
/// the same place and an index of any other provider refuses it as an invalid
/// identifier.
///
/// The rows are every provider that mints an identifier on a text search, each
/// resolved through its own second index and refused by every other provider's.
#[tokio::test]
async fn a_place_id_resolves_through_any_index_of_the_same_provider() {
    let router = emulator();
    for provider in PROVIDERS {
        create_index(&router, &format!("first-{provider}"), provider).await;
        create_index(&router, &format!("second-{provider}"), provider).await;
    }

    for provider in PROVIDERS_MINTING_TEXT_SEARCH_IDS {
        let case = format!("minted by {provider}");
        let minted = search_text(
            &router,
            &format!("first-{provider}"),
            json!({ "Text": "Space Needle" }),
        )
        .await;
        let result = results(&case, &minted)[0].clone();
        let identifier = result
            .get("PlaceId")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected a PlaceId, got {result}"))
            .to_string();
        let searched_place = result.get("Place").cloned().expect("the result's Place");

        // The index it was minted through resolves it, and so does another index
        // of the same provider — to the very same place.
        for index in [format!("first-{provider}"), format!("second-{provider}")] {
            let read = get_place(&router, &index, &identifier).await;
            assert_eq!(
                read.status, 200,
                "case {case:?} through {index}: expected the place to resolve, got {} {:?}",
                read.status, read.body
            );
            assert_eq!(
                read.json().get("Place"),
                Some(&searched_place),
                "case {case:?} through {index}: expected the place the search answered, got {:?}",
                read.body
            );
        }

        // No other provider's index resolves it: the identifier is not one of
        // theirs, which is an invalid input rather than a place they cannot find.
        for other in PROVIDERS.iter().filter(|other| **other != provider) {
            let refused = get_place(&router, &format!("first-{other}"), &identifier).await;
            assert_eq!(
                refused.status, 400,
                "case {case:?} through a {other} index: expected a 400 refusal, got {} {:?}",
                refused.status, refused.body
            );
            assert_eq!(
                refused.error_code(),
                "ValidationException",
                "case {case:?} through a {other} index: got {:?} ({:?})",
                refused.error_code(),
                refused.body
            );
            assert_eq!(
                refused.message(),
                INVALID_PLACE_ID,
                "case {case:?} through a {other} index: got {:?}",
                refused.message()
            );
        }
    }

    // An identifier no provider ever minted is refused the same way, whatever
    // shape it takes.
    for invented in ["0123456789abcdef", "not-an-identifier", "0"] {
        let refused = get_place(&router, "first-Here", invented).await;
        assert_eq!(
            refused.status, 400,
            "case {invented:?}: expected a 400 refusal, got {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.message(),
            INVALID_PLACE_ID,
            "case {invented:?}: got {:?}",
            refused.message()
        );
    }
}

/// RULE: `Language` selects the language a place's name is REPORTED in, without
/// changing which places are found: a place is findable by every name it has, and
/// a language the place has no name in keeps the name it is written under.
///
/// The rows span a language the place has a name in, a regional variant of that
/// language, a language it has no name in, no language at all, and a query written
/// in another language than the answer is asked for.
#[tokio::test]
async fn language_selects_the_name_a_place_reports() {
    let router = emulator();
    create_index(&router, "languages", "Here").await;

    // (query, requested language, the name the place must be reported under)
    let rows: [(&str, Option<&str>, &str); 7] = [
        ("Eiffel Tower", None, "Eiffel Tower"),
        ("Eiffel Tower", Some("fr"), "Tour Eiffel"),
        ("Eiffel Tower", Some("fr-CA"), "Tour Eiffel"),
        ("Eiffel Tower", Some("de"), "Eiffel Tower"),
        ("Tour Eiffel", Some("en"), "Eiffel Tower"),
        ("Αθήνα", Some("el"), "Αθήνα"),
        ("Αθήνα", None, "Athens"),
    ];

    for (query, language, expected) in rows {
        let case = format!("{query:?} in {language:?}");
        let body = match language {
            Some(tag) => json!({ "Text": query, "Language": tag }),
            None => json!({ "Text": query }),
        };
        let answer = search_text(&router, "languages", body).await;
        assert_search_invariants(&case, &answer);
        let names = place_names(&case, &answer);
        assert_eq!(
            names.first().map(String::as_str),
            Some(expected),
            "case {case:?}: expected {expected:?} first, got {names:?}"
        );
        assert_eq!(
            summary(&case, &answer).get("Language").and_then(Value::as_str),
            language,
            "case {case:?}: the summary echoes the requested language"
        );
    }

    // A place read reports the name its own `Language` asks for, from the same
    // records.
    let minted = search_text(&router, "languages", json!({ "Text": "Eiffel Tower" })).await;
    let identifier = results("read in French", &minted)[0]
        .get("PlaceId")
        .and_then(Value::as_str)
        .expect("a HERE index reports a PlaceId")
        .to_string();
    let read = call(
        &router,
        "GET",
        &format!(
            "/places/v0/indexes/languages/places/{}?language=fr",
            encode(&identifier)
        ),
        None,
    )
    .await;
    assert_eq!(
        read.status, 200,
        "case \"read in French\": expected the place to resolve, got {} {:?}",
        read.status, read.body
    );
    assert_eq!(
        read.json()
            .pointer("/Place/Label")
            .and_then(Value::as_str)
            .map(|label| label.split(',').next().unwrap_or_default().to_string()),
        Some("Tour Eiffel".to_string()),
        "case \"read in French\": expected the French name, got {:?}",
        read.body
    );
}
