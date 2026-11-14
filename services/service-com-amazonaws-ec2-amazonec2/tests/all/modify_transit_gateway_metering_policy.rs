// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ModifyTransitGatewayMeteringPolicy`, driven over the wire this service
//! speaks (`ec2Query`), with `DescribeTransitGatewayMeteringPolicies` reading back what the
//! modify stored (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test is the one every EC2 modify of a membership set follows: the call lays
//! the additions and the removals the request names over the SET the resource already holds and
//! PERSISTS the result, reporting the record it stored — the members the request does not touch
//! kept exactly as they were, the resource left in the TRANSIENT state its own catalog carries
//! for a change under way with the clock owing it the state the resource is in once the change
//! has taken effect, and the instant that change becomes effective read off the clock. Every id
//! the request names is resolved BEFORE anything is written, so a value that is not an id of its
//! kind, an id nothing holds, a removal of something the resource does not hold, an id both
//! added and removed, a resource on its way out and a dry run are each refused with nothing
//! written.
//!
//! Each table row is a distinct input along what that rule generalises over: HOW MANY
//! attachments the policy meters before the call (none, one, several, every one there is), how
//! many the request ADDS and REMOVES (none, one, several), in WHICH order, whether an addition
//! names one the policy already meters, whether a request names NEITHER list, and which state
//! the policy is in when the call arrives (the transient state of a create, and the state a
//! settled policy is in) — and, for the refusals, which id of the request is unusable and how.
//!
//! The middlebox attachments a policy meters are seeded through the store's own writer
//! (`insert_transit_gateway_attachment`), as every transit gateway suite in this crate does,
//! because `CreateTransitGatewayVpcAttachment` is still a scaffold stub; the policies
//! themselves are created over the wire through `CreateTransitGatewayMeteringPolicy` and
//! deleted over the wire through `DeleteTransitGatewayMeteringPolicy`.

use std::time::{Duration, Instant};

use ec2::ops::types::{TRANSIT_GATEWAY_METERING_POLICY_STATE_VALUES, TransitGatewayVpcAttachment};
use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, DRY_RUN_OPERATION, INCORRECT_STATE,
    INVALID_PARAMETER_COMBINATION, INVALID_PARAMETER_VALUE,
    INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_MALFORMED,
    INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_NOT_FOUND,
    INVALID_TRANSIT_GATEWAY_METERING_POLICY_ID_MALFORMED,
    INVALID_TRANSIT_GATEWAY_METERING_POLICY_ID_NOT_FOUND,
    RESOURCE_TYPE_TRANSIT_GATEWAY_METERING_POLICY, TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE,
    TRANSITION_DELAY, TransitGatewayAttachmentView,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{as_timestamp, element, error_code, error_message, indexed};
use super::wire::{Param, child, child_text, document, param, set_items};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The ec2Query `Action` under test.
const ACTION: &str = "ModifyTransitGatewayMeteringPolicy";

/// The create that mints the policies this suite modifies.
const CREATE: &str = "CreateTransitGatewayMeteringPolicy";

/// The delete that puts one policy on its way out.
const DELETE: &str = "DeleteTransitGatewayMeteringPolicy";

/// The describe that reads a modified policy back.
const DESCRIBE: &str = "DescribeTransitGatewayMeteringPolicies";

/// The HTTP status EC2 answers `DryRunOperation` with.
const DRY_RUN_STATUS: u16 = 412;

/// The HTTP status EC2 answers a refused request with.
const REFUSAL_STATUS: u16 = 400;

/// How many middlebox attachments every case seeds, so a row can name a subset of them in any
/// order.
const SEEDED_ATTACHMENTS: usize = 4;

/// The tag every policy this suite creates carries: a member no modify names, so it shows that
/// a change leaves what it does not touch exactly as it was.
const KEPT_TAG: (&str, &str) = ("Name", "north");

/// The deadline the polls below give up at: a large multiple of the service's own transition
/// span, so the assertion is about the state the clock owes and never about how fast the host
/// runs.
fn move_deadline() -> Duration {
    TRANSITION_DELAY * 60
}

/// How long one look waits before looking again — a fraction of the span, so a poll inside the
/// deadline above costs many looks rather than one.
fn poll_interval() -> Duration {
    TRANSITION_DELAY / 10
}

