// SPDX-License-Identifier: CC0-1.0

//! Which request members may move a `GetRecommendations` response at all.
//!
//! "USER_PERSONALIZATION - `userId` required, `itemId` not used RELATED_ITEMS -
//! `itemId` required, `userId` not used" (`GetRecommendations` doc): a resolved
//! resource reads exactly the ONE personalization input it needs, so the other one is
//! ignored — sending it, or changing it, leaves the returned items, their order and
//! their scores untouched. A resource whose domain use case needs neither input is not
//! personalized, so it answers every caller the same way.
//!
//! The rule is asserted over a TABLE: every registered resource kind (a
//! `USER_PERSONALIZATION` campaign, a `RELATED_ITEMS` campaign and one recommender for
//! each entry of [`RECOMMENDER_USE_CASES`]) crossed with the forms the UNUSED input can
//! take (absent, empty, an id the catalog holds, an id it does not, a different case).
//! Each row is compared against ONE reference: the same request with the unused input
//! left out entirely.
//!
//! The companion suite asserts the rule is not vacuous — the input a resource DOES use
//! moves the response — and that a reported score stays a property of the
//! (resource, personalization input, entry) tuple however many results are asked for.
//!
//! The rule covers every path a personalization input can reach, not only the score: a
//! filter clause over an interaction dataset reads the interactions recorded by the user
//! the request personalizes FOR, so a resource that personalizes for an item, or for
//! nothing at all, reads no interaction rows however many `userId`s a caller sends it.
//! The second half of this suite drives that path end to end over the INGESTED stream,
//! both through the request's own `filterArn` and through a promotion's.

use std::time::SystemTime;

use serde_json::{json, Value};

use service_com_amazonaws_personalizeevents_amazonpersonalizeevents::state as ingestion;
use service_com_amazonaws_personalizeruntime_amazonpersonalizeruntime::state::{
    self as registry, Campaign, RecipeType, Recommender, RequiredInput, RECOMMENDER_USE_CASES,
};

use crate::support::{
    assert_descending_and_bounded, assert_sums_to_one, entries, ids, items, scores,
    scores_by_id, Emulator, RECOMMENDATIONS,
};

/// The `USER_PERSONALIZATION` campaign every user row reads.
const CAMPAIGN_USER: &str =
    "arn:aws:personalize:us-east-1:000000000000:campaign/unused-input-user";
/// The `RELATED_ITEMS` campaign every item row reads.
const CAMPAIGN_RELATED: &str =
    "arn:aws:personalize:us-east-1:000000000000:campaign/unused-input-related";
/// The `USER_PERSONALIZATION` campaign FITTED to the dataset group whose ingested
/// interactions the filter rows read.
const CAMPAIGN_USER_FITTED: &str =
    "arn:aws:personalize:us-east-1:000000000000:campaign/unused-input-fitted-user";
/// Its `RELATED_ITEMS` counterpart, fitted to the same group.
const CAMPAIGN_RELATED_FITTED: &str =
    "arn:aws:personalize:us-east-1:000000000000:campaign/unused-input-fitted-related";

/// How many entries every seeded Items catalog holds.
const CATALOG_SIZE: usize = 40;

/// Two responses may not report an entry's certainty differently at all, so scores are
/// compared up to double-rounding noise only.
const TOLERANCE: f64 = 1e-12;

/// One resolved resource a row recommends from: how it reads in a failure message, the
/// request member naming it, its ARN, and the personalization input it USES.
struct Resource {
    case: String,
    member: &'static str,
    arn: String,
    uses: RequiredInput,
}

