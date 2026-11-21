// SPDX-License-Identifier: CC0-1.0

//! Removing a subscriber, and what a subscriber that is still there prevents.
//!
//! RULE: a subscriber belongs to the bus it was created on until it is deleted
//! or revoked. `DeleteSubscriber` removes it, so the ARN resolves to nothing
//! afterwards and its name and client token are free again; and while ANY
//! subscriber nobody has revoked is still attached to a bus, `DeleteEventBus`
//! is refused with the modelled `ResourceInUseException` (409) and leaves the
//! bus exactly as it stands — the very same delete succeeds once nothing holds
//! the bus any more.
//!
//! Which subscribers hold the bus is the refusal's own wording: it says the bus
//! has subscribers "that are not revoked", and names the two remedies that
//! release it — "Delete or revoke them and retry". So a revoked subscriber
//! stops holding its bus while staying readable, and a merely STOPPED one does
//! not: `State` is caller-controlled and reversible. The rows below assert both
//! halves of that, and [`IN_USE_MESSAGE`] is asserted character for character,
//! so a build that answered a differently scoped refusal could not word it this
//! way.
//!
//! The rows span that class rather than one recorded sequence: they differ along
//! how many subscribers hold the bus (one, two, three), what carries them (the
//! run state they were created in, the filters and description they hold), how
//! each one stops holding it (deleted, or revoked and left in place), and which
//! bus a subscriber is attached to at all.
//!
//! Nothing here waits on the wall clock: the one wait is for a bus's own
//! provisioning transition, polled for the CONDITION (`ACTIVE`) under a generous
//! deadline.
//!
//! Nothing here waits on the wall clock: the one wait is for a bus's own
//! provisioning transition, polled for the CONDITION (`ACTIVE`) under a generous
//! deadline.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_smithy_schema::codec::{Codec, FinishSerializer};
use aws_smithy_schema::serde::{SerializableStruct, ShapeSerializer};
use aws_smithy_schema_server::CborCodec;
use engine::InMemoryEngine;
use smithy_server_runtime::{
    EmbeddedEmulator, MultiServiceRouter, Operation, OperationContext, OperationError,
};

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

use eventsv2::ops::create_event_bus::CreateEventBus;
use eventsv2::ops::create_subscriber::CreateSubscriber;
use eventsv2::ops::delete_event_bus::{DeleteEventBus, DeleteEventBusErrorModeled};
use eventsv2::ops::delete_subscriber::{DeleteSubscriber, DeleteSubscriberError};
use eventsv2::ops::describe_event_bus::DescribeEventBus;
use eventsv2::ops::describe_subscriber::DescribeSubscriber;
use eventsv2::ops::revoke_resource::RevokeResource;
use eventsv2::ops::types::{
    CreateEventBusInput, CreateSubscriberInput, DeleteEventBusInput, DeleteSubscriberInput,
    DescribeEventBusInput, DescribeSubscriberInput, Filter, FilterConfiguration,
    InvokeConfiguration, RevokeResourceInput,
};
use eventsv2::scaffolded::Scaffolded;

use crate::modelled_input_refusal::with_client_token;

/// The region every request below is signed for; it must reach the minted ARNs.
const REGION: &str = "us-west-2";

/// The account every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The role a subscriber's `InvokeConfiguration` names, of the modelled
/// `RoleArn` shape.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/event-delivery";

/// The queue a subscriber delivers to. Nothing is published below — every
/// subscriber here starts at the end of an empty log — so it only has to be a
/// well-formed target.
const TARGET_ARN: &str = "arn:aws:sqs:us-west-2:000000000000:removal-target";

/// The whole `Message` of the `ResourceInUseException` `DeleteEventBus` answers
/// while a subscriber nobody has revoked still holds the bus: the fixed wording
/// of that refusal, written out here rather than read from the service, so this
/// suite pins the text instead of restating whatever the service carries.
const IN_USE_MESSAGE: &str = "The event bus cannot be deleted because it has one or more \
     subscribers that are not revoked. Delete or revoke them and retry.";

