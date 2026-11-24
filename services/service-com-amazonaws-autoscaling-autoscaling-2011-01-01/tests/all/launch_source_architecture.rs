// SPDX-License-Identifier: CC0-1.0

//! The ARCHITECTURE an Auto Scaling group's launch source would run on.
//!
//! RULE under test: a group launches an instance only for a configuration Amazon EC2 would
//! carry out, so a launch source pairing an AMI with an instance type whose architectures
//! disagree places NOTHING: the capacity change records a terminal `Failed` scaling activity
//! carrying Amazon EC2's own sentence for the pair it will not launch, the group reports no
//! instance at all, and the `DesiredCapacity` that was asked for stands unmet. A pair whose
//! architectures agree launches the instance and the activity is `Successful`.
//!
//! The rows span what the rule generalises over: which side is 64-bit Arm and which is 64-bit
//! x86, and the 32-bit AMIs that only some instance types still run — the same AMI
//! architecture is launchable on one type and refused on another, so no row is keyed to one
//! recorded pair. The architecture values are the model's own `ArchitectureValues`
//! (`smithy_query(query="shape",shape="ArchitectureValues",`
//! `model_path="services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json")`),
//! and the types that run 32-bit AMIs are the previous-generation and small burstable sizes
//! the EC2 User Guide lists for `i386`.
//!
//! Auto Scaling and Amazon EC2 are mounted on ONE engine and driven over their own wire
//! protocols — awsQuery and ec2Query — so every image, template, instance and activity is
//! written and read where a client writes and reads it.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ec2_amazonec2::state as ec2;

use crate::support::{
    borrowed, ec2_query, emulator, list_members, p, query, tag_value, tag_values, xml_unescape,
    REGION,
};

/// How long a case keeps looking for the outcome it waits on before it gives up — generous on
/// purpose, so a loaded host is never the reason a case fails.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long one look-again cycle pauses before asking again.
const POLL_PAUSE: Duration = Duration::from_millis(25);

/// The status codes a scaling activity has FINISHED in, from the model's own
/// `ScalingActivityStatusCode`: a client polling for the outcome of a capacity change stops at
/// one of these.
const TERMINAL_STATUS_CODES: &[&str] = &["Successful", "Failed", "Cancelled"];

/// The zone every group here places its instances in: the first of the Region's own catalog,
/// never written out.
fn zone() -> String {
    ec2::availability_zones_in_region(REGION)
        .first()
        .map(|zone| zone.zone_name.clone())
        .expect("the Region has Availability Zones")
}

/// What a launch Amazon EC2 will not carry out for a disagreeing AMI and instance type reports
/// as its `StatusMessage`, written out here rather than taken from either implementation: a
/// case that re-used the service's own literal would assert nothing about it.
///
/// SOURCE: Amazon EC2's `InvalidParameterValue` sentence for a launch whose instance type and
/// AMI are built for different architectures, followed by the sentence Amazon EC2 Auto Scaling
/// ends every failed launch with.
fn mismatch_message(type_architecture: &str, image_architecture: &str) -> String {
    format!(
        "The architecture '{type_architecture}' of the specified instance type does not match \
         the architecture '{image_architecture}' of the specified AMI. Specify an instance type \
         and an AMI that have matching architectures, and try again. You can use \
         'describe-instance-types' or 'describe-images' to discover the architecture of the \
         instance type or AMI. Launching EC2 instance failed."
    )
}

