// SPDX-License-Identifier: CC0-1.0

//! The sentence a duplicate event-destination name is refused with, end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with this service
//! registered — through the shared harness in the sibling [`crate::configuration_sets`]
//! suite, so routing, the constraint walk, the handler and the error envelope are all
//! exercised.
//!
//! The rule these cases pin: attaching an event destination under a name the addressed
//! configuration set already holds is refused `400 AlreadyExistsException` with SES's own
//! sentence — fixed text naming the kind of resource whose name is taken, with the
//! requested destination name and the configuration set that holds it each wrapped in
//! literal angle brackets and a closing period — and the refusal changes nothing the set
//! holds.
//!
//! The rows span what the rule generalises over: the form of the destination name (one
//! character, plain, hyphenated, underscored, mixed case, long) and of the configuration
//! set name, which of several destinations the set already holds is the one asked for
//! again, and whether the second request's definition matches the stored one or differs
//! from it — the refusal is about the NAME, so none of those changes the sentence. The
//! other side of the rule is pinned too: a name the set does not hold is attached, the
//! same name is free in another set, and a delete frees it again.

use serde_json::{json, Value};

use crate::configuration_set_event_destinations::{
    reported_destination, reported_destinations, EventDestinations,
};
use crate::configuration_sets::{Reply, Ses};

/// SES's sentence for an event-destination name the configuration set already holds: the
/// destination name the request asked for and the configuration set the request path
/// addressed it within, each between literal angle brackets.
fn already_exists(destination_name: &str, set_name: &str) -> String {
    format!(
        "An event destination with name <{destination_name}> already exists for configuration \
         set <{set_name}>."
    )
}

/// Assert `reply` is the refusal `code` at `status` carrying exactly `message`.
fn refused(case: &str, reply: &Reply, status: u16, code: &str, message: &str) {
    assert_eq!(
        (reply.status, reply.error_code().as_str()),
        (status, code),
        "case {case:?}: expected {status} {code}, got {} {} ({})",
        reply.status,
        reply.error_code(),
        reply.message()
    );
    assert_eq!(
        reply.message(),
        message,
        "case {case:?}: expected {message:?}, got {:?}",
        reply.message()
    );
}

/// The names a destination is attached under, spanning the forms a caller sends: one
/// character, a plain word, a hyphenated name, an underscored name, mixed case, and a long
/// one (`EventDestinationName` declares no character rule or length bound —
/// `smithy_query(query="shape",shape="EventDestinationName")`).
const DESTINATION_NAMES: [&str; 6] = [
    "d",
    "events",
    "to-the-bus",
    "to_the_bus",
    "MixedCase-Dest",
    "destination-name-that-a-caller-may-well-send-because-it-is-descriptive",
];

/// The definitions the second create carries, so the refusal is seen to depend on the name
/// alone: one identical to the stored definition, and ones that differ in the enabled flag,
/// in the matching event types and in the destination object the definition names.
fn second_definitions() -> Vec<(&'static str, Value)> {
    vec![
        (
            "the identical definition",
            json!({"Enabled": true, "MatchingEventTypes": ["SEND"]}),
        ),
        ("a definition that is disabled", json!({"Enabled": false})),
        (
            "a definition matching other event types",
            json!({"Enabled": true, "MatchingEventTypes": ["BOUNCE", "COMPLAINT"]}),
        ),
        (
            "a definition naming another destination object",
            json!({
                "Enabled": true,
                "MatchingEventTypes": ["DELIVERY"],
                "EventBridgeDestination": {
                    "EventBusArn": format!(
                        "arn:aws:events:us-east-1:{}:event-bus/default",
                        ::engine::emulator_account_id()
                    ),
                },
            }),
        ),
    ]
}

#[tokio::test]
async fn a_duplicate_event_destination_name_is_refused_in_awss_own_sentence() {
    // Each row is one (configuration set, destination name) pair, spanning the forms of
    // both names.
    for (index, destination) in DESTINATION_NAMES.iter().enumerate() {
        let ses = Ses::new();
        let set = format!("dup_message_set_{index}");
        assert_eq!(
            ses.create_set(json!({"ConfigurationSetName": set})).await.status,
            200,
            "case {destination:?}: the configuration set must be created first"
        );

        let stored = json!({"Enabled": true, "MatchingEventTypes": ["SEND"]});
        let created = ses.create_destination(&set, destination, stored.clone()).await;
        assert_eq!(
            created.status,
            200,
            "case {destination:?}: the first attach must answer 200, got {} ({})",
            created.status,
            created.message()
        );
        let held = reported_destination(&ses, &set, destination)
            .await
            .unwrap_or_else(|| panic!("case {destination:?}: the attached destination must be read back"));

        // Whatever the second request's definition is, the name is taken, so the sentence
        // is the one SES words for a taken destination name.
        for (case, definition) in second_definitions() {
            let duplicate = ses.create_destination(&set, destination, definition).await;
            refused(
                &format!("{destination:?} in {set:?} with {case}"),
                &duplicate,
                400,
                "AlreadyExistsException",
                &already_exists(destination, &set),
            );
            assert_eq!(
                reported_destination(&ses, &set, destination).await.as_ref(),
                Some(&held),
                "case {destination:?} with {case}: the refused attach must leave the stored \
                 definition alone, got {:?}",
                reported_destination(&ses, &set, destination).await
            );
        }

        // The other side of the rule, in the same set: a name the set does NOT hold is
        // attached rather than refused.
        let fresh = format!("{destination}-2");
        let accepted = ses.create_destination(&set, &fresh, stored.clone()).await;
        assert_eq!(
            accepted.status,
            200,
            "case {fresh:?}: a name the set does not hold must be attached, got {} ({})",
            accepted.status,
            accepted.message()
        );
    }
}