/// An emulator holding a campaign of each `GetRecommendations` recipe type and one
/// recommender per domain use case, all over the same seeded Items catalog.
fn seeded() -> (Emulator, Vec<Resource>) {
    let emulator = Emulator::new();
    let engine = &*emulator.engine;

    let campaigns = [
        (
            "USER_PERSONALIZATION campaign",
            CAMPAIGN_USER,
            RecipeType::UserPersonalization,
            RequiredInput::User,
        ),
        (
            "RELATED_ITEMS campaign",
            CAMPAIGN_RELATED,
            RecipeType::RelatedItems,
            RequiredInput::Item,
        ),
    ];
    let mut resources: Vec<Resource> = Vec::new();
    for (case, arn, recipe_type, uses) in campaigns {
        assert!(
            registry::register_campaign(
                engine,
                Campaign::new(arn, "unused-input", recipe_type, items(CATALOG_SIZE)),
            ),
            "case {case}: the campaign seam must register the campaign"
        );
        resources.push(Resource {
            case: case.to_string(),
            member: "campaignArn",
            arn: arn.to_string(),
            uses,
        });
    }

    // Every entry of the use-case catalog, not only the ones the issue cited: each use
    // case needs its own input, so each decides on its own which member is unused.
    for (index, (use_case, uses)) in RECOMMENDER_USE_CASES.iter().enumerate() {
        let arn = format!(
            "arn:aws:personalize:us-east-1:000000000000:recommender/unused-input-{index}"
        );
        assert!(
            registry::register_recommender(
                engine,
                Recommender::new(&arn, "unused-input", *use_case, items(CATALOG_SIZE)),
            ),
            "case {use_case}: the recommender seam must register the recommender"
        );
        resources.push(Resource {
            case: format!("recommender {use_case:?}"),
            member: "recommenderArn",
            arn,
            uses: *uses,
        });
    }

    (emulator, resources)
}

/// A `GetRecommendations` body naming `resource`, with the personalization members the
/// row carries.
fn request(
    resource: &Resource,
    user_id: Option<&str>,
    item_id: Option<&str>,
    num_results: usize,
) -> Value {
    let mut body = json!({ "numResults": num_results });
    body[resource.member] = json!(resource.arn);
    if let Some(user_id) = user_id {
        body["userId"] = json!(user_id);
    }
    if let Some(item_id) = item_id {
        body["itemId"] = json!(item_id);
    }
    body
}

/// The value of the input `resource` uses — the one it needs in the request.
fn used_input(resource: &Resource) -> (Option<&'static str>, Option<&'static str>) {
    match resource.uses {
        RequiredInput::User => (Some("user-7"), None),
        RequiredInput::Item => (None, Some("item-5")),
        RequiredInput::Nothing => (None, None),
    }
}

/// The forms an unused `userId` can take: absent, empty (no id at all on the wire),
/// ids of several shapes, and an id differing from another row's only in case.
const UNUSED_USER_IDS: [Option<&str>; 6] = [
    None,
    Some(""),
    Some("user-1"),
    Some("user-2"),
    Some("USER-1"),
    Some("a-user-that-never-interacted"),
];

/// The forms an unused `itemId` can take, including ids the catalog holds (the first
/// entry, a middle one, the last one) and one it never held.
const UNUSED_ITEM_IDS: [Option<&str>; 6] = [
    None,
    Some(""),
    Some("item-0"),
    Some("item-3"),
    Some("item-39"),
    Some("no-such-item"),
];

#[tokio::test]
async fn an_input_the_resolved_resource_does_not_use_is_ignored() {
    let (emulator, resources) = seeded();
    let num_results = 10;

    for resource in &resources {
        let (used_user, used_item) = used_input(resource);
        let reference = emulator
            .ok(
                RECOMMENDATIONS,
                request(resource, used_user, used_item, num_results),
            )
            .await;
        let expected = entries(&reference, "itemList");
        assert_eq!(
            expected.len(),
            num_results,
            "case {}: the reference must report {num_results} items, got {reference}",
            resource.case
        );
        assert_descending_and_bounded(&resource.case, &scores(&reference, "itemList"));

        // The unused member is whichever one the resource does not use; a resource that
        // uses neither has both unused, so both vary.
        let only_used_user = [used_user];
        let only_used_item = [used_item];
        let user_rows: &[Option<&str>] = if resource.uses == RequiredInput::User {
            &only_used_user
        } else {
            &UNUSED_USER_IDS
        };
        let item_rows: &[Option<&str>] = if resource.uses == RequiredInput::Item {
            &only_used_item
        } else {
            &UNUSED_ITEM_IDS
        };

        for user_id in user_rows {
            for item_id in item_rows {
                let case = format!(
                    "{} with userId {user_id:?} and itemId {item_id:?}",
                    resource.case
                );
                let response = emulator
                    .ok(
                        RECOMMENDATIONS,
                        request(resource, *user_id, *item_id, num_results),
                    )
                    .await;
                assert_eq!(
                    entries(&response, "itemList"),
                    expected,
                    "case {case}: the input this resource does not use must not change the \
                     response, got {response}"
                );
            }
        }
    }
}

