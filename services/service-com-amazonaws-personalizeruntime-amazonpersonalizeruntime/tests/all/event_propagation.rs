// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the propagation of INGESTED data into the three inference
//! operations: what a caller streams into a dataset group through Amazon Personalize
//! Events changes what a campaign of that group returns.
//!
//! Both services are mounted on ONE engine (`Emulator::with_ingestion`) and every call
//! goes over the wire — the interactions and the item records are PUT through the
//! ingestion operations, never written into a store by hand, and what they changed is read
//! back through an inference response.
//!
//! Each suite asserts a RULE over a table whose rows differ along what the rule
//! generalises over: which interaction dataset a filter clause reads, which column of it,
//! which predicate, which user the request personalizes for, and how many interactions
//! were recorded. The "exclude what this user purchased" case the issue cites is one row.

use std::collections::BTreeMap;
use std::time::SystemTime;

use serde_json::{json, Value};

use service_com_amazonaws_personalizeevents_amazonpersonalizeevents::state as ingestion;
use service_com_amazonaws_personalizeruntime_amazonpersonalizeruntime::state::{
    self as registry, Campaign, Item, RecipeType,
};

use crate::support::{
    actions, assert_descending_and_bounded, entries, ids, items, scores, scores_by_id, Emulator,
    ACTION_RECOMMENDATIONS, COLUMN_GENRE, COLUMN_YEAR, PERSONALIZE_RANKING, RECOMMENDATIONS,
};

/// The dataset group every resource of this suite belongs to. A Personalize dataset
/// group's ARN resource is `dataset-group/<name>`.
const GROUP: &str = "arn:aws:personalize:us-east-1:000000000000:dataset-group/propagation";
/// The group's Items dataset. A dataset's ARN resource is
/// `dataset/<dataset group name>/<DATASET_TYPE>`, which is what ties it to the group.
const ITEMS_DATASET: &str = "arn:aws:personalize:us-east-1:000000000000:dataset/propagation/ITEMS";
/// The group's Actions dataset.
const ACTIONS_DATASET: &str =
    "arn:aws:personalize:us-east-1:000000000000:dataset/propagation/ACTIONS";
/// A dataset of ANOTHER group, to pin that the join is by group and not by account.
const OTHER_ITEMS_DATASET: &str =
    "arn:aws:personalize:us-east-1:000000000000:dataset/elsewhere/ITEMS";
/// The tracking id the group's event tracker is addressed by.
const TRACKING_ID: &str = "tracker-propagation";
/// A SECOND event tracker of the same dataset group: one tracker retains only its most
/// recent [`ingestion::MAX_EVENTS_PER_TRACKER`] events, and a group may hold many, so the
/// rows a suite needs past that bound are recorded through this one.
const TRACKING_ID_SECOND: &str = "tracker-propagation-second";

/// A `USER_PERSONALIZATION` campaign of the group, with metadata in recommendations
/// enabled.
const CAMPAIGN_USER: &str =
    "arn:aws:personalize:us-east-1:000000000000:campaign/propagation-user";
/// A `PERSONALIZED_RANKING` campaign of the group, with metadata enabled.
const CAMPAIGN_RANKING: &str =
    "arn:aws:personalize:us-east-1:000000000000:campaign/propagation-ranking";
/// A `PERSONALIZED_ACTIONS` campaign of the group.
const CAMPAIGN_ACTIONS: &str =
    "arn:aws:personalize:us-east-1:000000000000:campaign/propagation-actions";
/// A `USER_PERSONALIZATION` campaign fitted to NO dataset group: nothing a caller
/// ingests is its data.
const CAMPAIGN_UNFITTED: &str =
    "arn:aws:personalize:us-east-1:000000000000:campaign/propagation-unfitted";

/// `EXCLUDE ItemID WHERE Interactions.EVENT_TYPE IN ("purchase")` — the canonical
/// published interaction filter.
const FILTER_EXCLUDE_PURCHASED: &str =
    "arn:aws:personalize:us-east-1:000000000000:filter/exclude-purchased";
/// `INCLUDE ItemID WHERE Interactions.EVENT_TYPE IN ("purchase")`.
const FILTER_INCLUDE_PURCHASED: &str =
    "arn:aws:personalize:us-east-1:000000000000:filter/include-purchased";
/// `EXCLUDE ItemID WHERE Interactions.EVENT_TYPE IN ($EVENT_TYPE)`.
const FILTER_EXCLUDE_EVENT_TYPE: &str =
    "arn:aws:personalize:us-east-1:000000000000:filter/exclude-event-type";
/// `EXCLUDE ItemID WHERE Interactions.EVENT_TYPE NOT IN ("purchase")`.
const FILTER_EXCLUDE_NOT_PURCHASED: &str =
    "arn:aws:personalize:us-east-1:000000000000:filter/exclude-not-purchased";
/// `EXCLUDE ItemID WHERE Interactions.EVENT_VALUE > 10`.
const FILTER_EXCLUDE_HIGH_VALUE: &str =
    "arn:aws:personalize:us-east-1:000000000000:filter/exclude-high-value";
/// `EXCLUDE ItemID WHERE Interactions.DEVICE IN ("mobile")` — a column that reaches the
/// interaction row through the event's own `properties`.
const FILTER_EXCLUDE_MOBILE: &str =
    "arn:aws:personalize:us-east-1:000000000000:filter/exclude-mobile";
/// `EXCLUDE ItemID WHERE Items.GENRE IN ("Comedy") AND Interactions.EVENT_TYPE IN
/// ("purchase")` — one clause over each kind of dataset.
const FILTER_EXCLUDE_PURCHASED_COMEDY: &str =
    "arn:aws:personalize:us-east-1:000000000000:filter/exclude-purchased-comedy";
/// `EXCLUDE ActionID WHERE Action_Interactions.EVENT_TYPE IN ("Taken")`.
const FILTER_EXCLUDE_TAKEN: &str =
    "arn:aws:personalize:us-east-1:000000000000:filter/exclude-taken";
/// `INCLUDE ActionID WHERE Action_Interactions.EVENT_TYPE IN ("Taken")`.
const FILTER_INCLUDE_TAKEN: &str =
    "arn:aws:personalize:us-east-1:000000000000:filter/include-taken";
/// `EXCLUDE ItemID WHERE Action_Interactions.EVENT_TYPE IN ("Taken")` — an interaction
/// dataset that describes actions, not the items this filter selects.
const FILTER_ITEMS_OVER_ACTION_INTERACTIONS: &str =
    "arn:aws:personalize:us-east-1:000000000000:filter/items-over-action-interactions";
