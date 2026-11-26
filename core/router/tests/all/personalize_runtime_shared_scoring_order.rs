// SPDX-License-Identifier: CC0-1.0

//! The ONE order the three Amazon Personalize Runtime inference operations apply their
//! scoring rules in, driven through the assembled router.
//!
//! RULE under test: **a catalog is scored, then each entry's share of the reported
//! certainty is settled over the WHOLE scored catalog, then the request's filter is
//! applied, then what is left is ordered by score descending — and the only part of that
//! composition which is per-operation is the key a TIED pair is settled by.** All three
//! operations compose those four steps through one helper
//! (`state::scoring::ranked` / `ranked_by`), so none of them can drift from the order:
//! `GetRecommendations` and `GetActionRecommendations` choose their own list and settle a
//! tie by the entry's own id, while `GetPersonalizedRanking` re-ranks a list the caller
//! sent and settles a tie by that list's position.
//!
//! The order is observable without reading any of them. Because normalization is settled
//! BEFORE the filter, an entry carries the SAME score whatever the request removed or
//! named, and a response reporting part of the catalog carries part of the certainty; had
//! the filter run first, every reported score would be inflated to make the surviving
//! entries sum to 1.0 again. So each row compares every reported score against ONE
//! reference — the same request over the whole catalog with no filter — which is an
//! assertion about the ORDER of the steps, not about any particular score.
//!
//! The rows span what the rule generalises over: the operation, which filter removes
//! which part of the catalog, how many entries the request names or asks back, whether
//! the catalog holds an id the request named, and whether the catalog carries any
//! popularity at all (a catalog that carries none ties every pair, which is what leaves
//! the tie-break key alone to decide).
//!
//! Why it lives beside the router's suites rather than in one service crate: the rule
//! spans the three operations AND the shared module they compose it in, and these rows
//! drive it where a client actually reaches them — the assembled router. The
//! per-operation tables (the denominator per request member, the tie-break key per
//! operation, repeated `inputList` ids) are in the service crate's own suites.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_personalizeruntime_amazonpersonalizeruntime as runtime;
use runtime::state::{self as registry, Action, Campaign, Item, RecipeType};

/// The endpoint host an Amazon Personalize Runtime caller addresses
/// (`endpointPrefix: personalize-runtime`).
const HOST: &str = "personalize-runtime.us-east-1.amazonaws.com";

/// A SigV4 `Authorization` header whose credential scope names the signing name this
/// service claims (`personalize`) — the routing signal the router reads first. Request
/// authentication is off here, so the signature itself is never verified.
const SIGV4_HEADER: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/personalize/aws4_request, \
     SignedHeaders=host, Signature=0";

/// `GetPersonalizedRanking`: `POST /personalize-ranking`.
const RANKING: &str = "/personalize-ranking";
/// `GetRecommendations`: `POST /recommendations`.
const RECOMMENDATIONS: &str = "/recommendations";
/// `GetActionRecommendations`: `POST /action-recommendations`.
const ACTION_RECOMMENDATIONS: &str = "/action-recommendations";

/// The Items-dataset column the seeded Items catalog carries, spelled the way a
/// Personalize Items dataset spells a column.
const COLUMN_GENRE: &str = "GENRE";
/// The `GENRE` values the seeded Items catalog cycles through, so a filter expression
/// over the column selects a known subset.
const GENRES: [&str; 4] = ["Comedy", "Drama", "Action", "Documentary"];
/// The Actions-dataset column the seeded Actions catalog carries.
const COLUMN_CHANNEL: &str = "CHANNEL";
/// The `CHANNEL` values the seeded Actions catalog cycles through.
const CHANNELS: [&str; 2] = ["App", "Email"];

/// How many entries the seeded Items catalogs hold. Past ten, so the ids' ascending
/// order (`item-0`, `item-1`, `item-10`, …) is NOT the catalog order and an assertion on
/// either cannot pass by accident.
const CATALOG_SIZE: usize = 12;
/// How many entries the seeded Actions catalogs hold — also past ten, for the same
/// reason.
const ACTION_CATALOG_SIZE: usize = 11;

/// An id no seeded catalog holds: "If an item was not included in the training dataset,
/// the item is appended to the end of the reranked list"
/// (`GetPersonalizedRanking$inputList`).
const UNKNOWN_ITEM: &str = "item-the-catalog-never-held";

/// Two responses may not report an entry's certainty differently at all, so scores are
/// compared exactly up to double-rounding noise.
const TOLERANCE: f64 = 1e-12;

