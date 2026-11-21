// SPDX-License-Identifier: CC0-1.0

//! What `ListTagsForResource` reports for each resource type a tag ARN can
//! address.
//!
//! RULE: a tag read reports the tag set STORED on the one resource its
//! `ResourceArn` addresses — `TaggableResourceArn` admits an event bus, a
//! subscriber and an event source, and each carries the tags the create that
//! minted it recorded — reports it as a set that is empty for a resource nobody
//! tagged, changes nothing (a repeat reports the same set, and no other
//! resource's set moves), and refuses an ARN naming no stored resource with
//! `ResourceNotFoundException`.
//!
//! The rows span that class rather than one recorded read: all three taggable
//! resource types (and both EventSource origin types, so both ARN type
//! segments), and tag sets differing in COUNT (none, one, several) and in the
//! shape of their entries — a key and a value at the modelled maxima of
//! `TagKey` (len 1..128) and `TagValue` (len 0..256), and the empty value that
//! `TagValue`'s minimum of 0 admits, per
//! `smithy_query(query="shape",shape="TagMap")`.
//!
//! Nothing here waits on the wall clock: each wait is for a CONDITION the
//! emulator owes — a bus reporting `ACTIVE`, and a deleted bus having
//! disappeared — polled under a generous deadline.

use std::collections::{BTreeMap, HashMap};
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
use eventsv2::ops::delete_event_bus::DeleteEventBus;
use eventsv2::ops::describe_event_bus::DescribeEventBus;
use eventsv2::ops::list_tags_for_resource::{ListTagsForResource, ListTagsForResourceErrorModeled};
use eventsv2::ops::types::{
    AwsServiceEventsSourceConfiguration, CreateEventBusInput, CreateEventSourceInput,
    CreateSubscriberInput, DeleteEventBusInput, DescribeEventBusInput, EventSourceConfiguration,
    InvokeConfiguration, ListTagsForResourceInput, PartnerEventsSourceConfiguration,
};
use eventsv2::scaffolded::Scaffolded;

/// The account segment every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The region every request below is signed for; it must reach the minted ARNs.
const REGION: &str = "us-west-2";

/// The role a subscriber's `InvokeConfiguration` names, of the modelled `RoleArn`
/// shape.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/tag-delivery";

/// The target a subscriber invokes. Nothing is published below, so it only has
/// to be a well-formed `TargetResourceArn`.
const TARGET_ARN: &str = "arn:aws:sqs:us-west-2:000000000000:tag-target";

/// The error shapes `ListTagsForResource` declares, per
/// `smithy_query(query="operation",operation="ListTagsForResource")` — the whole
/// set a refusal of this operation may name. `NotImplemented` is not among them.
const MODELLED_ERRORS: &[&str] = &[
    "AccessDeniedException",
    "InternalException",
    "InvalidInputException",
    "ResourceNotFoundException",
    "ThrottlingException",
];

/// The longest `TagKey` the model admits: its `@length` is 1..128.
const MAX_TAG_KEY_LEN: usize = 128;

/// The longest `TagValue` the model admits: its `@length` is 0..256.
const MAX_TAG_VALUE_LEN: usize = 256;