/// A member of `TransitGatewayMeteringPolicyState`, from the modelled catalog
/// (`smithy_query(query="shape",shape="TransitGatewayMeteringPolicyState")`, whose values are
/// `available | deleted | deleting | modifying | pending`) rather than written out here.
fn policy_state(name: &str) -> &'static str {
    TRANSIT_GATEWAY_METERING_POLICY_STATE_VALUES
        .iter()
        .find(|value| **value == name)
        .unwrap_or_else(|| panic!("{name:?} is no TransitGatewayMeteringPolicyState member"))
}

/// One call of `action` signed for the Region above.
async fn invoke(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// A transit gateway created through the wire, as its `TransitGatewayId`.
async fn transit_gateway(router: &MultiServiceRouter) -> String {
    let (status, body) = invoke(router, "CreateTransitGateway", &[]).await;
    assert_eq!(
        status, 200,
        "CreateTransitGateway must succeed, got {status} with body {body}"
    );
    element(&body, "transitGatewayId").expect("a created transit gateway reports its own id")
}

/// Store [`SEEDED_ATTACHMENTS`] VPC attachments of `gateway_id`, in service, and answer their
/// `TransitGatewayAttachmentId`s — the attachments a policy may meter.
fn seed_attachments(engine: &engine::InMemoryEngine, gateway_id: &str) -> Vec<String> {
    let store = engine.state::<Ec2State>();
    let mut guard = store.lock();
    (0..SEEDED_ATTACHMENTS)
        .map(|index| {
            guard
                .insert_transit_gateway_attachment(TransitGatewayAttachmentView::Vpc(
                    TransitGatewayVpcAttachment {
                        transit_gateway_id: Some(gateway_id.to_string()),
                        vpc_id: Some(format!("vpc-0123456789abcde{index:0>2x}")),
                        vpc_owner_id: Some("210987654321".to_string()),
                        state: Some(TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE.to_string()),
                        ..Default::default()
                    },
                ))
                .expect("the store holds the seeded VPC attachment")
        })
        .collect()
}

/// The emulator with one transit gateway and the middlebox attachments above: the router to
/// call, the engine to read the store through, the gateway's id, and the attachment ids.
async fn seeded() -> (
    MultiServiceRouter,
    std::sync::Arc<engine::InMemoryEngine>,
    String,
    Vec<String>,
) {
    let (router, engine) = super::ec2_query::emulator();
    let gateway_id = transit_gateway(&router).await;
    let attachment_ids = seed_attachments(&engine, &gateway_id);
    (router, engine, gateway_id, attachment_ids)
}

/// The attachment ids at `positions` in the seeded list.
fn at(attachment_ids: &[String], positions: &[usize]) -> Vec<String> {
    positions
        .iter()
        .map(|position| attachment_ids[*position].clone())
        .collect()
}

/// `<prefix>.N` form fields for `ids`, as ec2Query flattens a list of ids.
///
/// The three lists this suite sends are each bound to a singular xmlName —
/// `MiddleboxAttachmentId` on the create, `AddMiddleboxAttachmentId` and
/// `RemoveMiddleboxAttachmentId` on the modify
/// (`smithy_query(query="operation",operation="ModifyTransitGatewayMeteringPolicy")`) — which
/// is the key ec2Query flattens each under.
fn id_params(prefix: &str, ids: &[String]) -> Vec<Param> {
    indexed(prefix, &ids.iter().map(String::as_str).collect::<Vec<_>>())
}

/// A metering policy of `gateway_id` metering the seeded attachments at `positions` and
/// carrying [`KEPT_TAG`], created over the wire, as its `TransitGatewayMeteringPolicyId`.
async fn metering_policy(
    router: &MultiServiceRouter,
    gateway_id: &str,
    attachment_ids: &[String],
    positions: &[usize],
) -> String {
    let mut params = vec![param("TransitGatewayId", gateway_id)];
    params.extend(id_params(
        "MiddleboxAttachmentId",
        &at(attachment_ids, positions),
    ));
    params.push(param(
        "TagSpecifications.1.ResourceType",
        RESOURCE_TYPE_TRANSIT_GATEWAY_METERING_POLICY,
    ));
    params.push(param("TagSpecifications.1.Tag.1.Key", KEPT_TAG.0));
    params.push(param("TagSpecifications.1.Tag.1.Value", KEPT_TAG.1));
    let (status, body) = invoke(router, CREATE, &params).await;
    assert_eq!(
        status, 200,
        "{CREATE} must succeed, got {status} with body {body}"
    );
    member("the created policy", &body, "transitGatewayMeteringPolicyId")
        .expect("a created metering policy reports its own id")
}

/// Read something off the one metering policy a response carries — the element that has a
/// `transitGatewayMeteringPolicyId` child.
fn with_policy<T>(case: &str, body: &str, read: impl FnOnce(roxmltree::Node<'_, '_>) -> T) -> T {
    let parsed = document(case, body);
    let policy = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "transitGatewayMeteringPolicyId").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no metering policy in {body}"));
    read(policy)
}