/// One row: its name, the route it sends to, the request body, the response list member
/// and id member to read, and the ids the response must report.
type Row = (
    String,
    &'static str,
    Value,
    &'static str,
    &'static str,
    Vec<String>,
);

/// An ARN in the account and Region this emulator serves; any other account is a
/// cross-account reference the authorization stage refuses before anything resolves.
fn arn(resource: &str) -> String {
    format!(
        "arn:aws:personalize:us-east-1:{}:{resource}",
        ::engine::EMULATOR_ACCOUNT_ID
    )
}

/// The `PERSONALIZED_RANKING` campaign the re-ranking rows address.
fn campaign_ranking() -> String {
    arn("campaign/shared-order-ranking")
}

/// The `USER_PERSONALIZATION` campaign the `GetRecommendations` rows address.
fn campaign_user() -> String {
    arn("campaign/shared-order-user")
}

/// The `PERSONALIZED_ACTIONS` campaign the `GetActionRecommendations` rows address.
fn campaign_actions() -> String {
    arn("campaign/shared-order-actions")
}

/// The same three campaigns over catalogs that carry NO popularity, where every pair of
/// scores ties and the tie-break key alone decides the order.
fn campaign_flat_ranking() -> String {
    arn("campaign/shared-order-flat-ranking")
}

fn campaign_flat_user() -> String {
    arn("campaign/shared-order-flat-user")
}

fn campaign_flat_actions() -> String {
    arn("campaign/shared-order-flat-actions")
}

/// `EXCLUDE ItemID WHERE Items.GENRE IN ("Comedy")`.
fn filter_exclude_comedy() -> String {
    arn("filter/shared-order-exclude-comedy")
}

/// `INCLUDE ItemID WHERE Items.GENRE IN ($GENRE)`.
fn filter_include_genre() -> String {
    arn("filter/shared-order-include-genre")
}

/// `INCLUDE ActionID WHERE Actions.CHANNEL IN ("App")`.
fn filter_app_actions() -> String {
    arn("filter/shared-order-app-actions")
}

/// The inference plane mounted on ONE shared engine, the way `core/router/src/lib.rs`
/// mounts it.
struct Emulator {
    engine: Arc<::engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        Self {
            engine: Arc::new(::engine::InMemoryEngine::new()),
        }
    }

    /// A router serving this service over the shared engine. Built per call on purpose:
    /// anything a later call observes came from engine state, not from a handle held
    /// across calls.
    fn router(&self) -> MultiServiceRouter {
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(runtime::EmulatedService::new(Arc::clone(&self.engine)));
        router
    }

    /// Dispatch one request that must succeed, and return its parsed body.
    async fn ok(&self, case: &str, route: &str, body: Value) -> Value {
        let request = ::http::Request::builder()
            .method("POST")
            .uri(format!("https://{HOST}{route}"))
            .header("host", HOST)
            .header("content-type", "application/json")
            .header("authorization", SIGV4_HEADER)
            .body(Bytes::from(body.to_string()))
            .expect("the test request is well formed");
        let response = self.router().dispatch(request).await;
        let status = response.status().as_u16();
        let payload = response.into_body();
        let parsed: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
        assert_eq!(
            status, 200,
            "case {case}: expected HTTP 200 from {route} for {body}, got {status} with \
             {parsed}"
        );
        parsed
    }
}

/// A deterministic Items catalog of `size` entries, whose popularity decreases with the
/// index and whose `GENRE` cycles.
fn items(size: usize) -> Vec<Item> {
    (0..size)
        .map(|index| {
            Item::new(format!("item-{index}"), (size - index) as f64)
                .with_attribute(COLUMN_GENRE, GENRES[index % GENRES.len()])
        })
        .collect()
}

/// A deterministic Actions catalog of `size` entries.
fn actions(size: usize) -> Vec<Action> {
    (0..size)
        .map(|index| {
            Action::new(format!("action-{index}"), (size - index) as f64)
                .with_attribute(COLUMN_CHANNEL, CHANNELS[index % CHANNELS.len()])
        })
        .collect()
}

/// The same Items catalog with every entry's popularity removed, so each entry scores
/// 0.0 and every pair of them ties.
fn flat_items(size: usize) -> Vec<Item> {
    items(size)
        .into_iter()
        .map(|item| Item {
            base_score: 0.0,
            ..item
        })
        .collect()
}

