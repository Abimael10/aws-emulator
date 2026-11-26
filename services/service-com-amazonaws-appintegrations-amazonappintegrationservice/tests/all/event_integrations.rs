// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the AppIntegrations event-integration lifecycle, and for the
//! listing of the associations an event integration holds.
//!
//! Every assertion goes over the wire: a request is built, dispatched through the same
//! [`MultiServiceRouter`] the production emulator mounts, and only the HTTP status,
//! `x-amzn-errortype` header and JSON body are inspected. Nothing reaches into the
//! service's state struct, so a response that is not computed from persisted state
//! cannot pass.
//!
//! The one exception is a case that needs an event integration ASSOCIATION stored:
//! AppIntegrations publishes no create for one (`smithy_query(query="digest")` binds a
//! create for every other resource of this service and none for this one), because the
//! association comes into being on the side of the client that associates the
//! integration — so such a case puts records in the store the router serves from
//! (`Emulator::seed_event_integration_associations`) and still reads every assertion off
//! the wire.
//!
//! The tests are tables: each row is one input along the axis the rule under test
//! generalises over (name form, description presence and length, bus, filter source,
//! tag count, page size, signing region), and the recorded `demo-integration` case from
//! the bootstrap issue is only one row.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use service_com_amazonaws_appintegrations_amazonappintegrationservice::state::EventIntegrationAssociationRecord;

// The wire plumbing — router, request building, the reduced `Reply` — is the crate's
// one harness, so this suite and the data-integration ones cannot drift apart in how
// they talk to the service.
use super::harness::{Emulator, Reply, SeededEventIntegrationAssociation, REGION};

/// `MaxResults` is `range 1..50` on `ListEventIntegrations`, and on
/// `ListEventIntegrationAssociations` (model: `smithy_query(query="operation",
/// operation="ListEventIntegrations,ListEventIntegrationAssociations")`).
const MAX_RESULTS_MIN: i64 = 1;
const MAX_RESULTS_MAX: i64 = 50;

/// The event-integration operations, as methods on the shared harness: an inherent
/// impl, so every call site below still reads as a call on the service under test.
impl Emulator {
    async fn create(&self, region: &str, body: Value) -> Reply {
        self.send("POST", "/eventIntegrations", region, Some(body))
            .await
    }

    async fn get(&self, region: &str, name: &str) -> Reply {
        self.send(
            "GET",
            &format!("/eventIntegrations/{name}"),
            region,
            None,
        )
        .await
    }

    async fn list(&self, region: &str, query: &str) -> Reply {
        let uri = if query.is_empty() {
            "/eventIntegrations".to_string()
        } else {
            format!("/eventIntegrations?{query}")
        };
        self.send("GET", &uri, region, None).await
    }

    async fn update(&self, region: &str, name: &str, body: Value) -> Reply {
        self.send(
            "PATCH",
            &format!("/eventIntegrations/{name}"),
            region,
            Some(body),
        )
        .await
    }

    async fn delete(&self, region: &str, name: &str) -> Reply {
        self.send(
            "DELETE",
            &format!("/eventIntegrations/{name}"),
            region,
            None,
        )
        .await
    }
}

/// The ARN AWS documents for this resource:
/// `arn:aws:app-integrations:<region>:<account>:event-integration/<name>`. Written out
/// here rather than taken from the handler's helper, so the test pins the format.
fn expected_arn(region: &str, name: &str) -> String {
    format!(
        "arn:aws:app-integrations:{region}:{account}:event-integration/{name}",
        account = engine::EMULATOR_ACCOUNT_ID
    )
}

/// One row of the round-trip table: what a create is given, and therefore what every
/// later read must report.
#[derive(Debug, Clone)]
struct Case {
    /// What makes this row different from the others, for assertion messages.
    label: &'static str,
    name: String,
    description: Option<String>,
    event_bridge_bus: String,
    source: String,
    tags: BTreeMap<String, String>,
}

impl Case {
    fn create_body(&self) -> Value {
        let mut body = json!({
            "Name": self.name,
            "EventBridgeBus": self.event_bridge_bus,
            "EventFilter": { "Source": self.source },
        });
        if let Some(description) = &self.description {
            body["Description"] = json!(description);
        }
        // `TagMap` is `len 1..200`, so an untagged resource sends no `Tags` member at
        // all rather than an empty map.
        if !self.tags.is_empty() {
            body["Tags"] = json!(self.tags);
        }
        body
    }
}

fn case(
    label: &'static str,
    name: &str,
    description: Option<&str>,
    event_bridge_bus: &str,
    source: &str,
    tags: &[(&str, &str)],
) -> Case {
    Case {
        label,
        name: name.to_string(),
        description: description.map(str::to_string),
        event_bridge_bus: event_bridge_bus.to_string(),
        source: source.to_string(),
        tags: tags
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
    }
}

