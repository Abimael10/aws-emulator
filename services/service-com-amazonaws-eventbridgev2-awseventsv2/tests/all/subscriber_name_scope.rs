// SPDX-License-Identifier: CC0-1.0

//! The scope a subscriber's name is taken in.
//!
//! RULE: a subscriber is created on one event bus and enumerated under it, so
//! the bus is the scope its name is taken in — a second create of a name already
//! held ON THAT BUS is refused `ResourceAlreadyExistsException` (409), while the
//! same name on ANOTHER bus is accepted and mints a distinct subscriber that
//! each bus's own reads report.
//!
//! The rows span that class rather than one recorded name: they differ along the
//! form and the size of the name (a plain one, one carrying each punctuation
//! character `SubscriberName` allows, a single character, and one at the
//! member's `@length` maximum of 256 — from
//! `smithy_query(query="shape",shape="SubscriberName")`), and the colliding
//! create differs in what else it carries, so the refusal is the name and
//! nothing else.
//!
//! Nothing here waits on the wall clock: the one wait is for a bus's own
//! provisioning transition, polled for the CONDITION (`ACTIVE`) under a generous
//! deadline.

use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::InMemoryEngine;
use smithy_server_runtime::{
    EmbeddedEmulator, MultiServiceRouter, Operation, OperationContext, OperationError,
};

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

use eventsv2::ops::create_event_bus::CreateEventBus;
use eventsv2::ops::create_subscriber::{CreateSubscriber, CreateSubscriberErrorModeled};
use eventsv2::ops::delete_subscriber::DeleteSubscriber;
use eventsv2::ops::describe_event_bus::DescribeEventBus;
use eventsv2::ops::describe_subscriber::DescribeSubscriber;
use eventsv2::ops::list_subscribers::ListSubscribers;
use eventsv2::ops::types::{
    CreateEventBusInput, CreateSubscriberInput, CreateSubscriberOutput, DeleteSubscriberInput,
    DescribeEventBusInput, DescribeSubscriberInput, InvokeConfiguration, ListSubscribersInput,
};
use eventsv2::scaffolded::Scaffolded;

use crate::modelled_input_refusal::with_client_token;

/// The region every request below is signed for; it must reach the minted ARNs.
const REGION: &str = "us-west-2";

/// The role a subscriber's `InvokeConfiguration` names, of the modelled
/// `RoleArn` shape.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/event-delivery";

/// The queue a subscriber targets. Nothing is published here — every subscriber
/// below starts at the end of an empty log — so it only has to be a well-formed
/// target.
const TARGET_ARN: &str = "arn:aws:sqs:us-west-2:000000000000:name-scope-target";

/// A second target, so a colliding create can differ in what it invokes.
const OTHER_TARGET_ARN: &str = "arn:aws:sqs:us-west-2:000000000000:name-scope-other-target";

/// The whole `Message` of a `ResourceAlreadyExistsException`: the fixed text of
/// that error class.
const COLLISION_MESSAGE: &str = "Resource already exists";

/// The maximum length of a `SubscriberName`, per
/// `smithy_query(query="shape",shape="SubscriberName")` (`@length 1..256`).
const SUBSCRIBER_NAME_MAX: usize = 256;

/// How long a poll waits for a condition the emulator owes: far above the
/// provisioning span a bus takes, so a loaded host has ample room.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long one pass of a poll sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// A page big enough for every subscriber this suite mints, inside
/// `MaxResults`'s modelled `@range 1..100`.
const WHOLE_PAGE: i32 = 100;

