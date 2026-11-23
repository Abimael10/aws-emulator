// SPDX-License-Identifier: CC0-1.0

//! The SSM-managed public-subnet host estate a template declares, and the LAUNCH OPTIONS the
//! instance of such a template states.
//!
//! RULE: a property `AWS::EC2::Instance` publishes that is a member of `RunInstances` — the
//! call that creates the resource — travels to that call as that member, whatever the
//! property's shape: a structure as the member's own members and a list of structures as the
//! parameter name `ec2Query` flattens the list under. So the launch Amazon EC2 performs is the
//! one the declaration asked for, and a declaration that states such a property is deployed (or
//! refused by Amazon EC2 itself) rather than failed by CloudFormation for a property it does not
//! apply.
//!
//! Why it matters: a resource failed over a property CloudFormation declines to apply takes the
//! whole stack to `ROLLBACK_COMPLETE`, so every resource the template declared is rolled back
//! and a client that deployed the most ordinary hardened host — an encrypted root volume, IMDSv2
//! required — gets no estate at all and no refusal on any one API call to act on.
//!
//! The rows of [`every_structured_launch_property_reaches_amazon_ec2`] differ along what the
//! rule generalises over: the property's SHAPE (a structure, a list of structures), the TYPE of
//! the members inside it (an enum string, an integer, a boolean, an ARN, a device name), the
//! DEPTH of the nesting (a block device mapping's `Ebs` is a structure inside a list entry), and
//! whether the call spells the member as the type does (`CreditSpecification`'s is `CPUCredits`
//! on the type and `CpuCredits` on the call). The estate the issue is about is one row's worth of
//! that class, deployed whole in [`the_ssm_managed_host_estate_deploys`].

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, create_stack, xml_blocks};
use crate::stack_lifecycle::{
    acknowledging_iam, ec2, is_service_assigned_id, member, output_value, router_with_engine,
    settled_stack, xml_text, xml_texts,
};

/// The Availability Zone the estate's subnet is created in: the Region's first, which offers
/// every instance type the catalog publishes.
const ZONE: &str = "us-east-1a";

/// The public Systems Manager parameter the templates resolve their AMI from — a name in AWS's
/// own `/aws/service/` hierarchy, which the published catalog answers for, so no template here
/// names an image id this suite invented.
const IMAGE_PARAMETER: &str =
    "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64";

/// The refusal wording CloudFormation fails a resource with for a property it does not apply —
/// what no row of this suite may be answered with, since every property it declares is one
/// `RunInstances` has a member for.
const UNAPPLIED: &str = "does not apply";

/// The status a stack that deployed reports.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The reason of every event a stack recorded a FAILURE in, which is where a refused resource's
/// account of itself is.
async fn failure_reasons(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (status, events) = call(
        router,
        "DescribeStackEvents",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeStackEvents({stack}) must answer 200, got {status} {events}"
    );
    xml_blocks(&events, "member")
        .into_iter()
        .filter(|event| {
            xml_texts(event, "ResourceStatus")
                .first()
                .is_some_and(|reported| reported.ends_with("_FAILED"))
        })
        .filter_map(|event| xml_texts(&event, "ResourceStatusReason").into_iter().next())
        .collect()
}

/// The member `member` of the first `block` structure an `ec2Query` answer reports — how one
/// option of a described instance is read out of the structure Amazon EC2 nests it in.
fn reported_member(body: &str, block: &str, member: &str) -> Option<String> {
    xml_blocks(body, block)
        .first()
        .and_then(|found| xml_text(found, member))
}

/// One row of the launch-option table: the property it declares and what Amazon EC2 must report
/// back for it.
struct Row {
    case: &'static str,
    /// The stack the row is deployed as, so a failure names which row's stack to read.
    stack: &'static str,
    /// The instance properties this row declares, beside the three every row carries.
    declared: Value,
    /// What the described instance must report, as (the structure Amazon EC2 nests it in, the
    /// member of it, the value the declaration asked for).
    reported: &'static [(&'static str, &'static str, &'static str)],
    /// Whether Amazon EC2 serves a launch carrying this property at all. A row it does not
    /// serve is REFUSED by that service — which is the resource's failure — and only the rule
    /// itself is asserted for it: the property reached the call.
    served: bool,
}