/// A member of the one metering policy a response carries, or `None` when it carries no such
/// member.
fn member(case: &str, body: &str, name: &str) -> Option<String> {
    with_policy(case, body, |policy| child_text(policy, name))
}

/// Whether the one metering policy a response carries has a `name` child ELEMENT at all.
///
/// The absence check for a member nothing recorded: [`member`] reads an element's TEXT, which
/// a present-but-empty container (`<middleboxAttachmentIdSet/>`) does not carry, so only
/// element absence shows that nothing was reported for it.
fn has_member(case: &str, body: &str, name: &str) -> bool {
    with_policy(case, body, |policy| child(policy, name).is_some())
}

/// The middlebox attachment ids of the one metering policy a response carries, in document
/// order.
fn reported_middlebox_attachments(case: &str, body: &str) -> Vec<String> {
    with_policy(case, body, |policy| {
        set_items(policy, "middleboxAttachmentIdSet")
            .into_iter()
            .map(|item| item.text().unwrap_or_default().to_string())
            .collect()
    })
}

/// The (key, value) pairs of the one metering policy a response carries.
fn reported_tags(case: &str, body: &str) -> Vec<(String, String)> {
    with_policy(case, body, |policy| {
        set_items(policy, "tagSet")
            .into_iter()
            .map(|item| {
                (
                    child_text(item, "key").unwrap_or_default(),
                    child_text(item, "value").unwrap_or_default(),
                )
            })
            .collect()
    })
}

/// The middlebox attachment ids the store holds for one policy, in stored order — `None` when
/// the store holds no such policy.
fn stored_middlebox_attachments(
    engine: &engine::InMemoryEngine,
    metering_policy_id: &str,
) -> Option<Vec<String>> {
    engine
        .state::<Ec2State>()
        .lock()
        .transit_gateway_metering_policies
        .get(metering_policy_id)
        .map(|policy| policy.middlebox_attachment_ids.clone().unwrap_or_default())
}

/// The state the store holds for one policy, or `None` when it holds none.
fn stored_state(engine: &engine::InMemoryEngine, metering_policy_id: &str) -> Option<String> {
    engine
        .state::<Ec2State>()
        .lock()
        .transit_gateway_metering_policies
        .get(metering_policy_id)
        .and_then(|policy| policy.state.clone())
}

/// The `DescribeTransitGatewayMeteringPolicies` call narrowed to `metering_policy_id`.
async fn describe(router: &MultiServiceRouter, metering_policy_id: &str) -> (u16, String) {
    invoke(
        router,
        DESCRIBE,
        &indexed("TransitGatewayMeteringPolicyIds", &[metering_policy_id]),
    )
    .await
}

/// Look at the policy AGAIN until the describe reports it in `wanted`, or a deadline far past
/// the service's own transition span passes — the condition is waited for, never a fixed time,
/// so nothing here depends on how fast the host runs. Answers the whole body of the last look.
async fn poll_until_state(
    case: &str,
    router: &MultiServiceRouter,
    metering_policy_id: &str,
    wanted: &str,
) -> String {
    let deadline = Instant::now() + move_deadline();
    loop {
        let (status, body) = describe(router, metering_policy_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {DESCRIBE}, got {status} with body {body}"
        );
        let state = member(case, &body, "state");
        if state.as_deref() == Some(wanted) {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the policy to reach {wanted:?}, got {state:?} in {body}"
        );
        tokio::time::sleep(poll_interval()).await;
    }
}

