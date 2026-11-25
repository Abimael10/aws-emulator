// SPDX-License-Identifier: CC0-1.0

//! The IAM instance profile an instance an Auto Scaling group launches RUNS AS is the one
//! named by the launch-template VERSION the group's specification resolves to — and Amazon
//! EC2 reports it on that instance.
//!
//! RULE under test: a capacity change reads the ONE launch-template version the group's
//! `LaunchTemplateSpecification` names (the version by number, or by `$Latest` or
//! `$Default`, which is also what a specification carrying no version means), and the
//! `IamInstanceProfile` THAT version carries is the profile the launched instance runs as,
//! resolved against the account's own IAM records — so `DescribeInstances` reports
//! `Instance.IamInstanceProfile` with the ARN and the id IAM minted for the profile
//! (`Arn`: "The Amazon Resource Name (ARN) of the instance profile", `Id`: "The ID of the
//! instance profile",
//! `smithy_query(query="shape",shape="IamInstanceProfile")`), and reports NO such member
//! for an instance whose resolved version names no profile. The association Amazon EC2
//! holds over the instance is the same record, so `DescribeIamInstanceProfileAssociations`
//! and the instance's own projection cannot disagree.
//!
//! The rows below differ along what the rule generalises over: which version of a two-version
//! template the group resolves (an explicit number, `$Latest`, `$Default` and no selector at
//! all), where the template's default version sits when the group resolves it, whether the
//! newer version exists yet when the group is CREATED or is added before the capacity change,
//! how the template spells the reference (`Arn` or `Name`), whether the resolved version names
//! a profile at all, and how the group reaches one instance (capacity set at creation,
//! `SetDesiredCapacity`, or `UpdateAutoScalingGroup`). Every row's template has TWO versions
//! naming DIFFERENT profiles, so a launch that read the wrong version reports the wrong ARN
//! rather than nothing.
//!
//! Auto Scaling, Amazon EC2 and IAM are mounted on ONE engine and driven over their own wire
//! protocols — awsQuery for Auto Scaling and IAM, ec2Query for Amazon EC2 — so every
//! assertion is made where a client makes it.
//!
//! Nothing here waits a fixed time: the instance a capacity change launches is looked for
//! AGAIN until the group reports it or [`DEADLINE`] passes.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, ec2_query, emulator_with_iam, iam_query, p, query, seed_subnets, tag_value,
    tag_values,
};

/// How long a row looks again for the instance its capacity change launched.
///
/// Generous on purpose: the launch is a stored due instant of another component's clock and
/// nothing sleeps for it, so a row asks again until the group reports the instance.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long one look-again cycle pauses before asking again.
const POLL_PAUSE: Duration = Duration::from_millis(50);

/// The instance type every launch-template version here carries, so one version differs
/// from another only in the reference under test. The AMI is read off Amazon EC2's own
/// published images ([`image_for_the_instance_type`]), because a launch whose AMI is built
/// for another processor architecture than its instance type is one Amazon EC2 declines.
const INSTANCE_TYPE: &str = "t3.micro";

/// The processor architecture [`INSTANCE_TYPE`] runs, so the AMI every version names is one
/// it can boot.
const ARCHITECTURE: &str = "x86_64";

/// The instance profile IAM holds: the name it is addressed by, and the ARN and unique id
/// IAM minted for it — never values this suite spells out.
#[derive(Debug, Clone)]
struct HeldProfile {
    name: String,
    arn: String,
    id: String,
}

/// How a launch-template version spells its `IamInstanceProfile` reference.
#[derive(Debug, Clone, Copy)]
enum Spelling {
    /// `Arn`, the ARN IAM minted for the profile.
    ByArn,
    /// `Name`, the name IAM holds it under.
    ByName,
}

impl Spelling {
    /// The launch parameter the reference is carried under.
    fn member(self) -> &'static str {
        match self {
            Self::ByArn => "Arn",
            Self::ByName => "Name",
        }
    }

    /// The value that parameter carries for `profile`, taken from what IAM holds.
    fn value(self, profile: &HeldProfile) -> String {
        match self {
            Self::ByArn => profile.arn.clone(),
            Self::ByName => profile.name.clone(),
        }
    }
}

/// How the group comes to hold one instance.
#[derive(Debug, Clone, Copy)]
enum Launch {
    /// `CreateAutoScalingGroup` already asks for the capacity.
    AtCreate,
    /// The group is created empty and then scaled out by `SetDesiredCapacity`.
    SetDesiredCapacity,
    /// The group is created empty and then scaled out by `UpdateAutoScalingGroup`.
    UpdateGroup,
}