#[tokio::test]
async fn the_input_the_resolved_resource_uses_moves_the_response() {
    let (emulator, resources) = seeded();
    let num_results = CATALOG_SIZE;

    // The rule above would hold trivially if nothing personalized the response, so the
    // input each resource DOES use must move it. A use case that needs neither input is
    // not personalized at all, and is asserted to answer every caller the same way.
    for resource in &resources {
        match resource.uses {
            RequiredInput::User => {
                let first = emulator
                    .ok(
                        RECOMMENDATIONS,
                        request(resource, Some("user-1"), None, num_results),
                    )
                    .await;
                let second = emulator
                    .ok(
                        RECOMMENDATIONS,
                        request(resource, Some("user-2"), None, num_results),
                    )
                    .await;
                assert_ne!(
                    ids(&first, "itemList", "itemId"),
                    ids(&second, "itemList", "itemId"),
                    "case {}: userId is the input this resource uses, so two users must not \
                     be ranked identically",
                    resource.case
                );
            }
            RequiredInput::Item => {
                let first = emulator
                    .ok(
                        RECOMMENDATIONS,
                        request(resource, None, Some("item-3"), num_results),
                    )
                    .await;
                let second = emulator
                    .ok(
                        RECOMMENDATIONS,
                        request(resource, None, Some("item-4"), num_results),
                    )
                    .await;
                assert_ne!(
                    ids(&first, "itemList", "itemId"),
                    ids(&second, "itemList", "itemId"),
                    "case {}: itemId is the input this resource uses, so two queried items \
                     must not return the same related items",
                    resource.case
                );
            }
            RequiredInput::Nothing => {
                let first = emulator
                    .ok(
                        RECOMMENDATIONS,
                        request(resource, Some("user-1"), None, num_results),
                    )
                    .await;
                let second = emulator
                    .ok(
                        RECOMMENDATIONS,
                        request(resource, Some("user-2"), Some("item-3"), num_results),
                    )
                    .await;
                assert_eq!(
                    entries(&first, "itemList"),
                    entries(&second, "itemList"),
                    "case {}: this use case needs no personalization input, so it must \
                     answer every caller the same way",
                    resource.case
                );
            }
        }
    }
}

