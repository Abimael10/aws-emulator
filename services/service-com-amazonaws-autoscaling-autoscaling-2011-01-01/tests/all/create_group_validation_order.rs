// SPDX-License-Identifier: CC0-1.0

//! WHICH refusal a `CreateAutoScalingGroup` that is wrong in two ways at once answers.
//!
//! RULE: a create resolves the LAUNCH SOURCE the request names — every reference it carries,
//! whichever service holds the resource it names — before it weighs the group's name against
//! the names the account has already taken. So a request whose launch source resolves to
//! nothing is refused `ValidationError` for the launch source whatever its
//! `AutoScalingGroupName` states, and `AlreadyExists` is answered only to a request whose
//! launch source does resolve.
//!
//! That is the order a client that provisions by "create and interpret the refusal" branches
//! on: `AlreadyExists` means "the group is already there, adopt it", while `ValidationError`
//! means "the request itself is wrong, fix the reference". A stale or mistyped reference
//! reported as "the group already exists" makes the client adopt a group built from another
//! source.
//!
//! The rows span what the rule generalises over: WHICH half of the reference fails to resolve
//! (the template's id, its name, the version of a template the account does hold), WHERE the
//! launch source names it (the group's own `LaunchTemplate`, the launch template of a
//! `MixedInstancesPolicy`, the template an override of that policy names), which KIND of
//! launch source carries it (a launch template, a launch configuration), and whether the name
//! the request states is taken at all — a launch source that resolves is accepted under a
//! free name and refused `AlreadyExists` under a taken one.
//!
//! A refused create also changes nothing: the group that holds the name keeps the sizes it
//! was created with.
//!
//! Auto Scaling and Amazon EC2 are mounted on ONE engine and driven over their own wire
//! protocols, the way the emulator serves them, so the launch template these requests resolve
//! against is one a client created.

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{borrowed, create_launch_template, emulator, p, query, tag_value, xml_unescape};

/// The group that holds the name every "taken" case re-states, and the sizes it keeps.
const EXISTING_GROUP: &str = "asg-validation-order";
const EXISTING_MIN_SIZE: &str = "1";
const EXISTING_MAX_SIZE: &str = "3";
const EXISTING_DESIRED_CAPACITY: &str = "2";

/// The sizes every SECOND create states — different from the ones the existing group keeps,
/// so a create that was refused but still applied would show up as a changed size.
const SECOND_MIN_SIZE: &str = "0";
const SECOND_MAX_SIZE: &str = "1";
const SECOND_DESIRED_CAPACITY: &str = "0";

/// The launch template the account HOLDS, created over Amazon EC2's own wire.
const TEMPLATE_NAME: &str = "asg-validation-order-template";

/// The instance type an override of a mixed instances policy overrides.
const INSTANCE_TYPE: &str = "t3.micro";

/// An id of the published form (`lt-` and 17 hex digits) no template of the account carries,
/// a template name nothing creates, and a launch configuration name nothing creates: each
/// names a launch source that resolves to nothing.
const ABSENT_TEMPLATE_ID: &str = "lt-00000000000000999";
const ABSENT_TEMPLATE_NAME: &str = "asg-validation-order-template-never-created";
const ABSENT_LAUNCH_CONFIGURATION: &str = "asg-validation-order-lc-never-created";

/// The query error code the service answers a malformed request with, and the one it answers
/// a name already taken with (the modelled `AlreadyExistsFault`'s `awsQueryError` code).
const VALIDATION_ERROR: &str = "ValidationError";
const ALREADY_EXISTS: &str = "AlreadyExists";

/// The sentence Amazon EC2 — the service that holds launch templates — words for a launch
/// template NAME the account holds no template under, as Auto Scaling surfaces it.
const ABSENT_TEMPLATE_NAME_MESSAGE: &str =
    "At least one of the launch templates specified in the request does not exist.";

/// The launch source the second create names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// The group's own `LaunchTemplate`, by an id the account holds no template under.
    AbsentTemplateId,
    /// Its own `LaunchTemplate`, by a name the account holds no template under.
    AbsentTemplateName,
    /// The launch template of a `MixedInstancesPolicy`, by an absent id.
    AbsentPolicyTemplateId,
    /// The template an OVERRIDE of that policy names, by an absent name.
    AbsentOverrideTemplateName,
    /// A template the account DOES hold, at a version it never minted.
    UnmintedVersion,
    /// A launch configuration the account does not hold.
    AbsentLaunchConfiguration,
    /// A template the account holds, at the version it holds: the launch source resolves, so
    /// the name is what decides the answer.
    HeldTemplate,
}

/// One row: the launch source under test, and whether it resolves at all.
struct Row {
    case: &'static str,
    source: Source,
}

