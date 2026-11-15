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
    actions, entries, ids, items, scores_by_id, Emulator, ACTION_RECOMMENDATIONS, COLUMN_GENRE,
    COLUMN_YEAR, PERSONALIZE_RANKING, RECOMMENDATIONS,
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

    {
        let store = engine.state::<ingestion::ComAmazonawsPersonalizeeventsAmazonpersonalizeeventsState>();
        let mut store = store.lock();
        store
            .register_event_tracker(TRACKING_ID, Some(GROUP.to_string()))
            .expect("the ingestion store accepts the event tracker");
        for dataset in [ITEMS_DATASET, ACTIONS_DATASET, OTHER_ITEMS_DATASET] {
            store
                .register_dataset(dataset)
                .unwrap_or_else(|| panic!("the ingestion store accepts the dataset {dataset}"));
        }
    }

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