/// The same Actions catalog with every action's popularity removed.
fn flat_actions(size: usize) -> Vec<Action> {
    actions(size)
        .into_iter()
        .map(|action| Action {
            base_score: 0.0,
            ..action
        })
        .collect()
}

/// Every id of the seeded Items catalog, in catalog order.
fn catalog_ids() -> Vec<String> {
    items(CATALOG_SIZE)
        .into_iter()
        .map(|item| item.item_id)
        .collect()
}

/// Every id of the seeded Actions catalog, in catalog order.
fn action_ids() -> Vec<String> {
    actions(ACTION_CATALOG_SIZE)
        .into_iter()
        .map(|action| action.action_id)
        .collect()
}

/// The stored `GENRE` of one seeded Items entry — the column both Items filters read.
fn genre_of(index: usize) -> &'static str {
    GENRES[index % GENRES.len()]
}

/// The stored `CHANNEL` of one seeded Actions entry.
fn channel_of(index: usize) -> &'static str {
    CHANNELS[index % CHANNELS.len()]
}

/// The ids of the seeded Items catalog whose `GENRE` satisfies `keep`.
fn item_ids_where(keep: impl Fn(&str) -> bool) -> Vec<String> {
    (0..CATALOG_SIZE)
        .filter(|index| keep(genre_of(*index)))
        .map(|index| format!("item-{index}"))
        .collect()
}

/// The ids of the seeded Actions catalog whose `CHANNEL` satisfies `keep`.
fn action_ids_where(keep: impl Fn(&str) -> bool) -> Vec<String> {
    (0..ACTION_CATALOG_SIZE)
        .filter(|index| keep(channel_of(*index)))
        .map(|index| format!("action-{index}"))
        .collect()
}

/// An emulator holding the six campaigns and three filters these rows read.
fn seeded() -> Emulator {
    let emulator = Emulator::new();
    let engine = &*emulator.engine;

    let campaigns = [
        (
            campaign_ranking(),
            RecipeType::PersonalizedRanking,
            items(CATALOG_SIZE),
        ),
        (
            campaign_user(),
            RecipeType::UserPersonalization,
            items(CATALOG_SIZE),
        ),
        (
            campaign_flat_ranking(),
            RecipeType::PersonalizedRanking,
            flat_items(CATALOG_SIZE),
        ),
        (
            campaign_flat_user(),
            RecipeType::UserPersonalization,
            flat_items(CATALOG_SIZE),
        ),
    ];
    for (arn, recipe_type, catalog) in campaigns {
        assert!(
            registry::register_campaign(
                engine,
                Campaign::new(arn.as_str(), "shared-order", recipe_type, catalog),
            ),
            "case {arn}: the campaign seam must register the campaign"
        );
    }
    let action_campaigns = [
        (campaign_actions(), actions(ACTION_CATALOG_SIZE)),
        (campaign_flat_actions(), flat_actions(ACTION_CATALOG_SIZE)),
    ];
    for (arn, catalog) in action_campaigns {
        assert!(
            registry::register_campaign(
                engine,
                Campaign::new_actions(arn.as_str(), "shared-order", catalog),
            ),
            "case {arn}: the campaign seam must register the action campaign"
        );
    }

    let filters = [
        (
            filter_exclude_comedy(),
            "EXCLUDE ItemID WHERE Items.GENRE IN (\"Comedy\")",
        ),
        (
            filter_include_genre(),
            "INCLUDE ItemID WHERE Items.GENRE IN ($GENRE)",
        ),
        (
            filter_app_actions(),
            "INCLUDE ActionID WHERE Actions.CHANNEL IN (\"App\")",
        ),
    ];
    for (arn, expression) in filters {
        assert_eq!(
            registry::register_filter(engine, arn.as_str(), expression),
            Ok(true),
            "case {arn}: the filter seam must accept {expression:?}"
        );
    }
    emulator
}

/// The ids a list member reports, in response order.
fn ids(case: &str, response: &Value, member: &str, id_member: &str) -> Vec<String> {
    response
        .get(member)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case}: response {response} carries no {member}"))
        .iter()
        .map(|entry| {
            entry
                .get(id_member)
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case {case}: entry {entry} carries no {id_member}")
                })
                .to_string()
        })
        .collect()
}

/// The scores a list member reports, in response order.
fn scores(case: &str, response: &Value, member: &str) -> Vec<f64> {
    response
        .get(member)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case}: response {response} carries no {member}"))
        .iter()
        .map(|entry| {
            entry
                .get("score")
                .and_then(Value::as_f64)
                .unwrap_or_else(|| panic!("case {case}: entry {entry} carries no score"))
        })
        .collect()
}

