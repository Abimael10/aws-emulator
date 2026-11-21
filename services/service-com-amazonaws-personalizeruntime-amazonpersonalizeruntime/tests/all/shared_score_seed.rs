// SPDX-License-Identifier: CC0-1.0

//! ONE composition of the prediction-score seed, driven through all three inference
//! operations.
//!
//! A reported `score` is the entry's share of the certainty the service has for one
//! (resolved resource, personalization input, context, entry) tuple, so WHICH of those
//! inputs seed the score — and in which order — is a single rule the whole service
//! obeys, not a per-operation detail. The rule lives in
//! `crate::state::scoring::Seed`; these suites call that same helper from the test
//! side and compare its shares against what each operation actually reports, so a
//! handler that composed the tuple its own way would fail here even though the three
//! operations serve DISJOINT recipe types and no single request can reach two of them.
//!
//! The rows differ along what the rule generalizes over: which operation serves the
//! request, which resource it resolves (two resources over an identical catalog),
//! which personalization input that resource uses (`userId` or the `itemId` anchor of
//! a related-items campaign), whether a `context` is sent and how many keys it
//! carries, how much of the catalog the response reports, and whether the stored
//! catalog carries a popularity gradient or none at all.

use std::collections::{BTreeMap, HashMap};

use serde_json::{json, Value};

use service_com_amazonaws_personalizeruntime_amazonpersonalizeruntime::state::{
    self as registry, scoring, scoring::CatalogEntry, Action, Campaign, Entity, Item, RecipeType,
    RecordedInteractions,
};

use crate::support::{
    actions, assert_descending_and_bounded, assert_sums_to_one, ids, items, scores, scores_by_id,
    Emulator, ACTION_RECOMMENDATIONS, COLUMN_GENRE, PERSONALIZE_RANKING, RECOMMENDATIONS,
};

/// The `USER_PERSONALIZATION` campaign the `GetRecommendations` rows read.
const CAMPAIGN_USER: &str = "arn:aws:personalize:us-east-1:000000000000:campaign/seed-user";
/// A second `USER_PERSONALIZATION` campaign over an IDENTICAL catalog: the resolved
/// resource is a seed part, so the two score the same request differently.
const CAMPAIGN_USER_TWIN: &str =
    "arn:aws:personalize:us-east-1:000000000000:campaign/seed-user-twin";
/// The `RELATED_ITEMS` campaign whose personalization input is `itemId`.
const CAMPAIGN_RELATED: &str = "arn:aws:personalize:us-east-1:000000000000:campaign/seed-related";
/// The `PERSONALIZED_RANKING` campaign the `GetPersonalizedRanking` rows re-rank.
const CAMPAIGN_RANKING: &str = "arn:aws:personalize:us-east-1:000000000000:campaign/seed-ranking";
/// A second `PERSONALIZED_RANKING` campaign over an identical catalog.
const CAMPAIGN_RANKING_TWIN: &str =
    "arn:aws:personalize:us-east-1:000000000000:campaign/seed-ranking-twin";
/// The `PERSONALIZED_ACTIONS` campaign the `GetActionRecommendations` rows read.
const CAMPAIGN_ACTIONS: &str = "arn:aws:personalize:us-east-1:000000000000:campaign/seed-actions";
/// A second `PERSONALIZED_ACTIONS` campaign over an identical Actions catalog.
const CAMPAIGN_ACTIONS_TWIN: &str =
    "arn:aws:personalize:us-east-1:000000000000:campaign/seed-actions-twin";

/// A `USER_PERSONALIZATION` campaign whose entries are all EQUALLY popular.
const CAMPAIGN_FLAT_USER: &str =
    "arn:aws:personalize:us-east-1:000000000000:campaign/seed-flat-user";
/// A `PERSONALIZED_RANKING` campaign whose entries are all equally popular.
const CAMPAIGN_FLAT_RANKING: &str =
    "arn:aws:personalize:us-east-1:000000000000:campaign/seed-flat-ranking";
/// A `PERSONALIZED_ACTIONS` campaign whose actions are all equally popular.
const CAMPAIGN_FLAT_ACTIONS: &str =
    "arn:aws:personalize:us-east-1:000000000000:campaign/seed-flat-actions";

/// How many entries the seeded Items catalogs hold.
const CATALOG_SIZE: usize = 8;
/// How many entries the seeded Actions catalogs hold.
const ACTION_CATALOG_SIZE: usize = 6;
/// The popularity every entry of a flat catalog carries.
const FLAT_POPULARITY: f64 = 1.0;

/// The user the baseline rows personalize for.
const USER_A: &str = "user-7";
/// A second user, to vary the personalization part.
const USER_B: &str = "user-8";

/// The item a related-items row is the anchor of.
const ANCHOR_A: &str = "item-2";
/// A second anchor item.
const ANCHOR_B: &str = "item-5";

/// Two responses may not report an entry's certainty differently at all, so scores are
/// compared exactly up to double-rounding noise.
const TOLERANCE: f64 = 1e-12;

