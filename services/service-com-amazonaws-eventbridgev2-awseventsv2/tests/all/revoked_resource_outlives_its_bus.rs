// SPDX-License-Identifier: CC0-1.0

//! What a revocation leaves behind once the bus the resource was attached to is
//! gone.
//!
//! RULE: revoking a revocable resource — a subscriber or an EventSource, the two
//! types `RevocableResourceArn` admits — detaches it from the bus it was created
//! on, so the bus is released for deletion and the withdrawn record OUTLIVES that
//! deletion: once the bus itself no longer resolves, every read of the revoked
//! resource still answers it, reporting `Revoked` true and the `EventBusArn` it
//! was created on, and only the resource's OWN delete removes it, after which its
//! ARN resolves to nothing.
//!
//! That is the terminal half of what the operation documents — "Revocation is
//! terminal: there is no operation that clears it. A revoked resource refuses
//! mutating operations with InvalidStateException; delete stays available for
//! cleanup" — read where the bus is taken away under the record.
//!
//! The rows span the class rather than one recorded revocation: both revocable
//! resource types, both EventSource configuration variants (and so both ARN type
//! segments), and a subscriber in each of its two run states.
//!
//! Nothing here waits on the wall clock: the two waits are for the bus's own
//! provisioning and deletion transitions, each polled for the CONDITION under a
//! generous deadline.

use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::InMemoryEngine;
use smithy_server_runtime::{
    EmbeddedEmulator, MultiServiceRouter, Operation, OperationContext, OperationError,
};

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

use eventsv2::ops::create_event_bus::CreateEventBus;
use eventsv2::ops::create_event_source::CreateEventSource;
use eventsv2::ops::create_subscriber::CreateSubscriber;
use eventsv2::ops::delete_event_bus::DeleteEventBus;
use eventsv2::ops::delete_event_source::DeleteEventSource;
use eventsv2::ops::delete_subscriber::DeleteSubscriber;
use eventsv2::ops::describe_event_bus::DescribeEventBus;
use eventsv2::ops::describe_event_source::DescribeEventSource;
use eventsv2::ops::describe_subscriber::DescribeSubscriber;
use eventsv2::ops::list_event_sources::ListEventSources;
use eventsv2::ops::list_subscribers::ListSubscribers;
use eventsv2::ops::revoke_resource::RevokeResource;
use eventsv2::ops::types::{
    AwsServiceEventsSourceConfiguration, CreateEventBusInput, CreateEventSourceInput,
    CreateSubscriberInput, DeleteEventBusInput, DeleteEventSourceInput, DeleteSubscriberInput,
    DescribeEventBusInput, DescribeEventSourceInput, DescribeSubscriberInput,
    EventSourceConfiguration, InvokeConfiguration, ListEventSourcesInput, ListSubscribersInput,
    PartnerEventsSourceConfiguration, RevokeResourceInput,
};

use crate::modelled_input_refusal::with_client_token;

/// The account segment every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The region every request below is signed for; it must reach the minted ARNs.
const REGION: &str = "us-west-2";

/// The role a subscriber's `InvokeConfiguration` names, of the modelled `RoleArn`
/// shape.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/outlives-delivery";

/// The target a subscriber invokes. Nothing is published below, so it only has to
/// be a well-formed `TargetResourceArn`.
const TARGET_ARN: &str = "arn:aws:sqs:us-west-2:000000000000:outlives-target";

/// A page large enough to hold every resource a case creates, so a list below
/// enumerates them all in one answer.
const WHOLE_PAGE: i32 = 100;

