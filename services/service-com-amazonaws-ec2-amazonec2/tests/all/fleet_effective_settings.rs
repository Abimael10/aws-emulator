// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the SETTINGS an EC2 Fleet reads back, driven over the wire the
//! service actually speaks — `ec2Query`, through the crate's shared [`super::ec2_query`]
//! harness.
//!
//! The rules under test are:
//!
//! 1. a fleet reports every capacity and health SETTING it is managed under at the value in
//!    force — the one the create request asked for, or the service's own where it asked for
//!    none — rather than leaving out the ones the request never named, and reports none of
//!    the settings whose documentation supports them only for a fleet type this fleet is not
//!    ("supported only for fleets of type `maintain`", `FleetData$ExcessCapacityTerminationPolicy`
//!    and `$ReplaceUnhealthyInstances`; "supported only when Spot `AllocationStrategy` is set
//!    to `lowest-price`", `SpotOptionsRequest$InstancePoolsToUseCount`);
//! 2. a fleet reads back the allocation-strategy token its create recorded: the accepted
//!    spelling the request carried — the member of the fleet's own modelled catalog, or the
//!    Spot Fleet spelling of the same strategy, both of which the API takes — and, for a
//!    request that named no strategy, the default in the Spot Fleet spelling the fleet read
//!    path publishes;
//! 3. a strategy that is neither spelling of any modelled member is refused, and no fleet is
//!    stored for it.
//!
//! Each table row is a distinct input along what the rules generalise over — the fleet type,
//! which settings the request names, which strategy, which of its two spellings, and which
//! unaccepted value — so no row is the specification.

use ec2::ops::types::{ALLOCATION_STRATEGY_VALUES, ON_DEMAND_ALLOCATION_STRATEGY_VALUES};
use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, DEFAULT_FLEET_ON_DEMAND_ALLOCATION_STRATEGY,
    DEFAULT_SPOT_ALLOCATION_STRATEGY, DEFAULT_SPOT_INSTANCE_POOLS_TO_USE_COUNT,
    FLEET_EXCESS_CAPACITY_TERMINATION_POLICY_TERMINATION,
    FLEET_ON_DEMAND_ALLOCATION_STRATEGY_LOWEST_PRICE,
    FLEET_ON_DEMAND_ALLOCATION_STRATEGY_PRIORITIZED, FLEET_TYPE_INSTANT, FLEET_TYPE_MAINTAIN,
    FLEET_TYPE_REQUEST, SPOT_ALLOCATION_STRATEGY_DIVERSIFIED, SPOT_ALLOCATION_STRATEGY_LOWEST_PRICE,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code};
use super::wire::{child, child_text, document, set_items};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The HTTP status EC2 answers a refusal about the request with.
const REFUSAL_STATUS: u16 = 400;

/// The code the request-boundary validator answers for a value outside a modelled enum,
/// before the handler runs — the refusal a strategy outside the fleet's own catalog earns.
const BOUNDARY_ENUM_VIOLATION: &str = "ValidationError";

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

fn emulator() -> (MultiServiceRouter, std::sync::Arc<engine::InMemoryEngine>) {
    super::ec2_query::emulator()
}

/// An AMI of the published catalog a fresh account can already see, read out of the service's
/// own state so the test cannot invent an id.
fn image_id() -> String {
    Ec2State::default()
        .images
        .keys()
        .next()
        .cloned()
        .expect("a fresh account must publish at least one image")
}