#[tokio::test]
async fn a_reported_score_does_not_change_with_num_results() {
    let (emulator, resources) = seeded();

    // `PredictedItem$score` is "a numeric representation of the model's certainty that
    // the item will be the next user selection": the certainty for one (resource,
    // personalization input, item) tuple. `numResults` only says how many of the scored
    // catalog to report, so it may not move the number reported for an item, and a
    // response carrying part of the catalog carries part of the certainty.
    for resource in &resources {
        let (used_user, used_item) = used_input(resource);
        // A RELATED_ITEMS resource never recommends the queried item, so its scored
        // candidate set is the catalog minus that one entry.
        let candidates = if resource.uses == RequiredInput::Item {
            CATALOG_SIZE - 1
        } else {
            CATALOG_SIZE
        };

        let whole = emulator
            .ok(
                RECOMMENDATIONS,
                request(resource, used_user, used_item, CATALOG_SIZE),
            )
            .await;
        let reference = scores_by_id(&whole, "itemList", "itemId");
        assert_eq!(
            reference.len(),
            candidates,
            "case {}: expected {candidates} scored candidates, got {reference:?}",
            resource.case
        );
        // Reporting every scored candidate reports the whole certainty.
        assert_sums_to_one(&resource.case, &scores(&whole, "itemList"));

        for num_results in [1_usize, 2, 5, 10, 25, candidates] {
            let case = format!("{} numResults {num_results}", resource.case);
            let response = emulator
                .ok(
                    RECOMMENDATIONS,
                    request(resource, used_user, used_item, num_results),
                )
                .await;
            let reported = scores_by_id(&response, "itemList", "itemId");
            assert_eq!(
                reported.len(),
                num_results.min(candidates),
                "case {case}: expected {} items, got {reported:?}",
                num_results.min(candidates)
            );
            for (item_id, score) in &reported {
                let expected = reference.get(item_id).unwrap_or_else(|| {
                    panic!("case {case}: {item_id} is not a scored candidate of {reference:?}")
                });
                assert!(
                    (score - expected).abs() < TOLERANCE,
                    "case {case}: {item_id} scores {expected} over the whole scored catalog, \
                     got {score}"
                );
            }
            assert_descending_and_bounded(&case, &scores(&response, "itemList"));

            // A response that reports fewer entries than the catalog scored carries
            // strictly less than the whole certainty — asking for one result never
            // reports a certainty of 1.0.
            let total: f64 = reported.values().sum();
            if num_results < candidates {
                assert!(
                    total < 1.0 - 1e-9,
                    "case {case}: {num_results} of {candidates} scored candidates must carry \
                     part of the certainty, got {total}"
                );
            }
        }
    }
}

// The same rule on the FILTER path: a clause over an interaction dataset reads the rows
// of the user the request personalizes FOR, which is its `userId` only when the resolved
// resource uses that input.

/// The dataset group every fitted resource below belongs to, whose INGESTED stream a
/// filter clause over the `Interactions` dataset reads. A Personalize dataset group's ARN
/// resource is `dataset-group/<name>`.
const GROUP: &str = "arn:aws:personalize:us-east-1:000000000000:dataset-group/unused-input";
/// The tracking id the group's event tracker is addressed by.
const TRACKING_ID: &str = "tracker-unused-input";
/// `PutEvents`: `POST /events` on the Amazon Personalize Events data plane.
const EVENTS: &str = "/events";
/// `Event$sentAt`: the client-side instant of an interaction, in the Unix time the
/// member carries. Fixed, so nothing here depends on the wall clock.
const SENT_AT: f64 = 1_764_000_000.0;

/// `EXCLUDE ItemID WHERE Interactions.EVENT_TYPE IN ("purchase")` — a clause over the
/// interaction dataset, which is the only kind of clause a personalization input can
/// reach.
const FILTER_EXCLUDE_PURCHASED: &str =
    "arn:aws:personalize:us-east-1:000000000000:filter/unused-input-exclude-purchased";
/// Its `INCLUDE` counterpart, whose selection is EMPTY for a request that recorded
/// nothing — so the rule is asserted from both sides of the same clause.
const FILTER_INCLUDE_PURCHASED: &str =
    "arn:aws:personalize:us-east-1:000000000000:filter/unused-input-include-purchased";

/// How many entries the catalog of every fitted resource holds. Small on purpose: the
/// rule is about which rows a clause reads, not about catalog size, which the table
/// above spans.
const FITTED_CATALOG_SIZE: usize = 8;

/// The interactions this half of the suite records through `PutEvents`, as
/// `(userId, itemId, eventType)`: two users, two event types, and a user with several
/// rows — the axes an interaction clause reads over.
const RECORDED: [(&str, &str, &str); 4] = [
    ("user-1", "item-1", "purchase"),
    ("user-1", "item-3", "purchase"),
    ("user-1", "item-6", "watch"),
    ("user-2", "item-2", "purchase"),
];

/// The forms a SPURIOUS `userId` can take on a request against a resource that does not
/// use it: absent, empty, the user who recorded the most rows, another user who recorded
/// one, and an id differing from the first only in case.
const SPURIOUS_USER_IDS: [Option<&str>; 5] = [
    None,
    Some(""),
    Some("user-1"),
    Some("user-2"),
    Some("USER-1"),
];