/// Each reported score keyed by the entry it belongs to.
fn scores_by_id(
    case: &str,
    response: &Value,
    member: &str,
    id_member: &str,
) -> BTreeMap<String, f64> {
    ids(case, response, member, id_member)
        .into_iter()
        .zip(scores(case, response, member))
        .collect()
}

/// The first `count` entries of `candidates` as the shared order reports them: score
/// descending, the entry's own id settling a tie. Each candidate's score is read off the
/// whole-catalog reference, so the expectation is derived from what this request's scored
/// catalog gives it rather than written out.
fn head(
    reference: &BTreeMap<String, f64>,
    candidates: &[String],
    count: usize,
) -> Vec<String> {
    let mut ordered: Vec<(&String, f64)> = candidates
        .iter()
        .map(|id| {
            let score = *reference
                .get(id)
                .unwrap_or_else(|| panic!("{id} is not one of the scored candidates"));
            (id, score)
        })
        .collect();
    ordered.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.0.cmp(right.0))
    });
    ordered
        .into_iter()
        .take(count)
        .map(|(id, _)| id.clone())
        .collect()
}

/// The last step of the shared composition: the reported list never increases in score,
/// and each score is a share of the one unit of certainty.
fn assert_descending_and_bounded(case: &str, reported: &[f64]) {
    for pair in reported.windows(2) {
        assert!(
            pair[0] >= pair[1],
            "case {case}: the list is ordered by score descending, got {reported:?}"
        );
    }
    for score in reported {
        assert!(
            (0.0..=1.0).contains(score),
            "case {case}: every score is a share of 1.0, got {score} in {reported:?}"
        );
    }
}

