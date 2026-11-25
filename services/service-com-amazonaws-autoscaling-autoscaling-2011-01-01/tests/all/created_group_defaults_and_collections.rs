// SPDX-License-Identifier: CC0-1.0

//! What a created Auto Scaling group reads back as: the documented DEFAULTS of the members its
//! create left out, its bounds, and its collections answered as LISTS.
//!
//! THE RULE UNDER TEST: `DescribeAutoScalingGroups` answers every member of the group it
//! reports out of that group's own state — a member the create stated no value for carries the
//! value the service documents as that member's default, and a LIST-typed member is always
//! NAMED, carrying exactly what the group holds and EMPTY when it holds nothing, never left
//! out and never a null (`smithy_query(query="shape",shape="AutoScalingGroup")` types
//! `LoadBalancerNames`, `TargetGroupARNs`, `SuspendedProcesses`, `EnabledMetrics`,
//! `TerminationPolicies` and `Instances` as lists). A client that iterates one of them reads
//! an empty sequence off a group that holds nothing rather than failing on a member that is
//! not there, so "attached to nothing" stays distinguishable from "not reported".
//!
//! The rows span what the rule generalises over on the create side: HOW the group names its
//! launch template (by name, by id, by id at a numbered version, by id at `$Latest`), WHAT its
//! capacity bounds are (pinned at zero, headroom to the maximum, a minimum that holds a
//! member, a desired capacity stated rather than defaulted) and WHERE it places its instances
//! (by Availability Zone, by subnet) — none of which changes a default or turns a collection
//! into a null. The second test is the other side of the same rule: a collection the group is
//! GIVEN something in reports exactly that, and reads back as an EMPTY list once it is taken
//! away again, so the empty answers above are the group holding nothing rather than a constant.
//!
//! Auto Scaling, Amazon EC2, Elastic Load Balancing and Elastic Load Balancing v2 are mounted
//! on ONE engine and driven over their own wires, so the launch template, subnets, load
//! balancers and target groups these groups are built from are the ones a client created. Every
//! wait looks AGAIN until the condition holds or a generous deadline turns it into a failure.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, create_classic_load_balancer, create_launch_template, create_target_group,
    emulator_with_classic_elb, named_list, p, query, seed_subnets, tag_value, tag_values, vpc_of,
    SeededSubnet, REGION,
};

/// The launch template every group of this suite launches from, created over Amazon EC2's own
/// wire so each group launches from a template the account really holds.
const TEMPLATE: &str = "lt-group-defaults";

/// The cooldown a group whose create states none is configured with. SOURCE:
/// `CreateAutoScalingGroupType$DefaultCooldown` — "Default: `300` seconds".
const DEFAULT_COOLDOWN: &str = "300";

/// The grace period a group whose create states none is configured with. SOURCE:
/// `CreateAutoScalingGroupType$HealthCheckGracePeriod` — "Default: `0` seconds".
const DEFAULT_GRACE_PERIOD: &str = "0";

/// The health check a group whose create names none runs. SOURCE:
/// `CreateAutoScalingGroupType$HealthCheckType` — "`EC2` is the default health check and cannot
/// be disabled".
const DEFAULT_HEALTH_CHECK_TYPE: &str = "EC2";

/// The termination policy a group whose create configures none selects a member to terminate
/// with: the policy named `Default`, the first of the valid values
/// `CreateAutoScalingGroupType$TerminationPolicies` publishes.
const DEFAULT_TERMINATION_POLICY: &str = "Default";

/// How `AutoScalingGroup$NewInstancesProtectedFromScaleIn` reads for a group whose create asked
/// for no scale-in protection: the member is reported, and reports that the group's new
/// instances are not protected.
const UNPROTECTED: &str = "false";

/// The `TargetTypeEnum` of a target group whose targets are Amazon EC2 instances
/// (`TargetGroup$TargetType`: "`instance` - Targets are specified by instance ID").
const TARGET_TYPE_INSTANCE: &str = "instance";

