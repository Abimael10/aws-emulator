// SPDX-License-Identifier: CC0-1.0

//! What the `RECOMMENDATION_ID` column of an interaction row holds, driven across the two
//! Personalize data planes: Amazon Personalize Events INGESTS the interaction and Amazon
//! Personalize Runtime reads it back through a filter clause over the interaction dataset.
//!
//! RULE: Amazon Personalize reserves a `properties` keyword as the interaction FIELD it
//! names, in every spelling of that name — `Event$properties` /
//! `ActionInteraction$properties` list the six reserved keywords in camel case and state
//! that a key's camel-case name IS the upper-snake schema field it matches
//! ("`numberOfRatings` would match the 'NUMBER_OF_RATINGS' field"), which is the spelling a
//! filter expression uses. So a record naming one of those fields in its `properties` is
//! refused as an input fault, and the column a filter clause reads holds only what the
//! record's own modelled member carries: the recommendation id of the call that recorded
//! the interaction, or nothing at all.
//!
//! Both halves have to agree for that to hold, and only a router mounting both can show
//! it: the ingestion plane decides which `properties` a record may carry, and the inference
//! plane decides what a clause over `Interactions.RECOMMENDATION_ID` /
//! `Action_Interactions.RECOMMENDATION_ID` compares against.
//!
//! The rows span what the rule generalises over: which interaction stream recorded the row,
//! whether the record carried a recommendation id of its own, which spelling of the
//! reserved field name a property used, whether an ordinary property was there beside it,
//! and both directions of the clause (`EXCLUDE` and `INCLUDE`). The `RECOMMENDATION_ID`
//! property on an action interaction that the issue cited is one row.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_personalizeevents_amazonpersonalizeevents::state as ingestion;
use service_com_amazonaws_personalizeruntime_amazonpersonalizeruntime::state::{
    self as registry, Action, Campaign, Item, RecipeType,
};

/// The region every request is signed for.
const REGION: &str = "us-east-1";

/// The account the campaigns and filters of this suite live in: the one this emulator
/// serves, since a cross-account reference is refused before anything is resolved.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The dataset group both planes share: the tracker ingests into it and every campaign of
/// this suite was fitted to it.
const GROUP: &str = "arn:aws:personalize:us-east-1:000000000000:dataset-group/recommendation-id";

/// The tracking id the group's event tracker is addressed by.
const TRACKING_ID: &str = "tracker-recommendation-id";

/// The user every row records its one interaction for.
const USER: &str = "user-1";

/// The item that interaction is about, and the action for the other stream.
const ITEM: &str = "item-0";
const ACTION: &str = "action-0";

/// A second entry of each catalog, so a response that reports more than the subject entry
/// is still a response a row can read the subject out of.
const OTHER_ITEM: &str = "item-1";

/// The recommendation id a clause names, and one that is not it.
const CLAUSE_ID: &str = "rec-real";
const OTHER_ID: &str = "rec-other";

/// The client-side instant every record reports, in the Unix time `Event$sentAt` and
/// `ActionInteraction$timestamp` carry.
const SENT_AT: f64 = 1_764_000_000.0;

/// The sentence Amazon Personalize refuses a reserved `properties` keyword with, for the
/// one keyword these rows name. Written out here rather than read off either crate, so the
/// wording cannot drift in one place only.
const RESERVED_KEYWORD_MESSAGE: &str = "recommendationid is not allowed in properties. \
     Please refer to documentation to include recommendationid";

/// Both data planes on ONE engine, as `core/router/src/lib.rs` mounts them.
struct Emulator {
    engine: Arc<::engine::InMemoryEngine>,
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<::engine::InMemoryEngine> = Arc::new(::engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_personalizeevents_amazonpersonalizeevents::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_personalizeruntime_amazonpersonalizeruntime::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        Self { engine, router }
    }

    /// One restJson1 call to the endpoint host `host` serves.
    async fn post(&self, host: &str, path: &str, body: &Value) -> (u16, Value) {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/personalize/aws4_request, \
             SignedHeaders=host, Signature=0"
        );
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("content-type", "application/json")
            .header("authorization", authorization)
            .body(Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let body = serde_json::from_slice::<Value>(response.body()).unwrap_or(Value::Null);
        (status, body)
    }

