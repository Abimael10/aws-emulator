// SPDX-License-Identifier: CC0-1.0

//! The validation WARNINGS a launch template's parameters earn.
//!
//! The rule under test: "if the launch template contains parameters or parameter
//! combinations that are not valid, an error code and an error message are returned for each
//! issue that's found" (`CreateLaunchTemplateResult$Warning`, and the same sentence on
//! `CreateLaunchTemplateVersionResult$Warning`). A launch template is a set of launch
//! PARAMETERS, not a launch, so such a parameter is STORED and reported back as a finding on
//! a 200 — the launch that tries to use it is what refuses it — and a template with nothing
//! to report carries no `warning` element at all.
//!
//! Two parameters are findings here:
//!
//! * a template that specifies a network interface but does not carry its security groups
//!   inside it, because "if you specify a network interface, you must specify any security
//!   groups as part of the network interface instead of using this parameter"
//!   (`RequestLaunchTemplateData$SecurityGroupIds`);
//! * a template naming an IAM instance profile the account holds no record of, which is a
//!   question the component that OWNS instance profiles answers — so the suite creates the
//!   profile through IAM and watches the finding disappear, rather than asserting against
//!   this crate's own copy of anything.
//!
//! Both creates answer from one reading, so the table is driven through
//! `CreateLaunchTemplate` and the findings are then read back once more from
//! `CreateLaunchTemplateVersion` for the same parameters.

use std::sync::Arc;

use service_com_amazonaws_ec2_amazonec2 as ec2;
use service_com_amazonaws_iam_awsidentitymanagementv20100508 as iam;
use smithy_server_runtime::MultiServiceRouter;

use ec2::state::{
    INVALID_PARAMETER_COMBINATION, SECURITY_GROUPS_BELONG_TO_THE_NETWORK_INTERFACE,
    VALIDATION_WARNING_IAM_INSTANCE_PROFILE_NOT_FOUND,
};

use super::ec2_query::{call, unescaped};
use super::wire::{child, child_text, document, set_items};

/// The Region every call of this suite is signed for.
const REGION: &str = "us-west-2";

/// The IAM API version a client sends in an awsQuery body.
const IAM_API_VERSION: &str = "2010-05-08";

/// The access key id the IAM call here is presented under: the account's own published
/// credential, the identity that provisions the profile.
const ACCOUNT_KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// An AMI id of the right FORM for the launch parameters every row carries: a template
/// stores its parameters without resolving them, so which image it names is not the subject.
const SOME_AMI: &str = "ami-0123456789abcdef0";

/// A security group id of the right form, for the rows that name one: a template stores the
/// reference, so whether the account holds the group is not the subject either.
const SOME_GROUP: &str = "sg-0123456789abcdef0";

/// One finding, as a row asserts it: the code and the message.
type Finding = (String, String);

/// One row of the warning table: the case, the launch parameters it sends, and the findings
/// they earn.
type WarningRow = (&'static str, Vec<(String, String)>, Vec<Finding>);

fn p(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// The findings a create's body reports, in the order it reported them — empty for a body
/// that carries no `warning` element at all.
fn findings(case: &str, body: &str) -> Vec<Finding> {
    let parsed = document(case, body);
    let Some(warning) = parsed
        .root_element()
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "warning")
    else {
        return Vec::new();
    };
    set_items(warning, "errorSet")
        .into_iter()
        .map(|item| {
            (
                child_text(item, "code").unwrap_or_default(),
                unescaped(&child_text(item, "message").unwrap_or_default()),
            )
        })
        .collect()
}

/// One create that must be served, as its body.
async fn created(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(String, String)],
) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: a launch template's parameters are stored and reported on, so \
         {action} answers 200, got {status} with body {body}"
    );
    body
}

/// The `LaunchTemplateData.*` fields of a template that specifies a network interface, with
/// `groups` as the groups named INSIDE that interface.
fn with_network_interface(groups: &[&str]) -> Vec<(String, String)> {
    let mut params = vec![
        p("LaunchTemplateData.ImageId", SOME_AMI),
        p("LaunchTemplateData.NetworkInterface.1.DeviceIndex", "0"),
    ];
    for (position, group) in groups.iter().enumerate() {
        params.push(p(
            &format!("LaunchTemplateData.NetworkInterface.1.SecurityGroupId.{}", position + 1),
            group,
        ));
    }
    params
}

/// The sentence a finding about an absent instance profile carries, for a reference on
/// `member` naming `reference` — the one wording the service words it with, read off the
/// crate so the row is not the specification.
fn absent_profile(member: &str, reference: &str) -> String {
    format!("The IamInstanceProfile.{member} '{reference}' does not exist.")
}