/// An emulator holding every campaign these suites read.
fn seeded() -> Emulator {
    let emulator = Emulator::new();
    let engine = &*emulator.engine;

    let campaigns = [
        (
            CAMPAIGN_USER,
            "seed-user",
            RecipeType::UserPersonalization,
            items(CATALOG_SIZE),
        ),
        (
            CAMPAIGN_USER_TWIN,
            "seed-user-twin",
            RecipeType::UserPersonalization,
            items(CATALOG_SIZE),
        ),
        (
            CAMPAIGN_RELATED,
            "seed-related",
            RecipeType::RelatedItems,
            items(CATALOG_SIZE),
        ),
        (
            CAMPAIGN_RANKING,
            "seed-ranking",
            RecipeType::PersonalizedRanking,
            items(CATALOG_SIZE),
        ),
        (
            CAMPAIGN_RANKING_TWIN,
            "seed-ranking-twin",
            RecipeType::PersonalizedRanking,
            items(CATALOG_SIZE),
        ),
        (
            CAMPAIGN_FLAT_USER,
            "seed-flat-user",
            RecipeType::UserPersonalization,
            flat_items(),
        ),
        (
            CAMPAIGN_FLAT_RANKING,
            "seed-flat-ranking",
            RecipeType::PersonalizedRanking,
            flat_items(),
        ),
    ];
    for (arn, name, recipe_type, catalog) in campaigns {
        assert!(
            registry::register_campaign(engine, Campaign::new(arn, name, recipe_type, catalog)),
            "case {arn}: the campaign seam must register the campaign"
        );
    }

    let action_campaigns = [
        (
            CAMPAIGN_ACTIONS,
            "seed-actions",
            actions(ACTION_CATALOG_SIZE),
        ),
        (
            CAMPAIGN_ACTIONS_TWIN,
            "seed-actions-twin",
            actions(ACTION_CATALOG_SIZE),
        ),
        (CAMPAIGN_FLAT_ACTIONS, "seed-flat-actions", flat_actions()),
    ];
    for (arn, name, catalog) in action_campaigns {
        assert!(
            registry::register_campaign(engine, Campaign::new_actions(arn, name, catalog)),
            "case {arn}: the campaign seam must register the actions campaign"
        );
    }
    emulator
}

/// An Items catalog whose entries are all equally popular, so the only seed part that
/// still tells them apart is the entry id.
fn flat_items() -> Vec<Item> {
    items(CATALOG_SIZE)
        .into_iter()
        .map(|item| Item {
            base_score: FLAT_POPULARITY,
            ..item
        })
        .collect()
}

/// An Actions catalog whose entries are all equally popular.
fn flat_actions() -> Vec<Action> {
    actions(ACTION_CATALOG_SIZE)
        .into_iter()
        .map(|action| Action {
            base_score: FLAT_POPULARITY,
            ..action
        })
        .collect()
}

/// The seeded Items catalog as `(entry id, stored popularity)`.
fn catalog() -> Vec<(String, f64)> {
    items(CATALOG_SIZE)
        .into_iter()
        .map(|item| (item.item_id, item.base_score))
        .collect()
}

/// The seeded Items catalog minus one entry — the candidate set a related-items
/// request has, which never includes the anchor item itself.
fn catalog_without(anchor: &str) -> Vec<(String, f64)> {
    catalog()
        .into_iter()
        .filter(|(item_id, _)| item_id != anchor)
        .collect()
}

/// The seeded Actions catalog as `(entry id, stored popularity)`.
fn action_catalog() -> Vec<(String, f64)> {
    actions(ACTION_CATALOG_SIZE)
        .into_iter()
        .map(|action| (action.action_id, action.base_score))
        .collect()
}

/// Every id of the seeded Items catalog, in catalog order.
fn catalog_ids() -> Vec<String> {
    catalog().into_iter().map(|(item_id, _)| item_id).collect()
}

/// The response member a route reports its scored entries in, and the member each
/// entry carries its id in.
fn list_member(route: &str) -> (&'static str, &'static str) {
    match route {
        RECOMMENDATIONS => ("itemList", "itemId"),
        PERSONALIZE_RANKING => ("personalizedRanking", "itemId"),
        ACTION_RECOMMENDATIONS => ("actionList", "actionId"),
        other => panic!("case {other}: not one of this service's routes"),
    }
}

/// One row of the table: a request, and the seed parts the service must have scored it
/// with.
struct Row {
    /// What the row varies, named in every assertion.
    case: &'static str,
    /// The operation's modelled route.
    route: &'static str,
    /// The request body.
    body: Value,
    /// The resolved resource the scores are derived under.
    scope: &'static str,
    /// The ONE personalization input that resource uses.
    personalization: &'static str,
    /// The WHOLE scored candidate set the resolved resource holds for this request.
    candidates: Vec<(String, f64)>,
    /// Whether the response reports every one of those candidates (and so carries the
    /// whole certainty).
    reports_every_candidate: bool,
}

/// The `context` part of a row's seed: the map the request sent, or `None` for
/// `GetActionRecommendations`, whose model carries no `context` member at all.
fn context_part(row: &Row) -> Option<HashMap<String, String>> {
    if row.route == ACTION_RECOMMENDATIONS {
        return None;
    }
    let sent = row
        .body
        .get("context")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        value.as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    Some(sent)
}

/// The share of the certainty the SHARED seed composition gives each candidate of a
/// row — the numbers the row's operation has to report.
fn expected_shares(row: &Row) -> BTreeMap<String, f64> {
    let context = context_part(row);
    let seed = match context.as_ref() {
        Some(context) => {
            scoring::Seed::with_context(row.scope, row.personalization, Some(context))
        }
        None => scoring::Seed::without_context(row.scope, row.personalization),
    };
    let mut shares: Vec<f64> = row
        .candidates
        .iter()
        .map(|(entry_id, base_score)| seed.relevance(*base_score, entry_id))
        .collect();
    scoring::normalize(&mut shares);
    row.candidates
        .iter()
        .map(|(entry_id, _)| entry_id.clone())
        .zip(shares)
        .collect()
}

/// Dispatch one request that must succeed and return the score it reports per entry
/// id.
async fn reported_shares(emulator: &Emulator, route: &str, body: Value) -> BTreeMap<String, f64> {
    let (member, id_member) = list_member(route);
    let response = emulator.ok(route, body).await;
    scores_by_id(&response, member, id_member)
}

