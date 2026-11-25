// SPDX-License-Identifier: CC0-1.0

//! What this service does with a create that states no `ClientToken`, read where a client
//! reads it: off the `rpcv2Cbor` wire, and — for the fan-out a bus exists for — in the
//! messages the subscriber's target queue holds.
//!
//! The rule under test: **a member the model marks `@idempotencyToken` and not `@required`
//! is OPTIONAL, so a create that omits it is served and mints its resource; the token is
//! read only to replay a retry that states one.**
//! `smithy_query(query="operation",operation="CreateEventBus,CreateEventSource,CreateSubscriber")`
//! shows the member on each create's input tagged `@idempotencyToken`, with `Name` (and the
//! bus and configuration members) the only `@required` ones — a code-generated client fills
//! the token in itself, which is precisely why the contract leaves it optional and the
//! service must not demand it.
//!
//! The rows span what the rule generalises over:
//!
//! * every create of the service — the bus, the event source on it and the subscriber to it
//!   — so the rule is not keyed to one operation or one input shape;
//! * what else a tokenless request carries: a bus create naming only the modelled-required
//!   member, and ones naming the published bounds of `RetentionPeriodInDays`, so the answer
//!   is not keyed to one request shape;
//! * the name grammar, from the shortest name the pattern admits to one carrying every
//!   punctuation character it allows;
//! * what a missing token must NOT buy: two tokenless creates of different names mint two
//!   resources (a create with no token replays nothing), while a second create of a name
//!   already held still collides — and a retry that DOES state a token still replays the one
//!   resource that token minted.
//!
//! * the operations that model no token at all — a listing, an update and a delete — each
//!   served without one, so nothing of this service's surface asks for a member it never
//!   takes; and a tokenless create naming a bus that does not exist, refused for THAT bus,
//!   so a create is resolved on the resources it names.
//!
//! The last suite is the consequence a client watches for: with the bus minted by a
//! tokenless create and the subscriber on it minted by another, a published event is
//! delivered to the subscriber's SQS target, so that queue's own
//! `ApproximateNumberOfMessages` rises off zero and the delivered events are retrievable
//! there. The depth is polled for the CONDITION under a generous deadline rather than waited
//! out.
//!
//! The emulator (EventBridge V2 and Amazon SQS on ONE engine), both wire harnesses, the
//! queue and the two readers are `subscriber_delivery.rs`'s, reused rather than copied; what
//! is here is the tokenless request builders and the rows.

use std::time::Instant;

use engine::InMemoryEngine;
use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

use eventsv2::ops::create_event_bus::CreateEventBus;
use eventsv2::ops::create_event_source::CreateEventSource;
use eventsv2::ops::create_subscriber::CreateSubscriber;
use eventsv2::ops::delete_event_bus::DeleteEventBus;
use eventsv2::ops::describe_event_bus::DescribeEventBus;
use eventsv2::ops::describe_event_source::DescribeEventSource;
use eventsv2::ops::describe_subscriber::DescribeSubscriber;
use eventsv2::ops::list_event_buses::ListEventBuses;
use eventsv2::ops::put_events::PutEvents;
use eventsv2::ops::types::{
    AwsServiceEventsSourceConfiguration, CreateEventBusInput, CreateEventSourceInput,
    CreateSubscriberInput, DeleteEventBusInput, DescribeEventBusInput, DescribeEventSourceInput,
    DescribeSubscriberInput, EventSourceConfiguration, InvokeConfiguration, ListEventBusesInput,
    PutEventsInput, PutEventsRequestEntry, StorageConfiguration, UpdateEventBusInput,
};
use eventsv2::ops::update_event_bus::UpdateEventBus;

use crate::modelled_input_refusal::{
    refusal, rendered, unminted_arn, REGION, RESOURCE_NOT_FOUND_EXCEPTION,
};
use crate::subscriber_delivery::{
    emulator, eventsv2_call, eventsv2_ok, given_queue, queue_depth, received_bodies, PATIENCE,
    POLL_INTERVAL,
};

/// The role a subscriber's `InvokeConfiguration` names, of the modelled `RoleArn` shape.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/event-delivery";

/// The published bounds of `RetentionPeriodInDays`.
/// Source: `smithy_query(query="shape", shape="StorageConfiguration")` — `@range` 1..365.
const RETENTION_FLOOR: i32 = 1;
const RETENTION_CEILING: i32 = 365;

