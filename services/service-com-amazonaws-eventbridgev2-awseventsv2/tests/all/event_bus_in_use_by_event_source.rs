// SPDX-License-Identifier: CC0-1.0

//! What an event source attached to a bus prevents, and what it outlives.
//!
//! RULE: an event source forwards onto the bus it was created on until it is
//! deleted or revoked. While ANY source nobody has revoked is attached,
//! `DeleteEventBus` is refused with the modelled `ResourceInUseException` (409)
//! and leaves the bus exactly as it stands — still `ACTIVE` — and the very same
//! delete succeeds once every source has taken one of the two remedies that
//! refusal names ("Delete or revoke them and retry"). A REVOKED source is
//! detached from its bus: it releases the bus AND outlives it, staying readable
//! with `Revoked` true and the `EventBusArn` it was created on, until its own
//! `DeleteEventSource` removes it.
//!
//! The rows span that class rather than one sequence: they differ along how many
//! sources hold the bus (one, two, three), what each source carries (an AWS
//! service origin with and without a forwarding pattern, a partner origin, a
//! dead-letter queue), how each stops holding it (deleted, or revoked and left
//! in place), and which bus a source is attached to at all.
//! [`IN_USE_MESSAGE`] is asserted character for character, so a build answering
//! a differently scoped or differently worded refusal could not pass.
//!
//! Nothing here waits on the wall clock: every wait polls for the CONDITION it
//! needs — a bus becoming `ACTIVE`, a bus finishing its disappearance — under a
//! deadline far above the span the service takes.

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
use eventsv2::ops::create_event_source::CreateEventSource;
use eventsv2::ops::delete_event_bus::{DeleteEventBus, DeleteEventBusErrorModeled};
use eventsv2::ops::delete_event_source::DeleteEventSource;
use eventsv2::ops::describe_event_bus::DescribeEventBus;
use eventsv2::ops::describe_event_source::DescribeEventSource;
use eventsv2::ops::revoke_resource::RevokeResource;
use eventsv2::ops::types::{
    AwsServiceEventsSourceConfiguration, CreateEventBusInput, CreateEventSourceInput,
    DeleteEventBusInput, DeleteEventSourceInput, DescribeEventBusInput, DescribeEventSourceInput,
    DescribeEventSourceOutput, EventSourceConfiguration, OnFailureConfiguration,
    PartnerEventsSourceConfiguration, RevokeResourceInput,
};
use eventsv2::scaffolded::Scaffolded;

/// The region every request below is signed for; it must reach the minted ARNs.
const REGION: &str = "us-west-2";

/// The account every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The whole `Message` of the `ResourceInUseException` `DeleteEventBus` answers
/// while an event source nobody has revoked still holds the bus: the fixed
/// wording of that refusal, written out here rather than read from the service,
/// so this suite pins the text instead of restating whatever the service
/// carries.
const IN_USE_MESSAGE: &str = "The event bus cannot be deleted because it has one or more \
     event sources that are not revoked. Delete or revoke them and retry.";

/// How long a poll waits for a condition the emulator owes: far above the
/// lifecycle span a bus takes, so a loaded host has ample room.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long one pass of a poll waits before looking again.
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
        .uri("/service/AWSEventsV2/operation/DeleteEventBus")
        .header("authorization", authorization())
        .header("content-type", "application/cbor")
        .body(())
        .expect("the harness request is well formed")
        .into_parts();
    parts
}

/// An `AwsServiceEventsConfiguration`: the origin the recorded sequence uses,
/// whose `AwsServiceSource` pattern is `^aws\.[a-z0-9\-]+$`.
fn aws_service(
    service: &str,
    pattern: Option<&str>,
    dead_letter_queue: Option<&str>,
) -> EventSourceConfiguration {
    EventSourceConfiguration::AwsServiceEventsConfiguration(AwsServiceEventsSourceConfiguration {
        aws_service: service.to_string(),
        pattern: pattern.map(str::to_string),
        on_failure_configuration: dead_letter_queue.map(|arn| OnFailureConfiguration {
            arn: Some(arn.to_string()),
        }),
    })
}

