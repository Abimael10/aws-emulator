// SPDX-License-Identifier: CC0-1.0

//! The EventSource lifecycle against an event bus, end to end.
//!
//! Two harnesses, for the reasons the bus suite gives: [`Wire`] drives the real
//! router over HTTP with rpcv2Cbor bodies, so routing, the modelled constraints,
//! the statuses and the bodies are the emulator's own; [`Harness`] calls the
//! typed `Operation::handle` over one shared engine, so a table can span many
//! inputs per rule and an error's wire code can be asserted directly.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use aws_smithy_schema::codec::{Codec, FinishSerializer};
use aws_smithy_schema::serde::{SerializableStruct, ShapeSerializer};
use aws_smithy_schema_server::{CborCodec, DeserializableStruct};
use engine::InMemoryEngine;
use smithy_server_runtime::{
    EmbeddedEmulator, MultiServiceRouter, Operation, OperationContext, OperationError,
};

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

use eventsv2::ops::create_event_bus::CreateEventBus;
use eventsv2::ops::create_event_source::CreateEventSource;
use eventsv2::ops::delete_event_bus::DeleteEventBus;
use eventsv2::ops::delete_event_source::DeleteEventSource;
use eventsv2::ops::describe_event_bus::DescribeEventBus;
use eventsv2::ops::describe_event_source::DescribeEventSource;
use eventsv2::ops::revoke_resource::RevokeResource;
use eventsv2::ops::update_event_source::UpdateEventSource;
use eventsv2::ops::types::{
    AwsServiceEventsSourceConfiguration, CreateEventBusInput, CreateEventSourceInput,
    CreateEventSourceOutput, DeleteEventBusInput, DeleteEventSourceInput, DeleteEventSourceOutput,
    DescribeEventBusInput, DescribeEventSourceInput, DescribeEventSourceOutput,
    EventSourceConfiguration, OnFailureConfiguration, PartnerEventsSourceConfiguration,
    RevokeResourceInput, UpdateEventSourceInput, UpdateEventSourceOutput,
};

use crate::modelled_input_refusal::with_client_token;

/// The account segment every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The region the harness signs for; it must reach the minted ARNs.
const REGION: &str = "us-west-2";

/// Long enough for a due bus lifecycle transition to have come due. The span
/// itself is the service's own constant, so this tracks it.
fn past_the_lifecycle_span() -> Duration {
    eventsv2::state::LIFECYCLE_SPAN + Duration::from_millis(250)
}

