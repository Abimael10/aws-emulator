// SPDX-License-Identifier: CC0-1.0

//! What this service answers a create that states no `ClientToken`, read where a client
//! reads the answer: off the `rpcv2Cbor` wire.
//!
//! The rule under test: a member the model leaves OPTIONAL is optional on the wire, so no
//! operation of this service may refuse a request for omitting one. Every operation whose
//! input models the `@idempotencyToken` `ClientToken` — its three CREATES, and only those
//! (`smithy_query(query="operation",operation="CreateEventBus,CreateEventSource,CreateSubscriber,UpdateEventBus,DeleteEventBus")`
//! shows the member on each create's input shape, tagged `@idempotencyToken` and not
//! `@required`, and on no update's or delete's) — serves a request that states none and
//! mints its resource, which the resource's own Describe then resolves. The token buys
//! idempotency and nothing else: a create that states none keeps no replay record, so a
//! second create of the same name is the name collision rather than a replay of the first.
//!
//! The rows span what the rule generalises over:
//!
//! * every create of the service — the bus, the event source on it and the subscriber to it
//!   — so the rule is not keyed to one operation or one input shape;
//! * what else the request carries: a bus create naming only the modelled-required member
//!   and one naming an in-range storage configuration as well, so being served is not keyed
//!   to one request shape;
//! * the operations that model no token — a read, a listing, an update and a delete — each
//!   served without one, so nothing of this service's surface asks for a member it never
//!   takes.
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
    CreateEventBusInput, DeleteEventBusInput, DescribeEventBusInput, DescribeEventSourceInput,
    DescribeSubscriberInput, ListEventBusesInput, StorageConfiguration, UpdateEventBusInput,
};

use crate::modelled_input_refusal::{
    create_bus, create_source, create_subscriber, refusal, rendered, Wire,
};

/// The error shape a create of a name already taken answers with, and the whole `Message`
/// of that class — spelled out here rather than read from the crate, so a reworded refusal
/// fails this suite.
const RESOURCE_ALREADY_EXISTS_EXCEPTION: &str = "ResourceAlreadyExistsException";
const RESOURCE_ALREADY_EXISTS: &str = "Resource already exists";

/// A `RetentionPeriodInDays` inside the modelled range, used to vary what a tokenless
/// create carries beside its required member.
/// Source: `smithy_query(query="shape", shape="StorageConfiguration")` — `@range` 1..365.
const RETENTION_CEILING: i32 = 365;

/// Every create of this service serves a request that states no `ClientToken` and mints
/// its resource, whichever create it is and whatever else the request carries — and the
/// resource it reports is the one the store holds, read back through its own Describe.
#[tokio::test]
async fn every_create_is_served_without_the_idempotency_token_the_model_leaves_optional() {
    let wire = Wire::new();
    let host = wire.active_bus("the host bus", "tokenless-host-bus").await;
    let mut minted: Vec<String> = vec![host.clone()];

    // The bus create, twice: naming only the modelled-required member, and naming an
    // in-range storage configuration as well.
    for (case, name, retention) in [
        ("CreateEventBus naming only Name", "tokenless-plain-bus", None),
        (
            "CreateEventBus naming a storage configuration too",
            "tokenless-stored-bus",
            Some(RETENTION_CEILING),
        ),
    ] {
        let created = wire
            .ok::<CreateEventBus<InMemoryEngine>>(
                case,
                &CreateEventBusInput {
                    name: name.to_string(),
                    storage_configuration: retention.map(|days| StorageConfiguration {
                        retention_period_in_days: Some(days),
                    }),
                    client_token: None,
                    ..Default::default()
                },
            )
            .await;
        assert_eq!(
            created.name.as_deref(),
            Some(name),
            "case {case:?}: expected the requested name {name:?}, got {:?}",
            created.name
        );
        assert_eq!(
            created.state.as_deref(),
            Some("CREATING"),
            "case {case:?}: expected the bus reported CREATING, got {:?}",
            created.state
        );
        let arn = created.event_bus_arn.clone().unwrap_or_else(|| {
            panic!("case {case:?}: expected a minted EventBusArn, got {created:?}")
        });
        if let Some(days) = retention {
            assert_eq!(
                created
                    .storage_configuration
                    .as_ref()
                    .and_then(|storage| storage.retention_period_in_days),
                Some(days),
                "case {case:?}: expected RetentionPeriodInDays {days}, got {:?}",
                created.storage_configuration
            );
        }
        let read = wire
            .ok::<DescribeEventBus<InMemoryEngine>>(
                case,
                &DescribeEventBusInput {
                    event_bus_arn: arn.clone(),
                },
            )
            .await;
        assert_eq!(
            read.name.as_deref(),
            Some(name),
            "case {case:?}: the stored bus must carry the name the create asked for, got {:?}",
            read.name
        );
        minted.push(arn);
    }

    // The event source create on the live bus.
    let case = "CreateEventSource";
    let source = wire
        .ok::<CreateEventSource<InMemoryEngine>>(
            case,
            &create_source("tokenless-plain-source", &host, None),
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
        Some("tokenless-plain-source"),
        "case {case:?}: the stored source must carry the name the create asked for, got {:?}",
        read.name
    );
    assert_eq!(
        read.event_bus_arn.as_deref(),
        Some(host.as_str()),
        "case {case:?}: the stored source must sit on the bus the create named, got {:?}",
        read.event_bus_arn
    );
    minted.push(source_arn);

    // And the subscriber create on the same bus.
    let case = "CreateSubscriber";
    let subscriber = wire
        .ok::<CreateSubscriber<InMemoryEngine>>(
            case,
            &create_subscriber("tokenless-plain-subscriber", &host, None),
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
        Some("tokenless-plain-subscriber"),
        "case {case:?}: the stored subscriber must carry the name the create asked for, got {:?}",
        read.name
    );
    assert_eq!(
        read.event_bus_arn.as_deref(),
        Some(host.as_str()),
        "case {case:?}: the stored subscriber must sit on the bus the create named, got {:?}",
        read.event_bus_arn
    );
    minted.push(subscriber_arn);

    // Every tokenless create minted a resource of its own: no two of them share an ARN.
    let mut distinct = minted.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        minted.len(),
        "each tokenless create must mint a resource of its own, got {minted:?}"
    );
}

