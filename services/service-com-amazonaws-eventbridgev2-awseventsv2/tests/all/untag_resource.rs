// SPDX-License-Identifier: CC0-1.0

//! What an `UntagResource` call leaves on the resource its ARN addresses.
//!
//! RULE: a tag removal drops exactly the keys the request names from the tag
//! set stored on the one resource its `ResourceArn` addresses — the operation
//! "Removes tags from an event bus, subscriber, or event source" — so a key the
//! request names is gone whatever value it held, a key it does not name keeps
//! the value it had, a named key the resource does not hold leaves the set as it
//! stands (the operation is modelled `@idempotent`), and no other resource's set
//! moves. It refuses an ARN naming no stored resource with
//! `ResourceNotFoundException` and a resource whose revocation the model
//! documents as refusing every mutating operation with `InvalidStateException`,
//! removing nothing in each case.
//!
//! The rows span that class rather than one recorded removal: all three taggable
//! resource types (and both EventSource origin types, so both ARN type
//! segments), and removals differing in COUNT (one key, several, every key the
//! resource holds, and the `TagKeyList` maximum of 200 at once), in what they do
//! to the stored set (drop, drop nothing, repeat a drop already made) and in the
//! shape of the keys they name — a key at `TagKey`'s modelled maximum length, a
//! key holding inner whitespace, and a key whose stored value is the empty one
//! `TagValue`'s minimum of 0 admits, per
//! `smithy_query(query="shape",shape="TagKeyList,TagKey,TagMap")`.
//!
//! Nothing here waits on the wall clock: each wait is for a CONDITION the
//! emulator owes — a bus reporting `ACTIVE`, and a deleted bus having
//! disappeared — polled under a generous deadline.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

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
use eventsv2::ops::tag_resource::TagResource;
use eventsv2::ops::types::{
    AwsServiceEventsSourceConfiguration, CreateEventBusInput, CreateEventSourceInput,
    CreateSubscriberInput, DeleteEventBusInput, DescribeEventBusInput, EventSourceConfiguration,
    InvokeConfiguration, ListTagsForResourceInput, PartnerEventsSourceConfiguration,
    RevokeResourceInput, TagResourceInput, UntagResourceInput,
};
use eventsv2::ops::untag_resource::{UntagResource, UntagResourceErrorModeled};
use eventsv2::scaffolded::Scaffolded;

// The suites of this crate share one wire harness, the region they sign for and
// the bounds a poll of a condition the emulator owes waits under, so this suite
// states none of its own.
use crate::modelled_input_refusal::{refusal, rendered, Wire, POLL_DEADLINE, POLL_INTERVAL, REGION};

/// The account segment every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The role a subscriber's `InvokeConfiguration` names, of the modelled `RoleArn`
/// shape.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/untag-delivery";

/// The target a subscriber invokes. Nothing is published below, so it only has
/// to be a well-formed `TargetResourceArn`.
const TARGET_ARN: &str = "arn:aws:sqs:us-west-2:000000000000:untag-target";

/// The error shapes `UntagResource` declares, per
/// `smithy_query(query="operation",operation="UntagResource")` — the whole set a
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

/// The longest `TagKey` the model admits: its `@length` is 1..128.
const MAX_TAG_KEY_LEN: usize = 128;

/// The longest `TagValue` the model admits: its `@length` is 0..256.
const MAX_TAG_VALUE_LEN: usize = 256;

/// The most keys one removal names: `TagKeyList` is `@length(max: 200)`, which is
/// also the most tags a resource holds, so one call can name every key it has.
const MAX_TAG_KEYS: usize = 200;

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
        .uri("/service/AWSEventsV2/operation/UntagResource")
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

