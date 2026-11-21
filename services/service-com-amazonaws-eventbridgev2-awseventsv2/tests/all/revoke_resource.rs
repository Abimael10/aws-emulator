// SPDX-License-Identifier: CC0-1.0

//! What `RevokeResource` does to a revocable resource, and what a later read of
//! that resource reports.
//!
//! RULE: `RevokeResource` records, on the one revocable resource its `Arn`
//! addresses — a subscriber or an EventSource, the two resource types
//! `RevocableResourceArn` admits — that the bus owner has withdrawn it, and
//! answers that resource's own ARN; every later read of that resource reports
//! `Revoked` true (and nothing reports it before the withdrawal), the withdrawal
//! is terminal and `@idempotent` so a repeat answers the same ARN and changes
//! nothing further, revoking one resource withdraws no other, and an ARN naming
//! no stored revocable resource is refused with `ResourceNotFoundException`.
//!
//! The rows span that class rather than one recorded revocation: both revocable
//! resource types, both EventSource configuration variants (and so both ARN type
//! segments), and a subscriber in each of its two run states. The wire suite also
//! pins the SHAPE of the instants an EventSource read reports: this protocol's
//! tagged epoch date-time at the whole-second resolution the service reports an
//! instant in.
//!
//! Nothing here waits on the wall clock: the one wait is for a bus's own
//! provisioning transition, polled for the CONDITION (`ACTIVE`) under a generous
//! deadline.

use std::sync::Arc;
use std::time::{Duration, Instant};

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
use eventsv2::ops::create_subscriber::CreateSubscriber;
use eventsv2::ops::describe_event_bus::DescribeEventBus;
use eventsv2::ops::describe_event_source::DescribeEventSource;
use eventsv2::ops::describe_subscriber::DescribeSubscriber;
use eventsv2::ops::revoke_resource::{RevokeResource, RevokeResourceErrorModeled};
use eventsv2::ops::types::{
    AwsServiceEventsSourceConfiguration, CreateEventBusInput, CreateEventSourceInput,
    CreateSubscriberInput, DescribeEventBusInput, DescribeEventSourceInput,
    DescribeSubscriberInput, EventSourceConfiguration, InvokeConfiguration,
    PartnerEventsSourceConfiguration, RevokeResourceInput,
};
use eventsv2::scaffolded::Scaffolded;

/// The account segment every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The region every request below is signed for; it must reach the minted ARNs.
const REGION: &str = "us-west-2";

/// The role a subscriber's `InvokeConfiguration` names, of the modelled `RoleArn`
/// shape.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/revoke-delivery";

/// The target a subscriber invokes. Nothing is published below, so it only has to
/// be a well-formed `TargetResourceArn`.
const TARGET_ARN: &str = "arn:aws:sqs:us-west-2:000000000000:revoke-target";

/// The error shapes `RevokeResource` declares, per
/// `smithy_query(query="operation",operation="RevokeResource")` — the whole set a
/// refusal of this operation may name. `NotImplemented` is not among them.
const MODELLED_ERRORS: &[&str] = &[
    "AccessDeniedException",
    "ConcurrentModificationException",
    "InternalException",
    "InvalidInputException",
    "InvalidStateException",
    "ResourceNotFoundException",
    "ThrottlingException",
];

/// How long a poll waits for a condition the emulator owes: far above the
/// provisioning span a bus takes, so a loaded host has ample room.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long one pass of a poll sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