/// `EXCLUDE ActionID WHERE Interactions.EVENT_TYPE IN ("purchase")` — the reverse.
const FILTER_ACTIONS_OVER_INTERACTIONS: &str =
    "arn:aws:personalize:us-east-1:000000000000:filter/actions-over-interactions";

/// How many items the fitted catalog of every campaign here holds.
const CATALOG_SIZE: usize = 6;
/// How many actions the fitted Actions catalog holds.
const ACTION_CATALOG_SIZE: usize = 4;

/// `Event$sentAt` / `ActionInteraction$timestamp`: the client-side instant of an
/// interaction, in the Unix time the member carries. A fixed value, so nothing here
/// depends on the wall clock.
const SENT_AT: f64 = 1_764_000_000.0;

/// The ingestion routes this suite calls (`PutEvents` `POST /events`, `PutItems`
/// `POST /items`, `PutActions` `POST /actions`, `PutActionInteractions`
/// `POST /action-interactions`).
const EVENTS: &str = "/events";
const ITEMS: &str = "/items";
const ACTION_INTERACTIONS: &str = "/action-interactions";

/// One record this suite puts into the group's Items dataset through `PutItems`: the row
/// both the request and the metadata expectation are built from.
struct PutRecord {
    case: &'static str,
    item_id: &'static str,
    /// The record's `properties`, which become the item's Items-dataset columns.
    columns: &'static [(&'static str, &'static str)],
}

/// One interaction the fixture records: the row both the `PutEvents` request and the
/// expectation of every filter row are built from.
struct SeedEvent {
    user: &'static str,
    item: &'static str,
    event_type: &'static str,
    /// `Event$eventValue`, which reaches the interaction row as its `EVENT_VALUE` column.
    event_value: Option<f64>,
    /// An event property, which reaches the row as a column of its own name.
    device: Option<&'static str>,
}

/// The item interactions this suite records, spanning the axes its rules generalise over:
/// two users, three event types, an event with a value and an event with a property.
const SEED_EVENTS: [SeedEvent; 5] = [
    SeedEvent {
        user: "user-1",
        item: "item-0",
        event_type: "purchase",
        event_value: None,
        device: None,
    },
    SeedEvent {
        user: "user-1",
        item: "item-1",
        event_type: "watch",
        event_value: None,
        device: None,
    },
    SeedEvent {
        user: "user-1",
        item: "item-2",
        event_type: "purchase",
        event_value: Some(20.0),
        device: None,
    },
    SeedEvent {
        user: "user-2",
        item: "item-3",
        event_type: "purchase",
        event_value: None,
        device: None,
    },
    SeedEvent {
        user: "user-1",
        item: "item-4",
        event_type: "click",
        event_value: None,
        device: Some("mobile"),
    },
];

/// The ingestion targets of the group — the event trackers and the datasets — as the
/// Amazon Personalize CONTROL plane mints them.
///
/// A group may hold several trackers, and ONE tracker retains only its most recent
/// [`ingestion::MAX_EVENTS_PER_TRACKER`] events, so a suite that needs more rows than that
/// spreads them over the two trackers of this one group.
fn register_ingestion_targets(engine: &::engine::InMemoryEngine) {
    let store =
        engine.state::<ingestion::ComAmazonawsPersonalizeeventsAmazonpersonalizeeventsState>();
    let mut store = store.lock();
    for tracking_id in [TRACKING_ID, TRACKING_ID_SECOND] {
        store
            .register_event_tracker(tracking_id, Some(GROUP.to_string()))
            .expect("the ingestion store accepts the event tracker");
    }
    for dataset in [ITEMS_DATASET, ACTIONS_DATASET, OTHER_ITEMS_DATASET] {
        store
            .register_dataset(dataset)
            .unwrap_or_else(|| panic!("the ingestion store accepts the dataset {dataset}"));
    }
}

/// An emulator with both data planes mounted, the group's ingestion targets registered
/// and the campaigns and filters this suite reads.
///
/// The ingestion targets (an event tracker, the group's datasets) are minted by the
/// Amazon Personalize CONTROL plane, and the campaigns and filters by the same control
/// plane's `CreateCampaign` / `CreateFilter`, so this fixture seeds them through the seams
/// those operations use — with every campaign of the group FITTED to it at the instant
/// below, which is what `CreateCampaign` records for the solution version it deploys.
fn seeded() -> Emulator {
    let emulator = Emulator::with_ingestion();
    let engine = &*emulator.engine;

    register_ingestion_targets(engine);

    // Everything the group's campaigns were fitted to was fitted BEFORE this suite
    // records anything, so every interaction it records is one the fitted popularity does
    // not count yet.
    let fitted_at = SystemTime::now();
    for (arn, recipe_type) in [
        (CAMPAIGN_USER, RecipeType::UserPersonalization),
        (CAMPAIGN_RANKING, RecipeType::PersonalizedRanking),
    ] {
        assert!(
            registry::register_campaign(
                engine,
                Campaign::new(arn, "propagation", recipe_type, items(CATALOG_SIZE))
                    .with_metadata_enabled()
                    .fitted_to(GROUP, fitted_at)
            ),
            "case {arn}: the campaign seam must register the campaign"
        );
    }
    assert!(
        registry::register_campaign(
            engine,
            Campaign::new_actions(
                CAMPAIGN_ACTIONS,
                "propagation-actions",
                actions(ACTION_CATALOG_SIZE)
            )
            .fitted_to(GROUP, fitted_at)
        ),
        "the campaign seam must register the action campaign"
    );
    assert!(
        registry::register_campaign(
            engine,
            Campaign::new(
                CAMPAIGN_UNFITTED,
                "propagation-unfitted",
                RecipeType::UserPersonalization,
                items(CATALOG_SIZE)
            )
            .with_metadata_enabled()
        ),
        "the campaign seam must register the campaign fitted to no dataset group"
    );

    let filters = [
        (
            FILTER_EXCLUDE_PURCHASED,
            "EXCLUDE ItemID WHERE Interactions.EVENT_TYPE IN (\"purchase\")",
        ),
        (
            FILTER_INCLUDE_PURCHASED,
            "INCLUDE ItemID WHERE Interactions.EVENT_TYPE IN (\"purchase\")",
        ),
        (
            FILTER_EXCLUDE_EVENT_TYPE,
            "EXCLUDE ItemID WHERE Interactions.EVENT_TYPE IN ($EVENT_TYPE)",
        ),
        (
            FILTER_EXCLUDE_NOT_PURCHASED,
            "EXCLUDE ItemID WHERE Interactions.EVENT_TYPE NOT IN (\"purchase\")",
        ),
        (
            FILTER_EXCLUDE_HIGH_VALUE,
            "EXCLUDE ItemID WHERE Interactions.EVENT_VALUE > 10",
        ),
        (
            FILTER_EXCLUDE_MOBILE,
            "EXCLUDE ItemID WHERE Interactions.DEVICE IN (\"mobile\")",
        ),
        (
            FILTER_EXCLUDE_PURCHASED_COMEDY,
            "EXCLUDE ItemID WHERE Items.GENRE IN (\"Comedy\") AND Interactions.EVENT_TYPE IN \
             (\"purchase\")",
        ),
        (
            FILTER_EXCLUDE_TAKEN,
            "EXCLUDE ActionID WHERE Action_Interactions.EVENT_TYPE IN (\"Taken\")",
        ),
        (
            FILTER_INCLUDE_TAKEN,
            "INCLUDE ActionID WHERE Action_Interactions.EVENT_TYPE IN (\"Taken\")",
        ),
        (
            FILTER_ITEMS_OVER_ACTION_INTERACTIONS,
            "EXCLUDE ItemID WHERE Action_Interactions.EVENT_TYPE IN (\"Taken\")",
        ),
        (
            FILTER_ACTIONS_OVER_INTERACTIONS,
            "EXCLUDE ActionID WHERE Interactions.EVENT_TYPE IN (\"purchase\")",
        ),
    ];
    for (arn, expression) in filters {
        assert_eq!(
            registry::register_filter(engine, arn, expression),
            Ok(true),
            "case {arn}: the filter seam must accept {expression:?}"
        );
    }
    emulator
}