const ROWS: &[Row] = &[
    Row {
        case: "an id of the published form the account holds no template under",
        source: Source::AbsentTemplateId,
    },
    Row {
        case: "a name the account holds no template under",
        source: Source::AbsentTemplateName,
    },
    Row {
        case: "the launch template of a mixed instances policy, by an absent id",
        source: Source::AbsentPolicyTemplateId,
    },
    Row {
        case: "the template an override of that policy names, by an absent name",
        source: Source::AbsentOverrideTemplateName,
    },
    Row {
        case: "a template the account holds, at a version it never minted",
        source: Source::UnmintedVersion,
    },
    Row {
        case: "a launch configuration the account does not hold",
        source: Source::AbsentLaunchConfiguration,
    },
    Row {
        case: "a launch source that resolves: the name decides the answer",
        source: Source::HeldTemplate,
    },
];

/// The launch template the account holds, as the suite reads it off Amazon EC2: the id that
/// service assigned it, and a numbered version it never minted (one past its latest).
struct HeldTemplate {
    id: String,
    unminted_version: String,
}

/// One emulator holding the account's launch template and the group that holds the name.
async fn seeded() -> (MultiServiceRouter, HeldTemplate) {
    let (router, _engine) = emulator();
    let (id, latest_version) = create_launch_template(&router, TEMPLATE_NAME).await;
    let latest: i64 = latest_version
        .parse()
        .expect("Amazon EC2 reports the template's latest version as a number");
    let held = HeldTemplate {
        id,
        unminted_version: (latest + 1).to_string(),
    };

    let (status, body) = query(
        &router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", EXISTING_GROUP),
            ("LaunchTemplate.LaunchTemplateId", held.id.as_str()),
            ("MinSize", EXISTING_MIN_SIZE),
            ("MaxSize", EXISTING_MAX_SIZE),
            ("DesiredCapacity", EXISTING_DESIRED_CAPACITY),
            ("AvailabilityZones.member.1", "us-west-2a"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the group that holds the name must be created, got {status}: {body}"
    );
    (router, held)
}

/// The launch-source parameters the row's second create states.
fn launch_source(source: Source, held: &HeldTemplate) -> Vec<(String, String)> {
    match source {
        Source::AbsentTemplateId => {
            vec![p("LaunchTemplate.LaunchTemplateId", ABSENT_TEMPLATE_ID)]
        }
        Source::AbsentTemplateName => {
            vec![p("LaunchTemplate.LaunchTemplateName", ABSENT_TEMPLATE_NAME)]
        }
        Source::AbsentPolicyTemplateId => vec![p(
            "MixedInstancesPolicy.LaunchTemplate.LaunchTemplateSpecification.LaunchTemplateId",
            ABSENT_TEMPLATE_ID,
        )],
        Source::AbsentOverrideTemplateName => vec![
            p(
                "MixedInstancesPolicy.LaunchTemplate.Overrides.member.1.\
                 LaunchTemplateSpecification.LaunchTemplateName",
                ABSENT_TEMPLATE_NAME,
            ),
            // An override names the instance type it overrides, beside the template it
            // launches that type from.
            p(
                "MixedInstancesPolicy.LaunchTemplate.Overrides.member.1.InstanceType",
                INSTANCE_TYPE,
            ),
        ],
        Source::UnmintedVersion => vec![
            p("LaunchTemplate.LaunchTemplateId", held.id.as_str()),
            p("LaunchTemplate.Version", held.unminted_version.as_str()),
        ],
        Source::AbsentLaunchConfiguration => {
            vec![p("LaunchConfigurationName", ABSENT_LAUNCH_CONFIGURATION)]
        }
        Source::HeldTemplate => vec![p("LaunchTemplate.LaunchTemplateId", held.id.as_str())],
    }
}

/// What AWS answers a request carrying this launch source under a name already taken: the
/// code, and the whole message.
///
/// Each sentence is written out here rather than taken from the implementation — a test that
/// re-used the implementation's own literal would assert nothing. The launch-template halves
/// are Amazon EC2's own wording for the half that did not resolve (its
/// `InvalidLaunchTemplateId.Malformed` sentence for an id, its "does not exist" sentence for a
/// name, and its version-resolution sentence for a version), and the last is Auto Scaling's
/// own sentence for a group name already taken.
fn expected_refusal(source: Source, held: &HeldTemplate) -> (&'static str, String) {
    match source {
        Source::AbsentTemplateId | Source::AbsentPolicyTemplateId => (
            VALIDATION_ERROR,
            format!("Malformed launch template Id: {ABSENT_TEMPLATE_ID}"),
        ),
        Source::AbsentTemplateName | Source::AbsentOverrideTemplateName => {
            (VALIDATION_ERROR, ABSENT_TEMPLATE_NAME_MESSAGE.to_string())
        }
        Source::UnmintedVersion => (
            VALIDATION_ERROR,
            format!(
                "Could not find the specified version {} for the launch template with ID {}.",
                held.unminted_version, held.id
            ),
        ),
        Source::AbsentLaunchConfiguration => (
            VALIDATION_ERROR,
            format!(
                "Launch configuration name not found - Launch configuration \
                 {ABSENT_LAUNCH_CONFIGURATION} not found"
            ),
        ),
        Source::HeldTemplate => (
            ALREADY_EXISTS,
            format!(
                "AutoScalingGroup by this name already exists - A group with the name \
                 {EXISTING_GROUP} already exists"
            ),
        ),
    }
}

/// The `(MinSize, MaxSize, DesiredCapacity)` a group reports, read back through
/// `DescribeAutoScalingGroups`.
async fn reported_sizes(
    router: &MultiServiceRouter,
    group: &str,
    case: &str,
) -> (String, String, String) {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAutoScalingGroups must answer 200, got {status}: {body}"
    );
    (
        tag_value(&body, "MinSize"),
        tag_value(&body, "MaxSize"),
        tag_value(&body, "DesiredCapacity"),
    )
}