/// Assert two responses over a shared candidate set report at least one entry
/// differently — the part `varies` names reached the seed.
fn assert_differs(
    case: &str,
    varies: &str,
    left: &BTreeMap<String, f64>,
    right: &BTreeMap<String, f64>,
) {
    let shared: Vec<&String> = left.keys().filter(|id| right.contains_key(*id)).collect();
    assert!(
        !shared.is_empty(),
        "case {case}: the two responses must share at least one entry, got {left:?} and {right:?}"
    );
    let differs = shared
        .iter()
        .any(|id| (left[*id] - right[*id]).abs() > TOLERANCE);
    assert!(
        differs,
        "case {case}: {varies} seeds the score, so the two responses must differ on some \
         shared entry, got {left:?} and {right:?}"
    );
}

/// The table: every operation, scored against the one shared seed composition.
#[tokio::test]
async fn every_operation_scores_through_the_one_shared_seed_composition() {
    let emulator = seeded();
    let every_id = catalog_ids();
    let half: Vec<String> = every_id.iter().take(CATALOG_SIZE / 2).cloned().collect();

    let rows: Vec<Row> = vec![
        Row {
            case: "user personalization, no context",
            route: RECOMMENDATIONS,
            body: json!({
                "campaignArn": CAMPAIGN_USER,
                "userId": USER_A,
                "numResults": CATALOG_SIZE,
            }),
            scope: CAMPAIGN_USER,
            personalization: USER_A,
            candidates: catalog(),
            reports_every_candidate: true,
        },
        Row {
            case: "user personalization, a one-key context",
            route: RECOMMENDATIONS,
            body: json!({
                "campaignArn": CAMPAIGN_USER,
                "userId": USER_A,
                "numResults": CATALOG_SIZE,
                "context": {"DEVICE": "Mobile"},
            }),
            scope: CAMPAIGN_USER,
            personalization: USER_A,
            candidates: catalog(),
            reports_every_candidate: true,
        },
        Row {
            case: "user personalization, a two-key context",
            route: RECOMMENDATIONS,
            body: json!({
                "campaignArn": CAMPAIGN_USER,
                "userId": USER_A,
                "numResults": CATALOG_SIZE,
                "context": {"DEVICE": "Mobile", "WEATHER": "Rain"},
            }),
            scope: CAMPAIGN_USER,
            personalization: USER_A,
            candidates: catalog(),
            reports_every_candidate: true,
        },
        Row {
            case: "user personalization, another user",
            route: RECOMMENDATIONS,
            body: json!({
                "campaignArn": CAMPAIGN_USER,
                "userId": USER_B,
                "numResults": CATALOG_SIZE,
            }),
            scope: CAMPAIGN_USER,
            personalization: USER_B,
            candidates: catalog(),
            reports_every_candidate: true,
        },
        Row {
            case: "user personalization, a second campaign over the same catalog",
            route: RECOMMENDATIONS,
            body: json!({
                "campaignArn": CAMPAIGN_USER_TWIN,
                "userId": USER_A,
                "numResults": CATALOG_SIZE,
            }),
            scope: CAMPAIGN_USER_TWIN,
            personalization: USER_A,
            candidates: catalog(),
            reports_every_candidate: true,
        },
        Row {
            case: "user personalization, fewer results than the catalog holds",
            route: RECOMMENDATIONS,
            body: json!({
                "campaignArn": CAMPAIGN_USER,
                "userId": USER_A,
                "numResults": 3,
            }),
            scope: CAMPAIGN_USER,
            personalization: USER_A,
            candidates: catalog(),
            reports_every_candidate: false,
        },
        Row {
            case: "related items, the itemId anchor is the personalization input",
            route: RECOMMENDATIONS,
            body: json!({
                "campaignArn": CAMPAIGN_RELATED,
                "itemId": ANCHOR_A,
                "numResults": CATALOG_SIZE,
            }),
            scope: CAMPAIGN_RELATED,
            personalization: ANCHOR_A,
            candidates: catalog_without(ANCHOR_A),
            reports_every_candidate: true,
        },
        Row {
            case: "related items, another anchor and a context",
            route: RECOMMENDATIONS,
            body: json!({
                "campaignArn": CAMPAIGN_RELATED,
                "itemId": ANCHOR_B,
                "numResults": CATALOG_SIZE,
                "context": {"DEVICE": "TV"},
            }),
            scope: CAMPAIGN_RELATED,
            personalization: ANCHOR_B,
            candidates: catalog_without(ANCHOR_B),
            reports_every_candidate: true,
        },
        Row {
            case: "ranking, the whole catalog in the input list",
            route: PERSONALIZE_RANKING,
            body: json!({
                "campaignArn": CAMPAIGN_RANKING,
                "userId": USER_A,
                "inputList": every_id,
            }),
            scope: CAMPAIGN_RANKING,
            personalization: USER_A,
            candidates: catalog(),
            reports_every_candidate: true,
        },
        Row {
            case: "ranking, a context",
            route: PERSONALIZE_RANKING,
            body: json!({
                "campaignArn": CAMPAIGN_RANKING,
                "userId": USER_A,
                "inputList": every_id,
                "context": {"DEVICE": "Mobile", "WEATHER": "Rain"},
            }),
            scope: CAMPAIGN_RANKING,
            personalization: USER_A,
            candidates: catalog(),
            reports_every_candidate: true,
        },
        Row {
            case: "ranking, another user",
            route: PERSONALIZE_RANKING,
            body: json!({
                "campaignArn": CAMPAIGN_RANKING,
                "userId": USER_B,
                "inputList": every_id,
            }),
            scope: CAMPAIGN_RANKING,
            personalization: USER_B,
            candidates: catalog(),
            reports_every_candidate: true,
        },
        Row {
            case: "ranking, a second campaign over the same catalog",
            route: PERSONALIZE_RANKING,
            body: json!({
                "campaignArn": CAMPAIGN_RANKING_TWIN,
                "userId": USER_A,
                "inputList": every_id,
            }),
            scope: CAMPAIGN_RANKING_TWIN,
            personalization: USER_A,
            candidates: catalog(),
            reports_every_candidate: true,
        },
        Row {
            case: "ranking, half the catalog in the input list",
            route: PERSONALIZE_RANKING,
            body: json!({
                "campaignArn": CAMPAIGN_RANKING,
                "userId": USER_A,
                "inputList": half,
            }),
            scope: CAMPAIGN_RANKING,
            personalization: USER_A,
            candidates: catalog(),
            reports_every_candidate: false,
        },
        Row {
            case: "action recommendations, no context member is modelled",
            route: ACTION_RECOMMENDATIONS,
            body: json!({
                "campaignArn": CAMPAIGN_ACTIONS,
                "userId": USER_A,
                "numResults": ACTION_CATALOG_SIZE,
            }),
            scope: CAMPAIGN_ACTIONS,
            personalization: USER_A,
            candidates: action_catalog(),
            reports_every_candidate: true,
        },
        Row {
            case: "action recommendations, another user",
            route: ACTION_RECOMMENDATIONS,
            body: json!({
                "campaignArn": CAMPAIGN_ACTIONS,
                "userId": USER_B,
                "numResults": ACTION_CATALOG_SIZE,
            }),
            scope: CAMPAIGN_ACTIONS,
            personalization: USER_B,
            candidates: action_catalog(),
            reports_every_candidate: true,
        },
        Row {
            case: "action recommendations, a second campaign over the same catalog",
            route: ACTION_RECOMMENDATIONS,
            body: json!({
                "campaignArn": CAMPAIGN_ACTIONS_TWIN,
                "userId": USER_A,
                "numResults": ACTION_CATALOG_SIZE,
            }),
            scope: CAMPAIGN_ACTIONS_TWIN,
            personalization: USER_A,
            candidates: action_catalog(),
            reports_every_candidate: true,
        },
        Row {
            case: "action recommendations, fewer results than the catalog holds",
            route: ACTION_RECOMMENDATIONS,
            body: json!({
                "campaignArn": CAMPAIGN_ACTIONS,
                "userId": USER_A,
                "numResults": 2,
            }),
            scope: CAMPAIGN_ACTIONS,
            personalization: USER_A,
            candidates: action_catalog(),
            reports_every_candidate: false,
        },
    ];

    for row in &rows {
        let case = row.case;
        let (member, id_member) = list_member(row.route);
        let expected = expected_shares(row);
        let response = emulator.ok(row.route, row.body.clone()).await;
        let reported = scores_by_id(&response, member, id_member);
        let ordered = scores(&response, member);

        assert!(
            !reported.is_empty(),
            "case {case}: expected scored entries, got {response}"
        );
        if row.reports_every_candidate {
            assert_eq!(
                reported.len(),
                row.candidates.len(),
                "case {case}: expected every candidate reported, got {reported:?}"
            );
            assert_sums_to_one(case, &ordered);
        } else {
            assert!(
                reported.len() < row.candidates.len(),
                "case {case}: expected part of the candidate set, got {reported:?}"
            );
            assert_descending_and_bounded(case, &ordered);
        }
        for (entry_id, score) in &reported {
            let want = expected.get(entry_id).unwrap_or_else(|| {
                panic!("case {case}: {entry_id} is not one of the scored candidates {expected:?}")
            });
            assert!(
                (score - want).abs() < TOLERANCE,
                "case {case}: the shared seed scores {entry_id} {want}, got {score}"
            );
        }

        // The operations are `@idempotent`: the same seed parts, so the same numbers.
        let replayed = reported_shares(&emulator, row.route, row.body.clone()).await;
        assert_eq!(
            replayed, reported,
            "case {case}: a replayed identical request must report identical scores"
        );
    }
}