/// The table every round-trip assertion runs over. Rows differ in name form and
/// length, whether a description is present and how long it is, bus name, event
/// source, and tag count — the axes `CreateEventIntegration`'s members range over.
/// `Name` and `EventBridgeBus` are `len 1..255 pattern ^[a-zA-Z0-9\/\._\-]+$` and
/// `Description` is `len 0..1000`, so the rows include both published bounds.
fn round_trip_cases() -> Vec<Case> {
    vec![
        // The case the bootstrap issue recorded: one row of many.
        case(
            "issue's recorded case",
            "demo-integration",
            Some("first"),
            "default",
            "aws.partner/demo",
            &[],
        ),
        case(
            "shortest legal name, no description, one tag",
            "a",
            None,
            "custom.bus-1",
            "aws.partner/example/1",
            &[("owner", "team-a")],
        ),
        case(
            "every character class the name pattern allows",
            "Mixed.Case_name-9",
            Some(""),
            "Bus.Name_with-parts9",
            "aws.partner/vendor.name/account-7",
            &[("a", "1"), ("b", "2"), ("c", "3")],
        ),
        case(
            "longest legal name and description",
            &"n".repeat(255),
            Some(&"d".repeat(1000)),
            &"b".repeat(255),
            "aws.partner/long",
            &[("k", "")],
        ),
    ]
}

fn assert_reported_case(label: &str, reply: &Reply, expected: &Case, region: &str) {
    assert_eq!(
        reply.status, 200,
        "case {label:?}: expected HTTP 200, got {} with body {}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.string("Name").as_deref(),
        Some(expected.name.as_str()),
        "case {label:?}: expected the persisted Name, got {:?}",
        reply.string("Name")
    );
    assert_eq!(
        reply.string("EventIntegrationArn"),
        Some(expected_arn(region, &expected.name)),
        "case {label:?}: expected the ARN create returned, got {:?}",
        reply.string("EventIntegrationArn")
    );
    assert_eq!(
        reply.string("EventBridgeBus").as_deref(),
        Some(expected.event_bridge_bus.as_str()),
        "case {label:?}: expected the persisted EventBridgeBus, got {:?}",
        reply.string("EventBridgeBus")
    );
    assert_eq!(
        reply.field("EventFilter"),
        Some(&json!({ "Source": expected.source })),
        "case {label:?}: expected the persisted EventFilter, got {:?}",
        reply.field("EventFilter")
    );
    assert_eq!(
        reply.string("Description"),
        expected.description.clone(),
        "case {label:?}: expected the persisted Description, got {:?}",
        reply.string("Description")
    );
    // An untagged integration reports an EMPTY tag map, not an absent member.
    let expected_tags = json!(expected.tags);
    assert_eq!(
        reply.field("Tags"),
        Some(&expected_tags),
        "case {label:?}: expected Tags {expected_tags:?}, got {:?}",
        reply.field("Tags")
    );
}

#[tokio::test]
async fn a_created_event_integration_is_reported_by_get_and_list_exactly_as_stored() {
    let emulator = Emulator::new();

    for expected in round_trip_cases() {
        let label = expected.label;
        let created = emulator.create(REGION, expected.create_body()).await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: expected create to answer HTTP 200, got {} with body {}",
            created.status, created.body
        );
        let arn = created.string("EventIntegrationArn");
        assert_eq!(
            arn,
            Some(expected_arn(REGION, &expected.name)),
            "case {label:?}: expected create to mint the documented ARN, got {arn:?}"
        );

        let fetched = emulator.get(REGION, &expected.name).await;
        assert_reported_case(label, &fetched, &expected, REGION);

        // Reading twice must report the same thing: a read changes nothing.
        let again = emulator.get(REGION, &expected.name).await;
        assert_eq!(
            again.body, fetched.body,
            "case {label:?}: a second read reported {} after the first reported {}",
            again.body, fetched.body
        );
    }

    // Every row is now in the enumeration, each with the same members Get reported.
    let listed = emulator.list(REGION, "").await;
    assert_eq!(
        listed.status, 200,
        "expected list to answer HTTP 200, got {} with body {}",
        listed.status, listed.body
    );
    let items = listed
        .field("EventIntegrations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for expected in round_trip_cases() {
        let label = expected.label;
        let found = items
            .iter()
            .find(|item| item.get("Name").and_then(Value::as_str) == Some(expected.name.as_str()))
            .cloned();
        let summary = Reply {
            status: 200,
            error_type: String::new(),
            body: found.clone().unwrap_or(Value::Null),
        };
        assert!(
            found.is_some(),
            "case {label:?}: expected the listing to contain it, got {items:?}"
        );
        assert_reported_case(label, &summary, &expected, REGION);
    }
}

