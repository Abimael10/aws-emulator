// SPDX-License-Identifier: CC0-1.0

//! The HOST-READINESS gate in front of every EC2/on-premises (`Server`) deployment, driven
//! the way a client drives it.
//!
//! RULE under test: a running Amazon EC2 instance whose instance profile carries a role
//! Systems Manager's managed-node permission is granted to registers as a managed node, so
//! `ssm:DescribeInstanceInformation` reports one entry for that instance id with `PingStatus`
//! `Online` — whatever network the instance was launched into and whatever ELSE its role is
//! allowed to do. That gate is what a client waits out before it deploys to the host at all
//! (the sibling `Server` suites in this crate all wait on it), so an instance that never
//! appears in that list leaves such a client polling to its own deadline with nothing to
//! deploy to.
//!
//! The estate is the sibling suite's (`server_platform_rollback_read_through_run_command`),
//! which mounts CodeDeploy, Amazon EC2, Amazon S3, Systems Manager, IAM and CloudWatch on one
//! shared engine.
//!
//! Nothing here waits a fixed time: the launch reaching `running` and the node registering are
//! stored due instants, so each step looks again until the condition holds or a generous
//! deadline passes, and nothing asserts on elapsed time.

use serde_json::json;

use crate::server_platform_instance_deployments::element;
use crate::server_platform_rollback_read_through_run_command::{
    trust_policy, when_managed, Estate, AMI, EC2_API_VERSION, EC2_PRINCIPAL, IAM_API_VERSION,
    SSM_CORE_POLICY,
};

/// An inline policy granting the role read on a revision bucket, which is what a role that
/// holds a deployment's revisions carries BESIDE the managed-node permission.
const REVISION_READ_POLICY: &str = "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\
     \"Allow\",\"Action\":[\"s3:GetObject\",\"s3:ListBucket\"],\"Resource\":\"*\"}]}";

/// One row of the table: the network the instance is launched into and what else its role may
/// do.
struct Row {
    /// What the row is about.
    case: &'static str,
    /// Whether the instance is launched into a VPC and subnet of the account's own making
    /// rather than into the ambient default network.
    own_network: bool,
    /// Whether the role carries a policy of its own beside the managed-node permission.
    extra_policy: bool,
}

/// RULE: see the module docs — a running instance with an SSM-capable instance profile is
/// reported as an `Online` managed node.
#[tokio::test]
async fn a_running_instance_with_an_ssm_capable_profile_becomes_an_online_managed_node() {
    // Rows differ along what the rule generalises over: the network the host is launched
    // into, and whether its role is allowed anything beyond registering the node.
    let rows: &[Row] = &[
        Row {
            case: "the ambient default network, the managed-node permission alone",
            own_network: false,
            extra_policy: false,
        },
        Row {
            case: "a VPC and subnet of the account's own",
            own_network: true,
            extra_policy: false,
        },
        Row {
            case: "a VPC and subnet of its own, with revision read on the role too",
            own_network: true,
            extra_policy: true,
        },
        Row {
            case: "the default network, with revision read on the role too",
            own_network: false,
            extra_policy: true,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let estate = Estate::new();
        let role = format!("readiness-role-{index}");
        let profile = format!("readiness-profile-{index}");

        let trust = trust_policy(EC2_PRINCIPAL);
        estate
            .form(
                case,
                "iam",
                "CreateRole",
                IAM_API_VERSION,
                &[
                    ("RoleName", role.as_str()),
                    ("AssumeRolePolicyDocument", &trust[..]),
                ],
            )
            .await;
        estate
            .form(
                case,
                "iam",
                "AttachRolePolicy",
                IAM_API_VERSION,
                &[("RoleName", role.as_str()), ("PolicyArn", SSM_CORE_POLICY)],
            )
            .await;
        if row.extra_policy {
            estate
                .form(
                    case,
                    "iam",
                    "PutRolePolicy",
                    IAM_API_VERSION,
                    &[
                        ("RoleName", role.as_str()),
                        ("PolicyName", "revision-read"),
                        ("PolicyDocument", REVISION_READ_POLICY),
                    ],
                )
                .await;
        }
        estate
            .form(
                case,
                "iam",
                "CreateInstanceProfile",
                IAM_API_VERSION,
                &[("InstanceProfileName", profile.as_str())],
            )
            .await;
        estate
            .form(
                case,
                "iam",
                "AddRoleToInstanceProfile",
                IAM_API_VERSION,
                &[
                    ("InstanceProfileName", profile.as_str()),
                    ("RoleName", role.as_str()),
                ],
            )
            .await;

        let mut params: Vec<(String, String)> = vec![
            ("ImageId".to_string(), AMI.to_string()),
            ("InstanceType".to_string(), "t3.micro".to_string()),
            ("MinCount".to_string(), "1".to_string()),
            ("MaxCount".to_string(), "1".to_string()),
            ("IamInstanceProfile.Name".to_string(), profile.clone()),
        ];
        if row.own_network {
            // The network a client builds for a fleet of its own: a VPC, a subnet in it and
            // an internet gateway attached to it, which is what the host's agent reaches the
            // service over.
            let vpc = estate
                .form(
                    case,
                    "ec2",
                    "CreateVpc",
                    EC2_API_VERSION,
                    &[("CidrBlock", "10.9.0.0/16")],
                )
                .await;
            let vpc_id = element(&vpc, "vpcId")
                .unwrap_or_else(|| panic!("case {case:?}: CreateVpc reports an id: {vpc}"));
            let subnet = estate
                .form(
                    case,
                    "ec2",
                    "CreateSubnet",
                    EC2_API_VERSION,
                    &[("VpcId", vpc_id.as_str()), ("CidrBlock", "10.9.1.0/24")],
                )
                .await;
            let subnet_id = element(&subnet, "subnetId")
                .unwrap_or_else(|| panic!("case {case:?}: CreateSubnet reports an id: {subnet}"));
            let gateway = estate
                .form(case, "ec2", "CreateInternetGateway", EC2_API_VERSION, &[])
                .await;
            let gateway_id = element(&gateway, "internetGatewayId").unwrap_or_else(|| {
                panic!("case {case:?}: CreateInternetGateway reports an id: {gateway}")
            });
            estate
                .form(
                    case,
                    "ec2",
                    "AttachInternetGateway",
                    EC2_API_VERSION,
                    &[
                        ("InternetGatewayId", gateway_id.as_str()),
                        ("VpcId", vpc_id.as_str()),
                    ],
                )
                .await;
            params.push(("SubnetId".to_string(), subnet_id));
        }
        let borrowed: Vec<(&str, &str)> = params
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let launched = estate
            .form(case, "ec2", "RunInstances", EC2_API_VERSION, &borrowed)
            .await;
        let host = element(&launched, "instanceId")
            .unwrap_or_else(|| panic!("case {case:?}: RunInstances reports an id: {launched}"));

        // The gate itself, waited out through the one helper the sibling `Server` suites wait
        // on it with, and then READ: the node Systems Manager reports for the instance that
        // was launched.
        when_managed(&estate, case, &host).await;
        let reported = estate
            .ssm(
                case,
                "DescribeInstanceInformation",
                json!({"Filters": [{"Key": "InstanceIds", "Values": [host]}]}),
            )
            .await;
        let online = reported["InstanceInformationList"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|node| {
                node["InstanceId"].as_str() == Some(host.as_str())
                    && node["PingStatus"].as_str() == Some("Online")
            });
        assert!(
            online,
            "case {case:?}: expected {host} to be reported as an Online managed node, got \
             {reported:?}"
        );
    }
}
