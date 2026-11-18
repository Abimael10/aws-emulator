// SPDX-License-Identifier: CC0-1.0

//! Which operations of this service require the idempotency token the model leaves
//! optional, read where a client reads the answer: off the `rpcv2Cbor` wire.
//!
//! The rule under test: every operation of this service whose input models an
//! `@idempotencyToken` `ClientToken` — its three CREATES, and only those
//! (`smithy_query(query="operation",operation="CreateEventBus,CreateEventSource,CreateSubscriber,UpdateEventBus,DeleteEventBus")`
//! shows the member on each create's input shape and on no update's or delete's) — requires
//! one: a request that states no token is refused `400` with the `InvalidInputException` the
//! model lists as a common error of every operation of this service, carrying the fixed text
//! that names the member the request has to carry, and mints NOTHING; the same request
//! carrying a token is served. An operation whose input models no such member is untouched by
//! the rule.
//!
//! The rows span what the rule generalises over:
//!
//! * every create of the service — the bus, the event source on it and the subscriber to it —
//!   so the requirement is not keyed to one operation or one input shape;
//! * what else the request carries: a create of a resource whose parent exists and one whose
//!   parent does not, so the missing token is answered from the REQUEST rather than after the
//!   parent is resolved, and the refusal is the same either way;
//! * the operations that model no token — a read, a listing, an update and a delete — each
//!   served without one, so the rule reaches the creates and stops there.
//!
//! Each refused create is followed by the SAME request carrying a token, which is served
//! under the same name: that is what says the refusal minted nothing, and it is read back
//! through the resource's own Describe.
//!
//! The wire harness, the input builders and the refusal decoder are
//! `modelled_input_refusal.rs`'s, reused rather than copied.

use engine::InMemoryEngine;

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

use eventsv2::ops::create_event_bus::CreateEventBus;
use eventsv2::ops::create_event_source::CreateEventSource;
use eventsv2::ops::create_subscriber::CreateSubscriber;
use eventsv2::ops::delete_event_bus::DeleteEventBus;
use eventsv2::ops::describe_event_bus::DescribeEventBus;
use eventsv2::ops::describe_event_source::DescribeEventSource;
use eventsv2::ops::describe_subscriber::DescribeSubscriber;
use eventsv2::ops::list_event_buses::ListEventBuses;
use eventsv2::ops::update_event_bus::UpdateEventBus;
use eventsv2::ops::types::{
    DeleteEventBusInput, DescribeEventBusInput, DescribeEventSourceInput, DescribeSubscriberInput,
    ListEventBusesInput, UpdateEventBusInput,
};

use crate::modelled_input_refusal::{
    create_bus, create_source, create_subscriber, refusal, rendered, unminted_arn, Wire,
    CLIENT_TOKEN_REQUIRED, INVALID_INPUT_EXCEPTION, REGION,
};

/// An `EventBusArn` of the modelled form whose identifier segment this service never minted,
/// so the bus a create names there does not exist.
fn absent_bus() -> String {
    unminted_arn(REGION, "no-such-bus", "qqqqqqqqqqqqqqqqqqqqqqqqq")
}

/// Assert one create was refused for its missing token: the status, the modelled error shape
/// and the whole fixed sentence.
fn assert_token_required(case: &str, status: u16, body: &[u8]) {
    assert_eq!(
        status, 400,
        "case {case:?}: a create stating no ClientToken must be refused, got {status} with {:?}",
        rendered(body)
    );
    let (code, message) = refusal(case, body);
    assert_eq!(
        code, INVALID_INPUT_EXCEPTION,
        "case {case:?}: expected {INVALID_INPUT_EXCEPTION}, got {code:?} with {message:?}"
    );
    assert_eq!(
        message, CLIENT_TOKEN_REQUIRED,
        "case {case:?}: expected {CLIENT_TOKEN_REQUIRED:?}, got {message:?}"
    );
}

