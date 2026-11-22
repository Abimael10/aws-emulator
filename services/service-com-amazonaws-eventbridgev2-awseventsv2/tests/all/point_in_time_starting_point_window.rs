// SPDX-License-Identifier: CC0-1.0

//! The window a `POINT_IN_TIME` subscriber's `StartingPoint` has to fall in.
//!
//! RULE: a `POINT_IN_TIME` subscriber is placed inside its bus's RETAINED log,
//! so a `TIMESTAMP` point has to name an instant that log can still answer for:
//! one that has already happened, old enough to have been stored (five
//! minutes), and no earlier than the window the bus reports as
//! `StorageConfiguration.RetentionWindowStartTime` — "the earliest point in time
//! from which stored events are available". A point outside that window names no
//! reachable position, so `CreateSubscriber` is refused with the modelled
//! `InvalidInputException` (400) and mints nothing. `PointType` `HORIZON` names
//! no instant of the request — the bus itself decides where its log begins — and
//! a configuration carried beside any other `StartingPosition` is "only
//! applicable when StartingPosition is POINT_IN_TIME", so neither is placed
//! against the window.
//!
//! The rows span that class: they differ along WHERE the point falls (a minute,
//! an hour and a day in the future; the instant of the call; one and four
//! minutes old; just past the five-minute bound; a day old; the epoch), along
//! the member being absent where the model makes it required, and along the two
//! shapes that are accepted (the `HORIZON` control, and a configuration beside a
//! `LATEST` position). Every refusal's whole `Message` is asserted, and the one
//! message with a variable part is asserted to carry the REQUEST's own
//! timestamp at whole-second UTC and nothing else of the request.
//!
//! Nothing here waits on the wall clock: the one wait polls for the bus becoming
//! `ACTIVE` under a generous deadline, and every instant a row names is a
//! request VALUE rather than something asserted about elapsed time.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aws_smithy_schema::codec::{Codec, FinishSerializer};
use aws_smithy_schema::serde::{SerializableStruct, ShapeSerializer};
use aws_smithy_schema_server::CborCodec;
use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use engine::InMemoryEngine;
use smithy_server_runtime::{
    EmbeddedEmulator, MultiServiceRouter, Operation, OperationContext, OperationError,
};

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

use eventsv2::ops::create_event_bus::CreateEventBus;
use eventsv2::ops::create_subscriber::{CreateSubscriber, CreateSubscriberErrorModeled};
use eventsv2::ops::describe_event_bus::DescribeEventBus;
use eventsv2::ops::describe_subscriber::DescribeSubscriber;
use eventsv2::ops::list_subscribers::ListSubscribers;
use eventsv2::ops::put_events::PutEvents;
use eventsv2::ops::types::{
    CreateEventBusInput, CreateSubscriberInput, CreateSubscriberOutput, DescribeEventBusInput,
    DescribeSubscriberInput, InvokeConfiguration, ListSubscribersInput, PointInTimeConfiguration,
    PutEventsInput, PutEventsRequestEntry,
};
use eventsv2::scaffolded::Scaffolded;

/// The region every request below is signed for; it must reach the minted ARNs.
const REGION: &str = "us-west-2";

/// The role a subscriber's `InvokeConfiguration` names, of the modelled
/// `RoleArn` shape.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/event-delivery";

/// The queue a subscriber delivers to. Nothing is published here, so it only
/// has to be a well-formed target.
const TARGET_ARN: &str = "arn:aws:sqs:us-west-2:000000000000:point-in-time-target";

/// The whole `Message` of the refusal of a point that has not happened yet:
/// fixed text, written out here rather than read from the service, so this suite
/// pins the wording instead of restating whatever the service carries.
const IN_FUTURE_MESSAGE: &str = "PointInTimeConfiguration.StartingPoint must not be in the future";

/// The whole `Message` of the refusal of a point too recent to have been stored.
/// The bound is stated in the sentence, so a build applying a different width
/// could not word it this way.
const TOO_RECENT_MESSAGE: &str =
    "PointInTimeConfiguration.StartingPoint must be at least 5 minutes in the past";

