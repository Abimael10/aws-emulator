// SPDX-License-Identifier: CC0-1.0

//! What a MUTATION of a GraphQL API delivers to the subscriptions open on its real-time
//! endpoint.
//!
//! A GraphQL subscription of an AppSync API is not a field a client polls: it is started on
//! the API's real-time endpoint ([`super::realtime`]) and then RECEIVES what the API's
//! mutations produce. The schema is what links the two — `@aws_subscribe(mutations: […])` on
//! a subscription field names the mutations whose result that subscription receives
//! (`crate::state::GraphqlApiRecord::subscriptions_published_by`) — so a mutation served at
//! the API's GraphQL endpoint is published HERE, as the last step of executing it:
//!
//! * the value published is what the MUTATION FIELD resolved to, and a mutation field that
//!   resolved to nothing publishes nothing: there is no result to deliver;
//! * it goes to every live subscription on a subscription field that mutation is linked to,
//!   and to no other — a subscription on an unlinked field is one nothing delivers to;
//! * a subscription's own ARGUMENTS filter what it receives: AppSync delivers a published
//!   object to a subscription only where every argument the subscription states equals the
//!   same-named member of that object, which is what lets two subscribers on one field
//!   receive different mutations ([`matches_arguments`]);
//! * what the subscriber reads off its socket is the GraphQL result of its own subscription:
//!   the published object under the subscription FIELD's name, inside the `data` member of a
//!   `data` frame addressed by the subscription's own id ([`data_frame`]).
//!
//! The delivery is a real write over that subscriber's socket, through the queue the
//! connection's own task owns (`crate::state::GraphqlDataPlane::deliveries`), so a frame a
//! subscriber is sent is a frame it receives. Nothing is buffered without limit: a subscriber
//! whose queue is full is not reading its socket, and the frame is dropped for it rather than
//! held — the same allowance an Event API's subscriber gets
//! ([`crate::service::event_api::publish`]).

use ::serde_json::{json, Map as JsonMap, Value as Json};

use super::execution::SelectedField;
use super::PlaneContext;
use crate::state::graphql_data_plane::SubscriptionRecord;

/// The type of frame a subscriber receives a published mutation as, and the members it
/// carries: the GraphQL result, under the member every GraphQL answer carries its data in.
const DATA_FRAME_TYPE: &str = "data";
const PAYLOAD_MEMBER: &str = "payload";
const DATA_MEMBER: &str = "data";

/// Publish every field of one executed MUTATION to the subscriptions the API's schema links
/// it to.
///
/// `fields` is the mutation's own root selection and `data` what executing it answered, so
/// what is published is what the mutation RESOLVED TO — read under the field's response key,
/// which is its alias where the selection carried one.
pub async fn published(context: &PlaneContext, fields: &[SelectedField], data: &Json) {
    for field in fields {
        let Some(resolved) = data.get(&field.key).filter(|value| !value.is_null()) else {
            continue;
        };
        let subscribed = {
            let store = context.state.lock();
            store
                .graphql_apis
                .get(&context.api_id)
                .map(|api| api.subscriptions_published_by(&field.name))
                .unwrap_or_default()
        };
        for subscription_field in subscribed {
            delivered(context, &subscription_field, resolved).await;
        }
    }
}

/// Deliver `published` to every subscription of this API on `subscription_field` whose own
/// arguments match it.
///
/// The queues are taken out of the store and written to with no lock held. The fan-out
/// YIELDS between subscribers: how many there can be is bounded (the plane's connections per
/// API times its subscriptions per connection), and yielding keeps the executor serving other
/// connections while a wide fan-out is written.
async fn delivered(context: &PlaneContext, subscription_field: &str, published: &Json) {
    let deliveries = context
        .state
        .lock()
        .graphql_data_plane
        .deliveries(&context.api_id, subscription_field);
    for (subscription_id, frames, subscription) in deliveries {
        if !matches_arguments(&subscription, published) {
            continue;
        }
        let frame = data_frame(&subscription_id, subscription_field, published);
        if frames.try_send(frame).is_err() {
            ::tracing::warn!(
                api_id = %context.api_id,
                %subscription_id,
                "a subscriber is not reading its socket; a mutation was not delivered to it"
            );
        }
        ::tokio::task::yield_now().await;
    }
}