/// How long a wait looks again before it is called a failure. A BACKSTOP, not a schedule.
const DEADLINE: Duration = Duration::from_secs(60);

/// How long one look waits before looking again.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// How a row's group names the launch template it launches from — the forms
/// `LaunchTemplateSpecification` admits
/// (`smithy_query(query="shape",shape="LaunchTemplateSpecification")`): the template's name, its
/// id, and either of those at a stated version.
#[derive(Debug, Clone, Copy)]
enum Reference {
    /// `LaunchTemplate.LaunchTemplateName`.
    Name,
    /// `LaunchTemplate.LaunchTemplateId`.
    Id,
    /// The id at the version number Amazon EC2 minted for the template.
    IdAtVersion,
    /// The id at `$Latest`, "the latest version of the launch template"
    /// (`LaunchTemplateSpecification$Version`).
    IdAtLatest,
}

/// Where a row's group places the instances it launches.
#[derive(Debug, Clone, Copy)]
enum Placement {
    /// `AvailabilityZones`, naming one zone of the Region's own catalog.
    Zone,
    /// `VPCZoneIdentifier`, naming the account's own subnets.
    Subnets,
}

/// One row: a create whose group states no default, and what that group must read back as.
struct Row {
    case: &'static str,
    group: &'static str,
    reference: Reference,
    min_size: i32,
    max_size: i32,
    /// The `DesiredCapacity` the create states, and `None` for a create that states none —
    /// "if you do not specify a desired capacity, the default is the minimum size of the
    /// group" (`CreateAutoScalingGroupType$DesiredCapacity`).
    desired_capacity: Option<i32>,
    placement: Placement,
}

/// The creates the rule is asserted over: each names its launch template differently, carries
/// different bounds and places its instances differently, and none of them states a default.
const ROWS: &[Row] = &[
    Row {
        case: "the template named, bounds pinned at zero, placed by zone",
        group: "defaults-by-name",
        reference: Reference::Name,
        min_size: 0,
        max_size: 0,
        desired_capacity: None,
        placement: Placement::Zone,
    },
    Row {
        case: "the template by id, headroom up to the maximum, placed by subnet",
        group: "defaults-by-id",
        reference: Reference::Id,
        min_size: 0,
        max_size: 3,
        desired_capacity: None,
        placement: Placement::Subnets,
    },
    Row {
        case: "the template at a numbered version, a minimum that holds one member",
        group: "defaults-at-version",
        reference: Reference::IdAtVersion,
        min_size: 1,
        max_size: 2,
        desired_capacity: None,
        placement: Placement::Zone,
    },
    Row {
        case: "the template at $Latest, a desired capacity stated rather than defaulted",
        group: "defaults-at-latest",
        reference: Reference::IdAtLatest,
        min_size: 0,
        max_size: 5,
        desired_capacity: Some(0),
        placement: Placement::Subnets,
    },
];

/// The account every test here reads: the four services on one engine, two subnets, and one
/// launch template created over Amazon EC2's own wire.
struct Account {
    router: MultiServiceRouter,
    subnets: Vec<SeededSubnet>,
    template_id: String,
    template_version: String,
    vpc_id: String,
}

/// Stand up that account.
async fn account() -> Account {
    let (router, engine) = emulator_with_classic_elb();
    let subnets = seed_subnets(&engine, 2);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    let (template_id, template_version) = create_launch_template(&router, TEMPLATE).await;
    Account {
        router,
        subnets,
        template_id,
        template_version,
        vpc_id,
    }
}

