// SPDX-License-Identifier: CC0-1.0

//! What a `GetPersonalizedRanking` request whose `inputList` carries an entry that names
//! no item gets back.
//!
//! `InputList`'s member targets `ItemID`, whose only constraint is `length 0..256`
//! (`smithy_query(query="shape",shape="InputList,ItemID")`), so the empty string
//! satisfies every constraint an entry carries and reaches the handler. An empty string
//! identifies no item — the one reading this service gives a `UserID`/`ItemID`
//! (`crate::state::present`, which `GetRecommendations` applies to `userId`/`itemId` and
//! `GetActionRecommendations` to `userId`) — so such an entry is read as the entry not
//! being supplied: it names neither a catalog record nor an item to append, and the
//! ranking is exactly the one the same list WITHOUT that entry gets.
//!
//! The rows span what the rule generalizes over: where an entry that names no item sits
//! in the list, how many of them the list carries, what the rest of the list names (ids
//! the catalog holds, ids it never held, repeats, nothing at all), and whether the
//! request asks for metadata in recommendations or names a filter. The list LENGTH is a
//! fact about the REQUEST, measured before any entry is read, so the second suite pins
//! that an entry naming no item still counts towards the documented bound.

use serde_json::{json, Value};

use service_com_amazonaws_personalizeruntime_amazonpersonalizeruntime::state::{
    self as registry, Campaign, Item, RecipeType,
};

use crate::support::{
    assert_descending_and_bounded, entries, ids, items, recommendation_id, scores, Emulator,
    COLUMN_GENRE, PERSONALIZE_RANKING,
};

/// The `PERSONALIZED_RANKING` campaign this suite re-ranks against.
const CAMPAIGN_RANKING: &str =
    "arn:aws:personalize:us-east-1:000000000000:campaign/empty-entry-ranking";
/// The same, with metadata in recommendations enabled, so a request may ask for
/// `metadataColumns` — which is what puts a `metadata` member on every reported entry.
const CAMPAIGN_METADATA: &str =
    "arn:aws:personalize:us-east-1:000000000000:campaign/empty-entry-metadata";

/// `EXCLUDE ItemID WHERE Items.GENRE IN ("Comedy")` — an entry with no stored
/// attributes at all cannot be removed by it.
const FILTER_EXCLUDE_COMEDY: &str =
    "arn:aws:personalize:us-east-1:000000000000:filter/empty-entry-exclude-comedy";
/// `INCLUDE ItemID WHERE Items.GENRE IN ($GENRE)` — an entry with no stored attributes
/// cannot be kept by it.
const FILTER_INCLUDE_GENRE: &str =
    "arn:aws:personalize:us-east-1:000000000000:filter/empty-entry-include-genre";

/// How many entries the seeded Items catalog holds.
const CATALOG_SIZE: usize = 8;

/// Ids no seeded catalog holds: "If an item was not included in the training dataset,
/// the item is appended to the end of the reranked list"
/// (`GetPersonalizedRankingRequest$inputList`).
const UNKNOWN_ITEMS: [&str; 2] = ["no-such-item-a", "no-such-item-b"];

/// The user every row re-ranks for.
const USER: &str = "user-3";

/// Two calls may not report an entry's certainty differently, so scores are compared
/// exactly up to double-rounding noise.
const TOLERANCE: f64 = 1e-12;

/// The predicate a row's filter expression means over a listed id: `Some(item)` for an
/// id the catalog holds, `None` for one it never held, which carries no stored
/// attributes for a clause to read.
type Retains = fn(Option<&Item>) -> bool;

/// One re-ranking row.
struct Row {
    case: &'static str,
    campaign: &'static str,
    /// The `inputList` the row sends, entries that name no item included.
    sent: Vec<String>,
    /// Whether the row asks for `metadataColumns`.
    metadata: bool,
    filter: Option<&'static str>,
    filter_values: Value,
    retains: Retains,
}