/// A tag set from literal entries.
fn tags(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
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
                CreateEventBusInput {
                    name: name.to_string(),
                    client_token: Some(format!("{name}-token")),
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: CreateEventBus must succeed, got {e:?}"))
            .event_bus_arn
            .unwrap_or_else(|| panic!("case {case:?}: CreateEventBus reported no EventBusArn"));

        let deadline = std::time::Instant::now() + POLL_DEADLINE;
        loop {
            let described = self.describe_bus(&arn).await.unwrap_or_else(|e| {
                panic!("case {case:?}: DescribeEventBus must succeed, got {e:?}")
            });
            if described.as_deref() == Some("ACTIVE") {
                return arn;
            }
            assert!(
                std::time::Instant::now() < deadline,
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

    /// `TagResource`, requiring success: how a resource is given the set the
    /// removals below take keys out of.
    async fn tag_ok(&self, case: &str, arn: &str, entries: &BTreeMap<String, String>) {
        TagResource::new(Arc::clone(&self.engine))
            .handle(
                TagResourceInput {
                    resource_arn: arn.to_string(),
                    tags: as_input(entries),
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: TagResource must succeed, got {e:?}"));
    }

    /// `UntagResource` as the operation itself answers it.
    async fn untag(
        &self,
        arn: &str,
        keys: &[String],
    ) -> Result<(), Scaffolded<UntagResourceErrorModeled>> {
        UntagResource::new(Arc::clone(&self.engine))
            .handle(
                UntagResourceInput {
                    resource_arn: arn.to_string(),
                    tag_keys: keys.to_vec(),
                },
                &self.ctx(),
            )
            .await
            .map(|_| ())
    }

    /// `UntagResource`, requiring success.
    async fn untag_ok(&self, case: &str, arn: &str, keys: &[String]) {
        self.untag(arn, keys)
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: UntagResource must succeed, got {e:?}"));
    }

    /// The tag set `ListTagsForResource` reports for `arn`, requiring success:
    /// the READ is what says what the removal left stored.
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
// The rows: one per resource type the rule spans
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

/// One created taggable resource the removals below are applied to.
#[derive(Debug, Clone)]
struct Row {
    case: &'static str,
    kind: Kind,
    name: &'static str,
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

/// The table: every taggable resource type.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an event bus",
            kind: Kind::EventBus,
            name: "untag-bus",
        },
        Row {
            case: "an aws.service event source",
            kind: Kind::AwsServiceEventSource,
            name: "untag-service-source",
        },
        Row {
            case: "an aws.partner event source",
            kind: Kind::PartnerEventSource,
            name: "untag-partner-source",
        },
        Row {
            case: "a subscriber",
            kind: Kind::Subscriber,
            name: "untag-subscriber",
        },
    ]
}

/// Create the row's resource on `bus` and hand back its own ARN.
async fn create(harness: &Harness, bus: &str, row: &Row) -> String {
    let case = row.case;
    match row.kind {
        Kind::EventBus => harness.active_bus(case, row.name).await,
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
                        tags: None,
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
                    tags: None,
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
        .active_bus("the tag removal table", "untag-host-bus")
        .await;
    let mut created = Vec::new();
    for row in rows() {
        let arn = create(harness, &host, &row).await;
        created.push((row, arn));
    }
    created
}

/// The set every row is given before the removals are applied to it: entries
/// whose keys and values span what the modelled shapes admit — a key at
/// `TagKey`'s maximum paired with a value at `TagValue`'s, a key holding inner
/// whitespace, and the empty value `TagValue`'s minimum of 0 admits.
fn base_tags() -> BTreeMap<String, String> {
    let mut set = tags(&[
        ("Team", "platform"),
        ("env", "staging"),
        ("marker", ""),
        ("cost centre", "4711"),
    ]);
    set.insert("k".repeat(MAX_TAG_KEY_LEN), "v".repeat(MAX_TAG_VALUE_LEN));
    set
}

/// One removal of the sequence every row is driven through.
#[derive(Debug, Clone)]
struct Removal {
    case: &'static str,
    keys: Vec<String>,
}

/// A removal naming `keys`.
fn removal(case: &'static str, keys: &[&str]) -> Removal {
    Removal {
        case,
        keys: keys.iter().map(|key| (*key).to_string()).collect(),
    }
}

/// The removals, differing in count, in what they do to the stored set (drop,
/// drop nothing, repeat a drop already made) and in the shape of the keys they
/// name.
fn removals() -> Vec<Removal> {
    vec![
        removal("one key the resource holds", &["env"]),
        removal(
            "the same key again, which changes nothing",
            &["env"],
        ),
        removal(
            "a key the resource never held, which changes nothing",
            &["never-stored"],
        ),
        removal(
            "several keys at once, one of them empty-valued and one not held",
            &["marker", "cost centre", "never-stored"],
        ),
        Removal {
            case: "a key at the modelled maximum length",
            keys: vec!["k".repeat(MAX_TAG_KEY_LEN)],
        },
        removal("every key still stored", &["Team"]),
    ]
}

/// What the rule says the set holds after the keys in `removal` are taken out of
/// `stored`: every stored entry whose key the removal does not name.
fn without(stored: &BTreeMap<String, String>, keys: &[String]) -> BTreeMap<String, String> {
    stored
        .iter()
        .filter(|(key, _)| !keys.contains(key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// RULE, per row and per removal: the call drops exactly the keys it names and
/// leaves every other key as it stands — for every taggable resource type, and
/// whether or not the resource holds the key named.
#[tokio::test]
async fn a_tag_removal_drops_exactly_the_keys_it_names() {
    let harness = Harness::new();
    let created = created_rows(&harness).await;

    for (row, arn) in &created {
        harness.tag_ok(row.case, arn, &base_tags()).await;
        let mut stored = harness.tags_of(row.case, arn).await;
        assert_eq!(
            stored,
            base_tags(),
            "case {:?}: expected the written tag set {:?} before any removal, got {stored:?}",
            row.case,
            base_tags()
        );

        for step in removals() {
            let case = format!("{} / {}", row.case, step.case);
            let before = stored.clone();
            let expected = without(&before, &step.keys);

            harness.untag_ok(&case, arn, &step.keys).await;
            stored = harness.tags_of(&case, arn).await;

            assert_eq!(
                stored, expected,
                "case {case:?}: expected the remaining tag set {expected:?}, got {stored:?}"
            );
            // The named keys, spelled out: none of them is reported any more,
            // whatever value it held.
            for key in &step.keys {
                assert_eq!(
                    stored.get(key),
                    None,
                    "case {case:?}: the key {key:?} the removal named must be gone, got {:?}",
                    stored.get(key)
                );
            }
            // The keys it did NOT name keep the values they had, so the call
            // removed rather than cleared the set.
            for (key, value) in &before {
                if step.keys.contains(key) {
                    continue;
                }
                assert_eq!(
                    stored.get(key),
                    Some(value),
                    "case {case:?}: the key {key:?} the removal did not name must still hold \
                     {value:?}, got {:?}",
                    stored.get(key)
                );
            }
        }

        // The last removal named every key still stored, so the resource now
        // reports the empty set — which is a resource with no tags, not a read
        // that did not look.
        assert!(
            stored.is_empty(),
            "case {:?}: expected the empty tag set once every key was removed, got {stored:?}",
            row.case
        );
    }
}

/// RULE, the addressing part: a removal reaches the ONE resource its ARN
/// addresses, so untagging an event bus moves neither its subscribers' nor its
/// event sources' tag sets, and untagging one of them does not move the bus's.
#[tokio::test]
async fn a_tag_removal_reaches_only_the_resource_its_arn_addresses() {
    let harness = Harness::new();
    let created = created_rows(&harness).await;

    // Every row carries the same keys, so a removal that reached another
    // resource would show as a key missing from it.
    let mut expected: Vec<BTreeMap<String, String>> = Vec::new();
    for (row, arn) in &created {
        harness.tag_ok(row.case, arn, &base_tags()).await;
        expected.push(harness.tags_of(row.case, arn).await);
    }

    for (index, (row, arn)) in created.iter().enumerate() {
        let case = format!("{} / untagged alone", row.case);
        let keys = vec!["env".to_string(), "Team".to_string()];
        harness.untag_ok(&case, arn, &keys).await;
        expected[index] = without(&expected[index], &keys);

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

/// RULE, the count bound: `TagKeyList` admits up to 200 keys, which is also the
/// most tags a resource holds, so one call names every key a full resource has
/// and leaves it with none.
#[tokio::test]
async fn a_removal_may_name_as_many_keys_as_a_resource_holds() {
    let case = "a resource filled to the tag maximum";
    let harness = Harness::new();
    let arn = harness.active_bus(case, "untag-bulk-bus").await;

    let full: BTreeMap<String, String> = (0..MAX_TAG_KEYS)
        .map(|index| (format!("key-{index:03}"), format!("value-{index:03}")))
        .collect();
    harness.tag_ok(case, &arn, &full).await;
    let reported = harness.tags_of(case, &arn).await;
    assert_eq!(
        reported.len(),
        MAX_TAG_KEYS,
        "case {case:?}: expected {MAX_TAG_KEYS} stored tags before the removal, got {}",
        reported.len()
    );

    // All but one key, in one call: the resource keeps exactly the key the call
    // did not name.
    let kept = "key-000".to_string();
    let most: Vec<String> = full.keys().filter(|key| **key != kept).cloned().collect();
    harness.untag_ok(case, &arn, &most).await;
    let reported = harness.tags_of(case, &arn).await;
    assert_eq!(
        reported,
        full.iter()
            .filter(|(key, _)| **key == kept)
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<BTreeMap<String, String>>(),
        "case {case:?}: expected only {kept:?} left, got {reported:?}"
    );

    // The maximum the list admits, naming keys the resource mostly no longer
    // holds: it keeps none of them and is served, rather than refused for the
    // keys already gone.
    let every: Vec<String> = full.keys().cloned().collect();
    assert_eq!(
        every.len(),
        MAX_TAG_KEYS,
        "the removal must name the modelled maximum of {MAX_TAG_KEYS} keys, it names {}",
        every.len()
    );
    harness.untag_ok(case, &arn, &every).await;
    let reported = harness.tags_of(case, &arn).await;
    assert!(
        reported.is_empty(),
        "case {case:?}: expected the empty tag set, got {reported:?}"
    );
}

/// RULE, the refusal: a `ResourceArn` of any taggable resource type that names
/// no stored resource is refused with `ResourceNotFoundException` (404) — one of
/// this operation's modelled errors, never a not-implemented signal.
#[tokio::test]
async fn untagging_a_resource_that_does_not_exist_is_refused() {
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
            .untag(&arn, &["env".to_string()])
            .await
            .expect_err(&format!("case {case:?}: UntagResource must be refused"));
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
/// no tag set to remove from, so the removal of it is refused exactly as a read
/// of it is, and the removal does not bring it back. The wait is for the
/// CONDITION (the bus has disappeared), polled under a generous deadline.
#[tokio::test]
async fn untagging_a_deleted_bus_is_refused() {
    let case = "a deleted event bus";
    let harness = Harness::new();
    let arn = harness.active_bus(case, "untag-deleted-bus").await;
    harness.tag_ok(case, &arn, &tags(&[("Team", "platform")])).await;

    DeleteEventBus::new(Arc::clone(&harness.engine))
        .handle(
            DeleteEventBusInput {
                event_bus_arn: arn.clone(),
            },
            &harness.ctx(),
        )
        .await
        .unwrap_or_else(|e| panic!("case {case:?}: DeleteEventBus must succeed, got {e:?}"));

    let deadline = std::time::Instant::now() + POLL_DEADLINE;
    loop {
        if harness.describe_bus(&arn).await.is_err() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the deleted bus {arn} never disappeared"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    let error = harness
        .untag(&arn, &["Team".to_string()])
        .await
        .expect_err(&format!("case {case:?}: UntagResource must be refused"));
    assert_eq!(
        (error.shape_name(), error.http_status()),
        ("ResourceNotFoundException", 404),
        "case {case:?}: expected ResourceNotFoundException (404), got {error:?} at {}",
        error.http_status()
    );
    // The refused removal wrote nothing, so the bus is still gone.
    assert!(
        harness.describe_bus(&arn).await.is_err(),
        "case {case:?}: the refused removal must not have restored the bus {arn}"
    );
}

/// RULE, the state refusal: a resource whose revocation the model documents as
/// refusing every mutating operation on it ("Mutating operations on a revoked
/// subscriber fail with InvalidStateException, except DeleteSubscriber", and the
/// same for an EventSource) refuses a tag removal with `InvalidStateException`
/// (409) and keeps the tags it holds.
#[tokio::test]
async fn untagging_a_revoked_resource_is_refused() {
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
        harness.tag_ok(&case, arn, &base_tags()).await;
        let before = harness.tags_of(&case, arn).await;
        harness.revoke(&case, arn).await;

        let error = harness
            .untag(arn, &["env".to_string()])
            .await
            .expect_err(&format!("case {case:?}: UntagResource must be refused"));
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
            "case {case:?}: the refused removal must leave the tag set {before:?}, got \
             {reported:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// RULE again, where a client writes it: on the wire
// ---------------------------------------------------------------------------

/// The same rule over HTTP with rpcv2Cbor bodies, driven through the crate's
/// shared wire harness, so the status and the encoded body are the emulator's
/// own: the wire removal answers 200, the read that follows reports the set
/// without the keys it named, and an ARN naming nothing answers the modelled 404
/// rather than the 501 of an unimplemented operation.
#[tokio::test]
async fn the_wire_removal_answers_200_and_the_read_that_follows_reports_it() {
    let case = "the wire tag removal";
    let wire = Wire::new();

    let bus = wire.active_bus(case, "untag-wire-bus").await;
    let written = tags(&[("Team", "platform"), ("env", "staging"), ("marker", "")]);
    let _ = wire
        .ok::<TagResource<InMemoryEngine>>(
            case,
            &TagResourceInput {
                resource_arn: bus.clone(),
                tags: as_input(&written),
            },
        )
        .await;

    let removed = vec!["env".to_string(), "never-stored".to_string()];
    let (status, body) = wire
        .call::<UntagResource<InMemoryEngine>>(&UntagResourceInput {
            resource_arn: bus.clone(),
            tag_keys: removed.clone(),
        })
        .await;
    assert_eq!(
        status,
        200,
        "case {case:?}: the wire removal must answer 200, got {status} with {:?}",
        rendered(&body)
    );

    let expected = without(&written, &removed);
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
        .call::<UntagResource<InMemoryEngine>>(&UntagResourceInput {
            resource_arn: absent,
            tag_keys: removed,
        })
        .await;
    assert_eq!(
        status,
        404,
        "case {case:?}: a wire removal on an ARN naming nothing must answer 404, got {status} \
         with {:?}",
        rendered(&body)
    );
    let (code, _) = refusal(case, &body);
    assert_eq!(
        code, "ResourceNotFoundException",
        "case {case:?}: expected the modelled ResourceNotFoundException, got {code:?}"
    );
}