#[tokio::test]
async fn the_arn_carries_the_region_the_request_was_signed_for() {
    // The resource's ARN names the region the call was made in, not a fixed one.
    for region in ["us-east-1", "eu-west-1", "ap-southeast-2"] {
        let emulator = Emulator::new();
        let name = "regional-integration";
        let created = emulator
            .create(
                region,
                json!({
                    "Name": name,
                    "EventBridgeBus": "default",
                    "EventFilter": { "Source": "aws.partner/demo" },
                }),
            )
            .await;
        assert_eq!(
            created.string("EventIntegrationArn"),
            Some(expected_arn(region, name)),
            "case {region:?}: expected the ARN to name the signing region, got {:?}",
            created.string("EventIntegrationArn")
        );

        let fetched = emulator.get(region, name).await;
        assert_eq!(
            fetched.string("EventIntegrationArn"),
            created.string("EventIntegrationArn"),
            "case {region:?}: Get must report the ARN create stored, got {:?}",
            fetched.string("EventIntegrationArn")
        );
    }
}

#[tokio::test]
async fn a_second_create_of_a_live_name_is_refused_as_a_duplicate() {
    let emulator = Emulator::new();

    for expected in round_trip_cases() {
        let label = expected.label;
        let first = emulator.create(REGION, expected.create_body()).await;
        assert_eq!(
            first.status, 200,
            "case {label:?}: expected the first create to succeed, got {} / {}",
            first.status, first.body
        );

        // Same name, different everything else: the name is the resource's identity.
        let mut body = expected.create_body();
        body["EventBridgeBus"] = json!("other-bus");
        body["EventFilter"] = json!({ "Source": "aws.partner/other" });
        let second = emulator.create(REGION, body).await;
        assert_eq!(
            second.status, 409,
            "case {label:?}: expected HTTP 409 for a duplicate name, got {} / {}",
            second.status, second.body
        );
        assert_eq!(
            second.error_type, "DuplicateResourceException",
            "case {label:?}: expected DuplicateResourceException, got {:?}",
            second.error_type
        );
        // The sentence AppIntegrations words this refusal with: fixed text naming the
        // resource type, with the request's `Name` as its only variable part. Rows
        // differ in the form and length of that name, so the template is exercised and
        // not a single literal.
        assert_eq!(
            second.message(),
            Some(format!(
                "An EventIntegration already exists with name: {}.",
                expected.name
            )),
            "case {label:?}: expected the duplicate-name message, got {:?}",
            second.message()
        );

        // And the refused create changed nothing.
        let fetched = emulator.get(REGION, &expected.name).await;
        assert_reported_case(label, &fetched, &expected, REGION);
    }
}

#[tokio::test]
async fn a_store_at_its_ceiling_refuses_another_create_until_one_is_deleted() {
    let emulator = Emulator::new();

    // The emulator's own ceiling on this in-memory store, read from the crate rather
    // than restated, so the test follows the constant the handler checks.
    let ceiling = service_com_amazonaws_appintegrations_amazonappintegrationservice::state::MAX_STORED_EVENT_INTEGRATIONS;

    let body_for = |name: String| {
        json!({
            "Name": name,
            "EventBridgeBus": "default",
            "EventFilter": { "Source": "aws.partner/demo" },
        })
    };

    for index in 0..ceiling {
        let reply = emulator
            .create(REGION, body_for(format!("ceiling-{index:05}")))
            .await;
        assert_eq!(
            reply.status, 200,
            "case {index}: a create below the ceiling must succeed, got {} / {}",
            reply.status, reply.body
        );
    }

    let refused = emulator
        .create(REGION, body_for("one-too-many".to_string()))
        .await;
    assert_eq!(
        refused.status, 429,
        "case one past the ceiling: expected HTTP 429, got {} / {}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error_type, "ResourceQuotaExceededException",
        "case one past the ceiling: expected ResourceQuotaExceededException, got {:?}",
        refused.error_type
    );

    // A delete frees a slot, so the ceiling bounds the store rather than ending its
    // usefulness.
    let removed = emulator.delete(REGION, "ceiling-00000").await;
    assert_eq!(
        removed.status, 200,
        "case freeing a slot: expected HTTP 200, got {} / {}",
        removed.status, removed.body
    );
    let accepted = emulator
        .create(REGION, body_for("one-too-many".to_string()))
        .await;
    assert_eq!(
        accepted.status, 200,
        "case after a delete: expected HTTP 200, got {} / {}",
        accepted.status, accepted.body
    );
}