/// Every part of the shared seed has to reach the score of the operations that carry
/// it, and no part may be dropped by one of them.
#[tokio::test]
async fn each_seed_part_moves_the_scores_of_every_operation_that_carries_it() {
    let emulator = seeded();
    let every_id = catalog_ids();

    // (what the pair varies, route, the two requests that differ only in that part).
    let pairs: Vec<(&str, &str, Value, Value)> = vec![
        (
            "the resolved resource",
            RECOMMENDATIONS,
            json!({"campaignArn": CAMPAIGN_USER, "userId": USER_A, "numResults": CATALOG_SIZE}),
            json!({
                "campaignArn": CAMPAIGN_USER_TWIN,
                "userId": USER_A,
                "numResults": CATALOG_SIZE,
            }),
        ),
        (
            "the userId the resource uses",
            RECOMMENDATIONS,
            json!({"campaignArn": CAMPAIGN_USER, "userId": USER_A, "numResults": CATALOG_SIZE}),
            json!({"campaignArn": CAMPAIGN_USER, "userId": USER_B, "numResults": CATALOG_SIZE}),
        ),
        (
            "the itemId the resource uses",
            RECOMMENDATIONS,
            json!({"campaignArn": CAMPAIGN_RELATED, "itemId": ANCHOR_A, "numResults": CATALOG_SIZE}),
            json!({"campaignArn": CAMPAIGN_RELATED, "itemId": ANCHOR_B, "numResults": CATALOG_SIZE}),
        ),
        (
            "the context",
            RECOMMENDATIONS,
            json!({"campaignArn": CAMPAIGN_USER, "userId": USER_A, "numResults": CATALOG_SIZE}),
            json!({
                "campaignArn": CAMPAIGN_USER,
                "userId": USER_A,
                "numResults": CATALOG_SIZE,
                "context": {"DEVICE": "Mobile"},
            }),
        ),
        (
            "the resolved resource",
            PERSONALIZE_RANKING,
            json!({
                "campaignArn": CAMPAIGN_RANKING,
                "userId": USER_A,
                "inputList": every_id,
            }),
            json!({
                "campaignArn": CAMPAIGN_RANKING_TWIN,
                "userId": USER_A,
                "inputList": every_id,
            }),
        ),
        (
            "the userId the resource uses",
            PERSONALIZE_RANKING,
            json!({"campaignArn": CAMPAIGN_RANKING, "userId": USER_A, "inputList": every_id}),
            json!({"campaignArn": CAMPAIGN_RANKING, "userId": USER_B, "inputList": every_id}),
        ),
        (
            "the context",
            PERSONALIZE_RANKING,
            json!({"campaignArn": CAMPAIGN_RANKING, "userId": USER_A, "inputList": every_id}),
            json!({
                "campaignArn": CAMPAIGN_RANKING,
                "userId": USER_A,
                "inputList": every_id,
                "context": {"DEVICE": "Mobile"},
            }),
        ),
        (
            "the resolved resource",
            ACTION_RECOMMENDATIONS,
            json!({
                "campaignArn": CAMPAIGN_ACTIONS,
                "userId": USER_A,
                "numResults": ACTION_CATALOG_SIZE,
            }),
            json!({
                "campaignArn": CAMPAIGN_ACTIONS_TWIN,
                "userId": USER_A,
                "numResults": ACTION_CATALOG_SIZE,
            }),
        ),
        (
            "the userId the resource uses",
            ACTION_RECOMMENDATIONS,
            json!({
                "campaignArn": CAMPAIGN_ACTIONS,
                "userId": USER_A,
                "numResults": ACTION_CATALOG_SIZE,
            }),
            json!({
                "campaignArn": CAMPAIGN_ACTIONS,
                "userId": USER_B,
                "numResults": ACTION_CATALOG_SIZE,
            }),
        ),
    ];

    for (varies, route, left_body, right_body) in pairs {
        let case = format!("{route} varying {varies}");
        let left = reported_shares(&emulator, route, left_body).await;
        let right = reported_shares(&emulator, route, right_body).await;
        assert_differs(&case, varies, &left, &right);
    }
}

