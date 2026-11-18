// SPDX-License-Identifier: CC0-1.0

//! The `InstancesDistribution` a group launching from a `MixedInstancesPolicy` runs under.
//!
//! A mixed instances policy's `InstancesDistribution` is how the group spreads its capacity
//! over On-Demand and Spot Instances, and the group spreads it ONE way whichever members
//! the request stated — so every member of that structure whose own documentation states a
//! default reads back carrying it, rather than as a null a client would have to know the
//! default of. The defaults are the model's own
//! (`smithy_query(query="doc",shape="InstancesDistribution",member="…")`):
//!
//! * `OnDemandAllocationStrategy`: `lowest-price` is "the default value for Auto Scaling
//!   groups that specify InstanceRequirements" and `prioritized` "the default value for
//!   Auto Scaling groups that don't specify InstanceRequirements";
//! * `OnDemandBaseCapacity`: "Default: 0";
//! * `OnDemandPercentageAboveBaseCapacity`: "Default: 100";
//! * `SpotAllocationStrategy`: `lowest-price` — "this is the default value";
//! * `SpotInstancePools`: "Default: 2", "valid only when the SpotAllocationStrategy is
//!   `lowest-price`";
//! * `SpotMaxPrice`: no default is documented, so a request that states none leaves the
//!   member absent.
//!
//! The tables below span what that rule generalises over: whether the request carries an
//! `InstancesDistribution` at all, which of its members it states, what kind of override
//! the policy carries (an instance type named outright, attribute-based requirements, none
//! at all), and which of the two operations that write a group's launch source wrote it.
//! Every row goes over the awsQuery wire through one router.

use crate::support::{borrowed, new_engine, p, query, router_with, seed_subnets, tag_value};

/// The launch template of the policy every group here launches from, seeded by the harness
/// in the account's own Amazon EC2 store.
const POLICY_TEMPLATE: (&str, &str) = (
    "MixedInstancesPolicy.LaunchTemplate.LaunchTemplateSpecification.LaunchTemplateName",
    "lt-1",
);

/// What kind of override the policy under test carries, which is what decides
/// `OnDemandAllocationStrategy`'s own default.
#[derive(Clone, Copy)]
enum Override {
    /// No override at all: the policy launches the template's own instance type.
    None,
    /// One override naming its instance type outright.
    InstanceType,
    /// One override stating `InstanceRequirements` instead, so the group is one "that
    /// specifies InstanceRequirements".
    Requirements,
}

impl Override {
    /// The create parameters this override contributes.
    fn params(self) -> Vec<(String, String)> {
        match self {
            Self::None => Vec::new(),
            Self::InstanceType => vec![p(
                "MixedInstancesPolicy.LaunchTemplate.Overrides.member.1.InstanceType",
                "m5.large",
            )],
            Self::Requirements => vec![
                p(
                    "MixedInstancesPolicy.LaunchTemplate.Overrides.member.1.\
                     InstanceRequirements.VCpuCount.Min",
                    "2",
                ),
                p(
                    "MixedInstancesPolicy.LaunchTemplate.Overrides.member.1.\
                     InstanceRequirements.MemoryMiB.Min",
                    "1024",
                ),
            ],
        }
    }
}

/// The `InstancesDistribution` members one group reports, read back through
/// `DescribeAutoScalingGroups`. Each is `""` when the member is absent.
struct ReportedDistribution {
    on_demand_allocation_strategy: String,
    on_demand_base_capacity: String,
    on_demand_percentage_above_base_capacity: String,
    spot_allocation_strategy: String,
    spot_instance_pools: String,
    spot_max_price: String,
}

/// Read one group's reported instances distribution.
async fn reported_distribution(
    router: &smithy_server_runtime::MultiServiceRouter,
    group: &str,
) -> ReportedDistribution {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "reading the group {group:?} back: expected 200, got {status}: {body}"
    );
    assert!(
        body.contains("<InstancesDistribution"),
        "the group {group:?} launches from a mixed instances policy, so it reports an \
         InstancesDistribution, got {body}"
    );
    ReportedDistribution {
        on_demand_allocation_strategy: tag_value(&body, "OnDemandAllocationStrategy"),
        on_demand_base_capacity: tag_value(&body, "OnDemandBaseCapacity"),
        on_demand_percentage_above_base_capacity: tag_value(
            &body,
            "OnDemandPercentageAboveBaseCapacity",
        ),
        spot_allocation_strategy: tag_value(&body, "SpotAllocationStrategy"),
        spot_instance_pools: tag_value(&body, "SpotInstancePools"),
        spot_max_price: tag_value(&body, "SpotMaxPrice"),
    }
}

