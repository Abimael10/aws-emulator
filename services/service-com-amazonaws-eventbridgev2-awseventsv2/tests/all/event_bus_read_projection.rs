// SPDX-License-Identifier: CC0-1.0

//! What a READ of an event bus reports, over tables of buses rather than one
//! exchange.
//!
//! Three rules:
//!
//! * **An effective storage configuration.** Every bus holds a retention period —
//!   the one its create asked for, or the service default when it asked for none —
//!   so every read reports `StorageConfiguration` with that period and the
//!   `RetentionWindowStartTime` derived from it, which never precedes the bus's own
//!   creation.
//! * **A description, empty rather than absent.** A read reports the bus's
//!   free-text description, and the EMPTY string for a bus holding none, on every
//!   projection that carries the member (`DescribeEventBus` and the
//!   `ListEventBuses` summaries).
//! * **Whole-second instants.** Every `Timestamp` member a read reports carries a
//!   whole-second epoch instant, stable across reads, and travels on the rpcv2Cbor
//!   wire as an RFC 8949 tag(1) epoch date-time item.
//!
//! The `EventId` form `PutEvents` mints is the fourth member of this family and is
//! pinned beside the operation that mints it, in `put_events_entry_results`.

use std::sync::Arc;

use aws_smithy_schema::codec::{Codec, FinishSerializer};
use aws_smithy_schema::serde::{SerializableStruct, ShapeSerializer};
use aws_smithy_schema_server::{CborCodec, DeserializableStruct};
use aws_smithy_types::DateTime;
use engine::InMemoryEngine;
use smithy_server_runtime::{
    EmbeddedEmulator, MultiServiceRouter, Operation, OperationContext,
};

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;
use eventsv2::ops::create_event_bus::CreateEventBus;
use eventsv2::ops::describe_event_bus::DescribeEventBus;
use eventsv2::ops::list_event_buses::ListEventBuses;
use eventsv2::ops::types::{
    CreateEventBusInput, CreateEventBusOutput, DescribeEventBusInput, DescribeEventBusOutput,
    EventBusSummary, ListEventBusesInput, StorageConfiguration, StorageConfigurationOutput,
};

use crate::modelled_input_refusal::with_client_token;

/// The region this suite signs for.
const REGION: &str = "us-west-2";

/// The longest `Description` the model admits: the member is `@length 0..512`
/// (`smithy_query(query="shape",shape="Description")`).
const DESCRIPTION_MAX_LEN: usize = 512;