/// How long a poll waits for a condition the emulator owes: far above the
/// lifecycle span a bus takes, so a loaded host has ample room.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long one pass of a poll waits before looking again.
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
        .uri("/service/AWSEventsV2/operation/ListTagsForResource")
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

    /// A bus called `name` carrying `tags`, polled until it REPORTS `ACTIVE` —
    /// the state a create of a source or a subscriber requires of it. Polls the
    /// condition rather than sleeping a fixed time, because provisioning is
    /// asynchronous.
    async fn active_bus(&self, case: &str, name: &str, tags: &TagSet) -> String {
        let arn = CreateEventBus::new(Arc::clone(&self.engine))
            .handle(
                CreateEventBusInput {
                    name: name.to_string(),
                    tags: tags.as_input(),
                    client_token: Some(format!("{name}-token")),
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
            let described = self.describe_bus(&arn).await.unwrap_or_else(|e| {
                panic!("case {case:?}: DescribeEventBus must succeed, got {e:?}")
            });
            if described.as_deref() == Some("ACTIVE") {
                return arn;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the bus {arn} never became ACTIVE, last state {described:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// The `State` `DescribeEventBus` reports for `arn`, or the refusal it
    /// answers with.
    async fn describe_bus(
        &self,
        arn: &str,
    ) -> Result<Option<String>, Scaffolded<eventsv2::ops::describe_event_bus::DescribeEventBusErrorModeled>>
    {
        DescribeEventBus::new(Arc::clone(&self.engine))
            .handle(
                DescribeEventBusInput {
                    event_bus_arn: arn.to_string(),
                },
                &self.ctx(),
            )
            .await
            .map(|described| described.state)
    }

    /// `ListTagsForResource` as the operation itself answers it, with the tag
    /// set in a form two reads can be compared in.
    async fn list_tags(
        &self,
        arn: &str,
    ) -> Result<Option<BTreeMap<String, String>>, Scaffolded<ListTagsForResourceErrorModeled>> {
        ListTagsForResource::new(Arc::clone(&self.engine))
            .handle(
                ListTagsForResourceInput {
                    resource_arn: arn.to_string(),
                },
                &self.ctx(),
            )
            .await
            .map(|output| output.tags.map(sorted))
    }

    /// The tag set `ListTagsForResource` reports for `arn`, requiring success.
    async fn tags_of(&self, case: &str, arn: &str) -> BTreeMap<String, String> {
        self.list_tags(arn)
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: ListTagsForResource must succeed, got {e:?}"))
            .unwrap_or_else(|| {
                panic!("case {case:?}: ListTagsForResource reported no Tags member at all")
            })
    }
}

/// A tag map in a comparable order, whatever order the response's own map
/// iterates in.
fn sorted(tags: HashMap<String, String>) -> BTreeMap<String, String> {
    tags.into_iter().collect()
}

// ---------------------------------------------------------------------------
// The rows: one per (resource type, tag set) the rule spans
// ---------------------------------------------------------------------------

/// Which registry a taggable resource lives in — the three resource types
/// `TaggableResourceArn` admits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    EventBus,
    AwsServiceEventSource,
    PartnerEventSource,
    Subscriber,
}

/// The tags a row's create stores, and the set a later read must report.
#[derive(Debug, Clone)]
struct TagSet(BTreeMap<String, String>);

impl TagSet {
    fn new(entries: &[(String, String)]) -> Self {
        Self(entries.iter().cloned().collect())
    }

    /// The set as a create's `Tags` member: absent when the row stores none, so
    /// the "no tags" rows exercise a create that named no tags at all.
    fn as_input(&self) -> Option<HashMap<String, String>> {
        if self.0.is_empty() {
            None
        } else {
            Some(self.0.clone().into_iter().collect())
        }
    }

    fn expected(&self) -> BTreeMap<String, String> {
        self.0.clone()
    }
}

/// One created taggable resource, as the table carries it.
#[derive(Debug, Clone)]
struct Row {
    case: &'static str,
    kind: Kind,
    name: &'static str,
    tags: TagSet,
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

/// The table: every taggable resource type, against tag sets differing in count
/// and in the shape of their entries.
fn rows() -> Vec<Row> {
    let one = [("Team".to_string(), "platform".to_string())];
    let several = [
        ("Team".to_string(), "platform".to_string()),
        ("env".to_string(), "staging".to_string()),
        // `TagValue`'s minimum length is 0, so an empty value is a tag like any
        // other and must come back as one rather than being dropped.
        ("marker".to_string(), String::new()),
    ];
    let at_the_modelled_maxima = [
        ("k".repeat(MAX_TAG_KEY_LEN), "v".repeat(MAX_TAG_VALUE_LEN)),
        ("spaced key".to_string(), "spaced value".to_string()),
    ];

    vec![
        Row {
            case: "an event bus nobody tagged",
            kind: Kind::EventBus,
            name: "tags-bus-untagged",
            tags: TagSet::new(&[]),
        },
        Row {
            case: "an event bus with one tag",
            kind: Kind::EventBus,
            name: "tags-bus-one",
            tags: TagSet::new(&one),
        },
        Row {
            case: "an event bus with several tags, one of them empty-valued",
            kind: Kind::EventBus,
            name: "tags-bus-several",
            tags: TagSet::new(&several),
        },
        Row {
            case: "an event bus tagged at the modelled key and value maxima",
            kind: Kind::EventBus,
            name: "tags-bus-maxima",
            tags: TagSet::new(&at_the_modelled_maxima),
        },
        Row {
            case: "an aws.service event source with several tags",
            kind: Kind::AwsServiceEventSource,
            name: "tags-service-source",
            tags: TagSet::new(&several),
        },
        Row {
            case: "an aws.partner event source nobody tagged",
            kind: Kind::PartnerEventSource,
            name: "tags-partner-source",
            tags: TagSet::new(&[]),
        },
        Row {
            case: "a subscriber with one tag",
            kind: Kind::Subscriber,
            name: "tags-subscriber-one",
            tags: TagSet::new(&one),
        },
        Row {
            case: "a subscriber tagged at the modelled key and value maxima",
            kind: Kind::Subscriber,
            name: "tags-subscriber-maxima",
            tags: TagSet::new(&at_the_modelled_maxima),
        },
    ]
}

/// Create the row's resource on `bus` and hand back its own ARN.
async fn create(harness: &Harness, bus: &str, row: &Row) -> String {
    let case = row.case;
    match row.kind {
        Kind::EventBus => harness.active_bus(case, row.name, &row.tags).await,
        Kind::AwsServiceEventSource | Kind::PartnerEventSource => {
            let configuration = if row.kind == Kind::AwsServiceEventSource {
                aws_service_configuration("aws.s3")
            } else {
                partner_configuration("partner.example", "stream-1")
            };
            CreateEventSource::new(Arc::clone(&harness.engine))
                .handle(
                    CreateEventSourceInput {
                        name: row.name.to_string(),
                        event_bus_arn: bus.to_string(),
                        configuration,
                        description: None,
                        tags: row.tags.as_input(),
                        client_token: Some(format!("{}-token", row.name)),
                    },
                    &harness.ctx(),
                )
                .await
                .unwrap_or_else(|e| {
                    panic!("case {case:?}: CreateEventSource must succeed, got {e:?}")
                })
                .event_source_arn
                .unwrap_or_else(|| panic!("case {case:?}: CreateEventSource reported no ARN"))
        }
        Kind::Subscriber => CreateSubscriber::new(Arc::clone(&harness.engine))
            .handle(
                CreateSubscriberInput {
                    name: row.name.to_string(),
                    event_bus_arn: bus.to_string(),
                    invoke_configuration: InvokeConfiguration {
                        role_arn: ROLE_ARN.to_string(),
                        target_arn: TARGET_ARN.to_string(),
                        ..Default::default()
                    },
                    tags: row.tags.as_input(),
                    client_token: Some(format!("{}-token", row.name)),
                    ..Default::default()
                },
                &harness.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: CreateSubscriber must succeed, got {e:?}"))
            .subscriber_arn
            .unwrap_or_else(|| panic!("case {case:?}: CreateSubscriber reported no ARN")),
    }
}

/// Every row's resource, on one ACTIVE bus of its own, paired with its ARN.
async fn created_rows(harness: &Harness) -> Vec<(Row, String)> {
    let host = harness
        .active_bus("the tag table", "tags-host-bus", &TagSet::new(&[]))
        .await;
    let mut created = Vec::new();
    for row in rows() {
        let arn = create(harness, &host, &row).await;
        created.push((row, arn));
    }
    created
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// RULE, per row: the read reports the set the create stored, for every taggable
/// resource type and every shape of tag set the table spans.
#[tokio::test]
async fn a_tag_read_reports_the_set_stored_on_the_resource_its_arn_addresses() {
    let harness = Harness::new();
    let created = created_rows(&harness).await;

    for (row, arn) in &created {
        let case = row.case;
        let reported = harness.tags_of(case, arn).await;
        assert_eq!(
            reported,
            row.tags.expected(),
            "case {case:?}: expected the stored tag set {:?}, got {reported:?}",
            row.tags.expected()
        );
    }
}

/// RULE, the reporting part: the read changes nothing a later caller sees — a
/// repeat reports the same set, and reading one resource's tags moves no other
/// resource's set.
#[tokio::test]
async fn a_tag_read_changes_nothing_a_later_caller_sees() {
    let harness = Harness::new();
    let created = created_rows(&harness).await;

    for (row, arn) in &created {
        let case = row.case;
        let first = harness.tags_of(case, arn).await;
        for repeat in 1..=2 {
            let again = harness.tags_of(case, arn).await;
            assert_eq!(
                again, first,
                "case {case:?}: repeat {repeat} of the read must report the same set {first:?}, \
                 got {again:?}"
            );
        }
        // Reading one resource's tags is not a write to any other.
        for (other, other_arn) in &created {
            let reported = harness.tags_of(other.case, other_arn).await;
            assert_eq!(
                reported,
                other.tags.expected(),
                "case {case:?}: after reading it, {:?} must still report {:?}, got {reported:?}",
                other.case,
                other.tags.expected()
            );
        }
    }
}

/// RULE, the refusal: a `ResourceArn` of any taggable resource type that names
/// no stored resource is refused with `ResourceNotFoundException` (404) — one of
/// this operation's modelled errors, never a not-implemented signal.
#[tokio::test]
async fn listing_the_tags_of_a_resource_that_does_not_exist_is_refused() {
    let harness = Harness::new();
    // One stored resource of each kind, so the refusals below are about the ARN
    // naming nothing rather than about an empty store.
    let _ = created_rows(&harness).await;

    // Each row is a well-formed `TaggableResourceArn` — the modelled pattern is
    // where the id segment's `[a-z0-9]{25}` comes from — that addresses nothing.
    let cases: Vec<(&str, String)> = vec![
        (
            "an event bus ARN naming nothing",
            format!("arn:aws:events:{REGION}:{ACCOUNT}:event-busv2/absent/aaaaaaaaaaaaaaaaaaaaaaaaa"),
        ),
        (
            "a subscriber ARN naming nothing",
            format!("arn:aws:events:{REGION}:{ACCOUNT}:subscriber/absent/bbbbbbbbbbbbbbbbbbbbbbbbb"),
        ),
        (
            "an aws.service event source ARN naming nothing",
            format!(
                "arn:aws:events:{REGION}:{ACCOUNT}:event-sourcev2/aws.service/absent/\
                 ccccccccccccccccccccccccc"
            ),
        ),
        (
            "an aws.partner event source ARN naming nothing",
            format!(
                "arn:aws:events:{REGION}:{ACCOUNT}:event-sourcev2/aws.partner/absent/\
                 ddddddddddddddddddddddddd"
            ),
        ),
        (
            "a resource of another region",
            format!("arn:aws:events:eu-west-1:{ACCOUNT}:subscriber/absent/eeeeeeeeeeeeeeeeeeeeeeeee"),
        ),
    ];

    for (case, arn) in cases {
        let error = harness
            .list_tags(&arn)
            .await
            .expect_err(&format!("case {case:?}: ListTagsForResource must be refused"));
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

/// RULE, where the resource goes away: a bus whose deletion has completed holds
/// no tags to report, so the read of it is refused exactly as every other read
/// of it is. The wait is for the CONDITION (the bus has disappeared), polled
/// under a generous deadline.
#[tokio::test]
async fn the_tags_of_a_deleted_bus_are_no_longer_reported() {
    let case = "a deleted event bus";
    let harness = Harness::new();
    let tags = TagSet::new(&[("Team".to_string(), "platform".to_string())]);
    let arn = harness.active_bus(case, "tags-deleted-bus", &tags).await;

    let reported = harness.tags_of(case, &arn).await;
    assert_eq!(
        reported,
        tags.expected(),
        "case {case:?}: expected the stored tag set {:?} before the delete, got {reported:?}",
        tags.expected()
    );

    DeleteEventBus::new(Arc::clone(&harness.engine))
        .handle(
            DeleteEventBusInput {
                event_bus_arn: arn.clone(),
            },
            &harness.ctx(),
        )
        .await
        .unwrap_or_else(|e| panic!("case {case:?}: DeleteEventBus must succeed, got {e:?}"));

    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let described = harness.describe_bus(&arn).await;
        if described.is_err() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the deleted bus {arn} never disappeared, last state {:?}",
            described.ok()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    let error = harness
        .list_tags(&arn)
        .await
        .expect_err(&format!("case {case:?}: ListTagsForResource must be refused"));
    assert_eq!(
        (error.shape_name(), error.http_status()),
        ("ResourceNotFoundException", 404),
        "case {case:?}: expected ResourceNotFoundException (404), got {error:?} at {}",
        error.http_status()
    );
}

// ---------------------------------------------------------------------------
// RULE again, where a client reads it: on the wire
// ---------------------------------------------------------------------------

/// The same rule over HTTP with rpcv2Cbor bodies, so the status and the encoded
/// body are the emulator's own: the wire read answers 200 and carries the tag
/// set the create stored, for a bus and for a subscriber.
#[tokio::test]
async fn the_wire_answer_carries_the_stored_tag_set() {
    let case = "the wire tag read";
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(eventsv2::EmulatedService::new(Arc::clone(&engine)));

    let bus_tags: BTreeMap<String, String> = [
        ("Team".to_string(), "platform".to_string()),
        ("env".to_string(), "staging".to_string()),
    ]
    .into_iter()
    .collect();

    let bus = cbor_ok::<CreateEventBus<InMemoryEngine>>(
        &router,
        &CreateEventBusInput {
            name: "tags-wire-bus".to_string(),
            tags: Some(bus_tags.clone().into_iter().collect()),
            // The create's `@idempotencyToken`, which the model leaves optional.
            client_token: Some("a-wire-bus-token".to_string()),
            ..Default::default()
        },
        case,
    )
    .await
    .event_bus_arn
    .expect("case \"the wire tag read\": CreateEventBus reported no EventBusArn");

    let reported = cbor_ok::<ListTagsForResource<InMemoryEngine>>(
        &router,
        &ListTagsForResourceInput {
            resource_arn: bus.clone(),
        },
        case,
    )
    .await
    .tags
    .map(sorted);
    assert_eq!(
        reported,
        Some(bus_tags.clone()),
        "case {case:?}: the wire read of the bus must carry {bus_tags:?}, got {reported:?}"
    );

    // The bus has to be ACTIVE before it takes a subscriber, so poll for that
    // CONDITION over the wire.
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let described = cbor_ok::<DescribeEventBus<InMemoryEngine>>(
            &router,
            &DescribeEventBusInput {
                event_bus_arn: bus.clone(),
            },
            case,
        )
        .await;
        if described.state.as_deref() == Some("ACTIVE") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the bus never became ACTIVE, last state {:?}",
            described.state
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    let subscriber_tags: BTreeMap<String, String> =
        [("owner".to_string(), "events-team".to_string())]
            .into_iter()
            .collect();
    let subscriber = cbor_ok::<CreateSubscriber<InMemoryEngine>>(
        &router,
        &CreateSubscriberInput {
            name: "tags-wire-subscriber".to_string(),
            event_bus_arn: bus.clone(),
            invoke_configuration: InvokeConfiguration {
                role_arn: ROLE_ARN.to_string(),
                target_arn: TARGET_ARN.to_string(),
                ..Default::default()
            },
            tags: Some(subscriber_tags.clone().into_iter().collect()),
            client_token: Some("a-wire-subscriber-token".to_string()),
            ..Default::default()
        },
        case,
    )
    .await
    .subscriber_arn
    .expect("case \"the wire tag read\": CreateSubscriber reported no SubscriberArn");

    let reported = cbor_ok::<ListTagsForResource<InMemoryEngine>>(
        &router,
        &ListTagsForResourceInput {
            resource_arn: subscriber.clone(),
        },
        case,
    )
    .await
    .tags
    .map(sorted);
    assert_eq!(
        reported,
        Some(subscriber_tags.clone()),
        "case {case:?}: the wire read of the subscriber must carry {subscriber_tags:?}, \
         got {reported:?}"
    );

    // The same resource type, addressed by an ARN that names nothing: a 404,
    // never the 501 of an unimplemented operation.
    let absent =
        format!("arn:aws:events:{REGION}:{ACCOUNT}:subscriber/absent/fffffffffffffffffffffffff");
    let (status, _) = cbor_call::<ListTagsForResource<InMemoryEngine>>(
        &router,
        &ListTagsForResourceInput {
            resource_arn: absent,
        },
    )
    .await;
    assert_eq!(
        status, 404,
        "case {case:?}: a wire read of an ARN naming nothing must answer 404, got {status}"
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
    O::Output::deserialize(&mut deserializer).unwrap_or_else(|e| {
        panic!(
            "case {case:?}: {}'s body must decode as its modelled output, got {e:?}",
            O::NAME
        )
    })
}