/// Record [`SEED_EVENTS`] through `PutEvents`, one call per event: the user and the
/// session are per CALL, so an event of another user is another call.
async fn record_seed_events(emulator: &Emulator) {
    for (index, event) in SEED_EVENTS.iter().enumerate() {
        let mut body = json!({
            "eventType": event.event_type,
            "sentAt": SENT_AT,
            "itemId": event.item,
            "eventId": format!("seed-{index}"),
        });
        if let Some(event_value) = event.event_value {
            body["eventValue"] = json!(event_value);
        }
        if let Some(device) = event.device {
            body["properties"] = json!(format!("{{\"DEVICE\":\"{device}\"}}"));
        }
        emulator
            .ingest(
                EVENTS,
                json!({
                    "trackingId": TRACKING_ID,
                    "sessionId": format!("session-{}", event.user),
                    "userId": event.user,
                    "eventList": [body],
                }),
            )
            .await;
    }
}

/// Record one action interaction per row through `PutActionInteractions`.
async fn record_action_interactions(emulator: &Emulator, rows: &[(&str, &str, &str)]) {
    for (index, (user, action, event_type)) in rows.iter().enumerate() {
        emulator
            .ingest(
                ACTION_INTERACTIONS,
                json!({
                    "trackingId": TRACKING_ID,
                    "actionInteractions": [{
                        "actionId": action,
                        "sessionId": format!("session-{user}"),
                        "userId": user,
                        "timestamp": SENT_AT,
                        "eventType": event_type,
                        "eventId": format!("action-seed-{index}"),
                    }],
                }),
            )
            .await;
    }
}

/// Whether `user` recorded an interaction of `event_type` with `item` in
/// [`SEED_EVENTS`] — the expectation side of an interaction clause, read off the same
/// table the requests are built from.
fn interacted(user: &str, item: &str, event_type: &str) -> bool {
    SEED_EVENTS.iter().any(|event| {
        event.user == user && event.item == item && event.event_type == event_type
    })
}

/// Whether `user` recorded an interaction with `item` whose type is NOT `event_type`.
fn interacted_other_than(user: &str, item: &str, event_type: &str) -> bool {
    SEED_EVENTS.iter().any(|event| {
        event.user == user && event.item == item && event.event_type != event_type
    })
}

/// Whether `user` recorded an interaction with `item` carrying a value above `threshold`.
fn interacted_above(user: &str, item: &str, threshold: f64) -> bool {
    SEED_EVENTS.iter().any(|event| {
        event.user == user
            && event.item == item
            && event.event_value.is_some_and(|value| value > threshold)
    })
}

/// Whether `user` recorded an interaction with `item` from `device`.
fn interacted_from(user: &str, item: &str, device: &str) -> bool {
    SEED_EVENTS.iter().any(|event| {
        event.user == user && event.item == item && event.device == Some(device)
    })
}

/// The stored `GENRE` of one fitted catalog entry.
fn genre(item: &Item) -> &str {
    item.attributes
        .get(COLUMN_GENRE)
        .map(String::as_str)
        .unwrap_or_default()
}

/// The ids the fitted catalog holds, sorted.
fn catalog_ids(keep: impl Fn(&Item) -> bool) -> Vec<String> {
    let mut ids: Vec<String> = items(CATALOG_SIZE)
        .into_iter()
        .filter(|item| keep(item))
        .map(|item| item.item_id)
        .collect();
    ids.sort();
    ids
}

/// One `GetRecommendations` body for `CAMPAIGN_USER`.
fn recommendations(user: &str, filter_arn: &str, filter_values: Option<Value>) -> Value {
    let mut body = json!({
        "campaignArn": CAMPAIGN_USER,
        "userId": user,
        "numResults": CATALOG_SIZE,
        "filterArn": filter_arn,
    });
    if let Some(filter_values) = filter_values {
        body["filterValues"] = filter_values;
    }
    body
}

