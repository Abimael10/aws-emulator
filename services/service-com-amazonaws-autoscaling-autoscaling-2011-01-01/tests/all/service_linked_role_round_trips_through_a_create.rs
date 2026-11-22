// SPDX-License-Identifier: CC0-1.0

//! The service-linked role a group reports, whatever else the group carries, and read back
//! into a create.
//!
//! RULE: a group created without a `ServiceLinkedRoleARN` is bound to the account's own
//! default service-linked role for this service — the role name this service publishes,
//! filed under the path IAM reserves for a service-linked role of its principal, in the
//! caller's own account ("by default, Amazon EC2 Auto Scaling uses a service-linked role
//! named `AWSServiceRoleForAutoScaling`, which it creates if it does not exist",
//! `CreateAutoScalingGroupType$ServiceLinkedRoleARN`) — so a read of the group reports that
//! role as a present member whatever ELSE the group was configured with, and the role a read
//! reports is a value a create accepts back, binding the group it creates to the same role.
//!
//! `tests/all/service_linked_role_binding.rs` pins WHICH role a group is bound to, and
//! `tests/all/service_linked_role_in_group_reads.rs` that every READ of a group reports it.
//! This suite pins the two things neither covers: that the rest of the group's configuration
//! does not change the role reported, and that a described group round-trips — re-submitting
//! the role a read reported reproduces the same binding, which is what a client that reads a
//! group to re-create or to grant its principal something depends on.
//!
//! The rows span what the rule generalises over on the group side: the health check the group
//! runs and the grace period it allows, how the launch template version is selected (left
//! out, a floating selector, or the numbered version Amazon EC2 assigned), the group's
//! capacity bounds, whether it holds instances at all, and whether it carries a tag that
//! propagates at launch. The recorded case — a group checking ELB health with a 200-second
//! grace period, launching from a template named by id at `$Latest`, placed by subnet,
//! holding no instances and carrying one propagating tag — is one row of it.
//!
//! Auto Scaling and Amazon EC2 are mounted on ONE engine and driven over their own wire
//! protocols, so the launch template and subnets these groups are built from are the ones a
//! client created.

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, create_launch_template, default_service_linked_role_arn, emulator, p, query,
    seed_subnets, tag_value, tag_values, Placement, DEFAULT_SERVICE_LINKED_ROLE_NAME,
    DEFAULT_SERVICE_LINKED_ROLE_PATH,
};

/// The launch template every group in this suite launches from.
const TEMPLATE_NAME: &str = "lt-slr-round-trip";

/// How the create selects the launch template version.
///
/// SOURCE: `LaunchTemplateSpecification$Version` — "the version number, `$Latest`, or
/// `$Default`"; left out, the specification carries no version at all.
#[derive(Debug, Clone, Copy)]
enum Version {
    /// No `Version` member.
    Unstated,
    /// The number Amazon EC2 assigned the template's first version.
    Numbered,
    /// The floating selector `$Latest`.
    Latest,
    /// The floating selector `$Default`.
    Default,
}

