// SPDX-License-Identifier: CC0-1.0

//! `ListSubscribers`: the enumeration of the stored subscribers as summaries.
//!
//! RULE: the enumeration reports the subscribers the store holds, as summaries
//! projected from each stored subscriber, in one total order (by `Name`, the key
//! `NamePrefix` filters on, ties broken by the ARN). It spans the subscribers of
//! the region the request addresses, `EventBusArn` scopes it to the subscribers
//! of ONE bus — an ARN no bus is stored at being `ResourceNotFoundException`
//! (404) — `NamePrefix` keeps the names that start with it, `MaxResults` bounds
//! a page, and a `NextToken` resumes exactly after the last item of the page
//! that issued it, with no token once the enumeration is exhausted.
//!
//! The rows span that class rather than one sequence: they differ along the
//! scope (unscoped, each of two buses, each of two regions), the prefix (absent,
//! one that selects a subset, one that selects nothing), the page size (one, two,
//! larger than the enumeration), the form and the size of the names enumerated
//! (plain, each punctuation character the name grammar allows, one character,
//! and the name length maximum), and what the summaries carry (a revoked
//! subscriber, a STOPPED one, two ordering types).
//!
//! The order being TOTAL is asserted where only the tie-break can decide it: a
//! name held on two buses at once, which the name scope allows, so the two
//! subscribers of one name are enumerated once each and paging at a page of one
//! walks past both. And the enumeration being REPORTING ONLY is asserted by
//! reading every subscriber back, through this operation and through
//! `DescribeSubscriber`, after the enumeration has been paged and filtered
//! repeatedly.
//!
//! Nothing here waits on the wall clock: the one wait polls for a bus becoming
//! `ACTIVE` under a generous deadline.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::InMemoryEngine;
use smithy_server_runtime::{
    EmbeddedEmulator, MultiServiceRouter, Operation, OperationContext, OperationError,
};

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

use eventsv2::ops::create_event_bus::CreateEventBus;
use eventsv2::ops::create_subscriber::CreateSubscriber;
use eventsv2::ops::describe_event_bus::DescribeEventBus;
use eventsv2::ops::describe_subscriber::DescribeSubscriber;
use eventsv2::ops::list_subscribers::ListSubscribers;
use eventsv2::ops::revoke_resource::RevokeResource;
use eventsv2::ops::types::{
    CreateEventBusInput, CreateSubscriberInput, DescribeEventBusInput, DescribeSubscriberInput,
    InvokeConfiguration, ListSubscribersInput, ListSubscribersOutput, RevokeResourceInput,
    SubscriberSummary,
};

/// The account segment every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The region the harness signs for; it must reach the minted ARNs.
const REGION: &str = "us-west-2";

/// A second region, so the suite can assert an unscoped enumeration reports the
/// subscribers of the region the request addresses.
const OTHER_REGION: &str = "eu-west-3";

/// The role a subscriber's `InvokeConfiguration` names.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/event-delivery";

/// How long a poll waits for a condition the emulator owes: far above the
/// provisioning span a bus takes, so a loaded host has ample room.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long one pass of a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// A page large enough to hold every enumeration below in one answer, which is
/// also the modelled default of `MaxResults`.
const WHOLE_PAGE: i32 = 100;

/// The maximum length of a `SubscriberName`, per
/// `smithy_query(query="shape",shape="SubscriberName")` (`@length 1..256`), so a
/// row can enumerate a name at the member's own bound.
const SUBSCRIBER_NAME_MAX: usize = 256;

/// More summaries than any enumeration below holds, so a paging loop that would
/// not terminate fails instead of running forever.
const MAX_ENUMERATION: usize = 64;

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
        .uri("/service/AWSEventsV2/operation/ListSubscribers")
        .header("authorization", authorization(region))
        .header("content-type", "application/cbor")
        .body(())
        .expect("the harness request is well formed")
        .into_parts();
    parts
}

/// An `InvokeConfiguration` naming the role every row shares and `target`.
fn invoke(target: &str) -> InvokeConfiguration {
    InvokeConfiguration {
        role_arn: ROLE_ARN.to_string(),
        target_arn: target.to_string(),
        ..Default::default()
    }
}

