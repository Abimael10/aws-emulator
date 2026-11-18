// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ModifySpotFleetRequest`, driven over the wire the service actually
//! speaks — `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//!
//! The rules under test are:
//!
//! 1. a modification CHANGES THE STORED FLEET and the capacity it holds: "to scale up your Spot
//!    Fleet, increase its target capacity. The Spot Fleet launches the additional Spot Instances
//!    according to the allocation strategy for the Spot Fleet request", and "to scale down your
//!    Spot Fleet, decrease its target capacity. First, the Spot Fleet cancels any open requests
//!    that exceed the new target capacity. You can request that the Spot Fleet terminate Spot
//!    Instances until the size of the fleet no longer exceeds the new target capacity.
//!    Alternatively, you can request that the Spot Fleet keep the fleet at its current size"
//!    (`ModifySpotFleetRequest`) — so the instances the fleet ends up running, and the capacity
//!    it reports fulfilled, follow from the new target capacity and the excess-capacity policy,
//!    never from the call's own say-so;
//! 2. "while the Spot Fleet request is being modified, it is in the `modifying` state", which
//!    the fleet leaves for `active` on the clock — one `fleetRequestChange` event for the
//!    modification that started (`modify_in_progress`) and one for the modification that
//!    completed (`modify_succeeded`), and an `instanceChange` `termination_notified` event for
//!    each instance a scale-down terminated;
//! 3. a request that names no modification, a fleet the account does not hold, a fleet of a type
//!    or in a state a modification does not run from, and a launch template list on a fleet that
//!    was created with its own launch specifications are all REFUSED, and the fleet is left
//!    exactly as it was.
//!
//! Each table row is a distinct input along what those rules generalise over — which direction
//! the capacity moves, how many pools it moves over, what one instance of a pool provides,
//! whether the fleet's first capacity has been fulfilled yet, which excess-capacity policy is in
//! force, whether the launch is described by the fleet's own specifications or by a launch
//! template, and which unservable request is sent — so no row is the specification.

use std::collections::BTreeMap;

use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, EVENT_SUB_TYPE_MODIFY_IN_PROGRESS,
    EVENT_SUB_TYPE_MODIFY_SUCCEEDED, EVENT_SUB_TYPE_TERMINATION_NOTIFIED,
    EVENT_TYPE_FLEET_REQUEST_CHANGE, EVENT_TYPE_INSTANCE_CHANGE, EXCESS_CAPACITY_NO_TERMINATION,
    EXCESS_CAPACITY_TERMINATION_DEFAULT, FLEET_TYPE_REQUEST, INCORRECT_STATE,
    INVALID_SPOT_FLEET_REQUEST_ID_MALFORMED, INVALID_SPOT_FLEET_REQUEST_ID_NOT_FOUND,
    MAX_SPOT_FLEET_SPOT_INSTANCES, MAX_SPOT_FLEET_TARGET_CAPACITY,
    SPOT_FLEET_ALLOCATION_STRATEGY_DIVERSIFIED, SPOT_FLEET_STATE_ACTIVE, UNSUPPORTED_OPERATION,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, error_code, error_message, indexed};
use super::rbin_retention_rules::poll_until;
use super::wire::{child, child_text, document, set_items};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The ARN of the IAM role a Spot Fleet request grants the fleet. Its account is NOT this
/// emulator's: the role is the caller's.
const FLEET_ROLE: &str = "arn:aws:iam::111122223333:role/aws-ec2-spot-fleet-tagging-role";

/// A well-formed Spot Fleet request id no fleet of the account is stored under: the mint counts
/// from one, so a high counter value names nothing.
const UNKNOWN_FLEET_ID: &str = "sfr-0000ffff-0000-0000-0000-00000000ffff";

/// Something that is not a Spot Fleet request id at all.
const MALFORMED_FLEET_ID: &str = "sfr-not-an-id";

/// How close a reported capacity has to be to the expected one: the capacities here are whole
/// units of a pool's weight, so this only absorbs the representation of a sum of `f64`s.
const CAPACITY_TOLERANCE: f64 = 1e-9;

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One `SpotFleetRequestConfig.<member>` parameter of a `RequestSpotFleet`.
fn c(member: &str, value: impl Into<String>) -> (String, String) {
    p(&format!("SpotFleetRequestConfig.{member}"), value)
}

