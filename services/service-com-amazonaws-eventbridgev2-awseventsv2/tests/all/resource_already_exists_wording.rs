// SPDX-License-Identifier: CC0-1.0

//! The wording of a name-collision refusal, which a client reads character by
//! character.
//!
//! RULE: the `Message` of a `ResourceAlreadyExistsException` is the fixed text
//! of that error class — it names neither the colliding name, the resource that
//! already holds it, nor anything else of the request or of the store — wherever
//! this service refuses a name already taken (`CreateEventBus`,
//! `CreateEventSource`, `CreateSubscriber`), and the resource that holds the
//! name is left exactly as it was.
//!
//! The rows span that class rather than one recorded case: the three resource
//! kinds this service mints under a name, and what else the colliding request
//! varies — a fresh client token (so it is a collision and not an idempotent
//! replay), a different description, a different configuration.
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

use eventsv2::ops::create_event_bus::{CreateEventBus, CreateEventBusErrorModeled};
use eventsv2::ops::create_event_source::{CreateEventSource, CreateEventSourceErrorModeled};
use eventsv2::ops::create_subscriber::{CreateSubscriber, CreateSubscriberErrorModeled};
use eventsv2::ops::describe_event_bus::DescribeEventBus;
use eventsv2::ops::describe_event_source::DescribeEventSource;
use eventsv2::ops::describe_subscriber::DescribeSubscriber;
use eventsv2::ops::types::{
    AwsServiceEventsSourceConfiguration, CreateEventBusInput, CreateEventSourceInput,
    CreateSubscriberInput, DescribeEventBusInput, DescribeEventSourceInput,
    DescribeSubscriberInput, EventSourceConfiguration, InvokeConfiguration, StorageConfiguration,
};
use eventsv2::scaffolded::Scaffolded;

use crate::modelled_input_refusal::with_client_token;

/// The region every request below is signed for; it must reach the minted ARNs.
const REGION: &str = "us-west-2";

/// The role a subscriber's `InvokeConfiguration` names, of the modelled
/// `RoleArn` shape.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/event-delivery";

/// A queue ARN a subscriber targets. Nothing is delivered to it here — a
/// `LATEST` subscriber starts at the end of an empty log — so it only has to be
/// a well-formed target.
const TARGET_ARN: &str = "arn:aws:sqs:us-west-2:000000000000:collision-target";

/// The whole `Message` of a `ResourceAlreadyExistsException`: the fixed text of
/// that error class, written out here rather than read from the service, so this
/// suite pins the wording instead of restating whatever the service carries.
const COLLISION_MESSAGE: &str = "Resource already exists";

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
        .uri("/service/AWSEventsV2/operation/CreateEventBus")
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

    /// A bus called `name`, polled until it REPORTS `ACTIVE` — the state the
    /// operations below require of it. Polls the condition rather than sleeping
    /// a fixed time, because provisioning is asynchronous.
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
            .unwrap_or_else(|| panic!("case {case:?}: CreateEventBus must report an EventBusArn"));

        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let reported = self.describe_bus(case, &arn).await;
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
    async fn describe_bus(&self, case: &str, arn: &str) -> Option<String> {
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
}

/// A `CreateEventSource` request for an `AwsServiceEventsConfiguration` source.
/// Its input has no `Default` (the configuration union has no default member),
/// so every member is named here once.
fn source_input(
    name: &str,
    event_bus_arn: &str,
    aws_service: &str,
    description: Option<&str>,
    client_token: Option<&str>,
) -> CreateEventSourceInput {
    CreateEventSourceInput {
        name: name.to_string(),
        event_bus_arn: event_bus_arn.to_string(),
        configuration: EventSourceConfiguration::AwsServiceEventsConfiguration(
            AwsServiceEventsSourceConfiguration {
                aws_service: aws_service.to_string(),
                ..Default::default()
            },
        ),
        description: description.map(str::to_string),
        tags: None,
        client_token: client_token.map(str::to_string),
    }
}