/// A `PartnerEventsConfiguration`, the other origin an event source may carry.
fn partner(stream: &str) -> EventSourceConfiguration {
    EventSourceConfiguration::PartnerEventsConfiguration(PartnerEventsSourceConfiguration {
        partner_event_source_arn: format!(
            "arn:aws:events:{REGION}:{ACCOUNT}:event-source/aws.partner/vendor/{stream}"
        ),
        pattern: None,
        partner_bus_kms_key_identifier: None,
        on_failure_configuration: None,
    })
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

    /// A bus that has finished provisioning, which is the state a source can be
    /// created against. The wait polls for `ACTIVE` rather than for a duration.
    async fn active_bus(&self, case: &str, name: &str) -> String {
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
            .unwrap_or_else(|e| panic!("case {case:?}: CreateEventBus must succeed, got {e:?}"))
            .event_bus_arn
            .unwrap_or_else(|| panic!("case {case:?}: CreateEventBus must report an EventBusArn"));

        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            if self.bus_state(case, &arn).await.as_deref() == Some(active()) {
                return arn;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the bus never became {}",
                active()
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// The bus's `State` as a read reports it, or `None` once the bus is gone.
    async fn bus_state(&self, case: &str, arn: &str) -> Option<String> {
        DescribeEventBus::new(Arc::clone(&self.engine))
            .handle(
                DescribeEventBusInput {
                    event_bus_arn: arn.to_string(),
                },
                &self.ctx(),
            )
            .await
            .ok()
            .map(|described| {
                described.state.unwrap_or_else(|| {
                    panic!("case {case:?}: DescribeEventBus must report a State")
                })
            })
    }

    async fn create_source(
        &self,
        case: &str,
        name: &str,
        bus: &str,
        configuration: EventSourceConfiguration,
    ) -> String {
        CreateEventSource::new(Arc::clone(&self.engine))
            .handle(
                CreateEventSourceInput {
                    name: name.to_string(),
                    event_bus_arn: bus.to_string(),
                    configuration,
                    description: None,
                    tags: None,
                    client_token: Some(format!("token-for-{name}")),
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: CreateEventSource must succeed, got {e:?}"))
            .event_source_arn
            .unwrap_or_else(|| {
                panic!("case {case:?}: CreateEventSource must report an EventSourceArn")
            })
    }

    async fn describe_source(&self, arn: &str) -> Option<DescribeEventSourceOutput> {
        DescribeEventSource::new(Arc::clone(&self.engine))
            .handle(
                DescribeEventSourceInput {
                    event_source_arn: arn.to_string(),
                },
                &self.ctx(),
            )
            .await
            .ok()
    }

    async fn delete_source(&self, case: &str, arn: &str) {
        DeleteEventSource::new(Arc::clone(&self.engine))
            .handle(
                DeleteEventSourceInput {
                    event_source_arn: arn.to_string(),
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| {
                panic!("case {case:?}: DeleteEventSource must succeed, got {e:?}")
            });
    }

    /// Withdraw a resource, which is how a source becomes `Revoked`.
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

    /// `DeleteEventBus` as it answers.
    async fn delete_bus(
        &self,
        arn: &str,
    ) -> Result<
        eventsv2::ops::types::DeleteEventBusOutput,
        eventsv2::ops::delete_event_bus::DeleteEventBusError,
    > {
        DeleteEventBus::new(Arc::clone(&self.engine))
            .handle(
                DeleteEventBusInput {
                    event_bus_arn: arn.to_string(),
                },
                &self.ctx(),
            )
            .await
    }

    /// Assert the bus is in use: the delete is refused with the modelled error,
    /// its whole message, HTTP 409 — and the bus is left `ACTIVE`.
    async fn assert_bus_is_in_use(&self, case: &str, arn: &str) {
        let error = self
            .delete_bus(arn)
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: DeleteEventBus must be refused"));
        assert_eq!(
            error.shape_name(),
            "ResourceInUseException",
            "case {case:?}: expected ResourceInUseException, got {:?}",
            error.shape_name()
        );
        assert_eq!(
            error.http_status(),
            409,
            "case {case:?}: expected HTTP 409, got {}",
            error.http_status()
        );
        match &error {
            Scaffolded::Modeled(DeleteEventBusErrorModeled::ResourceInUseException(in_use)) => {
                assert_eq!(
                    in_use.message.as_deref(),
                    Some(IN_USE_MESSAGE),
                    "case {case:?}: expected {IN_USE_MESSAGE:?}, got {:?}",
                    in_use.message
                );
            }
            other => panic!("case {case:?}: expected ResourceInUseException, got {other:?}"),
        }
        let state = self.bus_state(case, arn).await;
        assert_eq!(
            state.as_deref(),
            Some(active()),
            "case {case:?}: a refused delete must leave the bus {}, got {state:?}",
            active()
        );
    }

    /// Poll until the bus has finished disappearing, which is the condition the
    /// survival assertions are made after.
    async fn await_bus_gone(&self, case: &str, arn: &str) {
        let deadline = Instant::now() + POLL_DEADLINE;
        while self.bus_state(case, arn).await.is_some() {
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the deleted bus never disappeared"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// `BusLifecycleState::ACTIVE` as the service words it on the wire.
fn active() -> &'static str {
    eventsv2::state::BusLifecycleState::Active.wire_value()
}

// ---------------------------------------------------------------------------
// RULE: a bus with an unrevoked event source is in use
// ---------------------------------------------------------------------------

/// How one source stops holding its bus — the two remedies the in-use refusal
/// names ([`IN_USE_MESSAGE`]: "Delete or revoke them and retry").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Release {
    /// `DeleteEventSource`: the source is removed.
    Delete,
    /// `RevokeResource`: the source stays stored and readable and forwards
    /// nothing, so it no longer holds the bus it was created on — which is why
    /// the refusal counts the sources "that are not revoked".
    Revoke,
}

/// One bus, the event sources attached to it, and how each stops holding it.
struct Row {
    case: &'static str,
    bus: &'static str,
    /// One entry per source: its name, the origin it forwards from, and how it
    /// is released.
    sources: Vec<(&'static str, EventSourceConfiguration, Release)>,
}

/// RULE: while any event source nobody has revoked is attached to a bus, the bus
/// is in use and `DeleteEventBus` is refused with `ResourceInUseException`
/// (409), leaving the bus `ACTIVE`; the same delete succeeds as soon as the last
/// such source is deleted or revoked, and a source that was revoked outlives the
/// bus until its own delete removes it.
#[tokio::test]
async fn an_event_bus_is_in_use_while_an_unrevoked_event_source_is_attached() {
    let harness = Harness::new();

    let rows = vec![
        Row {
            // The recorded shape of the sequence: one AWS-service source, no
            // pattern, released by the revoke that the refusal names.
            case: "one AWS-service source, revoked and left in place",
            bus: "in-use-ec2",
            sources: vec![(
                "hold-ec2",
                aws_service("aws.ec2", None, None),
                Release::Revoke,
            )],
        },
        Row {
            case: "one filtered source, deleted",
            bus: "in-use-filtered",
            sources: vec![(
                "hold-filtered",
                aws_service(
                    "aws.s3",
                    Some(r#"{"detail-type":["Object Created"]}"#),
                    None,
                ),
                Release::Delete,
            )],
        },
        Row {
            // Revoking ONE of two releases nothing while the other still holds
            // the bus, which is what scopes each remedy to the source it was
            // applied to.
            case: "two sources, one revoked and one deleted",
            bus: "in-use-two",
            sources: vec![
                (
                    "hold-two-a",
                    aws_service(
                        "aws.ec2",
                        None,
                        Some(&format!("arn:aws:sqs:{REGION}:{ACCOUNT}:in-use-dlq")),
                    ),
                    Release::Revoke,
                ),
                ("hold-two-b", partner("orders"), Release::Delete),
            ],
        },
        Row {
            case: "three sources, released one at a time",
            bus: "in-use-three",
            sources: vec![
                (
                    "hold-three-a",
                    aws_service("aws.ec2", None, None),
                    Release::Delete,
                ),
                ("hold-three-b", partner("shipments"), Release::Revoke),
                (
                    "hold-three-c",
                    aws_service("aws.dynamodb", None, None),
                    Release::Delete,
                ),
            ],
        },
    ];

    for row in rows {
        let bus = harness.active_bus(row.case, row.bus).await;
        let mut attached = Vec::new();
        for (name, configuration, release) in row.sources {
            let arn = harness
                .create_source(row.case, name, &bus, configuration)
                .await;
            attached.push((arn, release));
        }

        // Each pass releases ONE source, so every count from "all of them" down
        // to "one left" is measured as in use.
        for (index, (arn, release)) in attached.iter().enumerate() {
            let remaining = attached.len() - index;
            let case = format!("{}, {remaining} still attached", row.case);
            harness.assert_bus_is_in_use(&case, &bus).await;
            match release {
                Release::Delete => harness.delete_source(&case, arn).await,
                Release::Revoke => harness.revoke(&case, arn).await,
            }
            // A revoked source is still READABLE — revoking is not deleting.
            if *release == Release::Revoke {
                assert!(
                    harness.describe_source(arn).await.is_some(),
                    "case {case:?}: a revoked source must stay readable"
                );
            }
        }

        harness.delete_bus(&bus).await.unwrap_or_else(|e| {
            panic!(
                "case {:?}: DeleteEventBus must succeed once nothing holds the bus, got {e:?}",
                row.case
            )
        });
        harness.await_bus_gone(row.case, &bus).await;

        // The sources that were revoked OUTLIVE the bus: each still reads back
        // with Revoked true and the EventBusArn it was created on, and its own
        // delete is what removes it.
        for (arn, release) in &attached {
            let case = format!("{}, after the bus is gone", row.case);
            if *release != Release::Revoke {
                assert!(
                    harness.describe_source(arn).await.is_none(),
                    "case {case:?}: a deleted source must not come back"
                );
                continue;
            }
            let described = harness
                .describe_source(arn)
                .await
                .unwrap_or_else(|| panic!("case {case:?}: a revoked source must outlive its bus"));
            assert_eq!(
                described.revoked,
                Some(true),
                "case {case:?}: expected Revoked true, got {:?}",
                described.revoked
            );
            assert_eq!(
                described.event_bus_arn.as_deref(),
                Some(bus.as_str()),
                "case {case:?}: expected the EventBusArn it was created on, got {:?}",
                described.event_bus_arn
            );
            harness.delete_source(&case, arn).await;
            assert!(
                harness.describe_source(arn).await.is_none(),
                "case {case:?}: DeleteEventSource must remove the revoked source"
            );
        }
    }
}

/// RULE, the other way round: a source holds only the bus it was created on, so
/// another bus's delete is not refused — and a bus that never had a source is
/// deletable as it always was.
#[tokio::test]
async fn an_event_source_holds_only_the_bus_it_was_created_on() {
    let harness = Harness::new();
    let held = harness.active_bus("the held bus", "scoped-held-bus").await;
    let free = harness.active_bus("the free bus", "scoped-free-bus").await;
    let untouched = harness
        .active_bus("a bus with no source", "scoped-empty-bus")
        .await;

    let source = harness
        .create_source(
            "the held bus",
            "scoped-holder",
            &held,
            aws_service("aws.ec2", None, None),
        )
        .await;

    harness.assert_bus_is_in_use("the held bus", &held).await;
    harness
        .delete_bus(&free)
        .await
        .expect("case \"the free bus\": a bus with no source of its own must be deletable");
    harness
        .delete_bus(&untouched)
        .await
        .expect("case \"a bus with no source\": a bus that never had one must be deletable");
    // The held bus is still held: deleting other buses released nothing.
    harness.assert_bus_is_in_use("the held bus", &held).await;

    harness.delete_source("the held bus", &source).await;
    harness
        .delete_bus(&held)
        .await
        .expect("case \"the held bus\": the delete must succeed once the source is gone");
}

// ---------------------------------------------------------------------------
// RULE again, where a client reads it: on the wire
// ---------------------------------------------------------------------------

/// RULE: a client speaking this service's wire protocol reads the same thing —
/// the in-use refusal as a 409 body carrying the error shape and the fixed
/// wording that interpolates nothing, a `RevokeResource` 200, and then a
/// `DeleteEventBus` 200.
#[tokio::test]
async fn the_wire_refuses_the_delete_until_the_event_source_is_revoked() {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(eventsv2::EmulatedService::new(Arc::clone(&engine)));

    let (status, body) = cbor_call::<CreateEventBus<InMemoryEngine>>(
        &router,
        &CreateEventBusInput {
            name: "wire-in-use-bus".to_string(),
            client_token: Some("wire-in-use-bus-token".to_string()),
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

    // The bus has to be ACTIVE before it takes a source; poll the condition.
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, _) = cbor_call::<CreateEventSource<InMemoryEngine>>(
            &router,
            &CreateEventSourceInput {
                name: "wire-holder".to_string(),
                event_bus_arn: bus.clone(),
                configuration: aws_service("aws.ec2", None, None),
                description: None,
                tags: None,
                client_token: Some("wire-holder-token".to_string()),
            },
        )
        .await;
        if status == 200 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case \"wire, create source\": the create never succeeded, last status {status}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let source = eventsv2::state::service_state(&*engine)
        .expect("the in-memory engine always has this service's store")
        .lock()
        .event_sources
        .keys()
        .next()
        .cloned()
        .expect("case \"wire, create source\": the create must have stored a source");

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
    for leaked in [bus.as_str(), source.as_str(), "wire-holder"] {
        assert!(
            !rendered.contains(leaked),
            "case \"wire, in use\": the message interpolates nothing, but {rendered:?} \
             carries {leaked:?}"
        );
    }

    let (status, body) = cbor_call::<RevokeResource<InMemoryEngine>>(
        &router,
        &RevokeResourceInput { arn: source.clone() },
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"wire, revoke\": RevokeResource must answer 200, got {status} with {body:?}"
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
        "case \"wire, delete bus\": the same delete must answer 200 once the source is \
         revoked, got {status} with {body:?}"
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