fn emulator() -> (MultiServiceRouter, std::sync::Arc<engine::InMemoryEngine>) {
    super::ec2_query::emulator()
}

/// An AMI of the published catalog a fresh account can already see: read out of the service's
/// OWN state rather than written here, because a launch must name an image the account can
/// resolve.
fn image_id() -> String {
    Ec2State::default()
        .images
        .keys()
        .next()
        .cloned()
        .expect("a fresh account must publish at least one image")
}

/// How a row describes the launches its pools make.
enum Launch {
    /// The fleet's own `LaunchSpecifications`: one entry per pool, with its instance type and
    /// what one of its instances provides.
    Specifications(&'static [(&'static str, Option<f64>)]),
    /// One `LaunchTemplateConfigs` entry whose template carries `instance_type`, which is the
    /// only kind of fleet a modification may re-point ("you can only use this parameter if you
    /// specified a launch template … in your Spot Fleet request",
    /// `ModifySpotFleetRequestRequest$LaunchTemplateConfigs`).
    Template { instance_type: &'static str },
}

/// One successful-modification row.
struct Row {
    case: &'static str,
    /// `SpotFleetRequestConfig.TargetCapacity` the fleet is created with.
    target_capacity: u32,
    /// Whether the fleet spreads its target across every pool.
    diversified: bool,
    launch: Launch,
    /// Whether the row waits for the fleet's first capacity to be FULFILLED before modifying
    /// it: a modification of a fleet whose requests are still open cancels those requests,
    /// while one of a fulfilled fleet has running instances to terminate.
    fulfilled_first: bool,
    /// `TargetCapacity` the modification asks for.
    new_target_capacity: Option<u32>,
    /// `ExcessCapacityTerminationPolicy` the modification asks for, if any.
    policy: Option<&'static str>,
    /// The instance type a replacement `LaunchTemplateConfigs` override names, for a row that
    /// re-points the pools of a launch-template fleet.
    retarget_template_type: Option<&'static str>,
    /// How many instances of each type the fleet must be running once the modification has
    /// settled, by type.
    expected: &'static [(&'static str, usize)],
    /// The `FulfilledCapacity` it must report then.
    expected_fulfilled: f64,
    /// Whether the modification had to notify the termination of at least one instance.
    expects_termination_notified: bool,
}

