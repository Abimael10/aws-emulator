// SPDX-License-Identifier: CC0-1.0

//! The service-linked role EVERY read that reports an Auto Scaling group carries.
//!
//! RULE: an Auto Scaling group is bound to a service-linked role when it is created — the
//! one the request named, or the account's own default for this service when it named none
//! ("by default, Amazon EC2 Auto Scaling uses a service-linked role named
//! `AWSServiceRoleForAutoScaling`, which it creates if it does not exist",
//! `CreateAutoScalingGroupType$ServiceLinkedRoleARN`) — so every projection of a group
//! reports that role as a present, non-empty member, whichever read returns the group and
//! however the group was configured. `tests/all/service_linked_role_binding.rs` pins WHICH
//! role a group is bound to; this suite pins that every read of a group reports it.
//!
//! The rows span what the rule generalises over on the group side: how the create named its
//! launch source (its own launch template by id, by id and version, by name, or the launch
//! template of a mixed instances policy) and whether the group is placed by Availability
//! Zone or by subnet. The reads span what it generalises over on the answer side: a describe
//! naming the group, one naming none so the whole account's groups come back together, each
//! page of a one-group-per-page pagination, a describe that leaves the group's instances
//! out, and a describe that selects the group by a tag filter. The recorded case — a group
//! created from a launch template by id and version, placed by subnet, naming no role, read
//! back by name — is one row and one read of this table.
//!
//! Auto Scaling and Amazon EC2 are mounted on ONE engine and driven over their own wire
//! protocols, so the launch templates and subnets these groups are built from are the ones a
//! client created.

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, create_launch_template, default_service_linked_role_arn, emulator, p, query,
    seed_subnets, tag_value, tag_values, Placement,
};

/// The launch template every group in this suite launches from.
const TEMPLATE_NAME: &str = "lt-slr-reads";

/// The tag one row's group carries, which the tag-filter read selects it by.
const TAG_KEY: &str = "slr-reads";
const TAG_VALUE: &str = "yes";

/// How the create named the launch source the group launches from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// The group's own `LaunchTemplate`, by id.
    OwnById,
    /// The same, with the version the template's first version is numbered.
    OwnByIdAndVersion,
    /// The same, named by the name Amazon EC2 holds the template under.
    OwnByName,
    /// The launch template of a `MixedInstancesPolicy`, by id.
    MixedPolicyById,
}

/// One row: the case, the group's name, how it names its launch source, where it places its
/// instances ([`Placement`], the one copy this crate's suites share), and whether it carries
/// the suite's tag.
struct Row {
    case: &'static str,
    group: &'static str,
    source: Source,
    placement: Placement,
    tagged: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "a launch template named by id, placed by Availability Zone",
        group: "slr-reads-by-id",
        source: Source::OwnById,
        placement: Placement::Zone,
        tagged: false,
    },
    Row {
        case: "a launch template named by id and version, placed by subnet",
        group: "slr-reads-by-id-and-version",
        source: Source::OwnByIdAndVersion,
        placement: Placement::Subnet,
        tagged: false,
    },
    Row {
        case: "a launch template named by name, placed by subnet",
        group: "slr-reads-by-name",
        source: Source::OwnByName,
        placement: Placement::Subnet,
        tagged: true,
    },
    Row {
        case: "the launch template of a mixed instances policy, placed by Availability Zone",
        group: "slr-reads-mixed-policy",
        source: Source::MixedPolicyById,
        placement: Placement::Zone,
        tagged: false,
    },
];

/// One emulator with both services on one engine is [`emulator`], the one copy this crate's
/// suites share: the group is Auto Scaling's and the launch template and subnet Amazon EC2's,
/// and one engine is what makes the two views one account.
///
/// Create the row's group, naming NO `ServiceLinkedRoleARN`, and holding no instances.
async fn create_group(
    router: &MultiServiceRouter,
    row: &Row,
    template_id: &str,
    version: &str,
    zone: &str,
    subnet: &str,
) {
    let mut params = vec![
        p("AutoScalingGroupName", row.group),
        p("MinSize", "0"),
        p("MaxSize", "1"),
        p("DesiredCapacity", "0"),
    ];
    match row.source {
        Source::OwnById => params.push(p("LaunchTemplate.LaunchTemplateId", template_id)),
        Source::OwnByIdAndVersion => {
            params.push(p("LaunchTemplate.LaunchTemplateId", template_id));
            params.push(p("LaunchTemplate.Version", version));
        }
        Source::OwnByName => params.push(p("LaunchTemplate.LaunchTemplateName", TEMPLATE_NAME)),
        Source::MixedPolicyById => params.push(p(
            "MixedInstancesPolicy.LaunchTemplate.LaunchTemplateSpecification.LaunchTemplateId",
            template_id,
        )),
    }
    match row.placement {
        Placement::Zone => params.push(p("AvailabilityZones.member.1", zone)),
        Placement::Subnet => params.push(p("VPCZoneIdentifier", subnet)),
    }
    if row.tagged {
        params.push(p("Tags.member.1.Key", TAG_KEY));
        params.push(p("Tags.member.1.Value", TAG_VALUE));
    }
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {:?}: CreateAutoScalingGroup expected 200, got {status}: {body}",
        row.case
    );
}