/// One modified policy: what it meters before the call, what the request adds and removes, and
/// the set the call must leave it metering.
struct Case {
    /// What the row is, for the assertion messages.
    name: &'static str,
    /// The positions in the seeded attachment list the policy meters before the call.
    metered: &'static [usize],
    /// Whether the policy is left to settle into the state a policy in force is in before the
    /// modify arrives; a row that does not settle is modified while its create is still under
    /// way.
    settled: bool,
    /// The positions the request adds, in request order.
    add: &'static [usize],
    /// The positions the request removes, in request order.
    remove: &'static [usize],
    /// The positions the modified policy must report, in order.
    reports: &'static [usize],
}

/// The changes this suite is written against: an addition to a policy metering nothing, two
/// additions named out of order, one removal out of several, every attachment removed, an
/// addition and a removal in the same request, an addition of something already metered, a
/// request that names neither list, and one that replaces the whole set.
const CASES: &[Case] = &[
    Case {
        name: "one attachment added to a policy metering nothing",
        metered: &[],
        settled: true,
        add: &[1],
        remove: &[],
        reports: &[1],
    },
    Case {
        name: "two attachments added, named out of order, to a policy metering one",
        metered: &[0],
        settled: false,
        add: &[3, 1],
        remove: &[],
        reports: &[0, 3, 1],
    },
    Case {
        name: "one attachment removed, leaving the rest in their stored order",
        metered: &[0, 1, 2],
        settled: true,
        add: &[],
        remove: &[1],
        reports: &[0, 2],
    },
    Case {
        name: "every attachment removed leaves the policy metering nothing",
        metered: &[2, 3],
        settled: false,
        add: &[],
        remove: &[3, 2],
        reports: &[],
    },
    Case {
        name: "one attachment added and another removed by the same request",
        metered: &[0, 1],
        settled: true,
        add: &[2],
        remove: &[0],
        reports: &[1, 2],
    },
    Case {
        name: "an attachment already metered, added twice over, is metered once",
        metered: &[1, 2],
        settled: false,
        add: &[2, 2],
        remove: &[],
        reports: &[1, 2],
    },
    Case {
        name: "a request that names neither list leaves the set as it stands",
        metered: &[0, 3],
        settled: true,
        add: &[],
        remove: &[],
        reports: &[0, 3],
    },
    Case {
        name: "every attachment there is, replacing the whole set",
        metered: &[0],
        settled: false,
        add: &[1, 2, 3],
        remove: &[0],
        reports: &[1, 2, 3],
    },
];

/// The form fields one case's modify carries.
fn modify_params(metering_policy_id: &str, attachment_ids: &[String], case: &Case) -> Vec<Param> {
    let mut params = vec![param("TransitGatewayMeteringPolicyId", metering_policy_id)];
    params.extend(id_params(
        "AddMiddleboxAttachmentId",
        &at(attachment_ids, case.add),
    ));
    params.extend(id_params(
        "RemoveMiddleboxAttachmentId",
        &at(attachment_ids, case.remove),
    ));
    params
}