/// Create one launch template and answer its `LaunchTemplateId`.
async fn launch_template(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, body) = call(
        router,
        REGION,
        "CreateLaunchTemplate",
        &[
            p("LaunchTemplateName", name),
            p("LaunchTemplateData.ImageId", image_id()),
            p("LaunchTemplateData.InstanceType", "t3.micro"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateLaunchTemplate must succeed, got {status}: {body}"
    );
    element(&body, "launchTemplateId")
        .unwrap_or_else(|| panic!("case {case:?}: no launchTemplateId in {body}"))
}

/// The parameters every fleet of this suite is created with: one launch template
/// configuration and one unit of target capacity.
fn base(template_id: &str, fleet_type: &str) -> Params {
    vec![
        p(
            "LaunchTemplateConfigs.1.LaunchTemplateSpecification.LaunchTemplateId",
            template_id,
        ),
        p("Type", fleet_type),
        p("TargetCapacitySpecification.TotalTargetCapacity", "1"),
    ]
}

/// One `CreateFleet` carrying `params`.
async fn create(router: &MultiServiceRouter, params: &[(String, String)]) -> (u16, String) {
    call(router, REGION, "CreateFleet", params).await
}

/// Create one fleet and answer its `FleetId`, asserting the create succeeded.
async fn created(router: &MultiServiceRouter, case: &str, params: &[(String, String)]) -> String {
    let (status, body) = create(router, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateFleet, got {status} with body {body}"
    );
    element(&body, "fleetId").unwrap_or_else(|| panic!("case {case:?}: no fleetId in {body}"))
}

/// The member `path` of the one fleet `DescribeFleets` reports for `fleet_id` — `None` when
/// the report leaves it out, which is what tells "this fleet has no such setting" from a
/// setting reported at its default.
///
/// Read with the XML parser down the path rather than by substring, so a member of the fleet
/// itself is never confused with one of the same name inside its option structures.
async fn reported(
    router: &MultiServiceRouter,
    case: &str,
    fleet_id: &str,
    path: &[&str],
) -> Option<String> {
    let (status, body) = call(router, REGION, "DescribeFleets", &[p("FleetId.1", fleet_id)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeFleets of {fleet_id}, got {status} with \
         body {body}"
    );
    let parsed = document(case, &body);
    let item = set_items(parsed.root_element(), "fleetSet")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: no fleet reported for {fleet_id} in {body}"));
    let (last, parents) = path
        .split_last()
        .unwrap_or_else(|| panic!("case {case:?}: a member path must name a member"));
    let mut node = item;
    for name in parents {
        node = match child(node, name) {
            Some(found) => found,
            None => return None,
        };
    }
    child_text(node, last)
}

/// RULE 1: every setting a fleet is managed under is reported at the value in force, and a
/// setting the fleet's own type or strategy does not support is reported by nothing.
///
/// The rows differ along the fleet type, which settings the request names, and whether the
/// Spot allocation strategy in force is one that allocates across a number of pools at all.
#[tokio::test]
async fn a_fleet_reports_the_settings_it_is_managed_under() {
    let (router, _engine) = emulator();
    let template_id = launch_template(&router, "the settings table", "fleet-settings-suite").await;

    // The count a fleet that named none allocates across, as the wire renders it.
    let default_pools = DEFAULT_SPOT_INSTANCE_POOLS_TO_USE_COUNT.to_string();

    // (case, fleet type, the settings the request names, the expected
    //  excessCapacityTerminationPolicy / replaceUnhealthyInstances /
    //  terminateInstancesWithExpiration / spotOptions.instancePoolsToUseCount).
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        &str,
        &str,
        Params,
        Option<&str>,
        Option<&str>,
        Option<&str>,
        Option<String>,
    )> = vec![
        (
            "a maintain fleet created with defaults",
            FLEET_TYPE_MAINTAIN,
            Vec::new(),
            Some(FLEET_EXCESS_CAPACITY_TERMINATION_POLICY_TERMINATION),
            Some("false"),
            Some("false"),
            Some(default_pools.clone()),
        ),
        (
            "a maintain fleet that named every setting",
            FLEET_TYPE_MAINTAIN,
            vec![
                p("ExcessCapacityTerminationPolicy", "no-termination"),
                p("ReplaceUnhealthyInstances", "true"),
                p("TerminateInstancesWithExpiration", "true"),
                p("SpotOptions.InstancePoolsToUseCount", "3"),
            ],
            Some("no-termination"),
            Some("true"),
            Some("true"),
            Some("3".to_string()),
        ),
        (
            "a request fleet, which supports neither policy member",
            FLEET_TYPE_REQUEST,
            vec![p("TerminateInstancesWithExpiration", "true")],
            None,
            None,
            Some("true"),
            Some(default_pools.clone()),
        ),
        (
            "an instant fleet, which supports neither policy member",
            FLEET_TYPE_INSTANT,
            Vec::new(),
            None,
            None,
            Some("false"),
            Some(default_pools.clone()),
        ),
        (
            "a maintain fleet whose strategy chooses its own pools",
            FLEET_TYPE_MAINTAIN,
            vec![p(
                "SpotOptions.AllocationStrategy",
                SPOT_ALLOCATION_STRATEGY_DIVERSIFIED,
            )],
            Some(FLEET_EXCESS_CAPACITY_TERMINATION_POLICY_TERMINATION),
            Some("false"),
            Some("false"),
            None,
        ),
    ];

    for (case, fleet_type, settings, excess, replace, terminate, pools) in rows {
        let mut params = base(&template_id, fleet_type);
        params.extend(settings);
        let fleet_id = created(&router, case, &params).await;

        for (path, expected) in [
            (
                vec!["excessCapacityTerminationPolicy"],
                excess.map(str::to_string),
            ),
            (vec!["replaceUnhealthyInstances"], replace.map(str::to_string)),
            (
                vec!["terminateInstancesWithExpiration"],
                terminate.map(str::to_string),
            ),
            (
                vec!["spotOptions", "instancePoolsToUseCount"],
                pools.clone(),
            ),
        ] {
            let actual = reported(&router, case, &fleet_id, &path).await;
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {} to report {expected:?}, got {actual:?}",
                path.join("/")
            );
        }
    }
}