/// An emulator with the ingestion data plane mounted beside this one, the group's event
/// tracker registered, the two interaction filters, and one resource of every kind FITTED
/// to the group — so everything the suite records afterwards is live data its inference
/// reads.
fn seeded_with_interactions() -> (Emulator, Vec<Resource>) {
    let emulator = Emulator::with_ingestion();
    let engine = &*emulator.engine;

    {
        let store = engine
            .state::<ingestion::ComAmazonawsPersonalizeeventsAmazonpersonalizeeventsState>();
        let mut store = store.lock();
        store
            .register_event_tracker(TRACKING_ID, Some(GROUP.to_string()))
            .expect("the ingestion store accepts the event tracker");
    }

    // Fitted BEFORE anything is recorded, so every interaction below is one the fitted
    // catalog does not already count.
    let fitted_at = SystemTime::now();
    let mut resources: Vec<Resource> = Vec::new();
    for (case, arn, recipe_type, uses) in [
        (
            "fitted USER_PERSONALIZATION campaign",
            CAMPAIGN_USER_FITTED,
            RecipeType::UserPersonalization,
            RequiredInput::User,
        ),
        (
            "fitted RELATED_ITEMS campaign",
            CAMPAIGN_RELATED_FITTED,
            RecipeType::RelatedItems,
            RequiredInput::Item,
        ),
    ] {
        assert!(
            registry::register_campaign(
                engine,
                Campaign::new(arn, "unused-input", recipe_type, items(FITTED_CATALOG_SIZE))
                    .fitted_to(GROUP, fitted_at),
            ),
            "case {case}: the campaign seam must register the campaign"
        );
        resources.push(Resource {
            case: case.to_string(),
            member: "campaignArn",
            arn: arn.to_string(),
            uses,
        });
    }
    // Every domain use case, not only the ones the issue cited: each decides on its own
    // whether `userId` is an input it uses at all.
    for (index, (use_case, uses)) in RECOMMENDER_USE_CASES.iter().enumerate() {
        let arn = format!(
            "arn:aws:personalize:us-east-1:000000000000:recommender/unused-input-fitted-{index}"
        );
        assert!(
            registry::register_recommender(
                engine,
                Recommender::new(&arn, "unused-input", *use_case, items(FITTED_CATALOG_SIZE))
                    .fitted_to(GROUP, fitted_at),
            ),
            "case {use_case}: the recommender seam must register the recommender"
        );
        resources.push(Resource {
            case: format!("fitted recommender {use_case:?}"),
            member: "recommenderArn",
            arn,
            uses: *uses,
        });
    }

    for (arn, expression) in [
        (
            FILTER_EXCLUDE_PURCHASED,
            "EXCLUDE ItemID WHERE Interactions.EVENT_TYPE IN (\"purchase\")",
        ),
        (
            FILTER_INCLUDE_PURCHASED,
            "INCLUDE ItemID WHERE Interactions.EVENT_TYPE IN (\"purchase\")",
        ),
    ] {
        assert_eq!(
            registry::register_filter(engine, arn, expression),
            Ok(true),
            "case {arn}: the filter seam must accept {expression:?}"
        );
    }

    (emulator, resources)
}

/// Record [`RECORDED`] through `PutEvents`, one call per row: the user is a member of the
/// CALL, so a row of another user is another call.
async fn record_interactions(emulator: &Emulator) {
    for (index, (user, item, event_type)) in RECORDED.iter().enumerate() {
        emulator
            .ingest(
                EVENTS,
                json!({
                    "trackingId": TRACKING_ID,
                    "sessionId": format!("session-{user}"),
                    "userId": user,
                    "eventList": [{
                        "eventType": event_type,
                        "sentAt": SENT_AT,
                        "itemId": item,
                        "eventId": format!("unused-input-{index}"),
                    }],
                }),
            )
            .await;
    }
}