/// The template of one row: the smallest network an instance can be launched into, plus the
/// instance the row declares.
fn row_template(declared: &Value) -> String {
    let mut instance = json!({
        "ImageId": { "Ref": "InstanceAmi" },
        "InstanceType": "t3.micro",
        "SubnetId": { "Ref": "Subnet" },
    });
    if let Value::Object(extra) = declared {
        for (property, value) in extra {
            instance[property] = value.clone();
        }
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Parameters": {
            "InstanceAmi": {
                "Type": "AWS::SSM::Parameter::Value<AWS::EC2::Image::Id>",
                "Default": IMAGE_PARAMETER,
            }
        },
        "Resources": {
            "Vpc": { "Type": "AWS::EC2::VPC", "Properties": { "CidrBlock": "10.30.0.0/16" } },
            "Subnet": {
                "Type": "AWS::EC2::Subnet",
                "Properties": {
                    "VpcId": { "Ref": "Vpc" },
                    "CidrBlock": "10.30.1.0/24",
                    "AvailabilityZone": ZONE,
                }
            },
            "Host": { "Type": "AWS::EC2::Instance", "Properties": instance }
        },
        "Outputs": { "InstanceId": { "Value": { "Ref": "Host" } } }
    })
    .to_string()
}

/// Every structured property of the type that is a member of `RunInstances`, one row each.
///
/// Source: AWS's CloudFormation resource reference for `AWS::EC2::Instance` read against
/// `smithy_query(query="operation",operation="RunInstances")` — the six properties that are
/// STRUCTURES of that call and the five that are LISTS OF STRUCTURES. The values each row asks
/// for are chosen AWAY from what a launch that states nothing gets, so the reported value is
/// evidence the declaration reached the call rather than the service's own default.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the metadata options of an IMDSv2 host",
            stack: "launch-metadata-options",
            declared: json!({ "MetadataOptions": {
                "HttpTokens": "required",
                "HttpEndpoint": "enabled",
                "HttpProtocolIpv6": "disabled",
                "HttpPutResponseHopLimit": 3,
                "InstanceMetadataTags": "enabled",
            } }),
            reported: &[
                ("metadataOptions", "httpTokens", "required"),
                ("metadataOptions", "httpEndpoint", "enabled"),
                ("metadataOptions", "httpProtocolIpv6", "disabled"),
                ("metadataOptions", "httpPutResponseHopLimit", "3"),
                ("metadataOptions", "instanceMetadataTags", "enabled"),
            ],
            served: true,
        },
        Row {
            case: "the CPU options of a single-threaded host",
            stack: "launch-cpu-options",
            declared: json!({ "CpuOptions": { "CoreCount": 1, "ThreadsPerCore": 1 } }),
            reported: &[
                ("cpuOptions", "coreCount", "1"),
                ("cpuOptions", "threadsPerCore", "1"),
            ],
            served: true,
        },
        Row {
            // The one property the call spells differently: `CPUCredits` on the type is
            // `CreditSpecificationRequest$CpuCredits` on the call.
            case: "a credit specification, whose member the call renames",
            stack: "launch-credit-specification",
            declared: json!({ "CreditSpecification": { "CPUCredits": "standard" } }),
            reported: &[("item", "cpuCredits", "standard")],
            served: true,
        },
        Row {
            case: "enclave options asking for Nitro Enclaves",
            stack: "launch-enclave-options",
            declared: json!({ "EnclaveOptions": { "Enabled": true } }),
            reported: &[("enclaveOptions", "enabled", "true")],
            served: true,
        },
        Row {
            // Two properties of one declaration, the second nested two deep: hibernation is
            // admitted only over an encrypted root volume, so the mapping's `Ebs` structure has
            // to have reached the call for the launch to be served at all.
            case: "hibernation over the encrypted root volume a mapping declares",
            stack: "launch-hibernation-options",
            declared: json!({
                "HibernationOptions": { "Configured": true },
                "BlockDeviceMappings": [{
                    "DeviceName": "/dev/xvda",
                    "Ebs": { "VolumeSize": 20, "VolumeType": "gp3", "Encrypted": true },
                }],
            }),
            reported: &[("hibernationOptions", "configured", "true")],
            served: true,
        },
        Row {
            case: "a block device mapping keeping its volume past termination",
            stack: "launch-block-device-mappings",
            declared: json!({ "BlockDeviceMappings": [{
                "DeviceName": "/dev/xvda",
                "Ebs": { "VolumeSize": 20, "VolumeType": "gp3", "DeleteOnTermination": false },
            }] }),
            reported: &[
                ("blockDeviceMapping", "deviceName", "/dev/xvda"),
                ("ebs", "deleteOnTermination", "false"),
            ],
            served: true,
        },
        Row {
            case: "private DNS name options naming the host by resource name",
            stack: "launch-private-dns-options",
            declared: json!({ "PrivateDnsNameOptions": {
                "HostnameType": "resource-name",
                "EnableResourceNameDnsARecord": true,
                "EnableResourceNameDnsAAAARecord": false,
            } }),
            reported: &[
                ("privateDnsNameOptions", "hostnameType", "resource-name"),
                ("privateDnsNameOptions", "enableResourceNameDnsARecord", "true"),
                (
                    "privateDnsNameOptions",
                    "enableResourceNameDnsAAAARecord",
                    "false",
                ),
            ],
            served: true,
        },
        Row {
            case: "a license specification, a list of one ARN-carrying structure",
            stack: "launch-license-specifications",
            declared: json!({ "LicenseSpecifications": [{
                "LicenseConfigurationArn":
                    "arn:aws:license-manager:us-east-1:000000000000:license-configuration:lic-0123",
            }] }),
            reported: &[],
            served: true,
        },
        Row {
            // The list member that carries no `xmlName`, so the parameter is the member's own
            // name on the call (`ElasticGpuSpecification`).
            case: "an elastic graphics specification",
            stack: "launch-elastic-gpu",
            declared: json!({ "ElasticGpuSpecifications": [{ "Type": "eg1.medium" }] }),
            reported: &[],
            served: true,
        },
        Row {
            case: "an elastic inference accelerator, whose entry carries a count",
            stack: "launch-elastic-inference",
            declared: json!({
                "ElasticInferenceAccelerators": [{ "Type": "eia2.medium", "Count": 1 }]
            }),
            reported: &[],
            served: true,
        },
        Row {
            // Amazon EC2 does not serve a launch that names IPv6 addresses, so this row's
            // resource fails with THAT service's refusal. Only the rule is asserted for it: the
            // property reached the call, instead of CloudFormation declining to apply it.
            case: "an IPv6 address list, which the owning service refuses",
            stack: "launch-ipv6-addresses",
            declared: json!({ "Ipv6Addresses": [{ "Ipv6Address": "2600:1f18:1::1" }] }),
            reported: &[],
            served: false,
        },
    ]
}