#[tokio::test]
async fn every_inference_operation_settles_the_denominator_before_the_filter() {
    let emulator = seeded();
    let every_item = catalog_ids();
    let user_id = "shared-order-user-7";

    // The reference per operation: the same request over the WHOLE catalog with no
    // filter, which reports every scored candidate and therefore the whole certainty.
    let reference_rows: [(&str, Value, &str, &str, usize); 3] = [
        (
            RANKING,
            json!({
                "campaignArn": campaign_ranking(),
                "userId": user_id,
                "inputList": every_item,
            }),
            "personalizedRanking",
            "itemId",
            CATALOG_SIZE,
        ),
        (
            RECOMMENDATIONS,
            json!({
                "campaignArn": campaign_user(),
                "userId": user_id,
                "numResults": CATALOG_SIZE,
            }),
            "itemList",
            "itemId",
            CATALOG_SIZE,
        ),
        (
            ACTION_RECOMMENDATIONS,
            json!({
                "campaignArn": campaign_actions(),
                "userId": user_id,
                "numResults": ACTION_CATALOG_SIZE,
            }),
            "actionList",
            "actionId",
            ACTION_CATALOG_SIZE,
        ),
    ];
    let mut references: BTreeMap<&str, BTreeMap<String, f64>> = BTreeMap::new();
    for (route, body, member, id_member, size) in reference_rows {
        let case = format!("the whole catalog through {route}");
        let response = emulator.ok(&case, route, body).await;
        let reported = scores_by_id(&case, &response, member, id_member);
        assert_eq!(
            reported.len(),
            size,
            "case {case}: the reference must report every candidate, got {reported:?}"
        );
        assert_descending_and_bounded(&case, &scores(&case, &response, member));
        let total: f64 = reported.values().sum();
        assert!(
            (total - 1.0).abs() < TOLERANCE,
            "case {case}: a response reporting every candidate carries the whole \
             certainty, got {total}"
        );
        references.insert(route, reported);
    }

    // The rows: the same catalogs reported through a request that SELECTS part of them.
    // Every reported score must still be the reference's, which is only true when each
    // entry's share was settled over the whole scored catalog before the filter and the
    // selection ran.
    let not_comedy = item_ids_where(|genre| genre != "Comedy");
    let comedy_or_drama = item_ids_where(|genre| matches!(genre, "Comedy" | "Drama"));
    let app_actions = action_ids_where(|channel| channel == "App");
    let half: Vec<String> = every_item.iter().take(CATALOG_SIZE / 2).cloned().collect();
    let half_not_comedy: Vec<String> = half
        .iter()
        .filter(|id| not_comedy.contains(id))
        .cloned()
        .collect();
    let reversed: Vec<String> = every_item.iter().rev().cloned().collect();
    let with_unknown: Vec<String> = vec![
        UNKNOWN_ITEM.to_string(),
        every_item[4].clone(),
        every_item[1].clone(),
    ];

    let mut rows: Vec<Row> = vec![
        (
            "ranking, the whole inputList, EXCLUDE Comedy".to_string(),
            RANKING,
            json!({
                "campaignArn": campaign_ranking(),
                "userId": user_id,
                "inputList": every_item,
                "filterArn": filter_exclude_comedy(),
            }),
            "personalizedRanking",
            "itemId",
            not_comedy.clone(),
        ),
        (
            "ranking, half the inputList, EXCLUDE Comedy".to_string(),
            RANKING,
            json!({
                "campaignArn": campaign_ranking(),
                "userId": user_id,
                "inputList": half,
                "filterArn": filter_exclude_comedy(),
            }),
            "personalizedRanking",
            "itemId",
            half_not_comedy,
        ),
        (
            "ranking, the inputList reversed, INCLUDE GENRE Comedy or Drama".to_string(),
            RANKING,
            json!({
                "campaignArn": campaign_ranking(),
                "userId": user_id,
                "inputList": reversed,
                "filterArn": filter_include_genre(),
                "filterValues": { "GENRE": "Comedy,Drama" },
            }),
            "personalizedRanking",
            "itemId",
            comedy_or_drama.clone(),
        ),
        (
            // An entry the catalog never held carries no relevance for the user and no
            // share of the certainty, and is appended to the end of the reranked list.
            "ranking, an id the catalog does not hold".to_string(),
            RANKING,
            json!({
                "campaignArn": campaign_ranking(),
                "userId": user_id,
                "inputList": with_unknown,
            }),
            "personalizedRanking",
            "itemId",
            vec![
                UNKNOWN_ITEM.to_string(),
                every_item[4].clone(),
                every_item[1].clone(),
            ],
        ),
        (
            "recommendations, every result, EXCLUDE Comedy".to_string(),
            RECOMMENDATIONS,
            json!({
                "campaignArn": campaign_user(),
                "userId": user_id,
                "numResults": CATALOG_SIZE,
                "filterArn": filter_exclude_comedy(),
            }),
            "itemList",
            "itemId",
            not_comedy.clone(),
        ),
        (
            "recommendations, every result, INCLUDE GENRE Comedy or Drama".to_string(),
            RECOMMENDATIONS,
            json!({
                "campaignArn": campaign_user(),
                "userId": user_id,
                "numResults": CATALOG_SIZE,
                "filterArn": filter_include_genre(),
                "filterValues": { "GENRE": "Comedy,Drama" },
            }),
            "itemList",
            "itemId",
            comedy_or_drama,
        ),
        (
            "action recommendations, every result, INCLUDE CHANNEL App".to_string(),
            ACTION_RECOMMENDATIONS,
            json!({
                "campaignArn": campaign_actions(),
                "userId": user_id,
                "numResults": ACTION_CATALOG_SIZE,
                "filterArn": filter_app_actions(),
            }),
            "actionList",
            "actionId",
            app_actions,
        ),
    ];
    // Two rows that ask for FEWER results than survive, so the row set also spans the
    // selection member a response is cut by. Which survivors come back is the ranking's
    // own head, derived from the whole-catalog reference rather than written out here.
    for num_results in [1_usize, 3] {
        rows.push((
            format!("recommendations, {num_results} results, EXCLUDE Comedy"),
            RECOMMENDATIONS,
            json!({
                "campaignArn": campaign_user(),
                "userId": user_id,
                "numResults": num_results,
                "filterArn": filter_exclude_comedy(),
            }),
            "itemList",
            "itemId",
            head(&references[RECOMMENDATIONS], &not_comedy, num_results),
        ));
    }

    for (case, route, body, member, id_member, expected) in rows {
        let response = emulator.ok(&case, route, body).await;
        let reported = scores_by_id(&case, &response, member, id_member);
        let mut returned: Vec<String> = reported.keys().cloned().collect();
        let mut wanted = expected.clone();
        returned.sort();
        wanted.sort();
        assert_eq!(
            returned, wanted,
            "case {case}: the response must report exactly the entries the request \
             selected and the filter kept"
        );

        let reference = &references[route];
        for (id, score) in &reported {
            match reference.get(id) {
                Some(expected) => assert!(
                    (score - expected).abs() < TOLERANCE,
                    "case {case}: {id} scores {expected} over the whole scored catalog, \
                     got {score}"
                ),
                None => assert_eq!(
                    *score, 0.0,
                    "case {case}: {id} is no candidate of the catalog, so it carries no \
                     share of the certainty, got {score}"
                ),
            }
        }
        assert_descending_and_bounded(&case, &scores(&case, &response, member));

        // A response reporting part of the candidates carries part of the certainty: the
        // share of the ones it left out stays with them, which is the same statement as
        // "the denominator was settled first", read on the total.
        let total: f64 = reported.values().sum();
        if reported.len() < reference.len() {
            assert!(
                total < 1.0 - TOLERANCE,
                "case {case}: {} of {} candidates may not carry the whole certainty, got \
                 {total}",
                reported.len(),
                reference.len()
            );
        }
    }
}

