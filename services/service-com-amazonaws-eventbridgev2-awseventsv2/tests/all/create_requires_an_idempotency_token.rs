// SPDX-License-Identifier: CC0-1.0

//! What this service answers a create that states no `ClientToken`, read where a client
//! reads the answer: off the `rpcv2Cbor` wire.
//!
//! The rule under test: **every create of this service requires the caller to state the
//! `ClientToken` its input models — a request that states none is refused `400`
//! `InvalidInputException` with the whole message `ClientToken is required`, and mints
//! nothing.** The token is what makes a retried create a replay rather than a second
//! resource, so the service asks the caller for it; the model tags the member
//! `@idempotencyToken` and not `@required` because a code-generated client fills it in
//! itself, which is why the requirement is the service's rather than the constraint walk's.
//! Every operation whose input models the member — its three CREATES, and only those
//! (`smithy_query(query="operation",operation="CreateEventBus,CreateEventSource,CreateSubscriber,UpdateEventBus,DeleteEventBus")`
//! shows the member on each create's input shape, tagged `@idempotencyToken` and not
//! `@required`, and on no update's or delete's) — is held to it, while an operation that
//! models no token is served without one.
//!
//! The rows span what the rule generalises over:
//!
//! * every create of the service — the bus, the event source on it and the subscriber to it
//!   — so the rule is not keyed to one operation or one input shape;
//! * what else the request carries: a bus create naming only the modelled-required member
//!   and one naming an in-range storage configuration as well, so the refusal is not keyed
//!   to one request shape;
//! * a create whose OTHER members would be refused too — one naming a bus that does not
//!   exist — which is refused for the member it states rather than for the token;
//! * the operations that model no token — a read, a listing, an update and a delete — each
//!   served without one, so nothing of this service's surface asks for a member it never
//!   takes.
//!
//! Each refusal is followed by the read that proves nothing was minted for it: the create
//! is repeated with a token and the resource it then mints is the first one of that name.
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
    create_bus, create_source, create_subscriber, refusal, rendered, unminted_arn,
    with_client_token, Wire, CLIENT_TOKEN_REQUIRED, INVALID_INPUT_EXCEPTION, REGION,
};

/// The error shape a create naming a bus that is not there answers with.
const RESOURCE_NOT_FOUND_EXCEPTION: &str = "ResourceNotFoundException";

/// A `RetentionPeriodInDays` inside the modelled range, used to vary what a create carries
/// beside its required member.
/// Source: `smithy_query(query="shape", shape="StorageConfiguration")` — `@range` 1..365.
const RETENTION_CEILING: i32 = 365;

/// Assert one tokenless create was refused for the token and nothing else.
fn assert_refused_for_the_token(case: &str, status: u16, body: &[u8]) {
    assert_eq!(
        status,
        400,
        "case {case:?}: a create stating no ClientToken must answer 400, got {status} with \
         {:?}",
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

/// Every create of this service refuses a request that states no `ClientToken`, whichever
/// create it is and whatever else the request carries — and mints nothing for it, which the
/// create that follows with a token shows by minting the first resource of that name.
#[tokio::test]
async fn every_create_refuses_a_request_that_states_no_idempotency_token() {
    let wire = Wire::new();
    let host = wire.active_bus("the host bus", "tokenless-host-bus").await;

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
        let tokenless = CreateEventBusInput {
            name: name.to_string(),
            storage_configuration: retention.map(|days| StorageConfiguration {
                retention_period_in_days: Some(days),
            }),
            client_token: None,
            ..Default::default()
        };
        let (status, body) = wire
            .call::<CreateEventBus<InMemoryEngine>>(&tokenless)
            .await;
        assert_refused_for_the_token(case, status, &body);

        // Nothing was minted for the refused create: the same create carrying a token is
        // served, so the name was never taken.
        let created = wire
            .ok::<CreateEventBus<InMemoryEngine>>(case, &with_client_token(tokenless.clone()))
            .await;
        let arn = created.event_bus_arn.clone().unwrap_or_else(|| {
            panic!("case {case:?}: expected a minted EventBusArn, got {created:?}")
        });
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
    }

    // The event source create on the live bus.
    let case = "CreateEventSource";
    let tokenless = create_source("tokenless-plain-source", &host, None);
    let (status, body) = wire
        .call::<CreateEventSource<InMemoryEngine>>(&tokenless)
        .await;
    assert_refused_for_the_token(case, status, &body);
    let source = wire
        .ok::<CreateEventSource<InMemoryEngine>>(case, &with_client_token(tokenless.clone()))
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
        read.event_bus_arn.as_deref(),
        Some(host.as_str()),
        "case {case:?}: the stored source must sit on the bus the create named, got {:?}",
        read.event_bus_arn
    );

    // And the subscriber create on the same bus.
    let case = "CreateSubscriber";
    let tokenless = create_subscriber("tokenless-plain-subscriber", &host, None);
    let (status, body) = wire
        .call::<CreateSubscriber<InMemoryEngine>>(&tokenless)
        .await;
    assert_refused_for_the_token(case, status, &body);
    let subscriber = wire
        .ok::<CreateSubscriber<InMemoryEngine>>(case, &with_client_token(tokenless.clone()))
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
        read.event_bus_arn.as_deref(),
        Some(host.as_str()),
        "case {case:?}: the stored subscriber must sit on the bus the create named, got {:?}",
        read.event_bus_arn
    );
}

