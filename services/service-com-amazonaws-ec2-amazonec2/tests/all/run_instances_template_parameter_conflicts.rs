// SPDX-License-Identifier: CC0-1.0

//! That a LAUNCH NAMES ITS SUBNET, ITS SECURITY GROUPS AND ITS PRIMARY PRIVATE ADDRESS EITHER
//! ON ITS OWN MEMBERS OR ON ITS NETWORK INTERFACES AND NEVER BOTH, driven over `ec2Query`
//! (README.md / OPERATIONS.md "Wire protocol").
//!
//! "If you specify a network interface, you must specify any subnets / security groups as part
//! of the network interface instead of using this parameter"
//! (`RunInstancesRequest$SubnetId`, `$SecurityGroupIds`, `$SecurityGroups`), and "you can't
//! specify this option if you've specified the option to designate a private IP address as the
//! primary IP address in a network interface specification" (`$PrivateIpAddress`) — so a
//! launch that names each fact twice is refused with `InvalidParameterCombination` rather than
//! served from one of the two places.
//!
//! A LAUNCH TEMPLATE expresses those same facts, so the rule is about the launch as RESOLVED:
//! the rows below put the interfaces and the instance-level member on either side of a
//! template-driven launch — interfaces from the template and the member on the request, both
//! halves inside the template, both halves on the request — and all of them are refused the
//! same way, while a launch that names only one of the two is served.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, error_message};
use super::wire::{param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The image every launch here uses; `RunInstances` only needs one that resolves.
const AMI: &str = "ami-0123456789abcdef0";

/// The sentence EC2 refuses a launch that names its security groups on both sides with,
/// whichever of the two instance-level members named them.
const SECURITY_GROUPS_SENTENCE: &str =
    "Network interfaces and an instance-level security groups may not be specified on the \
     same request";

/// One `action` call carrying `params`, as its HTTP status and XML body.
async fn call(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// One `action` call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One element of a response body, or a panic naming the case.
fn member(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// The fixtures every test here drives: a VPC with a subnet and a security group of its own.
struct Fixtures {
    router: MultiServiceRouter,
    subnet_id: String,
    group_id: String,
    group_name: String,
}

/// A VPC with one subnet and one security group.
async fn fixtures(case: &str) -> Fixtures {
    let (router, _engine) = super::ec2_query::emulator();
    let created = ok(
        case,
        &router,
        "CreateVpc",
        &[param("CidrBlock", "10.210.0.0/16")],
    )
    .await;
    let vpc_id = member(case, &created, "vpcId");
    let subnet = ok(
        case,
        &router,
        "CreateSubnet",
        &[param("VpcId", &vpc_id), param("CidrBlock", "10.210.1.0/24")],
    )
    .await;
    let subnet_id = member(case, &subnet, "subnetId");
    let group_name = "conflicts-a".to_string();
    let group = ok(
        case,
        &router,
        "CreateSecurityGroup",
        &[
            param("VpcId", &vpc_id),
            param("GroupName", &group_name),
            param("GroupDescription", "the group the rows name"),
        ],
    )
    .await;
    let group_id = member(case, &group, "groupId");
    Fixtures {
        router,
        subnet_id,
        group_id,
        group_name,
    }
}

/// Create a launch template version carrying `data`, as its id. `data` members are spelled as
/// ec2Query spells them: `LaunchTemplateData.<member>`.
async fn template(
    case: &str,
    router: &MultiServiceRouter,
    name: &str,
    data: &[(String, String)],
) -> String {
    let mut params = vec![param("LaunchTemplateName", name)];
    params.extend(data.iter().cloned());
    let body = ok(case, router, "CreateLaunchTemplate", &params).await;
    member(case, &body, "launchTemplateId")
}

/// One `LaunchTemplateData.<member>` field.
fn data(name: &str, value: &str) -> Param {
    param(&format!("LaunchTemplateData.{name}"), value)
}

/// THE RULE: whichever side of a template-driven launch names the interfaces and whichever
/// names the instance-level fact, the launch is refused — with the sentence EC2 words for the
/// member that named it.
#[tokio::test]
async fn a_launch_that_names_one_fact_on_both_sides_is_refused() {
    let fixtures = fixtures("the fixtures").await;
    // The interface the template names, which every row below launches with.
    let template_interface = |subnet_id: &str| -> Vec<Param> {
        vec![
            data("ImageId", AMI),
            data("NetworkInterface.1.DeviceIndex", "0"),
            data("NetworkInterface.1.SubnetId", subnet_id),
        ]
    };

    // (case, the template's data, the request's own members beside the template, the sentence)
    let rows: Vec<(&str, Vec<Param>, Vec<Param>, String)> = vec![
        (
            "the template's interface beside the request's subnet",
            template_interface(&fixtures.subnet_id),
            vec![param("SubnetId", &fixtures.subnet_id)],
            "Network interfaces and an instance-level subnet ID may not be specified on the \
             same request"
                .to_string(),
        ),
        (
            "the template's interface beside the request's security group id",
            template_interface(&fixtures.subnet_id),
            vec![param("SecurityGroupId.1", &fixtures.group_id)],
            SECURITY_GROUPS_SENTENCE.to_string(),
        ),
        (
            "the template's interface beside the request's security group name",
            template_interface(&fixtures.subnet_id),
            vec![param("SecurityGroup.1", &fixtures.group_name)],
            SECURITY_GROUPS_SENTENCE.to_string(),
        ),
        (
            "the template's interface beside the request's private address",
            template_interface(&fixtures.subnet_id),
            vec![param("PrivateIpAddress", "10.210.1.11")],
            "Network interfaces and an instance-level private IP address may not be \
             specified on the same request"
                .to_string(),
        ),
        (
            "both halves inside the template",
            {
                let mut carried = template_interface(&fixtures.subnet_id);
                carried.push(data("SecurityGroupId.1", &fixtures.group_id));
                carried
            },
            Vec::new(),
            SECURITY_GROUPS_SENTENCE.to_string(),
        ),
        (
            "the template's security group beside the request's own interface",
            vec![
                data("ImageId", AMI),
                data("SecurityGroupId.1", &fixtures.group_id),
            ],
            vec![
                param("NetworkInterface.1.DeviceIndex", "0"),
                param("NetworkInterface.1.SubnetId", &fixtures.subnet_id),
            ],
            SECURITY_GROUPS_SENTENCE.to_string(),
        ),
    ];

    let before = described_instances(&fixtures.router).await;
    for (index, (case, carried, request, sentence)) in rows.into_iter().enumerate() {
        let launch_template_id = template(
            case,
            &fixtures.router,
            &format!("conflicts-{index}"),
            &carried,
        )
        .await;
        let mut params = vec![
            param("MinCount", "1"),
            param("MaxCount", "1"),
            param("LaunchTemplate.LaunchTemplateId", &launch_template_id),
        ];
        params.extend(request);
        let (status, body) = call(&fixtures.router, "RunInstances", &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body).as_str(),
            "InvalidParameterCombination",
            "case {case:?}: expected InvalidParameterCombination, got {body}"
        );
        assert_eq!(
            error_message(&body),
            sentence,
            "case {case:?}: expected the sentence EC2 words for the member, got {body}"
        );
    }
    let after = described_instances(&fixtures.router).await;
    assert_eq!(
        after, before,
        "the refused launches must have launched nothing"
    );
}

/// The same conflicts spelled out on the REQUEST alone are refused the same way: one rule,
/// whichever side each half came from.
#[tokio::test]
async fn a_request_that_names_one_fact_on_both_sides_is_refused() {
    let fixtures = fixtures("the fixtures").await;
    let interface = vec![
        param("NetworkInterface.1.DeviceIndex", "0"),
        param("NetworkInterface.1.SubnetId", &fixtures.subnet_id),
    ];

    // (case, the instance-level member, the sentence)
    let rows: Vec<(&str, Param, String)> = vec![
        (
            "an instance-level subnet",
            param("SubnetId", &fixtures.subnet_id),
            "Network interfaces and an instance-level subnet ID may not be specified on the \
             same request"
                .to_string(),
        ),
        (
            "an instance-level security group id",
            param("SecurityGroupId.1", &fixtures.group_id),
            SECURITY_GROUPS_SENTENCE.to_string(),
        ),
        (
            "an instance-level security group name",
            param("SecurityGroup.1", &fixtures.group_name),
            SECURITY_GROUPS_SENTENCE.to_string(),
        ),
    ];

    for (case, instance_level, sentence) in rows {
        let mut params = vec![
            param("ImageId", AMI),
            param("MinCount", "1"),
            param("MaxCount", "1"),
        ];
        params.extend(interface.clone());
        params.push(instance_level);
        let (status, body) = call(&fixtures.router, "RunInstances", &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body).as_str(),
            "InvalidParameterCombination",
            "case {case:?}: expected InvalidParameterCombination, got {body}"
        );
        assert_eq!(
            error_message(&body),
            sentence,
            "case {case:?}: expected the sentence EC2 words for the member, got {body}"
        );
    }
}

/// And a launch that names each fact ONCE is served: the rule refuses the conflict, not the
/// members — a template whose interfaces carry the subnet and the groups launches, and so does
/// one that names them at instance level with no interfaces at all.
#[tokio::test]
async fn a_launch_that_names_each_fact_once_is_served() {
    let fixtures = fixtures("the fixtures").await;

    let case = "a template whose interface carries the subnet and the group";
    let launch_template_id = template(
        case,
        &fixtures.router,
        "served-interfaces",
        &[
            data("ImageId", AMI),
            data("NetworkInterface.1.DeviceIndex", "0"),
            data("NetworkInterface.1.SubnetId", &fixtures.subnet_id),
            data("NetworkInterface.1.SecurityGroupId.1", &fixtures.group_id),
        ],
    )
    .await;
    let body = ok(
        case,
        &fixtures.router,
        "RunInstances",
        &[
            param("MinCount", "1"),
            param("MaxCount", "1"),
            param("LaunchTemplate.LaunchTemplateId", &launch_template_id),
        ],
    )
    .await;
    assert_eq!(
        element(&body, "subnetId").as_deref(),
        Some(fixtures.subnet_id.as_str()),
        "case {case:?}: expected the instance in the template interface's subnet, got {body}"
    );
    assert!(
        elements(&body, "groupId").contains(&fixtures.group_id),
        "case {case:?}: expected the template interface's group, got {body}"
    );

    let case = "a template that names neither, with the request naming both at instance level";
    let launch_template_id = template(
        case,
        &fixtures.router,
        "served-instance-level",
        &[data("ImageId", AMI)],
    )
    .await;
    let body = ok(
        case,
        &fixtures.router,
        "RunInstances",
        &[
            param("MinCount", "1"),
            param("MaxCount", "1"),
            param("LaunchTemplate.LaunchTemplateId", &launch_template_id),
            param("SubnetId", &fixtures.subnet_id),
            param("SecurityGroupId.1", &fixtures.group_id),
        ],
    )
    .await;
    assert_eq!(
        element(&body, "subnetId").as_deref(),
        Some(fixtures.subnet_id.as_str()),
        "case {case:?}: expected the instance in the request's own subnet, got {body}"
    );
    assert!(
        elements(&body, "groupId").contains(&fixtures.group_id),
        "case {case:?}: expected the request's own group, got {body}"
    );
}

/// Every instance id the account holds, as `DescribeInstances` reports them.
async fn described_instances(router: &MultiServiceRouter) -> Vec<String> {
    let body = ok(
        "the instances the account holds",
        router,
        "DescribeInstances",
        &[],
    )
    .await;
    elements(&body, "instanceId")
}