/// One row of the strategy table: the case, the Spot and On-Demand strategies the request
/// names, and the tokens the fleet must read them back as.
type StrategyRow = (
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
    &'static str,
    &'static str,
);

/// RULE 2: a fleet reads back the allocation-strategy token its create recorded — the
/// modelled word the request carried, and the Spot Fleet spelling of the default for a
/// request that carried none.
///
/// The rows differ along which strategy is asked for and whether it is asked for at all. The
/// published default tokens are asserted to be members of the Spot Fleet catalog of the same
/// capacity type ([`ec2::ops::types::ALLOCATION_STRATEGY_VALUES`] and
/// [`ec2::ops::types::ON_DEMAND_ALLOCATION_STRATEGY_VALUES`]), so no spelling is written by
/// hand here, and the strategies covered go beyond the `lowest-price` the recorded fleet used.
#[tokio::test]
async fn a_fleet_reads_back_the_strategy_token_its_create_recorded() {
    let (router, _engine) = emulator();
    let template_id = launch_template(&router, "the strategy table", "fleet-strategy-suite").await;

    // The token a fleet that named no strategy publishes is the Spot Fleet spelling of the
    // same strategy, which is a member of that capacity type's own generated catalog.
    assert!(
        ALLOCATION_STRATEGY_VALUES.contains(&DEFAULT_SPOT_ALLOCATION_STRATEGY.reported),
        "the published Spot default {:?} must be a member of the Spot Fleet catalog \
         {ALLOCATION_STRATEGY_VALUES:?}",
        DEFAULT_SPOT_ALLOCATION_STRATEGY.reported
    );
    assert!(
        ON_DEMAND_ALLOCATION_STRATEGY_VALUES
            .contains(&DEFAULT_FLEET_ON_DEMAND_ALLOCATION_STRATEGY.reported),
        "the published On-Demand default {:?} must be a member of the Spot Fleet catalog \
         {ON_DEMAND_ALLOCATION_STRATEGY_VALUES:?}",
        DEFAULT_FLEET_ON_DEMAND_ALLOCATION_STRATEGY.reported
    );

    // (case, the Spot strategy named, the On-Demand strategy named, the Spot token reported,
    //  the On-Demand token reported).
    let rows: Vec<StrategyRow> = vec![
        (
            "a fleet that named no strategy at all",
            None,
            None,
            DEFAULT_SPOT_ALLOCATION_STRATEGY.reported,
            DEFAULT_FLEET_ON_DEMAND_ALLOCATION_STRATEGY.reported,
        ),
        (
            "a fleet that named the default strategy itself",
            Some(SPOT_ALLOCATION_STRATEGY_LOWEST_PRICE),
            Some(FLEET_ON_DEMAND_ALLOCATION_STRATEGY_LOWEST_PRICE),
            SPOT_ALLOCATION_STRATEGY_LOWEST_PRICE,
            FLEET_ON_DEMAND_ALLOCATION_STRATEGY_LOWEST_PRICE,
        ),
        (
            "a fleet that named another strategy of each catalog",
            Some(SPOT_ALLOCATION_STRATEGY_DIVERSIFIED),
            Some(FLEET_ON_DEMAND_ALLOCATION_STRATEGY_PRIORITIZED),
            SPOT_ALLOCATION_STRATEGY_DIVERSIFIED,
            FLEET_ON_DEMAND_ALLOCATION_STRATEGY_PRIORITIZED,
        ),
        (
            "a fleet that named a strategy the issue never cited",
            Some("capacity-optimized"),
            Some(FLEET_ON_DEMAND_ALLOCATION_STRATEGY_LOWEST_PRICE),
            "capacity-optimized",
            FLEET_ON_DEMAND_ALLOCATION_STRATEGY_LOWEST_PRICE,
        ),
    ];

    for (case, spot, on_demand, spot_token, on_demand_token) in rows {
        let mut params = base(&template_id, FLEET_TYPE_MAINTAIN);
        if let Some(strategy) = spot {
            params.push(p("SpotOptions.AllocationStrategy", strategy));
        }
        if let Some(strategy) = on_demand {
            params.push(p("OnDemandOptions.AllocationStrategy", strategy));
        }
        let fleet_id = created(&router, case, &params).await;

        for (path, expected) in [
            (["spotOptions", "allocationStrategy"], spot_token),
            (["onDemandOptions", "allocationStrategy"], on_demand_token),
        ] {
            let actual = reported(&router, case, &fleet_id, &path).await;
            assert_eq!(
                actual.as_deref(),
                Some(expected),
                "case {case:?}: expected {} to report {expected:?}, got {actual:?}",
                path.join("/")
            );
        }
    }
}