/// Whether `published` satisfies the arguments `subscription` was started with.
///
/// A subscription argument is a FILTER on the published object: AppSync delivers a mutation's
/// result to a subscription "only if the argument values match", so every argument the
/// subscription states has to equal the same-named member of the published object, and a
/// subscription stating none receives every mutation published to its field. An object
/// carrying no member of that name matches no value for it, so such a delivery is filtered
/// out rather than treated as unconstrained.
fn matches_arguments(subscription: &SubscriptionRecord, published: &Json) -> bool {
    subscription
        .arguments
        .iter()
        .all(|(name, wanted)| published.get(name) == Some(wanted))
}

/// The frame a published mutation is delivered to one subscription as: the GraphQL result of
/// that subscription — the published object under the subscription FIELD's name — addressed
/// by the subscription's own id.
fn data_frame(subscription_id: &str, subscription_field: &str, published: &Json) -> String {
    let mut data = JsonMap::new();
    data.insert(subscription_field.to_string(), published.clone());
    json!({
        "id": subscription_id,
        "type": DATA_FRAME_TYPE,
        PAYLOAD_MEMBER: { DATA_MEMBER: Json::Object(data) },
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    //! The pure rules this module owns. The delivery itself is pinned end to end in
    //! `tests/all/graphql_api_subscription_delivery.rs`, over a real socket.

    use super::*;

    /// A subscription started on `field` with `arguments`.
    fn subscription(arguments: Json) -> SubscriptionRecord {
        SubscriptionRecord {
            field: "onChange".to_string(),
            arguments: arguments.as_object().cloned().unwrap_or_default(),
        }
    }

    /// RULE: every argument a subscription states has to equal the same-named member of the
    /// published object, and a subscription stating none receives everything.
    #[test]
    fn a_subscriptions_arguments_filter_what_it_receives() {
        // The rows differ along how many arguments the subscription states, the TYPE of the
        // value each states, and whether the published object carries that member at all.
        let rows: &[(&str, Json, Json, bool)] = &[
            (
                "a subscription stating no argument",
                json!({}),
                json!({"id": "a", "owner": "alice"}),
                true,
            ),
            (
                "one argument the published object matches",
                json!({"owner": "alice"}),
                json!({"id": "a", "owner": "alice"}),
                true,
            ),
            (
                "one argument the published object does not match",
                json!({"owner": "alice"}),
                json!({"id": "a", "owner": "bob"}),
                false,
            ),
            (
                "two arguments, both matched",
                json!({"owner": "alice", "kind": "note"}),
                json!({"owner": "alice", "kind": "note"}),
                true,
            ),
            (
                "two arguments, one of them unmatched",
                json!({"owner": "alice", "kind": "note"}),
                json!({"owner": "alice", "kind": "task"}),
                false,
            ),
            (
                "an argument the published object carries no member for",
                json!({"owner": "alice"}),
                json!({"id": "a"}),
                false,
            ),
            (
                "a numeric argument the published object matches",
                json!({"version": 3}),
                json!({"version": 3}),
                true,
            ),
            (
                "a numeric argument stated as the text of that number",
                json!({"version": "3"}),
                json!({"version": 3}),
                false,
            ),
            (
                "a boolean argument the published object matches",
                json!({"done": false}),
                json!({"done": false}),
                true,
            ),
        ];
        for (case, arguments, published, expected) in rows {
            let actual = matches_arguments(&subscription(arguments.clone()), published);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected}, got {actual}"
            );
        }
    }

    /// RULE: a subscriber reads the published object as the GraphQL result of its OWN
    /// subscription — under the subscription field's name, inside the frame's `data`.
    #[test]
    fn the_frame_carries_the_published_object_under_the_subscription_field() {
        let frame = data_frame("sub-1", "onChange", &json!({"id": "a", "owner": "alice"}));
        let parsed: Json = ::serde_json::from_str(&frame)
            .unwrap_or_else(|error| panic!("the frame must be JSON ({error}): {frame}"));
        let expected = json!({
            "id": "sub-1",
            "type": "data",
            "payload": {"data": {"onChange": {"id": "a", "owner": "alice"}}},
        });
        assert_eq!(
            parsed, expected,
            "expected {expected}, got {parsed} for the delivered frame"
        );
    }
}