/// An emulator holding the two campaigns and the two filters this suite reads.
fn seeded() -> Emulator {
    let emulator = Emulator::new();
    let engine = &*emulator.engine;

    assert!(
        registry::register_campaign(
            engine,
            Campaign::new(
                CAMPAIGN_RANKING,
                "empty-entry-ranking",
                RecipeType::PersonalizedRanking,
                items(CATALOG_SIZE),
            ),
        ),
        "the campaign seam must register the ranking campaign"
    );
    assert!(
        registry::register_campaign(
            engine,
            Campaign::new(
                CAMPAIGN_METADATA,
                "empty-entry-metadata",
                RecipeType::PersonalizedRanking,
                items(CATALOG_SIZE),
            )
            .with_metadata_enabled(),
        ),
        "the campaign seam must register the metadata-enabled ranking campaign"
    );

    for (arn, expression) in [
        (
            FILTER_EXCLUDE_COMEDY,
            "EXCLUDE ItemID WHERE Items.GENRE IN (\"Comedy\")",
        ),
        (
            FILTER_INCLUDE_GENRE,
            "INCLUDE ItemID WHERE Items.GENRE IN ($GENRE)",
        ),
    ] {
        assert_eq!(
            registry::register_filter(engine, arn, expression),
            Ok(true),
            "case {arn}: the filter seam must accept {expression:?}"
        );
    }
    emulator
}

/// Every id of the seeded Items catalog, in catalog order.
fn catalog_ids() -> Vec<String> {
    items(CATALOG_SIZE)
        .into_iter()
        .map(|item| item.item_id)
        .collect()
}

/// The stored `GENRE` of a seeded catalog entry.
fn genre(item: &Item) -> &str {
    item.attributes
        .get(COLUMN_GENRE)
        .map(String::as_str)
        .unwrap_or_default()
}

/// The entry that names no item: `ItemID`'s `length` bound starts at 0, so this is a
/// value the boundary admits.
fn names_no_item() -> String {
    String::new()
}

/// One `GetPersonalizedRanking` request body.
fn request(row: &Row, sent: &[String]) -> Value {
    let mut body = json!({
        "campaignArn": row.campaign,
        "userId": USER,
        "inputList": sent,
    });
    if let Some(filter_arn) = row.filter {
        body["filterArn"] = json!(filter_arn);
        body["filterValues"] = row.filter_values.clone();
    }
    if row.metadata {
        body["metadataColumns"] = json!({ "ITEMS": [COLUMN_GENRE] });
    }
    body
}

/// The ids of `sent` that name an item, in the order of their first occurrence and with
/// every repeat removed — the list a caller who left the entries naming no item out, and
/// de-duplicated the rest, would have sent.
fn names_an_item(sent: &[String]) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    for id in sent.iter().filter(|id| !id.is_empty()) {
        if !kept.iter().any(|seen| seen == id) {
            kept.push(id.clone());
        }
    }
    kept
}

/// The ids a row's ranking must hold, sorted: each distinct listed id that names an item
/// and that the row's filter keeps.
fn expected_ids(row: &Row) -> Vec<String> {
    let catalog = items(CATALOG_SIZE);
    let mut expected: Vec<String> = names_an_item(&row.sent)
        .into_iter()
        .filter(|id| (row.retains)(catalog.iter().find(|item| &item.item_id == id)))
        .collect();
    expected.sort();
    expected
}

