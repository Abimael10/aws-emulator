// SPDX-License-Identifier: CC0-1.0

//! The sentence an event destination the configuration set does not hold is refused with,
//! end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with this service
//! registered — through the shared harness in the sibling [`crate::configuration_sets`]
//! suite, so routing, the handler and the error envelope are all exercised.
//!
//! The rule these cases pin: each operation that addresses an event destination WITHIN a
//! configuration set answers the not-found sentence the service words for that operation —
//! the update names both the destination the request addressed and the set it addressed it
//! within, each between literal angle brackets and with a closing period, while the delete
//! answers its own fixed sentence naming only the set — and a request naming a set the
//! account does not hold is refused for the SET instead, whichever destination name it
//! carries.
//!
//! The rows span what the rule generalises over: how the addressed name misses (never
//! held, removed by an earlier delete, differing only in case, differing in its
//! separators, held only by another set), the form of both names, and the definition the
//! update carries — none of which changes the sentence.

use serde_json::{json, Value};

use crate::configuration_set_event_destinations::{reported_destination, EventDestinations};
use crate::configuration_sets::{Reply, Ses};

/// The update of one event destination, as the model binds it
/// (`smithy_query(query="operation",operation="UpdateConfigurationSetEventDestination")`:
/// `PUT /v2/email/configuration-sets/{ConfigurationSetName}/event-destinations/{EventDestinationName}`).
trait UpdateDestination {
    async fn update_destination(&self, set: &str, name: &str, definition: Value) -> Reply;
}

impl UpdateDestination for Ses {
    async fn update_destination(&self, set: &str, name: &str, definition: Value) -> Reply {
        self.call(
            "PUT",
            &format!("/v2/email/configuration-sets/{set}/event-destinations/{name}"),
            Some(json!({"EventDestination": definition})),
        )
        .await
    }
}

/// The sentence the UPDATE answers for a destination the addressed set does not hold: the
/// destination name the request addressed and the set it addressed it within, each between
/// literal angle brackets.
fn does_not_exist(destination_name: &str, set_name: &str) -> String {
    format!(
        "An event destination with name <{destination_name}> does not exist for configuration \
         set <{set_name}>."
    )
}

