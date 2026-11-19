// SPDX-License-Identifier: CC0-1.0

//! The VERSION SELECTOR in a group's launch-template reference, and the two halves of the
//! template beside it.
//!
//! RULE: a read of a group reports its launch-template reference COMPLETE — both halves of
//! the template, the one the request stated and the one this service resolved for it
//! ("Conditional: You must specify either a `LaunchTemplateId` or a `LaunchTemplateName`",
//! `LaunchTemplateSpecification`) — and the `Version` member exactly as the request stated it.
//! A read resolves the IDENTITY of the template, never the selector: "the version number,
//! `$Latest`, or `$Default` … If the value is `$Latest`, Amazon EC2 Auto Scaling selects the
//! latest version of the launch template when launching instances"
//! (`LaunchTemplateSpecification$Version`) — the selector is what the group FLOATS on, read
//! afresh by each launch, so a read of the group hands it back the word it was given and the
//! number it resolves to is recorded on the instances a launch produced
//! (`tests/all/launched_instance_launch_template_tags.rs`).
//!
//! The rows differ along what the rule generalises over: the FORM of the selector (a pinned
//! number, `$Latest`, `$Default` over a template whose default version was moved, and no
//! selector at all), which half the request states (the id Amazon EC2 minted, or the name it
//! holds the template under), and where the specification sits (the group's own
//! `LaunchTemplate`, and the one its `MixedInstancesPolicy` carries). Every row's template
//! holds TWO versions, so a selector that follows the template and one pinned to it name
//! different versions: a read that resolved the selector could not pass any symbolic row.
//!
//! Which halves a read reports is the subject of `tests/all/launch_template_specifications.rs`,
//! whose rows span where the specification sits and which half is stated; this suite is the
//! selector dimension, which no row there states.

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, create_launch_template, ec2_query, emulator, p, query, tag_value, tag_values,
};

/// Where the group's create carries the specification under test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Carried {
    /// The group's own `LaunchTemplate`.
    Own,
    /// The launch template of its `MixedInstancesPolicy`.
    MixedPolicy,
}

impl Carried {
    /// The parameter prefix the specification's members are sent under.
    fn prefix(self) -> &'static str {
        match self {
            Self::Own => "LaunchTemplate",
            Self::MixedPolicy => "MixedInstancesPolicy.LaunchTemplate.LaunchTemplateSpecification",
        }
    }

    /// The element a read reports that specification as.
    fn element(self) -> &'static str {
        match self {
            Self::Own => "LaunchTemplate",
            Self::MixedPolicy => "LaunchTemplateSpecification",
        }
    }
}

/// Which half of the template the create states.
#[derive(Debug, Clone, Copy)]
enum Named {
    /// `LaunchTemplateId`, carrying the id Amazon EC2 minted for the template.
    Id,
    /// `LaunchTemplateName`, carrying the name Amazon EC2 holds it under.
    Name,
}

/// One case: how the group names its template and its version.
struct Row {
    case: &'static str,
    group: &'static str,
    template: &'static str,
    /// The version the template's DEFAULT is moved to before the group is created, or `None`
    /// to leave it where a create puts it (the first version).
    default_version: Option<&'static str>,
    named: Named,
    carried: Carried,
    /// The `Version` the create states, or `None` for a create that states none — which a read
    /// reports as the member being ABSENT: the service hands back the selector it was given and
    /// resolves nothing in its place.
    selector: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "pinned to the template's first version, named by id",
        group: "ltsel-pinned",
        template: "ltsel-pinned-template",
        default_version: None,
        named: Named::Id,
        carried: Carried::Own,
        selector: Some("1"),
    },
    Row {
        case: "floating on $Latest over two versions, named by id",
        group: "ltsel-latest-by-id",
        template: "ltsel-latest-by-id-template",
        default_version: None,
        named: Named::Id,
        carried: Carried::Own,
        selector: Some("$Latest"),
    },
    Row {
        case: "floating on $Latest over two versions, named by name",
        group: "ltsel-latest-by-name",
        template: "ltsel-latest-by-name-template",
        default_version: None,
        named: Named::Name,
        carried: Carried::Own,
        selector: Some("$Latest"),
    },
    Row {
        case: "on $Default after the template's default version was moved, named by name",
        group: "ltsel-default",
        template: "ltsel-default-template",
        default_version: Some("2"),
        named: Named::Name,
        carried: Carried::Own,
        selector: Some("$Default"),
    },
    Row {
        case: "no version selector at all, named by id",
        group: "ltsel-unversioned",
        template: "ltsel-unversioned-template",
        default_version: None,
        named: Named::Id,
        carried: Carried::Own,
        selector: None,
    },
    Row {
        case: "the specification a mixed instances policy carries, on $Latest",
        group: "ltsel-mixed",
        template: "ltsel-mixed-template",
        default_version: None,
        named: Named::Id,
        carried: Carried::MixedPolicy,
        selector: Some("$Latest"),
    },
];

