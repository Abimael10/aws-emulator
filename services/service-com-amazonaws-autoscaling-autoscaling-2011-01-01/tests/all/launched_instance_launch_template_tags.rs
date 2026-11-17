// SPDX-License-Identifier: CC0-1.0

//! The LAUNCH-TEMPLATE tags an instance an Auto Scaling group launched carries, read back
//! through Amazon EC2's own wire.
//!
//! RULE: an instance launched FROM A LAUNCH TEMPLATE carries the two system tags Amazon EC2
//! adds for a template launch — `aws:ec2launchtemplate:id`, naming the template, and
//! `aws:ec2launchtemplate:version`, naming the VERSION the launch read as the number it is,
//! never the `$Latest` or `$Default` word a group may float on — beside the
//! `aws:autoscaling:groupName` tag this service puts on every instance it launches. They are
//! the account's one record of which template version produced an instance, so a client that
//! asks what an instance of the group is running gets an answer.
//!
//! The rows differ along what the rule generalises over: how the group's launch source names
//! the template (by id, by name, through a mixed instances policy) and how it names the
//! version (a number, `$Latest` over a template with two versions, `$Default`, and none at
//! all).

use std::sync::Arc;
use std::time::Duration;

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_ec2_amazonec2 as ec2_service;
use service_com_amazonaws_ec2_amazonec2::state as ec2;

use crate::support::{borrowed, ec2_query, p, query, tag_value, tag_values, REGION};

/// The two tags Amazon EC2 adds to an instance launched from a launch template, read from that
/// service's own statement of them so a rename cannot leave this suite green against the old
/// spelling.
const LAUNCH_TEMPLATE_ID_TAG: &str = ec2::LAUNCH_TEMPLATE_ID_TAG;
const LAUNCH_TEMPLATE_VERSION_TAG: &str = ec2::LAUNCH_TEMPLATE_VERSION_TAG;

/// The tag this service puts on every instance it launches, which must still be there beside
/// the two above.
const GROUP_NAME_TAG: &str = "aws:autoscaling:groupName";

/// How the group's launch source names the template, and the version it asks for.
#[derive(Debug, Clone, Copy)]
enum Source {
    /// `LaunchTemplate` naming the template by id, with the `Version` the row states.
    ById(Option<&'static str>),
    /// `LaunchTemplate` naming it by name, with the `Version` the row states.
    ByName(Option<&'static str>),
    /// `MixedInstancesPolicy` whose launch template names it by id, with that `Version`.
    MixedPolicy(Option<&'static str>),
}

/// One row: how the group names its template, and the version number the instance must report
/// having been launched from.
struct Row {
    case: &'static str,
    group: &'static str,
    template: &'static str,
    /// How many versions the template has when the group launches, so a row can float on
    /// `$Latest` over a template that has moved.
    versions: usize,
    source: Source,
    /// The version the instance's tag must carry.
    launched_version: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "the template named by id, at the version the group pinned",
        group: "asg-tags-by-id",
        template: "lt-tags-by-id",
        versions: 1,
        source: Source::ById(Some("1")),
        launched_version: "1",
    },
    Row {
        case: "the template named by name, floating on $Latest over two versions",
        group: "asg-tags-latest",
        template: "lt-tags-latest",
        versions: 2,
        source: Source::ByName(Some("$Latest")),
        launched_version: "2",
    },
    Row {
        case: "the template named by name, floating on $Default over two versions",
        group: "asg-tags-default",
        template: "lt-tags-default",
        versions: 2,
        source: Source::ByName(Some("$Default")),
        launched_version: "1",
    },
    Row {
        case: "the template named by id, with no version named at all",
        group: "asg-tags-unversioned",
        template: "lt-tags-unversioned",
        versions: 2,
        source: Source::ById(None),
        launched_version: "1",
    },
    Row {
        case: "the template a mixed instances policy carries",
        group: "asg-tags-mixed",
        template: "lt-tags-mixed",
        versions: 1,
        source: Source::MixedPolicy(Some("1")),
        launched_version: "1",
    },
];

#[tokio::test]
async fn an_instance_a_group_launched_carries_the_launch_templates_system_tags() {
    for row in ROWS {
        let case = row.case;
        let router = router();
        let (template_id, _) = create_template(&router, case, row.template, row.versions).await;

        let mut params = vec![
            p("AutoScalingGroupName", row.group),
            p("MinSize", "1"),
            p("MaxSize", "2"),
            p("DesiredCapacity", "1"),
            p("AvailabilityZones.member.1", "us-west-2a"),
        ];
        params.extend(launch_source(row.source, row.template, &template_id));
        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the group must be created, got {status} with {body}"
        );

        let instance_id = launched_instance(&router, case, row.group).await;
        let tags = instance_tags(&router, case, &instance_id).await;
        for (tag, expected) in [
            (LAUNCH_TEMPLATE_ID_TAG, template_id.as_str()),
            (LAUNCH_TEMPLATE_VERSION_TAG, row.launched_version),
            (GROUP_NAME_TAG, row.group),
        ] {
            let held = tags
                .iter()
                .find(|(key, _)| key == tag)
                .map(|(_, value)| value.clone());
            assert_eq!(
                held.as_deref(),
                Some(expected),
                "case {case:?}: expected the instance to carry {tag}={expected:?}, got {tags:?}"
            );
        }
    }
}

/// A router carrying Amazon EC2 Auto Scaling and Amazon EC2 on ONE engine: the instances a
/// group launches are the same account's Amazon EC2 instances, so one engine is what makes the
/// two views one account.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(autoscaling::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(ec2_service::EmulatedService::new(Arc::clone(&engine)));
    router
}