    /// An ingestion call (`personalize-events`).
    async fn ingest(&self, path: &str, body: &Value) -> (u16, Value) {
        self.post(
            &format!("personalize-events.{REGION}.amazonaws.com"),
            path,
            body,
        )
        .await
    }

    /// An inference call (`personalize-runtime`) that must succeed.
    async fn infer(&self, case: &str, path: &str, body: &Value) -> Value {
        let (status, answered) = self
            .post(
                &format!("personalize-runtime.{REGION}.amazonaws.com"),
                path,
                body,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the inference call to answer 200, got {status} with \
             {answered}"
        );
        answered
    }
}

/// The error code of a refusal body, without its namespace.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit(['#', '/'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The `message` of a refusal body.
fn error_message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
        .to_string()
}

/// The ids a response list reports, sorted so a row compares sets rather than the scoring
/// order (which `inference.rs` in the runtime crate owns).
fn sorted_ids(response: &Value, member: &str, id_member: &str) -> Vec<String> {
    let mut ids: Vec<String> = response
        .get(member)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("response {response} carries no {member}"))
        .iter()
        .map(|entry| {
            entry
                .get(id_member)
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("entry {entry} carries no {id_member}"))
                .to_string()
        })
        .collect();
    ids.sort();
    ids
}

/// The filter ARN of one direction of the clause.
fn filter_arn(direction: &str) -> String {
    format!("arn:aws:personalize:{REGION}:{ACCOUNT}:filter/recommendation-id-{direction}")
}

/// A campaign ARN of this suite.
fn campaign_arn(name: &str) -> String {
    format!("arn:aws:personalize:{REGION}:{ACCOUNT}:campaign/recommendation-id-{name}")
}

/// An emulator with the group's event tracker registered, the three campaigns of this
/// suite fitted to the group, and the two directions of the clause `column` registered as
/// filters over `subject`.
///
/// The tracker, the campaigns and the filters are all minted by the Amazon Personalize
/// CONTROL plane, so this fixture seeds them through the seams those operations use; the
/// interaction every row reads back is PUT over the wire.
fn seeded(subject: &str, column: &str) -> Emulator {
    let emulator = Emulator::new();
    emulator
        .engine
        .state::<ingestion::ComAmazonawsPersonalizeeventsAmazonpersonalizeeventsState>()
        .lock()
        .register_event_tracker(TRACKING_ID, Some(GROUP.to_string()))
        .expect("the ingestion store accepts the event tracker");

    // Every campaign was fitted before this row records anything, so the interaction it
    // records is one the fitted catalog does not count yet.
    let fitted_at = std::time::SystemTime::now();
    let items: Vec<Item> = (0..3)
        .map(|index| Item::new(format!("item-{index}"), (3 - index) as f64))
        .collect();
    let actions: Vec<Action> = (0..3)
        .map(|index| Action::new(format!("action-{index}"), (3 - index) as f64))
        .collect();
    for (name, recipe_type) in [
        ("user", RecipeType::UserPersonalization),
        ("ranking", RecipeType::PersonalizedRanking),
    ] {
        assert!(
            registry::register_campaign(
                &*emulator.engine,
                Campaign::new(campaign_arn(name), name, recipe_type, items.clone())
                    .fitted_to(GROUP, fitted_at)
            ),
            "case {name:?}: the campaign seam must register the campaign"
        );
    }
    assert!(
        registry::register_campaign(
            &*emulator.engine,
            Campaign::new_actions(campaign_arn("actions"), "actions", actions)
                .fitted_to(GROUP, fitted_at)
        ),
        "the campaign seam must register the action campaign"
    );

    for direction in ["exclude", "include"] {
        let expression = format!(
            "{} {subject} WHERE {column} IN (\"{CLAUSE_ID}\")",
            direction.to_uppercase()
        );
        assert_eq!(
            registry::register_filter(
                &*emulator.engine,
                filter_arn(direction),
                expression.as_str()
            ),
            Ok(true),
            "case {expression:?}: the filter seam must accept the expression"
        );
    }
    emulator
}

