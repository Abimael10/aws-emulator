// SPDX-License-Identifier: CC0-1.0

//! What the Places read plane REPORTS about a place and about the limit it served
//! an answer under, and which request field it names when a place identifier
//! cannot be read.
//!
//! Three rules, each over a table that spans what it generalises over:
//!
//! * a search summary's `MaxResults` is the limit the answer was served under —
//!   the request's own value when it names one, the member's documented default
//!   when it names none (`SearchPlaceIndexForTextRequest$MaxResults`: "The
//!   default: 50"), never absent — so the rows run the published range's two
//!   bounds, a value between them, no limit at all, and a query that matches
//!   nothing, plus the values one step outside the range, which are refused;
//! * every `Place` reports the Amazon Location categories that describe it
//!   (`Place$Categories`, `PlaceCategoryList` `len 1..10`), at whatever
//!   granularity the query geocodes to, and a place read reports the same
//!   categories the search that minted the identifier did;
//! * an identifier the index's provider never minted is refused naming the
//!   REQUEST's own field, with the modelled member quoted in the sentence.
//!
//! Every call goes through the shared harness (`crate::harness`), so each one is a
//! separate HTTP request against one shared engine.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, emulator, encode, Answer};

/// The documented default page size of a text search: the
/// `SearchPlaceIndexForTextRequest$MaxResults` doc, "The default: 50".
const DEFAULT_LIMIT: i64 = 50;

/// The published bounds of a search's result-count limit: the modelled
/// `PlaceIndexSearchResultLimit`, `range 1..50`.
const LIMIT_BOUNDS: (i64, i64) = (1, DEFAULT_LIMIT);

/// The sentence an identifier the index's own provider never minted is refused
/// with: fixed text quoting the MODELLED member name.
const INVALID_PLACE_ID: &str = "'PlaceId' must be a valid ID.";

/// The field name that refusal reports — the request's own path parameter, which
/// is not the modelled member spelling the sentence quotes.
const INVALID_PLACE_ID_FIELD: &str = "placeID";

/// The `ValidationExceptionReason` a refusal about a request field carries.
const FIELD_VALIDATION_FAILED: &str = "FieldValidationFailed";

/// Create the place index `name` against `data_source`.
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
        &format!("/places/v0/indexes/{index}/places/{}", encode(place_id)),
        None,
    )
    .await
}

/// A served answer's body, asserting it was served at all.
fn served(case: &str, answer: &Answer) -> Value {
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected the call to be served, got {} {:?}",
        answer.status, answer.body
    );
    answer.json()
}

/// A served search's results.
fn results(case: &str, answer: &Answer) -> Vec<Value> {
    served(case, answer)
        .get("Results")
        .and_then(|results| results.as_array())
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: a search answers a Results list, got {:?}", answer.body))
}

/// The first result's `Place`.
fn first_place(case: &str, answer: &Answer) -> Value {
    results(case, answer)
        .first()
        .and_then(|result| result.get("Place").cloned())
        .unwrap_or_else(|| panic!("case {case:?}: expected the query to answer a place, got {:?}", answer.body))
}

/// A place's own name: the first component of its assembled `Label`.
fn name_of(case: &str, place: &Value) -> String {
    place
        .get("Label")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: every place carries a Label, got {place}"))
        .split(',')
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// A place's `Categories`, asserting the member is reported at all and that it
/// holds between one and ten published-looking values: `PlaceCategoryList` is
/// `len 1..10` and every value Amazon Location publishes is a PascalCase name
/// ending in `Type`.
fn categories_of(case: &str, place: &Value) -> Vec<String> {
    let listed = place
        .get("Categories")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!("case {case:?}: a place reports the categories that describe it, got {place}")
        })
        .clone();
    assert!(
        (1..=10).contains(&listed.len()),
        "case {case:?}: Categories is modelled len 1..10, got {} entries in {place}",
        listed.len()
    );
    listed
        .iter()
        .map(|category| {
            let text = category
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a category is a string, got {category}"))
                .to_string();
            assert!(
                text.ends_with("Type")
                    && text
                        .chars()
                        .next()
                        .is_some_and(|first| first.is_ascii_uppercase()),
                "case {case:?}: expected a published Amazon Location category name, got {text:?}"
            );
            text
        })
        .collect()
}