/// The whole `Message` of the refusal of a `TIMESTAMP` point carrying no
/// instant at all — "Required when PointType is TIMESTAMP", per the member's own
/// documentation.
const REQUIRED_MESSAGE: &str =
    "PointInTimeConfiguration.StartingPoint is required when PointType is TIMESTAMP";

/// `StartingPosition::POINT_IN_TIME`, from the modelled enum.
const POINT_IN_TIME: &str = "POINT_IN_TIME";

/// `StartingPosition::LATEST`, the other entry of that enum.
const LATEST: &str = "LATEST";

/// `PointType::TIMESTAMP`, from the modelled enum.
const TIMESTAMP: &str = "TIMESTAMP";

/// `PointType::HORIZON`, the other entry of that enum.
const HORIZON: &str = "HORIZON";

/// How long a poll waits for a condition the emulator owes: far above the
/// provisioning span a bus takes, so a loaded host has ample room.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long one pass of a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The whole `Message` of the refusal of a point earlier than the bus's
/// retention window: fixed text around ONE variable part, `stamp` — the
/// `StartingPoint` the request named, at whole-second UTC.
fn outside_window_message(stamp: &str) -> String {
    format!(
        "PointInTimeConfiguration.StartingPoint ({stamp}) falls outside the bus's retention window"
    )
}

/// A SigV4 `Authorization` header naming the region and this service's signing
/// name, which is how a request names both.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{REGION}/events/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// The request metadata of a call signed for [`REGION`].
fn parts() -> http::request::Parts {
    let (parts, ()) = http::Request::builder()
        .method("POST")
        .uri("/service/AWSEventsV2/operation/CreateSubscriber")
        .header("authorization", authorization())
        .header("content-type", "application/cbor")
        .body(())
        .expect("the harness request is well formed")
        .into_parts();
    parts
}

/// An `InvokeConfiguration` naming the role and target every row shares.
fn invoke() -> InvokeConfiguration {
    InvokeConfiguration {
        role_arn: ROLE_ARN.to_string(),
        target_arn: TARGET_ARN.to_string(),
        ..Default::default()
    }
}

/// Whole epoch seconds at the moment this is read, which every row's instant is
/// expressed relative to.
fn epoch_seconds_now() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is after the epoch")
            .as_secs(),
    )
    .expect("an epoch second of this era fits an i64")
}

// ---------------------------------------------------------------------------
// The typed harness
// ---------------------------------------------------------------------------

/// One emulator over one shared engine, with a request context every typed call
/// reuses.
struct Harness {
    engine: Arc<InMemoryEngine>,
    definition: eventsv2::ServiceDefinition<InMemoryEngine>,
    callback: EmbeddedEmulator,
    parts: http::request::Parts,
}

impl Harness {
    fn new() -> Self {
        let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
        let definition = eventsv2::ServiceDefinition::new(Arc::clone(&engine));
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        let callback = EmbeddedEmulator::from_router(router);
        Self {
            engine,
            definition,
            callback,
            parts: parts(),
        }
    }