/// One row: the interaction record a caller tries to store, and what a clause over the
/// `RECOMMENDATION_ID` column must then answer for the entry it is about.
struct Row {
    case: &'static str,
    /// `Event$recommendationId` / `ActionInteraction$recommendationId` on the record, when
    /// the call carries one.
    recommendation_id: Option<&'static str>,
    /// The record's own `properties` object, as the modelled JSON string map.
    properties: Option<Value>,
    /// Whether the ingestion call is accepted at all: a `properties` key naming the
    /// reserved field is an input fault, whichever spelling writes it.
    accepted: bool,
    /// Whether a clause over the column, naming [`CLAUSE_ID`], matches the stored row.
    matched: bool,
}

/// The rows, one per (recommendation id the record carries, `properties` the call sends).
/// They are the same for both interaction streams, because the rule is the same one.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the record's own recommendationId is the one the clause names",
            recommendation_id: Some(CLAUSE_ID),
            properties: None,
            accepted: true,
            matched: true,
        },
        Row {
            case: "the record carries another recommendationId",
            recommendation_id: Some(OTHER_ID),
            properties: None,
            accepted: true,
            matched: false,
        },
        Row {
            case: "the record carries no recommendationId at all",
            recommendation_id: None,
            properties: None,
            accepted: true,
            matched: false,
        },
        Row {
            case: "an ordinary property beside no recommendationId",
            recommendation_id: None,
            properties: Some(json!({ "DEVICE": "mobile" })),
            accepted: true,
            matched: false,
        },
        Row {
            case: "a property spelling the reserved field as a schema field does",
            recommendation_id: None,
            properties: Some(json!({ "RECOMMENDATION_ID": CLAUSE_ID })),
            accepted: false,
            matched: false,
        },
        Row {
            case: "the same spelling in lower case",
            recommendation_id: None,
            properties: Some(json!({ "recommendation_id": CLAUSE_ID })),
            accepted: false,
            matched: false,
        },
        Row {
            case: "the camel-case keyword the model lists",
            recommendation_id: None,
            properties: Some(json!({ "recommendationId": CLAUSE_ID })),
            accepted: false,
            matched: false,
        },
        Row {
            case: "the reserved field beside the record's own recommendationId",
            recommendation_id: Some(OTHER_ID),
            properties: Some(json!({ "RECOMMENDATION_ID": CLAUSE_ID })),
            accepted: false,
            matched: false,
        },
        Row {
            case: "the reserved field beside an ordinary property",
            recommendation_id: None,
            properties: Some(json!({ "DEVICE": "mobile", "RECOMMENDATION_ID": CLAUSE_ID })),
            accepted: false,
            matched: false,
        },
    ]
}

/// Ingest `row`'s record through `path` and assert the answer the rule owes it: an empty
/// 200, or the service's own input fault for a `properties` key naming a reserved field.
async fn record(emulator: &Emulator, row: &Row, path: &str, body: &Value) {
    let (status, answered) = emulator.ingest(path, body).await;
    if row.accepted {
        assert_eq!(
            status, 200,
            "case {:?}: expected the record to be ingested, got {status} with {answered}",
            row.case
        );
    } else {
        assert_eq!(
            status, 400,
            "case {:?}: expected the reserved field to be refused, got {status} with \
             {answered}",
            row.case
        );
        assert_eq!(
            error_code(&answered),
            "InvalidInputException",
            "case {:?}: expected InvalidInputException, got {answered}",
            row.case
        );
        assert_eq!(
            error_message(&answered),
            RESERVED_KEYWORD_MESSAGE,
            "case {:?}: unexpected refusal wording, got {:?}",
            row.case,
            error_message(&answered)
        );
    }
}