#[tokio::test]
async fn every_structured_launch_property_reaches_amazon_ec2() {
    for row in rows() {
        let case = row.case;
        let (router, _engine) = router_with_engine();
        let (status, created) =
            create_stack(&router, row.stack, &row_template(&row.declared), &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateStack must answer 200 — the stack reports the outcome — got \
             {status} {created}"
        );
        let described = settled_stack(&router, case, row.stack).await;
        let settled = xml_text(&described, "StackStatus").unwrap_or_default();
        let reasons = failure_reasons(&router, row.stack).await;

        // The rule itself, asserted for every row: the property reached `RunInstances`, so no
        // resource failed over a property CloudFormation declines to apply.
        for reason in &reasons {
            assert!(
                !reason.contains(UNAPPLIED),
                "case {case:?}: expected the declared property to reach RunInstances, got the \
                 refusal {reason:?}"
            );
        }
        if !row.served {
            continue;
        }

        assert_eq!(
            settled, CREATE_COMPLETE,
            "case {case:?}: expected {CREATE_COMPLETE}, got {settled:?} with the failures \
             {reasons:?}"
        );
        let instance = output_value(&described, "InstanceId").unwrap_or_default();
        assert!(
            is_service_assigned_id(&instance, "i"),
            "case {case:?}: expected the instance id Amazon EC2 assigned, got {instance:?}"
        );
        let (status, body) = ec2(
            &router,
            "DescribeInstances",
            &[("InstanceId.1", instance.as_str())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeInstances({instance}) must answer 200, got {status} {body}"
        );
        // The credit option is reported by its own read rather than as a member of the
        // instance ("DescribeInstanceCreditSpecifications"), so the row that asks about it is
        // answered from there.
        let (status, credits) = ec2(
            &router,
            "DescribeInstanceCreditSpecifications",
            &[("InstanceId.1", instance.as_str())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeInstanceCreditSpecifications({instance}) must answer 200, \
             got {status} {credits}"
        );
        for (block, element, expected) in row.reported {
            let answered = reported_member(&body, block, element)
                .or_else(|| reported_member(&credits, block, element));
            assert_eq!(
                answered.as_deref(),
                Some(*expected),
                "case {case:?}: expected Amazon EC2 to report {block}.{element} as {expected:?}, \
                 got {answered:?} in {body} {credits}"
            );
        }
    }
}

/// The estate the issue is about: an SSM-managed host on a public subnet, with the network, the
/// bucket, the role and the instance profile a template declares around it, and an instance
/// whose declaration carries the hardening properties such a template carries.
fn estate_template() -> String {
    let tags = json!([{ "Key": "Name", "Value": "ssm-managed-host" }]);
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Parameters": {
            "AssetsBucketName": { "Type": "String" },
            "InstanceAz": { "Type": "String", "Default": ZONE },
            "InstanceAmi": {
                "Type": "AWS::SSM::Parameter::Value<AWS::EC2::Image::Id>",
                "Default": IMAGE_PARAMETER,
            }
        },
        "Resources": {
            "Vpc": {
                "Type": "AWS::EC2::VPC",
                "Properties": {
                    "CidrBlock": "10.20.0.0/16",
                    "EnableDnsSupport": true,
                    "EnableDnsHostnames": true,
                    "Tags": tags,
                }
            },
            "Igw": { "Type": "AWS::EC2::InternetGateway", "Properties": { "Tags": tags } },
            "IgwAttachment": {
                "Type": "AWS::EC2::VPCGatewayAttachment",
                "Properties": {
                    "VpcId": { "Ref": "Vpc" },
                    "InternetGatewayId": { "Ref": "Igw" },
                }
            },
            "PublicSubnet": {
                "Type": "AWS::EC2::Subnet",
                "Properties": {
                    "VpcId": { "Ref": "Vpc" },
                    "CidrBlock": "10.20.1.0/24",
                    "AvailabilityZone": { "Ref": "InstanceAz" },
                    "MapPublicIpOnLaunch": true,
                    "Tags": tags,
                }
            },
            "PublicRouteTable": {
                "Type": "AWS::EC2::RouteTable",
                "Properties": { "VpcId": { "Ref": "Vpc" }, "Tags": tags }
            },
            "DefaultRoute": {
                "Type": "AWS::EC2::Route",
                "DependsOn": "IgwAttachment",
                "Properties": {
                    "RouteTableId": { "Ref": "PublicRouteTable" },
                    "DestinationCidrBlock": "0.0.0.0/0",
                    "GatewayId": { "Ref": "Igw" },
                }
            },
            "SubnetAssociation": {
                "Type": "AWS::EC2::SubnetRouteTableAssociation",
                "Properties": {
                    "SubnetId": { "Ref": "PublicSubnet" },
                    "RouteTableId": { "Ref": "PublicRouteTable" },
                }
            },
            "SshGroup": {
                "Type": "AWS::EC2::SecurityGroup",
                "Properties": {
                    "GroupDescription": "ssh to the managed host",
                    "VpcId": { "Ref": "Vpc" },
                    "SecurityGroupIngress": [{
                        "IpProtocol": "tcp",
                        "FromPort": 22,
                        "ToPort": 22,
                        "CidrIp": "0.0.0.0/0",
                        "Description": "ssh",
                    }],
                }
            },
            "HostGroup": {
                "Type": "AWS::EC2::SecurityGroup",
                "Properties": {
                    "GroupDescription": "the managed host itself",
                    "VpcId": { "Ref": "Vpc" },
                    "SecurityGroupEgress": [{
                        "IpProtocol": "-1",
                        "CidrIp": "0.0.0.0/0",
                        "Description": "everything out",
                    }],
                }
            },
            "AssetsBucket": {
                "Type": "AWS::S3::Bucket",
                "Properties": {
                    "BucketName": { "Ref": "AssetsBucketName" },
                    "PublicAccessBlockConfiguration": {
                        "BlockPublicAcls": true,
                        "BlockPublicPolicy": true,
                        "IgnorePublicAcls": true,
                        "RestrictPublicBuckets": true,
                    },
                }
            },
            "HostRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "ec2.amazonaws.com" },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    "ManagedPolicyArns": [
                        "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"
                    ],
                    "Policies": [{
                        "PolicyName": "assets-read",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": ["s3:GetObject", "s3:ListBucket"],
                                "Resource": [
                                    { "Fn::GetAtt": ["AssetsBucket", "Arn"] },
                                    { "Fn::Sub": "${AssetsBucket.Arn}/*" },
                                ],
                            }],
                        },
                    }],
                }
            },
            "HostProfile": {
                "Type": "AWS::IAM::InstanceProfile",
                "Properties": { "Roles": [{ "Ref": "HostRole" }] }
            },
            "Host": {
                "Type": "AWS::EC2::Instance",
                "Properties": {
                    "ImageId": { "Ref": "InstanceAmi" },
                    "InstanceType": "t3.micro",
                    "SubnetId": { "Ref": "PublicSubnet" },
                    "SecurityGroupIds": [{ "Ref": "HostGroup" }, { "Ref": "SshGroup" }],
                    "IamInstanceProfile": { "Ref": "HostProfile" },
                    "MetadataOptions": { "HttpTokens": "required", "HttpEndpoint": "enabled" },
                    "BlockDeviceMappings": [{
                        "DeviceName": "/dev/xvda",
                        "Ebs": {
                            "VolumeSize": 20,
                            "VolumeType": "gp3",
                            "Encrypted": true,
                            "DeleteOnTermination": true,
                        },
                    }],
                    "CreditSpecification": { "CPUCredits": "standard" },
                    "Tags": tags,
                }
            }
        },
        "Outputs": {
            "InstanceId": { "Value": { "Ref": "Host" } },
            "SubnetId": { "Value": { "Ref": "PublicSubnet" } },
            "RouteTableId": { "Value": { "Ref": "PublicRouteTable" } },
            "InternetGatewayId": { "Value": { "Ref": "Igw" } },
            "SshGroupId": { "Value": { "Fn::GetAtt": ["SshGroup", "GroupId"] } },
            "HostGroupId": { "Value": { "Fn::GetAtt": ["HostGroup", "GroupId"] } },
            "RoleName": { "Value": { "Ref": "HostRole" } },
            "ProfileName": { "Value": { "Ref": "HostProfile" } },
            "BucketName": { "Value": { "Ref": "AssetsBucket" } },
        }
    })
    .to_string()
}