#[tokio::test]
async fn a_group_reports_both_template_halves_and_the_selector_it_was_given() {
    for row in ROWS {
        let case = row.case;
        let (router, _engine) = emulator();
        // The template the group launches from, created over Amazon EC2's own wire, with a
        // SECOND version: `$Latest` and `$Default` then name versions the pinned rows do not,
        // so no row's expectation is satisfied by a read that resolved the selector.
        let (template_id, first_version) = create_launch_template(&router, row.template).await;
        let second_version = add_version(&router, case, &template_id).await;
        assert_ne!(
            second_version, first_version,
            "case {case:?}: the second version of a template must be another version, got \
             {second_version:?} beside {first_version:?}"
        );
        if let Some(default_version) = row.default_version {
            set_default_version(&router, case, &template_id, default_version).await;
        }

        let (member, value) = match row.named {
            Named::Id => ("LaunchTemplateId", template_id.clone()),
            Named::Name => ("LaunchTemplateName", row.template.to_string()),
        };
        let mut params = vec![
            p("AutoScalingGroupName", row.group),
            p("MinSize", "0"),
            p("MaxSize", "1"),
            p("AvailabilityZones.member.1", "us-west-2a"),
            p(&format!("{}.{member}", row.carried.prefix()), value),
        ];
        if let Some(selector) = row.selector {
            params.push(p(&format!("{}.Version", row.carried.prefix()), selector));
        }
        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateAutoScalingGroup must answer 200, got {status}: {body}"
        );

        let (status, described) = query(
            &router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", row.group)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeAutoScalingGroups must answer 200, got {status}: {described}"
        );
        // Scoped to the specification the row's create carried, so each member is read inside
        // the element it belongs to.
        let specification = tag_value(&described, row.carried.element());
        assert!(
            !specification.is_empty(),
            "case {case:?}: expected the group to report a {} element, got {described}",
            row.carried.element()
        );
        let reported = (
            tag_value(&specification, "LaunchTemplateId"),
            tag_value(&specification, "LaunchTemplateName"),
            tag_values(&specification, "Version").first().cloned(),
        );
        let expected = (
            template_id.clone(),
            row.template.to_string(),
            row.selector.map(str::to_string),
        );
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the group to report (LaunchTemplateId, \
             LaunchTemplateName, Version) as {expected:?}, got {reported:?} in {described}"
        );
    }
}

/// Add a second version to the template over Amazon EC2's own wire, and answer the number that
/// service assigned it — never one this suite writes out.
///
/// It names another image, so which version a reader resolved is a fact of the account's own
/// Amazon EC2 state.
async fn add_version(router: &MultiServiceRouter, case: &str, launch_template_id: &str) -> String {
    let (status, body) = ec2_query(
        router,
        "CreateLaunchTemplateVersion",
        &[
            p("LaunchTemplateId", launch_template_id),
            p("LaunchTemplateData.ImageId", "ami-0123456789abcdef1"),
            p("LaunchTemplateData.InstanceType", "t3.small"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateLaunchTemplateVersion must answer 200, got {status}: {body}"
    );
    let version = tag_value(&body, "versionNumber");
    assert!(
        !version.is_empty(),
        "case {case:?}: CreateLaunchTemplateVersion must report the version's number, got {body}"
    );
    version
}

/// Move the template's DEFAULT version, through Amazon EC2's own operation for it, so a group
/// on `$Default` floats on a version that is not the first.
async fn set_default_version(
    router: &MultiServiceRouter,
    case: &str,
    launch_template_id: &str,
    version: &str,
) {
    let (status, body) = ec2_query(
        router,
        "ModifyLaunchTemplate",
        &[
            p("LaunchTemplateId", launch_template_id),
            p("SetDefaultVersion", version),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ModifyLaunchTemplate must answer 200, got {status}: {body}"
    );
}