// ---------------------------------------------------------------------------
// The requests a row is built from, each stating NO ClientToken
// ---------------------------------------------------------------------------

/// A `CreateEventBusInput` naming `name` and, when `retention` is stated, that retention
/// period — and stating no `ClientToken` at all.
fn tokenless_bus(name: &str, retention: Option<i32>) -> CreateEventBusInput {
    CreateEventBusInput {
        name: name.to_string(),
        storage_configuration: retention.map(|days| StorageConfiguration {
            retention_period_in_days: Some(days),
        }),
        client_token: None,
        ..Default::default()
    }
}

/// A `CreateEventSourceInput` on `event_bus_arn`, stating no `ClientToken`.
fn tokenless_source(name: &str, event_bus_arn: &str) -> CreateEventSourceInput {
    CreateEventSourceInput {
        name: name.to_string(),
        event_bus_arn: event_bus_arn.to_string(),
        configuration: EventSourceConfiguration::AwsServiceEventsConfiguration(
            AwsServiceEventsSourceConfiguration {
                aws_service: "aws.s3".to_string(),
                ..Default::default()
            },
        ),
        description: None,
        tags: None,
        client_token: None,
    }
}

/// A `CreateSubscriberInput` on `event_bus_arn` delivering to `target_arn`, stating no
/// `ClientToken`.
fn tokenless_subscriber(
    name: &str,
    event_bus_arn: &str,
    target_arn: &str,
) -> CreateSubscriberInput {
    CreateSubscriberInput {
        name: name.to_string(),
        event_bus_arn: event_bus_arn.to_string(),
        invoke_configuration: InvokeConfiguration {
            role_arn: ROLE_ARN.to_string(),
            target_arn: target_arn.to_string(),
            ..Default::default()
        },
        client_token: None,
        ..Default::default()
    }
}