/// Every fact a modified metering policy carries is the request's additions and removals laid
/// over the set it already metered, it is reported by the modify itself, and it reads back
/// identically through the describe — with the tag the policy carries untouched. One row per
/// case, so no single request is the specification.
#[tokio::test]
async fn a_modify_lays_its_additions_and_removals_over_the_set_the_policy_meters() {
    let (router, engine, gateway_id, attachment_ids) = seeded().await;
    let expected_tags = vec![(KEPT_TAG.0.to_string(), KEPT_TAG.1.to_string())];

    for case in CASES {
        let name = case.name;
        let metering_policy_id =
            metering_policy(&router, &gateway_id, &attachment_ids, case.metered).await;
        if case.settled {
            poll_until_state(name, &router, &metering_policy_id, policy_state("available")).await;
        }

        let (status, body) = invoke(
            &router,
            ACTION,
            &modify_params(&metering_policy_id, &attachment_ids, case),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected HTTP 200 from {ACTION}, got {status} with body {body}"
        );

        let reported_id = member(name, &body, "transitGatewayMeteringPolicyId");
        assert_eq!(
            reported_id.as_deref(),
            Some(metering_policy_id.as_str()),
            "case {name:?}: expected the policy the request named, got {reported_id:?} in {body}"
        );
        let reported_gateway = member(name, &body, "transitGatewayId");
        assert_eq!(
            reported_gateway.as_deref(),
            Some(gateway_id.as_str()),
            "case {name:?}: a modify must leave the gateway the policy meters alone, got \
             {reported_gateway:?} in {body}"
        );
        let reported_state = member(name, &body, "state");
        assert_eq!(
            reported_state.as_deref(),
            Some(policy_state("modifying")),
            "case {name:?}: expected the transient state of a change under way, got \
             {reported_state:?} in {body}"
        );
        let reported_effective = member(name, &body, "updateEffectiveAt");
        assert!(
            reported_effective.as_deref().and_then(as_timestamp).is_some(),
            "case {name:?}: expected the instant the change takes effect, got \
             {reported_effective:?} in {body}"
        );
        let actual_tags = reported_tags(name, &body);
        assert_eq!(
            actual_tags, expected_tags,
            "case {name:?}: a modify must leave the policy's tags alone, got {actual_tags:?} \
             in {body}"
        );

        let expected_attachments = at(&attachment_ids, case.reports);
        if expected_attachments.is_empty() {
            // A change that leaves the policy metering nothing REPORTS that: the member the
            // call is about carries the set the change left behind, which is the empty one —
            // not the absence a policy no attachment was ever named for carries.
            assert!(
                has_member(name, &body, "middleboxAttachmentIdSet"),
                "case {name:?}: a change that empties the set must still report \
                 <middleboxAttachmentIdSet>, got {body}"
            );
        }
        let actual = reported_middlebox_attachments(name, &body);
        assert_eq!(
            actual, expected_attachments,
            "case {name:?}: expected the set the request leaves the policy metering, got \
             {actual:?} in {body}"
        );
        let held = stored_middlebox_attachments(&engine, &metering_policy_id);
        assert_eq!(
            held.as_deref(),
            Some(expected_attachments.as_slice()),
            "case {name:?}: the store must hold the set the modify reported, got {held:?}"
        );

        // The round trip: the describe reports the policy the modify stored, member for
        // member, so the writer and every later reader cannot drift apart. The state is read
        // through the poll below rather than asserted here, because the clock owes this policy
        // the state it is in once the change has taken effect.
        let (status, read_back) = describe(&router, &metering_policy_id).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected HTTP 200 from {DESCRIBE}, got {status} with body {read_back}"
        );
        let read_back_attachments = reported_middlebox_attachments(name, &read_back);
        assert_eq!(
            read_back_attachments, expected_attachments,
            "case {name:?}: expected the stored set from {DESCRIBE}, got \
             {read_back_attachments:?} in {read_back}"
        );
        let read_back_tags = reported_tags(name, &read_back);
        assert_eq!(
            read_back_tags, expected_tags,
            "case {name:?}: expected the stored tags from {DESCRIBE}, got {read_back_tags:?} \
             in {read_back}"
        );
        for element_name in ["transitGatewayMeteringPolicyId", "transitGatewayId"] {
            let expected = member(name, &body, element_name);
            let actual = member(name, &read_back, element_name);
            assert_eq!(
                actual, expected,
                "case {name:?}: expected <{element_name}> {expected:?} from {DESCRIBE}, got \
                 {actual:?} in {read_back}"
            );
        }
    }
}