#[tokio::test]
async fn the_launch_source_is_resolved_before_the_group_name_is_weighed() {
    for row in ROWS {
        let case = row.case;
        let (router, held) = seeded().await;
        let mut params = vec![
            p("AutoScalingGroupName", EXISTING_GROUP),
            p("MinSize", SECOND_MIN_SIZE),
            p("MaxSize", SECOND_MAX_SIZE),
            p("DesiredCapacity", SECOND_DESIRED_CAPACITY),
            p("AvailabilityZones.member.1", "us-west-2a"),
        ];
        params.extend(launch_source(row.source, &held));

        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a create under a name already taken must be refused, got \
             {status}: {body}"
        );
        let (expected_code, expected_message) = expected_refusal(row.source, &held);
        assert_eq!(
            tag_value(&body, "Code"),
            expected_code,
            "case {case:?}: expected the {expected_code} code, got {body}"
        );
        assert_eq!(
            xml_unescape(&tag_value(&body, "Message")),
            expected_message,
            "case {case:?}: expected the message {expected_message:?}, got {body}"
        );

        // The refused create changed nothing: the group holding the name keeps the sizes it
        // was created with, not the ones the refused request stated.
        let sizes = reported_sizes(&router, EXISTING_GROUP, case).await;
        assert_eq!(
            (sizes.0.as_str(), sizes.1.as_str(), sizes.2.as_str()),
            (
                EXISTING_MIN_SIZE,
                EXISTING_MAX_SIZE,
                EXISTING_DESIRED_CAPACITY
            ),
            "case {case:?}: the refused create must leave the group's sizes untouched, got \
             {sizes:?}"
        );
    }
}

#[tokio::test]
async fn the_same_launch_source_is_refused_the_same_way_under_a_free_name() {
    for row in ROWS {
        let case = row.case;
        let (router, held) = seeded().await;
        let free_name = format!("{EXISTING_GROUP}-free");
        let mut params = vec![
            p("AutoScalingGroupName", free_name.as_str()),
            p("MinSize", SECOND_MIN_SIZE),
            p("MaxSize", SECOND_MAX_SIZE),
            p("DesiredCapacity", SECOND_DESIRED_CAPACITY),
            p("AvailabilityZones.member.1", "us-west-2a"),
        ];
        params.extend(launch_source(row.source, &held));

        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
        if row.source == Source::HeldTemplate {
            // The launch source that resolves is the one the service accepts: nothing but the
            // taken name made the request above fail, so the refusals the other rows get are
            // about the launch source and not about anything else these requests carry.
            assert_eq!(
                status, 200,
                "case {case:?}: a resolving launch source under a free name must be created, \
                 got {status}: {body}"
            );
            let sizes = reported_sizes(&router, &free_name, case).await;
            assert_eq!(
                (sizes.0.as_str(), sizes.1.as_str(), sizes.2.as_str()),
                (SECOND_MIN_SIZE, SECOND_MAX_SIZE, SECOND_DESIRED_CAPACITY),
                "case {case:?}: the created group must report the sizes the request stated, \
                 got {sizes:?}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: a launch source that resolves to nothing must be refused, got \
             {status}: {body}"
        );
        let (expected_code, expected_message) = expected_refusal(row.source, &held);
        assert_eq!(
            tag_value(&body, "Code"),
            expected_code,
            "case {case:?}: expected the {expected_code} code, got {body}"
        );
        assert_eq!(
            xml_unescape(&tag_value(&body, "Message")),
            expected_message,
            "case {case:?}: expected the message {expected_message:?}, got {body}"
        );
    }
}