/// The parameters one row's create carries: its launch source, its bounds and its placement,
/// and nothing else — every member this suite reads back a default for is left out.
fn create_params(account: &Account, row: &Row) -> Vec<(String, String)> {
    let mut params = vec![
        p("AutoScalingGroupName", row.group),
        p("MinSize", row.min_size.to_string()),
        p("MaxSize", row.max_size.to_string()),
    ];
    match row.reference {
        Reference::Name => params.push(p("LaunchTemplate.LaunchTemplateName", TEMPLATE)),
        Reference::Id => params.push(p(
            "LaunchTemplate.LaunchTemplateId",
            account.template_id.clone(),
        )),
        Reference::IdAtVersion => {
            params.push(p(
                "LaunchTemplate.LaunchTemplateId",
                account.template_id.clone(),
            ));
            params.push(p("LaunchTemplate.Version", account.template_version.clone()));
        }
        Reference::IdAtLatest => {
            params.push(p(
                "LaunchTemplate.LaunchTemplateId",
                account.template_id.clone(),
            ));
            params.push(p("LaunchTemplate.Version", "$Latest"));
        }
    }
    if let Some(desired) = row.desired_capacity {
        params.push(p("DesiredCapacity", desired.to_string()));
    }
    match row.placement {
        Placement::Zone => params.push(p(
            "AvailabilityZones.member.1",
            account.subnets[0].zone.clone(),
        )),
        Placement::Subnets => {
            let ids: Vec<&str> = account
                .subnets
                .iter()
                .map(|subnet| subnet.id.as_str())
                .collect();
            params.push(p("VPCZoneIdentifier", ids.join(",")));
        }
    }
    params
}

/// Post one `CreateAutoScalingGroup` and require it to be served.
async fn create_group(router: &MultiServiceRouter, case: &str, params: &[(String, String)]) {
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAutoScalingGroup expected 200, got {status}: {body}"
    );
}

/// Post one group-scoped action and require it to be served.
async fn served(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(String, String)],
) {
    let (status, body) = query(router, action, &borrowed(params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} expected 200, got {status}: {body}"
    );
}

/// The answer of a `DescribeAutoScalingGroups` naming one group.
async fn described(router: &MultiServiceRouter, case: &str, group: &str) -> String {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAutoScalingGroups for {group:?} expected 200, got {status}: \
         {body}"
    );
    body
}

/// The scalar member `tag` of the described group, read off the answer.
fn scalar(case: &str, xml: &str, tag: &str) -> String {
    let found = tag_value(xml, tag);
    assert!(
        !found.is_empty(),
        "case {case:?}: expected the described group to report {tag}, got {xml}"
    );
    found
}

/// Assert the group `xml` describes carries every documented default, named and with the
/// documented value.
fn assert_defaults(case: &str, xml: &str) {
    for (member, expected) in [
        ("HealthCheckType", DEFAULT_HEALTH_CHECK_TYPE),
        ("HealthCheckGracePeriod", DEFAULT_GRACE_PERIOD),
        ("DefaultCooldown", DEFAULT_COOLDOWN),
        ("NewInstancesProtectedFromScaleIn", UNPROTECTED),
    ] {
        let found = scalar(case, xml, member);
        assert_eq!(
            found, expected,
            "case {case:?}: expected {member} {expected:?}, got {found:?}"
        );
    }
    let policies = named_list(case, xml, "TerminationPolicies");
    assert_eq!(
        policies,
        vec![DEFAULT_TERMINATION_POLICY.to_string()],
        "case {case:?}: expected TerminationPolicies to name only \
         {DEFAULT_TERMINATION_POLICY:?}, got {policies:?}"
    );
}

/// Assert the four collections a group with no attachment, no suspension and no metrics
/// collection holds nothing in are each NAMED and EMPTY.
fn assert_empty_collections(case: &str, xml: &str) {
    for member in [
        "LoadBalancerNames",
        "TargetGroupARNs",
        "SuspendedProcesses",
        "EnabledMetrics",
    ] {
        let entries = named_list(case, xml, member);
        assert!(
            entries.is_empty(),
            "case {case:?}: expected {member} to be an empty list, got {entries:?}"
        );
    }
}