    fn ctx(&self) -> OperationContext<'_> {
        OperationContext::new(&self.parts, self.definition.identity(), self.callback.clone())
    }

    /// The service's own store, for the assertions that are about what is
    /// STORED rather than about a response.
    fn store(&self) -> engine::ServiceState<eventsv2::ComAmazonawsEventbridgev2Awseventsv2State> {
        eventsv2::state::service_state(&*self.engine)
            .expect("the in-memory engine always has this service's store")
    }

    /// A bus that has finished provisioning, which is the state a subscriber can
    /// be created against. The wait polls for `ACTIVE` rather than a duration.
    async fn active_bus(&self, name: &str) -> String {
        let arn = CreateEventBus::new(Arc::clone(&self.engine))
            .handle(
                CreateEventBusInput {
                    name: name.to_string(),
                    client_token: Some(format!("token-for-{name}")),
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {name:?}: CreateEventBus must succeed, got {e:?}"))
            .event_bus_arn
            .unwrap_or_else(|| panic!("case {name:?}: CreateEventBus must report an EventBusArn"));

        let active = eventsv2::state::BusLifecycleState::Active.wire_value();
        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let state = DescribeEventBus::new(Arc::clone(&self.engine))
                .handle(
                    DescribeEventBusInput {
                        event_bus_arn: arn.clone(),
                    },
                    &self.ctx(),
                )
                .await
                .unwrap_or_else(|e| panic!("case {name:?}: DescribeEventBus must succeed, {e:?}"))
                .state;
            if state.as_deref() == Some(active) {
                return arn;
            }
            assert!(
                Instant::now() < deadline,
                "case {name:?}: the bus never became {active}, last state {state:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// The bus's `StorageConfiguration.RetentionWindowStartTime` as a read
    /// reports it: the window a row's point is placed against.
    async fn retention_window_start(&self, case: &str, bus: &str) -> DateTime {
        DescribeEventBus::new(Arc::clone(&self.engine))
            .handle(
                DescribeEventBusInput {
                    event_bus_arn: bus.to_string(),
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: DescribeEventBus must succeed, got {e:?}"))
            .storage_configuration
            .and_then(|storage| storage.retention_window_start_time)
            .unwrap_or_else(|| {
                panic!("case {case:?}: a read must report a RetentionWindowStartTime")
            })
    }

    async fn create_subscriber(
        &self,
        input: CreateSubscriberInput,
    ) -> Result<
        CreateSubscriberOutput,
        eventsv2::ops::create_subscriber::CreateSubscriberError,
    > {
        CreateSubscriber::new(Arc::clone(&self.engine))
            .handle(input, &self.ctx())
            .await
    }

    /// Publish `count` events onto `bus`, so its log has a backlog a
    /// `POINT_IN_TIME` subscriber can be placed inside.
    async fn publish(&self, case: &str, bus: &str, count: usize) {
        let entries: Vec<PutEventsRequestEntry> = (0..count)
            .map(|index| PutEventsRequestEntry {
                source: "orders.service".to_string(),
                detail_type: "order.created".to_string(),
                detail: Some(format!(r#"{{"orderId":"A-{index}"}}"#)),
                ..Default::default()
            })
            .collect();
        let answer = PutEvents::new(Arc::clone(&self.engine))
            .handle(
                PutEventsInput {
                    event_bus_arn: bus.to_string(),
                    entries,
                    deduplication_configuration: None,
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: PutEvents must succeed, got {e:?}"));
        assert_eq!(
            answer.failed_entry_count, 0,
            "case {case:?}: every entry must be accepted, got {answer:?}"
        );
    }

    /// The `StartingPosition` a stored subscriber reports through
    /// `DescribeSubscriber` — the API-visible half of an accepted create.
    async fn described_starting_position(&self, case: &str, arn: &str) -> Option<String> {
        DescribeSubscriber::new(Arc::clone(&self.engine))
            .handle(
                DescribeSubscriberInput {
                    subscriber_arn: arn.to_string(),
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: DescribeSubscriber must succeed, got {e:?}"))
            .starting_position
    }

    /// The names of the subscribers stored on `bus`, which is where a refused
    /// create would show up had it minted anything.
    async fn subscriber_names(&self, case: &str, bus: &str) -> Vec<String> {
        ListSubscribers::new(Arc::clone(&self.engine))
            .handle(
                ListSubscribersInput {
                    event_bus_arn: Some(bus.to_string()),
                    max_results: 100,
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: ListSubscribers must succeed, got {e:?}"))
            .subscribers
            .unwrap_or_default()
            .into_iter()
            .map(|summary| summary.name.unwrap_or_default())
            .collect()
    }
}

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

/// What a row expects of its create.
enum Expect {
    /// The create succeeds and reports the starting position asked for.
    Accepted,
    /// The create is refused with this whole `Message`.
    Refused(String),
}

/// One row: the configuration a create carries and what it must answer.
struct Row {
    case: &'static str,
    name: &'static str,
    starting_position: Option<&'static str>,
    point: Option<PointInTimeConfiguration>,
    expect: Expect,
}

/// A `PointInTimeConfiguration` of `point_type` starting at `starting_point`.
fn point(point_type: &str, starting_point: Option<DateTime>) -> Option<PointInTimeConfiguration> {
    Some(PointInTimeConfiguration {
        point_type: point_type.to_string(),
        starting_point,
        end_point: None,
    })
}

/// RULE: a `TIMESTAMP` starting point outside the bus's replayable window is
/// refused with `InvalidInputException` (400) and mints nothing, while a
/// `HORIZON` point and a configuration beside a `LATEST` position are accepted.
#[tokio::test]
async fn a_point_in_time_starting_point_outside_the_window_is_refused() {
    let harness = Harness::new();
    let bus = harness.active_bus("point-in-time-bus").await;
    let window = harness
        .retention_window_start("the window", &bus)
        .await
        .secs();
    let now = epoch_seconds_now();

    // One day before the window the bus itself reports, which is the far side of
    // the retention window.
    let before_window = DateTime::from_secs(window - 86_400);

    let rows = vec![
        Row {
            case: "one hour in the future",
            name: "pit-future-hour",
            starting_position: Some(POINT_IN_TIME),
            point: point(TIMESTAMP, Some(DateTime::from_secs(now + 3_600))),
            expect: Expect::Refused(IN_FUTURE_MESSAGE.to_string()),
        },
        Row {
            case: "one minute in the future",
            name: "pit-future-minute",
            starting_position: Some(POINT_IN_TIME),
            point: point(TIMESTAMP, Some(DateTime::from_secs(now + 60))),
            expect: Expect::Refused(IN_FUTURE_MESSAGE.to_string()),
        },
        Row {
            case: "one day in the future",
            name: "pit-future-day",
            starting_position: Some(POINT_IN_TIME),
            point: point(TIMESTAMP, Some(DateTime::from_secs(now + 86_400))),
            expect: Expect::Refused(IN_FUTURE_MESSAGE.to_string()),
        },
        Row {
            case: "the instant of the call",
            name: "pit-now",
            starting_position: Some(POINT_IN_TIME),
            point: point(TIMESTAMP, Some(DateTime::from_secs(now))),
            expect: Expect::Refused(TOO_RECENT_MESSAGE.to_string()),
        },
        Row {
            case: "one minute in the past",
            name: "pit-recent-minute",
            starting_position: Some(POINT_IN_TIME),
            point: point(TIMESTAMP, Some(DateTime::from_secs(now - 60))),
            expect: Expect::Refused(TOO_RECENT_MESSAGE.to_string()),
        },
        Row {
            case: "four minutes in the past, still under the bound",
            name: "pit-recent-four",
            starting_position: Some(POINT_IN_TIME),
            point: point(TIMESTAMP, Some(DateTime::from_secs(now - 240))),
            expect: Expect::Refused(TOO_RECENT_MESSAGE.to_string()),
        },
        Row {
            // Just PAST the five-minute bound: old enough to have been stored,
            // and so refused for the other reason — this bus has no log that
            // old, its window starting where the bus itself does.
            case: "five minutes and five seconds in the past",
            name: "pit-past-the-bound",
            starting_position: Some(POINT_IN_TIME),
            point: point(TIMESTAMP, Some(DateTime::from_secs(now - 305))),
            expect: Expect::Refused(outside_window_message(&stamp_of(now - 305))),
        },
        Row {
            case: "one day in the past",
            name: "pit-old-day",
            starting_position: Some(POINT_IN_TIME),
            point: point(TIMESTAMP, Some(DateTime::from_secs(now - 86_400))),
            expect: Expect::Refused(outside_window_message(&stamp_of(now - 86_400))),
        },
        Row {
            case: "a day before the window the bus reports",
            name: "pit-before-window",
            starting_position: Some(POINT_IN_TIME),
            point: point(TIMESTAMP, Some(before_window)),
            expect: Expect::Refused(outside_window_message(&stamp_of(before_window.secs()))),
        },
        Row {
            case: "the epoch",
            name: "pit-epoch",
            starting_position: Some(POINT_IN_TIME),
            point: point(TIMESTAMP, Some(DateTime::from_secs(0))),
            expect: Expect::Refused(outside_window_message(&stamp_of(0))),
        },
        Row {
            case: "a TIMESTAMP point carrying no instant",
            name: "pit-no-instant",
            starting_position: Some(POINT_IN_TIME),
            point: point(TIMESTAMP, None),
            expect: Expect::Refused(REQUIRED_MESSAGE.to_string()),
        },
        Row {
            // The control: POINT_IN_TIME itself is not what is refused.
            case: "the HORIZON control",
            name: "pit-horizon",
            starting_position: Some(POINT_IN_TIME),
            point: point(HORIZON, None),
            expect: Expect::Accepted,
        },
        Row {
            // "Only applicable when StartingPosition is POINT_IN_TIME": beside
            // LATEST the configuration names no point to place.
            case: "a future point beside a LATEST position",
            name: "pit-latest",
            starting_position: Some(LATEST),
            point: point(TIMESTAMP, Some(DateTime::from_secs(now + 3_600))),
            expect: Expect::Accepted,
        },
        Row {
            case: "a future point beside no stated position at all",
            name: "pit-default",
            starting_position: None,
            point: point(TIMESTAMP, Some(DateTime::from_secs(now + 3_600))),
            expect: Expect::Accepted,
        },
    ];

    for row in rows {
        let case = row.case;
        let answered = harness
            .create_subscriber(CreateSubscriberInput {
                name: row.name.to_string(),
                event_bus_arn: bus.clone(),
                invoke_configuration: invoke(),
                starting_position: row.starting_position.map(str::to_string),
                point_in_time_configuration: row.point.clone(),
                client_token: Some(format!("token-for-{}", row.name)),
                ..Default::default()
            })
            .await;

        match row.expect {
            Expect::Accepted => {
                let created = answered.unwrap_or_else(|e| {
                    panic!("case {case:?}: CreateSubscriber must succeed, got {e:?}")
                });
                let expected_position = row.starting_position.unwrap_or(LATEST);
                assert_eq!(
                    created.starting_position.as_deref(),
                    Some(expected_position),
                    "case {case:?}: expected StartingPosition {expected_position:?}, got {:?}",
                    created.starting_position
                );
            }
            Expect::Refused(expected) => {
                let error = answered
                    .err()
                    .unwrap_or_else(|| panic!("case {case:?}: CreateSubscriber must be refused"));
                assert_eq!(
                    error.shape_name(),
                    "InvalidInputException",
                    "case {case:?}: expected InvalidInputException, got {:?}",
                    error.shape_name()
                );
                assert_eq!(
                    error.http_status(),
                    400,
                    "case {case:?}: expected HTTP 400, got {}",
                    error.http_status()
                );
                match &error {
                    Scaffolded::Modeled(CreateSubscriberErrorModeled::InvalidInputException(
                        invalid,
                    )) => assert_eq!(
                        invalid.message.as_deref(),
                        Some(expected.as_str()),
                        "case {case:?}: expected {expected:?}, got {:?}",
                        invalid.message
                    ),
                    other => {
                        panic!("case {case:?}: expected InvalidInputException, got {other:?}")
                    }
                }
                // A refused create mints NOTHING: the name is still free.
                assert!(
                    harness
                        .store()
                        .lock()
                        .subscriber_on_bus_by_name(&bus, row.name)
                        .is_none(),
                    "case {case:?}: a refused create must leave no subscriber behind"
                );
            }
        }
    }

    // Read back through the enumeration a client would use: exactly the
    // accepted subscribers are on the bus, none of the refused names.
    let listed = harness.subscriber_names("the enumeration", &bus).await;
    assert_eq!(
        listed,
        vec![
            "pit-default".to_string(),
            "pit-horizon".to_string(),
            "pit-latest".to_string()
        ],
        "case \"the enumeration\": expected only the accepted subscribers, got {listed:?}"
    );
}

// ---------------------------------------------------------------------------
// The other side of the window: a point inside it is accepted, and placed
// ---------------------------------------------------------------------------

/// RULE: a `TIMESTAMP` point INSIDE the window is accepted, and it places the
/// subscriber at the first event its bus received at or after that instant —
/// the end of the log when every stored event is older than the point. That is
/// the position the backlog a `POINT_IN_TIME` subscriber is owed starts from.
///
/// The window's near side is five minutes and its far side is the bus's own
/// `RetentionWindowStartTime`, which never precedes the bus's creation, so a bus
/// minted moments ago has no instant inside it at all. Rather than wait out
/// hours, this moves the stored instants of the bus and of its events BACK —
/// the one way a test drives this clock without sleeping — and then asks for
/// points on either side of the backlog, plus the five-minute bound and one
/// case inside it.
///
/// Every subscriber here is created `STOPPED`, because a subscriber that is
/// delivering is handed its backlog by the create itself and its position
/// advances past it; a stopped one keeps the backlog, so the position it was
/// PLACED at is the one that can be read back.
#[tokio::test]
async fn a_point_inside_the_window_is_accepted_and_placed_on_the_log() {
    let harness = Harness::new();
    let bus = harness.active_bus("inside-window-bus").await;
    harness.publish("inside-window-bus", &bus, 2).await;

    // The bus is made four hours old and its two events three hours old, so the
    // whole window — from the bus's creation to five minutes ago — is reachable.
    let moment = SystemTime::now();
    let (first_sequence, tip) = {
        let store = harness.store();
        let mut state = store.lock();
        let stored = state
            .event_buses
            .get_mut(&bus)
            .expect("the bus the create minted is in the store");
        stored.creation_time = moment - Duration::from_secs(4 * 3_600);
        for event in &mut stored.events {
            event.received_at = moment - Duration::from_secs(3 * 3_600);
        }
        let first = stored
            .events
            .first()
            .map(|event| event.sequence_number)
            .expect("the publish above stored its events");
        (first, stored.next_sequence_number)
    };
    assert!(
        first_sequence < tip,
        "the backlog must sit before the tip, got first {first_sequence} and tip {tip}"
    );

    let now = epoch_seconds_now();
    // (case, name, the point asked for, the position it must leave the
    // subscriber at — or None for the row that is refused as too recent)
    let rows: Vec<(&str, &str, Option<PointInTimeConfiguration>, Option<u64>)> = vec![
        (
            "a point before every stored event",
            "inside-before",
            point(TIMESTAMP, Some(DateTime::from_secs(now - 3 * 3_600 - 1_800))),
            Some(first_sequence),
        ),
        (
            "the HORIZON, which is the same position",
            "inside-horizon",
            point(HORIZON, None),
            Some(first_sequence),
        ),
        (
            "a point after every stored event",
            "inside-after",
            point(TIMESTAMP, Some(DateTime::from_secs(now - 3_600))),
            Some(tip),
        ),
        (
            // Just past the near side of the window: accepted here, where the
            // bus is old enough to have a window that reaches it.
            "five minutes and five seconds in the past",
            "inside-bound",
            point(TIMESTAMP, Some(DateTime::from_secs(now - 305))),
            Some(tip),
        ),
        (
            // One case inside the bound, which is refused however old the bus.
            "four minutes in the past, inside the bound",
            "inside-too-recent",
            point(TIMESTAMP, Some(DateTime::from_secs(now - 240))),
            None,
        ),
    ];

    for (case, name, requested, expected_position) in rows {
        let answered = harness
            .create_subscriber(CreateSubscriberInput {
                name: name.to_string(),
                event_bus_arn: bus.clone(),
                invoke_configuration: invoke(),
                starting_position: Some(POINT_IN_TIME.to_string()),
                point_in_time_configuration: requested.clone(),
                state: Some("STOPPED".to_string()),
                client_token: Some(format!("token-for-{name}")),
                ..Default::default()
            })
            .await;

        let Some(expected_position) = expected_position else {
            let error = answered
                .err()
                .unwrap_or_else(|| panic!("case {case:?}: CreateSubscriber must be refused"));
            match &error {
                Scaffolded::Modeled(CreateSubscriberErrorModeled::InvalidInputException(
                    invalid,
                )) => assert_eq!(
                    invalid.message.as_deref(),
                    Some(TOO_RECENT_MESSAGE),
                    "case {case:?}: expected {TOO_RECENT_MESSAGE:?}, got {:?}",
                    invalid.message
                ),
                other => panic!("case {case:?}: expected InvalidInputException, got {other:?}"),
            }
            continue;
        };

        let created = answered
            .unwrap_or_else(|e| panic!("case {case:?}: CreateSubscriber must succeed, got {e:?}"));
        assert_eq!(
            created.starting_position.as_deref(),
            Some(POINT_IN_TIME),
            "case {case:?}: expected StartingPosition POINT_IN_TIME, got {:?}",
            created.starting_position
        );
        let arn = created
            .subscriber_arn
            .unwrap_or_else(|| panic!("case {case:?}: the create must report a SubscriberArn"));
        assert_eq!(
            harness.described_starting_position(case, &arn).await.as_deref(),
            Some(POINT_IN_TIME),
            "case {case:?}: a read must report the position the create answered"
        );
        let position = harness
            .store()
            .lock()
            .subscriber(&arn)
            .map(|subscriber| subscriber.next_sequence_number)
            .unwrap_or_else(|| panic!("case {case:?}: the accepted create must store a subscriber"));
        assert_eq!(
            position, expected_position,
            "case {case:?}: expected the subscriber placed at {expected_position}, got {position}"
        );
    }
}

/// RULE: the one message with a variable part carries the REQUEST's own
/// `StartingPoint` at whole-second UTC (`…T…Z`, no sub-second remainder) and
/// nothing else of the request — not the bus, not the subscriber name, not the
/// window the bus reports.
#[tokio::test]
async fn the_outside_window_refusal_names_only_the_requested_instant() {
    let harness = Harness::new();
    let bus = harness.active_bus("point-in-time-stamp-bus").await;
    let window = harness
        .retention_window_start("the window", &bus)
        .await
        .secs();

    // Two instants before the window, one of them carrying a sub-second
    // remainder the rendered stamp must drop.
    for (case, requested) in [
        ("a whole second", DateTime::from_secs(window - 86_400)),
        (
            "an instant with a sub-second remainder",
            DateTime::from_secs_f64((window - 7_200) as f64 + 0.75),
        ),
    ] {
        let name = "pit-stamp";
        let error = harness
            .create_subscriber(CreateSubscriberInput {
                name: name.to_string(),
                event_bus_arn: bus.clone(),
                invoke_configuration: invoke(),
                starting_position: Some(POINT_IN_TIME.to_string()),
                point_in_time_configuration: point(TIMESTAMP, Some(requested)),
                client_token: Some(format!("token-for-{case}")),
                ..Default::default()
            })
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: CreateSubscriber must be refused"));
        let message = match &error {
            Scaffolded::Modeled(CreateSubscriberErrorModeled::InvalidInputException(invalid)) => {
                invalid.message.clone().unwrap_or_default()
            }
            other => panic!("case {case:?}: expected InvalidInputException, got {other:?}"),
        };
        let stamp = stamp_of(requested.secs());
        assert_eq!(
            message,
            outside_window_message(&stamp),
            "case {case:?}: expected the requested instant at whole seconds, got {message:?}"
        );
        assert!(
            stamp.len() == 20 && stamp.ends_with('Z') && !stamp.contains('.'),
            "case {case:?}: the stamp must be whole-second UTC, got {stamp:?}"
        );
        for absent in [bus.as_str(), name] {
            assert!(
                !message.contains(absent),
                "case {case:?}: the message must not carry {absent:?}, got {message:?}"
            );
        }
    }
}

/// RULE, where a client reads it: on the wire the recorded shape of this refusal
/// is a 400 whose body names `InvalidInputException` and carries the fixed
/// wording.
#[tokio::test]
async fn the_wire_refuses_a_future_starting_point() {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(eventsv2::EmulatedService::new(Arc::clone(&engine)));

    let (status, body) = cbor_call::<CreateEventBus<InMemoryEngine>>(
        &router,
        &CreateEventBusInput {
            name: "wire-point-in-time-bus".to_string(),
            client_token: Some("wire-point-in-time-token".to_string()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"wire, create bus\": CreateEventBus must answer 200, got {status} with {body:?}"
    );
    let bus = eventsv2::state::service_state(&*engine)
        .expect("the in-memory engine always has this service's store")
        .lock()
        .event_buses
        .keys()
        .next()
        .cloned()
        .expect("case \"wire, create bus\": the create must have stored a bus");

    // The bus has to be ACTIVE before it takes a subscriber at all; poll for
    // the HORIZON control to be accepted, then send the refused one.
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, _) = cbor_call::<CreateSubscriber<InMemoryEngine>>(
            &router,
            &CreateSubscriberInput {
                name: "wire-horizon".to_string(),
                event_bus_arn: bus.clone(),
                invoke_configuration: invoke(),
                starting_position: Some(POINT_IN_TIME.to_string()),
                point_in_time_configuration: point(HORIZON, None),
                client_token: Some("wire-horizon-token".to_string()),
                ..Default::default()
            },
        )
        .await;
        if status == 200 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case \"wire, horizon control\": the create never succeeded, last status {status}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    let (status, body) = cbor_call::<CreateSubscriber<InMemoryEngine>>(
        &router,
        &CreateSubscriberInput {
            name: "wire-future".to_string(),
            event_bus_arn: bus.clone(),
            invoke_configuration: invoke(),
            starting_position: Some(POINT_IN_TIME.to_string()),
            point_in_time_configuration: point(
                TIMESTAMP,
                Some(DateTime::from_secs(epoch_seconds_now() + 3_600)),
            ),
            client_token: Some("wire-future-token".to_string()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"wire, a future point\": CreateSubscriber must answer 400, got {status}"
    );
    let rendered = String::from_utf8_lossy(&body).to_string();
    assert!(
        rendered.contains("InvalidInputException"),
        "case \"wire, a future point\": the body must name the error shape, got {rendered:?}"
    );
    assert!(
        rendered.contains(IN_FUTURE_MESSAGE),
        "case \"wire, a future point\": the body must carry {IN_FUTURE_MESSAGE:?}, got \
         {rendered:?}"
    );
    assert!(
        eventsv2::state::service_state(&*engine)
            .expect("the in-memory engine always has this service's store")
            .lock()
            .subscriber_on_bus_by_name(&bus, "wire-future")
            .is_none(),
        "case \"wire, a future point\": a refused create must mint nothing"
    );
}

/// `secs` as the refusal renders an instant: RFC 3339 at whole-second
/// precision closed by `Z`. Rendered by the timestamp crate rather than by the
/// service's own helper, so the suite states the form instead of restating the
/// implementation.
fn stamp_of(secs: i64) -> String {
    DateTime::from_secs(secs)
        .fmt(Format::DateTime)
        .expect("an instant of this era renders")
}

/// Encode `input` as this service's wire protocol, dispatch it through the
/// router, and hand back the HTTP status and the raw response body.
async fn cbor_call<O: Operation>(
    router: &MultiServiceRouter,
    input: &O::Input,
) -> (u16, bytes::Bytes) {
    let codec = CborCodec::default();
    let mut serializer = codec.create_serializer();
    serializer
        .write_struct(O::INPUT_SCHEMA, input as &dyn SerializableStruct)
        .unwrap_or_else(|e| panic!("{}: encoding the input must succeed, got {e:?}", O::NAME));
    let body = serializer.finish();

    let request = http::Request::builder()
        .method("POST")
        .uri(format!("/service/AWSEventsV2/operation/{}", O::NAME))
        .header("authorization", authorization())
        .header("content-type", "application/cbor")
        .header("smithy-protocol", "rpc-v2-cbor")
        .body(bytes::Bytes::from(body))
        .unwrap_or_else(|e| panic!("{}: the request must be well formed, got {e:?}", O::NAME));

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, response.into_body())
}