/// A tokenless create whose OTHER members name a resource that is not there is refused for
/// the token it states, which is the input the service reads before it resolves anything —
/// and the same create carrying a token is then refused for the bus it names.
#[tokio::test]
async fn a_tokenless_create_is_refused_before_the_resources_it_names_are_resolved() {
    let wire = Wire::new();
    let absent = unminted_arn(REGION, "no-such-bus", "zzzzzzzzzzzzzzzzzzzzzzzzz");

    let case = "CreateEventSource with no ClientToken on a bus that does not exist";
    let tokenless = create_source("orphan-source", &absent, None);
    let (status, body) = wire
        .call::<CreateEventSource<InMemoryEngine>>(&tokenless)
        .await;
    assert_refused_for_the_token(case, status, &body);

    let case = "CreateEventSource stating a token on a bus that does not exist";
    let (status, body) = wire
        .call::<CreateEventSource<InMemoryEngine>>(&with_client_token(tokenless))
        .await;
    assert_eq!(
        status, 404,
        "case {case:?}: a create naming a bus that does not exist must be refused for that \
         bus, got {status} with {:?}",
        rendered(&body)
    );
    let (code, message) = refusal(case, &body);
    assert_eq!(
        code, RESOURCE_NOT_FOUND_EXCEPTION,
        "case {case:?}: expected {RESOURCE_NOT_FOUND_EXCEPTION}, got {code:?} with {message:?}"
    );
    assert!(
        message.contains("no-such-bus"),
        "case {case:?}: the refusal must name the bus the request addressed, got {message:?}"
    );
}

/// A create stating a token mints a resource of its own, and two creates each stating their
/// own token mint two resources — so the requirement above buys idempotency rather than
/// collapsing creates onto one resource.
#[tokio::test]
async fn a_create_stating_a_token_mints_a_resource_of_its_own() {
    let wire = Wire::new();

    let case = "two bus creates, each stating a token of its own";
    let first = wire
        .ok::<CreateEventBus<InMemoryEngine>>(case, &create_bus("tokened-one", Some("token-one")))
        .await;
    let second = wire
        .ok::<CreateEventBus<InMemoryEngine>>(case, &create_bus("tokened-two", Some("token-two")))
        .await;
    assert_ne!(
        first.event_bus_arn, second.event_bus_arn,
        "case {case:?}: each create must mint its own bus, got {:?} twice",
        first.event_bus_arn
    );

    let case = "a second create of the same name, stating a token of its own";
    let (status, body) = wire
        .call::<CreateEventBus<InMemoryEngine>>(&create_bus("tokened-one", Some("token-three")))
        .await;
    assert_eq!(
        status, 409,
        "case {case:?}: a second create of the same name must collide, got {status} with {:?}",
        rendered(&body)
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