/// A queue ARN of the account, which is all a target has to be here: nothing is
/// published below.
fn queue(name: &str) -> String {
    format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}")
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

    /// Create an event bus in `region` and poll until it reports `ACTIVE` — the
    /// state `CreateSubscriber` requires of it.
    async fn active_bus(&self, region: &str, name: &str) -> String {
        let arn = CreateEventBus::new(Arc::clone(&self.engine))
            .handle(
                CreateEventBusInput {
                    name: name.to_string(),
                    client_token: Some(format!("token-{region}-{name}")),
                    ..Default::default()
                },
                &self.ctx(region),
            )
            .await
            .unwrap_or_else(|e| panic!("case {name:?}: CreateEventBus must succeed, got {e:?}"))
            .event_bus_arn
            .unwrap_or_else(|| panic!("case {name:?}: CreateEventBus must report an EventBusArn"));

        let active = eventsv2::state::BusLifecycleState::Active.wire_value();
        let deadline = Instant::now() + POLL_DEADLINE;
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
            if reported.as_deref() == Some(active) {
                return arn;
            }
            assert!(
                Instant::now() < deadline,
                "case {name:?}: the bus never reported {active}, last reported {reported:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Create a subscriber on `bus_arn`, handing back its `SubscriberArn`.
    async fn create(
        &self,
        region: &str,
        bus_arn: &str,
        name: &str,
        ordering_type: Option<&str>,
        run_state: Option<&str>,
    ) -> String {
        self.create_with_token(
            region,
            bus_arn,
            name,
            ordering_type,
            run_state,
            &format!("token-{region}-{name}"),
        )
        .await
    }

    /// Create a subscriber under a stated idempotency token, which is what two
    /// creates of ONE name on two buses need: the token is scoped to the region,
    /// so sharing one would make the second create a replay of the first rather
    /// than another subscriber.
    async fn create_with_token(
        &self,
        region: &str,
        bus_arn: &str,
        name: &str,
        ordering_type: Option<&str>,
        run_state: Option<&str>,
        client_token: &str,
    ) -> String {
        CreateSubscriber::new(Arc::clone(&self.engine))
            .handle(
                CreateSubscriberInput {
                    name: name.to_string(),
                    event_bus_arn: bus_arn.to_string(),
                    invoke_configuration: invoke(&queue(&format!("target-{name}"))),
                    r#type: ordering_type.map(str::to_string),
                    state: run_state.map(str::to_string),
                    client_token: Some(client_token.to_string()),
                    ..Default::default()
                },
                &self.ctx(region),
            )
            .await
            .unwrap_or_else(|e| panic!("case {name:?}: CreateSubscriber must succeed, got {e:?}"))
            .subscriber_arn
            .unwrap_or_else(|| panic!("case {name:?}: CreateSubscriber must report a SubscriberArn"))
    }

    /// Every summary of the whole unscoped enumeration of `region`, paged until
    /// it reports no token, in the order the service reported them.
    async fn whole_enumeration(
        &self,
        region: &str,
        page_size: i32,
        case: &str,
    ) -> Vec<SubscriberSummary> {
        let mut seen: Vec<SubscriberSummary> = Vec::new();
        let mut token = None;
        loop {
            let answer = self
                .list(
                    region,
                    ListSubscribersInput {
                        max_results: page_size,
                        next_token: token.clone(),
                        ..Default::default()
                    },
                )
                .await
                .unwrap_or_else(|e| {
                    panic!("case {case:?}: ListSubscribers must succeed, got {e:?}")
                });
            seen.extend(answer.subscribers.unwrap_or_default());
            token = answer.next_token;
            if token.is_none() {
                return seen;
            }
            assert!(
                seen.len() <= MAX_ENUMERATION,
                "case {case:?}: paging must terminate, got {} summaries so far",
                seen.len()
            );
        }
    }

    /// What `DescribeSubscriber` reports about `arn` that an enumeration could
    /// have disturbed: its run state, its `Revoked` flag and the instant it was
    /// last modified.
    async fn described(&self, region: &str, arn: &str, case: &str) -> String {
        let described = DescribeSubscriber::new(Arc::clone(&self.engine))
            .handle(
                DescribeSubscriberInput {
                    subscriber_arn: arn.to_string(),
                },
                &self.ctx(region),
            )
            .await
            .unwrap_or_else(|e| {
                panic!("case {case:?}: DescribeSubscriber({arn}) must succeed, got {e:?}")
            });
        format!(
            "{:?}|{:?}|{:?}|{:?}",
            described.state,
            described.revoked,
            instant(&described.creation_time),
            instant(&described.last_modified_time)
        )
    }

    async fn list(
        &self,
        region: &str,
        input: ListSubscribersInput,
    ) -> Result<ListSubscribersOutput, eventsv2::ops::list_subscribers::ListSubscribersError> {
        ListSubscribers::new(Arc::clone(&self.engine))
            .handle(input, &self.ctx(region))
            .await
    }

    /// Every summary of one page, in the order the service reported them.
    async fn summaries(
        &self,
        region: &str,
        input: ListSubscribersInput,
        case: &str,
    ) -> Vec<SubscriberSummary> {
        self.list(region, input)
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: ListSubscribers must succeed, got {e:?}"))
            .subscribers
            .unwrap_or_else(|| panic!("case {case:?}: ListSubscribers must report Subscribers"))
    }

    /// Withdraw a resource, which is how a subscriber becomes `Revoked`.
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
}

/// The `Name` of each summary, which is the order the enumeration is asserted
/// in.
fn names_of(summaries: &[SubscriberSummary]) -> Vec<String> {
    summaries
        .iter()
        .map(|summary| summary.name.clone().unwrap_or_default())
        .collect()
}

/// The `SubscriberArn` of each summary, which is what the order falls back to
/// when two summaries carry one name.
fn arns_of(summaries: &[SubscriberSummary]) -> Vec<String> {
    summaries
        .iter()
        .map(|summary| summary.subscriber_arn.clone().unwrap_or_default())
        .collect()
}

/// An instant as the pair of whole seconds and nanoseconds it is, so two
/// readings of one timestamp compare without the shape having to be comparable.
fn instant(time: &Option<::aws_smithy_types::DateTime>) -> Option<(i64, u32)> {
    time.as_ref().map(|time| (time.secs(), time.subsec_nanos()))
}

/// Every member of a summary, rendered so two readings of one enumeration can be
/// compared member by member.
fn fingerprint(summary: &SubscriberSummary) -> String {
    format!(
        "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
        summary.subscriber_arn,
        summary.name,
        summary.event_bus_arn,
        summary.target_arn,
        summary.r#type,
        summary.revoked,
        summary.state,
        instant(&summary.creation_time),
        instant(&summary.last_modified_time),
        summary.subscriber_account_id
    )
}

/// The whole enumeration as one comparable value.
fn fingerprints(summaries: &[SubscriberSummary]) -> Vec<String> {
    summaries.iter().map(fingerprint).collect()
}

// ---------------------------------------------------------------------------
// The scope, the prefix and the order
// ---------------------------------------------------------------------------

/// RULE: the enumeration spans the subscribers of the region a request
/// addresses, `EventBusArn` scopes it to one bus, `NamePrefix` keeps the names
/// that start with it, and the order is by name whatever was created when.
#[tokio::test]
async fn the_enumeration_is_scoped_filtered_and_ordered_by_name() {
    let harness = Harness::new();
    let orders = harness.active_bus(REGION, "list-orders-bus").await;
    let billing = harness.active_bus(REGION, "list-billing-bus").await;
    let away = harness.active_bus(OTHER_REGION, "list-away-bus").await;

    // Created out of alphabetical order, so the order asserted below is the
    // service's own rather than the order of creation.
    harness
        .create(REGION, &orders, "orders-ship", None, None)
        .await;
    harness
        .create(REGION, &orders, "orders-create", Some("FIFO"), None)
        .await;
    harness
        .create(REGION, &billing, "billing-invoice", None, Some("STOPPED"))
        .await;
    harness.create(OTHER_REGION, &away, "away-one", None, None).await;

    for (case, region, input, expected) in [
        (
            "unscoped, the signed region",
            REGION,
            ListSubscribersInput {
                max_results: WHOLE_PAGE,
                ..Default::default()
            },
            vec!["billing-invoice", "orders-create", "orders-ship"],
        ),
        (
            "unscoped, the other region",
            OTHER_REGION,
            ListSubscribersInput {
                max_results: WHOLE_PAGE,
                ..Default::default()
            },
            vec!["away-one"],
        ),
        (
            "scoped to one bus",
            REGION,
            ListSubscribersInput {
                event_bus_arn: Some(orders.clone()),
                max_results: WHOLE_PAGE,
                ..Default::default()
            },
            vec!["orders-create", "orders-ship"],
        ),
        (
            "scoped to the other bus",
            REGION,
            ListSubscribersInput {
                event_bus_arn: Some(billing.clone()),
                max_results: WHOLE_PAGE,
                ..Default::default()
            },
            vec!["billing-invoice"],
        ),
        (
            "a prefix selecting a subset",
            REGION,
            ListSubscribersInput {
                name_prefix: Some("orders-".to_string()),
                max_results: WHOLE_PAGE,
                ..Default::default()
            },
            vec!["orders-create", "orders-ship"],
        ),
        (
            "a prefix selecting one name",
            REGION,
            ListSubscribersInput {
                name_prefix: Some("orders-s".to_string()),
                max_results: WHOLE_PAGE,
                ..Default::default()
            },
            vec!["orders-ship"],
        ),
        (
            "a prefix selecting nothing",
            REGION,
            ListSubscribersInput {
                name_prefix: Some("nothing".to_string()),
                max_results: WHOLE_PAGE,
                ..Default::default()
            },
            vec![],
        ),
        (
            "a bus scope and a prefix together",
            REGION,
            ListSubscribersInput {
                event_bus_arn: Some(orders.clone()),
                name_prefix: Some("orders-c".to_string()),
                max_results: WHOLE_PAGE,
                ..Default::default()
            },
            vec!["orders-create"],
        ),
        (
            // The bus of the other region: a scoped enumeration takes its
            // region from the bus the ARN names, not from the request.
            "a bus of the other region, asked for in the signed region",
            REGION,
            ListSubscribersInput {
                event_bus_arn: Some(away.clone()),
                max_results: WHOLE_PAGE,
                ..Default::default()
            },
            vec!["away-one"],
        ),
    ] {
        let got = names_of(&harness.summaries(region, input, case).await);
        let expected: Vec<String> = expected.iter().map(|name| (*name).to_string()).collect();
        assert_eq!(
            got, expected,
            "case {case:?}: expected {expected:?}, got {got:?}"
        );
    }
}

/// RULE: every member of a summary is read back out of the stored subscriber —
/// its ARN, name, bus, target, ordering type, run state, account and the
/// `Revoked` flag, which is "present only when true".
#[tokio::test]
async fn a_summary_is_projected_from_the_stored_subscriber() {
    let harness = Harness::new();
    let bus = harness.active_bus(REGION, "projection-bus").await;
    let running = harness
        .create(REGION, &bus, "projection-running", Some("FIFO"), None)
        .await;
    let revoked = harness
        .create(
            REGION,
            &bus,
            "projection-revoked",
            Some("UNORDERED"),
            Some("STOPPED"),
        )
        .await;
    harness.revoke(REGION, &revoked).await;

    let summaries = harness
        .summaries(
            REGION,
            ListSubscribersInput {
                event_bus_arn: Some(bus.clone()),
                max_results: WHOLE_PAGE,
                ..Default::default()
            },
            "the projection",
        )
        .await;

    for (case, arn, name, ordering_type, run_state, expected_revoked) in [
        (
            "a running subscriber",
            running.as_str(),
            "projection-running",
            "FIFO",
            "RUNNING",
            None,
        ),
        (
            "a revoked subscriber",
            revoked.as_str(),
            "projection-revoked",
            "UNORDERED",
            "STOPPED",
            Some(true),
        ),
    ] {
        let summary = summaries
            .iter()
            .find(|summary| summary.subscriber_arn.as_deref() == Some(arn))
            .unwrap_or_else(|| panic!("case {case:?}: the enumeration must report {arn}"));
        assert_eq!(
            summary.name.as_deref(),
            Some(name),
            "case {case:?}: expected Name {name:?}, got {:?}",
            summary.name
        );
        assert_eq!(
            summary.event_bus_arn.as_deref(),
            Some(bus.as_str()),
            "case {case:?}: expected the bus it was created on, got {:?}",
            summary.event_bus_arn
        );
        assert_eq!(
            summary.target_arn.as_deref(),
            Some(queue(&format!("target-{name}")).as_str()),
            "case {case:?}: expected the target it invokes, got {:?}",
            summary.target_arn
        );
        assert_eq!(
            summary.r#type.as_deref(),
            Some(ordering_type),
            "case {case:?}: expected Type {ordering_type:?}, got {:?}",
            summary.r#type
        );
        assert_eq!(
            summary.state.as_deref(),
            Some(run_state),
            "case {case:?}: expected State {run_state:?}, got {:?}",
            summary.state
        );
        assert_eq!(
            summary.revoked, expected_revoked,
            "case {case:?}: expected Revoked {expected_revoked:?}, got {:?}",
            summary.revoked
        );
        assert_eq!(
            summary.subscriber_account_id.as_deref(),
            Some(ACCOUNT),
            "case {case:?}: expected the account of the bus, got {:?}",
            summary.subscriber_account_id
        );
        assert!(
            summary.creation_time.is_some() && summary.last_modified_time.is_some(),
            "case {case:?}: a summary must carry both of its instants, got {summary:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Paging
// ---------------------------------------------------------------------------

/// RULE: `MaxResults` bounds a page and a `NextToken` resumes exactly after the
/// last item of the page that issued it, so paging the enumeration at any page
/// size yields it once, in order, with no token on the last page.
#[tokio::test]
async fn paging_yields_the_whole_enumeration_once_in_order() {
    let harness = Harness::new();
    let bus = harness.active_bus(REGION, "paging-bus").await;
    let created = ["page-a", "page-b", "page-c", "page-d", "page-e"];
    for name in created {
        harness.create(REGION, &bus, name, None, None).await;
    }
    let whole: Vec<String> = created.iter().map(|name| (*name).to_string()).collect();

    for page_size in [1, 2, 3, 5, WHOLE_PAGE] {
        let case = format!("pages of {page_size}");
        let mut seen: Vec<String> = Vec::new();
        let mut token = None;
        // Bounded by the enumeration itself: one pass per item at worst, plus
        // the pass that reports no token.
        for _ in 0..=created.len() {
            let answer = harness
                .list(
                    REGION,
                    ListSubscribersInput {
                        event_bus_arn: Some(bus.clone()),
                        max_results: page_size,
                        next_token: token.clone(),
                        ..Default::default()
                    },
                )
                .await
                .unwrap_or_else(|e| {
                    panic!("case {case:?}: ListSubscribers must succeed, got {e:?}")
                });
            let page = answer.subscribers.unwrap_or_default();
            assert!(
                page.len() <= usize::try_from(page_size).unwrap_or(usize::MAX),
                "case {case:?}: a page must hold at most {page_size} summaries, got {}",
                page.len()
            );
            seen.extend(names_of(&page));
            token = answer.next_token;
            if token.is_none() {
                break;
            }
        }
        assert!(
            token.is_none(),
            "case {case:?}: paging must end with no token, got {token:?}"
        );
        assert_eq!(
            seen, whole,
            "case {case:?}: expected the whole enumeration once in order, got {seen:?}"
        );
    }
}

/// RULE, the tie-break the order rests on: the enumeration is ordered by name
/// with the ARN deciding a tie, so two subscribers that hold ONE name — which
/// the name scope allows, a name being taken on its own bus only — are each
/// enumerated once, in ARN order, and a page boundary between them resumes after
/// the one it ended on rather than skipping or repeating either.
///
/// The rows span the name class rather than one name: a plain one, one carrying
/// every punctuation character `SubscriberName` allows, a single character and a
/// name at that member's `@length` maximum of 256 (from
/// `smithy_query(query="shape",shape="SubscriberName")`). No row's name is a
/// prefix of another's, so each row's prefix selects its own two subscribers.
#[tokio::test]
async fn one_name_on_two_buses_is_enumerated_once_per_bus_in_arn_order() {
    let harness = Harness::new();
    let first_bus = harness.active_bus(REGION, "collision-bus-one").await;
    let second_bus = harness.active_bus(REGION, "collision-bus-two").await;

    let rows: Vec<(&str, String)> = vec![
        ("a plain name", "shared".to_string()),
        (
            "a name carrying each allowed punctuation",
            "dup.ship-v1_final".to_string(),
        ),
        ("a one-character name", "x".to_string()),
        (
            "a name at the length maximum",
            format!("n{}", "a".repeat(SUBSCRIBER_NAME_MAX - 1)),
        ),
    ];

    for (row, (case, name)) in rows.iter().enumerate() {
        let on_first = harness
            .create_with_token(
                REGION,
                &first_bus,
                name,
                None,
                None,
                &format!("collision-first-{row}"),
            )
            .await;
        let on_second = harness
            .create_with_token(
                REGION,
                &second_bus,
                name,
                Some("FIFO"),
                None,
                &format!("collision-second-{row}"),
            )
            .await;

        // Both are minted, so the pair is two subscribers of one name.
        assert_ne!(
            on_first, on_second,
            "case {case:?}: one name on two buses must mint two subscribers, got {on_first}"
        );

        // The tie-break: ARN order, whichever ARN the service happened to mint
        // first.
        let mut expected = vec![on_first.clone(), on_second.clone()];
        expected.sort();

        let unscoped = harness
            .summaries(
                REGION,
                ListSubscribersInput {
                    name_prefix: Some(name.clone()),
                    max_results: WHOLE_PAGE,
                    ..Default::default()
                },
                case,
            )
            .await;
        let got = arns_of(&unscoped);
        assert_eq!(
            got, expected,
            "case {case:?}: expected both subscribers of that name in ARN order {expected:?}, \
             got {got:?}"
        );
        let listed_names = names_of(&unscoped);
        assert_eq!(
            listed_names,
            vec![name.clone(), name.clone()],
            "case {case:?}: both summaries must carry the name enumerated, got {listed_names:?}"
        );

        // Each bus's own enumeration holds exactly its own subscriber of that
        // name.
        for (bus_case, bus, subscriber) in [
            ("scoped to the first bus", &first_bus, &on_first),
            ("scoped to the second bus", &second_bus, &on_second),
        ] {
            let scoped = harness
                .summaries(
                    REGION,
                    ListSubscribersInput {
                        event_bus_arn: Some(bus.clone()),
                        name_prefix: Some(name.clone()),
                        max_results: WHOLE_PAGE,
                        ..Default::default()
                    },
                    bus_case,
                )
                .await;
            let got = arns_of(&scoped);
            assert_eq!(
                got,
                vec![subscriber.clone()],
                "case {case:?} / {bus_case:?}: expected exactly {subscriber}, got {got:?}"
            );
        }

        // A page boundary falling between two subscribers of one name: the
        // cursor has to separate them, so paging at one yields each exactly
        // once.
        let mut paged: Vec<String> = Vec::new();
        let mut token = None;
        for _ in 0..=expected.len() {
            let answer = harness
                .list(
                    REGION,
                    ListSubscribersInput {
                        name_prefix: Some(name.clone()),
                        max_results: 1,
                        next_token: token.clone(),
                        ..Default::default()
                    },
                )
                .await
                .unwrap_or_else(|e| {
                    panic!("case {case:?}: ListSubscribers must succeed, got {e:?}")
                });
            paged.extend(arns_of(&answer.subscribers.unwrap_or_default()));
            token = answer.next_token;
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            paged, expected,
            "case {case:?}: pages of one must yield each subscriber of that name once, in ARN \
             order {expected:?}, got {paged:?}"
        );
        assert!(
            token.is_none(),
            "case {case:?}: paging must end with no token, got {token:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Reporting only
// ---------------------------------------------------------------------------

/// RULE: the enumeration REPORTS the stored subscribers, so however often it is
/// paged, filtered or scoped, every later read — the enumeration itself and
/// `DescribeSubscriber` — reports exactly what it did before: no run state
/// advanced, no `Revoked` flag flipped and no instant rewritten by the act of
/// listing.
#[tokio::test]
async fn the_enumeration_reports_without_changing_what_a_later_read_sees() {
    let harness = Harness::new();
    let bus = harness.active_bus(REGION, "reporting-bus").await;
    let running = harness
        .create(REGION, &bus, "reporting-running", Some("FIFO"), None)
        .await;
    let stopped = harness
        .create(
            REGION,
            &bus,
            "reporting-stopped",
            Some("UNORDERED"),
            Some("STOPPED"),
        )
        .await;
    let revoked = harness
        .create(REGION, &bus, "reporting-revoked", None, None)
        .await;
    harness.revoke(REGION, &revoked).await;
    let subscribers = [
        ("a running subscriber", running.as_str()),
        ("a stopped subscriber", stopped.as_str()),
        ("a revoked subscriber", revoked.as_str()),
    ];

    let before = fingerprints(&harness.whole_enumeration(REGION, WHOLE_PAGE, "before").await);
    let mut described_before = Vec::new();
    for (case, arn) in subscribers {
        described_before.push(harness.described(REGION, arn, case).await);
    }

    // Every form of read this operation offers, several times over: a page of
    // one, a page of two, each scope, each kind of prefix, and two refusals.
    for page_size in [1, 2, WHOLE_PAGE] {
        let case = format!("reading at pages of {page_size}");
        let _ = harness.whole_enumeration(REGION, page_size, &case).await;
        for input in [
            ListSubscribersInput {
                event_bus_arn: Some(bus.clone()),
                max_results: page_size,
                ..Default::default()
            },
            ListSubscribersInput {
                name_prefix: Some("reporting-".to_string()),
                max_results: page_size,
                ..Default::default()
            },
            ListSubscribersInput {
                name_prefix: Some("selects-nothing".to_string()),
                max_results: page_size,
                ..Default::default()
            },
        ] {
            let _ = harness.summaries(REGION, input, &case).await;
        }
        // A refused read must not disturb the store either.
        let _ = harness
            .list(
                REGION,
                ListSubscribersInput {
                    max_results: page_size,
                    next_token: Some("not-a-token".to_string()),
                    ..Default::default()
                },
            )
            .await
            .err();
    }

    let after = fingerprints(&harness.whole_enumeration(REGION, WHOLE_PAGE, "after").await);
    assert_eq!(
        after, before,
        "case \"after the reads\": the enumeration must report exactly what it did before, \
         expected {before:?}, got {after:?}"
    );
    for (index, (case, arn)) in subscribers.into_iter().enumerate() {
        let described_after = harness.described(REGION, arn, case).await;
        assert_eq!(
            described_after, described_before[index],
            "case {case:?}: {arn} must read back unchanged, expected {:?}, got {described_after:?}",
            described_before[index]
        );
    }
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// RULE: a bus scope has to resolve — an `EventBusArn` the store holds no bus at
/// is `ResourceNotFoundException` (404) — and a `NextToken` this service did not
/// issue is refused rather than read as a cursor.
#[tokio::test]
async fn a_scope_that_resolves_to_no_bus_and_a_token_nothing_issued_are_refused() {
    let harness = Harness::new();
    let live = harness.active_bus(REGION, "refusal-bus").await;
    harness
        .create(REGION, &live, "refusal-subscriber", None, None)
        .await;

    // An ARN of the modelled form whose id segment this service never handed
    // out addresses no resource of this service at all.
    let unminted = format!(
        "arn:aws:events:{REGION}:{ACCOUNT}:event-busv2/absent-bus/aaaaaaaaaaaaaaaaaaaaaaaaa"
    );
    let error = harness
        .list(
            REGION,
            ListSubscribersInput {
                event_bus_arn: Some(unminted.clone()),
                max_results: WHOLE_PAGE,
                ..Default::default()
            },
        )
        .await
        .expect_err("case \"a bus nothing minted\": the enumeration must be refused");
    assert_eq!(
        error.shape_name(),
        "ResourceNotFoundException",
        "case \"a bus nothing minted\": expected ResourceNotFoundException, got {:?}",
        error.shape_name()
    );
    assert_eq!(
        error.http_status(),
        404,
        "case \"a bus nothing minted\": expected HTTP 404, got {}",
        error.http_status()
    );

    for (case, token) in [
        ("not hex at all", "not-a-token"),
        ("an odd number of hex digits", "abc"),
        ("hex carrying no cursor separator", "6162"),
    ] {
        let error = harness
            .list(
                REGION,
                ListSubscribersInput {
                    max_results: WHOLE_PAGE,
                    next_token: Some(token.to_string()),
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