/// RULE 3: a strategy outside the fleet's own modelled catalog is refused, and nothing is
/// stored for the request.
///
/// The rows differ along which member carries the value and how it fails to name a strategy
/// of that catalog: the Spot Fleet spelling of the right strategy, a word of no catalog, a
/// member of the OTHER capacity type's catalog, and the same strategy in the wrong case.
/// Every one is refused one step before the handler, by the boundary validator the modelled
/// enum drives, so the code asserted is the one that validator answers.
#[tokio::test]
async fn a_strategy_outside_the_modelled_catalog_is_refused() {
    let (router, engine) = emulator();
    let template_id = launch_template(&router, "the refusal table", "fleet-refusal-suite").await;

    // (case, the member the value is sent on, the value).
    let rows: Vec<(&str, &str, &str)> = vec![
        (
            "the Spot Fleet spelling of the default strategy",
            "SpotOptions.AllocationStrategy",
            DEFAULT_SPOT_ALLOCATION_STRATEGY.reported,
        ),
        (
            "a Spot strategy of no catalog",
            "SpotOptions.AllocationStrategy",
            "cheapest",
        ),
        (
            "a Spot strategy from the On-Demand catalog",
            "SpotOptions.AllocationStrategy",
            FLEET_ON_DEMAND_ALLOCATION_STRATEGY_PRIORITIZED,
        ),
        (
            "a Spot strategy in the wrong case",
            "SpotOptions.AllocationStrategy",
            "Lowest-Price",
        ),
        (
            "an On-Demand strategy from the Spot catalog",
            "OnDemandOptions.AllocationStrategy",
            SPOT_ALLOCATION_STRATEGY_DIVERSIFIED,
        ),
    ];

    for (case, member, value) in rows {
        let mut params = base(&template_id, FLEET_TYPE_MAINTAIN);
        params.push(p(member, value));
        let (status, body) = create(&router, &params).await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {case:?}: expected HTTP {REFUSAL_STATUS} from CreateFleet naming \
             {value:?}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            BOUNDARY_ENUM_VIOLATION,
            "case {case:?}: expected {BOUNDARY_ENUM_VIOLATION} for {value:?}, got {body}"
        );
        let stored = engine.state::<Ec2State>().lock().fleets.len();
        assert_eq!(
            stored, 0,
            "case {case:?}: a refused create stores nothing, so the account must hold no \
             fleet, got {stored}"
        );
    }
}