/// One row of the distribution table.
struct DistributionCase {
    case: &'static str,
    /// The override the policy carries.
    overrides: Override,
    /// The `InstancesDistribution` members the request states, as wire parameters under
    /// `MixedInstancesPolicy.InstancesDistribution.`.
    stated: &'static [(&'static str, &'static str)],
    /// What the group then reports, `""` for a member it leaves absent.
    expected_on_demand_allocation_strategy: &'static str,
    expected_on_demand_base_capacity: &'static str,
    expected_on_demand_percentage: &'static str,
    expected_spot_allocation_strategy: &'static str,
    expected_spot_instance_pools: &'static str,
    expected_spot_max_price: &'static str,
}

/// Every row of the table, shared by the create and the update test so the same rule is
/// asserted of both writers of a group's launch source.
fn distribution_cases() -> Vec<DistributionCase> {
    vec![
        DistributionCase {
            case: "no InstancesDistribution at all, overrides naming an instance type",
            overrides: Override::InstanceType,
            stated: &[],
            expected_on_demand_allocation_strategy: "prioritized",
            expected_on_demand_base_capacity: "0",
            expected_on_demand_percentage: "100",
            expected_spot_allocation_strategy: "lowest-price",
            expected_spot_instance_pools: "2",
            expected_spot_max_price: "",
        },
        DistributionCase {
            case: "no InstancesDistribution at all, no override either",
            overrides: Override::None,
            stated: &[],
            expected_on_demand_allocation_strategy: "prioritized",
            expected_on_demand_base_capacity: "0",
            expected_on_demand_percentage: "100",
            expected_spot_allocation_strategy: "lowest-price",
            expected_spot_instance_pools: "2",
            expected_spot_max_price: "",
        },
        DistributionCase {
            case: "no InstancesDistribution, an override stating InstanceRequirements",
            overrides: Override::Requirements,
            stated: &[],
            expected_on_demand_allocation_strategy: "lowest-price",
            expected_on_demand_base_capacity: "0",
            expected_on_demand_percentage: "100",
            expected_spot_allocation_strategy: "lowest-price",
            expected_spot_instance_pools: "2",
            expected_spot_max_price: "",
        },
        DistributionCase {
            case: "one member stated: the rest are still filled in",
            overrides: Override::InstanceType,
            stated: &[("OnDemandBaseCapacity", "1")],
            expected_on_demand_allocation_strategy: "prioritized",
            expected_on_demand_base_capacity: "1",
            expected_on_demand_percentage: "100",
            expected_spot_allocation_strategy: "lowest-price",
            expected_spot_instance_pools: "2",
            expected_spot_max_price: "",
        },
        DistributionCase {
            case: "the On-Demand strategy stated against the default of its own group kind",
            overrides: Override::Requirements,
            stated: &[("OnDemandAllocationStrategy", "prioritized")],
            expected_on_demand_allocation_strategy: "prioritized",
            expected_on_demand_base_capacity: "0",
            expected_on_demand_percentage: "100",
            expected_spot_allocation_strategy: "lowest-price",
            expected_spot_instance_pools: "2",
            expected_spot_max_price: "",
        },
        DistributionCase {
            case: "a Spot strategy the pool count is not valid under",
            overrides: Override::InstanceType,
            stated: &[("SpotAllocationStrategy", "capacity-optimized")],
            expected_on_demand_allocation_strategy: "prioritized",
            expected_on_demand_base_capacity: "0",
            expected_on_demand_percentage: "100",
            expected_spot_allocation_strategy: "capacity-optimized",
            expected_spot_instance_pools: "",
            expected_spot_max_price: "",
        },
        DistributionCase {
            case: "every documented member stated: each is kept exactly as written",
            overrides: Override::InstanceType,
            stated: &[
                ("OnDemandAllocationStrategy", "lowest-price"),
                ("OnDemandBaseCapacity", "3"),
                ("OnDemandPercentageAboveBaseCapacity", "40"),
                ("SpotAllocationStrategy", "price-capacity-optimized"),
                ("SpotMaxPrice", "0.10"),
            ],
            expected_on_demand_allocation_strategy: "lowest-price",
            expected_on_demand_base_capacity: "3",
            expected_on_demand_percentage: "40",
            expected_spot_allocation_strategy: "price-capacity-optimized",
            expected_spot_instance_pools: "",
            expected_spot_max_price: "0.10",
        },
        DistributionCase {
            case: "a pool count stated beside the strategy it is valid under",
            overrides: Override::InstanceType,
            stated: &[
                ("SpotAllocationStrategy", "lowest-price"),
                ("SpotInstancePools", "5"),
            ],
            expected_on_demand_allocation_strategy: "prioritized",
            expected_on_demand_base_capacity: "0",
            expected_on_demand_percentage: "100",
            expected_spot_allocation_strategy: "lowest-price",
            expected_spot_instance_pools: "5",
            expected_spot_max_price: "",
        },
        DistributionCase {
            case: "a zero percentage above the base: the value stated, not the default",
            overrides: Override::InstanceType,
            stated: &[("OnDemandPercentageAboveBaseCapacity", "0")],
            expected_on_demand_allocation_strategy: "prioritized",
            expected_on_demand_base_capacity: "0",
            expected_on_demand_percentage: "0",
            expected_spot_allocation_strategy: "lowest-price",
            expected_spot_instance_pools: "2",
            expected_spot_max_price: "",
        },
    ]
}