fn rows() -> Vec<Row> {
    let every_id = catalog_ids();
    vec![
        Row {
            case: "an entry naming no item last, after one the catalog holds",
            campaign: CAMPAIGN_RANKING,
            sent: vec![every_id[1].clone(), names_no_item()],
            metadata: false,
            filter: None,
            filter_values: json!({}),
            retains: |_| true,
        },
        Row {
            case: "an entry naming no item first",
            campaign: CAMPAIGN_RANKING,
            sent: vec![names_no_item(), every_id[0].clone(), every_id[4].clone()],
            metadata: false,
            filter: None,
            filter_values: json!({}),
            retains: |_| true,
        },
        Row {
            case: "an entry naming no item between two the catalog holds",
            campaign: CAMPAIGN_RANKING,
            sent: vec![every_id[6].clone(), names_no_item(), every_id[2].clone()],
            metadata: false,
            filter: None,
            filter_values: json!({}),
            retains: |_| true,
        },
        Row {
            case: "three entries naming no item spread through the whole catalog",
            campaign: CAMPAIGN_RANKING,
            sent: {
                let mut sent = vec![names_no_item()];
                for (index, id) in every_id.iter().enumerate() {
                    sent.push(id.clone());
                    if index % 3 == 0 {
                        sent.push(names_no_item());
                    }
                }
                sent
            },
            metadata: false,
            filter: None,
            filter_values: json!({}),
            retains: |_| true,
        },
        Row {
            case: "an entry naming no item beside ids the catalog never held",
            campaign: CAMPAIGN_RANKING,
            sent: vec![
                UNKNOWN_ITEMS[0].to_string(),
                names_no_item(),
                every_id[3].clone(),
                UNKNOWN_ITEMS[1].to_string(),
            ],
            metadata: false,
            filter: None,
            filter_values: json!({}),
            retains: |_| true,
        },
        Row {
            case: "an entry naming no item repeated, beside a repeated catalog id",
            campaign: CAMPAIGN_RANKING,
            sent: vec![
                names_no_item(),
                every_id[5].clone(),
                names_no_item(),
                every_id[5].clone(),
                names_no_item(),
            ],
            metadata: false,
            filter: None,
            filter_values: json!({}),
            retains: |_| true,
        },
        Row {
            case: "a list of nothing but entries naming no item",
            campaign: CAMPAIGN_RANKING,
            sent: vec![names_no_item(), names_no_item()],
            metadata: false,
            filter: None,
            filter_values: json!({}),
            retains: |_| true,
        },
        Row {
            case: "one entry, naming no item",
            campaign: CAMPAIGN_RANKING,
            sent: vec![names_no_item()],
            metadata: false,
            filter: None,
            filter_values: json!({}),
            retains: |_| true,
        },
        Row {
            case: "an entry naming no item in a request asking for metadata",
            campaign: CAMPAIGN_METADATA,
            sent: vec![
                every_id[2].clone(),
                names_no_item(),
                UNKNOWN_ITEMS[0].to_string(),
            ],
            metadata: true,
            filter: None,
            filter_values: json!({}),
            retains: |_| true,
        },
        Row {
            case: "a list of nothing but entries naming no item, asking for metadata",
            campaign: CAMPAIGN_METADATA,
            sent: vec![names_no_item(), names_no_item(), names_no_item()],
            metadata: true,
            filter: None,
            filter_values: json!({}),
            retains: |_| true,
        },
        Row {
            case: "an entry naming no item under an EXCLUDE filter",
            campaign: CAMPAIGN_RANKING,
            sent: {
                let mut sent = every_id.clone();
                sent.insert(2, names_no_item());
                sent.push(names_no_item());
                sent
            },
            metadata: false,
            filter: Some(FILTER_EXCLUDE_COMEDY),
            filter_values: json!({}),
            // An entry the catalog never held stores no attributes, so an EXCLUDE clause
            // over an Items column cannot remove it.
            retains: |item| item.is_none_or(|item| genre(item) != "Comedy"),
        },
        Row {
            case: "an entry naming no item under an INCLUDE filter",
            campaign: CAMPAIGN_RANKING,
            sent: {
                let mut sent = vec![names_no_item()];
                sent.extend(every_id.iter().cloned());
                sent.push(names_no_item());
                sent
            },
            metadata: false,
            filter: Some(FILTER_INCLUDE_GENRE),
            filter_values: json!({ "GENRE": "Comedy,Drama" }),
            // An INCLUDE clause keeps only what its expression matches, which an entry
            // with no stored attributes never does.
            retains: |item| item.is_some_and(|item| matches!(genre(item), "Comedy" | "Drama")),
        },
        Row {
            case: "a list of nothing but entries naming no item, under an EXCLUDE filter",
            campaign: CAMPAIGN_RANKING,
            sent: vec![names_no_item(), names_no_item()],
            metadata: false,
            filter: Some(FILTER_EXCLUDE_COMEDY),
            filter_values: json!({}),
            retains: |item| item.is_none_or(|item| genre(item) != "Comedy"),
        },
    ]
}

#[tokio::test]
async fn an_input_list_entry_that_names_no_item_adds_nothing_to_the_ranking() {
    let emulator = seeded();

    for row in rows() {
        let case = row.case;
        let response = emulator
            .ok(PERSONALIZE_RANKING, request(&row, &row.sent))
            .await;

        // Nothing that identifies no item is reported: every entry of the ranking names
        // an item, and an entry naming nothing is not one the catalog ever held.
        let returned = ids(&response, "personalizedRanking", "itemId");
        assert!(
            returned.iter().all(|id| !id.is_empty()),
            "case {case}: no reported entry may name no item, got {returned:?}"
        );

        // The ranking holds each distinct listed id that names an item and that the
        // row's filter keeps, and nothing else.
        let mut sorted_returned = returned.clone();
        sorted_returned.sort();
        assert_eq!(
            sorted_returned,
            expected_ids(&row),
            "case {case}: the ranking must hold exactly the distinct listed ids that name \
             an item and the filter keeps, got {returned:?}"
        );

        // "Read as the entry not being supplied" is the whole rule: the same request with
        // those entries left out answers with the same entries, in the same order, with
        // the same scores and the same metadata.
        let without = emulator
            .ok(PERSONALIZE_RANKING, request(&row, &names_an_item(&row.sent)))
            .await;
        assert_eq!(
            entries(&response, "personalizedRanking"),
            entries(&without, "personalizedRanking"),
            "case {case}: an entry that names no item must change nothing about the \
             ranking, got {:?} against {:?}",
            entries(&response, "personalizedRanking"),
            entries(&without, "personalizedRanking")
        );

        let reported = scores(&response, "personalizedRanking");
        for (position, (score, expected)) in reported
            .iter()
            .zip(scores(&without, "personalizedRanking"))
            .enumerate()
        {
            assert!(
                (score - expected).abs() < TOLERANCE,
                "case {case}: position {position} must score {expected} whether or not the \
                 list carries an entry naming no item, got {score}"
            );
        }
        assert_descending_and_bounded(case, &reported);

        // A served call still mints a recommendation id, whatever the list named.
        recommendation_id(case, &response);
    }
}