#[tokio::test]
async fn an_interaction_filter_removes_the_items_this_user_s_recorded_events_match() {
    let emulator = seeded();
    record_seed_events(&emulator).await;

    // Each row is a filter expression over the Interactions dataset and the user the
    // request personalizes for; the expectation is the clause itself, applied to the
    // interactions that user recorded.
    #[allow(clippy::type_complexity)]
    let rows: Vec<(&str, &str, &str, Option<Value>, Box<dyn Fn(&Item) -> bool>)> = vec![
        (
            "EXCLUDE a purchase, for the user who purchased",
            FILTER_EXCLUDE_PURCHASED,
            "user-1",
            None,
            Box::new(|item| !interacted("user-1", &item.item_id, "purchase")),
        ),
        (
            "EXCLUDE a purchase, for another user",
            FILTER_EXCLUDE_PURCHASED,
            "user-2",
            None,
            Box::new(|item| !interacted("user-2", &item.item_id, "purchase")),
        ),
        (
            "EXCLUDE a purchase, for a user who recorded nothing",
            FILTER_EXCLUDE_PURCHASED,
            "user-3",
            None,
            Box::new(|_| true),
        ),
        (
            "INCLUDE a purchase",
            FILTER_INCLUDE_PURCHASED,
            "user-1",
            None,
            Box::new(|item| interacted("user-1", &item.item_id, "purchase")),
        ),
        (
            "EXCLUDE the event type the request binds",
            FILTER_EXCLUDE_EVENT_TYPE,
            "user-1",
            Some(json!({ "EVENT_TYPE": "\"watch\"" })),
            Box::new(|item| !interacted("user-1", &item.item_id, "watch")),
        ),
        (
            "EXCLUDE anything but a purchase",
            FILTER_EXCLUDE_NOT_PURCHASED,
            "user-1",
            None,
            Box::new(|item| !interacted_other_than("user-1", &item.item_id, "purchase")),
        ),
        (
            "EXCLUDE an interaction worth more than 10",
            FILTER_EXCLUDE_HIGH_VALUE,
            "user-1",
            None,
            Box::new(|item| !interacted_above("user-1", &item.item_id, 10.0)),
        ),
        (
            "EXCLUDE an interaction from a device the event's properties named",
            FILTER_EXCLUDE_MOBILE,
            "user-1",
            None,
            Box::new(|item| !interacted_from("user-1", &item.item_id, "mobile")),
        ),
        (
            "EXCLUDE a purchased item of one genre",
            FILTER_EXCLUDE_PURCHASED_COMEDY,
            "user-1",
            None,
            Box::new(|item| {
                !(genre(item) == "Comedy" && interacted("user-1", &item.item_id, "purchase"))
            }),
        ),
    ];

    for (case, filter_arn, user, filter_values, keep) in rows {
        let expected = catalog_ids(&keep);
        let response = emulator
            .ok(RECOMMENDATIONS, recommendations(user, filter_arn, filter_values))
            .await;
        let mut returned = ids(&response, "itemList", "itemId");
        returned.sort();
        assert_eq!(
            returned, expected,
            "case {case:?}: expected {expected:?}, got {returned:?}"
        );
        // The rule has to BITE: an expectation equal to the whole catalog would pass
        // against a filter that does nothing, so every row but the ones about a user with
        // no matching interaction removes something.
        if user == "user-1" {
            assert_ne!(
                expected.len(),
                CATALOG_SIZE,
                "case {case:?}: this row must select a strict subset of the catalog"
            );
        }
    }
}

#[tokio::test]
async fn an_interaction_filter_removes_nothing_before_the_interaction_is_recorded() {
    let emulator = seeded();

    // The same filter, the same user, the same campaign: the ONLY difference is whether
    // the interaction has been recorded yet.
    let before = ids(
        &emulator
            .ok(
                RECOMMENDATIONS,
                recommendations("user-1", FILTER_EXCLUDE_PURCHASED, None),
            )
            .await,
        "itemList",
        "itemId",
    );
    assert_eq!(
        {
            let mut sorted = before.clone();
            sorted.sort();
            sorted
        },
        catalog_ids(|_| true),
        "before anything is recorded an interaction filter removes nothing, got {before:?}"
    );

    record_seed_events(&emulator).await;

    let mut after = ids(
        &emulator
            .ok(
                RECOMMENDATIONS,
                recommendations("user-1", FILTER_EXCLUDE_PURCHASED, None),
            )
            .await,
        "itemList",
        "itemId",
    );
    after.sort();
    assert_eq!(
        after,
        catalog_ids(|item| !interacted("user-1", &item.item_id, "purchase")),
        "after the purchases are recorded the filter must remove exactly the purchased \
         items, got {after:?}"
    );
}

#[tokio::test]
async fn an_action_interaction_filter_removes_the_actions_this_user_took() {
    let emulator = seeded();
    let recorded: [(&str, &str, &str); 3] = [
        ("user-1", "action-0", "Taken"),
        ("user-1", "action-1", "Viewed"),
        ("user-2", "action-2", "Taken"),
    ];
    record_action_interactions(&emulator, &recorded).await;

    let taken = |user: &str, action: &str| {
        recorded
            .iter()
            .any(|(row_user, row_action, event_type)| {
                *row_user == user && *row_action == action && *event_type == "Taken"
            })
    };

    let rows: [(&str, &str, &str, bool); 4] = [
        ("EXCLUDE a taken action, for the user who took one", FILTER_EXCLUDE_TAKEN, "user-1", false),
        ("EXCLUDE a taken action, for another user", FILTER_EXCLUDE_TAKEN, "user-2", false),
        ("INCLUDE a taken action, for the user who took one", FILTER_INCLUDE_TAKEN, "user-1", true),
        ("INCLUDE a taken action, for a user who took none", FILTER_INCLUDE_TAKEN, "user-3", true),
    ];

    for (case, filter_arn, user, include) in rows {
        let mut expected: Vec<String> = actions(ACTION_CATALOG_SIZE)
            .into_iter()
            .filter(|action| taken(user, &action.action_id) == include)
            .map(|action| action.action_id)
            .collect();
        expected.sort();

        let response = emulator
            .ok(
                ACTION_RECOMMENDATIONS,
                json!({
                    "campaignArn": CAMPAIGN_ACTIONS,
                    "userId": user,
                    "numResults": ACTION_CATALOG_SIZE,
                    "filterArn": filter_arn,
                }),
            )
            .await;
        let mut returned = ids(&response, "actionList", "actionId");
        returned.sort();
        assert_eq!(
            returned, expected,
            "case {case:?}: expected {expected:?}, got {returned:?}"
        );
    }
}

#[tokio::test]
async fn a_filter_over_the_other_entity_s_interaction_dataset_is_refused() {
    let emulator = seeded();
    record_seed_events(&emulator).await;

    let rows: [(&str, &str, Value); 2] = [
        (
            "an Items filter reading the Action interactions dataset",
            RECOMMENDATIONS,
            json!({
                "campaignArn": CAMPAIGN_USER,
                "userId": "user-1",
                "filterArn": FILTER_ITEMS_OVER_ACTION_INTERACTIONS,
            }),
        ),
        (
            "an Actions filter reading the Item interactions dataset",
            ACTION_RECOMMENDATIONS,
            json!({
                "campaignArn": CAMPAIGN_ACTIONS,
                "userId": "user-1",
                "filterArn": FILTER_ACTIONS_OVER_INTERACTIONS,
            }),
        ),
    ];

    for (case, route, body) in rows {
        let (status, code) = emulator.err(route, body).await;
        assert_eq!(
            (status, code.as_str()),
            (400, "InvalidInputException"),
            "case {case:?}: expected 400 InvalidInputException, got {status} {code}"
        );
    }
}