/// How long a poll waits for a condition the emulator owes: far above the
/// provisioning and deletion spans a bus takes, so a loaded host has ample room.
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

    /// A bus called `name`, polled until it REPORTS `ACTIVE` — the state a create
    /// of a source or a subscriber requires of it. Polls the condition rather
    /// than sleeping a fixed time, because provisioning is asynchronous.
    async fn active_bus(&self, case: &str, name: &str) -> String {
        let arn = CreateEventBus::new(Arc::clone(&self.engine))
            .handle(
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

    /// `RevokeResource` against `arn`, which must succeed and answer that ARN.
    async fn revoke(&self, case: &str, arn: &str) {
        let answered = RevokeResource::new(Arc::clone(&self.engine))
            .handle(
                RevokeResourceInput {
                    arn: arn.to_string(),
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: RevokeResource must succeed, got {e:?}"))
            .arn;
        assert_eq!(
            answered.as_deref(),
            Some(arn),
            "case {case:?}: RevokeResource must answer the resource's own ARN, got {answered:?}"
        );
    }

    /// Start the bus's deletion and poll until the bus itself no longer resolves:
    /// the read of it is refused with `ResourceNotFoundException` (404), which is
    /// the condition every assertion below is made after.
    async fn delete_bus_and_wait(&self, case: &str, bus: &str) {
        DeleteEventBus::new(Arc::clone(&self.engine))
            .handle(
                DeleteEventBusInput {
                    event_bus_arn: bus.to_string(),
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| {
                panic!(
                    "case {case:?}: DeleteEventBus must succeed once every attached resource is \
                     revoked, got {e:?}"
                )
            });

        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let read = DescribeEventBus::new(Arc::clone(&self.engine))
                .handle(
                    DescribeEventBusInput {
                        event_bus_arn: bus.to_string(),
                    },
                    &self.ctx(),
                )
                .await;
            match read {
                Err(error) => {
                    assert_eq!(
                        (error.shape_name(), error.http_status()),
                        ("ResourceNotFoundException", 404),
                        "case {case:?}: a bus that has finished disappearing must read as \
                         ResourceNotFoundException (404), got {error:?} at {}",
                        error.http_status()
                    );
                    return;
                }
                Ok(described) => {
                    assert!(
                        Instant::now() < deadline,
                        "case {case:?}: the bus {bus} never disappeared, last state {:?}",
                        described.state
                    );
                }
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// `Revoked` and `EventBusArn` as the read that owns `resource` reports them,
    /// which must succeed.
    async fn read_resource(&self, resource: &Resource) -> (Option<bool>, Option<String>) {
        let case = resource.case;
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
                (described.revoked, described.event_bus_arn)
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
                (described.revoked, described.event_bus_arn)
            }
        }
    }

    /// The resource's OWN delete, which must succeed.
    async fn delete_resource(&self, resource: &Resource) {
        let case = resource.case;
        match resource.kind {
            Kind::EventSource => {
                DeleteEventSource::new(Arc::clone(&self.engine))
                    .handle(
                        DeleteEventSourceInput {
                            event_source_arn: resource.arn.clone(),
                        },
                        &self.ctx(),
                    )
                    .await
                    .unwrap_or_else(|e| {
                        panic!("case {case:?}: DeleteEventSource must succeed, got {e:?}")
                    });
            }
            Kind::Subscriber => {
                DeleteSubscriber::new(Arc::clone(&self.engine))
                    .handle(
                        DeleteSubscriberInput {
                            subscriber_arn: resource.arn.clone(),
                        },
                        &self.ctx(),
                    )
                    .await
                    .unwrap_or_else(|e| {
                        panic!("case {case:?}: DeleteSubscriber must succeed, got {e:?}")
                    });
            }
        }
    }

    /// The error shape name and HTTP status the read that owns `resource` answers,
    /// which must be a refusal.
    async fn read_refusal(&self, resource: &Resource) -> (&'static str, u16) {
        let case = resource.case;
        match resource.kind {
            Kind::EventSource => {
                let error = DescribeEventSource::new(Arc::clone(&self.engine))
                    .handle(
                        DescribeEventSourceInput {
                            event_source_arn: resource.arn.clone(),
                        },
                        &self.ctx(),
                    )
                    .await
                    .expect_err(&format!(
                        "case {case:?}: DescribeEventSource must be refused once the source is \
                         deleted"
                    ));
                (error.shape_name(), error.http_status())
            }
            Kind::Subscriber => {
                let error = DescribeSubscriber::new(Arc::clone(&self.engine))
                    .handle(
                        DescribeSubscriberInput {
                            subscriber_arn: resource.arn.clone(),
                        },
                        &self.ctx(),
                    )
                    .await
                    .expect_err(&format!(
                        "case {case:?}: DescribeSubscriber must be refused once the subscriber is \
                         deleted"
                    ));
                (error.shape_name(), error.http_status())
            }
        }
    }

    /// Every EventSource ARN the whole-account enumeration answers.
    async fn listed_event_source_arns(&self) -> Vec<String> {
        ListEventSources::new(Arc::clone(&self.engine))
            .handle(
                ListEventSourcesInput {
                    max_results: WHOLE_PAGE,
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("ListEventSources must succeed, got {e:?}"))
            .event_sources
            .unwrap_or_default()
            .into_iter()
            .filter_map(|summary| summary.event_source_arn)
            .collect()
    }

    /// Every subscriber ARN the whole-account enumeration answers.
    async fn listed_subscriber_arns(&self) -> Vec<String> {
        ListSubscribers::new(Arc::clone(&self.engine))
            .handle(
                ListSubscribersInput {
                    max_results: WHOLE_PAGE,
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("ListSubscribers must succeed, got {e:?}"))
            .subscribers
            .unwrap_or_default()
            .into_iter()
            .filter_map(|summary| summary.subscriber_arn)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// The rows: one per revocable resource the rule spans
// ---------------------------------------------------------------------------

/// Which registry a revocable resource lives in, which is also which read reports
/// its `Revoked` member and which delete removes it.
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

/// Create one revocable resource per row the rule spans, all on the one bus, and
/// hand back the table. `prefix` keeps the names of one test apart from another's.
async fn revocable_resources(harness: &Harness, bus: &str, prefix: &str) -> Vec<Resource> {
    let sources: Vec<(&'static str, &'static str, EventSourceConfiguration)> = vec![
        (
            "an AWS-service event source",
            "service-source",
            aws_service_configuration("aws.s3"),
        ),
        (
            "a partner event source",
            "partner-source",
            partner_configuration("partner.example", "stream-1"),
        ),
    ];
    let subscribers: Vec<(&'static str, &'static str, Option<&'static str>)> = vec![
        ("a running subscriber", "running-subscriber", None),
        ("a stopped subscriber", "stopped-subscriber", Some("STOPPED")),
    ];

    let mut resources = Vec::new();
    for (case, name, configuration) in sources {
        let arn = CreateEventSource::new(Arc::clone(&harness.engine))
            .handle(
                with_client_token(CreateEventSourceInput {
                    name: format!("{prefix}-{name}"),
                    event_bus_arn: bus.to_string(),
                    configuration,
                    description: None,
                    tags: None,
                    client_token: None,
                }),
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
                with_client_token(CreateSubscriberInput {
                    name: format!("{prefix}-{name}"),
                    event_bus_arn: bus.to_string(),
                    invoke_configuration: InvokeConfiguration {
                        role_arn: ROLE_ARN.to_string(),
                        target_arn: TARGET_ARN.to_string(),
                        ..Default::default()
                    },
                    state: state.map(str::to_string),
                    ..Default::default()
                }),
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

/// RULE, per row: a revoked resource outlives the deletion of the bus it was
/// created on — once that bus no longer resolves, the read of the resource still
/// answers it with `Revoked` true and the `EventBusArn` it was created on, and
/// only the resource's own delete removes it.
#[tokio::test]
async fn a_revoked_resource_outlives_the_deletion_of_its_bus() {
    let harness = Harness::new();
    let bus = harness
        .active_bus("the outliving table", "outlives-table-bus")
        .await;
    let resources = revocable_resources(&harness, &bus, "outlives").await;

    for resource in &resources {
        harness.revoke(resource.case, &resource.arn).await;
    }

    // Revoking every attached resource is what releases the bus: no child is
    // deleted below, and the delete still runs to completion.
    harness.delete_bus_and_wait("the outliving table", &bus).await;

    for resource in &resources {
        let case = resource.case;
        let (revoked, event_bus_arn) = harness.read_resource(resource).await;
        assert_eq!(
            revoked,
            Some(true),
            "case {case:?}: a revoked resource must still report Revoked true after its bus is \
             gone, got {revoked:?}"
        );
        assert_eq!(
            event_bus_arn.as_deref(),
            Some(bus.as_str()),
            "case {case:?}: the record must still report the bus it was created on, got \
             {event_bus_arn:?}"
        );
    }

    // "Delete stays available for cleanup": the record goes only when its own
    // delete removes it, and its ARN then resolves to nothing.
    for resource in &resources {
        let case = resource.case;
        harness.delete_resource(resource).await;
        let refusal = harness.read_refusal(resource).await;
        assert_eq!(
            refusal,
            ("ResourceNotFoundException", 404),
            "case {case:?}: once deleted, the ARN must read as ResourceNotFoundException (404), \
             got {refusal:?}"
        );
    }
}

/// RULE, where the surviving record is ENUMERATED: every read reports it, so the
/// listings still carry each revoked resource after its bus is gone, and stop
/// carrying one as soon as its own delete removes it.
#[tokio::test]
async fn the_listings_still_carry_a_revoked_resource_whose_bus_is_gone() {
    let harness = Harness::new();
    let bus = harness
        .active_bus("the listing table", "outlives-listing-bus")
        .await;
    let resources = revocable_resources(&harness, &bus, "listing").await;

    for resource in &resources {
        harness.revoke(resource.case, &resource.arn).await;
    }
    harness.delete_bus_and_wait("the listing table", &bus).await;

    let listed_sources = harness.listed_event_source_arns().await;
    let listed_subscribers = harness.listed_subscriber_arns().await;
    for resource in &resources {
        let case = resource.case;
        let listed = match resource.kind {
            Kind::EventSource => &listed_sources,
            Kind::Subscriber => &listed_subscribers,
        };
        assert!(
            listed.contains(&resource.arn),
            "case {case:?}: the listing must still carry {} after its bus is gone, got {listed:?}",
            resource.arn
        );
    }

    // One row at a time, so each disappearance is attributed to that row's own
    // delete rather than to the bus that is already gone.
    for resource in &resources {
        let case = resource.case;
        harness.delete_resource(resource).await;
        let listed = match resource.kind {
            Kind::EventSource => harness.listed_event_source_arns().await,
            Kind::Subscriber => harness.listed_subscriber_arns().await,
        };
        assert!(
            !listed.contains(&resource.arn),
            "case {case:?}: the listing must drop {} once its own delete removed it, got \
             {listed:?}",
            resource.arn
        );
    }
}