#[tokio::test]
async fn a_create_repeated_with_its_client_token_reports_the_arn_it_already_minted() {
    // `ClientToken` is `@idempotencyToken`: replaying the call must neither create a
    // second resource nor fail, whatever the token's text.
    for token in ["t", "11111111-2222-3333-4444-555555555555", &"z".repeat(2048)] {
        let emulator = Emulator::new();
        let name = "idempotent-integration";
        let body = json!({
            "Name": name,
            "EventBridgeBus": "default",
            "EventFilter": { "Source": "aws.partner/demo" },
            "ClientToken": token,
        });

        let first = emulator.create(REGION, body.clone()).await;
        assert_eq!(
            first.status, 200,
            "case {token:?}: expected the first create to succeed, got {} / {}",
            first.status, first.body
        );

        let replay = emulator.create(REGION, body).await;
        assert_eq!(
            replay.status, 200,
            "case {token:?}: expected a replayed create to succeed, got {} / {}",
            replay.status, replay.body
        );
        assert_eq!(
            replay.string("EventIntegrationArn"),
            first.string("EventIntegrationArn"),
            "case {token:?}: expected the ARN of the first create, got {:?}",
            replay.string("EventIntegrationArn")
        );

        let items = emulator
            .list(REGION, "")
            .await
            .field("EventIntegrations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            items.len(),
            1,
            "case {token:?}: expected exactly one stored integration, got {items:?}"
        );
    }

    // Without a token the same second create is a duplicate, not an idempotent replay.
    let emulator = Emulator::new();
    let body = json!({
        "Name": "untokened-integration",
        "EventBridgeBus": "default",
        "EventFilter": { "Source": "aws.partner/demo" },
    });
    let first = emulator.create(REGION, body.clone()).await;
    assert_eq!(
        first.status, 200,
        "case \"no ClientToken\": expected the first create to succeed, got {} / {}",
        first.status, first.body
    );
    let second = emulator.create(REGION, body).await;
    assert_eq!(
        second.error_type, "DuplicateResourceException",
        "case \"no ClientToken\": expected DuplicateResourceException, got {:?} ({})",
        second.error_type, second.status
    );
}