#[tokio::test]
async fn an_entry_that_names_no_item_still_counts_towards_the_documented_list_bound() {
    // "If you are including metadata in recommendations, the maximum is 50. Otherwise,
    // the maximum is 500." (`GetPersonalizedRankingRequest$inputList`.) The length is a
    // fact about the REQUEST, settled before any entry is read, so it counts the entries
    // the caller sent — an entry that names no item among them. Both published maxima
    // are exercised AT the bound and one past it.
    let emulator = seeded();
    let every_id = catalog_ids();
    let listed = [every_id[0].clone(), every_id[7].clone()];

    let rows: [(&str, &str, bool, usize, bool); 6] = [
        (
            "entries naming no item fill the list to the maximum",
            CAMPAIGN_RANKING,
            false,
            registry::MAX_NUM_RESULTS,
            false,
        ),
        (
            "entries naming no item push the list one past the maximum",
            CAMPAIGN_RANKING,
            false,
            registry::MAX_NUM_RESULTS + 1,
            false,
        ),
        (
            "entries naming no item fill a metadata request to its maximum",
            CAMPAIGN_METADATA,
            true,
            registry::MAX_NUM_RESULTS_WITH_METADATA,
            false,
        ),
        (
            "entries naming no item push a metadata request one past its maximum",
            CAMPAIGN_METADATA,
            true,
            registry::MAX_NUM_RESULTS_WITH_METADATA + 1,
            false,
        ),
        (
            "a list of nothing but entries naming no item, one past the maximum",
            CAMPAIGN_RANKING,
            false,
            registry::MAX_NUM_RESULTS + 1,
            true,
        ),
        (
            "a list of nothing but entries naming no item, at the maximum",
            CAMPAIGN_RANKING,
            false,
            registry::MAX_NUM_RESULTS,
            true,
        ),
    ];

    for (case, campaign, metadata, occurrences, only_empty) in rows {
        let mut sent: Vec<String> = if only_empty {
            Vec::new()
        } else {
            listed.to_vec()
        };
        while sent.len() < occurrences {
            sent.push(names_no_item());
        }
        let row = Row {
            case,
            campaign,
            sent: sent.clone(),
            metadata,
            filter: None,
            filter_values: json!({}),
            retains: |_| true,
        };
        let max = if metadata {
            registry::MAX_NUM_RESULTS_WITH_METADATA
        } else {
            registry::MAX_NUM_RESULTS
        };

        if occurrences > max {
            let refusal = emulator
                .refusal(PERSONALIZE_RANKING, request(&row, &sent))
                .await;
            assert_eq!(
                (
                    refusal.status,
                    refusal.code.as_str(),
                    refusal.message.as_str()
                ),
                (
                    400,
                    "InvalidInputException",
                    format!("Length of 'inputList' cannot be greater than {max}").as_str()
                ),
                "case {case}: {occurrences} entries are past the bound of {max}, so the \
                 request is refused for its length, got {refusal:?}"
            );
            continue;
        }

        let response = emulator.ok(PERSONALIZE_RANKING, request(&row, &sent)).await;
        let returned = ids(&response, "personalizedRanking", "itemId");
        let expected: Vec<String> = if only_empty {
            Vec::new()
        } else {
            let mut expected = listed.to_vec();
            expected.sort();
            expected
        };
        let mut sorted_returned = returned.clone();
        sorted_returned.sort();
        assert_eq!(
            sorted_returned, expected,
            "case {case}: a list inside its bound is served, reporting only the entries \
             that name an item, got {returned:?}"
        );
        assert_descending_and_bounded(case, &scores(&response, "personalizedRanking"));
    }
}