/// A create that states no token keeps NO replay record, so a second create of the same
/// name is the name collision rather than a replay of the first — on the bus create and on
/// the subscriber create, which model the same optional member.
#[tokio::test]
async fn a_create_stating_no_token_keeps_no_replay_record() {
    let wire = Wire::new();

    let case = "a second tokenless CreateEventBus of the same name";
    let first = wire
        .ok::<CreateEventBus<InMemoryEngine>>(case, &create_bus("replayless-bus", None))
        .await;
    let first_arn = first
        .event_bus_arn
        .clone()
        .unwrap_or_else(|| panic!("case {case:?}: expected a minted EventBusArn, got {first:?}"));
    let (status, body) = wire
        .call::<CreateEventBus<InMemoryEngine>>(&create_bus("replayless-bus", None))
        .await;
    assert_eq!(
        status, 409,
        "case {case:?}: a second create of the same name must collide, got {status} with {:?}",
        rendered(&body)
    );
    let (code, message) = refusal(case, &body);
    assert_eq!(
        code, RESOURCE_ALREADY_EXISTS_EXCEPTION,
        "case {case:?}: expected {RESOURCE_ALREADY_EXISTS_EXCEPTION}, got {code:?} with \
         {message:?}"
    );
    assert_eq!(
        message, RESOURCE_ALREADY_EXISTS,
        "case {case:?}: expected {RESOURCE_ALREADY_EXISTS:?}, got {message:?}"
    );

    // A tokenless create of a name of its own mints a resource of its own rather than
    // replaying the first.
    let case = "a tokenless CreateEventBus of another name";
    let second = wire
        .ok::<CreateEventBus<InMemoryEngine>>(case, &create_bus("replayless-bus-again", None))
        .await;
    assert_ne!(
        second.event_bus_arn.as_deref(),
        Some(first_arn.as_str()),
        "case {case:?}: a tokenless create must mint its own bus, got {:?}",
        second.event_bus_arn
    );

    let case = "a second tokenless CreateSubscriber of the same name";
    let host = wire.active_bus("the host bus", "replayless-host-bus").await;
    let subscriber = wire
        .ok::<CreateSubscriber<InMemoryEngine>>(
            case,
            &create_subscriber("replayless-subscriber", &host, None),
        )
        .await;
    assert!(
        subscriber.subscriber_arn.is_some(),
        "case {case:?}: expected a minted SubscriberArn, got {subscriber:?}"
    );
    let (status, body) = wire
        .call::<CreateSubscriber<InMemoryEngine>>(&create_subscriber(
            "replayless-subscriber",
            &host,
            None,
        ))
        .await;
    assert_eq!(
        status, 409,
        "case {case:?}: a second create of the same name must collide, got {status} with {:?}",
        rendered(&body)
    );
    let (code, message) = refusal(case, &body);
    assert_eq!(
        code, RESOURCE_ALREADY_EXISTS_EXCEPTION,
        "case {case:?}: expected {RESOURCE_ALREADY_EXISTS_EXCEPTION}, got {code:?} with \
         {message:?}"
    );
}

/// The operations that model no `ClientToken` at all are served without one, so nothing of
/// this service's surface is refused for a member it never takes.
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