/// Register one AMI of the account for `architecture`, over Amazon EC2's own wire, and answer
/// the id EC2 minted for it.
async fn register_image(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    architecture: &str,
) -> String {
    let (status, body) = ec2_query(
        router,
        "RegisterImage",
        &[
            p("Name", name),
            p("Architecture", architecture),
            p("RootDeviceName", "/dev/xvda"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: RegisterImage for {name:?} expected 200, got {status}: {body}"
    );
    let image_id = tag_value(&body, "imageId");
    assert!(
        !image_id.is_empty(),
        "case {case:?}: RegisterImage must report the image's id, got {body}"
    );
    // The account's own read of the image is what the launch resolves, so the row rests on the
    // architecture EC2 holds rather than on the one it was asked for.
    let (status, body) = ec2_query(router, "DescribeImages", &[p("ImageId.1", &image_id)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeImages expected 200, got {status}: {body}"
    );
    assert_eq!(
        tag_value(&body, "architecture"),
        architecture,
        "case {case:?}: the registered image must report the architecture it was registered \
         for, got {body}"
    );
    image_id
}

/// Create one launch template naming `image_id` and `instance_type`, over Amazon EC2's own
/// wire.
async fn create_launch_template(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    image_id: &str,
    instance_type: &str,
) {
    let (status, body) = ec2_query(
        router,
        "CreateLaunchTemplate",
        &[
            p("LaunchTemplateName", name),
            p("LaunchTemplateData.ImageId", image_id),
            p("LaunchTemplateData.InstanceType", instance_type),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateLaunchTemplate for {name:?} expected 200, got {status}: {body}"
    );
}

/// Create one group of maximum size one, holding nothing yet, that launches from `template`.
async fn create_group(router: &MultiServiceRouter, case: &str, group: &str, template: &str) {
    let params = [
        p("AutoScalingGroupName", group),
        p("MinSize", "0"),
        p("MaxSize", "1"),
        p("DesiredCapacity", "0"),
        p("AvailabilityZones.member.1", zone()),
        p("LaunchTemplate.LaunchTemplateName", template),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAutoScalingGroup for {group:?} expected 200, got {status}: {body}"
    );
}

/// Ask the group for one unit of capacity.
async fn ask_for_one_instance(router: &MultiServiceRouter, case: &str, group: &str) {
    let (status, body) = query(
        router,
        "SetDesiredCapacity",
        &[("AutoScalingGroupName", group), ("DesiredCapacity", "1")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetDesiredCapacity for {group:?} expected 200, got {status}: {body}"
    );
}

/// What one scaling activity reported: the members a client reads the outcome of a capacity
/// change off.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Outcome {
    status_code: String,
    status_message: String,
}

/// The group's NEWEST scaling activity once it has reached a terminal status code, read
/// through `DescribeScalingActivities` the way a client polls for the outcome of the capacity
/// change it just asked for.
///
/// Looked for again until it is terminal or [`DEADLINE`] passes: an activity the clock still
/// owes is reported in progress, so this waits for the OUTCOME rather than for any length of
/// time.
async fn settled_activity(router: &MultiServiceRouter, case: &str, group: &str) -> Outcome {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, body) = query(
            router,
            "DescribeScalingActivities",
            &[("AutoScalingGroupName", group)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeScalingActivities expected 200, got {status}: {body}"
        );
        let members = list_members(&body, "Activities");
        let newest = members.first().copied().unwrap_or_default();
        let status_code = tag_value(newest, "StatusCode");
        if TERMINAL_STATUS_CODES.contains(&status_code.as_str()) {
            return Outcome {
                status_code,
                status_message: xml_unescape(&tag_value(newest, "StatusMessage")),
            };
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the capacity change must record a terminal activity, got {body}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// What the group reports about itself: the instances it holds and the capacity it maintains.
async fn group_view(router: &MultiServiceRouter, case: &str, group: &str) -> (Vec<String>, String) {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAutoScalingGroups expected 200, got {status}: {body}"
    );
    (
        tag_values(&body, "InstanceId"),
        tag_value(&body, "DesiredCapacity"),
    )
}

/// The instance ids the account's own Amazon EC2 store holds, so a refused launch is shown to
/// have minted nothing anywhere rather than merely to be unreported by the group.
async fn account_instances(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (status, body) = ec2_query(router, "DescribeInstances", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstances expected 200, got {status}: {body}"
    );
    tag_values(&body, "instanceId")
}

/// One row of the table: the architecture the AMI is registered for, the instance type the
/// launch template names, and whether Amazon EC2 would carry that launch out.
struct Row {
    case: &'static str,
    group: &'static str,
    image_architecture: &'static str,
    instance_type: &'static str,
    /// The architecture of that instance type, which the refusal names — `None` for a row
    /// whose launch is made.
    refused_as: Option<&'static str>,
}

/// A group whose launch source pairs an AMI and an instance type built for different
/// architectures places no instance and records Amazon EC2's own sentence for the pair; one
/// whose architectures agree launches its instance.
#[tokio::test]
async fn a_launch_is_made_only_where_the_ami_and_the_instance_type_agree() {
    let rows = [
        Row {
            case: "a 64-bit x86 AMI on a 64-bit x86 type",
            group: "arch-x86-on-x86",
            image_architecture: "x86_64",
            instance_type: "t3.micro",
            refused_as: None,
        },
        Row {
            case: "a 64-bit Arm AMI on a 64-bit Arm type",
            group: "arch-arm-on-arm",
            image_architecture: "arm64",
            instance_type: "t4g.micro",
            refused_as: None,
        },
        Row {
            case: "a 32-bit AMI on a type that still runs one",
            group: "arch-i386-on-burstable",
            image_architecture: "i386",
            instance_type: "t2.micro",
            refused_as: None,
        },
        Row {
            case: "a 64-bit x86 AMI on a 64-bit Arm type",
            group: "arch-x86-on-arm",
            image_architecture: "x86_64",
            instance_type: "t4g.micro",
            refused_as: Some("arm64"),
        },
        Row {
            case: "a 64-bit Arm AMI on a 64-bit x86 type",
            group: "arch-arm-on-x86",
            image_architecture: "arm64",
            instance_type: "m5.large",
            refused_as: Some("x86_64"),
        },
        Row {
            case: "a 32-bit AMI on a type that does not run one",
            group: "arch-i386-on-current",
            image_architecture: "i386",
            instance_type: "t3.micro",
            refused_as: Some("x86_64"),
        },
    ];

    for row in &rows {
        let case = row.case;
        // One emulator per row: each row is its own account, so the instances of one row never
        // explain another's.
        let (router, _engine) = emulator();
        let image_name = format!("{}-image", row.group);
        let image_id = register_image(&router, case, &image_name, row.image_architecture).await;
        let template = format!("{}-template", row.group);
        create_launch_template(&router, case, &template, &image_id, row.instance_type).await;
        create_group(&router, case, row.group, &template).await;
        ask_for_one_instance(&router, case, row.group).await;

        let outcome = settled_activity(&router, case, row.group).await;
        let (instances, desired) = group_view(&router, case, row.group).await;
        match row.refused_as {
            Some(type_architecture) => {
                assert_eq!(
                    (
                        outcome.status_code.as_str(),
                        outcome.status_message.as_str(),
                    ),
                    (
                        "Failed",
                        mismatch_message(type_architecture, row.image_architecture).as_str(),
                    ),
                    "case {case:?}: expected a failed activity carrying Amazon EC2's sentence \
                     for the pair, got {outcome:?}"
                );
                assert!(
                    instances.is_empty(),
                    "case {case:?}: the group must hold no instance when Amazon EC2 would not \
                     launch its source, got {instances:?}"
                );
                let minted = account_instances(&router, case).await;
                assert!(
                    minted.is_empty(),
                    "case {case:?}: a launch Amazon EC2 would refuse mints no instance of the \
                     account either, got {minted:?}"
                );
            }
            None => {
                assert_eq!(
                    outcome.status_code, "Successful",
                    "case {case:?}: a launch whose architectures agree must succeed, got \
                     {outcome:?}"
                );
                assert!(
                    outcome.status_message.is_empty(),
                    "case {case:?}: an activity that did not fail carries no status message, got \
                     {outcome:?}"
                );
                assert_eq!(
                    instances.len(),
                    1,
                    "case {case:?}: the group must hold the instance it launched, got \
                     {instances:?}"
                );
            }
        }
        // The capacity the caller asked for STANDS either way: it is the capacity the group
        // maintains, not an instruction one call carried out.
        assert_eq!(
            desired, "1",
            "case {case:?}: the group must keep the capacity that was asked for, got {desired:?}"
        );
    }
}

/// The capacity a refused launch could not place stays OWED: repointing the group at a launch
/// source whose architectures agree launches it, with no further capacity change from anybody.
#[tokio::test]
async fn the_capacity_a_refused_launch_left_owed_is_placed_once_the_source_agrees() {
    let case = "a group repointed at a template whose architectures agree";
    let (router, _engine) = emulator();
    let group = "arch-repointed";
    let image_id = register_image(&router, case, "arch-repointed-image", "arm64").await;
    create_launch_template(&router, case, "arch-repointed-mismatched", &image_id, "m5.large").await;
    create_group(&router, case, group, "arch-repointed-mismatched").await;
    ask_for_one_instance(&router, case, group).await;

    let refused = settled_activity(&router, case, group).await;
    assert_eq!(
        (refused.status_code.as_str(), refused.status_message.as_str()),
        ("Failed", mismatch_message("x86_64", "arm64").as_str()),
        "case {case:?}: the scale-out must fail while the source disagrees, got {refused:?}"
    );

    // The client's own remediation: the group is repointed at a template that names a type the
    // AMI runs. Nothing asks for the capacity again.
    create_launch_template(&router, case, "arch-repointed-agreeing", &image_id, "t4g.micro").await;
    let params = [
        p("AutoScalingGroupName", group),
        p("LaunchTemplate.LaunchTemplateName", "arch-repointed-agreeing"),
    ];
    let (status, body) = query(&router, "UpdateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: UpdateAutoScalingGroup expected 200, got {status}: {body}"
    );

    let deadline = Instant::now() + DEADLINE;
    let instances = loop {
        let (instances, _) = group_view(&router, case, group).await;
        if !instances.is_empty() || Instant::now() >= deadline {
            break instances;
        }
        tokio::time::sleep(POLL_PAUSE).await;
    };
    assert_eq!(
        instances.len(),
        1,
        "case {case:?}: the group must launch the capacity it was owed once repointed, got \
         {instances:?}"
    );
    let settled = settled_activity(&router, case, group).await;
    assert_eq!(
        settled.status_code, "Successful",
        "case {case:?}: the launch made after the repair must succeed, got {settled:?}"
    );
}