/// The two lists a row reads: what an `EXCLUDE` clause leaves and what an `INCLUDE` clause
/// keeps. An entry the clause matched is removed by the one and is the only entry kept by
/// the other, since the row's user interacted with no other entry.
fn assert_filtered(case: &str, subject: &str, matched: bool, excluded: &[String], included: &[String]) {
    assert_eq!(
        excluded.contains(&subject.to_string()),
        !matched,
        "case {case:?}: an EXCLUDE clause over RECOMMENDATION_ID must {} {subject}, got \
         {excluded:?}",
        if matched { "remove" } else { "keep" }
    );
    let expected: Vec<String> = if matched {
        vec![subject.to_string()]
    } else {
        Vec::new()
    };
    assert_eq!(
        included, expected,
        "case {case:?}: an INCLUDE clause over RECOMMENDATION_ID expected {expected:?}, got \
         {included:?}"
    );
}

#[tokio::test]
async fn an_action_interaction_clause_reads_the_recommendation_id_the_record_carries() {
    for row in rows() {
        let emulator = seeded("ActionID", "Action_Interactions.RECOMMENDATION_ID");
        let mut interaction = json!({
            "actionId": ACTION,
            "sessionId": format!("session-{USER}"),
            "userId": USER,
            "timestamp": SENT_AT,
            "eventType": "Taken",
            "eventId": "interaction-row",
        });
        if let Some(recommendation_id) = row.recommendation_id {
            interaction["recommendationId"] = json!(recommendation_id);
        }
        if let Some(properties) = &row.properties {
            interaction["properties"] = json!(properties.to_string());
        }
        record(
            &emulator,
            &row,
            "/action-interactions",
            &json!({ "trackingId": TRACKING_ID, "actionInteractions": [interaction] }),
        )
        .await;

        let mut answered = Vec::new();
        for direction in ["exclude", "include"] {
            let response = emulator
                .infer(
                    row.case,
                    "/action-recommendations",
                    &json!({
                        "campaignArn": campaign_arn("actions"),
                        "userId": USER,
                        "numResults": 3,
                        "filterArn": filter_arn(direction),
                    }),
                )
                .await;
            answered.push(sorted_ids(&response, "actionList", "actionId"));
        }
        assert_filtered(row.case, ACTION, row.matched, &answered[0], &answered[1]);
    }
}

#[tokio::test]
async fn an_item_interaction_clause_reads_the_recommendation_id_the_record_carries() {
    for row in rows() {
        let emulator = seeded("ItemID", "Interactions.RECOMMENDATION_ID");
        let mut event = json!({
            "eventType": "watch",
            "sentAt": SENT_AT,
            "itemId": ITEM,
            "eventId": "event-row",
        });
        if let Some(recommendation_id) = row.recommendation_id {
            event["recommendationId"] = json!(recommendation_id);
        }
        if let Some(properties) = &row.properties {
            event["properties"] = json!(properties.to_string());
        }
        record(
            &emulator,
            &row,
            "/events",
            &json!({
                "trackingId": TRACKING_ID,
                "sessionId": format!("session-{USER}"),
                "userId": USER,
                "eventList": [event],
            }),
        )
        .await;

        // The same recorded facts, read by both operations that rank items.
        let mut recommended = Vec::new();
        let mut ranked = Vec::new();
        for direction in ["exclude", "include"] {
            let response = emulator
                .infer(
                    row.case,
                    "/recommendations",
                    &json!({
                        "campaignArn": campaign_arn("user"),
                        "userId": USER,
                        "numResults": 3,
                        "filterArn": filter_arn(direction),
                    }),
                )
                .await;
            recommended.push(sorted_ids(&response, "itemList", "itemId"));

            let response = emulator
                .infer(
                    row.case,
                    "/personalize-ranking",
                    &json!({
                        "campaignArn": campaign_arn("ranking"),
                        "userId": USER,
                        "inputList": [ITEM, OTHER_ITEM],
                        "filterArn": filter_arn(direction),
                    }),
                )
                .await;
            ranked.push(sorted_ids(&response, "personalizedRanking", "itemId"));
        }
        assert_filtered(
            row.case,
            ITEM,
            row.matched,
            &recommended[0],
            &recommended[1],
        );
        assert_filtered(row.case, ITEM, row.matched, &ranked[0], &ranked[1]);
    }
}