/// How long a poll waits for a condition the emulator owes: far above the
/// provisioning span a bus takes, so a loaded host has ample room.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long one pass of a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// A SigV4 `Authorization` header naming `region` and this service's signing
/// name, which is how a request names both.
fn authorization(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{region}/events/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// The request metadata of a call signed for `region`.
fn parts_signed_for(region: &str) -> http::request::Parts {
    let (parts, ()) = http::Request::builder()
        .method("POST")
        .uri("/service/AWSEventsV2/operation/CreateEventSource")
        .header("authorization", authorization(region))
        .header("content-type", "application/cbor")
        .body(())
        .expect("the harness request is well formed")
        .into_parts();
    parts
}

// ---------------------------------------------------------------------------
// Configurations the table spans
// ---------------------------------------------------------------------------

/// An `AwsServiceEventsConfiguration`, whose ARN type segment is `aws.service`.
///
/// `AwsServiceSource`'s own pattern is `^aws\.[a-z0-9\-]+$`, so every service
/// value a row uses is of that shape.
fn aws_service_configuration(
    aws_service: &str,
    pattern: Option<&str>,
    dead_letter_queue: Option<&str>,
) -> EventSourceConfiguration {
    EventSourceConfiguration::AwsServiceEventsConfiguration(AwsServiceEventsSourceConfiguration {
        aws_service: aws_service.to_string(),
        pattern: pattern.map(str::to_string),
        on_failure_configuration: dead_letter_queue.map(|arn| OnFailureConfiguration {
            arn: Some(arn.to_string()),
        }),
    })
}

/// A `PartnerEventsConfiguration`, whose ARN type segment is `aws.partner`.
fn partner_configuration(
    partner_event_source_arn: &str,
    pattern: Option<&str>,
    kms_key: Option<&str>,
) -> EventSourceConfiguration {
    EventSourceConfiguration::PartnerEventsConfiguration(PartnerEventsSourceConfiguration {
        partner_event_source_arn: partner_event_source_arn.to_string(),
        pattern: pattern.map(str::to_string),
        partner_bus_kms_key_identifier: kms_key.map(str::to_string),
        on_failure_configuration: None,
    })
}

/// A `PartnerEventSourceArn` of the modelled shape
/// `event-source/aws.partner/<segment>/<segment>…`.
fn partner_arn(partner: &str, stream: &str) -> String {
    format!("arn:aws:events:{REGION}:{ACCOUNT}:event-source/aws.partner/{partner}/{stream}")
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
            parts: parts_signed_for(REGION),
        }
    }

    fn ctx(&self) -> OperationContext<'_> {
        OperationContext::new(&self.parts, self.definition.identity(), self.callback.clone())
    }

    /// The service's own store, for the few assertions that are about what is
    /// STORED rather than about a response.
    fn store(&self) -> engine::ServiceState<eventsv2::ComAmazonawsEventbridgev2Awseventsv2State> {
        eventsv2::state::service_state(&*self.engine)
            .expect("the in-memory engine always has this service's store")
    }

    async fn create_bus(&self, name: &str) -> String {
        CreateEventBus::new(Arc::clone(&self.engine))
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
            .unwrap_or_else(|e| panic!("case {name:?}: CreateEventBus must succeed, got {e:?}"))
            .event_bus_arn
            .unwrap_or_else(|| panic!("case {name:?}: CreateEventBus must report an EventBusArn"))
    }

    /// Create a bus and wait out its provisioning span, so it is ACTIVE — the
    /// state `CreateEventSource` requires of it.
    async fn active_bus(&self, name: &str) -> String {
        let arn = self.create_bus(name).await;
        tokio::time::sleep(past_the_lifecycle_span()).await;
        arn
    }

    async fn delete_bus(&self, arn: &str) {
        DeleteEventBus::new(Arc::clone(&self.engine))
            .handle(
                DeleteEventBusInput {
                    event_bus_arn: arn.to_string(),
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {arn:?}: DeleteEventBus must succeed, got {e:?}"));
    }

    async fn create_source(
        &self,
        input: CreateEventSourceInput,
    ) -> Result<CreateEventSourceOutput, eventsv2::ops::create_event_source::CreateEventSourceError>
    {
        // A case that means a RETRY states its own token; every other create is given a
        // fresh one, which is the member this service requires of a create.
        CreateEventSource::new(Arc::clone(&self.engine))
            .handle(with_client_token(input), &self.ctx())
            .await
    }

    async fn describe_source(
        &self,
        arn: &str,
    ) -> Result<
        DescribeEventSourceOutput,
        eventsv2::ops::describe_event_source::DescribeEventSourceError,
    > {
        DescribeEventSource::new(Arc::clone(&self.engine))
            .handle(
                DescribeEventSourceInput {
                    event_source_arn: arn.to_string(),
                },
                &self.ctx(),
            )
            .await
    }

    async fn update_source(
        &self,
        input: UpdateEventSourceInput,
    ) -> Result<UpdateEventSourceOutput, eventsv2::ops::update_event_source::UpdateEventSourceError>
    {
        UpdateEventSource::new(Arc::clone(&self.engine))
            .handle(input, &self.ctx())
            .await
    }

    async fn delete_source(
        &self,
        arn: &str,
    ) -> Result<DeleteEventSourceOutput, eventsv2::ops::delete_event_source::DeleteEventSourceError>
    {
        DeleteEventSource::new(Arc::clone(&self.engine))
            .handle(
                DeleteEventSourceInput {
                    event_source_arn: arn.to_string(),
                },
                &self.ctx(),
            )
            .await
    }

    /// Withdraw a resource, which is how a source becomes `Revoked`.
    async fn revoke(&self, arn: &str) {
        RevokeResource::new(Arc::clone(&self.engine))
            .handle(
                RevokeResourceInput {
                    arn: arn.to_string(),
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {arn:?}: RevokeResource must succeed, got {e:?}"));
    }
}

// ---------------------------------------------------------------------------
// The wire harness
// ---------------------------------------------------------------------------

/// One emulator reachable only over HTTP, exactly as the production router
/// mounts it.
struct Wire {
    router: MultiServiceRouter,
}

impl Wire {
    fn new() -> Self {
        let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(eventsv2::EmulatedService::new(engine));
        Self { router }
    }

    /// Encode `input` as this service's wire protocol, dispatch it through the
    /// router, and hand back the HTTP status and the raw response body.
    async fn call<O: Operation>(&self, input: &O::Input) -> (u16, bytes::Bytes) {
        let codec = CborCodec::default();
        let mut serializer = codec.create_serializer();
        serializer
            .write_struct(O::INPUT_SCHEMA, input as &dyn SerializableStruct)
            .unwrap_or_else(|e| panic!("{}: encoding the input must succeed, got {e:?}", O::NAME));
        let body = serializer.finish();

        let request = http::Request::builder()
            .method("POST")
            .uri(format!("/service/AWSEventsV2/operation/{}", O::NAME))
            .header("authorization", authorization(REGION))
            .header("content-type", "application/cbor")
            .header("smithy-protocol", "rpc-v2-cbor")
            .body(bytes::Bytes::from(body))
            .unwrap_or_else(|e| panic!("{}: the request must be well formed, got {e:?}", O::NAME));

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (status, response.into_body())
    }

    /// The same call, requiring a 200 and decoding the typed output body.
    async fn ok<O: Operation>(&self, input: &O::Input, case: &str) -> O::Output
    where
        O::Output: DeserializableStruct,
    {
        let (status, body) = self.call::<O>(input).await;
        assert_eq!(
            status,
            200,
            "case {case:?}: {} must answer 200, got {status} with body {body:?}",
            O::NAME
        );
        let codec = CborCodec::default();
        let mut deserializer = codec.create_deserializer(&body);
        O::Output::deserialize(&mut deserializer).unwrap_or_else(|e| {
            panic!(
                "case {case:?}: {}'s body must decode as its modelled output, got {e:?}",
                O::NAME
            )
        })
    }
}

// ---------------------------------------------------------------------------
// ARN assertions
// ---------------------------------------------------------------------------

/// Assert an `EventSourceArn` has the modelled form
/// `arn:aws:events:<region>:<account>:event-sourcev2/<type>/<name>/[a-z0-9]{25}`
/// and hand back its id segment.
fn assert_event_source_arn(
    arn: &str,
    region: &str,
    origin_type: &str,
    name: &str,
    case: &str,
) -> String {
    let prefix = format!("arn:aws:events:{region}:{ACCOUNT}:event-sourcev2/{origin_type}/{name}/");
    let id = arn.strip_prefix(&prefix).unwrap_or_else(|| {
        panic!("case {case:?}: EventSourceArn must start with {prefix:?}, got {arn:?}")
    });
    assert_eq!(
        id.len(),
        eventsv2::state::EVENT_SOURCE_ID_LEN,
        "case {case:?}: the ARN id segment must be {} characters, got {id:?}",
        eventsv2::state::EVENT_SOURCE_ID_LEN
    );
    assert!(
        id.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
        "case {case:?}: the ARN id segment must be lowercase alphanumeric, got {id:?}"
    );
    id.to_string()
}

/// The `AwsService` of an `AwsServiceEventsConfiguration`, or `None` for the
/// partner variant — enough to assert a read-back configuration is the one that
/// was created.
fn origin_of(configuration: &EventSourceConfiguration) -> (&'static str, String, Option<String>) {
    match configuration {
        EventSourceConfiguration::AwsServiceEventsConfiguration(config) => (
            "aws.service",
            config.aws_service.clone(),
            config.pattern.clone(),
        ),
        EventSourceConfiguration::PartnerEventsConfiguration(config) => (
            "aws.partner",
            config.partner_event_source_arn.clone(),
            config.pattern.clone(),
        ),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// RULE: `CreateEventSource` attaches a new EventSource to an ACTIVE event bus,
/// mints its ARN in that bus's region and account with the type segment its
/// configuration variant determines, persists it ACTIVE with the configuration,
/// description and tags the request carried, and reports it back; a later
/// `DescribeEventSource` reads exactly that stored resource.
///
/// The table spans the dimensions the rule generalises over: both configuration
/// variants, a configuration with and without an event pattern, with and
/// without failure handling, and a description and tags present or absent.
#[tokio::test]
async fn a_created_event_source_is_stored_and_reads_back() {
    let harness = Harness::new();
    let bus = harness.active_bus("source-table-bus").await;

    let cases: Vec<(&str, CreateEventSourceInput)> = vec![
        (
            "aws service, minimal",
            CreateEventSourceInput {
                name: "minimal-service".to_string(),
                event_bus_arn: bus.clone(),
                configuration: aws_service_configuration("aws.s3", None, None),
                description: None,
                tags: None,
                client_token: None,
            },
        ),
        (
            "aws service, pattern and dead-letter queue and tags",
            CreateEventSourceInput {
                name: "full.service_1".to_string(),
                event_bus_arn: bus.clone(),
                configuration: aws_service_configuration(
                    "aws.ec2",
                    Some(r#"{"detail-type":["Instance State-change Notification"]}"#),
                    Some(&format!("arn:aws:sqs:{REGION}:{ACCOUNT}:source-dlq")),
                ),
                description: Some("every optional member set".to_string()),
                tags: Some(HashMap::from([
                    ("team".to_string(), "platform".to_string()),
                    ("tier".to_string(), String::new()),
                ])),
                client_token: None,
            },
        ),
        (
            "partner, minimal",
            CreateEventSourceInput {
                name: "minimal-partner".to_string(),
                event_bus_arn: bus.clone(),
                configuration: partner_configuration(
                    &partner_arn("partner.example", "stream-1"),
                    None,
                    None,
                ),
                description: Some(String::new()),
                tags: None,
                client_token: None,
            },
        ),
        (
            "partner, pattern and customer key",
            CreateEventSourceInput {
                name: "partner-full".to_string(),
                event_bus_arn: bus.clone(),
                configuration: partner_configuration(
                    &partner_arn("partner.example", "stream-2"),
                    Some(r#"{"detail-type":["Order"]}"#),
                    Some("alias/partner-bus"),
                ),
                description: Some("partner source with a pattern".to_string()),
                tags: Some(HashMap::from([("team".to_string(), "orders".to_string())])),
                client_token: Some("partner-full-token".to_string()),
            },
        ),
    ];

    let mut minted_ids: Vec<String> = Vec::new();

    for (case, input) in cases {
        let (origin_type, origin, pattern) = origin_of(&input.configuration);
        let created = harness
            .create_source(input.clone())
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: CreateEventSource must succeed, got {e:?}"));

        let arn = created.event_source_arn.clone().unwrap_or_else(|| {
            panic!("case {case:?}: CreateEventSource must report an EventSourceArn")
        });
        minted_ids.push(assert_event_source_arn(
            &arn, REGION, origin_type, &input.name, case,
        ));
        assert_eq!(
            created.name.as_deref(),
            Some(input.name.as_str()),
            "case {case:?}: Name must be what was created, got {:?}",
            created.name
        );
        assert_eq!(
            created.event_bus_arn.as_deref(),
            Some(bus.as_str()),
            "case {case:?}: EventBusArn must be the bus the source was created against, got {:?}",
            created.event_bus_arn
        );
        // Provisioning is synchronous for an EventSource, so it is reported
        // operational by the call that mints it.
        assert_eq!(
            created.state.as_deref(),
            Some("ACTIVE"),
            "case {case:?}: State must be ACTIVE, got {:?}",
            created.state
        );
        let creation_time = created.creation_time.unwrap_or_else(|| {
            panic!("case {case:?}: CreationTime must be reported, got {created:?}")
        });

        // Read the resource back through the implemented reader: every member
        // must come out of state as it went in.
        let described = harness
            .describe_source(&arn)
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: DescribeEventSource must succeed, got {e:?}"));
        assert_eq!(
            described.event_source_arn.as_deref(),
            Some(arn.as_str()),
            "case {case:?}: EventSourceArn must be the source asked for, got {:?}",
            described.event_source_arn
        );
        assert_eq!(
            described.name.as_deref(),
            Some(input.name.as_str()),
            "case {case:?}: Name must be read back from state, got {:?}",
            described.name
        );
        assert_eq!(
            described.event_bus_arn.as_deref(),
            Some(bus.as_str()),
            "case {case:?}: EventBusArn must be read back from state, got {:?}",
            described.event_bus_arn
        );
        assert_eq!(
            described.description, input.description,
            "case {case:?}: Description must be read back from state, got {:?}",
            described.description
        );
        assert_eq!(
            described.state.as_deref(),
            Some("ACTIVE"),
            "case {case:?}: State must be read back as ACTIVE, got {:?}",
            described.state
        );
        assert_eq!(
            described.revoked,
            None,
            "case {case:?}: a source nobody revoked must not report Revoked at all, got {:?}",
            described.revoked
        );
        assert_eq!(
            described.creation_time,
            Some(creation_time),
            "case {case:?}: CreationTime must be the instant the create reported, got {:?}",
            described.creation_time
        );
        assert_eq!(
            described.last_modified_time,
            Some(creation_time),
            "case {case:?}: LastModifiedTime of an untouched source must be its creation time, \
             got {:?}",
            described.last_modified_time
        );
        let read_back = described.configuration.as_ref().unwrap_or_else(|| {
            panic!("case {case:?}: Configuration must be reported, got {described:?}")
        });
        assert_eq!(
            origin_of(read_back),
            (origin_type, origin.clone(), pattern.clone()),
            "case {case:?}: the configuration must be read back as created, got {:?}",
            origin_of(read_back)
        );

        // The tags the create carried are persisted, which is what a later
        // tag read projects.
        let requested_tags: std::collections::BTreeMap<String, String> =
            input.tags.clone().unwrap_or_default().into_iter().collect();
        let stored_tags = harness
            .store()
            .lock()
            .event_source(&arn)
            .map(|source| source.tags.clone())
            .unwrap_or_else(|| panic!("case {case:?}: the source must be in the store"));
        assert_eq!(
            stored_tags, requested_tags,
            "case {case:?}: the stored tags must be the ones requested, got {stored_tags:?}"
        );
    }

    // Each create mints its own id: the ARN is not derived from the name.
    let unique: std::collections::BTreeSet<&String> = minted_ids.iter().collect();
    assert_eq!(
        unique.len(),
        minted_ids.len(),
        "case \"minted ids\": every EventSource id must be distinct, got {minted_ids:?}"
    );
}

/// RULE: an EventSource can only be created against an event bus that exists
/// and is ACTIVE — a bus nothing minted is `ResourceNotFoundException` (404),
/// and a bus in any other lifecycle state is `InvalidStateException` (409).
#[tokio::test]
async fn an_event_source_create_needs_an_active_bus() {
    let harness = Harness::new();

    // A bus that was never created, and one whose ARN is well formed but names
    // an id nothing minted.
    let never = format!(
        "arn:aws:events:{REGION}:{ACCOUNT}:event-busv2/absent-bus/aaaaaaaaaaaaaaaaaaaaaaaaa"
    );
    // A bus on its way out.
    let deleting = harness.active_bus("on-its-way-out").await;
    harness.delete_bus(&deleting).await;
    // A bus still provisioning: created last, so no later wait in this test can
    // let its span elapse before the call below.
    let creating = harness.create_bus("still-creating").await;

    let cases: Vec<(&str, &str, &str, u16)> = vec![
        ("bus never created", never.as_str(), "ResourceNotFoundException", 404),
        ("bus CREATING", creating.as_str(), "InvalidStateException", 409),
        ("bus DELETING", deleting.as_str(), "InvalidStateException", 409),
    ];

    for (case, bus_arn, expected_code, expected_status) in cases {
        let error = harness
            .create_source(CreateEventSourceInput {
                name: "refused-source".to_string(),
                event_bus_arn: bus_arn.to_string(),
                configuration: aws_service_configuration("aws.s3", None, None),
                description: None,
                tags: None,
                client_token: None,
            })
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: CreateEventSource must be refused"));
        assert_eq!(
            error.shape_name(),
            expected_code,
            "case {case:?}: expected {expected_code}, got {:?}",
            error.shape_name()
        );
        assert_eq!(
            error.http_status(),
            expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {}",
            error.http_status()
        );
        assert!(
            harness.store().lock().event_sources.is_empty(),
            "case {case:?}: a refused create must persist nothing, got {} stored",
            harness.store().lock().event_sources.len()
        );
    }
}

/// RULE: an EventSource name is unique per account and region, so a second
/// create of the same name is `ResourceAlreadyExistsException` (409) whichever
/// bus or configuration it names.
#[tokio::test]
async fn an_event_source_name_is_taken_once() {
    let harness = Harness::new();
    let first_bus = harness.active_bus("name-bus-one").await;
    let second_bus = harness.active_bus("name-bus-two").await;

    harness
        .create_source(CreateEventSourceInput {
            name: "taken".to_string(),
            event_bus_arn: first_bus.clone(),
            configuration: aws_service_configuration("aws.s3", None, None),
            description: None,
            tags: None,
            client_token: None,
        })
        .await
        .expect("the first create must succeed");

    let cases: Vec<(&str, CreateEventSourceInput)> = vec![
        (
            "same bus, same configuration",
            CreateEventSourceInput {
                name: "taken".to_string(),
                event_bus_arn: first_bus.clone(),
                configuration: aws_service_configuration("aws.s3", None, None),
                description: None,
                tags: None,
                client_token: None,
            },
        ),
        (
            "same bus, another configuration variant",
            CreateEventSourceInput {
                name: "taken".to_string(),
                event_bus_arn: first_bus.clone(),
                configuration: partner_configuration(
                    &partner_arn("partner.example", "stream-9"),
                    None,
                    None,
                ),
                description: Some("different in every other way".to_string()),
                tags: None,
                client_token: None,
            },
        ),
        (
            "another bus in the same region",
            CreateEventSourceInput {
                name: "taken".to_string(),
                event_bus_arn: second_bus.clone(),
                configuration: aws_service_configuration("aws.ec2", None, None),
                description: None,
                tags: None,
                client_token: None,
            },
        ),
    ];

    for (case, input) in cases {
        let error = harness
            .create_source(input)
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: the second create must be refused"));
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
    }

    assert_eq!(
        harness.store().lock().event_sources.len(),
        1,
        "case \"after the refusals\": exactly the first source must be stored"
    );
}

/// RULE: a repeat create carrying a `ClientToken` already used replays the
/// EventSource that token minted when every other parameter is identical, and
/// is refused with `IdempotentParameterMismatchException` (409) when any of
/// them differs.
#[tokio::test]
async fn a_repeated_client_token_replays_the_source_it_minted() {
    let harness = Harness::new();
    let bus = harness.active_bus("token-bus").await;
    let other_bus = harness.active_bus("token-bus-two").await;

    let original = || CreateEventSourceInput {
        name: "token-source".to_string(),
        event_bus_arn: bus.clone(),
        configuration: aws_service_configuration(
            "aws.s3",
            Some(r#"{"detail-type":["Object Created"]}"#),
            Some(&format!("arn:aws:sqs:{REGION}:{ACCOUNT}:token-dlq")),
        ),
        description: Some("the first create".to_string()),
        tags: Some(HashMap::from([("team".to_string(), "platform".to_string())])),
        client_token: Some("token-1".to_string()),
    };

    let created = harness
        .create_source(original())
        .await
        .expect("the first create must succeed");
    let arn = created
        .event_source_arn
        .clone()
        .expect("CreateEventSource must report an EventSourceArn");

    // An identical retry replays the same resource and mints nothing.
    let replay = harness
        .create_source(original())
        .await
        .expect("case \"identical retry\": the replay must succeed");
    assert_eq!(
        replay.event_source_arn.as_deref(),
        Some(arn.as_str()),
        "case \"identical retry\": the replay must report the original ARN, got {:?}",
        replay.event_source_arn
    );
    assert_eq!(
        replay.creation_time, created.creation_time,
        "case \"identical retry\": the replay must report the original CreationTime, got {:?}",
        replay.creation_time
    );
    assert_eq!(
        harness.store().lock().event_sources.len(),
        1,
        "case \"identical retry\": a replay must not mint a second source"
    );

    let differing: Vec<(&str, CreateEventSourceInput)> = vec![
        (
            "another name",
            CreateEventSourceInput {
                name: "token-source-renamed".to_string(),
                ..original()
            },
        ),
        (
            "another bus",
            CreateEventSourceInput {
                event_bus_arn: other_bus.clone(),
                ..original()
            },
        ),
        (
            "another description",
            CreateEventSourceInput {
                description: Some("changed".to_string()),
                ..original()
            },
        ),
        (
            "description absent",
            CreateEventSourceInput {
                description: None,
                ..original()
            },
        ),
        (
            "another configuration variant",
            CreateEventSourceInput {
                configuration: partner_configuration(
                    &partner_arn("partner.example", "stream-3"),
                    None,
                    None,
                ),
                ..original()
            },
        ),
        (
            "another event pattern",
            CreateEventSourceInput {
                configuration: aws_service_configuration(
                    "aws.s3",
                    Some(r#"{"detail-type":["Instance State-change Notification"]}"#),
                    Some(&format!("arn:aws:sqs:{REGION}:{ACCOUNT}:token-dlq")),
                ),
                ..original()
            },
        ),
        (
            "failure handling absent",
            CreateEventSourceInput {
                configuration: aws_service_configuration(
                    "aws.s3",
                    Some(r#"{"detail-type":["Object Created"]}"#),
                    None,
                ),
                ..original()
            },
        ),
        (
            "another tag value",
            CreateEventSourceInput {
                tags: Some(HashMap::from([("team".to_string(), "orders".to_string())])),
                ..original()
            },
        ),
        (
            "tags absent",
            CreateEventSourceInput {
                tags: None,
                ..original()
            },
        ),
    ];

    for (case, input) in differing {
        let error = harness
            .create_source(input)
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: a mismatched replay must be refused"));
        assert_eq!(
            error.shape_name(),
            "IdempotentParameterMismatchException",
            "case {case:?}: expected IdempotentParameterMismatchException, got {:?}",
            error.shape_name()
        );
        assert_eq!(
            error.http_status(),
            409,
            "case {case:?}: expected HTTP 409, got {}",
            error.http_status()
        );
    }
}

/// RULE: `DescribeEventSource` reports only a stored EventSource — an ARN
/// nothing minted, and one whose bus has finished disappearing, are both
/// `ResourceNotFoundException` (404), because an EventSource forwards onto one
/// bus and cannot outlive it.
#[tokio::test]
async fn describe_refuses_an_event_source_that_is_not_stored() {
    let harness = Harness::new();
    let bus = harness.active_bus("transient-bus").await;
    let source = harness
        .create_source(CreateEventSourceInput {
            name: "attached".to_string(),
            event_bus_arn: bus.clone(),
            configuration: aws_service_configuration("aws.s3", None, None),
            description: None,
            tags: None,
            client_token: Some("transient-token".to_string()),
        })
        .await
        .expect("the create must succeed")
        .event_source_arn
        .expect("CreateEventSource must report an EventSourceArn");

    harness.delete_bus(&bus).await;
    // Wait for the CONDITION the rows below need — the bus's deletion having
    // completed, so the source it held is gone — rather than for a duration.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while harness.describe_source(&source).await.is_ok() {
        assert!(
            std::time::Instant::now() < deadline,
            "case \"bus deleted under it\": the source must disappear with the bus that held it"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let never = format!(
        "arn:aws:events:{REGION}:{ACCOUNT}:event-sourcev2/aws.service/absent/\
         aaaaaaaaaaaaaaaaaaaaaaaaa"
    );
    for (case, arn) in [
        ("never created", never.as_str()),
        ("bus deleted under it", source.as_str()),
    ] {
        let error = harness
            .describe_source(arn)
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: DescribeEventSource must be refused"));
        assert_eq!(
            error.shape_name(),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {:?}",
            error.shape_name()
        );
        assert_eq!(
            error.http_status(),
            404,
            "case {case:?}: expected HTTP 404, got {}",
            error.http_status()
        );
    }

    // The token that minted the forgotten source replays nothing either: the
    // replay record is the source itself, and the source is gone.
    assert!(
        harness
            .store()
            .lock()
            .event_source_by_client_token(REGION, "transient-token")
            .is_none(),
        "case \"bus deleted under it\": the replay record must go with the source"
    );
}

/// The documented workflow over the wire, asserting on response BODIES and HTTP
/// statuses: create a bus, create an EventSource on it, describe it, and see a
/// create against a bus nothing minted refused with 404.
#[tokio::test]
async fn the_event_source_workflow_runs_over_the_wire() {
    let wire = Wire::new();

    let bus = wire
        .ok::<CreateEventBus<InMemoryEngine>>(
            &CreateEventBusInput {
                name: "wire-bus".to_string(),
                // The create's `@idempotencyToken`, which the model leaves optional.
                client_token: Some("a-wire-bus-token".to_string()),
                ..Default::default()
            },
            "create bus",
        )
        .await
        .event_bus_arn
        .expect("case \"create bus\": CreateEventBus must report an EventBusArn");

    // The bus must be ACTIVE before it can take an EventSource.
    let early = CreateEventSourceInput {
        name: "wire-source".to_string(),
        event_bus_arn: bus.clone(),
        configuration: aws_service_configuration("aws.s3", None, None),
        description: None,
        tags: None,
        client_token: Some("an-early-wire-token".to_string()),
    };
    let (status, _) = wire.call::<CreateEventSource<InMemoryEngine>>(&early).await;
    assert_eq!(
        status, 409,
        "case \"create while the bus is CREATING\": expected HTTP 409, got {status}"
    );

    // Provisioning is asynchronous, so the suite polls for the CONDITION — the bus
    // reporting ACTIVE — rather than waiting a fixed time, under a deadline far above
    // the span that transition takes.
    let deadline = std::time::Instant::now() + POLL_DEADLINE;
    loop {
        let reported = wire
            .ok::<DescribeEventBus<InMemoryEngine>>(
                &DescribeEventBusInput {
                    event_bus_arn: bus.clone(),
                },
                "describe the bus while it provisions",
            )
            .await
            .state;
        if reported.as_deref() == Some("ACTIVE") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case \"create source\": the bus never reported ACTIVE, last reported {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    let create = CreateEventSourceInput {
        name: "wire-source".to_string(),
        event_bus_arn: bus.clone(),
        configuration: aws_service_configuration(
            "aws.s3",
            Some(r#"{"detail-type":["Object Created"]}"#),
            Some(&format!("arn:aws:sqs:{REGION}:{ACCOUNT}:wire-dlq")),
        ),
        description: Some("created over the wire".to_string()),
        tags: Some(HashMap::from([("team".to_string(), "platform".to_string())])),
        client_token: Some("wire-token".to_string()),
    };
    let created = wire
        .ok::<CreateEventSource<InMemoryEngine>>(&create, "create source")
        .await;
    let arn = created
        .event_source_arn
        .clone()
        .expect("case \"create source\": CreateEventSource must report an EventSourceArn");
    assert_event_source_arn(&arn, REGION, "aws.service", "wire-source", "create source");
    assert_eq!(
        created.state.as_deref(),
        Some("ACTIVE"),
        "case \"create source\": State must be ACTIVE, got {:?}",
        created.state
    );

    let described = wire
        .ok::<DescribeEventSource<InMemoryEngine>>(
            &DescribeEventSourceInput {
                event_source_arn: arn.clone(),
            },
            "describe source",
        )
        .await;
    assert_eq!(
        described.event_source_arn.as_deref(),
        Some(arn.as_str()),
        "case \"describe source\": EventSourceArn must be the source asked for, got {:?}",
        described.event_source_arn
    );
    assert_eq!(
        described.description.as_deref(),
        Some("created over the wire"),
        "case \"describe source\": Description must be read back, got {:?}",
        described.description
    );
    assert_eq!(
        origin_of(
            described
                .configuration
                .as_ref()
                .expect("case \"describe source\": Configuration must be reported")
        ),
        (
            "aws.service",
            "aws.s3".to_string(),
            Some(r#"{"detail-type":["Object Created"]}"#.to_string())
        ),
        "case \"describe source\": the configuration must be read back as created"
    );

    // A bus nothing minted is a 404 on the wire too.
    let absent = CreateEventSourceInput {
        name: "orphan".to_string(),
        event_bus_arn: format!(
            "arn:aws:events:{REGION}:{ACCOUNT}:event-busv2/absent/aaaaaaaaaaaaaaaaaaaaaaaaa"
        ),
        configuration: aws_service_configuration("aws.s3", None, None),
        description: None,
        tags: None,
        client_token: Some("an-orphan-wire-token".to_string()),
    };
    let (status, _) = wire.call::<CreateEventSource<InMemoryEngine>>(&absent).await;
    assert_eq!(
        status, 404,
        "case \"create against an absent bus\": expected HTTP 404, got {status}"
    );
}

// ---------------------------------------------------------------------------
// Updating and deleting an EventSource
// ---------------------------------------------------------------------------

/// Everything an update can change about an EventSource, as a read reports it:
/// the description, and the configuration's origin type, origin, filter pattern
/// and failure destination.
#[derive(Debug, PartialEq, Eq)]
struct SourceFacts {
    description: Option<String>,
    origin_type: &'static str,
    origin: String,
    pattern: Option<String>,
    dead_letter_queue_arn: Option<String>,
    state: Option<String>,
    revoked: Option<bool>,
}

/// The facts a `DescribeEventSource` response carries.
fn facts_of_described_source(output: &DescribeEventSourceOutput) -> SourceFacts {
    let configuration = output
        .configuration
        .as_ref()
        .expect("DescribeEventSource must report a Configuration");
    let (origin_type, origin, pattern) = origin_of(configuration);
    SourceFacts {
        description: output.description.clone(),
        origin_type,
        origin,
        pattern,
        dead_letter_queue_arn: dead_letter_queue_of(configuration),
        state: output.state.clone(),
        revoked: output.revoked,
    }
}

/// The `OnFailureConfiguration` destination a configuration carries, whichever
/// variant it is.
fn dead_letter_queue_of(configuration: &EventSourceConfiguration) -> Option<String> {
    match configuration {
        EventSourceConfiguration::AwsServiceEventsConfiguration(config) => config
            .on_failure_configuration
            .as_ref()
            .and_then(|on_failure| on_failure.arn.clone()),
        EventSourceConfiguration::PartnerEventsConfiguration(config) => config
            .on_failure_configuration
            .as_ref()
            .and_then(|on_failure| on_failure.arn.clone()),
    }
}

/// The whole source a row expects. An EventSource is provisioned synchronously,
/// so an applied change leaves it ACTIVE and unrevoked.
fn expected_source(
    description: Option<&str>,
    origin: &str,
    pattern: Option<&str>,
    dead_letter_queue_arn: Option<&str>,
) -> SourceFacts {
    SourceFacts {
        description: description.map(str::to_string),
        origin_type: "aws.service",
        origin: origin.to_string(),
        pattern: pattern.map(str::to_string),
        dead_letter_queue_arn: dead_letter_queue_arn.map(str::to_string),
        state: Some("ACTIVE".to_string()),
        revoked: None,
    }
}

/// RULE: an update writes onto the stored resource exactly the members the
/// request carries — each replacing the stored value, and a structure or union
/// member wholesale rather than merged into the stored one, so a member omitted
/// INSIDE a supplied configuration is cleared — and leaves every member the
/// request omits as it stands; every later read reports the resource as the
/// update left it.
///
/// The table spans what the rule generalises over: which member is carried (one,
/// both, none at all), the kind of value (text, the empty text, a union variant
/// with and without its optional members), and each row is applied to the source
/// the previous row left, so an expectation names the WHOLE resource rather than
/// only the members its own row carried.
#[tokio::test]
async fn an_update_applies_what_it_carries_and_replaces_the_configuration_wholesale() {
    let harness = Harness::new();
    let bus = harness.active_bus("updatable-source-bus").await;
    let dead_letter_queue = format!("arn:aws:sqs:{REGION}:{ACCOUNT}:source-dlq");
    let created = harness
        .create_source(CreateEventSourceInput {
            name: "updatable-source".to_string(),
            event_bus_arn: bus.clone(),
            configuration: aws_service_configuration(
                "aws.ec2",
                Some(r#"{"detail-type": ["EC2 Instance State-change Notification"]}"#),
                Some(&dead_letter_queue),
            ),
            description: Some("first description".to_string()),
            tags: None,
            client_token: Some("updatable-source-token".to_string()),
        })
        .await
        .expect("CreateEventSource must succeed");
    let arn = created
        .event_source_arn
        .clone()
        .expect("CreateEventSource must report an EventSourceArn");
    let created_at = created.creation_time;

    let rows: Vec<(&str, UpdateEventSourceInput, SourceFacts)> = vec![
        (
            "description only",
            UpdateEventSourceInput {
                event_source_arn: arn.clone(),
                description: Some("second description".to_string()),
                ..Default::default()
            },
            expected_source(
                Some("second description"),
                "aws.ec2",
                Some(r#"{"detail-type": ["EC2 Instance State-change Notification"]}"#),
                Some(&dead_letter_queue),
            ),
        ),
        (
            "configuration only, carrying neither a pattern nor a failure destination",
            UpdateEventSourceInput {
                event_source_arn: arn.clone(),
                configuration: Some(aws_service_configuration("aws.s3", None, None)),
                ..Default::default()
            },
            expected_source(Some("second description"), "aws.s3", None, None),
        ),
        (
            "configuration carrying a pattern again",
            UpdateEventSourceInput {
                event_source_arn: arn.clone(),
                configuration: Some(aws_service_configuration(
                    "aws.s3",
                    Some(r#"{"detail-type":["Object Created"]}"#),
                    None,
                )),
                ..Default::default()
            },
            expected_source(
                Some("second description"),
                "aws.s3",
                Some(r#"{"detail-type":["Object Created"]}"#),
                None,
            ),
        ),
        (
            "description and configuration together",
            UpdateEventSourceInput {
                event_source_arn: arn.clone(),
                configuration: Some(aws_service_configuration(
                    "aws.lambda",
                    None,
                    Some(&dead_letter_queue),
                )),
                description: Some(String::new()),
            },
            expected_source(Some(""), "aws.lambda", None, Some(&dead_letter_queue)),
        ),
        (
            "no member carried at all",
            UpdateEventSourceInput {
                event_source_arn: arn.clone(),
                ..Default::default()
            },
            expected_source(Some(""), "aws.lambda", None, Some(&dead_letter_queue)),
        ),
    ];

    for (case, input, expected) in rows {
        let updated = harness
            .update_source(input)
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: UpdateEventSource must succeed, got {e:?}"));
        assert_eq!(
            updated.event_source_arn.as_deref(),
            Some(arn.as_str()),
            "case {case:?}: the response must name the source updated, got {:?}",
            updated.event_source_arn
        );
        assert_eq!(
            updated.event_bus_arn.as_deref(),
            Some(bus.as_str()),
            "case {case:?}: the source must still forward onto its bus, got {:?}",
            updated.event_bus_arn
        );
        assert_eq!(
            updated.state.as_deref(),
            Some("ACTIVE"),
            "case {case:?}: State must be ACTIVE, got {:?}",
            updated.state
        );

        let described = harness
            .describe_source(&arn)
            .await
            .unwrap_or_else(|e| {
                panic!("case {case:?}: DescribeEventSource must succeed, got {e:?}")
            });
        let read_back = facts_of_described_source(&described);
        assert_eq!(
            read_back, expected,
            "case {case:?}: the read-back source must be the one the update left, got \
             {read_back:?}"
        );

        // An update changes the resource's configuration, never its identity.
        assert_eq!(
            described.name.as_deref(),
            Some("updatable-source"),
            "case {case:?}: the Name must not change, got {:?}",
            described.name
        );
        assert_eq!(
            described.creation_time, created_at,
            "case {case:?}: CreationTime must not change, got {:?}",
            described.creation_time
        );
        assert!(
            described.last_modified_time.is_some(),
            "case {case:?}: LastModifiedTime must be reported, got {:?}",
            described.last_modified_time
        );
    }
}

/// RULE: an update that is refused writes nothing — an ARN naming no stored
/// EventSource is `ResourceNotFoundException` (404); a `Configuration` of the
/// other variant is not a change this resource can take, because its ARN carries
/// the origin type the service set, and is refused with the modelled
/// `InvalidInputException` (400); and a revoked EventSource refuses every
/// mutating operation with `InvalidStateException` (409).
#[tokio::test]
async fn an_update_that_is_refused_leaves_the_event_source_as_it_was() {
    let harness = Harness::new();
    let bus = harness.active_bus("refused-update-bus").await;
    let arn = harness
        .create_source(CreateEventSourceInput {
            name: "refused-update-source".to_string(),
            event_bus_arn: bus.clone(),
            configuration: aws_service_configuration("aws.ec2", None, None),
            description: Some("first description".to_string()),
            tags: None,
            client_token: Some("refused-update-token".to_string()),
        })
        .await
        .expect("CreateEventSource must succeed")
        .event_source_arn
        .expect("CreateEventSource must report an EventSourceArn");

    let unknown = format!(
        "arn:aws:events:{REGION}:{ACCOUNT}:event-sourcev2/aws.service/absent/\
         bbbbbbbbbbbbbbbbbbbbbbbbb"
    );
    let rows: [(&str, UpdateEventSourceInput, &str, u16); 2] = [
        (
            "an ARN naming no stored source",
            UpdateEventSourceInput {
                event_source_arn: unknown.clone(),
                description: Some("nowhere to apply this".to_string()),
                ..Default::default()
            },
            "ResourceNotFoundException",
            404,
        ),
        (
            "a configuration of the other variant",
            UpdateEventSourceInput {
                event_source_arn: arn.clone(),
                configuration: Some(partner_configuration(
                    &partner_arn("partner.example", "stream-9"),
                    None,
                    None,
                )),
                ..Default::default()
            },
            "InvalidInputException",
            400,
        ),
    ];

    for (case, input, shape, status) in rows {
        let refused = harness
            .update_source(input)
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: UpdateEventSource must be refused"));
        assert_eq!(
            refused.shape_name(),
            shape,
            "case {case:?}: expected {shape}, got {:?}",
            refused.shape_name()
        );
        assert_eq!(
            refused.http_status(),
            status,
            "case {case:?}: expected HTTP {status}, got {}",
            refused.http_status()
        );
    }

    // Neither refusal wrote anything.
    let described = harness
        .describe_source(&arn)
        .await
        .expect("DescribeEventSource must succeed");
    assert_eq!(
        facts_of_described_source(&described),
        expected_source(Some("first description"), "aws.ec2", None, None),
        "case \"after the refusals\": the source must stand exactly as it was created"
    );

    // Revocation is terminal, and a revoked source takes no further change.
    harness.revoke(&arn).await;
    let refused = harness
        .update_source(UpdateEventSourceInput {
            event_source_arn: arn.clone(),
            description: Some("after the revocation".to_string()),
            ..Default::default()
        })
        .await
        .expect_err("case \"revoked\": UpdateEventSource must be refused");
    assert_eq!(
        refused.shape_name(),
        "InvalidStateException",
        "case \"revoked\": expected InvalidStateException, got {:?}",
        refused.shape_name()
    );
    assert_eq!(
        refused.http_status(),
        409,
        "case \"revoked\": expected HTTP 409, got {}",
        refused.http_status()
    );
    let described = harness
        .describe_source(&arn)
        .await
        .expect("DescribeEventSource must succeed");
    assert_eq!(
        described.description.as_deref(),
        Some("first description"),
        "case \"revoked\": the refused update must change nothing, got {:?}",
        described.description
    );
}

/// RULE: `DeleteEventSource` removes the EventSource it addresses, whichever
/// state it is in, so forwarding from its origin stops: every later read of its
/// ARN is `ResourceNotFoundException` (404), a repeated delete is refused the
/// same way, and the name it held is free for a new source.
#[tokio::test]
async fn a_deleted_event_source_is_gone_and_frees_what_it_held() {
    let harness = Harness::new();
    let bus = harness.active_bus("deletable-source-bus").await;

    // Rows: a plain source, and one the bus owner has revoked — `Revoked`
    // documents that DeleteEventSource "stays available so a revoked EventSource
    // can still be cleaned up".
    for (case, name, revoke_first) in [
        ("active source", "deletable-source", false),
        ("revoked source", "revoked-source", true),
    ] {
        let arn = harness
            .create_source(CreateEventSourceInput {
                name: name.to_string(),
                event_bus_arn: bus.clone(),
                configuration: aws_service_configuration("aws.s3", None, None),
                description: None,
                tags: None,
                client_token: Some(format!("{name}-token")),
            })
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: CreateEventSource must succeed, got {e:?}"))
            .event_source_arn
            .unwrap_or_else(|| panic!("case {case:?}: an EventSourceArn must be reported"));
        if revoke_first {
            harness.revoke(&arn).await;
        }

        harness
            .delete_source(&arn)
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: DeleteEventSource must succeed, got {e:?}"));

        let gone = harness
            .describe_source(&arn)
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: the deleted source must not read back"));
        assert_eq!(
            gone.shape_name(),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {:?}",
            gone.shape_name()
        );
        assert_eq!(
            gone.http_status(),
            404,
            "case {case:?}: expected HTTP 404, got {}",
            gone.http_status()
        );

        let repeated = harness
            .delete_source(&arn)
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: a repeated delete must be refused"));
        assert_eq!(
            repeated.shape_name(),
            "ResourceNotFoundException",
            "case {case:?}: a repeated delete must answer ResourceNotFoundException, got {:?}",
            repeated.shape_name()
        );

        // The name — and the client token the source carried — are free again.
        let replacement = harness
            .create_source(CreateEventSourceInput {
                name: name.to_string(),
                event_bus_arn: bus.clone(),
                configuration: aws_service_configuration("aws.s3", None, None),
                description: Some("the replacement".to_string()),
                tags: None,
                client_token: Some(format!("{name}-token")),
            })
            .await
            .unwrap_or_else(|e| {
                panic!("case {case:?}: the name must be free for a new source, got {e:?}")
            });
        let replacement_arn = replacement
            .event_source_arn
            .unwrap_or_else(|| panic!("case {case:?}: an EventSourceArn must be reported"));
        assert_ne!(
            replacement_arn, arn,
            "case {case:?}: the replacement must be a new resource, got {replacement_arn:?}"
        );
        let described = harness
            .describe_source(&replacement_arn)
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: the replacement must read back, got {e:?}"));
        assert_eq!(
            described.description.as_deref(),
            Some("the replacement"),
            "case {case:?}: the replacement is its own resource, got {:?}",
            described.description
        );
    }
}
