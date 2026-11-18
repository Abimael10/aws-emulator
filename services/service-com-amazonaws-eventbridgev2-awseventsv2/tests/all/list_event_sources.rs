// SPDX-License-Identifier: CC0-1.0

//! `ListEventSources`: the enumeration of the stored EventSources as summaries.
//!
//! Two harnesses, for the reasons the other suites of this crate give:
//! [`Harness`] calls the typed `Operation::handle` over one shared engine, so a
//! table can span many inputs per rule and an error's wire code can be asserted
//! directly; [`Wire`] drives the real router over HTTP with rpcv2Cbor bodies, so
//! the routing, the status and the body are the emulator's own.

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
use eventsv2::ops::list_event_sources::ListEventSources;
use eventsv2::ops::revoke_resource::RevokeResource;
use eventsv2::ops::types::{
    AwsServiceEventsSourceConfiguration, CreateEventBusInput, CreateEventSourceInput,
    DeleteEventBusInput, DeleteEventSourceInput, DescribeEventBusInput, DescribeEventSourceInput,
    EventSourceConfiguration, EventSourceSummary, ListEventSourcesInput, ListEventSourcesOutput,
    PartnerEventsSourceConfiguration, RevokeResourceInput,
};

/// The account segment every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The region the harness signs for; it must reach the minted ARNs.
const REGION: &str = "us-west-2";

/// A second region, so the suite can assert an enumeration reports the sources
/// of the region the request addresses.
const OTHER_REGION: &str = "eu-west-3";

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
        .uri("/service/AWSEventsV2/operation/ListEventSources")
        .header("authorization", authorization(region))
        .header("content-type", "application/cbor")
        .body(())
        .expect("the harness request is well formed")
        .into_parts();
    parts
}

/// An `AwsServiceEventsConfiguration`, whose `EventSourceType` is
/// `AWS_SERVICE_EVENTS`. `AwsServiceSource`'s pattern is `^aws\.[a-z0-9\-]+$`.
fn aws_service_configuration(aws_service: &str) -> EventSourceConfiguration {
    EventSourceConfiguration::AwsServiceEventsConfiguration(AwsServiceEventsSourceConfiguration {
        aws_service: aws_service.to_string(),
        pattern: None,
        on_failure_configuration: None,
    })
}

/// A `PartnerEventsConfiguration`, whose `EventSourceType` is `PARTNER_EVENTS`.
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

// ---------------------------------------------------------------------------
// The typed harness
// ---------------------------------------------------------------------------

/// One emulator over one shared engine. Every typed call names the region it is
/// signed for, so a table can span regions over one store.
struct Harness {
    engine: Arc<InMemoryEngine>,
    definition: eventsv2::ServiceDefinition<InMemoryEngine>,
    callback: EmbeddedEmulator,
    parts: HashMap<String, http::request::Parts>,
}

impl Harness {
    fn new() -> Self {
        let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
        let definition = eventsv2::ServiceDefinition::new(Arc::clone(&engine));
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        let callback = EmbeddedEmulator::from_router(router);
        let parts = [REGION, OTHER_REGION]
            .into_iter()
            .map(|region| (region.to_string(), parts_signed_for(region)))
            .collect();
        Self {
            engine,
            definition,
            callback,
            parts,
        }
    }