/// A `GetRecommendations` body naming a fitted `resource`: the item input the resource
/// needs, the `userId` the row carries, the request's filter, and a promotion over a
/// filter of its own when the row names one.
fn filtered_request(
    resource: &Resource,
    user_id: Option<&str>,
    filter_arn: Option<&str>,
    promotion_filter_arn: Option<&str>,
) -> Value {
    let mut body = json!({ "numResults": FITTED_CATALOG_SIZE });
    body[resource.member] = json!(resource.arn);
    if let Some(user_id) = user_id {
        body["userId"] = json!(user_id);
    }
    if let Some(item_id) = used_input(resource).1 {
        body["itemId"] = json!(item_id);
    }
    if let Some(filter_arn) = filter_arn {
        body["filterArn"] = json!(filter_arn);
    }
    if let Some(promotion_filter_arn) = promotion_filter_arn {
        body["promotions"] = json!([{
            "name": "unused-input-promotion",
            "percentPromotedItems": 50,
            "filterArn": promotion_filter_arn,
        }]);
    }
    body
}

/// `ids` sorted, for a comparison against a set of entries whose order the score decides.
fn sorted(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids
}

/// The entries a request against `resource` scores at all: its catalog, minus the entry a
/// related-items request queried (which is never recommended back).
fn candidate_ids(resource: &Resource) -> Vec<String> {
    let queried = used_input(resource).1.unwrap_or_default();
    sorted(
        items(FITTED_CATALOG_SIZE)
            .into_iter()
            .map(|item| item.item_id)
            .filter(|item_id| !(resource.uses == RequiredInput::Item && item_id == queried))
            .collect(),
    )
}

/// The catalog entries `user` recorded a `purchase` for in [`RECORDED`] — the expectation
/// side of the interaction clause, read off the same table the events are put from.
fn purchased_by(user: &str) -> Vec<String> {
    sorted(
        RECORDED
            .iter()
            .filter(|(row_user, _, event_type)| *row_user == user && *event_type == "purchase")
            .map(|(_, item, _)| (*item).to_string())
            .collect(),
    )
}