/// A SigV4 `Authorization` header naming `REGION` and this service's signing name.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{REGION}/events/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// The request metadata of a call signed for [`REGION`].
fn signed_parts() -> http::request::Parts {
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

/// One emulator over one shared engine: the typed handlers for the tables, and
/// the real router for the wire form.
struct Harness {
    engine: Arc<InMemoryEngine>,
    definition: eventsv2::ServiceDefinition<InMemoryEngine>,
    callback: EmbeddedEmulator,
    parts: http::request::Parts,
    router: MultiServiceRouter,
}

impl Harness {
    fn new() -> Self {
        let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
        let definition = eventsv2::ServiceDefinition::new(Arc::clone(&engine));
        let mut callback_router = MultiServiceRouter::new();
        callback_router.set_request_authentication(false);
        let callback = EmbeddedEmulator::from_router(callback_router);
        // The router serves the SAME engine the typed calls use, so a bus created
        // either way is readable both ways.
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(eventsv2::EmulatedService::new(Arc::clone(&engine)));
        Self {
            engine,
            definition,
            callback,
            parts: signed_parts(),
            router,
        }
    }

    fn ctx(&self) -> OperationContext<'_> {
        OperationContext::new(&self.parts, self.definition.identity(), self.callback.clone())
    }

    /// Create one bus, with the idempotency token this service requires of every create
    /// supplied when the case states none of its own
    /// (`crate::modelled_input_refusal::with_client_token`).
    async fn create(&self, case: &str, input: CreateEventBusInput) -> CreateEventBusOutput {
        CreateEventBus::new(Arc::clone(&self.engine))
            .handle(with_client_token(input), &self.ctx())
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: CreateEventBus must succeed, got {e:?}"))
    }

    async fn describe(&self, case: &str, arn: &str) -> DescribeEventBusOutput {
        DescribeEventBus::new(Arc::clone(&self.engine))
            .handle(
                DescribeEventBusInput {
                    event_bus_arn: arn.to_string(),
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: DescribeEventBus must succeed, got {e:?}"))
    }

    /// The `ListEventBuses` summary of the bus named `name`.
    async fn summary(&self, case: &str, name: &str) -> EventBusSummary {
        let listed = ListEventBuses::new(Arc::clone(&self.engine))
            .handle(
                ListEventBusesInput {
                    name_prefix: Some(name.to_string()),
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: ListEventBuses must succeed, got {e:?}"));
        listed
            .event_buses
            .unwrap_or_default()
            .into_iter()
            .find(|summary| summary.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("case {case:?}: ListEventBuses must report the bus {name:?}"))
    }

    /// Encode `input` as this service's wire protocol, dispatch it through the
    /// router, and hand back the HTTP status and the raw response body.
    async fn over_the_wire<O: Operation>(&self, case: &str, input: &O::Input) -> (u16, Vec<u8>) {
        let codec = CborCodec::default();
        let mut serializer = codec.create_serializer();
        serializer
            .write_struct(O::INPUT_SCHEMA, input as &dyn SerializableStruct)
            .unwrap_or_else(|e| {
                panic!("case {case:?}: encoding {} must succeed, got {e:?}", O::NAME)
            });
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("/service/AWSEventsV2/operation/{}", O::NAME))
            .header("authorization", authorization())
            .header("content-type", "application/cbor")
            .header("smithy-protocol", "rpc-v2-cbor")
            .body(bytes::Bytes::from(serializer.finish()))
            .unwrap_or_else(|e| panic!("case {case:?}: the request must be well formed, got {e:?}"));
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (status, response.into_body().to_vec())
    }

    /// The same call, requiring a 200 and handing back both the decoded output and
    /// the bytes it was decoded from.
    async fn wire_ok<O: Operation>(&self, case: &str, input: &O::Input) -> (O::Output, Vec<u8>)
    where
        O::Output: DeserializableStruct,
    {
        let (status, body) = self.over_the_wire::<O>(case, input).await;
        assert_eq!(
            status,
            200,
            "case {case:?}: {} must answer 200, got {status}",
            O::NAME
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
}

/// Require that `reported` carries `days` and a window start that is neither
/// absent nor earlier than the bus's own creation.
fn assert_storage(
    case: &str,
    reported: Option<&StorageConfigurationOutput>,
    days: i32,
    creation_time: Option<DateTime>,
) {
    let storage = reported.unwrap_or_else(|| {
        panic!("case {case:?}: expected a StorageConfiguration, got None")
    });
    assert_eq!(
        storage.retention_period_in_days,
        Some(days),
        "case {case:?}: expected RetentionPeriodInDays {days}, got {:?}",
        storage.retention_period_in_days
    );
    let window_start = storage.retention_window_start_time.unwrap_or_else(|| {
        panic!("case {case:?}: expected a RetentionWindowStartTime, got None")
    });
    let created = creation_time
        .unwrap_or_else(|| panic!("case {case:?}: expected a CreationTime, got None"));
    assert!(
        window_start.secs() >= created.secs(),
        "case {case:?}: expected a window start at or after the creation {created:?}, \
         got {window_start:?}"
    );
}

/// Require that every named instant is a whole-second epoch instant.
fn assert_whole_seconds(case: &str, instants: &[(&str, Option<DateTime>)]) {
    for (member, at) in instants {
        let at = at
            .unwrap_or_else(|| panic!("case {case:?}: expected {member} to be reported, got None"));
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
}

/// The bytes an RFC 8949 §3.4.2 tag(1) epoch date-time item carries for `at`: the
/// tag head (major type 6, value 1) followed by a double-precision epoch-seconds
/// value — the representation the rpcv2Cbor protocol gives a `Timestamp` member.
fn tagged_epoch_bytes(at: DateTime) -> Vec<u8> {
    let mut bytes = vec![0xc1, 0xfb];
    bytes.extend_from_slice(&at.as_secs_f64().to_be_bytes());
    bytes
}

/// Require that `body` carries `at` as a tag(1) epoch date-time item.
fn assert_tagged_timestamp(case: &str, member: &str, body: &[u8], at: Option<DateTime>) {
    let at =
        at.unwrap_or_else(|| panic!("case {case:?}: expected {member} to be reported, got None"));
    let expected = tagged_epoch_bytes(at);
    assert!(
        body.windows(expected.len()).any(|window| window == expected),
        "case {case:?}: expected {member} as a tag(1) epoch date-time item \
         ({expected:02x?}) for {at:?}, got body {body:02x?}"
    );
}

/// RULE: every event bus holds an effective retention period — the one its create
/// asked for, or the service default when it asked for none — and every read
/// reports it with the window start derived from it.
///
/// The rows span the ways a create can leave the retention period unspecified and
/// the published `RetentionPeriodInDays` range (`@range 1..365`, from
/// `smithy_query(query="shape",shape="StorageConfigurationOutput")`): both bounds
/// and a value between them.
#[tokio::test]
async fn every_bus_reports_an_effective_storage_configuration() {
    let harness = Harness::new();
    let default_days = eventsv2::state::DEFAULT_RETENTION_PERIOD_IN_DAYS;

    let rows: Vec<(&str, Option<StorageConfiguration>, i32)> = vec![
        ("no storage configuration at all", None, default_days),
        (
            "a storage configuration naming no retention period",
            Some(StorageConfiguration {
                retention_period_in_days: None,
            }),
            default_days,
        ),
        (
            "the minimum of the modelled range",
            Some(StorageConfiguration {
                retention_period_in_days: Some(1),
            }),
            1,
        ),
        (
            "a period inside the modelled range",
            Some(StorageConfiguration {
                retention_period_in_days: Some(30),
            }),
            30,
        ),
        (
            "the maximum of the modelled range",
            Some(StorageConfiguration {
                retention_period_in_days: Some(365),
            }),
            365,
        ),
    ];

    for (index, (case, requested, expected_days)) in rows.into_iter().enumerate() {
        let name = format!("storage-{index}");
        let created = harness
            .create(
                case,
                CreateEventBusInput {
                    name: name.clone(),
                    storage_configuration: requested,
                    ..Default::default()
                },
            )
            .await;
        let arn = created
            .event_bus_arn
            .clone()
            .unwrap_or_else(|| panic!("case {case:?}: expected an EventBusArn, got None"));
        assert_storage(
            case,
            created.storage_configuration.as_ref(),
            expected_days,
            created.creation_time,
        );

        let described = harness.describe(case, &arn).await;
        assert_storage(
            case,
            described.storage_configuration.as_ref(),
            expected_days,
            described.creation_time,
        );
    }
}

/// RULE: a read of a bus reports its free-text description — what the bus holds,
/// and the empty string for a bus holding none — on every read projection that
/// carries the member.
///
/// The rows span a bus with no description, one created with the empty string, and
/// descriptions at one character and at the modelled maximum length (512).
#[tokio::test]
async fn a_bus_read_reports_its_description_as_empty_rather_than_absent() {
    let harness = Harness::new();
    let longest = "d".repeat(DESCRIPTION_MAX_LEN);

    let rows: Vec<(&str, Option<String>, &str)> = vec![
        ("no description", None, ""),
        (
            "a description created as the empty string",
            Some(String::new()),
            "",
        ),
        ("a one-character description", Some("x".to_string()), "x"),
        (
            "a description carrying spaces and punctuation",
            Some("bus for the v2 probe: east".to_string()),
            "bus for the v2 probe: east",
        ),
        (
            "a description at the modelled maximum length",
            Some(longest.clone()),
            longest.as_str(),
        ),
    ];

    for (index, (case, description, expected)) in rows.into_iter().enumerate() {
        let name = format!("described-{index}");
        let created = harness
            .create(
                case,
                CreateEventBusInput {
                    name: name.clone(),
                    description: description.clone(),
                    ..Default::default()
                },
            )
            .await;
        let arn = created
            .event_bus_arn
            .clone()
            .unwrap_or_else(|| panic!("case {case:?}: expected an EventBusArn, got None"));
        // The create answers the description as SUBMITTED: a create that named
        // none carries no member at all.
        assert_eq!(
            created.description, description,
            "case {case:?}: expected CreateEventBus to answer the submitted description \
             {description:?}, got {:?}",
            created.description
        );

        let described = harness.describe(case, &arn).await;
        assert_eq!(
            described.description.as_deref(),
            Some(expected),
            "case {case:?}: expected DescribeEventBus to report {expected:?}, got {:?}",
            described.description
        );

        let summary = harness.summary(case, &name).await;
        assert_eq!(
            summary.description.as_deref(),
            Some(expected),
            "case {case:?}: expected the ListEventBuses summary to report {expected:?}, got {:?}",
            summary.description
        );
    }
}

/// RULE: every `Timestamp` a read of a bus reports is a whole-second epoch
/// instant, the same on every read, and travels on the rpcv2Cbor wire as an RFC
/// 8949 tag(1) epoch date-time item.
///
/// Checked on the create response, the describe response and the list summary of
/// the same bus — through the handlers for the value and over the wire for the
/// encoding.
#[tokio::test]
async fn every_reported_instant_is_a_whole_second_tagged_epoch_value() {
    let harness = Harness::new();
    let case = "a bus with a retention window";
    let name = "instants-bus";

    let (created, create_body) = harness
        .wire_ok::<CreateEventBus<InMemoryEngine>>(
            case,
            &CreateEventBusInput {
                name: name.to_string(),
                storage_configuration: Some(StorageConfiguration {
                    retention_period_in_days: Some(7),
                }),
                // The create's `@idempotencyToken`, which the model leaves optional.
                client_token: Some("an-instants-token".to_string()),
                ..Default::default()
            },
        )
        .await;
    let arn = created
        .event_bus_arn
        .clone()
        .unwrap_or_else(|| panic!("case {case:?}: expected an EventBusArn, got None"));
    assert_whole_seconds(case, &[("CreationTime", created.creation_time)]);
    assert_whole_seconds(
        case,
        &[(
            "StorageConfiguration.RetentionWindowStartTime",
            created
                .storage_configuration
                .as_ref()
                .and_then(|storage| storage.retention_window_start_time),
        )],
    );
    assert_tagged_timestamp(case, "CreationTime", &create_body, created.creation_time);

    let (described, describe_body) = harness
        .wire_ok::<DescribeEventBus<InMemoryEngine>>(
            case,
            &DescribeEventBusInput {
                event_bus_arn: arn.clone(),
            },
        )
        .await;
    assert_whole_seconds(
        case,
        &[
            ("CreationTime", described.creation_time),
            ("LastModifiedTime", described.last_modified_time),
            (
                "StorageConfiguration.RetentionWindowStartTime",
                described
                    .storage_configuration
                    .as_ref()
                    .and_then(|storage| storage.retention_window_start_time),
            ),
        ],
    );
    assert_tagged_timestamp(
        case,
        "CreationTime",
        &describe_body,
        described.creation_time,
    );
    assert_tagged_timestamp(
        case,
        "LastModifiedTime",
        &describe_body,
        described.last_modified_time,
    );
    assert_tagged_timestamp(
        case,
        "StorageConfiguration.RetentionWindowStartTime",
        &describe_body,
        described
            .storage_configuration
            .as_ref()
            .and_then(|storage| storage.retention_window_start_time),
    );
    // The instant a bus was created at does not move between reads.
    assert_eq!(
        described.creation_time, created.creation_time,
        "case {case:?}: expected the same CreationTime on every read, got {:?} after {:?}",
        described.creation_time, created.creation_time
    );

    // The same instants through the list projection, over the same engine.
    let summary = harness.summary(case, name).await;
    assert_whole_seconds(
        case,
        &[
            ("EventBusSummary.CreationTime", summary.creation_time),
            (
                "EventBusSummary.LastModifiedTime",
                summary.last_modified_time,
            ),
        ],
    );
    assert_eq!(
        summary.creation_time, created.creation_time,
        "case {case:?}: expected the list summary's CreationTime to match the create's {:?}, \
         got {:?}",
        created.creation_time, summary.creation_time
    );
}