/// A SigV4 `Authorization` header naming the region and this service's signing
/// name, which is how a request names both.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{REGION}/events/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// The request metadata of a call signed for [`REGION`]: the credential scope is
/// where a request names the region it addresses.
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

    /// A bus called `name`, polled until it REPORTS `ACTIVE` — the state
    /// `CreateSubscriber` requires of it. Polls the condition rather than
    /// sleeping a fixed time, because provisioning is asynchronous.
    async fn active_bus(&self, name: &str) -> String {
        let arn = CreateEventBus::new(Arc::clone(&self.engine))
            .handle(
                with_client_token(CreateEventBusInput {
                    name: name.to_string(),
                    ..Default::default()
                }),
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("bus {name:?}: CreateEventBus must succeed, got {e:?}"))
            .event_bus_arn
            .unwrap_or_else(|| panic!("bus {name:?}: CreateEventBus reported no EventBusArn"));

        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let reported = DescribeEventBus::new(Arc::clone(&self.engine))
                .handle(
                    DescribeEventBusInput {
                        event_bus_arn: arn.clone(),
                    },
                    &self.ctx(),
                )
                .await
                .unwrap_or_else(|e| panic!("bus {name:?}: DescribeEventBus must succeed, got {e:?}"))
                .state;
            if reported.as_deref() == Some("ACTIVE") {
                return arn;
            }
            assert!(
                Instant::now() < deadline,
                "bus {name:?}: {arn} never reported ACTIVE, last reported {reported:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// `CreateSubscriber` with a FRESH idempotency token, so a repeat is a name
    /// collision rather than an idempotent replay.
    async fn create(
        &self,
        name: &str,
        event_bus_arn: &str,
        target_arn: &str,
        description: Option<&str>,
    ) -> Result<CreateSubscriberOutput, Scaffolded<CreateSubscriberErrorModeled>> {
        CreateSubscriber::new(Arc::clone(&self.engine))
            .handle(
                with_client_token(CreateSubscriberInput {
                    name: name.to_string(),
                    event_bus_arn: event_bus_arn.to_string(),
                    invoke_configuration: InvokeConfiguration {
                        role_arn: ROLE_ARN.to_string(),
                        target_arn: target_arn.to_string(),
                        ..Default::default()
                    },
                    description: description.map(str::to_string),
                    ..Default::default()
                }),
                &self.ctx(),
            )
            .await
    }

    /// The `EventBusArn` `DescribeSubscriber` reports for `arn`.
    async fn bus_of(&self, case: &str, arn: &str) -> Option<String> {
        DescribeSubscriber::new(Arc::clone(&self.engine))
            .handle(
                DescribeSubscriberInput {
                    subscriber_arn: arn.to_string(),
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| {
                panic!("case {case:?}: DescribeSubscriber({arn}) must succeed, got {e:?}")
            })
            .event_bus_arn
    }

    /// The subscriber ARNs `ListSubscribers` reports on `event_bus_arn` under
    /// `name_prefix`.
    async fn listed(&self, case: &str, event_bus_arn: &str, name_prefix: &str) -> Vec<String> {
        ListSubscribers::new(Arc::clone(&self.engine))
            .handle(
                ListSubscribersInput {
                    event_bus_arn: Some(event_bus_arn.to_string()),
                    name_prefix: Some(name_prefix.to_string()),
                    max_results: WHOLE_PAGE,
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: ListSubscribers must succeed, got {e:?}"))
            .subscribers
            .unwrap_or_default()
            .into_iter()
            .map(|summary| summary.subscriber_arn.unwrap_or_default())
            .collect()
    }
}

/// One name the rule is exercised over, and what the colliding create on the
/// same bus differs by.
struct Row {
    case: &'static str,
    name: String,
    /// The target the colliding create names, so a collision is refused however
    /// else the request differs.
    colliding_target: &'static str,
    /// The description the colliding create carries, same reason.
    colliding_description: Option<&'static str>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// RULE: the bus is the scope a subscriber name is taken in — the same name is
/// refused on the bus that holds it and accepted on another.
#[tokio::test]
async fn a_subscriber_name_is_taken_on_its_own_bus_only() {
    let harness = Harness::new();
    let first_bus = harness.active_bus("name-scope-bus-one").await;
    let second_bus = harness.active_bus("name-scope-bus-two").await;
    let third_bus = harness.active_bus("name-scope-bus-three").await;

    let rows = vec![
        Row {
            case: "a plain name",
            name: "orders".to_string(),
            colliding_target: TARGET_ARN,
            colliding_description: None,
        },
        Row {
            // Every punctuation character the member's pattern allows after the
            // first, alphanumeric one.
            case: "a name carrying each allowed punctuation",
            name: "orders.ship-v1_final".to_string(),
            colliding_target: OTHER_TARGET_ARN,
            colliding_description: Some("a colliding create that differs in every other way"),
        },
        Row {
            case: "a one-character name",
            name: "s".to_string(),
            colliding_target: OTHER_TARGET_ARN,
            colliding_description: None,
        },
        Row {
            case: "a name at the length maximum",
            name: format!("n{}", "a".repeat(SUBSCRIBER_NAME_MAX - 1)),
            colliding_target: TARGET_ARN,
            colliding_description: Some("another subscriber entirely"),
        },
    ];

    for row in &rows {
        let case = row.case;

        // The name is taken on the first bus.
        let first = harness
            .create(&row.name, &first_bus, TARGET_ARN, None)
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: the first create must succeed, got {e:?}"))
            .subscriber_arn
            .unwrap_or_else(|| panic!("case {case:?}: the create reported no SubscriberArn"));

        // On that bus it is taken, whatever else the request says.
        let error = harness
            .create(
                &row.name,
                &first_bus,
                row.colliding_target,
                row.colliding_description,
            )
            .await
            .err()
            .unwrap_or_else(|| {
                panic!("case {case:?}: a second create on the same bus must be refused")
            });
        assert_eq!(
            error.shape_name(),
            "ResourceAlreadyExistsException",
            "case {case:?}: expected ResourceAlreadyExistsException, got {:?}",
            error.shape_name()
        );
        assert_eq!(
            error.http_status(),
            409,
            "case {case:?}: expected HTTP 409, got {}",
            error.http_status()
        );
        match &error {
            Scaffolded::Modeled(CreateSubscriberErrorModeled::ResourceAlreadyExistsException(
                already,
            )) => assert_eq!(
                already.message.as_deref(),
                Some(COLLISION_MESSAGE),
                "case {case:?}: expected the fixed message {COLLISION_MESSAGE:?}, got {:?}",
                already.message
            ),
            other => panic!("case {case:?}: expected ResourceAlreadyExistsException, got {other:?}"),
        }

        // On another bus the same name names another subscriber — on two other
        // buses, so acceptance is not one bus's accident.
        let second = harness
            .create(&row.name, &second_bus, TARGET_ARN, None)
            .await
            .unwrap_or_else(|e| {
                panic!("case {case:?}: the same name on another bus must be accepted, got {e:?}")
            })
            .subscriber_arn
            .unwrap_or_else(|| panic!("case {case:?}: the create reported no SubscriberArn"));
        let third = harness
            .create(&row.name, &third_bus, OTHER_TARGET_ARN, None)
            .await
            .unwrap_or_else(|e| {
                panic!("case {case:?}: the same name on a third bus must be accepted, got {e:?}")
            })
            .subscriber_arn
            .unwrap_or_else(|| panic!("case {case:?}: the create reported no SubscriberArn"));
        assert_ne!(
            first, second,
            "case {case:?}: two buses' subscribers of one name must be distinct, got {first}"
        );
        assert_ne!(
            second, third,
            "case {case:?}: two buses' subscribers of one name must be distinct, got {second}"
        );

        // Each one is read back on the bus it was created on, and each bus's
        // enumeration holds exactly its own.
        for (bus, subscriber) in [
            (&first_bus, &first),
            (&second_bus, &second),
            (&third_bus, &third),
        ] {
            let reported = harness.bus_of(case, subscriber).await;
            assert_eq!(
                reported.as_deref(),
                Some(bus.as_str()),
                "case {case:?}: {subscriber} must report the bus it was created on, got \
                 {reported:?}"
            );
            let listed = harness.listed(case, bus, &row.name).await;
            assert_eq!(
                listed,
                vec![subscriber.clone()],
                "case {case:?}: the bus {bus} must hold exactly its own subscriber of that \
                 name, got {listed:?}"
            );
        }
    }
}

/// RULE, the other side of "taken": a name held on a bus is free again once the
/// subscriber holding it is deleted, and the delete frees it on that bus only.
#[tokio::test]
async fn deleting_a_subscriber_frees_its_name_on_its_bus() {
    let harness = Harness::new();
    let first_bus = harness.active_bus("name-freed-bus-one").await;
    let second_bus = harness.active_bus("name-freed-bus-two").await;
    let name = "recycled";

    let first = harness
        .create(name, &first_bus, TARGET_ARN, None)
        .await
        .expect("case \"the first create\": CreateSubscriber must succeed")
        .subscriber_arn
        .expect("case \"the first create\": the create must report a SubscriberArn");
    let other = harness
        .create(name, &second_bus, TARGET_ARN, None)
        .await
        .expect("case \"the other bus\": the same name must be accepted there")
        .subscriber_arn
        .expect("case \"the other bus\": the create must report a SubscriberArn");

    DeleteSubscriber::new(Arc::clone(&harness.engine))
        .handle(
            DeleteSubscriberInput {
                subscriber_arn: first.clone(),
            },
            &harness.ctx(),
        )
        .await
        .expect("case \"the delete\": DeleteSubscriber must succeed");

    let again = harness
        .create(name, &first_bus, OTHER_TARGET_ARN, None)
        .await
        .expect("case \"after the delete\": the freed name must be accepted again")
        .subscriber_arn
        .expect("case \"after the delete\": the create must report a SubscriberArn");
    assert_ne!(
        again, first,
        "case \"after the delete\": the recreated subscriber must be a new one, got {again}"
    );

    // The other bus's subscriber of that name was never touched by either call.
    let reported = harness.bus_of("the other bus", &other).await;
    assert_eq!(
        reported.as_deref(),
        Some(second_bus.as_str()),
        "case \"the other bus\": {other} must still report its own bus, got {reported:?}"
    );
    let listed = harness.listed("after the delete", &first_bus, name).await;
    assert_eq!(
        listed,
        vec![again.clone()],
        "case \"after the delete\": the first bus must hold exactly the recreated subscriber, \
         got {listed:?}"
    );
}