/// RULE: a search summary reports the result-count limit the answer was served
/// under — the request's own `MaxResults` when it names one, the member's
/// documented default when it names none — so a client reads the page size it got
/// instead of guessing it, and a value outside the published range is refused.
#[tokio::test]
async fn the_summary_reports_the_limit_the_answer_was_served_under() {
    let router = emulator();
    create_index(&router, "served-limit", "Here").await;

    // (case, the search body, the limit the summary must report)
    let (least, most) = LIMIT_BOUNDS;
    let rows: [(&str, Value, i64); 5] = [
        (
            "no limit at all",
            json!({ "Text": "tower" }),
            DEFAULT_LIMIT,
        ),
        (
            "the least limit the range allows",
            json!({ "Text": "tower", "MaxResults": least }),
            least,
        ),
        (
            "a limit between the bounds",
            json!({ "Text": "tower", "MaxResults": 7 }),
            7,
        ),
        (
            "the most the range allows",
            json!({ "Text": "tower", "MaxResults": most }),
            most,
        ),
        (
            "no limit, and nothing matched",
            json!({ "Text": "Zzzyzx Nowhere Boulevard" }),
            DEFAULT_LIMIT,
        ),
    ];

    for (case, body, reported) in rows {
        let answer = search_text(&router, "served-limit", body.clone()).await;
        let summary = served(case, &answer)
            .get("Summary")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: a search answers a Summary, got {:?}", answer.body));
        let served_limit = summary.get("MaxResults").and_then(Value::as_i64);
        assert_eq!(
            served_limit,
            Some(reported),
            "case {case:?}: expected the summary to report {reported}, got {served_limit:?} for {body}"
        );
        let answered = results(case, &answer).len() as i64;
        assert!(
            answered <= reported,
            "case {case:?}: expected at most {reported} results, got {answered}"
        );
    }

    // One step outside each published bound is refused by the modelled range
    // rather than served under a clamped limit.
    for outside in [least - 1, most + 1] {
        let case = format!("MaxResults {outside}");
        let refused =
            search_text(&router, "served-limit", json!({ "Text": "tower", "MaxResults": outside }))
                .await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected a 400 refusal, got {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            "ValidationException",
            "case {case:?}: got {:?} ({:?})",
            refused.error_code(),
            refused.body
        );
    }
}

/// RULE: every `Place` a search answers reports the Amazon Location categories
/// that describe it, whatever granularity the query geocodes to — a locality, a
/// point of interest, a plain street address — and the query is answered by the
/// place it NAMES rather than by a place inside it.
///
/// The expected lists are the published Amazon Location category names the dataset
/// carries; the source of the vocabulary is the `Place$Categories` member doc
/// ("The Amazon Location categories that describe this Place"), which points at
/// the developer guide's categories list.
#[tokio::test]
async fn every_place_reports_the_categories_that_describe_it() {
    let router = emulator();
    create_index(&router, "categorised", "Here").await;

    // (query, the place it names, the categories that place is described by)
    let rows: [(&str, &str, &[&str]); 8] = [
        // A locality: the query names a city, so the city itself is the answer.
        ("Seattle", "Seattle", &["MunicipalityType"]),
        // Another locality, in another country and another language.
        ("Berlin", "Berlin", &["MunicipalityType"]),
        // A point of interest, described by one category.
        ("Space Needle", "Space Needle", &["TouristAttractionType"]),
        // A point of interest described by two.
        (
            "Louvre Museum",
            "Louvre Museum",
            &["MuseumType", "TouristAttractionType"],
        ),
        // A kind of place the issue's own cluster never names.
        (
            "Seattle-Tacoma International Airport",
            "Seattle-Tacoma International Airport",
            &["AirportType"],
        ),
        (
            "Starbucks Reserve Roastery",
            "Starbucks Reserve Roastery",
            &["CoffeeShopType"],
        ),
        (
            "Table Mountain",
            "Table Mountain",
            &["ParkType", "TouristAttractionType"],
        ),
        // A plain street address: a record with no name of its own.
        (
            "2001 8th Avenue Seattle",
            "2001 8th Avenue",
            &["AddressType"],
        ),
    ];

    for (query, expected_name, expected_categories) in rows {
        let answer = search_text(&router, "categorised", json!({ "Text": query })).await;
        let place = first_place(query, &answer);
        assert_eq!(
            name_of(query, &place),
            expected_name,
            "case {query:?}: expected the query to name {expected_name:?}, got {place}"
        );
        let categories = categories_of(query, &place);
        assert_eq!(
            categories,
            expected_categories
                .iter()
                .map(|category| (*category).to_string())
                .collect::<Vec<String>>(),
            "case {query:?}: expected {expected_categories:?}, got {categories:?}"
        );

        // The rule is about every result, not only the first one.
        for result in results(query, &answer) {
            let other = result
                .get("Place")
                .cloned()
                .unwrap_or_else(|| panic!("case {query:?}: a result carries its Place, got {result}"));
            let listed = categories_of(query, &other);
            assert!(
                !listed.is_empty(),
                "case {query:?}: every answered place is described by a category, got {other}"
            );
        }
    }
}