    fn ctx(&self, region: &str) -> OperationContext<'_> {
        let parts = self
            .parts
            .get(region)
            .unwrap_or_else(|| panic!("the harness signs for {region:?}"));
        OperationContext::new(parts, self.definition.identity(), self.callback.clone())
    }

    /// Create an event bus in `region` and poll until it reports ACTIVE — the
    /// state `CreateEventSource` requires of it. Polls for the CONDITION, under
    /// a deadline far above the provisioning span.
    async fn active_bus(&self, region: &str, name: &str) -> String {
        let arn = CreateEventBus::new(Arc::clone(&self.engine))
            .handle(
                CreateEventBusInput {
                    name: name.to_string(),
                    ..Default::default()
                },
                &self.ctx(region),
            )
            .await
            .unwrap_or_else(|e| panic!("case {name:?}: CreateEventBus must succeed, got {e:?}"))
            .event_bus_arn
            .unwrap_or_else(|| panic!("case {name:?}: CreateEventBus must report an EventBusArn"));

        let deadline = std::time::Instant::now() + POLL_DEADLINE;
        loop {
            let reported = DescribeEventBus::new(Arc::clone(&self.engine))
                .handle(
                    DescribeEventBusInput {
                        event_bus_arn: arn.clone(),
                    },
                    &self.ctx(region),
                )
                .await
                .unwrap_or_else(|e| {
                    panic!("case {name:?}: DescribeEventBus must succeed, got {e:?}")
                })
                .state;
            if reported.as_deref() == Some("ACTIVE") {
                return arn;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "case {name:?}: the bus never reported ACTIVE, last reported {reported:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Create an EventSource on `bus_arn`, handing back its `EventSourceArn`.
    async fn create_source(
        &self,
        region: &str,
        bus_arn: &str,
        name: &str,
        configuration: EventSourceConfiguration,
    ) -> String {
        CreateEventSource::new(Arc::clone(&self.engine))
            .handle(
                CreateEventSourceInput {
                    name: name.to_string(),
                    event_bus_arn: bus_arn.to_string(),
                    configuration,
                    description: None,
                    tags: None,
                    client_token: Some(format!("token-{region}-{name}")),
                },
                &self.ctx(region),
            )
            .await
            .unwrap_or_else(|e| panic!("case {name:?}: CreateEventSource must succeed, got {e:?}"))
            .event_source_arn
            .unwrap_or_else(|| {
                panic!("case {name:?}: CreateEventSource must report an EventSourceArn")
            })
    }

    async fn list(
        &self,
        region: &str,
        input: ListEventSourcesInput,
    ) -> Result<ListEventSourcesOutput, eventsv2::ops::list_event_sources::ListEventSourcesError>
    {
        ListEventSources::new(Arc::clone(&self.engine))
            .handle(input, &self.ctx(region))
            .await
    }

    /// Every summary of a single page large enough to hold the whole
    /// enumeration, in the order the service reported them.
    async fn summaries(
        &self,
        region: &str,
        input: ListEventSourcesInput,
        case: &str,
    ) -> Vec<EventSourceSummary> {
        self.list(region, input)
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: ListEventSources must succeed, got {e:?}"))
            .event_sources
            .unwrap_or_else(|| panic!("case {case:?}: ListEventSources must report EventSources"))
    }

    async fn describe_source(
        &self,
        region: &str,
        arn: &str,
        case: &str,
    ) -> eventsv2::ops::types::DescribeEventSourceOutput {
        DescribeEventSource::new(Arc::clone(&self.engine))
            .handle(
                DescribeEventSourceInput {
                    event_source_arn: arn.to_string(),
                },
                &self.ctx(region),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: DescribeEventSource must succeed, got {e:?}"))
    }

    /// Withdraw a resource, which is how a source becomes `Revoked`.
    async fn revoke(&self, region: &str, arn: &str) {
        RevokeResource::new(Arc::clone(&self.engine))
            .handle(
                RevokeResourceInput {
                    arn: arn.to_string(),
                },
                &self.ctx(region),
            )
            .await
            .unwrap_or_else(|e| panic!("case {arn:?}: RevokeResource must succeed, got {e:?}"));
    }

    async fn delete_bus(&self, region: &str, arn: &str) {
        DeleteEventBus::new(Arc::clone(&self.engine))
            .handle(
                DeleteEventBusInput {
                    event_bus_arn: arn.to_string(),
                },
                &self.ctx(region),
            )
            .await
            .unwrap_or_else(|e| panic!("case {arn:?}: DeleteEventBus must succeed, got {e:?}"));
    }

    async fn delete_source(&self, region: &str, arn: &str) {
        DeleteEventSource::new(Arc::clone(&self.engine))
            .handle(
                DeleteEventSourceInput {
                    event_source_arn: arn.to_string(),
                },
                &self.ctx(region),
            )
            .await
            .unwrap_or_else(|e| panic!("case {arn:?}: DeleteEventSource must succeed, got {e:?}"));
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

/// The names of a page of summaries, in the order they were reported.
fn names_of(summaries: &[EventSourceSummary]) -> Vec<String> {
    summaries
        .iter()
        .map(|summary| {
            summary
                .name
                .clone()
                .expect("every summary reports its Name")
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// RULE: `ListEventSources` enumerates every stored EventSource that passes its
/// filters, in name order, in pages of at most `MaxResults`, and the `NextToken`
/// of a page resumes exactly where that page stopped — so following the tokens
/// yields each matching EventSource exactly once, for ANY page size.
#[tokio::test]
async fn paging_yields_every_event_source_exactly_once_for_any_page_size() {
    let harness = Harness::new();
    let bus = harness.active_bus(REGION, "paging-bus").await;

    // Names spanning the grammar's character classes, created in an order that
    // is NOT their sorted order, so the enumeration's order is the service's.
    let names = [
        "echo", "alpha", "beta.two", "beta", "delta_1", "charlie-9", "Foxtrot",
    ];
    for (index, name) in names.iter().enumerate() {
        // Both configuration variants are represented, since the rule spans
        // them: the origin decides the reported Type, not the enumeration.
        let configuration = if index % 2 == 0 {
            aws_service_configuration("aws.s3")
        } else {
            partner_configuration("examplepartner", name)
        };
        harness
            .create_source(REGION, &bus, name, configuration)
            .await;
    }
    let mut sorted: Vec<String> = names.iter().map(|name| (*name).to_string()).collect();
    sorted.sort();

    // Page sizes spanning the published 1..100 range (`MaxResults` is
    // `@range(min: 1, max: 100)` with a default of 100): both bounds, and sizes
    // smaller than, equal to and larger than the source count. Row 0 stands for
    // a request that names no `MaxResults` at all, which must page at the
    // member's MODELLED default of 100, not at the range's minimum of 1.
    for page_size in [0_i32, 1, 2, 3, 7, 8, 100] {
        let mut seen: Vec<String> = Vec::new();
        let mut next_token = None;
        loop {
            let page = harness
                .list(
                    REGION,
                    ListEventSourcesInput {
                        max_results: page_size,
                        next_token: next_token.clone(),
                        ..Default::default()
                    },
                )
                .await
                .unwrap_or_else(|e| {
                    panic!("case {page_size}: ListEventSources must succeed, got {e:?}")
                });
            let batch = names_of(&page.event_sources.clone().unwrap_or_default());
            let allowed = if page_size <= 0 {
                100
            } else {
                usize::try_from(page_size).unwrap_or(100)
            };
            assert!(
                batch.len() <= allowed,
                "case {page_size}: a page must hold at most {allowed} summaries, got {batch:?}"
            );
            if page_size <= 0 {
                assert_eq!(
                    batch.len(),
                    names.len(),
                    "case {page_size}: an absent MaxResults must page at the modelled default \
                     of 100, got {batch:?}"
                );
            }
            seen.extend(batch);
            next_token = page.next_token;
            if next_token.is_none() {
                break;
            }
            assert!(
                seen.len() <= names.len(),
                "case {page_size}: paging must terminate, saw {seen:?}"
            );
        }
        assert_eq!(
            seen, sorted,
            "case {page_size}: paging must yield every EventSource once, in name order, \
             got {seen:?}"
        );
    }
}

/// One row of the filter table: the bus scope and name prefix a request names,
/// and the names the enumeration must report for them.
type FilterCase = (
    &'static str,
    Option<String>,
    Option<&'static str>,
    Vec<&'static str>,
);

/// RULE: `NamePrefix` selects exactly the EventSources whose name starts with
/// it, and `EventBusArn` scopes the enumeration to the sources forwarding onto
/// that one bus; with neither, the enumeration spans every source of the region
/// the request addresses.
#[tokio::test]
async fn name_prefix_and_bus_scope_select_exactly_the_matching_sources() {
    let harness = Harness::new();
    let first = harness.active_bus(REGION, "scope-bus-one").await;
    let second = harness.active_bus(REGION, "scope-bus-two").await;

    for (bus, name) in [
        (&first, "alpha"),
        (&first, "alpha.two"),
        (&first, "bravo"),
        (&second, "alpha.three"),
        (&second, "charlie"),
    ] {
        harness
            .create_source(REGION, bus, name, aws_service_configuration("aws.s3"))
            .await;
    }

    // Each row is one filter combination; the expectation is the matching
    // names, in the order the enumeration reports.
    let cases: Vec<FilterCase> = vec![
        (
            "no filter",
            None,
            None,
            vec!["alpha", "alpha.three", "alpha.two", "bravo", "charlie"],
        ),
        (
            "prefix spanning both buses",
            None,
            Some("alpha"),
            vec!["alpha", "alpha.three", "alpha.two"],
        ),
        ("prefix matching nothing", None, Some("zulu"), vec![]),
        (
            "one bus",
            Some(first.clone()),
            None,
            vec!["alpha", "alpha.two", "bravo"],
        ),
        (
            "the other bus",
            Some(second.clone()),
            None,
            vec!["alpha.three", "charlie"],
        ),
        (
            "one bus and a prefix",
            Some(first.clone()),
            Some("alpha."),
            vec!["alpha.two"],
        ),
        (
            "one bus and a prefix nothing on it matches",
            Some(second.clone()),
            Some("bravo"),
            vec![],
        ),
    ];

    for (case, event_bus_arn, name_prefix, expected) in cases {
        let got = names_of(
            &harness
                .summaries(
                    REGION,
                    ListEventSourcesInput {
                        event_bus_arn,
                        name_prefix: name_prefix.map(str::to_string),
                        max_results: 100,
                        next_token: None,
                    },
                    case,
                )
                .await,
        );
        let expected: Vec<String> = expected.iter().map(|name| (*name).to_string()).collect();
        assert_eq!(
            got, expected,
            "case {case:?}: expected {expected:?}, got {got:?}"
        );
    }
}

/// RULE: every member of a summary is read back out of the stored EventSource —
/// the same ARN, name, bus, state, timestamps and owning account
/// `DescribeEventSource` reports — its `Type` is the one its configuration's
/// origin determines, and `Revoked` is present only once the bus owner has
/// withdrawn it.
#[tokio::test]
async fn a_summary_reports_the_stored_event_source() {
    let harness = Harness::new();
    let bus = harness.active_bus(REGION, "projection-bus").await;

    // One row per origin kind, which is what decides the reported `Type`
    // (`EventSourceType`: AWS_SERVICE_EVENTS | PARTNER_EVENTS), and one revoked
    // row per kind, which is what decides whether `Revoked` is reported.
    let cases: Vec<(&str, &str, EventSourceConfiguration, &str, bool)> = vec![
        (
            "aws service events",
            "service-source",
            aws_service_configuration("aws.s3"),
            "AWS_SERVICE_EVENTS",
            false,
        ),
        (
            "partner events",
            "partner-source",
            partner_configuration("examplepartner", "stream"),
            "PARTNER_EVENTS",
            false,
        ),
        (
            "aws service events, revoked",
            "revoked-service-source",
            aws_service_configuration("aws.ec2"),
            "AWS_SERVICE_EVENTS",
            true,
        ),
        (
            "partner events, revoked",
            "revoked-partner-source",
            partner_configuration("otherpartner", "stream"),
            "PARTNER_EVENTS",
            true,
        ),
    ];

    for (case, name, configuration, expected_type, revoke) in &cases {
        let arn = harness
            .create_source(REGION, &bus, name, configuration.clone())
            .await;
        if *revoke {
            harness.revoke(REGION, &arn).await;
        }

        let summaries = harness
            .summaries(
                REGION,
                ListEventSourcesInput {
                    name_prefix: Some((*name).to_string()),
                    max_results: 100,
                    ..Default::default()
                },
                case,
            )
            .await;
        let summary = summaries
            .iter()
            .find(|summary| summary.name.as_deref() == Some(*name))
            .unwrap_or_else(|| {
                panic!("case {case:?}: the enumeration must report {name:?}, got {summaries:?}")
            });

        // The read-back resource is what the summary must agree with: the
        // summary is a projection of the stored EventSource, not of the request.
        let described = harness.describe_source(REGION, &arn, case).await;
        assert_eq!(
            summary.event_source_arn.as_deref(),
            Some(arn.as_str()),
            "case {case:?}: expected EventSourceArn {arn:?}, got {:?}",
            summary.event_source_arn
        );
        assert_eq!(
            summary.event_bus_arn, described.event_bus_arn,
            "case {case:?}: expected EventBusArn {:?}, got {:?}",
            described.event_bus_arn, summary.event_bus_arn
        );
        assert_eq!(
            summary.state, described.state,
            "case {case:?}: expected State {:?}, got {:?}",
            described.state, summary.state
        );
        assert_eq!(
            summary.creation_time, described.creation_time,
            "case {case:?}: expected CreationTime {:?}, got {:?}",
            described.creation_time, summary.creation_time
        );
        assert_eq!(
            summary.last_modified_time, described.last_modified_time,
            "case {case:?}: expected LastModifiedTime {:?}, got {:?}",
            described.last_modified_time, summary.last_modified_time
        );
        assert_eq!(
            summary.revoked, described.revoked,
            "case {case:?}: expected Revoked {:?}, got {:?}",
            described.revoked, summary.revoked
        );
        assert_eq!(
            summary.revoked,
            revoke.then_some(true),
            "case {case:?}: Revoked is reported only once the source is revoked, got {:?}",
            summary.revoked
        );
        assert_eq!(
            summary.r#type.as_deref(),
            Some(*expected_type),
            "case {case:?}: expected Type {expected_type:?}, got {:?}",
            summary.r#type
        );
        assert_eq!(
            summary.event_source_account_id.as_deref(),
            Some(ACCOUNT),
            "case {case:?}: expected EventSourceAccountId {ACCOUNT:?}, got {:?}",
            summary.event_source_account_id
        );
    }

    // Reading the enumeration changes nothing a later caller sees: the same
    // request answers the same way, and every source is still there.
    let first = names_of(
        &harness
            .summaries(
                REGION,
                ListEventSourcesInput {
                    max_results: 100,
                    ..Default::default()
                },
                "repeat, first read",
            )
            .await,
    );
    let again = names_of(
        &harness
            .summaries(
                REGION,
                ListEventSourcesInput {
                    max_results: 100,
                    ..Default::default()
                },
                "repeat, second read",
            )
            .await,
    );
    assert_eq!(
        first, again,
        "case \"repeat\": an enumeration must not change what a later caller sees, \
         got {first:?} then {again:?}"
    );
    assert_eq!(
        first.len(),
        cases.len(),
        "case \"repeat\": every created EventSource must still be enumerated, got {first:?}"
    );
}

/// RULE: a deleted EventSource is no longer enumerated — the enumeration reports
/// the sources the store holds, not the ones it once held.
#[tokio::test]
async fn a_deleted_event_source_leaves_the_enumeration() {
    let harness = Harness::new();
    let bus = harness.active_bus(REGION, "deletion-bus").await;
    let kept = harness
        .create_source(REGION, &bus, "kept", aws_service_configuration("aws.s3"))
        .await;
    let doomed = harness
        .create_source(
            REGION,
            &bus,
            "doomed",
            partner_configuration("examplepartner", "stream"),
        )
        .await;

    harness.delete_source(REGION, &doomed).await;

    let got = names_of(
        &harness
            .summaries(
                REGION,
                ListEventSourcesInput {
                    max_results: 100,
                    ..Default::default()
                },
                "after the delete",
            )
            .await,
    );
    assert_eq!(
        got,
        vec!["kept".to_string()],
        "case \"after the delete\": only the surviving EventSource must be enumerated, got {got:?}"
    );
    assert!(
        !kept.is_empty(),
        "case \"after the delete\": the surviving source must have an ARN"
    );
}

/// RULE: an EventSource lives in the region of the bus it forwards onto, so an
/// enumeration reports the sources of the region the request addresses and no
/// other.
#[tokio::test]
async fn an_enumeration_reports_only_the_addressed_region() {
    let harness = Harness::new();
    let home = harness.active_bus(REGION, "region-bus").await;
    let away = harness.active_bus(OTHER_REGION, "region-bus").await;
    harness
        .create_source(REGION, &home, "home-source", aws_service_configuration("aws.s3"))
        .await;
    harness
        .create_source(
            OTHER_REGION,
            &away,
            "away-source",
            aws_service_configuration("aws.s3"),
        )
        .await;

    for (case, region, expected) in [
        ("the signed region", REGION, vec!["home-source"]),
        ("the other region", OTHER_REGION, vec!["away-source"]),
    ] {
        let got = names_of(
            &harness
                .summaries(
                    region,
                    ListEventSourcesInput {
                        max_results: 100,
                        ..Default::default()
                    },
                    case,
                )
                .await,
        );
        let expected: Vec<String> = expected.iter().map(|name| (*name).to_string()).collect();
        assert_eq!(
            got, expected,
            "case {case:?}: expected {expected:?}, got {got:?}"
        );
    }
}

/// RULE: a bus scope has to resolve — an `EventBusArn` the store holds no bus at
/// is `ResourceNotFoundException` (404), worded for the bus this service minted
/// when it minted one and with the error class's fixed text when the ARN names
/// an identifier it never handed out.
#[tokio::test]
async fn a_bus_scope_that_resolves_to_no_bus_is_refused() {
    let harness = Harness::new();
    let live = harness.active_bus(REGION, "refusal-bus").await;
    harness
        .create_source(REGION, &live, "a-source", aws_service_configuration("aws.s3"))
        .await;

    // An ARN of the modelled form whose id segment this service never handed
    // out addresses no resource of this service at all.
    let unminted = format!(
        "arn:aws:events:{REGION}:{ACCOUNT}:event-busv2/absent-bus/aaaaaaaaaaaaaaaaaaaaaaaaa"
    );
    // A bus this store DID mint and has since retired, which is the other side
    // of that distinction.
    let retired = harness.active_bus(REGION, "retired-bus").await;
    harness.delete_bus(REGION, &retired).await;
    // Deletion is asynchronous, so this polls for the CONDITION — the bus being
    // gone — under a deadline far above the span it takes.
    let deadline = std::time::Instant::now() + POLL_DEADLINE;
    loop {
        let answer = harness
            .list(
                REGION,
                ListEventSourcesInput {
                    event_bus_arn: Some(retired.clone()),
                    max_results: 100,
                    ..Default::default()
                },
            )
            .await;
        if answer.is_err() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case \"a retired bus\": the deleted bus never disappeared"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    for (case, bus_arn, expected_message) in [
        (
            "an identifier this service never minted",
            unminted.as_str(),
            eventsv2::state::RESOURCE_NOT_FOUND_MESSAGE.to_string(),
        ),
        (
            "a bus this service minted and has retired",
            retired.as_str(),
            "Event bus not found: retired-bus".to_string(),
        ),
    ] {
        let error = harness
            .list(
                REGION,
                ListEventSourcesInput {
                    event_bus_arn: Some(bus_arn.to_string()),
                    max_results: 100,
                    ..Default::default()
                },
            )
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: ListEventSources must be refused"));
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
        let reported = format!("{error:?}");
        assert!(
            reported.contains(&expected_message),
            "case {case:?}: expected the refusal to say {expected_message:?}, got {reported:?}"
        );
    }
}

/// RULE: a `NextToken` this service did not issue for this enumeration is a bad
/// request (400), not a page: the token is opaque and only the service mints it.
#[tokio::test]
async fn a_next_token_this_service_did_not_issue_is_refused() {
    let harness = Harness::new();
    let bus = harness.active_bus(REGION, "token-bus").await;
    harness
        .create_source(REGION, &bus, "only-source", aws_service_configuration("aws.s3"))
        .await;

    for (case, token) in [
        ("not hex at all", "not-a-token"),
        ("an odd number of hex digits", "abc"),
        ("hex carrying no cursor separator", "6162"),
    ] {
        let error = harness
            .list(
                REGION,
                ListEventSourcesInput {
                    next_token: Some(token.to_string()),
                    max_results: 100,
                    ..Default::default()
                },
            )
            .await
            .err()
            .unwrap_or_else(|| {
                panic!("case {case:?}: a token this service did not issue must be refused")
            });
        assert_eq!(
            error.http_status(),
            400,
            "case {case:?}: expected HTTP 400, got {}",
            error.http_status()
        );
    }
}

/// RULE: the same enumeration over the wire — the router answers 200 with the
/// modelled body for a listing, and 404 for a bus scope that resolves to no bus.
#[tokio::test]
async fn the_enumeration_answers_over_the_wire() {
    let wire = Wire::new();

    let bus = wire
        .ok::<CreateEventBus<InMemoryEngine>>(
            &CreateEventBusInput {
                name: "wire-listing-bus".to_string(),
                client_token: Some("wire-listing-bus-token".to_string()),
                ..Default::default()
            },
            "create the bus",
        )
        .await
        .event_bus_arn
        .expect("CreateEventBus must report an EventBusArn");

    // Provisioning is asynchronous, so this polls for the CONDITION — the bus
    // reporting ACTIVE — under a deadline far above the span it takes.
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
            "case \"wire\": the bus never reported ACTIVE, last reported {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    for name in ["wire-two", "wire-one"] {
        wire.ok::<CreateEventSource<InMemoryEngine>>(
            &CreateEventSourceInput {
                name: name.to_string(),
                event_bus_arn: bus.clone(),
                configuration: aws_service_configuration("aws.s3"),
                description: None,
                tags: None,
                client_token: Some(format!("wire-{name}")),
            },
            name,
        )
        .await;
    }

    let listed = wire
        .ok::<ListEventSources<InMemoryEngine>>(
            &ListEventSourcesInput {
                event_bus_arn: Some(bus.clone()),
                max_results: 1,
                ..Default::default()
            },
            "first page over the wire",
        )
        .await;
    let first = names_of(&listed.event_sources.unwrap_or_default());
    assert_eq!(
        first,
        vec!["wire-one".to_string()],
        "case \"first page over the wire\": expected the first name in order, got {first:?}"
    );
    let token = listed.next_token.unwrap_or_else(|| {
        panic!("case \"first page over the wire\": a page that stops short must carry a NextToken")
    });

    let listed = wire
        .ok::<ListEventSources<InMemoryEngine>>(
            &ListEventSourcesInput {
                event_bus_arn: Some(bus.clone()),
                max_results: 1,
                next_token: Some(token),
                ..Default::default()
            },
            "second page over the wire",
        )
        .await;
    let second = names_of(&listed.event_sources.unwrap_or_default());
    assert_eq!(
        second,
        vec!["wire-two".to_string()],
        "case \"second page over the wire\": the token must resume where the page stopped, \
         got {second:?}"
    );
    assert!(
        listed.next_token.is_none(),
        "case \"second page over the wire\": an exhausted enumeration must issue no token, \
         got {:?}",
        listed.next_token
    );

    let absent =
        format!("arn:aws:events:{REGION}:{ACCOUNT}:event-busv2/absent-bus/aaaaaaaaaaaaaaaaaaaaaaaaa");
    let (status, body) = wire
        .call::<ListEventSources<InMemoryEngine>>(&ListEventSourcesInput {
            event_bus_arn: Some(absent),
            max_results: 100,
            ..Default::default()
        })
        .await;
    assert_eq!(
        status, 404,
        "case \"absent bus over the wire\": expected HTTP 404, got {status} with body {body:?}"
    );
}