/// Post one `DescribeAutoScalingGroups` and answer its body, failing the test when the read
/// itself is refused.
async fn describe(router: &MultiServiceRouter, case: &str, params: &[(String, String)]) -> String {
    let (status, body) = query(router, "DescribeAutoScalingGroups", &borrowed(params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAutoScalingGroups expected 200, got {status}: {body}"
    );
    body
}

/// The groups one answer reports, as `(name, ServiceLinkedRoleARN)` pairs in the order the
/// answer lists them.
///
/// The two lists are read off the same body and zipped, so a group whose role member is
/// ABSENT shifts the pairing and is caught by the length check rather than passing as a
/// group whose role happens to read empty.
fn reported(body: &str) -> Vec<(String, String)> {
    let names = tag_values(body, "AutoScalingGroupName");
    let roles = tag_values(body, "ServiceLinkedRoleARN");
    assert_eq!(
        names.len(),
        roles.len(),
        "every group an answer reports must carry a ServiceLinkedRoleARN: {} group(s) and {} \
         role(s) in {body}",
        names.len(),
        roles.len()
    );
    names.into_iter().zip(roles).collect()
}

/// Build one group's emulator, its launch template and its subnet, and create every row's
/// group on it.
async fn account_with_every_row() -> MultiServiceRouter {
    let (router, engine) = emulator();
    let subnet = seed_subnets(&engine, 1).remove(0);
    let (template_id, version) = create_launch_template(&router, TEMPLATE_NAME).await;
    for row in ROWS {
        create_group(&router, row, &template_id, &version, &subnet.zone, &subnet.id).await;
    }
    router
}

#[tokio::test]
async fn a_describe_naming_the_group_reports_its_service_linked_role() {
    let router = account_with_every_row().await;
    let expected = default_service_linked_role_arn();
    for row in ROWS {
        let case = row.case;
        let body = describe(&router, case, &[p("AutoScalingGroupNames.member.1", row.group)]).await;
        assert_eq!(
            reported(&body),
            vec![(row.group.to_string(), expected.clone())],
            "case {case:?}: expected the group to report {expected:?} as the role it is bound \
             to, got {body}"
        );
    }
}

#[tokio::test]
async fn a_describe_naming_no_group_reports_the_role_of_every_group() {
    let router = account_with_every_row().await;
    let expected = default_service_linked_role_arn();
    let case = "every group of the account in one answer";

    let body = describe(&router, case, &[]).await;
    let every_group = reported(&body);
    assert_eq!(
        every_group.len(),
        ROWS.len(),
        "case {case:?}: expected {} groups, got {every_group:?}",
        ROWS.len()
    );
    for (name, role) in &every_group {
        assert_eq!(
            role, &expected,
            "case {case:?}: expected group {name:?} to report {expected:?}, got {role:?} in \
             {body}"
        );
    }

    // The same read with the groups' instances left out is still a read of the groups, so it
    // reports the same role for each of them.
    let body = describe(&router, case, &[p("IncludeInstances", "false")]).await;
    assert_eq!(
        reported(&body),
        every_group,
        "case {case:?}: leaving the instances out must not change the role each group \
         reports, got {body}"
    );
}

#[tokio::test]
async fn every_page_of_a_paginated_describe_reports_the_role() {
    let router = account_with_every_row().await;
    let expected = default_service_linked_role_arn();
    let case = "one group per page";

    let mut seen: Vec<(String, String)> = Vec::new();
    let mut next = String::new();
    // One page per group, plus one turn's slack, so a page that reported nothing is a
    // failure of the loop's bound rather than a hang.
    for _ in 0..=ROWS.len() {
        let mut params = vec![p("MaxRecords", "1")];
        if !next.is_empty() {
            params.push(p("NextToken", next.as_str()));
        }
        let body = describe(&router, case, &params).await;
        let page = reported(&body);
        assert_eq!(
            page.len(),
            1,
            "case {case:?}: a page of one must report one group, got {page:?} in {body}"
        );
        for (name, role) in &page {
            assert_eq!(
                role, &expected,
                "case {case:?}: expected group {name:?} to report {expected:?}, got {role:?} in \
                 {body}"
            );
        }
        seen.extend(page);
        next = tag_value(&body, "NextToken");
        if next.is_empty() {
            break;
        }
    }
    assert!(
        next.is_empty(),
        "case {case:?}: the pagination must end within {} pages, got {seen:?}",
        ROWS.len()
    );
    let mut names: Vec<String> = seen.iter().map(|(name, _)| name.clone()).collect();
    names.sort();
    let mut all: Vec<String> = ROWS.iter().map(|row| row.group.to_string()).collect();
    all.sort();
    assert_eq!(
        names, all,
        "case {case:?}: the pages together must report every group of the account, got {seen:?}"
    );
}

#[tokio::test]
async fn a_describe_selecting_the_group_by_a_tag_reports_the_role() {
    let router = account_with_every_row().await;
    let expected = default_service_linked_role_arn();
    let tagged: Vec<&Row> = ROWS.iter().filter(|row| row.tagged).collect();
    assert!(
        !tagged.is_empty(),
        "this suite must tag at least one group for the tag-filter read to select"
    );

    // The two documented tag filter types that select on a key: the key alone, and the
    // key/value pair (`Filter$Name` — `tag-key` and `tag:<key>`).
    for (case, filter_name, filter_value) in [
        ("selected by tag-key", "tag-key".to_string(), TAG_KEY),
        ("selected by tag:<key>", format!("tag:{TAG_KEY}"), TAG_VALUE),
    ] {
        let body = describe(
            &router,
            case,
            &[
                p("Filters.member.1.Name", filter_name.as_str()),
                p("Filters.member.1.Values.member.1", filter_value),
            ],
        )
        .await;
        let selected = reported(&body);
        assert_eq!(
            selected.len(),
            tagged.len(),
            "case {case:?}: expected the {} tagged group(s), got {selected:?} in {body}",
            tagged.len()
        );
        for (name, role) in &selected {
            assert_eq!(
                role, &expected,
                "case {case:?}: expected group {name:?} to report {expected:?}, got {role:?} in \
                 {body}"
            );
        }
    }
}