#[tokio::test]
async fn a_tied_pair_is_settled_by_each_operations_own_key() {
    let emulator = seeded();
    let catalog = catalog_ids();

    // A catalog whose entries carry no popularity scores every entry 0.0, so EVERY pair
    // ties and the tie-break key alone decides the response order — the one part of the
    // shared composition each operation states for itself.
    let reversed: Vec<String> = catalog.iter().rev().cloned().collect();
    let rotated: Vec<String> = catalog[5..]
        .iter()
        .chain(catalog[..5].iter())
        .cloned()
        .collect();
    let mut ascending_items = catalog.clone();
    ascending_items.sort();
    let mut ascending_actions = action_ids();
    ascending_actions.sort();

    let rows: Vec<Row> = vec![
        (
            // The caller's `inputList` position: the order it sent, whatever that is.
            "ranking, the inputList in catalog order".to_string(),
            RANKING,
            json!({
                "campaignArn": campaign_flat_ranking(),
                "userId": "tie-user",
                "inputList": catalog,
            }),
            "personalizedRanking",
            "itemId",
            catalog.clone(),
        ),
        (
            "ranking, the inputList reversed".to_string(),
            RANKING,
            json!({
                "campaignArn": campaign_flat_ranking(),
                "userId": "tie-user",
                "inputList": reversed.clone(),
            }),
            "personalizedRanking",
            "itemId",
            reversed,
        ),
        (
            "ranking, the inputList rotated by five".to_string(),
            RANKING,
            json!({
                "campaignArn": campaign_flat_ranking(),
                "userId": "another-tie-user",
                "inputList": rotated.clone(),
            }),
            "personalizedRanking",
            "itemId",
            rotated,
        ),
        (
            // The entry's own id, ascending, where the operation chooses the list.
            "recommendations, the whole catalog".to_string(),
            RECOMMENDATIONS,
            json!({
                "campaignArn": campaign_flat_user(),
                "userId": "tie-user",
                "numResults": CATALOG_SIZE,
            }),
            "itemList",
            "itemId",
            ascending_items.clone(),
        ),
        (
            "recommendations, the first three".to_string(),
            RECOMMENDATIONS,
            json!({
                "campaignArn": campaign_flat_user(),
                "userId": "tie-user",
                "numResults": 3,
            }),
            "itemList",
            "itemId",
            ascending_items[..3].to_vec(),
        ),
        (
            "action recommendations, the whole catalog".to_string(),
            ACTION_RECOMMENDATIONS,
            json!({
                "campaignArn": campaign_flat_actions(),
                "userId": "tie-user",
                "numResults": ACTION_CATALOG_SIZE,
            }),
            "actionList",
            "actionId",
            ascending_actions.clone(),
        ),
        (
            "action recommendations, the first two".to_string(),
            ACTION_RECOMMENDATIONS,
            json!({
                "campaignArn": campaign_flat_actions(),
                "userId": "tie-user",
                "numResults": 2,
            }),
            "actionList",
            "actionId",
            ascending_actions[..2].to_vec(),
        ),
    ];

    for (case, route, body, member, id_member, expected) in rows {
        let response = emulator.ok(&case, route, body).await;
        let returned = ids(&case, &response, member, id_member);
        assert_eq!(
            returned, expected,
            "case {case}: every score ties, so this operation's own tie-break key must \
             order the response {expected:?}, got {returned:?}"
        );
        let reported = scores(&case, &response, member);
        assert!(
            reported.iter().all(|score| *score == 0.0),
            "case {case}: a catalog carrying no popularity has no certainty to \
             apportion, got {reported:?}"
        );
    }
}