/// The bus `create` mints, polled until it is `ACTIVE` — the state `CreateSubscriber`
/// requires of it.
///
/// `subscriber_delivery.rs`'s own helper states a token of its own, which is the one thing a
/// row here must not do, so the create is driven from the input the row built.
async fn given_active_bus(
    router: &MultiServiceRouter,
    case: &str,
    create: &CreateEventBusInput,
) -> String {
    let created = eventsv2_ok::<CreateEventBus<InMemoryEngine>>(router, create, case).await;
    let arn = created
        .event_bus_arn
        .unwrap_or_else(|| panic!("case {case:?}: CreateEventBus reported no EventBusArn"));
    let deadline = Instant::now() + PATIENCE;
    loop {
        let described = eventsv2_ok::<DescribeEventBus<InMemoryEngine>>(
            router,
            &DescribeEventBusInput {
                event_bus_arn: arn.clone(),
            },
            case,
        )
        .await;
        if described.state.as_deref() == Some("ACTIVE") {
            return arn;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the bus {arn} never became ACTIVE, last state {:?}",
            described.state
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll the queue's own depth until it reports at least `wanted`, answering the last depth it
/// reported.
async fn depth_reaching(
    router: &MultiServiceRouter,
    case: &str,
    queue_url: &str,
    wanted: u64,
) -> u64 {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let depth = queue_depth(router, case, queue_url).await;
        if depth >= wanted || Instant::now() >= deadline {
            return depth;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One event entry carrying `detail`.
fn entry(source: &str, detail_type: &str, detail: &str) -> PutEventsRequestEntry {
    PutEventsRequestEntry {
        source: source.to_string(),
        detail_type: detail_type.to_string(),
        detail: Some(detail.to_string()),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// The rule, over a table
// ---------------------------------------------------------------------------

/// Every create of this service is SERVED when the request states no `ClientToken`, whichever
/// create it is and whatever else the request carries — and the resource it minted is read
/// back through that resource's own Describe.
#[tokio::test]
async fn every_create_is_served_when_it_states_no_idempotency_token() {
    let router = emulator();

    // (case, the bus name, the RetentionPeriodInDays the request names)
    // The retention rows are the published bounds of the member plus a request naming no
    // storage configuration at all; the name rows span the grammar the pattern admits.
    let rows = [
        ("naming only the required member", "tokenless-plain-bus", None),
        ("the shortest name the grammar admits", "A", None),
        (
            "every punctuation character the name admits",
            "A.b-c_9",
            None,
        ),
        (
            "the retention floor",
            "tokenless-floor-bus",
            Some(RETENTION_FLOOR),
        ),
        (
            "the retention ceiling",
            "tokenless-ceiling-bus",
            Some(RETENTION_CEILING),
        ),
    ];

    for (case, name, retention) in rows {
        let created = eventsv2_ok::<CreateEventBus<InMemoryEngine>>(
            &router,
            &tokenless_bus(name, retention),
            case,
        )
        .await;
        assert_eq!(
            created.name.as_deref(),
            Some(name),
            "case {case:?}: expected the requested name {name:?}, got {:?}",
            created.name
        );
        let arn = created.event_bus_arn.clone().unwrap_or_else(|| {
            panic!("case {case:?}: expected a minted EventBusArn, got {created:?}")
        });
        let read = eventsv2_ok::<DescribeEventBus<InMemoryEngine>>(
            &router,
            &DescribeEventBusInput {
                event_bus_arn: arn.clone(),
            },
            case,
        )
        .await;
        assert_eq!(
            read.name.as_deref(),
            Some(name),
            "case {case:?}: the stored bus must carry the name the create asked for, got {:?}",
            read.name
        );
        if let Some(days) = retention {
            assert_eq!(
                read.storage_configuration
                    .as_ref()
                    .and_then(|storage| storage.retention_period_in_days),
                Some(days),
                "case {case:?}: the stored bus must carry the retention period the create \
                 asked for, got {:?}",
                read.storage_configuration
            );
        }
    }

    // The event source and the subscriber, both on a bus a tokenless create minted.
    let case = "the host bus of the source and the subscriber";
    let bus = given_active_bus(&router, case, &tokenless_bus("tokenless-host-bus", None)).await;

    let case = "CreateEventSource stating no ClientToken";
    let created = eventsv2_ok::<CreateEventSource<InMemoryEngine>>(
        &router,
        &tokenless_source("tokenless-source", &bus),
        case,
    )
    .await;
    let source_arn = created.event_source_arn.clone().unwrap_or_else(|| {
        panic!("case {case:?}: expected a minted EventSourceArn, got {created:?}")
    });
    let read = eventsv2_ok::<DescribeEventSource<InMemoryEngine>>(
        &router,
        &DescribeEventSourceInput {
            event_source_arn: source_arn.clone(),
        },
        case,
    )
    .await;
    assert_eq!(
        read.event_bus_arn.as_deref(),
        Some(bus.as_str()),
        "case {case:?}: the stored source must sit on the bus the create named, got {:?}",
        read.event_bus_arn
    );

    let case = "CreateSubscriber stating no ClientToken";
    let (_, queue_arn) = given_queue(&router, case, "tokenless-create-target").await;
    let created = eventsv2_ok::<CreateSubscriber<InMemoryEngine>>(
        &router,
        &tokenless_subscriber("tokenless-subscriber", &bus, &queue_arn),
        case,
    )
    .await;
    let subscriber_arn = created.subscriber_arn.clone().unwrap_or_else(|| {
        panic!("case {case:?}: expected a minted SubscriberArn, got {created:?}")
    });
    let read = eventsv2_ok::<DescribeSubscriber<InMemoryEngine>>(
        &router,
        &DescribeSubscriberInput {
            subscriber_arn: subscriber_arn.clone(),
        },
        case,
    )
    .await;
    assert_eq!(
        read.event_bus_arn.as_deref(),
        Some(bus.as_str()),
        "case {case:?}: the stored subscriber must sit on the bus the create named, got {:?}",
        read.event_bus_arn
    );
}

/// What an absent token does NOT buy: a create that states none replays nothing, so two such
/// creates of different names mint two resources and a second create of a name already held
/// still collides — while a retry that DOES state a token replays the one resource that token
/// minted.
#[tokio::test]
async fn an_absent_token_replays_nothing_while_a_stated_one_still_does() {
    let router = emulator();

    let case = "two tokenless bus creates of different names";
    let first = eventsv2_ok::<CreateEventBus<InMemoryEngine>>(
        &router,
        &tokenless_bus("tokenless-one", None),
        case,
    )
    .await;
    let second = eventsv2_ok::<CreateEventBus<InMemoryEngine>>(
        &router,
        &tokenless_bus("tokenless-two", None),
        case,
    )
    .await;
    assert_ne!(
        first.event_bus_arn, second.event_bus_arn,
        "case {case:?}: each create must mint its own bus, got {:?} twice",
        first.event_bus_arn
    );

    let case = "a second tokenless create of a name already held";
    let (status, body) = eventsv2_call::<CreateEventBus<InMemoryEngine>>(
        &router,
        &tokenless_bus("tokenless-one", None),
    )
    .await;
    assert_eq!(
        status,
        409,
        "case {case:?}: a second create of the same name must collide, got {status} with {:?}",
        String::from_utf8_lossy(&body)
    );

    let case = "two creates stating one and the same token";
    let stated = CreateEventBusInput {
        name: "tokened-replay".to_string(),
        client_token: Some("a-stated-token".to_string()),
        ..Default::default()
    };
    let minted = eventsv2_ok::<CreateEventBus<InMemoryEngine>>(&router, &stated, case).await;
    let replayed = eventsv2_ok::<CreateEventBus<InMemoryEngine>>(&router, &stated, case).await;
    assert_eq!(
        minted.event_bus_arn, replayed.event_bus_arn,
        "case {case:?}: the retry must replay the bus the token minted, got {:?} then {:?}",
        minted.event_bus_arn, replayed.event_bus_arn
    );
}

/// A create that states no token is resolved on the resources it NAMES: one naming a bus
/// that does not exist is refused `404` `ResourceNotFoundException` naming that bus, which is
/// the refusal the removed token check used to pre-empt. Over both creates that name a bus.
#[tokio::test]
async fn a_tokenless_create_is_refused_for_the_bus_it_names() {
    let router = emulator();
    // A well-formed `EventBusArn` this service never minted: the identifier segment is the
    // 25 characters the modelled pattern pins.
    let absent = unminted_arn(REGION, "no-such-bus", "zzzzzzzzzzzzzzzzzzzzzzzzz");

    let case = "CreateEventSource stating no ClientToken on a bus that does not exist";
    let (status, body) = eventsv2_call::<CreateEventSource<InMemoryEngine>>(
        &router,
        &tokenless_source("orphan-source", &absent),
    )
    .await;
    assert_refused_for_the_bus(case, status, &body);

    let case = "CreateSubscriber stating no ClientToken on a bus that does not exist";
    let (_, queue_arn) = given_queue(&router, case, "tokenless-orphan-target").await;
    let (status, body) = eventsv2_call::<CreateSubscriber<InMemoryEngine>>(
        &router,
        &tokenless_subscriber("orphan-subscriber", &absent, &queue_arn),
    )
    .await;
    assert_refused_for_the_bus(case, status, &body);
}

/// Assert one create was refused for the bus it named, and that the refusal names that bus.
fn assert_refused_for_the_bus(case: &str, status: u16, body: &[u8]) {
    assert_eq!(
        status,
        404,
        "case {case:?}: a create naming a bus that does not exist must answer 404, got \
         {status} with {:?}",
        rendered(body)
    );
    let (code, message) = refusal(case, body);
    assert_eq!(
        code, RESOURCE_NOT_FOUND_EXCEPTION,
        "case {case:?}: expected {RESOURCE_NOT_FOUND_EXCEPTION}, got {code:?} with {message:?}"
    );
    assert!(
        message.contains("no-such-bus"),
        "case {case:?}: the refusal must name the bus the request addressed, got {message:?}"
    );
}

/// The operations that model no `ClientToken` at all are served without one, so nothing of
/// this service's surface asks for a member it never takes.
#[tokio::test]
async fn an_operation_that_models_no_idempotency_token_is_served_without_one() {
    let router = emulator();
    let case = "the bus the read, the update and the delete address";
    let bus = given_active_bus(&router, case, &tokenless_bus("token-scope-bus", None)).await;

    let case = "ListEventBuses";
    let listed = eventsv2_ok::<ListEventBuses<InMemoryEngine>>(
        &router,
        &ListEventBusesInput {
            // `MaxResults` is a plain `i32` here, so a page size inside its modelled
            // `@range` is stated rather than left at the type's zero.
            max_results: 10,
            ..Default::default()
        },
        case,
    )
    .await;
    assert!(
        listed.event_buses.as_ref().is_some_and(|buses| buses
            .iter()
            .any(|held| held.event_bus_arn.as_deref() == Some(bus.as_str()))),
        "case {case:?}: expected the listing to hold the live bus, got {listed:?}"
    );

    let case = "UpdateEventBus";
    let updated = eventsv2_ok::<UpdateEventBus<InMemoryEngine>>(
        &router,
        &UpdateEventBusInput {
            event_bus_arn: bus.clone(),
            description: Some("updated without an idempotency token".to_string()),
            ..Default::default()
        },
        case,
    )
    .await;
    assert_eq!(
        updated.description.as_deref(),
        Some("updated without an idempotency token"),
        "case {case:?}: expected the update to apply what it carried, got {:?}",
        updated.description
    );

    let case = "DeleteEventBus";
    let (status, body) = eventsv2_call::<DeleteEventBus<InMemoryEngine>>(
        &router,
        &DeleteEventBusInput {
            event_bus_arn: bus.clone(),
        },
    )
    .await;
    assert_eq!(
        status,
        200,
        "case {case:?}: expected the delete to be served without a token, got {status} with \
         {:?}",
        rendered(&body)
    );
}

/// One row of the fan-out: how many events the publish carries.
struct FanOut {
    case: &'static str,
    /// The payloads the row publishes, one entry each.
    details: &'static [&'static str],
}

/// The consequence a client watches for: with the bus and the subscriber on it BOTH minted by
/// creates that stated no `ClientToken`, a published event is delivered to the subscriber's
/// SQS target, so that queue's own `ApproximateNumberOfMessages` rises off zero by one per
/// delivered event and every delivered event is retrievable there.
#[tokio::test]
async fn a_tokenless_bus_and_subscriber_deliver_their_events_to_the_target_queue() {
    let rows = [
        FanOut {
            case: "one published event",
            details: &[r#"{"orderId":"A-1"}"#],
        },
        FanOut {
            case: "three published events",
            details: &[
                r#"{"orderId":"B-1"}"#,
                r#"{"orderId":"B-2"}"#,
                r#"{"orderId":"B-3"}"#,
            ],
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let (queue_url, queue_arn) =
            given_queue(&router, case, &format!("tokenless-fan-out-{index}")).await;
        let bus = given_active_bus(
            &router,
            case,
            &tokenless_bus(&format!("tokenless-fan-out-bus-{index}"), None),
        )
        .await;
        eventsv2_ok::<CreateSubscriber<InMemoryEngine>>(
            &router,
            &tokenless_subscriber(
                &format!("tokenless-fan-out-subscriber-{index}"),
                &bus,
                &queue_arn,
            ),
            case,
        )
        .await;

        // Nothing has been published yet, so the queue holds nothing: the depth below is the
        // delivery's own doing rather than a queue that was never empty.
        let before = queue_depth(&router, case, &queue_url).await;
        assert_eq!(
            before, 0,
            "case {case:?}: a queue nothing was delivered to must report depth 0, got {before}"
        );

        let entries: Vec<PutEventsRequestEntry> = row
            .details
            .iter()
            .map(|detail| entry("orders.service", "OrderPlaced", detail))
            .collect();
        let published = eventsv2_ok::<PutEvents<InMemoryEngine>>(
            &router,
            &PutEventsInput {
                event_bus_arn: bus.clone(),
                entries,
                deduplication_configuration: None,
            },
            case,
        )
        .await;
        assert_eq!(
            published.failed_entry_count, 0,
            "case {case:?}: every entry must be accepted, got {published:?}"
        );

        let wanted = row.details.len() as u64;
        let depth = depth_reaching(&router, case, &queue_url, wanted).await;
        assert_eq!(
            depth, wanted,
            "case {case:?}: the subscriber's target queue must report {wanted} message(s) \
             available, got {depth}"
        );

        // And the messages the depth counted are the published events, retrievable there.
        let bodies = received_bodies(&router, case, &queue_url, row.details.len()).await;
        let delivered: Vec<Value> = bodies
            .iter()
            .map(|body| body.pointer("/detail").cloned().unwrap_or(Value::Null))
            .collect();
        for detail in row.details {
            let expected: Value =
                serde_json::from_str(detail).expect("the row's payload is a document");
            assert!(
                delivered.contains(&expected),
                "case {case:?}: expected the delivered details to hold {expected}, got \
                 {delivered:?}"
            );
        }

        // Every delivery was taken off the queue, so the depth the queue reports now is what
        // is left rather than what was delivered.
        let drained = queue_depth(&router, case, &queue_url).await;
        assert_eq!(
            drained, 0,
            "case {case:?}: a drained queue must report depth 0, got {drained}"
        );
    }
}