/// The ids of the entries a response reports as PROMOTED, sorted.
fn promoted_ids(response: &Value) -> Vec<String> {
    sorted(
        entries(response, "itemList")
            .iter()
            .filter(|entry| entry.get("promotionName").is_some())
            .map(|entry| {
                entry
                    .get("itemId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            })
            .collect(),
    )
}

#[tokio::test]
async fn an_interaction_clause_reads_no_rows_for_a_resource_that_uses_no_user_input() {
    let (emulator, resources) = seeded_with_interactions();
    record_interactions(&emulator).await;

    // Each row is a resource that does NOT use `userId`, a filter whose clause reads the
    // interaction dataset, and the form the spurious `userId` takes. Such a request
    // personalizes for no user, so it recorded no interaction: an EXCLUDE clause removes
    // nothing and an INCLUDE clause selects nothing, whatever `userId` the caller sends.
    for resource in resources
        .iter()
        .filter(|resource| resource.uses != RequiredInput::User)
    {
        let candidates = candidate_ids(resource);
        let rows: [(&str, &str, Vec<String>); 2] = [
            (
                "EXCLUDE a purchase",
                FILTER_EXCLUDE_PURCHASED,
                candidates.clone(),
            ),
            ("INCLUDE a purchase", FILTER_INCLUDE_PURCHASED, Vec::new()),
        ];
        for (clause, filter_arn, expected) in rows {
            let case = format!("{} with {clause}", resource.case);
            let reference = emulator
                .ok(
                    RECOMMENDATIONS,
                    filtered_request(resource, None, Some(filter_arn), None),
                )
                .await;
            let reported = sorted(ids(&reference, "itemList", "itemId"));
            assert_eq!(
                reported, expected,
                "case {case}: this resource uses no user input, so the request \
                 personalizes for no user and recorded no interaction: expected \
                 {expected:?}, got {reported:?} in {reference}"
            );

            for spurious in SPURIOUS_USER_IDS {
                let case = format!("{case} and userId {spurious:?}");
                let response = emulator
                    .ok(
                        RECOMMENDATIONS,
                        filtered_request(resource, spurious, Some(filter_arn), None),
                    )
                    .await;
                assert_eq!(
                    entries(&response, "itemList"),
                    entries(&reference, "itemList"),
                    "case {case}: the input this resource does not use may not change which \
                     entries the filter leaves, got {response}"
                );
            }
        }
    }
}

#[tokio::test]
async fn an_interaction_clause_reads_the_rows_of_the_user_a_resource_does_personalize_for() {
    let (emulator, resources) = seeded_with_interactions();
    record_interactions(&emulator).await;

    // The rule above would hold trivially if an interaction clause read nothing at all,
    // so for the resources that DO use `userId` the same clause must read exactly that
    // user's rows: a user who recorded purchases, a user who recorded one, and a user who
    // recorded nothing.
    for resource in resources
        .iter()
        .filter(|resource| resource.uses == RequiredInput::User)
    {
        for user in ["user-1", "user-2", "user-never-interacted"] {
            let purchased = purchased_by(user);
            let kept: Vec<String> = candidate_ids(resource)
                .into_iter()
                .filter(|item_id| !purchased.contains(item_id))
                .collect();
            let rows: [(&str, &str, Vec<String>); 2] = [
                ("EXCLUDE a purchase", FILTER_EXCLUDE_PURCHASED, kept),
                (
                    "INCLUDE a purchase",
                    FILTER_INCLUDE_PURCHASED,
                    purchased.clone(),
                ),
            ];
            for (clause, filter_arn, expected) in rows {
                let case = format!("{} for {user} with {clause}", resource.case);
                let response = emulator
                    .ok(
                        RECOMMENDATIONS,
                        filtered_request(resource, Some(user), Some(filter_arn), None),
                    )
                    .await;
                let reported = sorted(ids(&response, "itemList", "itemId"));
                assert_eq!(
                    reported, expected,
                    "case {case}: the clause reads the rows this user recorded, so expected \
                     {expected:?}, got {reported:?} in {response}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_promotion_clause_reads_the_same_user_as_the_request_s_own_filter() {
    let (emulator, resources) = seeded_with_interactions();
    record_interactions(&emulator).await;

    // A promotion's filter selects which entries take the promoted slots, from the same
    // interactions — so it reads the same user, and a resource that uses no user input
    // promotes nothing however many `userId`s the request carries.
    for resource in &resources {
        if resource.uses == RequiredInput::User {
            let case = format!("{} promoting its user's purchases", resource.case);
            let response = emulator
                .ok(
                    RECOMMENDATIONS,
                    filtered_request(resource, Some("user-1"), None, Some(FILTER_INCLUDE_PURCHASED)),
                )
                .await;
            let promoted = promoted_ids(&response);
            let expected = purchased_by("user-1");
            assert_eq!(
                promoted, expected,
                "case {case}: the promotion's clause reads this user's rows, so expected \
                 {expected:?} promoted, got {promoted:?} in {response}"
            );
            continue;
        }

        let case = format!("{} with a promotion over interactions", resource.case);
        let reference = emulator
            .ok(
                RECOMMENDATIONS,
                filtered_request(resource, None, None, Some(FILTER_INCLUDE_PURCHASED)),
            )
            .await;
        let promoted = promoted_ids(&reference);
        assert!(
            promoted.is_empty(),
            "case {case}: this resource uses no user input, so the request recorded no \
             interaction and the promotion selects nothing, got {promoted:?} in {reference}"
        );
        for spurious in SPURIOUS_USER_IDS {
            let case = format!("{case} and userId {spurious:?}");
            let response = emulator
                .ok(
                    RECOMMENDATIONS,
                    filtered_request(resource, spurious, None, Some(FILTER_INCLUDE_PURCHASED)),
                )
                .await;
            assert_eq!(
                entries(&response, "itemList"),
                entries(&reference, "itemList"),
                "case {case}: the input this resource does not use may not change which \
                 entries the promotion takes, got {response}"
            );
        }
    }
}