/// What one refused create came to: the row it belongs to, the wire error code
/// and the whole `Message`.
type Refusal = (&'static str, String, String);

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// RULE: a second create of a name already held is refused as
/// `ResourceAlreadyExistsException` (409) whose whole `Message` is the fixed
/// text of that class, for EVERY named resource this service mints, and the
/// resource that holds the name is untouched.
#[tokio::test]
async fn a_name_collision_is_refused_with_the_fixed_wording_of_its_class() {
    let harness = Harness::new();
    let bus = harness.active_bus("collisions", "collision-host-bus").await;
    let mut refusals: Vec<Refusal> = Vec::new();

    // --- event bus ---------------------------------------------------------
    let taken_bus = harness.active_bus("event bus", "collision-taken-bus").await;
    for (case, input) in [
        (
            "event bus, a fresh client token",
            CreateEventBusInput {
                name: "collision-taken-bus".to_string(),
                client_token: Some("a-second-token".to_string()),
                ..Default::default()
            },
        ),
        (
            "event bus, a different description",
            CreateEventBusInput {
                name: "collision-taken-bus".to_string(),
                description: Some("another bus entirely".to_string()),
                ..Default::default()
            },
        ),
        (
            "event bus, a different storage configuration",
            CreateEventBusInput {
                name: "collision-taken-bus".to_string(),
                storage_configuration: Some(StorageConfiguration {
                    retention_period_in_days: Some(30),
                }),
                ..Default::default()
            },
        ),
    ] {
        let error = CreateEventBus::new(Arc::clone(&harness.engine))
            .handle(with_client_token(input), &harness.ctx())
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: the second create must be refused"));
        assert_eq!(
            error.http_status(),
            409,
            "case {case:?}: expected HTTP 409, got {}",
            error.http_status()
        );
        let shape = error.shape_name().to_string();
        let message = match error {
            Scaffolded::Modeled(CreateEventBusErrorModeled::ResourceAlreadyExistsException(
                already,
            )) => already.message.clone().unwrap_or_default(),
            other => panic!("case {case:?}: expected ResourceAlreadyExistsException, got {other:?}"),
        };
        refusals.push((case, shape, message));
    }
    // The bus that held the name is exactly as it was.
    let state = harness.describe_bus("event bus", &taken_bus).await;
    assert_eq!(
        state.as_deref(),
        Some("ACTIVE"),
        "case \"event bus\": the bus that held the name must survive the collision ACTIVE, \
         got {state:?}"
    );

    // --- event source ------------------------------------------------------
    let taken_source = CreateEventSource::new(Arc::clone(&harness.engine))
        .handle(
            with_client_token(source_input(
                "collision-taken-source",
                &bus,
                "aws.s3",
                None,
                None,
            )),
            &harness.ctx(),
        )
        .await
        .expect("case \"event source\": the first create must succeed")
        .event_source_arn
        .expect("case \"event source\": the create must report an EventSourceArn");
    for (case, input) in [
        (
            "event source, a fresh client token",
            source_input(
                "collision-taken-source",
                &bus,
                "aws.s3",
                None,
                Some("a-second-token"),
            ),
        ),
        (
            "event source, a different configuration",
            source_input(
                "collision-taken-source",
                &bus,
                "aws.ec2",
                Some("another source entirely"),
                None,
            ),
        ),
    ] {
        let error = CreateEventSource::new(Arc::clone(&harness.engine))
            .handle(with_client_token(input), &harness.ctx())
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: the second create must be refused"));
        assert_eq!(
            error.http_status(),
            409,
            "case {case:?}: expected HTTP 409, got {}",
            error.http_status()
        );
        let shape = error.shape_name().to_string();
        let message = match error {
            Scaffolded::Modeled(CreateEventSourceErrorModeled::ResourceAlreadyExistsException(
                already,
            )) => already.message.clone().unwrap_or_default(),
            other => panic!("case {case:?}: expected ResourceAlreadyExistsException, got {other:?}"),
        };
        refusals.push((case, shape, message));
    }
    let described = DescribeEventSource::new(Arc::clone(&harness.engine))
        .handle(
            DescribeEventSourceInput {
                event_source_arn: taken_source.clone(),
            },
            &harness.ctx(),
        )
        .await
        .expect("case \"event source\": DescribeEventSource must succeed");
    assert_eq!(
        described.event_source_arn.as_deref(),
        Some(taken_source.as_str()),
        "case \"event source\": the source that held the name must survive the collision, \
         got {:?}",
        described.event_source_arn
    );

    // --- subscriber --------------------------------------------------------
    let invoke = || InvokeConfiguration {
        role_arn: ROLE_ARN.to_string(),
        target_arn: TARGET_ARN.to_string(),
        ..Default::default()
    };
    let taken_subscriber = CreateSubscriber::new(Arc::clone(&harness.engine))
        .handle(
            with_client_token(CreateSubscriberInput {
                name: "collision-taken-subscriber".to_string(),
                event_bus_arn: bus.clone(),
                invoke_configuration: invoke(),
                ..Default::default()
            }),
            &harness.ctx(),
        )
        .await
        .expect("case \"subscriber\": the first create must succeed")
        .subscriber_arn
        .expect("case \"subscriber\": the create must report a SubscriberArn");
    for (case, input) in [
        (
            "subscriber, a fresh client token",
            CreateSubscriberInput {
                name: "collision-taken-subscriber".to_string(),
                event_bus_arn: bus.clone(),
                invoke_configuration: invoke(),
                client_token: Some("a-second-token".to_string()),
                ..Default::default()
            },
        ),
        (
            "subscriber, a different description",
            CreateSubscriberInput {
                name: "collision-taken-subscriber".to_string(),
                event_bus_arn: bus.clone(),
                invoke_configuration: invoke(),
                description: Some("another subscriber entirely".to_string()),
                ..Default::default()
            },
        ),
    ] {
        let error = CreateSubscriber::new(Arc::clone(&harness.engine))
            .handle(with_client_token(input), &harness.ctx())
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: the second create must be refused"));
        assert_eq!(
            error.http_status(),
            409,
            "case {case:?}: expected HTTP 409, got {}",
            error.http_status()
        );
        let shape = error.shape_name().to_string();
        let message = match error {
            Scaffolded::Modeled(CreateSubscriberErrorModeled::ResourceAlreadyExistsException(
                already,
            )) => already.message.clone().unwrap_or_default(),
            other => panic!("case {case:?}: expected ResourceAlreadyExistsException, got {other:?}"),
        };
        refusals.push((case, shape, message));
    }
    let described = DescribeSubscriber::new(Arc::clone(&harness.engine))
        .handle(
            DescribeSubscriberInput {
                subscriber_arn: taken_subscriber.clone(),
            },
            &harness.ctx(),
        )
        .await
        .expect("case \"subscriber\": DescribeSubscriber must succeed");
    assert_eq!(
        described.subscriber_arn.as_deref(),
        Some(taken_subscriber.as_str()),
        "case \"subscriber\": the subscriber that held the name must survive the collision, \
         got {:?}",
        described.subscriber_arn
    );

    // --- the wording, one row at a time ------------------------------------
    assert_eq!(
        refusals.len(),
        7,
        "every row must have produced a refusal, got {refusals:?}"
    );
    for (case, shape, message) in &refusals {
        assert_eq!(
            shape, "ResourceAlreadyExistsException",
            "case {case:?}: expected ResourceAlreadyExistsException, got {shape:?}"
        );
        assert_eq!(
            message, COLLISION_MESSAGE,
            "case {case:?}: expected the fixed message {COLLISION_MESSAGE:?}, got {message:?}"
        );
        // The same requirement said the other way round: nothing of the request
        // or of the store is interpolated, so no name and no ARN the refusal
        // could have reached for appears in it.
        for leaked in [
            "collision-taken-bus",
            "collision-taken-source",
            "collision-taken-subscriber",
            "arn:aws:",
            "a-second-token",
        ] {
            assert!(
                !message.contains(leaked),
                "case {case:?}: the message must interpolate nothing, but {message:?} \
                 carries {leaked:?}"
            );
        }
    }
}

/// RULE again, where a client reads it: on the wire the 409 body carries the
/// fixed text and nothing of the request.
#[tokio::test]
async fn the_collision_wording_is_what_the_wire_body_carries() {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(eventsv2::EmulatedService::new(Arc::clone(&engine)));

    let name = "wire-collision-bus";
    let (status, body) = cbor_call::<CreateEventBus<InMemoryEngine>>(
        &router,
        &CreateEventBusInput {
            name: name.to_string(),
            // The create's `@idempotencyToken`, which the model leaves optional.
            client_token: Some("a-first-token".to_string()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"wire, first create\": CreateEventBus must answer 200, got {status}"
    );
    assert!(
        !body.is_empty(),
        "case \"wire, first create\": the create must answer a body"
    );

    let (status, body) = cbor_call::<CreateEventBus<InMemoryEngine>>(
        &router,
        &CreateEventBusInput {
            name: name.to_string(),
            client_token: Some("a-second-token".to_string()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        status, 409,
        "case \"wire, collision\": the second create must answer 409, got {status} with \
         body {body:?}"
    );
    let rendered = String::from_utf8_lossy(&body).to_string();
    assert!(
        rendered.contains(COLLISION_MESSAGE),
        "case \"wire, collision\": the body must carry the fixed message \
         {COLLISION_MESSAGE:?}, got {rendered:?}"
    );
    assert!(
        rendered.contains("ResourceAlreadyExistsException"),
        "case \"wire, collision\": the body must name the error shape, got {rendered:?}"
    );
    for leaked in [name, "arn:aws:", "a-second-token"] {
        assert!(
            !rendered.contains(leaked),
            "case \"wire, collision\": the body must interpolate nothing, but {rendered:?} \
             carries {leaked:?}"
        );
    }
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
