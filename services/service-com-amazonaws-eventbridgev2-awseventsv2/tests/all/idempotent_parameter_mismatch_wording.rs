// SPDX-License-Identifier: CC0-1.0

//! What a create answers when the `ClientToken` it carries was already used
//! with other parameters, which a client reads character by character.
//!
//! RULE: the `Message` of an `IdempotentParameterMismatchException` is the fixed
//! text of that error class — it states the rule the caller broke and names
//! neither the token, nor the resource that token minted, nor anything else of
//! the request or of the store — wherever this service refuses a mismatched
//! replay (`CreateEventBus`, `CreateEventSource`, `CreateSubscriber`), and the
//! refused replay leaves the resource that token minted exactly as it was.
//!
//! The rows span that class rather than one recorded case: the three resources
//! this service mints under an idempotency token, and the single member a repeat
//! differs by — a name, a description that was not sent before, the
//! configuration of what the resource does.
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
use eventsv2::ops::describe_subscriber::DescribeSubscriber;
use eventsv2::ops::list_subscribers::ListSubscribers;
use eventsv2::ops::types::{
    AwsServiceEventsSourceConfiguration, CreateEventBusInput, CreateEventSourceInput,
    CreateSubscriberInput, DescribeEventBusInput, DescribeSubscriberInput, EventSourceConfiguration,
    InvokeConfiguration, ListSubscribersInput, StorageConfiguration,
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
const TARGET_ARN: &str = "arn:aws:sqs:us-west-2:000000000000:mismatch-target";

/// A second target, so a repeat can differ in what it invokes.
const OTHER_TARGET_ARN: &str = "arn:aws:sqs:us-west-2:000000000000:mismatch-other-target";

/// The whole `Message` of an `IdempotentParameterMismatchException`: the fixed
/// text of that error class, written out here rather than read from the service,
/// so this suite pins the wording instead of restating whatever the service
/// carries.
const MISMATCH_MESSAGE: &str =
    "ClientToken was used by a previous request with different parameters";

/// The error shape every refusal below must name.
const MISMATCH_SHAPE: &str = "IdempotentParameterMismatchException";

/// The token the first create of each resource carries, and that every repeat
/// below reuses: it is what makes a repeat a replay rather than a second
/// resource.
const BUS_TOKEN: &str = "mismatch-bus-token";

/// The token the first `CreateEventSource` carries.
const SOURCE_TOKEN: &str = "mismatch-source-token";

/// The token the first `CreateSubscriber` carries.
const SUBSCRIBER_TOKEN: &str = "mismatch-subscriber-token";

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

    /// A bus called `name` carrying `client_token`, polled until it REPORTS
    /// `ACTIVE` — the state the creates below require of it. Polls the condition
    /// rather than sleeping a fixed time, because provisioning is asynchronous.
    async fn active_bus(&self, case: &str, name: &str, client_token: Option<&str>) -> String {
        let arn = CreateEventBus::new(Arc::clone(&self.engine))
            .handle(
                with_client_token(CreateEventBusInput {
                    name: name.to_string(),
                    client_token: client_token.map(str::to_string),
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
            let reported = DescribeEventBus::new(Arc::clone(&self.engine))
                .handle(
                    DescribeEventBusInput {
                        event_bus_arn: arn.clone(),
                    },
                    &self.ctx(),
                )
                .await
                .unwrap_or_else(|e| {
                    panic!("case {case:?}: DescribeEventBus must succeed, got {e:?}")
                })
                .state;
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
}

/// A `CreateEventSourceInput` whose configuration a row can vary. Its input has
/// no `Default` (the configuration union has no default member), so every member
/// is named here once.
fn source_input(
    name: &str,
    event_bus_arn: &str,
    aws_service: &str,
    description: Option<&str>,
    client_token: &str,
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
        client_token: Some(client_token.to_string()),
    }
}

/// A `CreateSubscriberInput` on `event_bus_arn` that a row can vary.
fn subscriber_input(
    name: &str,
    event_bus_arn: &str,
    target_arn: &str,
    description: Option<&str>,
    client_token: &str,
) -> CreateSubscriberInput {
    CreateSubscriberInput {
        name: name.to_string(),
        event_bus_arn: event_bus_arn.to_string(),
        invoke_configuration: InvokeConfiguration {
            role_arn: ROLE_ARN.to_string(),
            target_arn: target_arn.to_string(),
            ..Default::default()
        },
        description: description.map(str::to_string),
        client_token: Some(client_token.to_string()),
        ..Default::default()
    }
}

/// What one refused replay came to: the row it belongs to, the wire error code
/// and the whole `Message`.
type Refusal = (&'static str, String, String);

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// RULE: a repeat carrying a token already used with other parameters is refused
/// as `IdempotentParameterMismatchException` (409) whose whole `Message` is the
/// fixed text of that class, for EVERY resource this service mints under a
/// token, and the resource that token minted is untouched.
#[tokio::test]
async fn a_mismatched_replay_is_refused_with_the_fixed_wording_of_its_class() {
    let harness = Harness::new();
    let host_bus = harness
        .active_bus("the host bus", "mismatch-host-bus", None)
        .await;
    let mut refusals: Vec<Refusal> = Vec::new();

    // --- event bus ---------------------------------------------------------
    let taken_bus = harness
        .active_bus("event bus", "mismatch-token-bus", Some(BUS_TOKEN))
        .await;
    for (case, input) in [
        (
            "event bus, another name",
            CreateEventBusInput {
                name: "mismatch-token-bus-renamed".to_string(),
                client_token: Some(BUS_TOKEN.to_string()),
                ..Default::default()
            },
        ),
        (
            "event bus, a description that was not sent before",
            CreateEventBusInput {
                name: "mismatch-token-bus".to_string(),
                description: Some("another bus entirely".to_string()),
                client_token: Some(BUS_TOKEN.to_string()),
                ..Default::default()
            },
        ),
        (
            "event bus, another storage configuration",
            CreateEventBusInput {
                name: "mismatch-token-bus".to_string(),
                storage_configuration: Some(StorageConfiguration {
                    retention_period_in_days: Some(30),
                }),
                client_token: Some(BUS_TOKEN.to_string()),
                ..Default::default()
            },
        ),
    ] {
        let error = CreateEventBus::new(Arc::clone(&harness.engine))
            .handle(input, &harness.ctx())
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: the mismatched replay must be refused"));
        assert_eq!(
            error.http_status(),
            409,
            "case {case:?}: expected HTTP 409, got {}",
            error.http_status()
        );
        let shape = error.shape_name().to_string();
        let message = match error {
            Scaffolded::Modeled(
                CreateEventBusErrorModeled::IdempotentParameterMismatchException(mismatch),
            ) => mismatch.message.clone().unwrap_or_default(),
            other => panic!("case {case:?}: expected {MISMATCH_SHAPE}, got {other:?}"),
        };
        refusals.push((case, shape, message));
    }
    // A byte-identical repeat is still the idempotent replay, and the bus the
    // token minted is exactly as it was.
    let replayed = harness
        .active_bus("event bus", "mismatch-token-bus", Some(BUS_TOKEN))
        .await;
    assert_eq!(
        replayed, taken_bus,
        "case \"event bus\": an identical repeat must replay the bus the token minted, got \
         {replayed} for {taken_bus}"
    );

    // --- event source ------------------------------------------------------
    let taken_source = CreateEventSource::new(Arc::clone(&harness.engine))
        .handle(
            source_input(
                "mismatch-token-source",
                &host_bus,
                "aws.s3",
                None,
                SOURCE_TOKEN,
            ),
            &harness.ctx(),
        )
        .await
        .expect("case \"event source\": the first create must succeed")
        .event_source_arn
        .expect("case \"event source\": the create must report an EventSourceArn");
    for (case, input) in [
        (
            "event source, another name",
            source_input(
                "mismatch-token-source-renamed",
                &host_bus,
                "aws.s3",
                None,
                SOURCE_TOKEN,
            ),
        ),
        (
            "event source, a description that was not sent before",
            source_input(
                "mismatch-token-source",
                &host_bus,
                "aws.s3",
                Some("another source entirely"),
                SOURCE_TOKEN,
            ),
        ),
        (
            "event source, another configuration",
            source_input(
                "mismatch-token-source",
                &host_bus,
                "aws.ec2",
                None,
                SOURCE_TOKEN,
            ),
        ),
    ] {
        let error = CreateEventSource::new(Arc::clone(&harness.engine))
            .handle(input, &harness.ctx())
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: the mismatched replay must be refused"));
        assert_eq!(
            error.http_status(),
            409,
            "case {case:?}: expected HTTP 409, got {}",
            error.http_status()
        );
        let shape = error.shape_name().to_string();
        let message = match error {
            Scaffolded::Modeled(
                CreateEventSourceErrorModeled::IdempotentParameterMismatchException(mismatch),
            ) => mismatch.message.clone().unwrap_or_default(),
            other => panic!("case {case:?}: expected {MISMATCH_SHAPE}, got {other:?}"),
        };
        refusals.push((case, shape, message));
    }
    let replayed = CreateEventSource::new(Arc::clone(&harness.engine))
        .handle(
            source_input(
                "mismatch-token-source",
                &host_bus,
                "aws.s3",
                None,
                SOURCE_TOKEN,
            ),
            &harness.ctx(),
        )
        .await
        .expect("case \"event source\": an identical repeat must replay")
        .event_source_arn;
    assert_eq!(
        replayed.as_deref(),
        Some(taken_source.as_str()),
        "case \"event source\": an identical repeat must replay the source the token minted, \
         got {replayed:?}"
    );

    // --- subscriber --------------------------------------------------------
    let taken_subscriber = CreateSubscriber::new(Arc::clone(&harness.engine))
        .handle(
            subscriber_input(
                "mismatch-token-subscriber",
                &host_bus,
                TARGET_ARN,
                None,
                SUBSCRIBER_TOKEN,
            ),
            &harness.ctx(),
        )
        .await
        .expect("case \"subscriber\": the first create must succeed")
        .subscriber_arn
        .expect("case \"subscriber\": the create must report a SubscriberArn");
    for (case, input) in [
        (
            "subscriber, another name",
            subscriber_input(
                "mismatch-token-subscriber-renamed",
                &host_bus,
                TARGET_ARN,
                None,
                SUBSCRIBER_TOKEN,
            ),
        ),
        (
            "subscriber, a description that was not sent before",
            subscriber_input(
                "mismatch-token-subscriber",
                &host_bus,
                TARGET_ARN,
                Some("another subscriber entirely"),
                SUBSCRIBER_TOKEN,
            ),
        ),
        (
            "subscriber, another target",
            subscriber_input(
                "mismatch-token-subscriber",
                &host_bus,
                OTHER_TARGET_ARN,
                None,
                SUBSCRIBER_TOKEN,
            ),
        ),
    ] {
        let error = CreateSubscriber::new(Arc::clone(&harness.engine))
            .handle(input, &harness.ctx())
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: the mismatched replay must be refused"));
        assert_eq!(
            error.http_status(),
            409,
            "case {case:?}: expected HTTP 409, got {}",
            error.http_status()
        );
        let shape = error.shape_name().to_string();
        let message = match error {
            Scaffolded::Modeled(
                CreateSubscriberErrorModeled::IdempotentParameterMismatchException(mismatch),
            ) => mismatch.message.clone().unwrap_or_default(),
            other => panic!("case {case:?}: expected {MISMATCH_SHAPE}, got {other:?}"),
        };
        refusals.push((case, shape, message));
    }
    let replayed = CreateSubscriber::new(Arc::clone(&harness.engine))
        .handle(
            subscriber_input(
                "mismatch-token-subscriber",
                &host_bus,
                TARGET_ARN,
                None,
                SUBSCRIBER_TOKEN,
            ),
            &harness.ctx(),
        )
        .await
        .expect("case \"subscriber\": an identical repeat must replay")
        .subscriber_arn;
    assert_eq!(
        replayed.as_deref(),
        Some(taken_subscriber.as_str()),
        "case \"subscriber\": an identical repeat must replay the subscriber the token minted, \
         got {replayed:?}"
    );

    // The refused replays changed nothing: the subscriber the token minted is
    // the only one on the bus, and it still carries no `Description` — the
    // member one refused replay asked for.
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
        described.description, None,
        "case \"subscriber\": a refused replay must not write the Description it asked for, \
         got {:?}",
        described.description
    );
    let listed = ListSubscribers::new(Arc::clone(&harness.engine))
        .handle(
            ListSubscribersInput {
                event_bus_arn: Some(host_bus.clone()),
                name_prefix: Some("mismatch-token-subscriber".to_string()),
                max_results: WHOLE_PAGE,
                ..Default::default()
            },
            &harness.ctx(),
        )
        .await
        .expect("case \"subscriber\": ListSubscribers must succeed")
        .subscribers
        .unwrap_or_default();
    let listed_arns: Vec<String> = listed
        .iter()
        .map(|summary| summary.subscriber_arn.clone().unwrap_or_default())
        .collect();
    assert_eq!(
        listed_arns,
        vec![taken_subscriber.clone()],
        "case \"subscriber\": the refused replays must have minted nothing, got {listed_arns:?}"
    );

    // --- the wording, one row at a time ------------------------------------
    assert_eq!(
        refusals.len(),
        9,
        "every row must have produced a refusal, got {refusals:?}"
    );
    for (case, shape, message) in &refusals {
        assert_eq!(
            shape, MISMATCH_SHAPE,
            "case {case:?}: expected {MISMATCH_SHAPE}, got {shape:?}"
        );
        assert_eq!(
            message, MISMATCH_MESSAGE,
            "case {case:?}: expected the fixed message {MISMATCH_MESSAGE:?}, got {message:?}"
        );
        // The same requirement said the other way round: nothing of the request
        // or of the store is interpolated, so no token and no ARN the refusal
        // could have reached for appears in it.
        for leaked in [
            BUS_TOKEN,
            SOURCE_TOKEN,
            SUBSCRIBER_TOKEN,
            "arn:aws:",
            "mismatch-token-bus",
            "mismatch-token-source",
            "mismatch-token-subscriber",
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
async fn the_mismatch_wording_is_what_the_wire_body_carries() {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(eventsv2::EmulatedService::new(Arc::clone(&engine)));

    let token = "wire-mismatch-token";
    let (status, body) = cbor_call::<CreateEventBus<InMemoryEngine>>(
        &router,
        &CreateEventBusInput {
            name: "wire-mismatch-bus".to_string(),
            client_token: Some(token.to_string()),
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
            name: "wire-mismatch-bus".to_string(),
            description: Some("a description the first create did not send".to_string()),
            client_token: Some(token.to_string()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        status, 409,
        "case \"wire, mismatched replay\": the repeat must answer 409, got {status} with \
         body {body:?}"
    );
    let rendered = String::from_utf8_lossy(&body).to_string();
    assert!(
        rendered.contains(MISMATCH_MESSAGE),
        "case \"wire, mismatched replay\": the body must carry the fixed message \
         {MISMATCH_MESSAGE:?}, got {rendered:?}"
    );
    assert!(
        rendered.contains(MISMATCH_SHAPE),
        "case \"wire, mismatched replay\": the body must name the error shape, got {rendered:?}"
    );
    for leaked in [token, "arn:aws:", "wire-mismatch-bus"] {
        assert!(
            !rendered.contains(leaked),
            "case \"wire, mismatched replay\": the body must interpolate nothing, but \
             {rendered:?} carries {leaked:?}"
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