/// Deploy the estate as `stack`, and answer the router it was deployed on beside the
/// `DescribeStacks` body that reported it settled.
async fn deploy_the_estate(
    case: &str,
    stack: &str,
    bucket: &str,
) -> (MultiServiceRouter, String) {
    let (router, _engine) = router_with_engine();
    let (status, created) = create_stack(
        &router,
        stack,
        &estate_template(),
        &acknowledging_iam(&[
            member("Parameters.member.1.ParameterKey", "AssetsBucketName"),
            member("Parameters.member.1.ParameterValue", bucket),
        ]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack must answer 200 — the stack reports the outcome — got \
         {status} {created}"
    );
    let described = settled_stack(&router, case, stack).await;
    (router, described)
}

#[tokio::test]
async fn the_ssm_managed_host_estate_deploys() {
    const STACK: &str = "ssm-managed-host";
    const CASE: &str = "the SSM-managed public-subnet host estate";
    let (router, described) = deploy_the_estate(CASE, STACK, "ssm-managed-host-assets").await;
    let settled = xml_text(&described, "StackStatus").unwrap_or_default();
    assert_eq!(
        settled,
        CREATE_COMPLETE,
        "case {CASE:?}: expected {CREATE_COMPLETE}, got {settled:?} with the failures {:?}",
        failure_reasons(&router, STACK).await
    );

    // Every output the template declares carries a resolved value: the only way a client gets
    // the created resources' identifiers out of the deployment.
    for key in [
        "InstanceId",
        "SubnetId",
        "RouteTableId",
        "InternetGatewayId",
        "SshGroupId",
        "HostGroupId",
        "RoleName",
        "ProfileName",
        "BucketName",
    ] {
        let value = output_value(&described, key);
        assert!(
            value.as_deref().is_some_and(|value| !value.is_empty()),
            "case {CASE:?}: expected the output {key:?} to carry a value, got {value:?} in \
             {described}"
        );
    }

    // ...and the instance really is Amazon EC2's, launched with the options the template asked
    // for: a read of that service addressed by the published id reports exactly it.
    let instance = output_value(&described, "InstanceId").unwrap_or_default();
    assert!(
        is_service_assigned_id(&instance, "i"),
        "case {CASE:?}: expected the instance id Amazon EC2 assigned, got {instance:?}"
    );
    let (status, body) = ec2(
        &router,
        "DescribeInstances",
        &[("InstanceId.1", instance.as_str())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {CASE:?}: DescribeInstances({instance}) must answer 200, got {status} {body}"
    );
    assert_eq!(
        xml_texts(&body, "instanceId"),
        vec![instance.clone()],
        "case {CASE:?}: expected DescribeInstances({instance}) to report exactly that one \
         instance, got {body}"
    );
    for (block, element, expected) in [
        ("metadataOptions", "httpTokens", "required"),
        ("metadataOptions", "httpEndpoint", "enabled"),
        ("blockDeviceMapping", "deviceName", "/dev/xvda"),
    ] {
        let answered = reported_member(&body, block, element);
        assert_eq!(
            answered.as_deref(),
            Some(expected),
            "case {CASE:?}: expected Amazon EC2 to report {block}.{element} as {expected:?}, got \
             {answered:?} in {body}"
        );
    }
}