/// Every create of this service refuses a request that states no `ClientToken`, mints nothing
/// for it, and serves the same request once it carries one — whichever create it is, and
/// whether or not the parent the request names exists.
#[tokio::test]
async fn every_create_requires_the_idempotency_token_its_input_models() {
    let wire = Wire::new();
    let bus = wire.active_bus("the host bus", "token-required-bus").await;
    let absent = absent_bus();

    // The bus create, named the same way twice: refused without a token, served with one.
    let case = "CreateEventBus";
    let (status, body) = wire
        .call::<CreateEventBus<InMemoryEngine>>(&create_bus("token-required-subject", None))
        .await;
    assert_token_required(case, status, &body);
    let created = wire
        .ok::<CreateEventBus<InMemoryEngine>>(
            case,
            &create_bus("token-required-subject", Some("a-bus-token")),
        )
        .await;
    let created_arn = created
        .event_bus_arn
        .clone()
        .unwrap_or_else(|| panic!("case {case:?}: expected a minted EventBusArn, got {created:?}"));
    let read = wire
        .ok::<DescribeEventBus<InMemoryEngine>>(
            case,
            &DescribeEventBusInput {
                event_bus_arn: created_arn.clone(),
            },
        )
        .await;
    assert_eq!(
        read.name.as_deref(),
        Some("token-required-subject"),
        "case {case:?}: the stored bus must carry the name the served create asked for, got {:?}",
        read.name
    );

    // The event source create, on the live bus and on a bus that does not exist: the missing
    // token is the answer either way, so it is read from the request rather than after the
    // parent is resolved.
    for (case, parent) in [
        ("CreateEventSource on a live bus", bus.as_str()),
        ("CreateEventSource on a bus that does not exist", absent.as_str()),
    ] {
        let (status, body) = wire
            .call::<CreateEventSource<InMemoryEngine>>(&create_source(
                "token-required-source",
                parent,
                None,
            ))
            .await;
        assert_token_required(case, status, &body);
    }
    let case = "CreateEventSource carrying a token";
    let source = wire
        .ok::<CreateEventSource<InMemoryEngine>>(
            case,
            &create_source("token-required-source", &bus, Some("a-source-token")),
        )
        .await;
    let source_arn = source.event_source_arn.clone().unwrap_or_else(|| {
        panic!("case {case:?}: expected a minted EventSourceArn, got {source:?}")
    });
    let read = wire
        .ok::<DescribeEventSource<InMemoryEngine>>(
            case,
            &DescribeEventSourceInput {
                event_source_arn: source_arn.clone(),
            },
        )
        .await;
    assert_eq!(
        read.name.as_deref(),
        Some("token-required-source"),
        "case {case:?}: the stored source must carry the name the served create asked for, got \
         {:?}",
        read.name
    );

    // And the subscriber create, the same two ways.
    for (case, parent) in [
        ("CreateSubscriber on a live bus", bus.as_str()),
        ("CreateSubscriber on a bus that does not exist", absent.as_str()),
    ] {
        let (status, body) = wire
            .call::<CreateSubscriber<InMemoryEngine>>(&create_subscriber(
                "token-required-subscriber",
                parent,
                None,
            ))
            .await;
        assert_token_required(case, status, &body);
    }
    let case = "CreateSubscriber carrying a token";
    let subscriber = wire
        .ok::<CreateSubscriber<InMemoryEngine>>(
            case,
            &create_subscriber("token-required-subscriber", &bus, Some("a-subscriber-token")),
        )
        .await;
    let subscriber_arn = subscriber.subscriber_arn.clone().unwrap_or_else(|| {
        panic!("case {case:?}: expected a minted SubscriberArn, got {subscriber:?}")
    });
    let read = wire
        .ok::<DescribeSubscriber<InMemoryEngine>>(
            case,
            &DescribeSubscriberInput {
                subscriber_arn: subscriber_arn.clone(),
            },
        )
        .await;
    assert_eq!(
        read.name.as_deref(),
        Some("token-required-subscriber"),
        "case {case:?}: the stored subscriber must carry the name the served create asked for, \
         got {:?}",
        read.name
    );
}

/// The rule reaches the creates and stops there: an operation whose input models no
/// `ClientToken` at all is served without one, so nothing of this service's surface is
/// refused for a member it never takes.
#[tokio::test]
async fn an_operation_that_models_no_idempotency_token_is_served_without_one() {
    let wire = Wire::new();
    let bus = wire.active_bus("the host bus", "token-scope-bus").await;

    let case = "ListEventBuses";
    let listed = wire
        .ok::<ListEventBuses<InMemoryEngine>>(
            case,
            &ListEventBusesInput {
                // `MaxResults` is a plain `i32` here, so a page size inside its modelled
                // `@range` is stated rather than left at the type's zero.
                max_results: 10,
                ..Default::default()
            },
        )
        .await;
    assert!(
        listed
            .event_buses
            .as_ref()
            .is_some_and(|buses| buses.iter().any(|held| held.event_bus_arn.as_deref()
                == Some(bus.as_str()))),
        "case {case:?}: expected the listing to hold the live bus, got {listed:?}"
    );

    let case = "UpdateEventBus";
    let updated = wire
        .ok::<UpdateEventBus<InMemoryEngine>>(
            case,
            &UpdateEventBusInput {
                event_bus_arn: bus.clone(),
                description: Some("updated without an idempotency token".to_string()),
                ..Default::default()
            },
        )
        .await;
    assert_eq!(
        updated.description.as_deref(),
        Some("updated without an idempotency token"),
        "case {case:?}: expected the update to apply what it carried, got {:?}",
        updated.description
    );

    let case = "DeleteEventBus";
    let (status, body) = wire
        .call::<DeleteEventBus<InMemoryEngine>>(&DeleteEventBusInput {
            event_bus_arn: bus.clone(),
        })
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the delete to be served without a token, got {status} with {:?}",
        rendered(&body)
    );
}