/// The entry's own id is a seed part too: over a catalog whose entries are all equally
/// popular, the only thing left to tell two entries apart is their id, so every
/// reported score is still distinct — in all three operations.
#[tokio::test]
async fn the_entry_id_seeds_the_score_of_every_operation() {
    let emulator = seeded();
    let every_id = catalog_ids();

    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "recommendations over an equally popular catalog",
            RECOMMENDATIONS,
            json!({
                "campaignArn": CAMPAIGN_FLAT_USER,
                "userId": USER_A,
                "numResults": CATALOG_SIZE,
            }),
        ),
        (
            "ranking over an equally popular catalog",
            PERSONALIZE_RANKING,
            json!({
                "campaignArn": CAMPAIGN_FLAT_RANKING,
                "userId": USER_A,
                "inputList": every_id,
            }),
        ),
        (
            "action recommendations over an equally popular catalog",
            ACTION_RECOMMENDATIONS,
            json!({
                "campaignArn": CAMPAIGN_FLAT_ACTIONS,
                "userId": USER_A,
                "numResults": ACTION_CATALOG_SIZE,
            }),
        ),
    ];

    for (case, route, body) in rows {
        let reported = reported_shares(&emulator, route, body).await;
        assert!(
            reported.len() > 1,
            "case {case}: expected more than one scored entry, got {reported:?}"
        );
        for (left_id, left) in &reported {
            for (right_id, right) in &reported {
                if left_id >= right_id {
                    continue;
                }
                assert!(
                    (left - right).abs() > TOLERANCE,
                    "case {case}: {left_id} and {right_id} are equally popular, so their ids \
                     must score them apart, got {left} and {right}"
                );
            }
        }
    }
}

/// `EXCLUDE ItemID WHERE Items.GENRE IN ("Comedy")`.
const FILTER_EXCLUDE_COMEDY: &str =
    "arn:aws:personalize:us-east-1:000000000000:filter/pipeline-exclude-comedy";
/// `INCLUDE ItemID WHERE Items.GENRE IN ($GENRE)`, whose value the request binds.
const FILTER_INCLUDE_GENRE: &str =
    "arn:aws:personalize:us-east-1:000000000000:filter/pipeline-include-genre";
/// `INCLUDE ItemID WHERE Items.YEAR >= 1995` — an ordering predicate over a numerical
/// column, so the row selects by something other than a categorical value.
const FILTER_RECENT: &str = "arn:aws:personalize:us-east-1:000000000000:filter/pipeline-recent";
/// `INCLUDE ActionID WHERE Actions.CHANNEL IN ("App")`.
const FILTER_APP_ACTIONS: &str =
    "arn:aws:personalize:us-east-1:000000000000:filter/pipeline-app-actions";
/// `EXCLUDE ActionID WHERE Actions.CHANNEL IN ("Email")`.
const FILTER_NOT_EMAIL_ACTIONS: &str =
    "arn:aws:personalize:us-east-1:000000000000:filter/pipeline-not-email-actions";

/// One row of the pipeline table: what it varies, the filter it names (its registered
/// ARN and the expression that ARN carries), the `filterValues` it binds, and how many
/// results it asks for.
struct PipelineRow {
    /// What the row varies, named in every assertion.
    case: &'static str,
    /// The request's `filterArn`, and the expression the registry holds for it, or
    /// `None` for a row that names no filter at all.
    filter: Option<(&'static str, &'static str)>,
    /// The `filterValues` the row binds the expression's placeholders to.
    filter_values: Vec<(&'static str, &'static str)>,
    /// The `numResults` the row asks for.
    num_results: usize,
}