/// Look again until the group reports `wanted` instances, so what a case asserts is the
/// membership the clock has settled on rather than a guess about how fast the host is.
async fn instances_settle(router: &MultiServiceRouter, case: &str, group: &str, wanted: usize) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = described(router, case, group).await;
        let held = named_list(case, &body, "Instances");
        if held.len() == wanted {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the group to report {wanted} instance(s), got {} of them",
            held.len()
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The scaling processes the service publishes as its own catalog, read over the wire so no
/// case here writes that membership out.
async fn published_processes(router: &MultiServiceRouter) -> Vec<String> {
    let (status, body) = query(router, "DescribeScalingProcessTypes", &[]).await;
    assert_eq!(
        status, 200,
        "the published process catalog expected 200, got {status}: {body}"
    );
    let catalog = tag_values(&body, "ProcessName");
    assert!(
        !catalog.is_empty(),
        "the published process catalog must name a process, got {catalog:?}"
    );
    catalog
}

/// The group metrics the service publishes, and the granularity it aggregates them at, read
/// over the wire for the same reason.
async fn published_metrics(router: &MultiServiceRouter) -> (Vec<String>, String) {
    let (status, body) = query(router, "DescribeMetricCollectionTypes", &[]).await;
    assert_eq!(
        status, 200,
        "the published metric catalog expected 200, got {status}: {body}"
    );
    let metrics = tag_values(&body, "Metric");
    let granularity = tag_value(&body, "Granularity");
    assert!(
        !metrics.is_empty() && !granularity.is_empty(),
        "the published metric catalog must name a metric and a granularity, got {metrics:?} at \
         {granularity:?}"
    );
    (metrics, granularity)
}

/// RULE: a group whose create states no default reads back with every documented default, with
/// the bounds it was given, and with each of its collections named — empty where it holds
/// nothing — however it named its launch template, whatever its bounds and wherever it places
/// its instances.
#[tokio::test]
async fn a_created_group_reads_back_its_defaults_and_its_collections_as_lists() {
    let account = account().await;
    for row in ROWS {
        let case = row.case;
        create_group(&account.router, case, &create_params(&account, row)).await;

        // The capacity the create asked for is what the group holds: a row with a minimum above
        // zero owes members, so the read is taken once the clock has settled on them.
        let expected_desired = row.desired_capacity.unwrap_or(row.min_size);
        let wanted = usize::try_from(expected_desired).expect("a row's capacity is non-negative");
        instances_settle(&account.router, case, row.group, wanted).await;

        let body = described(&account.router, case, row.group).await;
        assert_defaults(case, &body);
        assert_empty_collections(case, &body);

        for (member, expected) in [
            ("MinSize", row.min_size),
            ("MaxSize", row.max_size),
            ("DesiredCapacity", expected_desired),
        ] {
            let found = tag_value(&body, member);
            assert_eq!(
                found,
                expected.to_string(),
                "case {case:?}: expected {member} {expected}, got {found:?}"
            );
        }
        let instances = named_list(case, &body, "Instances");
        assert_eq!(
            instances.len(),
            wanted,
            "case {case:?}: expected Instances to carry {wanted} entries, got {}",
            instances.len()
        );
    }
}

/// RULE, the other side of it: each of those four collections carries exactly what the group is
/// given, and is a NAMED EMPTY list again once what it held is taken away — so an empty answer
/// is the group holding nothing rather than a constant.
#[tokio::test]
async fn a_collection_carries_what_the_group_holds_and_is_empty_again_when_it_is_taken_away() {
    let account = account().await;
    let router = &account.router;
    let group = "collections-given-and-taken";
    let case = "a group given one entry in each of its four collections";
    let zone = account.subnets[0].zone.clone();

    create_group(
        router,
        case,
        &[
            p("AutoScalingGroupName", group),
            p("LaunchTemplate.LaunchTemplateName", TEMPLATE),
            p("MinSize", "0"),
            p("MaxSize", "2"),
            p("AvailabilityZones.member.1", zone.clone()),
        ],
    )
    .await;

    // BEFORE: the group holds nothing in any of them.
    let body = described(router, case, group).await;
    assert_empty_collections(case, &body);

    // One entry in each, each written through the call that writes it, and each naming a
    // resource the service that owns it really holds.
    let load_balancer = format!("{group}-lb");
    create_classic_load_balancer(router, REGION, &zone, &load_balancer).await;
    served(
        router,
        case,
        "AttachLoadBalancers",
        &[
            p("AutoScalingGroupName", group),
            p("LoadBalancerNames.member.1", load_balancer.clone()),
        ],
    )
    .await;
    let target_group =
        create_target_group(router, &format!("{group}-tg"), 80, TARGET_TYPE_INSTANCE, &account.vpc_id)
            .await;
    served(
        router,
        case,
        "AttachLoadBalancerTargetGroups",
        &[
            p("AutoScalingGroupName", group),
            p("TargetGroupARNs.member.1", target_group.clone()),
        ],
    )
    .await;
    let process = published_processes(router)
        .await
        .first()
        .cloned()
        .expect("the published process catalog names a process");
    served(
        router,
        case,
        "SuspendProcesses",
        &[
            p("AutoScalingGroupName", group),
            p("ScalingProcesses.member.1", process.clone()),
        ],
    )
    .await;
    let (metrics, granularity) = published_metrics(router).await;
    let metric = metrics
        .first()
        .cloned()
        .expect("the published metric catalog names a metric");
    served(
        router,
        case,
        "EnableMetricsCollection",
        &[
            p("AutoScalingGroupName", group),
            p("Granularity", granularity),
            p("Metrics.member.1", metric.clone()),
        ],
    )
    .await;

    // GIVEN: each collection carries exactly the one entry the group was given.
    let body = described(router, case, group).await;
    let names = named_list(case, &body, "LoadBalancerNames");
    assert_eq!(
        names,
        vec![load_balancer.clone()],
        "case {case:?}: expected LoadBalancerNames to carry {load_balancer:?}, got {names:?}"
    );
    let arns = named_list(case, &body, "TargetGroupARNs");
    assert_eq!(
        arns,
        vec![target_group.clone()],
        "case {case:?}: expected TargetGroupARNs to carry {target_group:?}, got {arns:?}"
    );
    let suspended: Vec<String> = named_list(case, &body, "SuspendedProcesses")
        .iter()
        .map(|entry| tag_value(entry, "ProcessName"))
        .collect();
    assert_eq!(
        suspended,
        vec![process.clone()],
        "case {case:?}: expected SuspendedProcesses to carry {process:?}, got {suspended:?}"
    );
    let enabled: Vec<String> = named_list(case, &body, "EnabledMetrics")
        .iter()
        .map(|entry| tag_value(entry, "Metric"))
        .collect();
    assert_eq!(
        enabled,
        vec![metric.clone()],
        "case {case:?}: expected EnabledMetrics to carry {metric:?}, got {enabled:?}"
    );
    // The defaults the create never stated are untouched by any of that.
    assert_defaults(case, &body);

    // TAKEN AWAY: each collection is a named empty list again, not an absent member.
    let case = "a group whose four collections were emptied again";
    served(
        router,
        case,
        "DetachLoadBalancers",
        &[
            p("AutoScalingGroupName", group),
            p("LoadBalancerNames.member.1", load_balancer),
        ],
    )
    .await;
    served(
        router,
        case,
        "DetachLoadBalancerTargetGroups",
        &[
            p("AutoScalingGroupName", group),
            p("TargetGroupARNs.member.1", target_group),
        ],
    )
    .await;
    served(
        router,
        case,
        "ResumeProcesses",
        &[
            p("AutoScalingGroupName", group),
            p("ScalingProcesses.member.1", process),
        ],
    )
    .await;
    served(
        router,
        case,
        "DisableMetricsCollection",
        &[
            p("AutoScalingGroupName", group),
            p("Metrics.member.1", metric),
        ],
    )
    .await;

    let body = described(router, case, group).await;
    assert_empty_collections(case, &body);
    assert_defaults(case, &body);
}