/// The move the clock owes a change: a policy handed back `modifying` is in force once the
/// transition span has passed, whether or not anybody called again — carrying the set the
/// change left it metering, with the instant that change took effect unmoved. Driven for a
/// change that adds and one that removes, so the move does not depend on what the request
/// asked for.
#[tokio::test]
async fn the_clock_owes_a_modified_policy_the_state_a_policy_in_force_is_in() {
    let (router, engine, gateway_id, attachment_ids) = seeded().await;

    for case in [&CASES[1], &CASES[2]] {
        let name = case.name;
        let metering_policy_id =
            metering_policy(&router, &gateway_id, &attachment_ids, case.metered).await;
        let (status, body) = invoke(
            &router,
            ACTION,
            &modify_params(&metering_policy_id, &attachment_ids, case),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected HTTP 200 from {ACTION}, got {status} with body {body}"
        );
        let effective_at = member(name, &body, "updateEffectiveAt");

        let read_back =
            poll_until_state(name, &router, &metering_policy_id, policy_state("available")).await;
        let still_effective_at = member(name, &read_back, "updateEffectiveAt");
        assert_eq!(
            still_effective_at, effective_at,
            "case {name:?}: the instant the change takes effect must not move, expected \
             {effective_at:?}, got {still_effective_at:?}"
        );
        let expected_attachments = at(&attachment_ids, case.reports);
        let actual = reported_middlebox_attachments(name, &read_back);
        assert_eq!(
            actual, expected_attachments,
            "case {name:?}: a policy the span has passed for still meters the set the change \
             left it, got {actual:?} in {read_back}"
        );
        assert_eq!(
            stored_state(&engine, &metering_policy_id).as_deref(),
            Some(policy_state("available")),
            "case {name:?}: the store must hold the state the describe reported"
        );
    }
}

/// One refused modify: the fields it carries beyond the policy id, and the refusal it must
/// answer.
struct Refusal {
    /// What the row is, for the assertion messages.
    name: &'static str,
    /// The `TransitGatewayMeteringPolicyId` the request names, or `None` for the policy the row
    /// created.
    policy_id: Option<&'static str>,
    /// The `AddMiddleboxAttachmentId.N` values the request names, as literals.
    add: &'static [&'static str],
    /// The positions in the seeded attachment list the request also adds.
    seeded_add: &'static [usize],
    /// The `RemoveMiddleboxAttachmentId.N` values the request names, as literals.
    remove: &'static [&'static str],
    /// The positions in the seeded attachment list the request also removes.
    seeded_remove: &'static [usize],
    /// Whether the policy's deletion is asked for before the modify arrives.
    deleted_first: bool,
    /// Whether the request asks for a dry run.
    dry_run: bool,
    /// The HTTP status it must answer.
    status: u16,
    /// The EC2 error code it must answer.
    code: &'static str,
}

/// The refusal every row below starts from: the row's own policy, nothing else.
const CLEAN: Refusal = Refusal {
    name: "",
    policy_id: None,
    add: &[],
    seeded_add: &[],
    remove: &[],
    seeded_remove: &[],
    deleted_first: false,
    dry_run: false,
    status: REFUSAL_STATUS,
    code: "",
};

/// The attachments the policy of every refusal row meters before the call.
const REFUSED_METERS: &[usize] = &[0, 1];

/// The refusals this suite is written against: a value that is no metering policy id, a
/// well-formed one nothing holds, an id to add and an id to remove that are no attachment ids
/// at all, a well-formed attachment id nothing holds (named beside a usable one, so the refusal
/// is not about the list's length), a removal of an attachment the policy does not meter, one
/// attachment both added and removed, a policy whose deletion has been asked for, and the dry
/// run — which is answered about PERMISSION, so it wins over an id that cannot be resolved but
/// not over a request whose own form is wrong.
fn refusals() -> Vec<Refusal> {
    vec![
        Refusal {
            name: "a value that is not a metering policy id at all",
            policy_id: Some("metering-policy-one"),
            code: INVALID_TRANSIT_GATEWAY_METERING_POLICY_ID_MALFORMED,
            ..CLEAN
        },
        Refusal {
            name: "a metering policy id this account holds no policy for",
            policy_id: Some("tgw-mp-0123456789abcdef0"),
            code: INVALID_TRANSIT_GATEWAY_METERING_POLICY_ID_NOT_FOUND,
            ..CLEAN
        },
        Refusal {
            name: "an id to add that is not an attachment id at all",
            add: &["middlebox-one"],
            code: INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_MALFORMED,
            ..CLEAN
        },
        Refusal {
            name: "an id to remove that is not an attachment id at all",
            remove: &["middlebox-one"],
            code: INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_MALFORMED,
            ..CLEAN
        },
        Refusal {
            name: "a well-formed id to add this account holds no attachment for",
            seeded_add: &[2],
            add: &["tgw-attach-0123456789abcdef0"],
            code: INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_NOT_FOUND,
            ..CLEAN
        },
        Refusal {
            name: "a removal of an attachment the policy does not meter",
            seeded_remove: &[3],
            code: INVALID_PARAMETER_VALUE,
            ..CLEAN
        },
        Refusal {
            name: "one attachment both added and removed",
            seeded_add: &[0],
            seeded_remove: &[0],
            code: INVALID_PARAMETER_COMBINATION,
            ..CLEAN
        },
        Refusal {
            name: "a policy whose deletion has been asked for",
            seeded_add: &[2],
            deleted_first: true,
            code: INCORRECT_STATE,
            ..CLEAN
        },
        Refusal {
            name: "a dry run",
            seeded_add: &[2],
            dry_run: true,
            status: DRY_RUN_STATUS,
            code: DRY_RUN_OPERATION,
            ..CLEAN
        },
        Refusal {
            name: "a dry run naming a metering policy this account holds no policy for",
            policy_id: Some("tgw-mp-0123456789abcdef0"),
            dry_run: true,
            status: DRY_RUN_STATUS,
            code: DRY_RUN_OPERATION,
            ..CLEAN
        },
        Refusal {
            name: "a dry run naming an id to add that is not an attachment id at all",
            add: &["middlebox-one"],
            dry_run: true,
            code: INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_MALFORMED,
            ..CLEAN
        },
    ]
}