/// One row: the case, the group's name, how it selects the template version, the health
/// check it runs and the grace period it states, its `(MinSize, MaxSize, DesiredCapacity)`,
/// where it places instances and whether it carries a propagating tag.
struct Row {
    case: &'static str,
    group: &'static str,
    version: Version,
    health_check_type: Option<&'static str>,
    health_check_grace_period: Option<&'static str>,
    bounds: (&'static str, &'static str, &'static str),
    placement: Placement,
    propagating_tag: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "ELB health with a grace period, on $Latest, placed by subnet, one propagating tag",
        group: "slr-round-trip-elb-latest",
        version: Version::Latest,
        health_check_type: Some("ELB"),
        health_check_grace_period: Some("200"),
        bounds: ("0", "2", "0"),
        placement: Placement::Subnet,
        propagating_tag: true,
    },
    Row {
        case: "no health check or version stated, placed by Availability Zone",
        group: "slr-round-trip-defaults",
        version: Version::Unstated,
        health_check_type: None,
        health_check_grace_period: None,
        bounds: ("0", "1", "0"),
        placement: Placement::Zone,
        propagating_tag: false,
    },
    Row {
        case: "EC2 health with a zero grace period, on the numbered version, placed by subnet",
        group: "slr-round-trip-ec2-numbered",
        version: Version::Numbered,
        health_check_type: Some("EC2"),
        health_check_grace_period: Some("0"),
        bounds: ("0", "4", "0"),
        placement: Placement::Subnet,
        propagating_tag: false,
    },
    Row {
        case: "ELB health on $Default, holding instances, placed by Availability Zone",
        group: "slr-round-trip-holding-instances",
        version: Version::Default,
        health_check_type: Some("ELB"),
        health_check_grace_period: Some("300"),
        bounds: ("1", "3", "2"),
        placement: Placement::Zone,
        propagating_tag: true,
    },
];

/// The trailing text of the default role's ARN, built from the path IAM reserves for a
/// service-linked role of this service's principal and the role name this service publishes
/// (the one copy of both, `crate::support`): a client that checks the group names a
/// service-linked role of this service checks exactly this.
fn default_role_arn_suffix() -> String {
    format!(":role{DEFAULT_SERVICE_LINKED_ROLE_PATH}{DEFAULT_SERVICE_LINKED_ROLE_NAME}")
}

/// One account: the emulator every request in a test goes to, and the Amazon EC2 resources
/// its groups are built from — the launch template's id and the number its first version
/// carries, and the one subnet and the zone Amazon EC2 holds it in.
struct Account {
    router: MultiServiceRouter,
    template_id: String,
    numbered_version: String,
    zone: String,
    subnet: String,
}

impl Account {
    /// One emulator with Auto Scaling and Amazon EC2 on ONE engine ([`emulator`], the one
    /// copy this crate's suites share), holding the suite's launch template and one subnet.
    async fn new() -> Self {
        let (router, engine) = emulator();
        let subnet = seed_subnets(&engine, 1).remove(0);
        let (template_id, numbered_version) = create_launch_template(&router, TEMPLATE_NAME).await;
        Self {
            router,
            template_id,
            numbered_version,
            zone: subnet.zone,
            subnet: subnet.id,
        }
    }

    /// Create the row's group under `name`, naming `role` only when the case names one, and
    /// answer the create's status and body.
    async fn create_group(&self, row: &Row, name: &str, role: Option<&str>) -> (u16, String) {
        let (min, max, desired) = row.bounds;
        let mut params = vec![
            p("AutoScalingGroupName", name),
            p("LaunchTemplate.LaunchTemplateId", self.template_id.as_str()),
            p("MinSize", min),
            p("MaxSize", max),
            p("DesiredCapacity", desired),
        ];
        match row.version {
            Version::Unstated => {}
            Version::Numbered => {
                params.push(p("LaunchTemplate.Version", self.numbered_version.as_str()))
            }
            Version::Latest => params.push(p("LaunchTemplate.Version", "$Latest")),
            Version::Default => params.push(p("LaunchTemplate.Version", "$Default")),
        }
        if let Some(health_check_type) = row.health_check_type {
            params.push(p("HealthCheckType", health_check_type));
        }
        if let Some(grace) = row.health_check_grace_period {
            params.push(p("HealthCheckGracePeriod", grace));
        }
        match row.placement {
            Placement::Zone => params.push(p("AvailabilityZones.member.1", self.zone.as_str())),
            Placement::Subnet => params.push(p("VPCZoneIdentifier", self.subnet.as_str())),
        }
        if row.propagating_tag {
            params.push(p("Tags.member.1.Key", "slr-round-trip"));
            params.push(p("Tags.member.1.Value", row.group));
            params.push(p("Tags.member.1.PropagateAtLaunch", "true"));
        }
        if let Some(role) = role {
            params.push(p("ServiceLinkedRoleARN", role));
        }
        query(&self.router, "CreateAutoScalingGroup", &borrowed(&params)).await
    }
}