// ---------------------------------------------------------------------------
// The harness
// ---------------------------------------------------------------------------

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
        .uri("/service/AWSEventsV2/operation/RevokeResource")
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

    /// The service's own store, for the few assertions that are about what is
    /// STORED rather than about a response.
    fn store(&self) -> engine::ServiceState<eventsv2::ComAmazonawsEventbridgev2Awseventsv2State> {
        eventsv2::state::service_state(&*self.engine)
            .expect("the in-memory engine always has this service's store")
    }

    /// A bus called `name`, polled until it REPORTS `ACTIVE` — the state a
    /// create of a source or a subscriber requires of it. Polls the condition
    /// rather than sleeping a fixed time, because provisioning is asynchronous.
    async fn active_bus(&self, case: &str, name: &str) -> String {
        let arn = CreateEventBus::new(Arc::clone(&self.engine))
            .handle(
                CreateEventBusInput {
                    name: name.to_string(),
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: CreateEventBus must succeed, got {e:?}"))
            .event_bus_arn
            .unwrap_or_else(|| panic!("case {case:?}: CreateEventBus reported no EventBusArn"));

        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let described = DescribeEventBus::new(Arc::clone(&self.engine))
                .handle(
                    DescribeEventBusInput {
                        event_bus_arn: arn.clone(),
                    },
                    &self.ctx(),
                )
                .await
                .unwrap_or_else(|e| {
                    panic!("case {case:?}: DescribeEventBus must succeed, got {e:?}")
                });
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

    /// `RevokeResource` as the operation itself answers it.
    async fn revoke(
        &self,
        arn: &str,
    ) -> Result<Option<String>, Scaffolded<RevokeResourceErrorModeled>> {
        RevokeResource::new(Arc::clone(&self.engine))
            .handle(
                RevokeResourceInput {
                    arn: arn.to_string(),
                },
                &self.ctx(),
            )
            .await
            .map(|output| output.arn)
    }

    /// Whether the resource at `arn` reports `Revoked`, read through whichever
    /// read owns it, plus the instant that read reports as `LastModifiedTime`.
    async fn read_revoked(&self, case: &str, resource: &Resource) -> (Option<bool>, Option<i64>) {
        match resource.kind {
            Kind::EventSource => {
                let described = DescribeEventSource::new(Arc::clone(&self.engine))
                    .handle(
                        DescribeEventSourceInput {
                            event_source_arn: resource.arn.clone(),
                        },
                        &self.ctx(),
                    )
                    .await
                    .unwrap_or_else(|e| {
                        panic!("case {case:?}: DescribeEventSource must succeed, got {e:?}")
                    });
                (
                    described.revoked,
                    described.last_modified_time.map(|time| time.secs()),
                )
            }
            Kind::Subscriber => {
                let described = DescribeSubscriber::new(Arc::clone(&self.engine))
                    .handle(
                        DescribeSubscriberInput {
                            subscriber_arn: resource.arn.clone(),
                        },
                        &self.ctx(),
                    )
                    .await
                    .unwrap_or_else(|e| {
                        panic!("case {case:?}: DescribeSubscriber must succeed, got {e:?}")
                    });
                (
                    described.revoked,
                    described.last_modified_time.map(|time| time.secs()),
                )
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The rows: one per revocable resource the rule spans
// ---------------------------------------------------------------------------

/// Which registry a revocable resource lives in, which is also which read
/// reports its `Revoked` member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    EventSource,
    Subscriber,
}

/// One created revocable resource, as the table carries it.
#[derive(Debug, Clone)]
struct Resource {
    case: &'static str,
    kind: Kind,
    arn: String,
}

/// An `AwsServiceEventsConfiguration`, whose ARN type segment is `aws.service`.
fn aws_service_configuration(aws_service: &str) -> EventSourceConfiguration {
    EventSourceConfiguration::AwsServiceEventsConfiguration(AwsServiceEventsSourceConfiguration {
        aws_service: aws_service.to_string(),
        pattern: None,
        on_failure_configuration: None,
    })
}

/// A `PartnerEventsConfiguration`, whose ARN type segment is `aws.partner`.
fn partner_configuration(partner: &str, stream: &str) -> EventSourceConfiguration {
    EventSourceConfiguration::PartnerEventsConfiguration(PartnerEventsSourceConfiguration {
        partner_event_source_arn: format!(
            "arn:aws:events:{REGION}:{ACCOUNT}:event-source/aws.partner/{partner}/{stream}"
        ),
        pattern: None,
        partner_bus_kms_key_identifier: None,
        on_failure_configuration: None,
    })
}

/// Create one revocable resource of each kind the rule spans, on one ACTIVE bus,
/// and hand back the table.
async fn revocable_resources(harness: &Harness, bus: &str) -> Vec<Resource> {
    let sources: Vec<(&'static str, &'static str, EventSourceConfiguration)> = vec![
        (
            "an AWS-service event source",
            "revoke-service-source",
            aws_service_configuration("aws.s3"),
        ),
        (
            "a partner event source",
            "revoke-partner-source",
            partner_configuration("partner.example", "stream-1"),
        ),
    ];
    let subscribers: Vec<(&'static str, &'static str, Option<&'static str>)> = vec![
        ("a running subscriber", "revoke-running-subscriber", None),
        (
            "a stopped subscriber",
            "revoke-stopped-subscriber",
            Some("STOPPED"),
        ),
    ];

    let mut resources = Vec::new();
    for (case, name, configuration) in sources {
        let arn = CreateEventSource::new(Arc::clone(&harness.engine))
            .handle(
                CreateEventSourceInput {
                    name: name.to_string(),
                    event_bus_arn: bus.to_string(),
                    configuration,
                    description: None,
                    tags: None,
                    client_token: None,
                },
                &harness.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: CreateEventSource must succeed, got {e:?}"))
            .event_source_arn
            .unwrap_or_else(|| panic!("case {case:?}: CreateEventSource reported no ARN"));
        resources.push(Resource {
            case,
            kind: Kind::EventSource,
            arn,
        });
    }
    for (case, name, state) in subscribers {
        let arn = CreateSubscriber::new(Arc::clone(&harness.engine))
            .handle(
                CreateSubscriberInput {
                    name: name.to_string(),
                    event_bus_arn: bus.to_string(),
                    invoke_configuration: InvokeConfiguration {
                        role_arn: ROLE_ARN.to_string(),
                        target_arn: TARGET_ARN.to_string(),
                        ..Default::default()
                    },
                    state: state.map(str::to_string),
                    ..Default::default()
                },
                &harness.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: CreateSubscriber must succeed, got {e:?}"))
            .subscriber_arn
            .unwrap_or_else(|| panic!("case {case:?}: CreateSubscriber reported no ARN"));
        resources.push(Resource {
            case,
            kind: Kind::Subscriber,
            arn,
        });
    }
    resources
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// RULE, per row: nothing reports `Revoked` before a revocation; `RevokeResource`
/// answers the resource's own ARN and the read of that resource then reports
/// `Revoked` true; revoking one resource withdraws no other.
#[tokio::test]
async fn a_revocation_is_recorded_and_every_later_read_reports_it() {
    let harness = Harness::new();
    let bus = harness.active_bus("the revocation table", "revoke-table-bus").await;
    let resources = revocable_resources(&harness, &bus).await;

    // Before any revocation, the fact a read carries is "nobody has withdrawn
    // this": the member is absent, which is how the model words "not revoked".
    for resource in &resources {
        let (revoked, _) = harness.read_revoked(resource.case, resource).await;
        assert_eq!(
            revoked, None,
            "case {:?}: a resource nobody revoked must report no Revoked, got {revoked:?}",
            resource.case
        );
    }

    for (index, resource) in resources.iter().enumerate() {
        let case = resource.case;
        let answered = harness
            .revoke(&resource.arn)
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: RevokeResource must succeed, got {e:?}"));
        assert_eq!(
            answered.as_deref(),
            Some(resource.arn.as_str()),
            "case {case:?}: RevokeResource must answer the ARN it withdrew, got {answered:?}"
        );

        let (revoked, _) = harness.read_revoked(case, resource).await;
        assert_eq!(
            revoked,
            Some(true),
            "case {case:?}: the read of a revoked resource must report Revoked true, \
             got {revoked:?}"
        );

        // Revocation is per resource: the rows not revoked yet are untouched,
        // and the rows already revoked stay revoked.
        for (other_index, other) in resources.iter().enumerate() {
            if other_index == index {
                continue;
            }
            let expected = (other_index < index).then_some(true);
            let (revoked, _) = harness.read_revoked(other.case, other).await;
            assert_eq!(
                revoked, expected,
                "case {case:?}: after revoking it, {:?} must report Revoked {expected:?}, \
                 got {revoked:?}",
                other.case
            );
        }
    }
}

/// RULE, the `@idempotent` and terminal parts: a repeat of a revocation answers
/// the same ARN, leaves the resource revoked, and reports no second
/// modification of it.
#[tokio::test]
async fn revoking_an_already_revoked_resource_changes_nothing_further() {
    let harness = Harness::new();
    let bus = harness
        .active_bus("the idempotence table", "revoke-idempotent-bus")
        .await;
    let resources = revocable_resources(&harness, &bus).await;

    for resource in &resources {
        let case = resource.case;
        harness
            .revoke(&resource.arn)
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: RevokeResource must succeed, got {e:?}"));
        let (_, first_modified) = harness.read_revoked(case, resource).await;

        for repeat in 1..=2 {
            let answered = harness.revoke(&resource.arn).await.unwrap_or_else(|e| {
                panic!("case {case:?}: repeat {repeat} of RevokeResource must succeed, got {e:?}")
            });
            assert_eq!(
                answered.as_deref(),
                Some(resource.arn.as_str()),
                "case {case:?}: repeat {repeat} must answer the same ARN, got {answered:?}"
            );
            let (revoked, modified) = harness.read_revoked(case, resource).await;
            assert_eq!(
                revoked,
                Some(true),
                "case {case:?}: repeat {repeat} must leave the resource revoked, got {revoked:?}"
            );
            assert_eq!(
                modified, first_modified,
                "case {case:?}: repeat {repeat} modifies nothing, so LastModifiedTime must stay \
                 {first_modified:?}, got {modified:?}"
            );
        }
    }
}

/// RULE, where the withdrawal is CONSUMED: a revoked subscriber is no longer one
/// the bus delivers to, which is the stored consequence of the same flag the read
/// reports ("Delivery requires State RUNNING on a subscriber that is not
/// revoked").
#[tokio::test]
async fn a_revoked_subscriber_no_longer_delivers() {
    let harness = Harness::new();
    let bus = harness
        .active_bus("the delivery consequence", "revoke-delivery-bus")
        .await;
    let resources = revocable_resources(&harness, &bus).await;

    for resource in resources.iter().filter(|row| row.kind == Kind::Subscriber) {
        let case = resource.case;
        harness
            .revoke(&resource.arn)
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: RevokeResource must succeed, got {e:?}"));
        let store = harness.store();
        let state = store.lock();
        let subscriber = state
            .subscriber(&resource.arn)
            .unwrap_or_else(|| panic!("case {case:?}: the revoked subscriber must still be stored"));
        assert!(
            subscriber.revoked,
            "case {case:?}: the stored subscriber must carry the withdrawal, got {subscriber:?}"
        );
        assert!(
            !subscriber.delivers(),
            "case {case:?}: a revoked subscriber must not be one the bus delivers to, \
             got {subscriber:?}"
        );
    }
}

/// RULE, the refusal: an `Arn` of either revocable resource type that names no
/// stored resource is refused with `ResourceNotFoundException` (404) — one of this
/// operation's modelled errors, never a not-implemented signal.
#[tokio::test]
async fn revoking_a_resource_that_does_not_exist_is_refused() {
    let harness = Harness::new();
    let bus = harness
        .active_bus("the refusal table", "revoke-refusal-bus")
        .await;
    // One stored resource of each kind, so the refusals below are about the ARN
    // naming nothing rather than about an empty store.
    let _ = revocable_resources(&harness, &bus).await;

    // Each row is a well-formed `RevocableResourceArn` — the modelled pattern is
    // what the id segment's `[a-z0-9]{25}` comes from — that addresses nothing.
    let cases: Vec<(&str, String)> = vec![
        (
            "a subscriber ARN naming nothing",
            format!(
                "arn:aws:events:{REGION}:{ACCOUNT}:subscriber/absent/\
                 aaaaaaaaaaaaaaaaaaaaaaaaa"
            ),
        ),
        (
            "an aws.service event source ARN naming nothing",
            format!(
                "arn:aws:events:{REGION}:{ACCOUNT}:event-sourcev2/aws.service/absent/\
                 bbbbbbbbbbbbbbbbbbbbbbbbb"
            ),
        ),
        (
            "an aws.partner event source ARN naming nothing",
            format!(
                "arn:aws:events:{REGION}:{ACCOUNT}:event-sourcev2/aws.partner/absent/\
                 ccccccccccccccccccccccccc"
            ),
        ),
        (
            "a resource of another region",
            format!(
                "arn:aws:events:eu-west-1:{ACCOUNT}:subscriber/absent/\
                 ddddddddddddddddddddddddd"
            ),
        ),
    ];

    for (case, arn) in cases {
        let error = harness
            .revoke(&arn)
            .await
            .expect_err(&format!("case {case:?}: RevokeResource must be refused"));
        assert!(
            matches!(error, Scaffolded::Modeled(_)),
            "case {case:?}: expected a modelled refusal, got {error:?}"
        );
        assert!(
            MODELLED_ERRORS.contains(&error.shape_name()),
            "case {case:?}: expected one of {MODELLED_ERRORS:?}, got {:?}",
            error.shape_name()
        );
        assert_eq!(
            (error.shape_name(), error.http_status()),
            ("ResourceNotFoundException", 404),
            "case {case:?}: expected ResourceNotFoundException (404), got {error:?} at {}",
            error.http_status()
        );
    }
}

// ---------------------------------------------------------------------------
// RULE again, where a client reads it: on the wire
// ---------------------------------------------------------------------------

/// The same rule over HTTP with rpcv2Cbor bodies, so the status and the encoded
/// body are the emulator's own: a wire revocation answers 200 with the ARN, and
/// the wire read of that EventSource carries `Revoked` true.
#[tokio::test]
async fn the_wire_answer_reports_a_revoked_event_source() {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(eventsv2::EmulatedService::new(Arc::clone(&engine)));

    let bus = cbor_ok::<CreateEventBus<InMemoryEngine>>(
        &router,
        &CreateEventBusInput {
            name: "revoke-wire-bus".to_string(),
            // The create's `@idempotencyToken`, which the model leaves optional.
            client_token: Some("a-wire-bus-token".to_string()),
            ..Default::default()
        },
        "the wire revocation",
    )
    .await
    .event_bus_arn
    .expect("case \"the wire revocation\": CreateEventBus reported no EventBusArn");

    // The bus has to be ACTIVE before it takes a source, so poll for that
    // CONDITION over the wire.
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let described = cbor_ok::<DescribeEventBus<InMemoryEngine>>(
            &router,
            &DescribeEventBusInput {
                event_bus_arn: bus.clone(),
            },
            "the wire revocation",
        )
        .await;
        if described.state.as_deref() == Some("ACTIVE") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case \"the wire revocation\": the bus never became ACTIVE, last state {:?}",
            described.state
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    let source = cbor_ok::<CreateEventSource<InMemoryEngine>>(
        &router,
        &CreateEventSourceInput {
            name: "revoke-wire-source".to_string(),
            event_bus_arn: bus.clone(),
            configuration: aws_service_configuration("aws.ec2"),
            description: None,
            tags: None,
            client_token: Some("a-wire-source-token".to_string()),
        },
        "the wire revocation",
    )
    .await
    .event_source_arn
    .expect("case \"the wire revocation\": CreateEventSource reported no ARN");

    let before = cbor_ok::<DescribeEventSource<InMemoryEngine>>(
        &router,
        &DescribeEventSourceInput {
            event_source_arn: source.clone(),
        },
        "the wire revocation",
    )
    .await;
    assert_eq!(
        before.revoked, None,
        "case \"the wire revocation\": the read before the withdrawal must carry no Revoked, \
         got {:?}",
        before.revoked
    );

    let revoked = cbor_ok::<RevokeResource<InMemoryEngine>>(
        &router,
        &RevokeResourceInput { arn: source.clone() },
        "the wire revocation",
    )
    .await;
    assert_eq!(
        revoked.arn.as_deref(),
        Some(source.as_str()),
        "case \"the wire revocation\": RevokeResource must answer the ARN it withdrew, got {:?}",
        revoked.arn
    );

    let (after, after_body) = cbor_ok_with_body::<DescribeEventSource<InMemoryEngine>>(
        &router,
        &DescribeEventSourceInput {
            event_source_arn: source.clone(),
        },
        "the wire revocation",
    )
    .await;
    // The read's instants travel as this protocol's timestamps, at the whole-second
    // resolution this service reports an instant in — not as bare fractional
    // numbers an SDK would decode as a number rather than an instant.
    for (member, at) in [
        ("CreationTime", after.creation_time),
        ("LastModifiedTime", after.last_modified_time),
    ] {
        assert_whole_second_instant("the wire revocation", member, at);
        assert_tagged_timestamp("the wire revocation", member, &after_body, at);
    }
    assert_eq!(
        after.revoked,
        Some(true),
        "case \"the wire revocation\": the wire read of a revoked source must report Revoked \
         true, got {:?}",
        after.revoked
    );
    assert_eq!(
        after.creation_time, before.creation_time,
        "case \"the wire revocation\": a withdrawal does not re-create the source, so \
         CreationTime must be unchanged, got {:?}",
        after.creation_time
    );
    assert!(
        after.last_modified_time.map(|at| at.secs())
            >= before.last_modified_time.map(|at| at.secs()),
        "case \"the wire revocation\": the withdrawal is a modification of the source, so \
         LastModifiedTime must not go backwards, got {:?} after {:?}",
        after.last_modified_time,
        before.last_modified_time
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

/// The same call, requiring a 200 and decoding the typed output body.
async fn cbor_ok<O: Operation>(
    router: &MultiServiceRouter,
    input: &O::Input,
    case: &str,
) -> O::Output
where
    O::Output: DeserializableStruct,
{
    cbor_ok_with_body::<O>(router, input, case).await.0
}

/// The same call, handing back the decoded output AND the bytes it was decoded
/// from, for an assertion about the ENCODING of a member rather than its value.
async fn cbor_ok_with_body<O: Operation>(
    router: &MultiServiceRouter,
    input: &O::Input,
    case: &str,
) -> (O::Output, bytes::Bytes)
where
    O::Output: DeserializableStruct,
{
    let (status, body) = cbor_call::<O>(router, input).await;
    assert_eq!(
        status,
        200,
        "case {case:?}: {} must answer 200, got {status} with {:?}",
        O::NAME,
        body
    );
    let codec = CborCodec::default();
    let mut deserializer = codec.create_deserializer(&body);
    let output = O::Output::deserialize(&mut deserializer).unwrap_or_else(|e| {
        panic!(
            "case {case:?}: {}'s body must decode as its modelled output, got {e:?}",
            O::NAME
        )
    });
    (output, body)
}

/// Require that a reported instant is a whole-second epoch instant: the
/// resolution this service reports a modelled `Timestamp` in.
fn assert_whole_second_instant(case: &str, member: &str, at: Option<::aws_smithy_types::DateTime>) {
    let at =
        at.unwrap_or_else(|| panic!("case {case:?}: expected {member} to be reported, got None"));
    assert_eq!(
        at.subsec_nanos(),
        0,
        "case {case:?}: expected {member} to be a whole-second epoch instant, got {at:?}"
    );
    assert!(
        at.secs() > 0,
        "case {case:?}: expected {member} to be a real instant, got {at:?}"
    );
}

/// Require that `body` carries `at` as an RFC 8949 §3.4.2 tag(1) epoch date-time
/// item — the representation the rpcv2Cbor protocol gives a `Timestamp` member:
/// the tag head (major type 6, value 1) followed by a double-precision
/// epoch-seconds value.
fn assert_tagged_timestamp(
    case: &str,
    member: &str,
    body: &[u8],
    at: Option<::aws_smithy_types::DateTime>,
) {
    let at =
        at.unwrap_or_else(|| panic!("case {case:?}: expected {member} to be reported, got None"));
    let mut expected = vec![0xc1, 0xfb];
    expected.extend_from_slice(&at.as_secs_f64().to_be_bytes());
    assert!(
        body.windows(expected.len()).any(|window| window == expected),
        "case {case:?}: expected {member} as a tag(1) epoch date-time item \
         ({expected:02x?}) for {at:?}, got body {body:02x?}"
    );
}