/// RULE: a place read reports the same `Place` the search that minted the
/// identifier did, categories included — the documented way to re-read a place by
/// its `PlaceId`.
///
/// The rows are every provider that mints an identifier on a text search, each
/// read back through a SECOND index of that provider, so the answer comes from the
/// records rather than from anything the first call remembered.
#[tokio::test]
async fn a_place_read_reports_the_categories_the_search_answered() {
    let router = emulator();
    // Source: the `SearchForTextResult$PlaceId` doc — "the PlaceId is returned
    // only by place indexes that use HERE or Grab as a data provider".
    for provider in ["Grab", "Here"] {
        create_index(&router, &format!("minting-{provider}"), provider).await;
        create_index(&router, &format!("reading-{provider}"), provider).await;

        for query in ["Seattle", "Louvre Museum", "2001 8th Avenue Seattle"] {
            let case = format!("{query} through {provider}");
            let answer = search_text(&router, &format!("minting-{provider}"), json!({ "Text": query }))
                .await;
            let result = results(&case, &answer)
                .first()
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: expected a result, got {:?}", answer.body));
            let identifier = result
                .get("PlaceId")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: expected a PlaceId, got {result}"))
                .to_string();
            let searched = result
                .get("Place")
                .cloned()
                .expect("a result carries its Place");

            let read = get_place(&router, &format!("reading-{provider}"), &identifier).await;
            let place = served(&case, &read)
                .get("Place")
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: a read answers a Place, got {:?}", read.body));
            assert_eq!(
                place, searched,
                "case {case:?}: expected the place the search answered, got {place}"
            );
            assert_eq!(
                categories_of(&case, &place),
                categories_of(&case, &searched),
                "case {case:?}: a read reports the same categories the search did"
            );
        }
    }
}

/// RULE: a place read refusing an identifier its index cannot read names the
/// REQUEST's own field in `fieldList[].name` — the path parameter the read is
/// addressed by — while the sentence quotes the modelled member name; both halves
/// are fixed text for this refusal, so nothing of the rejected identifier appears
/// in either (`PlaceId` is `@sensitive`).
///
/// The rows span the forms an unreadable identifier takes: one of another
/// provider's identifiers, an identifier of the right shape no provider minted,
/// and identifiers of other shapes entirely.
#[tokio::test]
async fn an_unreadable_identifier_names_the_request_field() {
    let router = emulator();
    create_index(&router, "refusing", "Here").await;
    create_index(&router, "other-provider", "Grab").await;

    // An identifier that IS valid — for another provider's index.
    let minted = search_text(&router, "other-provider", json!({ "Text": "Seattle" })).await;
    let elsewhere = results("minted by Grab", &minted)
        .first()
        .and_then(|result| result.get("PlaceId").and_then(Value::as_str).map(str::to_string))
        .expect("a Grab index mints a PlaceId on a text search");

    let rows: [(&str, String); 4] = [
        ("another provider's identifier", elsewhere),
        (
            "an identifier of the right shape nothing minted",
            "0123456789abcdef0123456789abcdef".to_string(),
        ),
        ("text that is not an identifier", "not-an-identifier".to_string()),
        ("a single digit", "0".to_string()),
    ];

    for (case, identifier) in rows {
        let refused = get_place(&router, "refusing", &identifier).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected a 400 refusal, got {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            "ValidationException",
            "case {case:?}: got {:?} ({:?})",
            refused.error_code(),
            refused.body
        );
        let body = refused.json();
        assert_eq!(
            body.get("reason").and_then(Value::as_str),
            Some(FIELD_VALIDATION_FAILED),
            "case {case:?}: expected the field-validation reason, got {body}"
        );
        assert_eq!(
            refused.message(),
            INVALID_PLACE_ID,
            "case {case:?}: got {:?}",
            refused.message()
        );
        assert_eq!(
            body.get("fieldList").cloned().unwrap_or_default(),
            json!([{ "name": INVALID_PLACE_ID_FIELD, "message": INVALID_PLACE_ID }]),
            "case {case:?}: expected the request's own field name, got {body}"
        );
        assert!(
            !refused.body.contains(&identifier),
            "case {case:?}: a @sensitive identifier may not appear in the refusal, got {:?}",
            refused.body
        );
    }
}