#[tokio::test]
async fn the_duplicate_sentence_names_whichever_destination_of_the_set_was_asked_for_again() {
    let ses = Ses::new();
    let set = "dup_message_many";
    assert_eq!(
        ses.create_set(json!({"ConfigurationSetName": set})).await.status,
        200,
        "the configuration set must be created first"
    );

    // A set holding several destinations: the sentence names the one the second request
    // asked for, whichever position in the set it occupies.
    let held = ["first-dest", "second_dest", "T"];
    for name in held {
        let created = ses
            .create_destination(set, name, json!({"Enabled": true, "MatchingEventTypes": ["OPEN"]}))
            .await;
        assert_eq!(
            created.status,
            200,
            "case {name:?}: the attach must answer 200, got {} ({})",
            created.status,
            created.message()
        );
    }

    for name in held {
        let duplicate = ses
            .create_destination(set, name, json!({"Enabled": true}))
            .await;
        refused(
            &format!("a second attach of {name:?}"),
            &duplicate,
            400,
            "AlreadyExistsException",
            &already_exists(name, set),
        );
        // Repeating the refused request answers the same sentence: the refusal is decided
        // from the store, not from a one-shot guard.
        let again = ses
            .create_destination(set, name, json!({"Enabled": true}))
            .await;
        refused(
            &format!("a third attach of {name:?}"),
            &again,
            400,
            "AlreadyExistsException",
            &already_exists(name, set),
        );
    }

    // Nothing the refusals touched: the set still reports exactly the destinations it was
    // given, no more.
    let mut reported: Vec<String> = reported_destinations(&ses, set)
        .await
        .iter()
        .map(|destination| {
            destination
                .get("Name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    reported.sort();
    let mut expected: Vec<String> = held.iter().map(|name| name.to_string()).collect();
    expected.sort();
    assert_eq!(
        reported, expected,
        "the refused attaches must leave the set holding exactly {expected:?}, got {reported:?}"
    );
}

#[tokio::test]
async fn a_destination_name_is_taken_only_within_the_set_that_holds_it() {
    let ses = Ses::new();
    let destination = "shared-dest-name";
    // Two sets, so the sentence is seen to name the set the request addressed and the name
    // is seen to be scoped to it.
    for set in ["dup_scope_a", "dup_scope_b"] {
        assert_eq!(
            ses.create_set(json!({"ConfigurationSetName": set})).await.status,
            200,
            "case {set:?}: the configuration set must be created first"
        );
        let created = ses
            .create_destination(set, destination, json!({"Enabled": true}))
            .await;
        assert_eq!(
            created.status,
            200,
            "case {set:?}: the same destination name must be free in each set, got {} ({})",
            created.status,
            created.message()
        );
    }

    for set in ["dup_scope_a", "dup_scope_b"] {
        let duplicate = ses
            .create_destination(set, destination, json!({"Enabled": true}))
            .await;
        refused(
            &format!("a second attach in {set:?}"),
            &duplicate,
            400,
            "AlreadyExistsException",
            &already_exists(destination, set),
        );
    }

    // And the name is taken only while the destination is there: a delete frees it, and the
    // attach that follows is accepted rather than refused.
    let set = "dup_scope_a";
    let deleted = ses.delete_destination(set, destination).await;
    assert_eq!(
        deleted.status,
        200,
        "the delete must answer 200, got {} ({})",
        deleted.status,
        deleted.message()
    );
    let reattached = ses
        .create_destination(set, destination, json!({"Enabled": false}))
        .await;
    assert_eq!(
        reattached.status,
        200,
        "a freed name must be attachable again, got {} ({})",
        reattached.status,
        reattached.message()
    );
    // While the other set, untouched, still refuses the name it holds.
    let duplicate = ses
        .create_destination("dup_scope_b", destination, json!({"Enabled": true}))
        .await;
    refused(
        "the set that still holds the name",
        &duplicate,
        400,
        "AlreadyExistsException",
        &already_exists(destination, "dup_scope_b"),
    );
}