/// A request this operation refuses answers that refusal's own code and changes NOTHING, so a
/// modify either applies the whole change or none of it.
#[tokio::test]
async fn an_unusable_modify_is_refused_and_changes_nothing() {
    let (router, engine, gateway_id, attachment_ids) = seeded().await;
    let untouched = at(&attachment_ids, REFUSED_METERS);

    for refusal in refusals() {
        let name = refusal.name;
        let metering_policy_id =
            metering_policy(&router, &gateway_id, &attachment_ids, REFUSED_METERS).await;
        if refusal.deleted_first {
            let (status, body) = invoke(
                &router,
                DELETE,
                &[param("TransitGatewayMeteringPolicyId", &metering_policy_id)],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {name:?}: {DELETE} must succeed, got {status} with body {body}"
            );
        }

        let named = refusal.policy_id.unwrap_or(metering_policy_id.as_str());
        let mut params = vec![param("TransitGatewayMeteringPolicyId", named)];
        let mut added = at(&attachment_ids, refusal.seeded_add);
        added.extend(refusal.add.iter().map(|id| (*id).to_string()));
        params.extend(id_params("AddMiddleboxAttachmentId", &added));
        let mut removed = at(&attachment_ids, refusal.seeded_remove);
        removed.extend(refusal.remove.iter().map(|id| (*id).to_string()));
        params.extend(id_params("RemoveMiddleboxAttachmentId", &removed));
        if refusal.dry_run {
            params.push(param("DryRun", "true"));
        }

        let (status, body) = invoke(&router, ACTION, &params).await;
        assert_eq!(
            status, refusal.status,
            "case {name:?}: expected HTTP {expected} from {ACTION}, got {status} with body \
             {body}",
            expected = refusal.status
        );
        let code = error_code(&body);
        assert_eq!(
            code, refusal.code,
            "case {name:?}: expected the {expected:?} refusal, got {code:?} in {body}",
            expected = refusal.code
        );
        if refusal.code == INVALID_TRANSIT_GATEWAY_METERING_POLICY_ID_NOT_FOUND {
            let message = error_message(&body);
            assert!(
                message.contains(named),
                "case {name:?}: the refusal must name the policy {named}, got {message:?}"
            );
        }

        let held = stored_middlebox_attachments(&engine, &metering_policy_id);
        assert_eq!(
            held.as_deref(),
            Some(untouched.as_slice()),
            "case {name:?}: a refused modify must leave the policy metering what it did, got \
             {held:?}"
        );
        let state = stored_state(&engine, &metering_policy_id);
        assert_ne!(
            state.as_deref(),
            Some(policy_state("modifying")),
            "case {name:?}: a refused modify must not leave the policy in the state of a \
             change under way"
        );
    }
}
