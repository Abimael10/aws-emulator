// SPDX-License-Identifier: CC0-1.0

//! What a `TagResource` write leaves on the resource its ARN addresses.
//!
//! RULE: a tag write MERGES the entries the request carries into the tag set
//! stored on the one resource its `ResourceArn` addresses — the operation
//! "Adds or replaces tags on an event bus, subscriber, or event source" — so an
//! entry whose key the resource already holds replaces that key's value, an
//! entry whose key it does not holds is added, a key the request does not name
//! is left exactly as it stands, and no other resource's set moves. It refuses
//! an ARN naming no stored resource with `ResourceNotFoundException`, a
//! resource whose revocation the model documents as refusing every mutating
//! operation with `InvalidStateException`, and a write that would leave the
//! resource holding more tags than a `TagMap` can report with
//! `LimitExceededException` — writing nothing in each case.
//!
//! The rows span that class rather than one recorded write: all three taggable
//! resource types (and both EventSource origin types, so both ARN type
//! segments), resources that were created tagged and untagged, and writes
//! differing in COUNT (one entry, several), in what they do to the stored set
//! (add, replace, repeat unchanged) and in the shape of their entries — a key
//! and a value at the modelled maxima of `TagKey` (len 1..128) and `TagValue`
//! (len 0..256), and the empty value `TagValue`'s minimum of 0 admits, per
//! `smithy_query(query="shape",shape="TagMap")`.
//!
//! Nothing here waits on the wall clock: each wait is for a CONDITION the
//! emulator owes — a bus reporting `ACTIVE`, and a deleted bus having
//! disappeared — polled under a generous deadline.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Instant;

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
use eventsv2::ops::list_tags_for_resource::ListTagsForResource;
use eventsv2::ops::revoke_resource::RevokeResource;
use eventsv2::ops::tag_resource::{TagResource, TagResourceErrorModeled};
use eventsv2::ops::types::{
    AwsServiceEventsSourceConfiguration, CreateEventBusInput, CreateEventSourceInput,
    CreateSubscriberInput, DeleteEventBusInput, DescribeEventBusInput, EventSourceConfiguration,
    InvokeConfiguration, ListTagsForResourceInput, PartnerEventsSourceConfiguration,
    RevokeResourceInput, TagResourceInput,
};
use eventsv2::scaffolded::Scaffolded;

// The suites of this crate share one wire harness, the region they sign for and
// the bounds a poll of a condition the emulator owes waits under, so this suite
// states none of its own.
use crate::modelled_input_refusal::{refusal, rendered, Wire, POLL_DEADLINE, POLL_INTERVAL, REGION};

/// The account segment every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The role a subscriber's `InvokeConfiguration` names, of the modelled `RoleArn`
/// shape.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/tag-write-delivery";

/// The target a subscriber invokes. Nothing is published below, so it only has
/// to be a well-formed `TargetResourceArn`.
const TARGET_ARN: &str = "arn:aws:sqs:us-west-2:000000000000:tag-write-target";

/// The error shapes `TagResource` declares, per
/// `smithy_query(query="operation",operation="TagResource")` — the whole set a
/// refusal of this operation may name. `NotImplemented` is not among them.
const MODELLED_ERRORS: &[&str] = &[
    "AccessDeniedException",
    "ConcurrentModificationException",
    "InternalException",
    "InvalidInputException",
    "InvalidStateException",
    "LimitExceededException",
    "ResourceNotFoundException",
    "ThrottlingException",
];

/// The longest `TagKey` the model admits: its `@length` is 1..128.
const MAX_TAG_KEY_LEN: usize = 128;

/// The longest `TagValue` the model admits: its `@length` is 0..256.
const MAX_TAG_VALUE_LEN: usize = 256;

/// The most tags one resource may hold: `TagMap` is `@length(max: 200)` and a
/// tag read reports a resource's whole set as one such map.
const MAX_TAGS_PER_RESOURCE: usize = 200;

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
        .uri("/service/AWSEventsV2/operation/TagResource")
        .header("authorization", authorization())
        .header("content-type", "application/cbor")
        .body(())
        .expect("the harness request is well formed")
        .into_parts();
    parts
}

/// A tag map in a comparable order, whatever order the response's own map
/// iterates in.
fn sorted(tags: HashMap<String, String>) -> BTreeMap<String, String> {
    tags.into_iter().collect()
}