/// The whole filtered, score-descending list the SHARED pipeline produces for one row —
/// the list the row's operation has to report the head of.
///
/// Computed from the test side through `scoring::ranked`, over the WHOLE stored catalog
/// and the row's bound filter: an operation that applied the same three rules in any
/// other order (normalizing after its filter, or after its result count) would report
/// different numbers here.
///
/// The interaction half of a filter's input is empty: [`seeded`] registers campaigns and
/// ingests nothing, so no user has interaction rows, and every row's expression reads
/// entity columns only (`GENRE`, `YEAR`, `CHANNEL`). A row with a clause over an
/// interaction dataset would have to read the ingested rows of the user it personalizes
/// for instead.
fn expected_pipeline<Entry: CatalogEntry>(
    catalog: &[Entry],
    seed: &scoring::Seed<'_>,
    entity: Entity,
    row: &PipelineRow,
) -> Vec<(String, f64)> {
    let case = row.case;
    let bound = row.filter.map(|(_, expression)| {
        let parsed = registry::filter::parse(expression).unwrap_or_else(|error| {
            panic!("case {case}: {expression:?} must parse, got {error}")
        });
        let values: HashMap<String, String> = row
            .filter_values
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        registry::filter::bind(&parsed, entity, Some(&values)).unwrap_or_else(|error| {
            panic!("case {case}: {expression:?} must bind to {values:?}, got {error}")
        })
    });
    scoring::ranked(
        catalog.iter(),
        seed,
        bound.as_ref(),
        &RecordedInteractions::default(),
    )
    .into_iter()
    .map(|(entry, score)| (entry.entry_id().to_string(), score))
    .collect()
}

/// The `(id, score)` pairs one response reports, in response order.
fn reported_pipeline(response: &Value, member: &str, id_member: &str) -> Vec<(String, f64)> {
    ids(response, member, id_member)
        .into_iter()
        .zip(scores(response, member))
        .collect()
}

/// Assert one response reports exactly `expected` — the same ids, in the same order,
/// each with the same share.
fn assert_same_ranking(
    case: &str,
    what: &str,
    returned: &[(String, f64)],
    expected: &[(String, f64)],
) {
    assert_eq!(
        returned.len(),
        expected.len(),
        "case {case}: expected {} entries, got {returned:?}",
        expected.len()
    );
    for (position, ((got_id, got_score), (want_id, want_score))) in
        returned.iter().zip(expected).enumerate()
    {
        assert_eq!(
            got_id, want_id,
            "case {case}: position {position} must report {want_id}, got {got_id}"
        );
        assert!(
            (got_score - want_score).abs() < TOLERANCE,
            "case {case}: {want_id} is {what} share {want_score}, got {got_score}"
        );
    }
}

/// Assert one response is exactly the head of the list the shared pipeline produced.
fn assert_is_the_shared_pipeline(
    case: &str,
    catalog_size: usize,
    row_filters: bool,
    num_results: usize,
    whole: &[(String, f64)],
    returned: &[(String, f64)],
) {
    assert!(
        !whole.is_empty(),
        "case {case}: the row must leave at least one candidate, got {whole:?}"
    );
    if row_filters {
        assert!(
            whole.len() < catalog_size,
            "case {case}: the row's filter must select a strict subset of the {catalog_size} \
             stored entries, got {}",
            whole.len()
        );
    }
    let expected: &[(String, f64)] = &whole[..num_results.min(whole.len())];
    assert_same_ranking(case, "the shared pipeline's", returned, expected);
}

/// The list the rule MEANS over a whole catalog, spelled out without the shared helper:
/// each entry's relevance for this request, each divided by the total of all of them,
/// ordered by share descending and, on a tie, by the entry's own id ascending.
///
/// An independent oracle for the order, so the unfiltered rows do not rest on the same
/// helper the handlers now call.
fn independent_ranking(catalog: &[(String, f64)], seed: &scoring::Seed<'_>) -> Vec<(String, f64)> {
    let mut scored: Vec<(String, f64)> = catalog
        .iter()
        .map(|(entry_id, base_score)| (entry_id.clone(), seed.relevance(*base_score, entry_id)))
        .collect();
    let total: f64 = scored.iter().map(|(_, score)| *score).sum();
    assert!(
        total > 0.0,
        "the seeded catalog carries popularity, so its scores must total above 0, got {total}"
    );
    for (_, score) in &mut scored {
        *score /= total;
    }
    scored.sort_by(|(left_id, left), (right_id, right)| {
        right
            .partial_cmp(left)
            .expect("a stored popularity is finite, so its share is comparable")
            .then_with(|| left_id.cmp(right_id))
    });
    scored
}

/// Assert every entry one response reports carries exactly the score the SAME request
/// over the whole, unfiltered catalog gave it — read off the API rather than off the
/// shared helper, so a handler that went back to applying the three rules in its own
/// order fails here too.
fn assert_matches_the_unfiltered_reference(
    case: &str,
    returned: &[(String, f64)],
    reference: &BTreeMap<String, f64>,
) {
    for (id, score) in returned {
        let expected = reference.get(id).unwrap_or_else(|| {
            panic!("case {case}: {id} is not one of the scored catalog entries {reference:?}")
        });
        assert!(
            (score - expected).abs() < TOLERANCE,
            "case {case}: {id} scores {expected} over the whole unfiltered catalog, got {score}"
        );
    }
}