#[tokio::test]
async fn items_put_through_the_ingestion_plane_become_recommendable_and_serve_their_metadata() {
    let emulator = seeded();

    // Each row is one record put into the group's Items dataset: a brand-new item, one
    // carrying a column the fitted catalog never held, and a replacement for a fitted
    // entry's own column.
    let rows: [PutRecord; 4] = [
        PutRecord {
            case: "a new item with the fitted catalog's columns",
            item_id: "item-new-a",
            columns: &[(COLUMN_GENRE, "Documentary"), (COLUMN_YEAR, "2024")],
        },
        PutRecord {
            case: "a new item with a column of its own",
            item_id: "item-new-b",
            columns: &[(COLUMN_GENRE, "Drama"), ("RUNTIME_MINUTES", "120")],
        },
        PutRecord {
            case: "a new item with no columns at all",
            item_id: "item-new-c",
            columns: &[],
        },
        PutRecord {
            case: "a replacement for a fitted entry's column",
            item_id: "item-0",
            columns: &[(COLUMN_GENRE, "Horror")],
        },
    ];

    for PutRecord {
        case,
        item_id,
        columns,
    } in &rows
    {
        let properties: BTreeMap<&str, &str> = columns.iter().copied().collect();
        let mut record = json!({ "itemId": item_id });
        if !properties.is_empty() {
            record["properties"] = json!(serde_json::to_string(&properties)
                .unwrap_or_else(|error| panic!("case {case:?}: serialize properties: {error}")));
        }
        emulator
            .ingest(
                ITEMS,
                json!({ "datasetArn": ITEMS_DATASET, "items": [record] }),
            )
            .await;
    }
    // A record of ANOTHER dataset group is not this campaign's data.
    emulator
        .ingest(
            ITEMS,
            json!({
                "datasetArn": OTHER_ITEMS_DATASET,
                "items": [{ "itemId": "item-elsewhere", "properties": "{\"GENRE\":\"Comedy\"}" }],
            }),
        )
        .await;

    let response = emulator
        .ok(
            RECOMMENDATIONS,
            json!({
                "campaignArn": CAMPAIGN_USER,
                "userId": "user-1",
                "numResults": CATALOG_SIZE + rows.len(),
                "metadataColumns": {
                    "ITEMS": [COLUMN_GENRE, COLUMN_YEAR, "RUNTIME_MINUTES"],
                },
            }),
        )
        .await;
    let returned = ids(&response, "itemList", "itemId");
    let metadata: BTreeMap<String, Value> = entries(&response, "itemList")
        .into_iter()
        .map(|entry| {
            let item_id = entry
                .get("itemId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            (item_id, entry.get("metadata").cloned().unwrap_or(Value::Null))
        })
        .collect();

    for PutRecord {
        case,
        item_id,
        columns,
    } in &rows
    {
        assert!(
            returned.iter().any(|returned| returned == item_id),
            "case {case:?}: {item_id} must be recommendable, got {returned:?}"
        );
        let served = metadata
            .get(*item_id)
            .unwrap_or_else(|| panic!("case {case:?}: no entry for {item_id} in {metadata:?}"));
        for (column, value) in columns.iter() {
            assert_eq!(
                served.get(*column).and_then(Value::as_str),
                Some(*value),
                "case {case:?}: expected {column}={value}, got {served}"
            );
        }
    }
    // The fitted entry's OTHER column is still the one it was fitted with: a record
    // replaces the columns it carries, not the entry.
    let fitted = items(CATALOG_SIZE);
    let fitted_year = fitted[0]
        .attributes
        .get(COLUMN_YEAR)
        .expect("the fitted catalog carries a YEAR");
    assert_eq!(
        metadata
            .get("item-0")
            .and_then(|served| served.get(COLUMN_YEAR))
            .and_then(Value::as_str),
        Some(fitted_year.as_str()),
        "a put record replaces only the columns it carries, got {metadata:?}"
    );
    assert!(
        !returned.iter().any(|id| id == "item-elsewhere"),
        "a record of another dataset group is not this campaign's data, got {returned:?}"
    );
}

#[tokio::test]
async fn a_newly_put_item_can_be_ranked_and_an_interaction_filter_applies_to_the_ranking() {
    let emulator = seeded();
    emulator
        .ingest(
            ITEMS,
            json!({
                "datasetArn": ITEMS_DATASET,
                "items": [{ "itemId": "item-new-a", "properties": "{\"GENRE\":\"Documentary\"}" }],
            }),
        )
        .await;
    record_seed_events(&emulator).await;

    let mut input_list: Vec<String> = items(CATALOG_SIZE)
        .into_iter()
        .map(|item| item.item_id)
        .collect();
    input_list.push("item-new-a".to_string());

    // With no filter, every listed id comes back — the put item included, now that the
    // catalog holds it.
    let ranked = emulator
        .ok(
            PERSONALIZE_RANKING,
            json!({
                "campaignArn": CAMPAIGN_RANKING,
                "userId": "user-1",
                "inputList": input_list.clone(),
            }),
        )
        .await;
    let mut returned = ids(&ranked, "personalizedRanking", "itemId");
    returned.sort();
    let mut expected = input_list.clone();
    expected.sort();
    assert_eq!(
        returned, expected,
        "a ranking reports every id the caller listed, got {returned:?}"
    );

    // With the interaction filter, the items this user purchased are gone.
    let filtered = emulator
        .ok(
            PERSONALIZE_RANKING,
            json!({
                "campaignArn": CAMPAIGN_RANKING,
                "userId": "user-1",
                "inputList": input_list.clone(),
                "filterArn": FILTER_EXCLUDE_PURCHASED,
            }),
        )
        .await;
    let mut kept = ids(&filtered, "personalizedRanking", "itemId");
    kept.sort();
    let mut expected: Vec<String> = input_list
        .iter()
        .filter(|item_id| !interacted("user-1", item_id, "purchase"))
        .cloned()
        .collect();
    expected.sort();
    assert_eq!(
        kept, expected,
        "a ranking honours an interaction filter, got {kept:?}"
    );
}

#[tokio::test]
async fn recorded_interactions_raise_an_item_s_score() {
    // The popularity an inference call reads is the fitted one plus the interactions
    // recorded since, so the same item's score rises with the number of interactions
    // recorded for it — and rises MORE for more of them.
    let item_id = format!("item-{}", CATALOG_SIZE - 1);
    let mut previous: Option<f64> = None;

    for recorded in [0_usize, 1, 5, 20] {
        let case = format!("{recorded} recorded interactions");
        let emulator = seeded();
        // `eventList` is modelled `len 1..10`, so a larger number of interactions is
        // several calls.
        for batch in 0..recorded.div_ceil(10) {
            let events: Vec<Value> = (0..recorded.min((batch + 1) * 10) - batch * 10)
                .map(|offset| {
                    json!({
                        "eventType": "watch",
                        "sentAt": SENT_AT,
                        "itemId": item_id,
                        "eventId": format!("boost-{batch}-{offset}"),
                    })
                })
                .collect();
            emulator
                .ingest(
                    EVENTS,
                    json!({
                        "trackingId": TRACKING_ID,
                        "sessionId": "session-boost",
                        "userId": "user-1",
                        "eventList": events,
                    }),
                )
                .await;
        }

        let response = emulator
            .ok(
                RECOMMENDATIONS,
                json!({
                    "campaignArn": CAMPAIGN_USER,
                    "userId": "user-1",
                    "numResults": CATALOG_SIZE,
                }),
            )
            .await;
        let scores = scores_by_id(&response, "itemList", "itemId");
        let score = *scores
            .get(&item_id)
            .unwrap_or_else(|| panic!("case {case}: no score for {item_id} in {scores:?}"));
        if let Some(previous) = previous {
            assert!(
                score > previous,
                "case {case}: expected a score above {previous}, got {score}"
            );
        }
        previous = Some(score);
    }
}

#[tokio::test]
async fn ingestion_changes_nothing_for_a_campaign_fitted_to_no_dataset_group() {
    let emulator = seeded();

    let before = emulator
        .ok(
            RECOMMENDATIONS,
            json!({
                "campaignArn": CAMPAIGN_UNFITTED,
                "userId": "user-1",
                "numResults": CATALOG_SIZE,
            }),
        )
        .await;

    record_seed_events(&emulator).await;
    emulator
        .ingest(
            ITEMS,
            json!({
                "datasetArn": ITEMS_DATASET,
                "items": [{ "itemId": "item-new-a", "properties": "{\"GENRE\":\"Drama\"}" }],
            }),
        )
        .await;

    let after = emulator
        .ok(
            RECOMMENDATIONS,
            json!({
                "campaignArn": CAMPAIGN_UNFITTED,
                "userId": "user-1",
                "numResults": CATALOG_SIZE,
            }),
        )
        .await;

    assert_eq!(
        scores_by_id(&before, "itemList", "itemId"),
        scores_by_id(&after, "itemList", "itemId"),
        "a campaign fitted to no dataset group reads nothing a caller ingested"
    );
    let (status, code) = emulator
        .err(
            RECOMMENDATIONS,
            json!({
                "campaignArn": CAMPAIGN_UNFITTED,
                "userId": "user-1",
                "metadataColumns": { "ITEMS": ["RUNTIME_MINUTES"] },
            }),
        )
        .await;
    assert_eq!(
        (status, code.as_str()),
        (400, "InvalidInputException"),
        "a column no catalog of this campaign holds is still refused, got {status} {code}"
    );
}

// ---------------------------------------------------------------------------------------
// What the fitted instant separates: the interactions a STORED popularity already counted.
// ---------------------------------------------------------------------------------------

/// The most interactions one `PutEvents` / `PutActionInteractions` call may carry
/// (`EventList` and `ActionInteractionsList` are both modelled `len 1..10`), so a larger
/// number of interactions is several calls.
const EVENTS_PER_CALL: usize = 10;

/// An item id the fitted catalog of every campaign in this suite does NOT hold, so the
/// fitted popularity counts nothing for it.
const ADDED_ITEM: &str = "item-added";
/// A second such id, for the row that drives the catalog bound.
const PRE_FITTED_ADDED_ITEM: &str = "item-added-before-fitting";
/// An action id the fitted Actions catalog does not hold.
const ADDED_ACTION: &str = "action-added";

/// One row of the popularity table: how many interactions were recorded for one entry
/// BEFORE the instant the campaign was fitted at, and how many after it.
struct PopularityRow {
    case: &'static str,
    before_fitting: usize,
    after_fitting: usize,
}

/// The rows the popularity rule is asserted over: no interaction at all, interactions only
/// before the fitted instant, only after it, split across it, and the same total reached
/// through different splits — the axes "whenever that interaction arrived" generalises
/// over.
const POPULARITY_ROWS: [PopularityRow; 7] = [
    PopularityRow {
        case: "no interaction at all",
        before_fitting: 0,
        after_fitting: 0,
    },
    PopularityRow {
        case: "one interaction, recorded before the campaign was fitted",
        before_fitting: 1,
        after_fitting: 0,
    },
    PopularityRow {
        case: "one interaction, recorded after the campaign was fitted",
        before_fitting: 0,
        after_fitting: 1,
    },
    PopularityRow {
        case: "three interactions, all recorded before the campaign was fitted",
        before_fitting: 3,
        after_fitting: 0,
    },
    PopularityRow {
        case: "three interactions, two of them before the campaign was fitted",
        before_fitting: 2,
        after_fitting: 1,
    },
    PopularityRow {
        case: "three interactions, all recorded after the campaign was fitted",
        before_fitting: 0,
        after_fitting: 3,
    },
    PopularityRow {
        case: "more interactions than one call carries, all before the campaign was fitted",
        before_fitting: EVENTS_PER_CALL + 2,
        after_fitting: 0,
    },
];

/// An emulator with both data planes mounted and the group's ingestion targets registered,
/// but NO campaign yet.
///
/// A suite whose subject is an interaction recorded BEFORE the instant a campaign was
/// fitted at has to ingest first and register the campaign afterwards, which is the order
/// a caller streaming into a dataset group before `CreateCampaign` produces.
fn ingestion_before_fitting() -> Emulator {
    let emulator = Emulator::with_ingestion();
    register_ingestion_targets(&emulator.engine);
    emulator
}

/// Register the group's ranking and action campaigns, each fitted to the group at
/// `fitted_at` — what `CreateCampaign` records for the solution version it deploys.
fn register_campaigns_fitted_at(emulator: &Emulator, fitted_at: SystemTime) {
    let engine = &*emulator.engine;
    assert!(
        registry::register_campaign(
            engine,
            Campaign::new(
                CAMPAIGN_RANKING,
                "propagation",
                RecipeType::PersonalizedRanking,
                items(CATALOG_SIZE)
            )
            .fitted_to(GROUP, fitted_at)
        ),
        "the campaign seam must register the ranking campaign"
    );
    assert!(
        registry::register_campaign(
            engine,
            Campaign::new_actions(
                CAMPAIGN_ACTIONS,
                "propagation-actions",
                actions(ACTION_CATALOG_SIZE)
            )
            .fitted_to(GROUP, fitted_at)
        ),
        "the campaign seam must register the action campaign"
    );
}

/// Record `count` item interactions for `item_id` through `PutEvents` on `tracking_id`,
/// with `tag` keeping every event id of one fixture distinct.
async fn record_interactions(
    emulator: &Emulator,
    tracking_id: &str,
    item_id: &str,
    count: usize,
    tag: &str,
) {
    let mut recorded = 0;
    let mut batch = 0;
    while recorded < count {
        let size = (count - recorded).min(EVENTS_PER_CALL);
        let events: Vec<Value> = (0..size)
            .map(|offset| {
                json!({
                    "eventType": "watch",
                    "sentAt": SENT_AT,
                    "itemId": item_id,
                    "eventId": format!("{tag}-{batch}-{offset}"),
                })
            })
            .collect();
        emulator
            .ingest(
                EVENTS,
                json!({
                    "trackingId": tracking_id,
                    "sessionId": format!("session-{tag}"),
                    "userId": "user-1",
                    "eventList": events,
                }),
            )
            .await;
        recorded += size;
        batch += 1;
    }
}

/// Record `count` action interactions for `action_id` through `PutActionInteractions`.
async fn record_action_interactions_for(
    emulator: &Emulator,
    action_id: &str,
    count: usize,
    tag: &str,
) {
    let mut recorded = 0;
    let mut batch = 0;
    while recorded < count {
        let size = (count - recorded).min(EVENTS_PER_CALL);
        let interactions: Vec<Value> = (0..size)
            .map(|offset| {
                json!({
                    "actionId": action_id,
                    "sessionId": format!("session-{tag}"),
                    "userId": "user-1",
                    "timestamp": SENT_AT,
                    "eventType": "Taken",
                    "eventId": format!("{tag}-{batch}-{offset}"),
                })
            })
            .collect();
        emulator
            .ingest(
                ACTION_INTERACTIONS,
                json!({
                    "trackingId": TRACKING_ID,
                    "actionInteractions": interactions,
                }),
            )
            .await;
        recorded += size;
        batch += 1;
    }
}

/// The score `GetPersonalizedRanking` reports for `item_id` when `row`'s interactions were
/// recorded around the instant the ranking campaign was fitted at. The request lists the
/// whole fitted catalog plus [`ADDED_ITEM`], so the reported scores are the shares of the
/// scored catalog.
async fn ranked_score(row: &PopularityRow, item_id: &str) -> BTreeMap<String, f64> {
    let emulator = ingestion_before_fitting();
    record_interactions(&emulator, TRACKING_ID, item_id, row.before_fitting, "before").await;
    register_campaigns_fitted_at(&emulator, SystemTime::now());
    record_interactions(&emulator, TRACKING_ID, item_id, row.after_fitting, "after").await;

    let mut input_list: Vec<String> = items(CATALOG_SIZE)
        .into_iter()
        .map(|item| item.item_id)
        .collect();
    input_list.push(ADDED_ITEM.to_string());
    let response = emulator
        .ok(
            PERSONALIZE_RANKING,
            json!({
                "campaignArn": CAMPAIGN_RANKING,
                "userId": "user-1",
                "inputList": input_list,
            }),
        )
        .await;
    assert_descending_and_bounded(row.case, &scores(&response, "personalizedRanking"));
    scores_by_id(&response, "personalizedRanking", "itemId")
}

/// The one score of `item_id` out of [`ranked_score`].
async fn ranked_score_of(row: &PopularityRow, item_id: &str) -> f64 {
    let scores = ranked_score(row, item_id).await;
    *scores.get(item_id).unwrap_or_else(|| {
        panic!("case {:?}: no score for {item_id} in {scores:?}", row.case)
    })
}

#[tokio::test]
async fn an_added_entry_s_popularity_counts_every_interaction_whenever_it_arrived() {
    // An entry the fitted catalog holds no popularity for had none of its interactions
    // counted, so all of them count — which makes its popularity, and therefore its
    // reported score, a function of HOW MANY interactions it has and not of where the
    // fitted instant falls among them.
    let mut by_total: BTreeMap<usize, Vec<(&str, f64)>> = BTreeMap::new();
    for row in &POPULARITY_ROWS {
        let score = ranked_score_of(row, ADDED_ITEM).await;
        by_total
            .entry(row.before_fitting + row.after_fitting)
            .or_default()
            .push((row.case, score));
    }

    let mut previous: Option<(usize, f64)> = None;
    for (total, scored) in &by_total {
        let (reference_case, reference) = scored[0];
        for (case, score) in scored {
            assert!(
                (score - reference).abs() < 1e-12,
                "case {case:?}: {total} recorded interactions must score what {reference_case:?} \
                 scores ({reference}), got {score}"
            );
        }
        if *total == 0 {
            assert_eq!(
                reference, 0.0,
                "case {reference_case:?}: an entry nobody interacted with has no popularity, \
                 got {reference}"
            );
        } else {
            assert!(
                reference > 0.0,
                "case {reference_case:?}: {total} recorded interactions are a popularity above \
                 zero, got {reference}"
            );
        }
        if let Some((previous_total, previous_score)) = previous {
            assert!(
                reference > previous_score,
                "case {reference_case:?}: {total} recorded interactions must score above the \
                 {previous_score} of {previous_total}, got {reference}"
            );
        }
        previous = Some((*total, reference));
    }
}

#[tokio::test]
async fn a_fitted_entry_counts_only_the_interactions_its_stored_popularity_does_not() {
    // The other half of the same rule, and the only reason the fitted instant is kept: the
    // interactions a stored popularity ALREADY counted are not counted again, so rows that
    // predate the fitted instant leave a fitted entry's score exactly where it was.
    let fitted_item = format!("item-{}", CATALOG_SIZE - 1);
    let baseline = ranked_score(&POPULARITY_ROWS[0], &fitted_item).await;

    for row in POPULARITY_ROWS
        .iter()
        .filter(|row| row.after_fitting == 0 && row.before_fitting > 0)
    {
        let scored = ranked_score(row, &fitted_item).await;
        assert_eq!(
            scored, baseline,
            "case {:?}: an interaction the fitted popularity already counted may not be \
             counted twice, expected {baseline:?}, got {scored:?}",
            row.case
        );
    }

    let mut previous = *baseline
        .get(&fitted_item)
        .unwrap_or_else(|| panic!("no score for {fitted_item} in {baseline:?}"));
    for row in POPULARITY_ROWS
        .iter()
        .filter(|row| row.before_fitting == 0 && row.after_fitting > 0)
    {
        let score = ranked_score_of(row, &fitted_item).await;
        assert!(
            score > previous,
            "case {:?}: an interaction recorded since the catalog was fitted must raise the \
             score above {previous}, got {score}",
            row.case
        );
        previous = score;
    }
}

#[tokio::test]
async fn an_added_action_s_popularity_counts_its_interactions_from_before_the_fitted_instant() {
    // The same rule over the OTHER entity an inference call recommends: an action the
    // fitted Actions catalog does not hold, reached through `GetActionRecommendations`.
    let mut scored: Vec<(&str, f64)> = Vec::new();
    for (case, before_fitting, after_fitting) in [
        ("no action interaction at all", 0_usize, 0_usize),
        ("three before the campaign was fitted", 3, 0),
        ("three after the campaign was fitted", 0, 3),
    ] {
        let emulator = ingestion_before_fitting();
        record_action_interactions_for(&emulator, ADDED_ACTION, before_fitting, "before").await;
        register_campaigns_fitted_at(&emulator, SystemTime::now());
        record_action_interactions_for(&emulator, ADDED_ACTION, after_fitting, "after").await;

        let response = emulator
            .ok(
                ACTION_RECOMMENDATIONS,
                json!({
                    "campaignArn": CAMPAIGN_ACTIONS,
                    "userId": "user-1",
                    "numResults": ACTION_CATALOG_SIZE + 1,
                }),
            )
            .await;
        let scores = scores_by_id(&response, "actionList", "actionId");
        scored.push((case, scores.get(ADDED_ACTION).copied().unwrap_or(0.0)));
    }

    assert_eq!(
        scored[0].1, 0.0,
        "case {:?}: an action nobody interacted with is not recommendable, got {}",
        scored[0].0, scored[0].1
    );
    assert!(
        scored[1].1 > 0.0,
        "case {:?}: interactions recorded before the campaign was fitted are a popularity \
         above zero, got {}",
        scored[1].0,
        scored[1].1
    );
    assert!(
        (scored[1].1 - scored[2].1).abs() < 1e-12,
        "case {:?}: the same number of interactions scores the same whenever they arrived, \
         expected {}, got {}",
        scored[1].0,
        scored[2].1,
        scored[1].1
    );
}

#[tokio::test]
async fn the_catalog_bound_keeps_the_most_interacted_with_added_entries() {
    // The bound on the entries ingestion may ADD keeps the most interacted-with ones, so a
    // bounded catalog is the head of the ordering a full one would report: an entry whose
    // interactions all predate the fitted instant is still more interacted-with than one
    // with a single interaction, and survives the cut.
    const PRE_FITTING: usize = 3;
    let bound = registry::ingested::MAX_LIVE_CATALOG_ENTRIES;

    let emulator = ingestion_before_fitting();
    record_interactions(
        &emulator,
        TRACKING_ID,
        PRE_FITTED_ADDED_ITEM,
        PRE_FITTING,
        "pre",
    )
    .await;
    register_campaigns_fitted_at(&emulator, SystemTime::now());

    // One added entry MORE than the bound keeps, each with a single interaction recorded
    // after the fitted instant: exactly one added entry is dropped. The ids are
    // zero-padded, so the order their text settles ties in is their numeric order.
    let live: Vec<String> = (0..bound).map(|index| format!("item-live-{index:04}")).collect();
    for (batch, chunk) in live.chunks(EVENTS_PER_CALL).enumerate() {
        // One tracker retains a bounded number of events, so the rows past that bound are
        // recorded through the group's second tracker.
        let tracking_id = if PRE_FITTING + (batch + 1) * EVENTS_PER_CALL
            <= ingestion::MAX_EVENTS_PER_TRACKER
        {
            TRACKING_ID
        } else {
            TRACKING_ID_SECOND
        };
        let events: Vec<Value> = chunk
            .iter()
            .enumerate()
            .map(|(offset, item_id)| {
                json!({
                    "eventType": "watch",
                    "sentAt": SENT_AT,
                    "itemId": item_id,
                    "eventId": format!("live-{batch}-{offset}"),
                })
            })
            .collect();
        emulator
            .ingest(
                EVENTS,
                json!({
                    "trackingId": tracking_id,
                    "sessionId": "session-live",
                    "userId": "user-1",
                    "eventList": events,
                }),
            )
            .await;
    }

    let kept_single = live.first().expect("the table holds ids").clone();
    let dropped = live.last().expect("the table holds ids").clone();
    let response = emulator
        .ok(
            PERSONALIZE_RANKING,
            json!({
                "campaignArn": CAMPAIGN_RANKING,
                "userId": "user-1",
                "inputList": [PRE_FITTED_ADDED_ITEM, kept_single, dropped],
            }),
        )
        .await;
    let scores = scores_by_id(&response, "personalizedRanking", "itemId");

    for (case, item_id) in [
        (
            "an entry whose interactions all predate the fitted instant",
            PRE_FITTED_ADDED_ITEM,
        ),
        (
            "an entry with one interaction, inside the bound",
            kept_single.as_str(),
        ),
    ] {
        let score = scores
            .get(item_id)
            .copied()
            .unwrap_or_else(|| panic!("case {case:?}: no score for {item_id} in {scores:?}"));
        assert!(
            score > 0.0,
            "case {case:?}: {item_id} must stay in the bounded catalog with a popularity \
             above zero, got {score}"
        );
    }
    let least = scores
        .get(&dropped)
        .copied()
        .unwrap_or_else(|| panic!("no score for {dropped} in {scores:?}"));
    assert_eq!(
        least, 0.0,
        "the added entry the bound drops is the least interacted-with one, so {dropped} \
         carries no relevance, got {least}"
    );
    let pre_fitted = scores
        .get(PRE_FITTED_ADDED_ITEM)
        .copied()
        .unwrap_or_default();
    let single = scores.get(&kept_single).copied().unwrap_or_default();
    assert!(
        pre_fitted > single,
        "an entry with {PRE_FITTING} interactions is more interacted-with than one with a \
         single interaction, got {pre_fitted} against {single}"
    );
}