/// A tag set as a request's `Tags` member.
fn as_input(tags: &BTreeMap<String, String>) -> HashMap<String, String> {
    tags.clone().into_iter().collect()
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
    async fn active_bus(&self, case: &str, name: &str, tags: &BTreeMap<String, String>) -> String {
        let arn = CreateEventBus::new(Arc::clone(&self.engine))
            .handle(
                CreateEventBusInput {
                    name: name.to_string(),
                    tags: (!tags.is_empty()).then(|| as_input(tags)),
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
    ) -> Result<
        Option<String>,
        Scaffolded<eventsv2::ops::describe_event_bus::DescribeEventBusErrorModeled>,
    > {
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

    /// `TagResource` as the operation itself answers it.
    async fn tag(
        &self,
        arn: &str,
        tags: &BTreeMap<String, String>,
    ) -> Result<(), Scaffolded<TagResourceErrorModeled>> {
        TagResource::new(Arc::clone(&self.engine))
            .handle(
                TagResourceInput {
                    resource_arn: arn.to_string(),
                    tags: as_input(tags),
                },
                &self.ctx(),
            )
            .await
            .map(|_| ())
    }

    /// `TagResource`, requiring success.
    async fn tag_ok(&self, case: &str, arn: &str, tags: &BTreeMap<String, String>) {
        self.tag(arn, tags)
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: TagResource must succeed, got {e:?}"));
    }

    /// The tag set `ListTagsForResource` reports for `arn`, requiring success:
    /// the READ is what says what the write left stored.
    async fn tags_of(&self, case: &str, arn: &str) -> BTreeMap<String, String> {
        ListTagsForResource::new(Arc::clone(&self.engine))
            .handle(
                ListTagsForResourceInput {
                    resource_arn: arn.to_string(),
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: ListTagsForResource must succeed, got {e:?}"))
            .tags
            .map(sorted)
            .unwrap_or_else(|| {
                panic!("case {case:?}: ListTagsForResource reported no Tags member at all")
            })
    }

    /// Withdraw a subscriber or an EventSource, which is the state the model
    /// documents as refusing every mutating operation on it.
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
}

// ---------------------------------------------------------------------------
// The rows: one per (resource type, create-time tag set) the rule spans
// ---------------------------------------------------------------------------

/// Which registry a taggable resource lives in — the three resource types
/// `TaggableResourceArn` admits, with an EventSource's two origin types told
/// apart because their ARN type segments differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    EventBus,
    AwsServiceEventSource,
    PartnerEventSource,
    Subscriber,
}

/// One created taggable resource the writes below are applied to.
#[derive(Debug, Clone)]
struct Row {
    case: &'static str,
    kind: Kind,
    name: &'static str,
    /// The tags the create that minted it carried, which a later write merges
    /// into rather than replaces.
    created_with: BTreeMap<String, String>,
}

/// A tag set from literal entries.
fn tags(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
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

/// The table: every taggable resource type, created tagged and untagged.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an event bus nobody tagged at create",
            kind: Kind::EventBus,
            name: "tag-write-bus-untagged",
            created_with: BTreeMap::new(),
        },
        Row {
            case: "an event bus created with tags",
            kind: Kind::EventBus,
            name: "tag-write-bus-tagged",
            created_with: tags(&[("Team", "platform"), ("env", "staging")]),
        },
        Row {
            case: "an aws.service event source created with a tag",
            kind: Kind::AwsServiceEventSource,
            name: "tag-write-service-source",
            created_with: tags(&[("env", "staging")]),
        },
        Row {
            case: "an aws.partner event source nobody tagged at create",
            kind: Kind::PartnerEventSource,
            name: "tag-write-partner-source",
            created_with: BTreeMap::new(),
        },
        Row {
            case: "a subscriber created with a tag",
            kind: Kind::Subscriber,
            name: "tag-write-subscriber-tagged",
            created_with: tags(&[("owner", "events-team")]),
        },
        Row {
            case: "a subscriber nobody tagged at create",
            kind: Kind::Subscriber,
            name: "tag-write-subscriber-untagged",
            created_with: BTreeMap::new(),
        },
    ]
}

/// Create the row's resource on `bus` and hand back its own ARN.
async fn create(harness: &Harness, bus: &str, row: &Row) -> String {
    let case = row.case;
    match row.kind {
        Kind::EventBus => harness.active_bus(case, row.name, &row.created_with).await,
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
                        tags: (!row.created_with.is_empty())
                            .then(|| as_input(&row.created_with)),
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
                    tags: (!row.created_with.is_empty()).then(|| as_input(&row.created_with)),
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
        .active_bus("the tag write table", "tag-write-host-bus", &BTreeMap::new())
        .await;
    let mut created = Vec::new();
    for row in rows() {
        let arn = create(harness, &host, &row).await;
        created.push((row, arn));
    }
    created
}

/// One write of the sequence every row is driven through, and what it does to
/// the stored set.
#[derive(Debug, Clone)]
struct Write {
    case: &'static str,
    entries: BTreeMap<String, String>,
}

/// The writes, differing in count, in what they do to the stored set (add,
/// replace, repeat unchanged) and in the shape of their entries.
fn writes() -> Vec<Write> {
    vec![
        Write {
            case: "one entry whose key the resource does not hold",
            entries: tags(&[("cost-centre", "4711")]),
        },
        Write {
            case: "several entries at once, one of them empty-valued",
            // `TagValue`'s minimum length is 0, so an empty value is a tag like
            // any other and must be stored as one rather than dropped.
            entries: tags(&[("tier", "gold"), ("marker", ""), ("spaced key", "spaced value")]),
        },
        Write {
            case: "a replacement for a key the resource already holds",
            entries: tags(&[("cost-centre", "815")]),
        },
        Write {
            case: "the same entries again, which changes nothing",
            entries: tags(&[("cost-centre", "815")]),
        },
        Write {
            case: "a key and a value at the modelled maxima",
            entries: [
                ("k".repeat(MAX_TAG_KEY_LEN), "v".repeat(MAX_TAG_VALUE_LEN)),
                // The empty-valued key written above, now given a value: the
                // same entry replaced the other way round.
                ("marker".to_string(), "now set".to_string()),
            ]
            .into_iter()
            .collect(),
        },
    ]
}

/// What the rule says the set holds after `write` is applied to `stored`: every
/// entry the write carries, and every stored entry whose key the write does not
/// name.
fn merged(
    stored: &BTreeMap<String, String>,
    write: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut expected: BTreeMap<String, String> = stored.clone();
    for (key, value) in write {
        expected.insert(key.clone(), value.clone());
    }
    expected
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// RULE, per row and per write: the write adds the keys the resource does not
/// hold, replaces the values of the keys it does, and leaves every key it does
/// not name as it stands — for every taggable resource type, created tagged and
/// untagged.
#[tokio::test]
async fn a_tag_write_adds_or_replaces_the_entries_it_carries() {
    let harness = Harness::new();
    let created = created_rows(&harness).await;

    for (row, arn) in &created {
        // What the create stored is where the merge starts.
        let mut stored = harness.tags_of(row.case, arn).await;
        assert_eq!(
            stored, row.created_with,
            "case {:?}: expected the create's tag set {:?} before any write, got {stored:?}",
            row.case, row.created_with
        );

        for write in writes() {
            let case = format!("{} / {}", row.case, write.case);
            let before = stored.clone();
            let expected = merged(&before, &write.entries);

            harness.tag_ok(&case, arn, &write.entries).await;
            stored = harness.tags_of(&case, arn).await;

            assert_eq!(
                stored, expected,
                "case {case:?}: expected the merged tag set {expected:?}, got {stored:?}"
            );
            // The untouched keys, spelled out: a key the write did not name
            // keeps the value it had, so the write merged rather than replaced
            // the whole set.
            for (key, value) in &before {
                if write.entries.contains_key(key) {
                    continue;
                }
                assert_eq!(
                    stored.get(key),
                    Some(value),
                    "case {case:?}: the key {key:?} the write did not name must still hold \
                     {value:?}, got {:?}",
                    stored.get(key)
                );
            }
            // The keys the write DID name hold exactly what it carried.
            for (key, value) in &write.entries {
                assert_eq!(
                    stored.get(key),
                    Some(value),
                    "case {case:?}: the key {key:?} the write carried must hold {value:?}, \
                     got {:?}",
                    stored.get(key)
                );
            }
        }
    }
}

/// RULE, the addressing part: a write reaches the ONE resource its ARN
/// addresses, so tagging an event bus moves neither its subscribers' nor its
/// event sources' tag sets, and tagging one of them does not move the bus's.
#[tokio::test]
async fn a_tag_write_reaches_only_the_resource_its_arn_addresses() {
    let harness = Harness::new();
    let created = created_rows(&harness).await;

    // The sets as they stand before any write, so every row can be checked
    // against its own.
    let mut expected: Vec<BTreeMap<String, String>> = Vec::new();
    for (row, arn) in &created {
        expected.push(harness.tags_of(row.case, arn).await);
    }

    for (index, (row, arn)) in created.iter().enumerate() {
        let case = format!("{} / written to alone", row.case);
        let write = tags(&[("written-to", row.name)]);
        harness.tag_ok(&case, arn, &write).await;
        expected[index] = merged(&expected[index], &write);

        for (other, (other_row, other_arn)) in created.iter().enumerate() {
            let reported = harness.tags_of(other_row.case, other_arn).await;
            assert_eq!(
                reported, expected[other],
                "case {case:?}: after it, {:?} must report {:?}, got {reported:?}",
                other_row.case, expected[other]
            );
        }
    }
}

/// RULE, the refusal: a `ResourceArn` of any taggable resource type that names
/// no stored resource is refused with `ResourceNotFoundException` (404) — one of
/// this operation's modelled errors, never a not-implemented signal.
#[tokio::test]
async fn tagging_a_resource_that_does_not_exist_is_refused() {
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
            .tag(&arn, &tags(&[("Team", "platform")]))
            .await
            .expect_err(&format!("case {case:?}: TagResource must be refused"));
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
/// no tag set to write to, so the write of it is refused exactly as a read of it
/// is, and the write does not bring it back. The wait is for the CONDITION (the
/// bus has disappeared), polled under a generous deadline.
#[tokio::test]
async fn tagging_a_deleted_bus_is_refused() {
    let case = "a deleted event bus";
    let harness = Harness::new();
    let arn = harness
        .active_bus(case, "tag-write-deleted-bus", &tags(&[("Team", "platform")]))
        .await;

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
        if harness.describe_bus(&arn).await.is_err() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the deleted bus {arn} never disappeared"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    let error = harness
        .tag(&arn, &tags(&[("env", "staging")]))
        .await
        .expect_err(&format!("case {case:?}: TagResource must be refused"));
    assert_eq!(
        (error.shape_name(), error.http_status()),
        ("ResourceNotFoundException", 404),
        "case {case:?}: expected ResourceNotFoundException (404), got {error:?} at {}",
        error.http_status()
    );
    // The refused write stored nothing, so the bus is still gone.
    assert!(
        harness.describe_bus(&arn).await.is_err(),
        "case {case:?}: the refused write must not have restored the bus {arn}"
    );
}

/// RULE, the state refusal: a resource whose revocation the model documents as
/// refusing every mutating operation on it ("Mutating operations on a revoked
/// subscriber fail with InvalidStateException, except DeleteSubscriber", and the
/// same for an EventSource) refuses a tag write with `InvalidStateException`
/// (409) and keeps the tags it holds.
#[tokio::test]
async fn tagging_a_revoked_resource_is_refused() {
    let harness = Harness::new();
    let created = created_rows(&harness).await;

    for (row, arn) in &created {
        // Only the two revocable resource types: `RevocableResourceArn` admits a
        // subscriber and an EventSource, and an event bus is not revocable at
        // all.
        if row.kind == Kind::EventBus {
            continue;
        }
        let case = format!("{} / revoked", row.case);
        let before = harness.tags_of(&case, arn).await;
        harness.revoke(&case, arn).await;

        let error = harness
            .tag(arn, &tags(&[("after", "revocation")]))
            .await
            .expect_err(&format!("case {case:?}: TagResource must be refused"));
        assert!(
            MODELLED_ERRORS.contains(&error.shape_name()),
            "case {case:?}: expected one of {MODELLED_ERRORS:?}, got {:?}",
            error.shape_name()
        );
        assert_eq!(
            (error.shape_name(), error.http_status()),
            ("InvalidStateException", 409),
            "case {case:?}: expected InvalidStateException (409), got {error:?} at {}",
            error.http_status()
        );

        let reported = harness.tags_of(&case, arn).await;
        assert_eq!(
            reported, before,
            "case {case:?}: the refused write must leave the tag set {before:?}, got {reported:?}"
        );
    }
}

/// RULE, the quota: a resource holds at most as many tags as a `TagMap` can
/// report, so a write that would leave it holding more is refused with
/// `LimitExceededException` (400) and stores nothing — while a write that only
/// replaces values of keys it already holds is served at the maximum.
#[tokio::test]
async fn a_tag_write_past_the_maximum_a_resource_holds_is_refused() {
    let case = "a resource filled to the tag maximum";
    let harness = Harness::new();
    let arn = harness
        .active_bus(case, "tag-write-quota-bus", &BTreeMap::new())
        .await;

    // Exactly the maximum, in one write.
    let full: BTreeMap<String, String> = (0..MAX_TAGS_PER_RESOURCE)
        .map(|index| (format!("key-{index:03}"), format!("value-{index:03}")))
        .collect();
    harness.tag_ok(case, &arn, &full).await;
    let reported = harness.tags_of(case, &arn).await;
    assert_eq!(
        reported.len(),
        MAX_TAGS_PER_RESOURCE,
        "case {case:?}: expected {MAX_TAGS_PER_RESOURCE} stored tags, got {}",
        reported.len()
    );

    // One key past it.
    let error = harness
        .tag(&arn, &tags(&[("one-too-many", "x")]))
        .await
        .expect_err(&format!("case {case:?}: TagResource must be refused"));
    assert!(
        MODELLED_ERRORS.contains(&error.shape_name()),
        "case {case:?}: expected one of {MODELLED_ERRORS:?}, got {:?}",
        error.shape_name()
    );
    assert_eq!(
        (error.shape_name(), error.http_status()),
        ("LimitExceededException", 400),
        "case {case:?}: expected LimitExceededException (400), got {error:?} at {}",
        error.http_status()
    );
    let reported = harness.tags_of(case, &arn).await;
    assert_eq!(
        reported, full,
        "case {case:?}: the refused write must leave the stored set untouched, got {} tags",
        reported.len()
    );

    // A write naming only keys it already holds stays at the maximum, so the
    // quota bounds the SET rather than refusing every write to a full resource.
    let replacement = tags(&[("key-000", "replaced")]);
    harness.tag_ok(case, &arn, &replacement).await;
    let reported = harness.tags_of(case, &arn).await;
    assert_eq!(
        reported.get("key-000").map(String::as_str),
        Some("replaced"),
        "case {case:?}: the replacement must be stored, got {:?}",
        reported.get("key-000")
    );
    assert_eq!(
        reported.len(),
        MAX_TAGS_PER_RESOURCE,
        "case {case:?}: expected {MAX_TAGS_PER_RESOURCE} stored tags after the replacement, got {}",
        reported.len()
    );
}

// ---------------------------------------------------------------------------
// RULE again, where a client writes it: on the wire
// ---------------------------------------------------------------------------

/// The same rule over HTTP with rpcv2Cbor bodies, driven through the crate's
/// shared wire harness, so the status and the encoded body are the emulator's
/// own: the wire write answers 200, the read that follows carries the merged
/// set, and an ARN naming nothing answers the modelled 404 rather than the 501
/// of an unimplemented operation.
#[tokio::test]
async fn the_wire_write_answers_200_and_the_read_that_follows_carries_it() {
    let case = "the wire tag write";
    let wire = Wire::new();

    let created_with = tags(&[("Team", "platform")]);
    let bus = wire
        .ok::<CreateEventBus<InMemoryEngine>>(
            case,
            &CreateEventBusInput {
                name: "tag-write-wire-bus".to_string(),
                tags: Some(as_input(&created_with)),
                // The create's `@idempotencyToken`, which the model leaves
                // optional.
                client_token: Some("a-wire-tag-write-token".to_string()),
                ..Default::default()
            },
        )
        .await
        .event_bus_arn
        .unwrap_or_else(|| panic!("case {case:?}: CreateEventBus reported no EventBusArn"));

    let write = tags(&[("env", "staging"), ("Team", "events")]);
    let (status, body) = wire
        .call::<TagResource<InMemoryEngine>>(&TagResourceInput {
            resource_arn: bus.clone(),
            tags: as_input(&write),
        })
        .await;
    assert_eq!(
        status,
        200,
        "case {case:?}: the wire write must answer 200, got {status} with {:?}",
        rendered(&body)
    );

    let expected = merged(&created_with, &write);
    let reported = wire
        .ok::<ListTagsForResource<InMemoryEngine>>(
            case,
            &ListTagsForResourceInput {
                resource_arn: bus.clone(),
            },
        )
        .await
        .tags
        .map(sorted);
    assert_eq!(
        reported,
        Some(expected.clone()),
        "case {case:?}: the wire read must carry {expected:?}, got {reported:?}"
    );

    let absent =
        format!("arn:aws:events:{REGION}:{ACCOUNT}:subscriber/absent/fffffffffffffffffffffffff");
    let (status, body) = wire
        .call::<TagResource<InMemoryEngine>>(&TagResourceInput {
            resource_arn: absent,
            tags: as_input(&write),
        })
        .await;
    assert_eq!(
        status,
        404,
        "case {case:?}: a wire write to an ARN naming nothing must answer 404, got {status} \
         with {:?}",
        rendered(&body)
    );
    let (code, _) = refusal(case, &body);
    assert_eq!(
        code, "ResourceNotFoundException",
        "case {case:?}: expected the modelled ResourceNotFoundException, got {code:?}"
    );
}