/// The parameters one row's policy is written with.
fn policy_params(case: &DistributionCase) -> Vec<(String, String)> {
    let mut params = vec![p(POLICY_TEMPLATE.0, POLICY_TEMPLATE.1)];
    params.extend(case.overrides.params());
    for (member, value) in case.stated {
        params.push(p(
            &format!("MixedInstancesPolicy.InstancesDistribution.{member}"),
            *value,
        ));
    }
    params
}

/// Assert one row's expectations against what the group reports.
fn assert_reported(case: &DistributionCase, reported: &ReportedDistribution) {
    assert_eq!(
        reported.on_demand_allocation_strategy, case.expected_on_demand_allocation_strategy,
        "case {:?}: expected OnDemandAllocationStrategy {:?}, got {:?}",
        case.case,
        case.expected_on_demand_allocation_strategy,
        reported.on_demand_allocation_strategy
    );
    assert_eq!(
        reported.on_demand_base_capacity, case.expected_on_demand_base_capacity,
        "case {:?}: expected OnDemandBaseCapacity {:?}, got {:?}",
        case.case, case.expected_on_demand_base_capacity, reported.on_demand_base_capacity
    );
    assert_eq!(
        reported.on_demand_percentage_above_base_capacity, case.expected_on_demand_percentage,
        "case {:?}: expected OnDemandPercentageAboveBaseCapacity {:?}, got {:?}",
        case.case,
        case.expected_on_demand_percentage,
        reported.on_demand_percentage_above_base_capacity
    );
    assert_eq!(
        reported.spot_allocation_strategy, case.expected_spot_allocation_strategy,
        "case {:?}: expected SpotAllocationStrategy {:?}, got {:?}",
        case.case, case.expected_spot_allocation_strategy, reported.spot_allocation_strategy
    );
    assert_eq!(
        reported.spot_instance_pools, case.expected_spot_instance_pools,
        "case {:?}: expected SpotInstancePools {:?}, got {:?}",
        case.case, case.expected_spot_instance_pools, reported.spot_instance_pools
    );
    assert_eq!(
        reported.spot_max_price, case.expected_spot_max_price,
        "case {:?}: expected SpotMaxPrice {:?}, got {:?}",
        case.case, case.expected_spot_max_price, reported.spot_max_price
    );
}

/// A group CREATED with a mixed instances policy reports the distribution it runs under:
/// every member the request stated as written, and every one it left out at the default the
/// model documents for it.
#[tokio::test]
async fn a_created_group_reports_the_distribution_it_runs_under() {
    let engine = new_engine();
    let router = router_with(engine.clone());
    let subnets = seed_subnets(&engine, 1);

    for (index, case) in distribution_cases().iter().enumerate() {
        let name = format!("mip-create-{index}");
        let mut params = vec![
            p("AutoScalingGroupName", name.clone()),
            p("MinSize", "0"),
            p("MaxSize", "2"),
            p("DesiredCapacity", "0"),
            p("VPCZoneIdentifier", subnets[0].id.clone()),
        ];
        params.extend(policy_params(case));
        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {:?}: CreateAutoScalingGroup expected 200, got {status}: {body}",
            case.case
        );

        let reported = reported_distribution(&router, &name).await;
        assert_reported(case, &reported);
    }
}