/// The whole table: each row is a set of launch parameters and the findings it earns.
#[tokio::test]
async fn a_launch_template_reports_a_finding_per_parameter_no_launch_could_use() {
    let combination = (
        INVALID_PARAMETER_COMBINATION.to_string(),
        SECURITY_GROUPS_BELONG_TO_THE_NETWORK_INTERFACE.to_string(),
    );
    let not_found = |member: &str, reference: &str| -> Finding {
        (
            VALIDATION_WARNING_IAM_INSTANCE_PROFILE_NOT_FOUND.to_string(),
            absent_profile(member, reference),
        )
    };
    let profile_arn = format!(
        "arn:aws:iam::{}:instance-profile/absent-profile",
        ::engine::emulator_account_id()
    );

    // (case, the launch parameters, the findings they earn)
    let rows: Vec<WarningRow> = vec![
        (
            "no network interface and no profile: nothing to report",
            vec![
                p("LaunchTemplateData.ImageId", SOME_AMI),
                p("LaunchTemplateData.InstanceType", "t3.micro"),
            ],
            Vec::new(),
        ),
        (
            "security groups named on the template's own member, with no interface",
            vec![
                p("LaunchTemplateData.ImageId", SOME_AMI),
                p("LaunchTemplateData.SecurityGroupId.1", SOME_GROUP),
            ],
            Vec::new(),
        ),
        (
            "an interface carrying its own groups",
            with_network_interface(&[SOME_GROUP]),
            Vec::new(),
        ),
        (
            "an interface carrying several groups of its own",
            with_network_interface(&[SOME_GROUP, "sg-0fedcba9876543210"]),
            Vec::new(),
        ),
        (
            "an interface carrying no group at all: a launch gives it the VPC's default",
            with_network_interface(&[]),
            Vec::new(),
        ),
        (
            "an interface beside groups named on the template's own member",
            [
                with_network_interface(&[SOME_GROUP]),
                vec![p("LaunchTemplateData.SecurityGroupId.1", SOME_GROUP)],
            ]
            .concat(),
            vec![combination.clone()],
        ),
        (
            "an interface beside groups named by NAME on the template's own member",
            [
                with_network_interface(&[SOME_GROUP]),
                vec![p("LaunchTemplateData.SecurityGroup.1", "web")],
            ]
            .concat(),
            vec![combination.clone()],
        ),
        (
            "an instance profile named by name that the account does not hold",
            vec![
                p("LaunchTemplateData.ImageId", SOME_AMI),
                p("LaunchTemplateData.IamInstanceProfile.Name", "absent-profile"),
            ],
            vec![not_found("Name", "absent-profile")],
        ),
        (
            "an instance profile named by ARN that the account does not hold",
            vec![
                p("LaunchTemplateData.ImageId", SOME_AMI),
                p("LaunchTemplateData.IamInstanceProfile.Arn", &profile_arn),
            ],
            vec![not_found("Arn", &profile_arn)],
        ),
        (
            "both findings at once, in the order the parameters are read",
            [
                with_network_interface(&[SOME_GROUP]),
                vec![
                    p("LaunchTemplateData.SecurityGroupId.1", SOME_GROUP),
                    p(
                        "LaunchTemplateData.IamInstanceProfile.Name",
                        "absent-profile",
                    ),
                ],
            ]
            .concat(),
            vec![combination.clone(), not_found("Name", "absent-profile")],
        ),
    ];

    for (case, data, expected) in rows {
        let (router, _engine) = super::ec2_query::emulator();
        let mut params = vec![p("LaunchTemplateName", "probe")];
        params.extend(data.clone());
        let body = created(&router, case, "CreateLaunchTemplate", &params).await;
        let reported = findings(case, &body);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the findings {expected:?}, got {reported:?} in {body}"
        );
        // The template is stored whatever it was told: a finding is advisory.
        let parsed = document(case, &body);
        assert!(
            parsed
                .descendants()
                .any(|node| node.is_element() && child(node, "launchTemplateId").is_some()),
            "case {case:?}: the template is created even when its parameters earn a \
             finding, got {body}"
        );

        // A new VERSION of the same template answers from the same reading, so the same
        // parameters earn the same findings there.
        let mut version = vec![p("LaunchTemplateName", "probe")];
        version.extend(data);
        let body = created(&router, case, "CreateLaunchTemplateVersion", &version).await;
        let reported = findings(case, &body);
        assert_eq!(
            reported, expected,
            "case {case:?}: a new version of the same parameters earns the same findings, \
             got {reported:?} in {body}"
        );
    }
}

/// The IAM finding is about a resource ANOTHER component owns: once the account's IAM holds
/// the profile the template names, there is nothing to report about it.
#[tokio::test]
async fn an_instance_profile_the_account_holds_earns_no_finding() {
    let case = "a profile IAM holds";
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(ec2::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(iam::EmulatedService::new(Arc::clone(&engine)));

    let profile = "launch-profile";
    let (status, body) = super::ec2_query::query_call_as(
        &router,
        REGION,
        "iam",
        IAM_API_VERSION,
        ACCOUNT_KEY_ID,
        "CreateInstanceProfile",
        &[("InstanceProfileName", profile)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the profile has to be created through the service that owns it, \
         got {status} with body {body}"
    );

    let params = vec![
        p("LaunchTemplateName", "probe"),
        p("LaunchTemplateData.ImageId", SOME_AMI),
        p("LaunchTemplateData.IamInstanceProfile.Name", profile),
    ];
    let body = created(&router, case, "CreateLaunchTemplate", &params).await;
    assert_eq!(
        findings(case, &body),
        Vec::<Finding>::new(),
        "case {case:?}: a profile the account holds is a reference a launch can use, so the \
         template earns no finding, got {body}"
    );
}