/// Both operations that choose their own list report it through the ONE shared
/// score-then-normalize-then-filter-then-sort pipeline.
///
/// The rows differ along what that order generalizes over: which entity type the
/// catalog holds (`Item` through `GetRecommendations`, `Action` through
/// `GetActionRecommendations`), whether the request names a filter, what the expression
/// selects by (a categorical value, a bound `$placeholder`, an ordering predicate over
/// a numerical column, an `EXCLUDE` instead of an `INCLUDE`), and how many results it
/// asks back — one, a few, and more than the catalog holds.
#[tokio::test]
async fn both_catalogs_are_reported_through_the_one_shared_pipeline() {
    let emulator = seeded();
    let engine = &*emulator.engine;
    let filters = [
        (
            FILTER_EXCLUDE_COMEDY,
            "EXCLUDE ItemID WHERE Items.GENRE IN (\"Comedy\")",
        ),
        (
            FILTER_INCLUDE_GENRE,
            "INCLUDE ItemID WHERE Items.GENRE IN ($GENRE)",
        ),
        (FILTER_RECENT, "INCLUDE ItemID WHERE Items.YEAR >= 1995"),
        (
            FILTER_APP_ACTIONS,
            "INCLUDE ActionID WHERE Actions.CHANNEL IN (\"App\")",
        ),
        (
            FILTER_NOT_EMAIL_ACTIONS,
            "EXCLUDE ActionID WHERE Actions.CHANNEL IN (\"Email\")",
        ),
    ];
    for (arn, expression) in filters {
        assert_eq!(
            registry::register_filter(engine, arn, expression),
            Ok(true),
            "case {arn}: the filter seam must accept {expression:?}"
        );
    }

    // `GetRecommendations` over the stored Items catalog. The reference every row's
    // scores are also compared against: the same request over the whole catalog, with
    // no filter and no bound on how many entries come back.
    let item_catalog = items(CATALOG_SIZE);
    let item_seed = scoring::Seed::with_context(CAMPAIGN_USER, USER_A, None);
    let item_whole = emulator
        .ok(
            RECOMMENDATIONS,
            json!({
                "campaignArn": CAMPAIGN_USER,
                "userId": USER_A,
                "numResults": CATALOG_SIZE,
            }),
        )
        .await;
    let item_reference = scores_by_id(&item_whole, "itemList", "itemId");
    assert_eq!(
        item_reference.len(),
        CATALOG_SIZE,
        "the reference must report every stored item, got {item_reference:?}"
    );
    // The reference itself, against the order the rule means, written out without the
    // shared helper.
    assert_same_ranking(
        "items, the whole catalog against the rule spelled out",
        "the rule's",
        &reported_pipeline(&item_whole, "itemList", "itemId"),
        &independent_ranking(&catalog(), &item_seed),
    );
    let item_rows = [
        PipelineRow {
            case: "items, no filter, one result",
            filter: None,
            filter_values: Vec::new(),
            num_results: 1,
        },
        PipelineRow {
            case: "items, no filter, the whole catalog",
            filter: None,
            filter_values: Vec::new(),
            num_results: CATALOG_SIZE,
        },
        PipelineRow {
            case: "items, no filter, more results than the catalog holds",
            filter: None,
            filter_values: Vec::new(),
            num_results: CATALOG_SIZE + 3,
        },
        PipelineRow {
            case: "items, EXCLUDE Comedy, one result",
            filter: Some((
                FILTER_EXCLUDE_COMEDY,
                "EXCLUDE ItemID WHERE Items.GENRE IN (\"Comedy\")",
            )),
            filter_values: Vec::new(),
            num_results: 1,
        },
        PipelineRow {
            case: "items, EXCLUDE Comedy, three results",
            filter: Some((
                FILTER_EXCLUDE_COMEDY,
                "EXCLUDE ItemID WHERE Items.GENRE IN (\"Comedy\")",
            )),
            filter_values: Vec::new(),
            num_results: 3,
        },
        PipelineRow {
            case: "items, INCLUDE a bound GENRE, the whole catalog",
            filter: Some((
                FILTER_INCLUDE_GENRE,
                "INCLUDE ItemID WHERE Items.GENRE IN ($GENRE)",
            )),
            filter_values: vec![("GENRE", "Drama")],
            num_results: CATALOG_SIZE,
        },
        PipelineRow {
            case: "items, INCLUDE two bound GENRE values, two results",
            filter: Some((
                FILTER_INCLUDE_GENRE,
                "INCLUDE ItemID WHERE Items.GENRE IN ($GENRE)",
            )),
            filter_values: vec![("GENRE", "Comedy,Action")],
            num_results: 2,
        },
        PipelineRow {
            case: "items, INCLUDE YEAR >= 1995, the whole catalog",
            filter: Some((FILTER_RECENT, "INCLUDE ItemID WHERE Items.YEAR >= 1995")),
            filter_values: Vec::new(),
            num_results: CATALOG_SIZE,
        },
    ];
    for row in &item_rows {
        let mut body = json!({
            "campaignArn": CAMPAIGN_USER,
            "userId": USER_A,
            "numResults": row.num_results,
        });
        if let Some((filter_arn, _)) = row.filter {
            body["filterArn"] = json!(filter_arn);
            let values: BTreeMap<&str, &str> = row.filter_values.iter().copied().collect();
            body["filterValues"] = json!(values);
        }
        let response = emulator.ok(RECOMMENDATIONS, body).await;
        let whole = expected_pipeline(&item_catalog, &item_seed, Entity::Items, row);
        let returned = reported_pipeline(&response, "itemList", "itemId");
        assert_is_the_shared_pipeline(
            row.case,
            CATALOG_SIZE,
            row.filter.is_some(),
            row.num_results,
            &whole,
            &returned,
        );
        assert_matches_the_unfiltered_reference(row.case, &returned, &item_reference);
        assert_descending_and_bounded(row.case, &scores(&response, "itemList"));
    }

    // One FILTERED row with its expectation spelled out without the shared helper and
    // without the filter evaluator: the rule's own ranking over the whole catalog, with
    // the entries whose stored `GENRE` is the excluded one dropped, then cut to the
    // result count. `GENRE` is the Items-dataset column the seeded catalog cycles
    // through (`crate::support::GENRES`), and `EXCLUDE` keeps what the expression does
    // not match.
    let excluded_genre = "Comedy";
    let kept: Vec<(String, f64)> = independent_ranking(&catalog(), &item_seed)
        .into_iter()
        .filter(|(item_id, _)| {
            item_catalog.iter().any(|item| {
                &item.item_id == item_id
                    && item.attributes.get(COLUMN_GENRE).map(String::as_str)
                        != Some(excluded_genre)
            })
        })
        .take(3)
        .collect();
    assert_eq!(
        kept.len(),
        3,
        "the seeded catalog must leave at least three entries once {excluded_genre} is \
         excluded, got {kept:?}"
    );
    let spelled_out = emulator
        .ok(
            RECOMMENDATIONS,
            json!({
                "campaignArn": CAMPAIGN_USER,
                "userId": USER_A,
                "numResults": 3,
                "filterArn": FILTER_EXCLUDE_COMEDY,
            }),
        )
        .await;
    assert_same_ranking(
        "items, EXCLUDE Comedy against the rule spelled out",
        "the rule's",
        &reported_pipeline(&spelled_out, "itemList", "itemId"),
        &kept,
    );

    // `GetActionRecommendations` over the stored Actions catalog: the same order, the
    // other entity type, against its own whole-catalog reference.
    let action_catalog_entries = actions(ACTION_CATALOG_SIZE);
    let action_seed = scoring::Seed::without_context(CAMPAIGN_ACTIONS, USER_A);
    let action_whole = emulator
        .ok(
            ACTION_RECOMMENDATIONS,
            json!({
                "campaignArn": CAMPAIGN_ACTIONS,
                "userId": USER_A,
                "numResults": ACTION_CATALOG_SIZE,
            }),
        )
        .await;
    let action_reference = scores_by_id(&action_whole, "actionList", "actionId");
    assert_eq!(
        action_reference.len(),
        ACTION_CATALOG_SIZE,
        "the reference must report every stored action, got {action_reference:?}"
    );
    assert_same_ranking(
        "actions, the whole catalog against the rule spelled out",
        "the rule's",
        &reported_pipeline(&action_whole, "actionList", "actionId"),
        &independent_ranking(&action_catalog(), &action_seed),
    );
    let action_rows = [
        PipelineRow {
            case: "actions, no filter, one result",
            filter: None,
            filter_values: Vec::new(),
            num_results: 1,
        },
        PipelineRow {
            case: "actions, no filter, the whole catalog",
            filter: None,
            filter_values: Vec::new(),
            num_results: ACTION_CATALOG_SIZE,
        },
        PipelineRow {
            case: "actions, no filter, more results than the catalog holds",
            filter: None,
            filter_values: Vec::new(),
            num_results: ACTION_CATALOG_SIZE + 3,
        },
        PipelineRow {
            case: "actions, INCLUDE CHANNEL App, one result",
            filter: Some((
                FILTER_APP_ACTIONS,
                "INCLUDE ActionID WHERE Actions.CHANNEL IN (\"App\")",
            )),
            filter_values: Vec::new(),
            num_results: 1,
        },
        PipelineRow {
            case: "actions, INCLUDE CHANNEL App, the whole catalog",
            filter: Some((
                FILTER_APP_ACTIONS,
                "INCLUDE ActionID WHERE Actions.CHANNEL IN (\"App\")",
            )),
            filter_values: Vec::new(),
            num_results: ACTION_CATALOG_SIZE,
        },
        PipelineRow {
            case: "actions, EXCLUDE CHANNEL Email, two results",
            filter: Some((
                FILTER_NOT_EMAIL_ACTIONS,
                "EXCLUDE ActionID WHERE Actions.CHANNEL IN (\"Email\")",
            )),
            filter_values: Vec::new(),
            num_results: 2,
        },
    ];
    for row in &action_rows {
        let mut body = json!({
            "campaignArn": CAMPAIGN_ACTIONS,
            "userId": USER_A,
            "numResults": row.num_results,
        });
        if let Some((filter_arn, _)) = row.filter {
            body["filterArn"] = json!(filter_arn);
        }
        let response = emulator.ok(ACTION_RECOMMENDATIONS, body).await;
        let whole = expected_pipeline(&action_catalog_entries, &action_seed, Entity::Actions, row);
        let returned = reported_pipeline(&response, "actionList", "actionId");
        assert_is_the_shared_pipeline(
            row.case,
            ACTION_CATALOG_SIZE,
            row.filter.is_some(),
            row.num_results,
            &whole,
            &returned,
        );
        assert_matches_the_unfiltered_reference(row.case, &returned, &action_reference);
        assert_descending_and_bounded(row.case, &scores(&response, "actionList"));
    }

    // The same independent expectation for a filtered ACTIONS row: the rule's own
    // ranking over the whole Actions catalog, keeping the entries whose stored `CHANNEL`
    // is the included one. `CHANNEL` is the Actions-dataset column the seeded catalog
    // cycles through (`crate::support::CHANNELS`), and `INCLUDE` keeps what the
    // expression matches.
    let included_channel = "App";
    let app_actions: Vec<(String, f64)> = independent_ranking(&action_catalog(), &action_seed)
        .into_iter()
        .filter(|(action_id, _)| {
            action_catalog_entries.iter().any(|action| {
                &action.action_id == action_id
                    && action.attributes.get("CHANNEL").map(String::as_str)
                        == Some(included_channel)
            })
        })
        .take(2)
        .collect();
    assert_eq!(
        app_actions.len(),
        2,
        "the seeded Actions catalog must leave at least two {included_channel} entries, got \
         {app_actions:?}"
    );
    let spelled_out_actions = emulator
        .ok(
            ACTION_RECOMMENDATIONS,
            json!({
                "campaignArn": CAMPAIGN_ACTIONS,
                "userId": USER_A,
                "numResults": 2,
                "filterArn": FILTER_APP_ACTIONS,
            }),
        )
        .await;
    assert_same_ranking(
        "actions, INCLUDE CHANNEL App against the rule spelled out",
        "the rule's",
        &reported_pipeline(&spelled_out_actions, "actionList", "actionId"),
        &app_actions,
    );
}