/// The rows. No two share a direction, pool count, weight, fulfillment, policy and launch
/// combination.
const ROWS: &[Row] = &[
    Row {
        case: "a fulfilled one-pool fleet scaled up from one unit to three",
        target_capacity: 1,
        diversified: false,
        launch: Launch::Specifications(&[("t3.micro", None)]),
        fulfilled_first: true,
        new_target_capacity: Some(3),
        policy: None,
        retarget_template_type: None,
        expected: &[("t3.micro", 3)],
        expected_fulfilled: 3.0,
        expects_termination_notified: false,
    },
    Row {
        case: "a fleet scaled up before its first requests are fulfilled",
        target_capacity: 1,
        diversified: false,
        launch: Launch::Specifications(&[("m5.large", Some(1.0))]),
        fulfilled_first: false,
        new_target_capacity: Some(2),
        policy: None,
        retarget_template_type: None,
        expected: &[("m5.large", 2)],
        expected_fulfilled: 2.0,
        expects_termination_notified: false,
    },
    Row {
        case: "a diversified fleet scaled up across both its pools",
        target_capacity: 2,
        diversified: true,
        launch: Launch::Specifications(&[("t3.small", Some(1.0)), ("c6g.medium", Some(1.0))]),
        fulfilled_first: true,
        new_target_capacity: Some(4),
        policy: None,
        retarget_template_type: None,
        expected: &[("t3.small", 2), ("c6g.medium", 2)],
        expected_fulfilled: 4.0,
        expects_termination_notified: false,
    },
    Row {
        case: "a fleet whose instance provides two units scaled up to six",
        target_capacity: 2,
        diversified: false,
        launch: Launch::Specifications(&[("c5.xlarge", Some(2.0))]),
        fulfilled_first: true,
        new_target_capacity: Some(6),
        policy: None,
        retarget_template_type: None,
        expected: &[("c5.xlarge", 3)],
        expected_fulfilled: 6.0,
        expects_termination_notified: false,
    },
    Row {
        case: "a fulfilled fleet scaled down terminates the excess instances by default",
        target_capacity: 3,
        diversified: false,
        launch: Launch::Specifications(&[("t3.medium", None)]),
        fulfilled_first: true,
        new_target_capacity: Some(1),
        policy: Some(EXCESS_CAPACITY_TERMINATION_DEFAULT),
        retarget_template_type: None,
        expected: &[("t3.medium", 1)],
        expected_fulfilled: 1.0,
        expects_termination_notified: true,
    },
    Row {
        case: "a fulfilled fleet scaled down under noTermination keeps its instances",
        target_capacity: 2,
        diversified: false,
        launch: Launch::Specifications(&[("t3.large", Some(1.0))]),
        fulfilled_first: true,
        new_target_capacity: Some(1),
        policy: Some(EXCESS_CAPACITY_NO_TERMINATION),
        retarget_template_type: None,
        expected: &[("t3.large", 2)],
        expected_fulfilled: 2.0,
        expects_termination_notified: false,
    },
    Row {
        case: "a fleet scaled down before fulfillment cancels the open requests",
        target_capacity: 3,
        diversified: false,
        launch: Launch::Specifications(&[("c5.large", Some(1.0))]),
        fulfilled_first: false,
        new_target_capacity: Some(1),
        policy: None,
        retarget_template_type: None,
        expected: &[("c5.large", 1)],
        expected_fulfilled: 1.0,
        expects_termination_notified: false,
    },
    Row {
        case: "a launch-template fleet re-pointed at another instance type as it scales up",
        target_capacity: 1,
        diversified: false,
        launch: Launch::Template {
            instance_type: "m5.xlarge",
        },
        fulfilled_first: true,
        new_target_capacity: Some(2),
        policy: None,
        retarget_template_type: Some("t4g.small"),
        expected: &[("m5.xlarge", 1), ("t4g.small", 1)],
        expected_fulfilled: 2.0,
        expects_termination_notified: false,
    },
];

impl Row {
    /// The expected instance types as a count by type.
    fn expected_counts(&self) -> BTreeMap<String, usize> {
        self.expected
            .iter()
            .map(|(instance_type, count)| ((*instance_type).to_string(), *count))
            .collect()
    }
}