/// The one group `name`, read back over the wire, as the body the describe answered.
async fn describe(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAutoScalingGroups for {name:?} expected 200, got {status}: {body}"
    );
    assert_eq!(
        tag_values(&body, "AutoScalingGroupName"),
        vec![name.to_string()],
        "case {case:?}: expected the one group {name:?}, got {body}"
    );
    body
}

/// The `ServiceLinkedRoleARN` one answered group carries, failing when the member is absent
/// or empty — the omission this suite guards against reads as an empty value here.
fn reported_role(case: &str, body: &str) -> String {
    let roles = tag_values(body, "ServiceLinkedRoleARN");
    assert_eq!(
        roles.len(),
        1,
        "case {case:?}: expected the group to carry one ServiceLinkedRoleARN, got {roles:?} in \
         {body}"
    );
    let role = roles[0].clone();
    assert!(
        !role.is_empty(),
        "case {case:?}: expected a non-empty ServiceLinkedRoleARN, got {body}"
    );
    role
}

#[tokio::test]
async fn the_role_a_group_reports_does_not_depend_on_the_rest_of_its_configuration() {
    let account = Account::new().await;
    let expected = default_service_linked_role_arn();
    let suffix = default_role_arn_suffix();

    for row in ROWS {
        let case = row.case;
        let (status, body) = account.create_group(row, row.group, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateAutoScalingGroup expected 200, got {status}: {body}"
        );

        let body = describe(&account.router, case, row.group).await;
        let role = reported_role(case, &body);
        assert_eq!(
            role, expected,
            "case {case:?}: expected the group to report {expected:?} as the role it is bound \
             to, got {role:?}"
        );
        assert!(
            role.ends_with(&suffix),
            "case {case:?}: expected a service-linked role of this service, ending in \
             {suffix:?}, got {role:?}"
        );

        // The role is reported BESIDE the configuration the row stated, so the member is part
        // of the group a client reads rather than something a particular configuration adds.
        let reported_health_check = tag_value(&body, "HealthCheckType");
        let stated_health_check = row.health_check_type.unwrap_or("EC2");
        assert_eq!(
            reported_health_check, stated_health_check,
            "case {case:?}: expected HealthCheckType {stated_health_check:?}, got \
             {reported_health_check:?} in {body}"
        );
        if let Some(grace) = row.health_check_grace_period {
            let reported_grace = tag_value(&body, "HealthCheckGracePeriod");
            assert_eq!(
                reported_grace, grace,
                "case {case:?}: expected HealthCheckGracePeriod {grace:?}, got \
                 {reported_grace:?} in {body}"
            );
        }
    }
}

#[tokio::test]
async fn the_role_a_read_reports_binds_the_group_a_create_resubmits_it_for() {
    let account = Account::new().await;

    for row in ROWS {
        let case = row.case;
        let (status, body) = account.create_group(row, row.group, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateAutoScalingGroup expected 200, got {status}: {body}"
        );
        let first = reported_role(case, &describe(&account.router, case, row.group).await);

        // Re-submit the group the read reported, naming the role it reported: the second
        // group is bound to the same role, so a client can re-create a described group
        // without losing or having to invent its role binding.
        let resubmitted = format!("{}-resubmitted", row.group);
        let (status, body) = account
            .create_group(row, &resubmitted, Some(&first))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: re-submitting the described group expected 200, got {status}: {body}"
        );
        let second = reported_role(case, &describe(&account.router, case, &resubmitted).await);
        assert_eq!(
            second, first,
            "case {case:?}: expected the re-created group to report the role the read reported \
             ({first:?}), got {second:?}"
        );
    }
}