/// Create one launch template over Amazon EC2's own wire, with `versions` versions, and answer
/// the `(id, latest version number)` that service assigned it.
///
/// Each version names its own image, so which version a launch read is a fact of the account's
/// own EC2 state rather than a value written out here.
async fn create_template(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    versions: usize,
) -> (String, String) {
    let (status, body) = ec2_query(
        router,
        "CreateLaunchTemplate",
        &[
            p("LaunchTemplateName", name),
            p("LaunchTemplateData.ImageId", "ami-0123456789abcdef0"),
            p("LaunchTemplateData.InstanceType", "t3.micro"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateLaunchTemplate must answer 200, got {status} with {body}"
    );
    let id = tag_value(&body, "launchTemplateId");
    let mut latest = tag_value(&body, "latestVersionNumber");
    for _ in 1..versions {
        let (status, body) = ec2_query(
            router,
            "CreateLaunchTemplateVersion",
            &[
                p("LaunchTemplateId", &id),
                p("LaunchTemplateData.ImageId", "ami-0123456789abcdef1"),
                p("LaunchTemplateData.InstanceType", "t3.small"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateLaunchTemplateVersion must answer 200, got {status} with \
             {body}"
        );
        latest = tag_value(&body, "versionNumber");
    }
    (id, latest)
}

/// The members a group's create carries to name its launch source.
fn launch_source(source: Source, name: &str, id: &str) -> Vec<(String, String)> {
    let (prefix, named, version) = match source {
        Source::ById(version) => ("LaunchTemplate", p("LaunchTemplateId", id), version),
        Source::ByName(version) => ("LaunchTemplate", p("LaunchTemplateName", name), version),
        Source::MixedPolicy(version) => (
            "MixedInstancesPolicy.LaunchTemplate.LaunchTemplateSpecification",
            p("LaunchTemplateId", id),
            version,
        ),
    };
    let mut params = vec![p(&format!("{prefix}.{}", named.0), named.1)];
    if let Some(version) = version {
        params.push(p(&format!("{prefix}.Version"), version));
    }
    params
}

/// The id of the one instance the group holds, polled until this service reports it.
///
/// A group's capacity change launches its instances as the call that asked for it runs, so what
/// this waits for is the CONDITION — the group reporting an instance — under a deadline far
/// longer than the span this emulator gives a launch.
async fn launched_instance(router: &MultiServiceRouter, case: &str, group: &str) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let (status, body) = query(
            router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", group)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeAutoScalingGroups must answer 200, got {status} with {body}"
        );
        let reported = tag_value(&body, "InstanceId");
        if !reported.is_empty() {
            return reported;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the group to launch an instance, got {body}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The tags Amazon EC2 reports for ONE instance, as `(key, value)` pairs.
///
/// Read off a `DescribeInstances` that named exactly that instance, so every member of the
/// answer belongs to it; the keys and the values are reported as parallel lists, which is
/// sound because every tag entry carries both.
async fn instance_tags(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> Vec<(String, String)> {
    let (status, body) =
        ec2_query(router, "DescribeInstances", &[p("InstanceId.1", instance_id)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstances({instance_id}) must answer 200, got {status} with \
         {body}"
    );
    assert_eq!(
        tag_value(&body, "instanceId"),
        instance_id,
        "case {case:?}: expected Amazon EC2 to report that instance in {REGION}, got {body}"
    );
    tag_values(&body, "key")
        .into_iter()
        .zip(tag_values(&body, "value"))
        .collect()
}