/// `SubscriberState::STOPPED`, one of the two run states a create may name — a
/// subscriber that delivers nothing yet is still attached to its bus.
const RUN_STATE_STOPPED: &str = "STOPPED";

/// How long a poll waits for a condition the emulator owes: far above the
/// provisioning span a bus takes, so a loaded host has ample room.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long one pass of a poll sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

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
        .uri("/service/AWSEventsV2/operation/DeleteSubscriber")
        .header("authorization", authorization())
        .header("content-type", "application/cbor")
        .body(())
        .expect("the harness request is well formed")
        .into_parts();
    parts
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

    /// A bus called `name`, polled until it REPORTS `ACTIVE` — the state
    /// `CreateSubscriber` requires of it. Polls the condition rather than
    /// sleeping a fixed time, because provisioning is asynchronous.
    async fn active_bus(&self, case: &str, name: &str) -> String {
        let arn = CreateEventBus::new(Arc::clone(&self.engine))
            .handle(
                // Every create of this service states the idempotency token it requires;
                // the harness supplies a fresh one, as a code-generated client does
                // (`crate::modelled_input_refusal::with_client_token`).
                with_client_token(CreateEventBusInput {
                    name: name.to_string(),
                    ..Default::default()
                }),
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: CreateEventBus must succeed, got {e:?}"))
            .event_bus_arn
            .unwrap_or_else(|| panic!("case {case:?}: CreateEventBus reported no EventBusArn"));

        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let reported = self.bus_state(case, &arn).await;
            if reported.as_deref() == Some("ACTIVE") {
                return arn;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the bus {arn} never reported ACTIVE, last reported {reported:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// The `State` `DescribeEventBus` reports for `arn`.
    async fn bus_state(&self, case: &str, arn: &str) -> Option<String> {
        DescribeEventBus::new(Arc::clone(&self.engine))
            .handle(
                DescribeEventBusInput {
                    event_bus_arn: arn.to_string(),
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: DescribeEventBus must succeed, got {e:?}"))
            .state
    }

    /// Create a subscriber and hand back its `SubscriberArn`.
    ///
    /// A case that means a RETRY states its own token; every other create is given a fresh
    /// one, which is the member this service requires of a create.
    async fn create(&self, case: &str, input: CreateSubscriberInput) -> String {
        CreateSubscriber::new(Arc::clone(&self.engine))
            .handle(with_client_token(input), &self.ctx())
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: CreateSubscriber must succeed, got {e:?}"))
            .subscriber_arn
            .unwrap_or_else(|| panic!("case {case:?}: CreateSubscriber reported no SubscriberArn"))
    }

    /// `DeleteSubscriber` as the operation itself answers it.
    async fn delete_subscriber(&self, arn: &str) -> Result<(), DeleteSubscriberError> {
        DeleteSubscriber::new(Arc::clone(&self.engine))
            .handle(
                DeleteSubscriberInput {
                    subscriber_arn: arn.to_string(),
                },
                &self.ctx(),
            )
            .await
            .map(|_| ())
    }

    /// Whether `DescribeSubscriber` still resolves `arn`.
    async fn subscriber_is_readable(&self, arn: &str) -> bool {
        DescribeSubscriber::new(Arc::clone(&self.engine))
            .handle(
                DescribeSubscriberInput {
                    subscriber_arn: arn.to_string(),
                },
                &self.ctx(),
            )
            .await
            .is_ok()
    }

    /// `RevokeResource` against a subscriber, which leaves it stored.
    async fn revoke(&self, case: &str, arn: &str) {
        RevokeResource::new(Arc::clone(&self.engine))
            .handle(
                RevokeResourceInput {
                    arn: arn.to_string(),
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: RevokeResource must succeed, got {e:?}"));
    }

    /// Whether the store itself still holds a subscriber at `arn` — the
    /// assertion that is about what is STORED rather than about a response.
    fn engine_holds(&self, arn: &str) -> bool {
        eventsv2::state::service_state(&*self.engine)
            .expect("the in-memory engine always has this service's store")
            .lock()
            .subscriber(arn)
            .is_some()
    }

    /// `DeleteEventBus` as the operation itself answers it.
    async fn delete_bus(
        &self,
        arn: &str,
    ) -> Result<(), eventsv2::ops::delete_event_bus::DeleteEventBusError> {
        DeleteEventBus::new(Arc::clone(&self.engine))
            .handle(
                DeleteEventBusInput {
                    event_bus_arn: arn.to_string(),
                },
                &self.ctx(),
            )
            .await
            .map(|_| ())
    }

    /// A delete of `bus` that must be refused as in use, asserting the status,
    /// the error shape, the whole wording, and that the bus is untouched.
    async fn assert_bus_is_in_use(&self, case: &str, bus: &str) {
        let error = self
            .delete_bus(bus)
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: DeleteEventBus must be refused"));
        assert_eq!(
            error.http_status(),
            409,
            "case {case:?}: expected HTTP 409, got {}",
            error.http_status()
        );
        assert_eq!(
            error.shape_name(),
            "ResourceInUseException",
            "case {case:?}: expected ResourceInUseException, got {:?}",
            error.shape_name()
        );
        let message = match error {
            Scaffolded::Modeled(DeleteEventBusErrorModeled::ResourceInUseException(in_use)) => {
                in_use.message.clone().unwrap_or_default()
            }
            other => panic!("case {case:?}: expected ResourceInUseException, got {other:?}"),
        };
        assert_eq!(
            message, IN_USE_MESSAGE,
            "case {case:?}: expected the fixed message {IN_USE_MESSAGE:?}, got {message:?}"
        );
        // The refusal leaves the bus as it stands: a caller that removes what
        // holds it and retries is deleting the same bus, not a bus already on
        // its way out.
        let state = self.bus_state(case, bus).await;
        assert_eq!(
            state.as_deref(),
            Some("ACTIVE"),
            "case {case:?}: the refused delete must leave the bus ACTIVE, got {state:?}"
        );
    }
}

/// The `InvokeConfiguration` every subscriber below carries.
fn invoke() -> InvokeConfiguration {
    InvokeConfiguration {
        role_arn: ROLE_ARN.to_string(),
        target_arn: TARGET_ARN.to_string(),
        ..Default::default()
    }
}

/// A `FilterConfiguration` holding one pattern read against the event's payload,
/// so a row can carry a subscriber that selects rather than one that takes
/// everything.
fn data_filter() -> FilterConfiguration {
    FilterConfiguration {
        language: None,
        filters: Some(vec![Filter {
            pattern: r#"{"status":["shipped"]}"#.to_string(),
            scope: "DATA".to_string(),
        }]),
    }
}

/// A well-formed `SubscriberArn` naming a subscriber this service never minted,
/// of the form the modelled pattern pins
/// (`…:subscriber/<name>/[a-z0-9]{25}`).
fn unminted_subscriber_arn(name: &str) -> String {
    format!("arn:aws:events:{REGION}:{ACCOUNT}:subscriber/{name}/aaaaaaaaaaaaaaaaaaaaaaaaa")
}

// ---------------------------------------------------------------------------
// RULE: a deleted subscriber resolves to nothing, and frees what it held
// ---------------------------------------------------------------------------

/// One row of the deletion table: the case it names and the create it deletes.
type DeletedRow = (&'static str, fn(&str) -> CreateSubscriberInput);

/// RULE: `DeleteSubscriber` removes the subscriber it addresses, whatever that
/// subscriber carries — so a later read of its ARN is refused as
/// `ResourceNotFoundException` (404), a repeated delete is refused the same way,
/// and the name and the client token it held are free for a new create.
#[tokio::test]
async fn deleting_a_subscriber_removes_it_from_every_read() {
    let harness = Harness::new();
    let bus = harness
        .active_bus("the bus every row is created on", "removal-bus")
        .await;

    // The rows differ along what the deleted subscriber carried: the least a
    // create can name, a selecting subscriber with a description and a token, and
    // one created STOPPED.
    let rows: Vec<DeletedRow> = vec![
        (
            "the least a create can name",
            |bus: &str| CreateSubscriberInput {
                name: "removal-plain".to_string(),
                event_bus_arn: bus.to_string(),
                invoke_configuration: invoke(),
                ..Default::default()
            },
        ),
        (
            "a filtered subscriber with a description and a token",
            |bus: &str| CreateSubscriberInput {
                name: "removal-filtered".to_string(),
                event_bus_arn: bus.to_string(),
                invoke_configuration: invoke(),
                description: Some("deleted while selecting".to_string()),
                filter_configuration: Some(data_filter()),
                client_token: Some("removal-filtered-token".to_string()),
                ..Default::default()
            },
        ),
        (
            "a subscriber created STOPPED",
            |bus: &str| CreateSubscriberInput {
                name: "removal-stopped".to_string(),
                event_bus_arn: bus.to_string(),
                invoke_configuration: invoke(),
                state: Some(RUN_STATE_STOPPED.to_string()),
                ..Default::default()
            },
        ),
    ];

    for (case, build) in rows {
        let input = build(&bus);
        let arn = harness.create(case, input.clone()).await;
        assert!(
            harness.subscriber_is_readable(&arn).await,
            "case {case:?}: the created subscriber {arn} must be readable before the delete"
        );

        harness
            .delete_subscriber(&arn)
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: DeleteSubscriber must succeed, got {e:?}"));

        assert!(
            !harness.subscriber_is_readable(&arn).await,
            "case {case:?}: DescribeSubscriber must no longer resolve {arn}"
        );
        assert!(
            !harness.engine_holds(&arn),
            "case {case:?}: the store must no longer hold {arn}"
        );

        // A delete of what is no longer there is the not-found refusal, never a
        // second success.
        let error = harness
            .delete_subscriber(&arn)
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: a repeated delete must be refused"));
        assert_eq!(
            error.http_status(),
            404,
            "case {case:?}: a repeated delete must answer HTTP 404, got {}",
            error.http_status()
        );
        assert_eq!(
            error.shape_name(),
            "ResourceNotFoundException",
            "case {case:?}: a repeated delete must answer ResourceNotFoundException, got {:?}",
            error.shape_name()
        );

        // What the deleted subscriber held is free again: the same name, and the
        // same client token, mint a NEW subscriber rather than replaying the one
        // that is gone.
        let reborn = harness.create(case, input).await;
        assert_ne!(
            reborn, arn,
            "case {case:?}: a create after the delete must mint a new subscriber, got {reborn}"
        );
        harness
            .delete_subscriber(&reborn)
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: DeleteSubscriber must succeed, got {e:?}"));
    }

    // An ARN this service never minted is the same refusal: nothing to delete.
    for (case, arn) in [
        ("an ARN nothing minted", unminted_subscriber_arn("absent")),
        (
            "an ARN naming a name that was used",
            unminted_subscriber_arn("removal-plain"),
        ),
    ] {
        let error = harness
            .delete_subscriber(&arn)
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: the delete must be refused"));
        assert_eq!(
            error.http_status(),
            404,
            "case {case:?}: expected HTTP 404, got {}",
            error.http_status()
        );
        assert_eq!(
            error.shape_name(),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {:?}",
            error.shape_name()
        );
    }

    // Every subscriber of the bus is gone, so nothing holds it any more.
    harness
        .delete_bus(&bus)
        .await
        .expect("the bus every row used must be deletable once no subscriber is left");
}

// ---------------------------------------------------------------------------
// RULE: a bus with an unrevoked subscriber is in use
// ---------------------------------------------------------------------------

/// How one subscriber stops holding its bus — the two remedies the in-use
/// refusal names ([`IN_USE_MESSAGE`]: "Delete or revoke them and retry").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Release {
    /// `DeleteSubscriber`: the subscriber is removed.
    Delete,
    /// `RevokeResource`: the subscriber stays stored and readable, and delivers
    /// nothing — so it no longer holds the bus it was created on, which is why
    /// the refusal counts the subscribers "that are not revoked".
    Revoke,
}

/// One bus, the subscribers attached to it, and how each stops holding it.
struct Row {
    case: &'static str,
    bus: &'static str,
    /// One entry per subscriber: its name, the run state it is created in, and
    /// how it is released.
    subscribers: Vec<(&'static str, Option<&'static str>, Release)>,
}

/// RULE: while any subscriber nobody has revoked is attached to a bus, the bus
/// is in use and `DeleteEventBus` is refused with `ResourceInUseException`
/// (409), leaving the bus ACTIVE; the same delete succeeds as soon as the last
/// such subscriber is deleted or revoked.
#[tokio::test]
async fn an_event_bus_is_in_use_while_an_unrevoked_subscriber_is_attached() {
    let harness = Harness::new();

    let rows = vec![
        Row {
            case: "one subscriber, deleted",
            bus: "in-use-one",
            subscribers: vec![("hold-one", None, Release::Delete)],
        },
        Row {
            case: "one STOPPED subscriber, deleted",
            bus: "in-use-stopped",
            subscribers: vec![("hold-stopped", Some(RUN_STATE_STOPPED), Release::Delete)],
        },
        Row {
            // A subscriber left in place after a revoke: the second of the two
            // remedies the refusal names, so the bus is released without the
            // subscriber being removed — asserted below to be still readable.
            case: "one subscriber, revoked and left in place",
            bus: "in-use-revoked",
            subscribers: vec![("hold-revoked", None, Release::Revoke)],
        },
        Row {
            // Revoking ONE of two releases nothing while the other still holds
            // the bus, which is what scopes the remedy to the subscriber it was
            // applied to.
            case: "two subscribers, one revoked and one deleted",
            bus: "in-use-two",
            subscribers: vec![
                ("hold-two-a", None, Release::Revoke),
                ("hold-two-b", Some(RUN_STATE_STOPPED), Release::Delete),
            ],
        },
        Row {
            case: "three subscribers, deleted one at a time",
            bus: "in-use-three",
            subscribers: vec![
                ("hold-three-a", None, Release::Delete),
                ("hold-three-b", None, Release::Delete),
                ("hold-three-c", None, Release::Delete),
            ],
        },
    ];

    for row in rows {
        let bus = harness.active_bus(row.case, row.bus).await;
        let mut attached = Vec::new();
        for (name, run_state, release) in &row.subscribers {
            let arn = harness
                .create(
                    row.case,
                    CreateSubscriberInput {
                        name: (*name).to_string(),
                        event_bus_arn: bus.clone(),
                        invoke_configuration: invoke(),
                        state: run_state.map(str::to_string),
                        ..Default::default()
                    },
                )
                .await;
            attached.push((arn, *release));
        }

        // Each pass releases ONE subscriber, so every count from "all of them"
        // down to "one left" is measured as in use.
        for (index, (arn, release)) in attached.iter().enumerate() {
            let remaining = attached.len() - index;
            let case = format!("{}, {remaining} still attached", row.case);
            harness.assert_bus_is_in_use(&case, &bus).await;
            match release {
                Release::Delete => harness.delete_subscriber(arn).await.unwrap_or_else(|e| {
                    panic!("case {case:?}: DeleteSubscriber must succeed, got {e:?}")
                }),
                Release::Revoke => harness.revoke(&case, arn).await,
            }
            // A revoked subscriber is still READABLE — revoking is not deleting.
            if *release == Release::Revoke {
                assert!(
                    harness.subscriber_is_readable(arn).await,
                    "case {case:?}: a revoked subscriber must stay readable"
                );
            }
        }

        harness.delete_bus(&bus).await.unwrap_or_else(|e| {
            panic!(
                "case {:?}: DeleteEventBus must succeed once nothing holds the bus, got {e:?}",
                row.case
            )
        });
    }
}

/// RULE, the other way round: a subscriber holds only the bus it was created on,
/// so another bus's delete is not refused — and a bus that never had a
/// subscriber is deletable as it always was.
#[tokio::test]
async fn a_subscriber_holds_only_the_bus_it_was_created_on() {
    let harness = Harness::new();
    let held = harness.active_bus("the held bus", "scoped-held-bus").await;
    let free = harness.active_bus("the free bus", "scoped-free-bus").await;
    let untouched = harness
        .active_bus("a bus with no subscriber", "scoped-empty-bus")
        .await;

    let subscriber = harness
        .create(
            "the held bus",
            CreateSubscriberInput {
                name: "scoped-holder".to_string(),
                event_bus_arn: held.clone(),
                invoke_configuration: invoke(),
                ..Default::default()
            },
        )
        .await;

    harness.assert_bus_is_in_use("the held bus", &held).await;
    harness
        .delete_bus(&free)
        .await
        .expect("case \"the free bus\": a bus with no subscriber of its own must be deletable");
    harness.delete_bus(&untouched).await.expect(
        "case \"a bus with no subscriber\": a bus that never had one must be deletable",
    );
    // The held bus is still held: deleting other buses released nothing.
    harness.assert_bus_is_in_use("the held bus", &held).await;

    harness
        .delete_subscriber(&subscriber)
        .await
        .expect("case \"the held bus\": DeleteSubscriber must succeed");
    harness
        .delete_bus(&held)
        .await
        .expect("case \"the held bus\": the delete must succeed once the subscriber is gone");
}

// ---------------------------------------------------------------------------
// RULE again, where a client reads it: on the wire
// ---------------------------------------------------------------------------

/// RULE: a client speaking this service's wire protocol reads the same thing —
/// the in-use refusal as a 409 body carrying the error shape and the fixed
/// wording, a `DeleteSubscriber` 200, and then a `DeleteEventBus` 200.
#[tokio::test]
async fn the_wire_refuses_the_delete_until_the_subscriber_is_removed() {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(eventsv2::EmulatedService::new(Arc::clone(&engine)));

    let (status, body) = cbor_call::<CreateEventBus<InMemoryEngine>>(
        &router,
        &CreateEventBusInput {
            name: "wire-removal-bus".to_string(),
            client_token: Some("wire-removal-bus-token".to_string()),
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

    // The bus has to be ACTIVE before it takes a subscriber; poll the condition.
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, _) = cbor_call::<CreateSubscriber<InMemoryEngine>>(
            &router,
            &CreateSubscriberInput {
                name: "wire-holder".to_string(),
                event_bus_arn: bus.clone(),
                invoke_configuration: invoke(),
                client_token: Some("wire-holder-token".to_string()),
                ..Default::default()
            },
        )
        .await;
        if status == 200 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case \"wire, create subscriber\": the create never succeeded, last status {status}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let subscriber = eventsv2::state::service_state(&*engine)
        .expect("the in-memory engine always has this service's store")
        .lock()
        .subscribers
        .keys()
        .next()
        .cloned()
        .expect("case \"wire, create subscriber\": the create must have stored a subscriber");

    let (status, body) = cbor_call::<DeleteEventBus<InMemoryEngine>>(
        &router,
        &DeleteEventBusInput {
            event_bus_arn: bus.clone(),
        },
    )
    .await;
    assert_eq!(
        status, 409,
        "case \"wire, in use\": DeleteEventBus must answer 409, got {status}"
    );
    let rendered = String::from_utf8_lossy(&body).to_string();
    assert!(
        rendered.contains("ResourceInUseException"),
        "case \"wire, in use\": the body must name the error shape, got {rendered:?}"
    );
    assert!(
        rendered.contains(IN_USE_MESSAGE),
        "case \"wire, in use\": the body must carry {IN_USE_MESSAGE:?}, got {rendered:?}"
    );
    for leaked in [bus.as_str(), subscriber.as_str(), "wire-holder"] {
        assert!(
            !rendered.contains(leaked),
            "case \"wire, in use\": the message interpolates nothing, but {rendered:?} \
             carries {leaked:?}"
        );
    }

    let (status, body) = cbor_call::<DeleteSubscriber<InMemoryEngine>>(
        &router,
        &DeleteSubscriberInput {
            subscriber_arn: subscriber.clone(),
        },
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"wire, delete subscriber\": DeleteSubscriber must answer 200, got {status} \
         with {body:?}"
    );

    let (status, body) = cbor_call::<DeleteEventBus<InMemoryEngine>>(
        &router,
        &DeleteEventBusInput {
            event_bus_arn: bus.clone(),
        },
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"wire, delete bus\": the same delete must answer 200 once the subscriber is \
         gone, got {status} with {body:?}"
    );
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