/// The same rule holds of the other writer of a group's launch source: a group UPDATED onto
/// a mixed instances policy reports the distribution that policy runs under.
#[tokio::test]
async fn an_updated_group_reports_the_distribution_it_runs_under() {
    let engine = new_engine();
    let router = router_with(engine.clone());
    let subnets = seed_subnets(&engine, 1);

    for (index, case) in distribution_cases().iter().enumerate() {
        let name = format!("mip-update-{index}");
        // Created from its own launch template, so the update is what gives it the policy.
        let (status, body) = query(
            &router,
            "CreateAutoScalingGroup",
            &[
                ("AutoScalingGroupName", name.as_str()),
                ("LaunchTemplate.LaunchTemplateName", "lt-1"),
                ("MinSize", "0"),
                ("MaxSize", "2"),
                ("DesiredCapacity", "0"),
                ("VPCZoneIdentifier", subnets[0].id.as_str()),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: the base create expected 200, got {status}: {body}",
            case.case
        );

        let mut params = vec![p("AutoScalingGroupName", name.clone())];
        params.extend(policy_params(case));
        let (status, body) = query(&router, "UpdateAutoScalingGroup", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {:?}: UpdateAutoScalingGroup expected 200, got {status}: {body}",
            case.case
        );

        let reported = reported_distribution(&router, &name).await;
        assert_reported(case, &reported);
    }
}

/// A read reports the distribution, it does not move it: the group reports the same members
/// however many times it is read.
#[tokio::test]
async fn reading_the_distribution_twice_reports_it_the_same_way() {
    let engine = new_engine();
    let router = router_with(engine.clone());
    let subnets = seed_subnets(&engine, 1);

    let (status, body) = query(
        &router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", "mip-stable"),
            POLICY_TEMPLATE,
            ("MinSize", "0"),
            ("MaxSize", "2"),
            ("DesiredCapacity", "0"),
            ("VPCZoneIdentifier", subnets[0].id.as_str()),
        ],
    )
    .await;
    assert_eq!(status, 200, "the create expected 200, got {status}: {body}");

    let first = reported_distribution(&router, "mip-stable").await;
    let second = reported_distribution(&router, "mip-stable").await;
    assert_eq!(
        (
            first.on_demand_allocation_strategy.clone(),
            first.on_demand_base_capacity.clone(),
            first.on_demand_percentage_above_base_capacity.clone(),
            first.spot_allocation_strategy.clone(),
            first.spot_instance_pools.clone(),
        ),
        (
            second.on_demand_allocation_strategy.clone(),
            second.on_demand_base_capacity.clone(),
            second.on_demand_percentage_above_base_capacity.clone(),
            second.spot_allocation_strategy.clone(),
            second.spot_instance_pools.clone(),
        ),
        "a second read reports the same distribution, got {:?} after {:?}",
        (
            &second.on_demand_allocation_strategy,
            &second.spot_allocation_strategy
        ),
        (
            &first.on_demand_allocation_strategy,
            &first.spot_allocation_strategy
        ),
    );
}

/// A group launching from its own launch template has no mixed instances policy, so it
/// reports no distribution at all: a default is materialised for the policy a group HAS,
/// never invented for one it does not.
#[tokio::test]
async fn a_group_without_a_policy_reports_no_distribution() {
    let engine = new_engine();
    let router = router_with(engine.clone());
    let subnets = seed_subnets(&engine, 1);

    let (status, body) = query(
        &router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", "no-policy"),
            ("LaunchTemplate.LaunchTemplateName", "lt-1"),
            ("MinSize", "0"),
            ("MaxSize", "2"),
            ("DesiredCapacity", "0"),
            ("VPCZoneIdentifier", subnets[0].id.as_str()),
        ],
    )
    .await;
    assert_eq!(status, 200, "the create expected 200, got {status}: {body}");

    let (status, body) = query(
        &router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", "no-policy")],
    )
    .await;
    assert_eq!(status, 200, "the read expected 200, got {status}: {body}");
    assert!(
        !body.contains("<MixedInstancesPolicy"),
        "a group launching from its own launch template reports no mixed instances policy, \
         got {body}"
    );
    assert!(
        !body.contains("<InstancesDistribution"),
        "and no instances distribution either, got {body}"
    );
}