/// Create the launch template one row needs, and answer its id.
async fn create_template(
    case: &str,
    router: &MultiServiceRouter,
    name: &str,
    instance_type: &str,
) -> String {
    let (status, body) = call(
        router,
        REGION,
        "CreateLaunchTemplate",
        &[
            p("LaunchTemplateName", name),
            p("LaunchTemplateData.ImageId", image_id()),
            p("LaunchTemplateData.InstanceType", instance_type),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateLaunchTemplate must succeed, got {status}: {body}"
    );
    let parsed = document(case, &body);
    child(parsed.root_element(), "launchTemplate")
        .and_then(|template| child_text(template, "launchTemplateId"))
        .unwrap_or_else(|| panic!("case {case:?}: no launchTemplateId in {body}"))
}

/// Request one Spot Fleet for `row`, and answer its id beside the launch template id it was
/// created with, if any.
async fn requested_fleet(
    case: &str,
    router: &MultiServiceRouter,
    row: &Row,
    index: usize,
) -> (String, Option<String>) {
    let mut params: Params = vec![
        c("IamFleetRole", FLEET_ROLE),
        c("TargetCapacity", row.target_capacity.to_string()),
    ];
    if row.diversified {
        params.push(c(
            "AllocationStrategy",
            SPOT_FLEET_ALLOCATION_STRATEGY_DIVERSIFIED,
        ));
    }
    let mut template_id = None;
    match row.launch {
        Launch::Specifications(pools) => {
            for (position, (instance_type, weight)) in (1..).zip(pools) {
                params.push(c(
                    &format!("LaunchSpecifications.{position}.ImageId"),
                    image_id(),
                ));
                params.push(c(
                    &format!("LaunchSpecifications.{position}.InstanceType"),
                    *instance_type,
                ));
                if let Some(weight) = weight {
                    params.push(c(
                        &format!("LaunchSpecifications.{position}.WeightedCapacity"),
                        weight.to_string(),
                    ));
                }
            }
        }
        Launch::Template { instance_type } => {
            let created = create_template(
                case,
                router,
                &format!("modify-spot-fleet-{index}"),
                instance_type,
            )
            .await;
            params.push(c(
                "LaunchTemplateConfigs.1.LaunchTemplateSpecification.LaunchTemplateId",
                created.clone(),
            ));
            template_id = Some(created);
        }
    }
    let (status, body) = call(router, REGION, "RequestSpotFleet", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: RequestSpotFleet must succeed, got {status}: {body}"
    );
    let parsed = document(case, &body);
    let fleet_id = child_text(parsed.root_element(), "spotFleetRequestId")
        .unwrap_or_else(|| panic!("case {case:?}: no spotFleetRequestId in {body}"));
    (fleet_id, template_id)
}

/// One `ModifySpotFleetRequest` call.
async fn modify(router: &MultiServiceRouter, params: &[(String, String)]) -> (u16, String) {
    call(router, REGION, "ModifySpotFleetRequest", params).await
}

/// What a `DescribeSpotFleetRequests` reports about one fleet.
#[derive(Debug, Clone, PartialEq)]
struct Described {
    state: String,
    activity_status: String,
    target_capacity: f64,
    fulfilled_capacity: f64,
    excess_capacity_termination_policy: Option<String>,
    context: Option<String>,
}

/// The one fleet `fleet_id` names, as a describe reports it.
async fn described(case: &str, router: &MultiServiceRouter, fleet_id: &str) -> Described {
    let (status, body) = call(
        router,
        REGION,
        "DescribeSpotFleetRequests",
        &indexed("SpotFleetRequestId", &[fleet_id]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeSpotFleetRequests must succeed, got {status}: {body}"
    );
    let parsed = document(case, &body);
    let item = set_items(parsed.root_element(), "spotFleetRequestConfigSet")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: no fleet reported for {fleet_id} in {body}"));
    let config = child(item, "spotFleetRequestConfig")
        .unwrap_or_else(|| panic!("case {case:?}: no spotFleetRequestConfig in {body}"));
    let number = |name: &str| {
        child_text(config, name)
            .and_then(|text| text.parse::<f64>().ok())
            .unwrap_or_else(|| panic!("case {case:?}: no numeric <{name}> in {body}"))
    };
    Described {
        state: child_text(item, "spotFleetRequestState").unwrap_or_default(),
        activity_status: child_text(item, "activityStatus").unwrap_or_default(),
        target_capacity: number("targetCapacity"),
        fulfilled_capacity: number("fulfilledCapacity"),
        excess_capacity_termination_policy: child_text(
            config,
            "excessCapacityTerminationPolicy",
        ),
        context: child_text(config, "context"),
    }
}

/// The instance types `DescribeSpotFleetInstances` reports for one fleet, counted by type.
async fn running_counts(
    case: &str,
    router: &MultiServiceRouter,
    fleet_id: &str,
) -> BTreeMap<String, usize> {
    let (status, body) = call(
        router,
        REGION,
        "DescribeSpotFleetInstances",
        &[p("SpotFleetRequestId", fleet_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeSpotFleetInstances must succeed, got {status}: {body}"
    );
    let parsed = document(case, &body);
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for item in set_items(parsed.root_element(), "activeInstanceSet") {
        let instance_type = child_text(item, "instanceType").unwrap_or_default();
        *counts.entry(instance_type).or_default() += 1;
    }
    counts
}

/// The `(eventType, eventSubType)` pairs of one fleet's whole history, in report order.
async fn history_kinds(
    case: &str,
    router: &MultiServiceRouter,
    fleet_id: &str,
) -> Vec<(String, String)> {
    let (status, body) = call(
        router,
        REGION,
        "DescribeSpotFleetRequestHistory",
        &[
            p("SpotFleetRequestId", fleet_id),
            // The epoch: every event of a fleet created by this suite is later than it, so the
            // window is the fleet's whole history.
            p("StartTime", "1970-01-01T00:00:00Z"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeSpotFleetRequestHistory must succeed, got {status}: {body}"
    );
    let parsed = document(case, &body);
    set_items(parsed.root_element(), "historyRecordSet")
        .into_iter()
        .map(|item| {
            (
                child_text(item, "eventType").unwrap_or_default(),
                child(item, "eventInformation")
                    .and_then(|information| child_text(information, "eventSubType"))
                    .unwrap_or_default(),
            )
        })
        .collect()
}

/// Rule 1 and rule 2: the capacity a modification leaves the fleet holding is the capacity the
/// new target capacity and the excess-capacity policy call for, and the fleet passes through
/// `modifying` on its way back to `active`, recording both halves of the modification.
#[tokio::test]
async fn a_modification_moves_the_fleet_to_the_capacity_the_new_target_calls_for() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let (router, _engine) = emulator();
        let (fleet_id, template_id) = requested_fleet(case, &router, row, index).await;
        let probe_router = &router;
        let probe_fleet = fleet_id.as_str();

        if row.fulfilled_first {
            // Wait for the CONDITION a fulfilled fleet is in — its first capacity launched —
            // rather than for a span, then modify it.
            let target = f64::from(row.target_capacity);
            poll_until(case, "the fleet's first capacity to be fulfilled", || async move {
                let counts = running_counts(case, probe_router, probe_fleet).await;
                counts.values().sum::<usize>() > 0
                    && described(case, probe_router, probe_fleet)
                        .await
                        .fulfilled_capacity
                        >= target
            })
            .await;
        }

        let mut params: Params = vec![p("SpotFleetRequestId", fleet_id.clone())];
        if let Some(new_target_capacity) = row.new_target_capacity {
            params.push(p("TargetCapacity", new_target_capacity.to_string()));
        }
        if let Some(policy) = row.policy {
            params.push(p("ExcessCapacityTerminationPolicy", policy));
        }
        if let Some(instance_type) = row.retarget_template_type {
            let template_id = template_id
                .clone()
                .unwrap_or_else(|| panic!("case {case:?}: this row has no launch template"));
            params.push(p(
                "LaunchTemplateConfig.1.LaunchTemplateSpecification.LaunchTemplateId",
                template_id,
            ));
            params.push(p(
                "LaunchTemplateConfig.1.Overrides.1.InstanceType",
                instance_type,
            ));
        }
        let (status, body) = modify(&router, &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: ModifySpotFleetRequest must succeed, got {status}: {body}"
        );
        let parsed = document(case, &body);
        assert_eq!(
            child_text(parsed.root_element(), "return").as_deref(),
            Some("true"),
            "case {case:?}: a modification that succeeded returns true, got {body}"
        );

        // The fleet settles on the capacity the new target calls for: the instances it runs and
        // the capacity it reports fulfilled are the same answer, reached by the clock.
        let expected_counts = row.expected_counts();
        let expected = &expected_counts;
        let expected_fulfilled = row.expected_fulfilled;
        poll_until(case, "the modified fleet to report its new capacity", || async move {
            let reported = described(case, probe_router, probe_fleet).await;
            reported.state == SPOT_FLEET_STATE_ACTIVE
                && (reported.fulfilled_capacity - expected_fulfilled).abs() < CAPACITY_TOLERANCE
                && running_counts(case, probe_router, probe_fleet).await == *expected
        })
        .await;

        let reported = described(case, &router, &fleet_id).await;
        assert_eq!(
            reported.target_capacity,
            f64::from(row.new_target_capacity.unwrap_or(row.target_capacity)),
            "case {case:?}: the fleet reports the target capacity the modification set, \
             got {reported:?}"
        );
        if let Some(policy) = row.policy {
            assert_eq!(
                reported.excess_capacity_termination_policy.as_deref(),
                Some(policy),
                "case {case:?}: the fleet reports the excess-capacity policy the modification \
                 set, got {reported:?}"
            );
        }
        let counts = running_counts(case, &router, &fleet_id).await;
        assert_eq!(
            counts, expected_counts,
            "case {case:?}: expected the fleet to run {expected_counts:?}, got {counts:?}"
        );

        // Rule 2: both halves of the modification are events of the fleet's history, and a
        // scale-down that terminated an instance notified that termination.
        let kinds = history_kinds(case, &router, &fleet_id).await;
        for sub_type in [
            EVENT_SUB_TYPE_MODIFY_IN_PROGRESS,
            EVENT_SUB_TYPE_MODIFY_SUCCEEDED,
        ] {
            assert!(
                kinds.contains(&(
                    EVENT_TYPE_FLEET_REQUEST_CHANGE.to_string(),
                    sub_type.to_string()
                )),
                "case {case:?}: the fleet's history must carry a \
                 {EVENT_TYPE_FLEET_REQUEST_CHANGE} {sub_type} event, got {kinds:?}"
            );
        }
        let notified = kinds.contains(&(
            EVENT_TYPE_INSTANCE_CHANGE.to_string(),
            EVENT_SUB_TYPE_TERMINATION_NOTIFIED.to_string(),
        ));
        assert_eq!(
            notified, row.expects_termination_notified,
            "case {case:?}: expected a {EVENT_SUB_TYPE_TERMINATION_NOTIFIED} event: \
             {}, got history {kinds:?}",
            row.expects_termination_notified
        );
    }
}

/// How a refusal row names the fleet it modifies.
enum Fleet {
    /// A fleet of type `maintain` this suite created, which is the only kind a modification
    /// runs from.
    Maintain,
    /// A fleet of type `request`, which "only places the required requests"
    /// (`SpotFleetRequestConfigData$Type`) and cannot be modified.
    OneTimeRequest,
    /// A fleet of type `maintain` this suite created AND then cancelled.
    Cancelled,
    /// An id of the right form the account holds no fleet under.
    Unknown,
    /// Something that is not a Spot Fleet request id at all.
    Malformed,
    /// No `SpotFleetRequestId` at all.
    Unnamed,
    /// A `SpotFleetRequestId` that arrives empty, which the handler itself answers for.
    Empty,
}

/// One refusal row: a request this operation cannot serve, and the answer it gets.
struct Refusal {
    case: &'static str,
    fleet: Fleet,
    /// The modification parameters beside the fleet id.
    params: &'static [(&'static str, &'static str)],
    expected_code: &'static str,
    /// A fragment the refusal's message must carry.
    expected_message: &'static str,
}

/// The refusal rows. No row repeats another's reason.
const REFUSALS: &[Refusal] = &[
    Refusal {
        case: "a request that names no modification at all",
        fleet: Fleet::Maintain,
        params: &[],
        expected_code: "MissingParameter",
        expected_message: "targetCapacity",
    },
    Refusal {
        case: "a request that names no fleet",
        fleet: Fleet::Unnamed,
        params: &[("TargetCapacity", "2")],
        expected_code: "MissingParameter",
        expected_message: "SpotFleetRequestId",
    },
    Refusal {
        case: "a request whose fleet id is empty",
        fleet: Fleet::Empty,
        params: &[("TargetCapacity", "2")],
        expected_code: "MissingParameter",
        expected_message: "SpotFleetRequestId",
    },
    Refusal {
        case: "a negative target capacity",
        fleet: Fleet::Maintain,
        params: &[("TargetCapacity", "-1")],
        expected_code: "InvalidParameterValue",
        expected_message: "targetCapacity",
    },
    Refusal {
        case: "a target capacity above the capacity one fleet may be asked for",
        fleet: Fleet::Maintain,
        params: &[("TargetCapacity", "2147483647")],
        expected_code: "InvalidParameterValue",
        expected_message: "targetCapacity",
    },
    Refusal {
        case: "an On-Demand target capacity above that same ceiling",
        fleet: Fleet::Maintain,
        params: &[("OnDemandTargetCapacity", "2147483647")],
        expected_code: "InvalidParameterValue",
        expected_message: "onDemandTargetCapacity",
    },
    Refusal {
        case: "On-Demand capacity above the fleet's target capacity",
        fleet: Fleet::Maintain,
        params: &[("TargetCapacity", "1"), ("OnDemandTargetCapacity", "5")],
        expected_code: "InvalidParameterCombination",
        expected_message: "onDemandTargetCapacity",
    },
    Refusal {
        case: "a launch template list on a fleet created with launch specifications",
        fleet: Fleet::Maintain,
        params: &[(
            "LaunchTemplateConfig.1.LaunchTemplateSpecification.LaunchTemplateName",
            "a-template",
        )],
        expected_code: "InvalidParameterCombination",
        expected_message: "launchTemplateConfigs",
    },
    Refusal {
        case: "a fleet of type request",
        fleet: Fleet::OneTimeRequest,
        params: &[("TargetCapacity", "2")],
        expected_code: UNSUPPORTED_OPERATION,
        expected_message: "maintain",
    },
    Refusal {
        case: "a fleet that has already been cancelled",
        fleet: Fleet::Cancelled,
        params: &[("TargetCapacity", "2")],
        expected_code: INCORRECT_STATE,
        expected_message: "Spot fleet request",
    },
    Refusal {
        case: "an id the account holds no fleet under",
        fleet: Fleet::Unknown,
        params: &[("TargetCapacity", "2")],
        expected_code: INVALID_SPOT_FLEET_REQUEST_ID_NOT_FOUND,
        expected_message: UNKNOWN_FLEET_ID,
    },
    Refusal {
        case: "something that is not a Spot Fleet request id",
        fleet: Fleet::Malformed,
        params: &[("TargetCapacity", "2")],
        expected_code: INVALID_SPOT_FLEET_REQUEST_ID_MALFORMED,
        expected_message: MALFORMED_FLEET_ID,
    },
];

/// Request one `maintain` fleet of one unit over one pool, for a refusal row to name.
async fn refusal_fleet(case: &str, router: &MultiServiceRouter, fleet_type: Option<&str>) -> String {
    let mut params: Params = vec![
        c("IamFleetRole", FLEET_ROLE),
        c("TargetCapacity", "1"),
        c("LaunchSpecifications.1.ImageId", image_id()),
        c("LaunchSpecifications.1.InstanceType", "t3.micro"),
    ];
    if let Some(fleet_type) = fleet_type {
        params.push(c("Type", fleet_type));
    }
    let (status, body) = call(router, REGION, "RequestSpotFleet", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: RequestSpotFleet must succeed, got {status}: {body}"
    );
    let parsed = document(case, &body);
    child_text(parsed.root_element(), "spotFleetRequestId")
        .unwrap_or_else(|| panic!("case {case:?}: no spotFleetRequestId in {body}"))
}

/// Rule 3: a request this operation cannot serve is refused with the code and sentence its
/// reason calls for, and the fleet it named is left exactly as a describe reported it.
#[tokio::test]
async fn an_unservable_modification_is_refused_and_changes_nothing() {
    for row in REFUSALS {
        let case = row.case;
        let (router, _engine) = emulator();
        let (fleet_id, named) = match row.fleet {
            Fleet::Maintain => {
                let fleet_id = refusal_fleet(case, &router, None).await;
                (Some(fleet_id.clone()), Some(fleet_id))
            }
            Fleet::OneTimeRequest => {
                let fleet_id = refusal_fleet(case, &router, Some(FLEET_TYPE_REQUEST)).await;
                (Some(fleet_id.clone()), Some(fleet_id))
            }
            Fleet::Cancelled => {
                let fleet_id = refusal_fleet(case, &router, None).await;
                let (status, body) = call(
                    &router,
                    REGION,
                    "CancelSpotFleetRequests",
                    &[
                        p("SpotFleetRequestId.1", fleet_id.clone()),
                        p("TerminateInstances", "true"),
                    ],
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: CancelSpotFleetRequests must succeed, got {status}: {body}"
                );
                (Some(fleet_id.clone()), Some(fleet_id))
            }
            Fleet::Unknown => (None, Some(UNKNOWN_FLEET_ID.to_string())),
            Fleet::Malformed => (None, Some(MALFORMED_FLEET_ID.to_string())),
            Fleet::Empty => (None, Some(String::new())),
            Fleet::Unnamed => (None, None),
        };

        // What a describe reported before the refused call, for the rows that name a fleet the
        // account holds.
        let before = match fleet_id.as_deref() {
            Some(fleet_id) => Some(described(case, &router, fleet_id).await),
            None => None,
        };

        let mut params: Params = Vec::new();
        if let Some(named) = named {
            params.push(p("SpotFleetRequestId", named));
        }
        for (key, value) in row.params {
            params.push(p(key, *value));
        }
        let (status, body) = modify(&router, &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from ModifySpotFleetRequest, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            row.expected_code,
            "case {case:?}: expected the code {}, got {body}",
            row.expected_code
        );
        let message = error_message(&body);
        assert!(
            message.contains(row.expected_message),
            "case {case:?}: expected a message carrying {:?}, got {message:?}",
            row.expected_message
        );

        // Nothing the refused call named was changed by it.
        if let (Some(fleet_id), Some(before)) = (fleet_id.as_deref(), before) {
            let after = described(case, &router, fleet_id).await;
            assert_eq!(
                after.target_capacity, before.target_capacity,
                "case {case:?}: a refused modification leaves the target capacity alone, \
                 expected {before:?}, got {after:?}"
            );
            assert_eq!(
                after.excess_capacity_termination_policy,
                before.excess_capacity_termination_policy,
                "case {case:?}: a refused modification leaves the excess-capacity policy \
                 alone, expected {before:?}, got {after:?}"
            );
            let kinds = history_kinds(case, &router, fleet_id).await;
            assert!(
                !kinds.contains(&(
                    EVENT_TYPE_FLEET_REQUEST_CHANGE.to_string(),
                    EVENT_SUB_TYPE_MODIFY_IN_PROGRESS.to_string()
                )),
                "case {case:?}: a refused modification records no \
                 {EVENT_SUB_TYPE_MODIFY_IN_PROGRESS} event, got {kinds:?}"
            );
        }
    }
}

/// The `Context` member — "Reserved" (`ModifySpotFleetRequestRequest$Context`) — is carried onto
/// the fleet's stored configuration, so nothing a caller sent is dropped and a describe reports
/// exactly what the modification set.
#[tokio::test]
async fn the_context_a_modification_names_round_trips_through_the_describe() {
    let case = "a modification that names only a context";
    let (router, _engine) = emulator();
    let fleet_id = refusal_fleet(case, &router, None).await;
    let before = described(case, &router, &fleet_id).await;
    assert_eq!(
        before.context, None,
        "case {case:?}: a fleet created with no context reports none, got {before:?}"
    );

    let (status, body) = modify(
        &router,
        &[
            p("SpotFleetRequestId", fleet_id.clone()),
            p("Context", "reserved-context"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ModifySpotFleetRequest must succeed, got {status}: {body}"
    );
    let probe_router = &router;
    let probe_fleet = fleet_id.as_str();
    poll_until(case, "the modified fleet to report the context", || async move {
        described(case, probe_router, probe_fleet)
            .await
            .context
            .as_deref()
            == Some("reserved-context")
    })
    .await;
    let after = described(case, &router, &fleet_id).await;
    assert_eq!(
        after.target_capacity, before.target_capacity,
        "case {case:?}: a modification that names only a context leaves the target capacity \
         alone, expected {before:?}, got {after:?}"
    );
}

/// The ceiling this store holds one fleet's capacity to is a bound, not a wall: the capacity AT
/// the ceiling is accepted and reported, while the refusal table above covers the values past
/// it. A fleet asked for more capacity than this emulator can place keeps reporting its capacity
/// as not yet fulfilled, which is what AWS reports for a fleet below its target.
#[tokio::test]
async fn the_capacity_at_the_published_ceiling_is_accepted() {
    let case = "a target capacity exactly at the ceiling";
    let (router, _engine) = emulator();
    let fleet_id = refusal_fleet(case, &router, None).await;

    let (status, body) = modify(
        &router,
        &[
            p("SpotFleetRequestId", fleet_id.clone()),
            p("TargetCapacity", MAX_SPOT_FLEET_TARGET_CAPACITY.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ModifySpotFleetRequest must succeed at the ceiling, got {status}: {body}"
    );
    let reported = described(case, &router, &fleet_id).await;
    assert_eq!(
        reported.target_capacity,
        f64::from(MAX_SPOT_FLEET_TARGET_CAPACITY),
        "case {case:?}: the fleet reports the capacity the modification set, got {reported:?}"
    );
    assert!(
        reported.fulfilled_capacity < f64::from(MAX_SPOT_FLEET_TARGET_CAPACITY),
        "case {case:?}: a fleet asked for more capacity than this emulator places reports \
         less than its target as fulfilled, got {reported:?}"
    );
    let instances = running_counts(case, &router, &fleet_id).await;
    let placed: usize = instances.values().sum();
    assert!(
        placed <= MAX_SPOT_FLEET_SPOT_INSTANCES,
        "case {case:?}: at most {MAX_SPOT_FLEET_SPOT_INSTANCES} instance(s) are placed for one \
         fleet, got {instances:?}"
    );
}