/// Which version's profile the launched instance must be reported running as.
#[derive(Debug, Clone, Copy)]
enum Expected {
    /// The profile the template's FIRST version names.
    FirstVersion,
    /// The profile its SECOND version names.
    SecondVersion,
    /// None at all: the resolved version names no instance profile.
    Nothing,
}

/// One row: a two-version launch template, the version the group resolves, and the profile
/// its instance must therefore run as.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    spelling: Spelling,
    /// Whether the SECOND version names an instance profile of its own; `false` means it
    /// names none.
    second_names_profile: bool,
    /// Whether that second version is created only AFTER the group was, so the selector is
    /// resolved against a template that grew in the meantime.
    second_version_after_group: bool,
    /// Whether `ModifyLaunchTemplate` moves the template's default version onto the second.
    default_moved_to_second: bool,
    /// The `Version` the group's launch-template specification carries; `None` leaves it
    /// out, which the model documents as `$Default`.
    selector: Option<&'static str>,
    launch: Launch,
    expected: Expected,
}

/// Create one instance profile through IAM's own API and answer the ARN and id IAM minted
/// for it.
async fn create_profile(router: &MultiServiceRouter, case: &str, name: &str) -> HeldProfile {
    let (status, body) =
        iam_query(router, "CreateInstanceProfile", &[p("InstanceProfileName", name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateInstanceProfile({name}) must succeed, got {status}: {body}"
    );
    let arn = tag_value(&body, "Arn");
    let id = tag_value(&body, "InstanceProfileId");
    assert!(
        !arn.is_empty() && !id.is_empty(),
        "case {case:?}: CreateInstanceProfile({name}) must report the profile's ARN and id, \
         got {body}"
    );
    HeldProfile {
        name: name.to_string(),
        arn,
        id,
    }
}

/// One AMI of the account's own Amazon EC2 images that [`INSTANCE_TYPE`] can boot: an
/// available image built for [`ARCHITECTURE`], asked of the service that owns the images
/// rather than written out here.
async fn image_for_the_instance_type(router: &MultiServiceRouter) -> String {
    let (status, body) = ec2_query(
        router,
        "DescribeImages",
        &[
            p("Filter.1.Name", "architecture"),
            p("Filter.1.Value.1", ARCHITECTURE),
            p("Filter.2.Name", "state"),
            p("Filter.2.Value.1", "available"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeImages must answer, got {status}: {body}"
    );
    let reported = tag_values(&body, "imageId");
    reported.first().cloned().unwrap_or_else(|| {
        panic!("expected one available {ARCHITECTURE} image to launch from, got {reported:?}")
    })
}

/// The `LaunchTemplateData` parameters one version carries: the image and instance type
/// every version here has, plus the `IamInstanceProfile` reference when it names one.
fn version_data(image_id: &str, reference: Option<(&str, String)>) -> Vec<(String, String)> {
    let mut params = vec![
        p("LaunchTemplateData.ImageId", image_id),
        p("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
    ];
    if let Some((member, value)) = reference {
        params.push(p(
            &format!("LaunchTemplateData.IamInstanceProfile.{member}"),
            value,
        ));
    }
    params
}

/// Create one Amazon EC2 launch template over the wire, whose first version carries
/// `reference`.
async fn create_launch_template(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    image_id: &str,
    reference: Option<(&str, String)>,
) {
    let mut params = vec![p("LaunchTemplateName", name)];
    params.extend(version_data(image_id, reference));
    let (status, body) = ec2_query(router, "CreateLaunchTemplate", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateLaunchTemplate({name}) must succeed, got {status}: {body}"
    );
}

/// Add one further version to the launch template `name`, carrying `reference`, and assert
/// Amazon EC2 numbered it the version this suite expects.
async fn create_launch_template_version(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    image_id: &str,
    reference: Option<(&str, String)>,
    expected_number: &str,
) {
    let mut params = vec![p("LaunchTemplateName", name)];
    params.extend(version_data(image_id, reference));
    let (status, body) = ec2_query(router, "CreateLaunchTemplateVersion", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateLaunchTemplateVersion({name}) must succeed, got {status}: {body}"
    );
    let number = tag_value(&body, "versionNumber");
    assert_eq!(
        number, expected_number,
        "case {case:?}: expected the new version of {name} to be numbered \
         {expected_number:?}, got {number:?} in {body}"
    );
}

/// Move the launch template's default version onto `version`.
async fn set_default_version(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    version: &str,
) {
    let (status, body) = ec2_query(
        router,
        "ModifyLaunchTemplate",
        &[
            p("LaunchTemplateName", name),
            p("SetDefaultVersion", version),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ModifyLaunchTemplate({name}) must succeed, got {status}: {body}"
    );
    let reported = tag_value(&body, "defaultVersionNumber");
    assert_eq!(
        reported, version,
        "case {case:?}: expected {name} to report default version {version:?}, got \
         {reported:?} in {body}"
    );
}

/// The one instance the group `name` holds, asked for again until it reports exactly one.
async fn sole_instance(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let until = Instant::now() + DEADLINE;
    loop {
        let (status, body) = query(
            router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeAutoScalingGroups must answer, got {status}: {body}"
        );
        let reported = tag_values(&body, "InstanceId");
        if let [instance_id] = reported.as_slice() {
            return instance_id.clone();
        }
        if Instant::now() >= until {
            // What the group DID instead: a launch the service declined records its own
            // scaling activity, so the reason is part of the failure rather than hidden.
            let (_, activities) = query(
                router,
                "DescribeScalingActivities",
                &[("AutoScalingGroupName", name)],
            )
            .await;
            panic!(
                "case {case:?}: expected the group {name} to hold one instance, got \
                 {reported:?}; its scaling activities: {activities}"
            );
        }
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// The instance profile Amazon EC2 reports on `instance_id` itself, as `(ARN, id)`, or
/// `None` when the instance carries no such member.
async fn instance_profile_of(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> Option<(String, String)> {
    let (status, body) =
        ec2_query(router, "DescribeInstances", &[p("InstanceId.1", instance_id)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstances({instance_id}) must answer, got {status}: {body}"
    );
    let reported = tag_value(&body, "iamInstanceProfile");
    if reported.is_empty() {
        return None;
    }
    Some((tag_value(&reported, "arn"), tag_value(&reported, "id")))
}

/// Every instance-profile association Amazon EC2 holds over `instance_id`, as `(ARN, id)`
/// pairs — the association store's own answer to the same question.
async fn associated_profiles_of(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> Vec<(String, String)> {
    let (status, body) = ec2_query(
        router,
        "DescribeIamInstanceProfileAssociations",
        &[
            p("Filter.1.Name", "instance-id"),
            p("Filter.1.Value.1", instance_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeIamInstanceProfileAssociations({instance_id}) must answer, \
         got {status}: {body}"
    );
    tag_values(&body, "iamInstanceProfile")
        .iter()
        .map(|block| (tag_value(block, "arn"), tag_value(block, "id")))
        .collect()
}

/// An instance a group launches runs as the profile the launch-template version the group
/// RESOLVES names, whichever of the template's versions that is.
#[tokio::test]
async fn a_launched_instance_runs_as_the_resolved_versions_instance_profile() {
    let rows = [
        Row {
            case: "an explicit version 1 while the newest version names another profile, \
                   scaled out from nothing",
            spelling: Spelling::ByArn,
            second_names_profile: true,
            second_version_after_group: false,
            default_moved_to_second: false,
            selector: Some("1"),
            launch: Launch::SetDesiredCapacity,
            expected: Expected::FirstVersion,
        },
        Row {
            case: "$Latest, named by name, with the capacity asked for when the group is \
                   created",
            spelling: Spelling::ByName,
            second_names_profile: true,
            second_version_after_group: false,
            default_moved_to_second: false,
            selector: Some("$Latest"),
            launch: Launch::AtCreate,
            expected: Expected::SecondVersion,
        },
        Row {
            case: "$Default while the default version is still the first, scaled out by an \
                   update",
            spelling: Spelling::ByArn,
            second_names_profile: true,
            second_version_after_group: false,
            default_moved_to_second: false,
            selector: Some("$Default"),
            launch: Launch::UpdateGroup,
            expected: Expected::FirstVersion,
        },
        Row {
            case: "no version selector at all, after the default version was moved onto the \
                   second",
            spelling: Spelling::ByName,
            second_names_profile: true,
            second_version_after_group: false,
            default_moved_to_second: true,
            selector: None,
            launch: Launch::SetDesiredCapacity,
            expected: Expected::SecondVersion,
        },
        Row {
            case: "$Latest where the newest version names no instance profile at all",
            spelling: Spelling::ByArn,
            second_names_profile: false,
            second_version_after_group: false,
            default_moved_to_second: false,
            selector: Some("$Latest"),
            launch: Launch::SetDesiredCapacity,
            expected: Expected::Nothing,
        },
        Row {
            case: "$Latest resolved at the capacity change, the second version added after \
                   the group was created",
            spelling: Spelling::ByName,
            second_names_profile: true,
            second_version_after_group: true,
            default_moved_to_second: false,
            selector: Some("$Latest"),
            launch: Launch::SetDesiredCapacity,
            expected: Expected::SecondVersion,
        },
        Row {
            case: "an explicit version 2, named by ARN, with the capacity asked for when the \
                   group is created",
            spelling: Spelling::ByArn,
            second_names_profile: true,
            second_version_after_group: false,
            default_moved_to_second: false,
            selector: Some("2"),
            launch: Launch::AtCreate,
            expected: Expected::SecondVersion,
        },
    ];

    // One account for every row: the rows are told apart by the names they create, and one
    // engine is what makes the Auto Scaling, Amazon EC2 and IAM views that account.
    let (router, engine) = emulator_with_iam();
    let subnet = seed_subnets(&engine, 1)
        .first()
        .map(|subnet| subnet.id.clone())
        .expect("one seeded subnet");
    let image_id = image_for_the_instance_type(&router).await;

    for (index, row) in rows.iter().enumerate() {
        // Two profiles IAM holds, one per version, so a launch that read the wrong version
        // reports the wrong ARN rather than nothing.
        let first = create_profile(&router, row.case, &format!("asg-profile-{index}-one")).await;
        let second = if row.second_names_profile {
            Some(create_profile(&router, row.case, &format!("asg-profile-{index}-two")).await)
        } else {
            None
        };

        let template = format!("lt-resolved-{index}");
        create_launch_template(
            &router,
            row.case,
            &template,
            &image_id,
            Some((row.spelling.member(), row.spelling.value(&first))),
        )
        .await;
        let second_reference = second
            .as_ref()
            .map(|held| (row.spelling.member(), row.spelling.value(held)));
        if !row.second_version_after_group {
            create_launch_template_version(
                &router,
                row.case,
                &template,
                &image_id,
                second_reference.clone(),
                "2",
            )
            .await;
            if row.default_moved_to_second {
                set_default_version(&router, row.case, &template, "2").await;
            }
        }

        let group = format!("asg-resolved-{index}");
        let desired_at_create = match row.launch {
            Launch::AtCreate => "1",
            Launch::SetDesiredCapacity | Launch::UpdateGroup => "0",
        };
        let mut params = vec![
            p("AutoScalingGroupName", group.clone()),
            p("MinSize", "0"),
            p("MaxSize", "1"),
            p("DesiredCapacity", desired_at_create),
            p("VPCZoneIdentifier", subnet.clone()),
            p("LaunchTemplate.LaunchTemplateName", template.clone()),
        ];
        if let Some(selector) = row.selector {
            params.push(p("LaunchTemplate.Version", selector));
        }
        let (status, body) =
            query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {:?}: CreateAutoScalingGroup must succeed, got {status}: {body}",
            row.case
        );

        // The version the group resolves may be one added after it was created: `$Latest`
        // is a way of NAMING a version, read when the launch happens.
        if row.second_version_after_group {
            create_launch_template_version(
                &router,
                row.case,
                &template,
                &image_id,
                second_reference.clone(),
                "2",
            )
            .await;
            if row.default_moved_to_second {
                set_default_version(&router, row.case, &template, "2").await;
            }
        }

        match row.launch {
            Launch::AtCreate => {}
            Launch::SetDesiredCapacity => {
                let (status, body) = query(
                    &router,
                    "SetDesiredCapacity",
                    &[
                        ("AutoScalingGroupName", group.as_str()),
                        ("DesiredCapacity", "1"),
                    ],
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {:?}: SetDesiredCapacity must succeed, got {status}: {body}",
                    row.case
                );
            }
            Launch::UpdateGroup => {
                let (status, body) = query(
                    &router,
                    "UpdateAutoScalingGroup",
                    &[
                        ("AutoScalingGroupName", group.as_str()),
                        ("DesiredCapacity", "1"),
                    ],
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {:?}: UpdateAutoScalingGroup must succeed, got {status}: {body}",
                    row.case
                );
            }
        }

        let instance_id = sole_instance(&router, row.case, &group).await;
        let expected = match row.expected {
            Expected::FirstVersion => Some((first.arn.clone(), first.id.clone())),
            Expected::SecondVersion => second
                .as_ref()
                .map(|held| (held.arn.clone(), held.id.clone())),
            Expected::Nothing => None,
        };

        // What the instance itself reports: the member a client reads the instance's role
        // off, present exactly when the resolved version names a profile.
        let reported = instance_profile_of(&router, row.case, &instance_id).await;
        assert_eq!(
            reported, expected,
            "case {:?}: expected DescribeInstances to report the instance profile \
             {expected:?} on {instance_id}, got {reported:?}",
            row.case
        );

        // And the association Amazon EC2 holds over the same instance: one record saying
        // the same thing, so the two reads cannot disagree.
        let associated = associated_profiles_of(&router, row.case, &instance_id).await;
        let expected_associations: Vec<(String, String)> = expected.iter().cloned().collect();
        assert_eq!(
            associated, expected_associations,
            "case {:?}: expected DescribeIamInstanceProfileAssociations over {instance_id} \
             to report {expected_associations:?}, got {associated:?}",
            row.case
        );
    }
}