/// The sentence the DELETE answers for the same miss: fixed text naming only the set the
/// request addressed, with no closing period.
fn not_found(set_name: &str) -> String {
    format!("No EventDestination found for {set_name}")
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

/// The definitions an update of a missing destination carries, so the refusal is seen to
/// depend on the addressed name alone.
fn definitions() -> Vec<(&'static str, Value)> {
    vec![
        ("an enabled definition", json!({"Enabled": true})),
        (
            "a definition matching several event types",
            json!({"Enabled": true, "MatchingEventTypes": ["SEND", "BOUNCE"]}),
        ),
        (
            "a definition naming a destination object",
            json!({
                "Enabled": false,
                "MatchingEventTypes": ["CLICK"],
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
async fn an_update_of_a_destination_the_set_does_not_hold_names_both_the_destination_and_the_set() {
    let ses = Ses::new();
    let set = "unknown_dest_set";
    let other_set = "unknown-dest-set-2";
    for name in [set, other_set] {
        assert_eq!(
            ses.create_set(json!({"ConfigurationSetName": name})).await.status,
            200,
            "case {name:?}: the configuration set must be created first"
        );
    }

    let held = "held-dest";
    assert_eq!(
        ses.create_destination(set, held, json!({"Enabled": true, "MatchingEventTypes": ["SEND"]}))
            .await
            .status,
        200,
        "attaching the destination that must survive: expected 200"
    );
    // A destination the set held and no longer holds is one the set does not hold, so one
    // attach-and-delete makes a row the refusals below cover.
    let removed = "removed_dest";
    assert_eq!(
        ses.create_destination(set, removed, json!({"Enabled": false}))
            .await
            .status,
        200,
        "attaching the destination to delete: expected 200"
    );
    assert_eq!(
        ses.delete_destination(set, removed).await.status,
        200,
        "the delete of {removed:?} must answer 200"
    );
    // The other set holds the same name, so a request that addresses it through the set
    // that does NOT hold it is a miss.
    assert_eq!(
        ses.create_destination(other_set, "only-here", json!({"Enabled": true}))
            .await
            .status,
        200,
        "attaching the other set's destination: expected 200"
    );

    for (case, addressed) in [
        ("a name the set never held", "never-there"),
        ("a name an earlier delete removed", removed),
        ("a name differing only in case", "HELD-DEST"),
        ("a name differing in its separators", "held_dest"),
        ("a name only another set holds", "only-here"),
        ("a one-character name", "d"),
    ] {
        for (definition_case, definition) in definitions() {
            let updated = ses.update_destination(set, addressed, definition).await;
            refused(
                &format!("{case} ({addressed:?}) with {definition_case}"),
                &updated,
                404,
                "NotFoundException",
                &does_not_exist(addressed, set),
            );
        }

        // The DELETE of the same miss answers the sentence the service words for IT: the
        // two operations are refused for the same reason and say it differently.
        let deleted = ses.delete_destination(set, addressed).await;
        refused(
            &format!("a delete of {addressed:?}"),
            &deleted,
            404,
            "NotFoundException",
            &not_found(set),
        );
    }

    // Neither refusal touched what the set holds.
    assert!(
        reported_destination(&ses, set, held).await.is_some(),
        "the refused calls must leave the destination the set holds in place"
    );
    assert!(
        reported_destination(&ses, set, removed).await.is_none(),
        "the refused calls must not resurrect the deleted destination"
    );
}

#[tokio::test]
async fn an_update_under_a_set_the_account_does_not_hold_is_refused_for_the_set() {
    let ses = Ses::new();

    // The other half of the rule: when the SET is the resource that is not there, the
    // refusal names the set, whichever destination name the request addressed.
    for absent in ["absent-set", "Absent_Set-2", "a"] {
        for addressed in ["held-dest", "never-there"] {
            let updated = ses
                .update_destination(absent, addressed, json!({"Enabled": true}))
                .await;
            refused(
                &format!("the set {absent:?} with the destination {addressed:?}"),
                &updated,
                404,
                "NotFoundException",
                &format!("Configuration set <{absent}> does not exist."),
            );
        }
    }
}

#[tokio::test]
async fn an_update_of_a_destination_the_set_holds_is_accepted_and_read_back() {
    let ses = Ses::new();
    let set = "updatable_set";
    assert_eq!(
        ses.create_set(json!({"ConfigurationSetName": set})).await.status,
        200,
        "the configuration set must be created first"
    );
    let name = "events";
    assert_eq!(
        ses.create_destination(set, name, json!({"Enabled": true, "MatchingEventTypes": ["SEND"]}))
            .await
            .status,
        200,
        "attaching the destination must answer 200"
    );

    // The side of the rule that places the refusals above: a destination the set DOES hold
    // is updated rather than refused, and the read reports what each update stored.
    for (case, definition, reported_types) in [
        (
            "another single event type",
            json!({"Enabled": true, "MatchingEventTypes": ["BOUNCE"]}),
            json!(["BOUNCE"]),
        ),
        (
            "several event types",
            json!({"Enabled": false, "MatchingEventTypes": ["OPEN", "CLICK"]}),
            json!(["CLICK", "OPEN"]),
        ),
    ] {
        let updated = ses.update_destination(set, name, definition).await;
        assert_eq!(
            updated.status,
            200,
            "case {case:?}: the update must answer 200, got {} ({})",
            updated.status,
            updated.message()
        );
        let reported = reported_destination(&ses, set, name)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: the updated destination must be read back"));
        assert_eq!(
            reported.get("MatchingEventTypes").cloned(),
            Some(reported_types.clone()),
            "case {case:?}: expected MatchingEventTypes {reported_types:?}, got {reported:?}"
        );
    }
}