/// Walk every page `ListEventIntegrations` offers and return the names it reported, in
/// the order it reported them, asserting each page honours `MaxResults`.
async fn drain_pages(emulator: &Emulator, max_results: Option<i64>, label: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut token: Option<String> = None;
    // One extra round trip than there can be pages, so a non-terminating cursor fails
    // loudly instead of looping forever.
    for _ in 0..64 {
        let mut query = Vec::new();
        if let Some(max) = max_results {
            query.push(format!("maxResults={max}"));
        }
        if let Some(cursor) = &token {
            query.push(format!("nextToken={cursor}"));
        }
        let page = emulator.list(REGION, &query.join("&")).await;
        assert_eq!(
            page.status, 200,
            "case {label:?}: expected HTTP 200 for a page, got {} / {}",
            page.status, page.body
        );
        let items = page
            .field("EventIntegrations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if let Some(max) = max_results {
            assert!(
                i64::try_from(items.len()).unwrap_or(i64::MAX) <= max,
                "case {label:?}: expected at most {max} items per page, got {}",
                items.len()
            );
        }
        for item in &items {
            names.push(
                item.get("Name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            );
        }
        token = page.string("NextToken");
        if token.is_none() {
            return names;
        }
    }
    panic!("case {label:?}: the cursor never terminated after 64 pages");
}

#[tokio::test]
async fn pagination_reports_every_stored_integration_exactly_once() {
    // Rows differ in how many integrations are stored and how large a page is asked
    // for, including both published `MaxResults` bounds and no `MaxResults` at all.
    for stored in [0usize, 1, 2, 7] {
        let emulator = Emulator::new();
        let mut expected: Vec<String> = Vec::new();
        for index in 0..stored {
            let name = format!("integration-{index:02}");
            let created = emulator
                .create(
                    REGION,
                    json!({
                        "Name": name,
                        "EventBridgeBus": "default",
                        "EventFilter": { "Source": format!("aws.partner/demo/{index}") },
                    }),
                )
                .await;
            assert_eq!(
                created.status, 200,
                "case {stored} stored: expected create to succeed, got {} / {}",
                created.status, created.body
            );
            expected.push(name);
        }
        expected.sort();

        for max_results in [
            Some(MAX_RESULTS_MIN),
            Some(2),
            Some(3),
            Some(MAX_RESULTS_MAX),
            None,
        ] {
            let label = format!("{stored} stored, maxResults={max_results:?}");
            let reported = drain_pages(&emulator, max_results, &label).await;
            assert_eq!(
                reported, expected,
                "case {label:?}: expected every stored name once in name order, got {reported:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_max_results_outside_the_modelled_range_is_refused() {
    let emulator = Emulator::new();
    // One past each published bound of `MaxResults` (range 1..50).
    for max in [MAX_RESULTS_MIN - 1, MAX_RESULTS_MAX + 1] {
        let reply = emulator.list(REGION, &format!("maxResults={max}")).await;
        assert_eq!(
            reply.status, 400,
            "case maxResults={max}: expected HTTP 400, got {} / {}",
            reply.status, reply.body
        );
    }
    // And each published bound itself is accepted.
    for max in [MAX_RESULTS_MIN, MAX_RESULTS_MAX] {
        let reply = emulator.list(REGION, &format!("maxResults={max}")).await;
        assert_eq!(
            reply.status, 200,
            "case maxResults={max}: expected HTTP 200, got {} / {}",
            reply.status, reply.body
        );
    }
}

#[tokio::test]
async fn a_next_token_this_service_did_not_mint_is_refused() {
    let emulator = Emulator::new();
    for name in ["one", "two"] {
        emulator
            .create(
                REGION,
                json!({
                    "Name": name,
                    "EventBridgeBus": "default",
                    "EventFilter": { "Source": "aws.partner/demo" },
                }),
            )
            .await;
    }

    // Tokens differing in why they are not cursors: not hex at all, odd-length hex,
    // hex that decodes to bytes that are not text.
    for token in ["not-a-cursor", "abc", "fffe"] {
        let reply = emulator.list(REGION, &format!("nextToken={token}")).await;
        assert_eq!(
            reply.status, 400,
            "case {token:?}: expected HTTP 400 for a foreign NextToken, got {} / {}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_type, "InvalidRequestException",
            "case {token:?}: expected InvalidRequestException, got {:?}",
            reply.error_type
        );
    }
}

#[tokio::test]
async fn update_mutates_the_stored_description_and_leaves_everything_else_alone() {
    // Rows differ in the description's presence and length before and after, including
    // both published bounds of `Description` (len 0..1000).
    let transitions: Vec<(&str, Option<&str>, Option<String>)> = vec![
        ("the issue's recorded case", Some("first"), Some("second".to_string())),
        ("absent becomes present", None, Some("now described".to_string())),
        ("present becomes empty", Some("was here"), Some(String::new())),
        ("present becomes longest legal", Some("short"), Some("d".repeat(1000))),
        ("omitted member leaves it unchanged", Some("unchanged"), None),
    ];

    for (label, before, after) in transitions {
        let emulator = Emulator::new();
        let name = "updatable-integration";
        let mut body = json!({
            "Name": name,
            "EventBridgeBus": "default",
            "EventFilter": { "Source": "aws.partner/demo" },
            "Tags": { "owner": "team-a" },
        });
        if let Some(before) = before {
            body["Description"] = json!(before);
        }
        let created = emulator.create(REGION, body).await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: expected create to succeed, got {} / {}",
            created.status, created.body
        );

        let patch = match &after {
            Some(description) => json!({ "Description": description }),
            None => json!({}),
        };
        let updated = emulator.update(REGION, name, patch).await;
        assert_eq!(
            updated.status, 200,
            "case {label:?}: expected update to answer HTTP 200, got {} / {}",
            updated.status, updated.body
        );

        let expected_description = after
            .clone()
            .or_else(|| before.map(str::to_string));
        let fetched = emulator.get(REGION, name).await;
        assert_eq!(
            fetched.string("Description"),
            expected_description,
            "case {label:?}: expected Description {expected_description:?}, got {:?}",
            fetched.string("Description")
        );
        // The members update does not name keep their stored values.
        assert_eq!(
            fetched.string("EventIntegrationArn"),
            Some(expected_arn(REGION, name)),
            "case {label:?}: expected the ARN to survive the update, got {:?}",
            fetched.string("EventIntegrationArn")
        );
        assert_eq!(
            fetched.field("Tags"),
            Some(&json!({ "owner": "team-a" })),
            "case {label:?}: expected the tags to survive the update, got {:?}",
            fetched.field("Tags")
        );
        assert_eq!(
            fetched.field("EventFilter"),
            Some(&json!({ "Source": "aws.partner/demo" })),
            "case {label:?}: expected the filter to survive the update, got {:?}",
            fetched.field("EventFilter")
        );
    }
}

#[tokio::test]
async fn delete_removes_the_resource_and_only_that_resource() {
    let emulator = Emulator::new();
    let cases = round_trip_cases();
    for expected in &cases {
        let created = emulator.create(REGION, expected.create_body()).await;
        assert_eq!(
            created.status, 200,
            "case {:?}: expected create to succeed, got {} / {}",
            expected.label, created.status, created.body
        );
    }

    let mut remaining: Vec<String> = cases.iter().map(|c| c.name.clone()).collect();
    remaining.sort();

    for expected in &cases {
        let label = expected.label;
        let deleted = emulator.delete(REGION, &expected.name).await;
        assert_eq!(
            deleted.status, 200,
            "case {label:?}: expected delete to answer HTTP 200, got {} / {}",
            deleted.status, deleted.body
        );

        let fetched = emulator.get(REGION, &expected.name).await;
        assert_eq!(
            fetched.status, 404,
            "case {label:?}: expected HTTP 404 after delete, got {} / {}",
            fetched.status, fetched.body
        );
        assert_eq!(
            fetched.error_type, "ResourceNotFoundException",
            "case {label:?}: expected ResourceNotFoundException after delete, got {:?}",
            fetched.error_type
        );

        let again = emulator.delete(REGION, &expected.name).await;
        assert_eq!(
            again.status, 404,
            "case {label:?}: expected a second delete to answer HTTP 404, got {} / {}",
            again.status, again.body
        );

        remaining.retain(|name| name != &expected.name);
        let listed = drain_pages(&emulator, None, label).await;
        assert_eq!(
            listed, remaining,
            "case {label:?}: expected the listing to be {remaining:?}, got {listed:?}"
        );
    }
}

/// `ClientId` is `len 1..255, pattern .*` and `EventBridgeRuleName` is `len 1..2048,
/// pattern ^[a-zA-Z0-9\/\._\-]+$` on `EventIntegrationAssociation` (model:
/// `smithy_query(query="shape", shape="EventIntegrationAssociation")`).
const CLIENT_ID_MAX: usize = 255;
const EVENT_BRIDGE_RULE_NAME_MAX: usize = 2048;

fn association(
    client_id: Option<&str>,
    rule_name: Option<&str>,
    metadata: &[(&str, &str)],
) -> SeededEventIntegrationAssociation {
    (
        client_id.map(str::to_string),
        rule_name.map(str::to_string),
        metadata
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect(),
    )
}

/// `ListEventIntegrationAssociations`:
/// `GET /eventIntegrations/{EventIntegrationName}/associations`.
async fn list_associations(emulator: &Emulator, name: &str, query: &str) -> Reply {
    let base = format!("/eventIntegrations/{name}/associations");
    let uri = if query.is_empty() {
        base
    } else {
        format!("{base}?{query}")
    };
    emulator.send("GET", &uri, REGION, None).await
}

/// The reported page, as a list (an absent member is an empty page).
fn associations_page(reply: &Reply) -> Vec<Value> {
    reply
        .field("EventIntegrationAssociations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The ARN form an event integration association carries, written out here rather than
/// taken from the crate's helper so the test pins the format.
fn expected_association_arn(region: &str, name: &str, association_id: &str) -> String {
    format!(
        "arn:aws:app-integrations:{region}:{account}:\
         event-integration-association/{name}/{association_id}",
        account = engine::EMULATOR_ACCOUNT_ID
    )
}

/// The entry the listing must report for `record`: the members
/// `EventIntegrationAssociation` carries, each one absent when the association holds no
/// value for it.
fn expected_association(record: &EventIntegrationAssociationRecord) -> Value {
    let mut expected = json!({
        "EventIntegrationAssociationArn": record.arn,
        "EventIntegrationAssociationId": record.id,
        "EventIntegrationName": record.event_integration_name,
    });
    if let Some(client_id) = &record.client_id {
        expected["ClientId"] = json!(client_id);
    }
    if let Some(rule_name) = &record.event_bridge_rule_name {
        expected["EventBridgeRuleName"] = json!(rule_name);
    }
    if !record.client_association_metadata.is_empty() {
        expected["ClientAssociationMetadata"] = json!(record.client_association_metadata);
    }
    expected
}

/// Store one event integration under `name`, so a case lists the associations of an
/// integration this service really holds.
async fn create_integration(emulator: &Emulator, name: &str) {
    let created = emulator
        .create(
            REGION,
            json!({
                "Name": name,
                "EventBridgeBus": "default",
                "EventFilter": { "Source": "aws.partner/demo" },
            }),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "CreateEventIntegration must succeed so a case has an integration, got {} / {}",
        created.status, created.body
    );
}

#[tokio::test]
async fn the_association_listing_reports_the_stored_associations_of_the_integration_it_names() {
    let long_client_id = "c".repeat(CLIENT_ID_MAX);
    let long_rule_name = "r".repeat(EVENT_BRIDGE_RULE_NAME_MAX);
    // Rows differ in how many associations the integration holds and in which members
    // each association carries: none (the recorded case), one, and several spanning both
    // published bounds of `ClientId`, a rule name at its upper bound, client metadata
    // present and absent, and an association that carries neither optional member.
    let rows: Vec<(&str, Vec<SeededEventIntegrationAssociation>)> = vec![
        ("no association", Vec::new()),
        (
            "one association",
            vec![association(
                Some("connect-instance-1"),
                Some("AmazonConnect-event-rule"),
                &[],
            )],
        ),
        (
            "several associations, at both bounds of ClientId and with metadata",
            vec![
                association(Some("c"), Some("rule.one_1-x"), &[("k", "v")]),
                association(
                    Some(&long_client_id),
                    Some(&long_rule_name),
                    &[("owner", "team-a"), ("purpose", "events")],
                ),
                association(
                    Some("arn:aws:connect:us-east-1:111122223333:instance/abc"),
                    None,
                    &[],
                ),
                association(None, None, &[]),
            ],
        ),
    ];

    for (label, seeds) in rows {
        let emulator = Emulator::new();
        create_integration(&emulator, "target").await;
        create_integration(&emulator, "neighbour").await;
        // A neighbour's associations are not this integration's: the listing reports the
        // ones belonging to the integration the request names.
        emulator.seed_event_integration_associations(
            "neighbour",
            &[association(Some("neighbour-client"), None, &[])],
        );
        let stored = emulator.seed_event_integration_associations("target", &seeds);
        let expected: Vec<Value> = stored.iter().map(expected_association).collect();

        let reply = list_associations(&emulator, "target", "").await;
        assert_eq!(
            reply.status, 200,
            "case {label:?}: expected HTTP 200, got {} / {}",
            reply.status, reply.body
        );
        assert_eq!(
            associations_page(&reply),
            expected,
            "case {label:?}: expected {expected:?}, got {}",
            reply.body
        );
        assert_eq!(
            reply.field("NextToken"),
            None,
            "case {label:?}: a last page must report no NextToken, got {:?}",
            reply.field("NextToken")
        );

        // The ARN the listing reports is the documented form, nested under the event
        // integration the association belongs to.
        for record in &stored {
            assert_eq!(
                record.arn,
                expected_association_arn(REGION, "target", &record.id),
                "case {label:?}: expected the documented association ARN, got {:?}",
                record.arn
            );
        }

        // The listing reports; it changes nothing a later caller sees.
        let repeated = list_associations(&emulator, "target", "").await;
        assert_eq!(
            associations_page(&repeated),
            expected,
            "case {label:?}: a repeated listing must report the same page, got {}",
            repeated.body
        );
        assert_eq!(
            associations_page(&list_associations(&emulator, "neighbour", "").await).len(),
            1,
            "case {label:?}: the neighbour must still hold its own association"
        );
    }
}

#[tokio::test]
async fn an_integration_with_no_associations_and_an_unknown_name_report_the_same_empty_page() {
    let emulator = Emulator::new();
    create_integration(&emulator, "present-integration").await;
    create_integration(&emulator, "doomed-integration").await;
    let deleted = emulator.delete(REGION, "doomed-integration").await;
    assert_eq!(
        deleted.status, 200,
        "DeleteEventIntegration must succeed so a case has a name with no integration, \
         got {} / {}",
        deleted.status, deleted.body
    );

    // Rows differ in the state of the name the request addresses: an integration this
    // service holds that nothing has associated, one it held and deleted, a name never
    // created, and names at both published bounds of `Name`'s length. Every one of them
    // reports the same empty page, because the name is resolved to the associations
    // stored under it rather than verified against a stored integration.
    let mut bodies: Vec<(&str, Value)> = Vec::new();
    for (label, name) in [
        ("an integration with no associations", "present-integration"),
        ("an integration that was deleted", "doomed-integration"),
        ("a name never created", "absent-integration"),
        ("the shortest legal name", "a"),
        ("the longest legal name", &"n".repeat(255) as &str),
    ] {
        let reply = list_associations(&emulator, name, "").await;
        assert_eq!(
            reply.status, 200,
            "case {label:?}: expected HTTP 200, got {} / {}",
            reply.status, reply.body
        );
        assert_eq!(
            associations_page(&reply),
            Vec::<Value>::new(),
            "case {label:?}: expected an empty page, got {}",
            reply.body
        );
        assert_eq!(
            reply.field("NextToken"),
            None,
            "case {label:?}: an empty page must report no NextToken, got {:?}",
            reply.field("NextToken")
        );
        bodies.push((label, reply.body));
    }

    let (first_label, first_body) = bodies.first().cloned().expect("the table has rows");
    for (label, body) in &bodies {
        assert_eq!(
            body, &first_body,
            "case {label:?}: expected the same page as case {first_label:?} \
             ({first_body}), got {body}"
        );
    }
}

#[tokio::test]
async fn the_page_controls_of_the_association_listing_follow_their_published_bounds() {
    let emulator = Emulator::new();
    create_integration(&emulator, "paged-integration").await;
    let seeds: Vec<SeededEventIntegrationAssociation> = (0..7)
        .map(|index| association(Some(&format!("client-{index}")), None, &[]))
        .collect();
    let stored = emulator.seed_event_integration_associations("paged-integration", &seeds);
    let all: Vec<Value> = stored.iter().map(expected_association).collect();

    // Rows differ in the page size the walk uses: one per page, a size that divides the
    // stored count, one that does not, exactly the stored count, and the largest the
    // model allows. Following the cursors must report every association exactly once, in
    // the store's order, whatever the page size.
    for page_size in [1_usize, 3, 4, 7, MAX_RESULTS_MAX as usize] {
        let mut seen: Vec<Value> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0_usize;
        loop {
            let query = match &token {
                None => format!("maxResults={page_size}"),
                Some(cursor) => format!("maxResults={page_size}&nextToken={cursor}"),
            };
            let reply = list_associations(&emulator, "paged-integration", &query).await;
            assert_eq!(
                reply.status, 200,
                "case maxResults={page_size} page {pages}: expected HTTP 200, got {} / {}",
                reply.status, reply.body
            );
            let page = associations_page(&reply);
            assert!(
                page.len() <= page_size,
                "case maxResults={page_size} page {pages}: a page must not exceed the \
                 requested size, got {} entries",
                page.len()
            );
            seen.extend(page);
            pages += 1;
            assert!(
                pages <= stored.len() + 1,
                "case maxResults={page_size}: the walk must terminate, got {pages} pages"
            );
            token = reply.string("NextToken");
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            seen, all,
            "case maxResults={page_size}: expected every stored association once in \
             order, got {seen:?}"
        );
    }

    // One past each published bound of `MaxResults`, then each bound itself.
    for (max, expected_status) in [
        (MAX_RESULTS_MIN - 1, 400),
        (MAX_RESULTS_MAX + 1, 400),
        (MAX_RESULTS_MIN, 200),
        (MAX_RESULTS_MAX, 200),
    ] {
        let reply =
            list_associations(&emulator, "paged-integration", &format!("maxResults={max}")).await;
        assert_eq!(
            reply.status, expected_status,
            "case maxResults={max}: expected HTTP {expected_status}, got {} / {}",
            reply.status, reply.body
        );
    }

    // Tokens differing in why they are not cursors this service minted: not hex at all,
    // odd-length hex, hex that decodes to bytes that are not text.
    for token in ["not-a-cursor", "abc", "fffe"] {
        let reply = list_associations(
            &emulator,
            "paged-integration",
            &format!("nextToken={token}"),
        )
        .await;
        assert_eq!(
            reply.status, 400,
            "case {token:?}: expected HTTP 400 for a foreign NextToken, got {} / {}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_type, "InvalidRequestException",
            "case {token:?}: expected InvalidRequestException, got {:?}",
            reply.error_type
        );
    }
}

#[tokio::test]
async fn every_operation_on_an_absent_name_answers_resource_not_found() {
    let emulator = Emulator::new();
    // A stored neighbour, so the 404 is about the named resource and not an empty store.
    emulator
        .create(
            REGION,
            json!({
                "Name": "present-integration",
                "EventBridgeBus": "default",
                "EventFilter": { "Source": "aws.partner/demo" },
            }),
        )
        .await;

    for name in ["absent-integration", "a", &"n".repeat(255)] {
        for (operation, reply) in [
            ("GetEventIntegration", emulator.get(REGION, name).await),
            (
                "UpdateEventIntegration",
                emulator
                    .update(REGION, name, json!({ "Description": "x" }))
                    .await,
            ),
            (
                "DeleteEventIntegration",
                emulator.delete(REGION, name).await,
            ),
        ] {
            let label = format!("{operation} on {name:?}");
            assert_eq!(
                reply.status, 404,
                "case {label:?}: expected HTTP 404, got {} / {}",
                reply.status, reply.body
            );
            assert_eq!(
                reply.error_type, "ResourceNotFoundException",
                "case {label:?}: expected ResourceNotFoundException, got {:?}",
                reply.error_type
            );
        }
    }
}
