// SPDX-License-Identifier: CC0-1.0

//! A PARENT stack whose children are `AWS::CloudFormation::Stack` resources CHAINED to one
//! another, read back through `DescribeStacks` — the container-platform estate end to end
//! over the awsQuery wire.
//!
//! RULE: a stack that reaches a terminal `*_COMPLETE` status reports every output its
//! template declares as a complete pair — an `OutputKey` AND the `OutputValue` the
//! expression resolved to — whatever the expression reads, including an `Fn::GetAtt` over a
//! nested stack's `Outputs.<key>` whose own value is an attribute the owning service only
//! publishes once the resource is up, and however many levels of children the value travelled
//! through.
//!
//! The rows differ along what the rule generalises over: WHICH child publishes the value
//! (one that depends on nothing, one whose parameters are another child's outputs, and one
//! two links down that chain), and what KIND of value the output carries (a physical name a
//! request stated, an identifier the owning service minted, an address it publishes only once
//! the resource serves traffic, a value composed by an intrinsic, and an identifier that
//! embeds another resource's).

use serde_json::json;

use crate::change_set_lifecycle::{create_change_set, describe_change_set, execute_change_set};
use crate::harness::{call, create_stack, member, router_with_engine, settled_stack, REGION};
use crate::nested_stacks::{staged_url, STACK_RESOURCE_TYPE};
use crate::stack_lifecycle::{acknowledging_iam, xml_text};
use crate::stack_outputs::{output_for, wire_outputs, WireOutput};
use crate::template_by_reference::{create_staging_bucket, stage};
use service_com_amazonaws_cloudformation_cloudformation::state::status;
use smithy_server_runtime::MultiServiceRouter;

/// The logical ids the parent declares its three children under.
const NETWORK: &str = "Network";
const SERVICE: &str = "Service";
const OBSERVABILITY: &str = "Observability";
/// A fourth child, declared so that TWO children wait on nothing and are therefore deployed
/// in the same pass: a parent with several children coming up at once is what the progress
/// pass must advance in bounded work.
const REGISTRY: &str = "Registry";

/// The registry child: a child that takes no parameters at all, so the parent starts it in
/// the same pass as the network child.
fn registry_template() -> String {
    json!({
        "Resources": {
            "Shared": {
                "Type": "AWS::ECR::Repository",
                "Properties": {"RepositoryName": "platform/shared"}
            },
            "BuildLogs": {
                "Type": "AWS::Logs::LogGroup",
                "Properties": {"LogGroupName": "/platform/builds", "RetentionInDays": 14}
            }
        },
        "Outputs": {
            "SharedRepositoryUri": {"Value": {"Fn::GetAtt": ["Shared", "RepositoryUri"]}},
            "BuildLogsArn": {"Value": {"Fn::GetAtt": ["BuildLogs", "Arn"]}}
        }
    })
    .to_string()
}

/// The network child: the VPC and the two subnets the rest of the estate is placed in, and
/// the outputs the siblings are wired to by `Fn::GetAtt`.
fn network_template() -> String {
    json!({
        "Resources": {
            "Vpc": {
                "Type": "AWS::EC2::VPC",
                "Properties": {
                    "CidrBlock": "10.20.0.0/16",
                    "EnableDnsSupport": true,
                    "EnableDnsHostnames": true
                }
            },
            "Gateway": {"Type": "AWS::EC2::InternetGateway"},
            "GatewayAttachment": {
                "Type": "AWS::EC2::VPCGatewayAttachment",
                "Properties": {
                    "VpcId": {"Ref": "Vpc"},
                    "InternetGatewayId": {"Ref": "Gateway"}
                }
            },
            "SubnetA": {
                "Type": "AWS::EC2::Subnet",
                "Properties": {
                    "VpcId": {"Ref": "Vpc"},
                    "CidrBlock": "10.20.1.0/24",
                    "AvailabilityZone": "us-east-1a",
                    "MapPublicIpOnLaunch": true
                }
            },
            "SubnetB": {
                "Type": "AWS::EC2::Subnet",
                "Properties": {
                    "VpcId": {"Ref": "Vpc"},
                    "CidrBlock": "10.20.2.0/24",
                    "AvailabilityZone": "us-east-1b",
                    "MapPublicIpOnLaunch": true
                }
            },
            "RouteTable": {
                "Type": "AWS::EC2::RouteTable",
                "Properties": {"VpcId": {"Ref": "Vpc"}}
            },
            "PublicRoute": {
                "Type": "AWS::EC2::Route",
                "DependsOn": "GatewayAttachment",
                "Properties": {
                    "RouteTableId": {"Ref": "RouteTable"},
                    "DestinationCidrBlock": "0.0.0.0/0",
                    "GatewayId": {"Ref": "Gateway"}
                }
            },
            "SubnetARoute": {
                "Type": "AWS::EC2::SubnetRouteTableAssociation",
                "Properties": {
                    "SubnetId": {"Ref": "SubnetA"},
                    "RouteTableId": {"Ref": "RouteTable"}
                }
            },
            "SubnetBRoute": {
                "Type": "AWS::EC2::SubnetRouteTableAssociation",
                "Properties": {
                    "SubnetId": {"Ref": "SubnetB"},
                    "RouteTableId": {"Ref": "RouteTable"}
                }
            },
            "AlbSecurityGroup": {
                "Type": "AWS::EC2::SecurityGroup",
                "Properties": {
                    "GroupDescription": "the load balancers own group",
                    "VpcId": {"Ref": "Vpc"},
                    "SecurityGroupIngress": [{
                        "IpProtocol": "tcp",
                        "FromPort": 80,
                        "ToPort": 80,
                        "CidrIp": "0.0.0.0/0"
                    }]
                }
            },
            "TaskSecurityGroup": {
                "Type": "AWS::EC2::SecurityGroup",
                "Properties": {
                    "GroupDescription": "the tasks own group",
                    "VpcId": {"Ref": "Vpc"},
                    "SecurityGroupIngress": [{
                        "IpProtocol": "tcp",
                        "FromPort": 80,
                        "ToPort": 80,
                        "SourceSecurityGroupId": {"Fn::GetAtt": ["AlbSecurityGroup", "GroupId"]}
                    }]
                }
            }
        },
        "Outputs": {
            "VpcId": {"Value": {"Ref": "Vpc"}},
            "SubnetAId": {"Value": {"Ref": "SubnetA"}},
            "SubnetBId": {"Value": {"Ref": "SubnetB"}},
            "SubnetIds": {
                "Value": {"Fn::Join": [",", [{"Ref": "SubnetA"}, {"Ref": "SubnetB"}]]}
            },
            "AlbSecurityGroupId": {"Value": {"Fn::GetAtt": ["AlbSecurityGroup", "GroupId"]}},
            "TaskSecurityGroupId": {"Value": {"Fn::GetAtt": ["TaskSecurityGroup", "GroupId"]}}
        }
    })
    .to_string()
}

/// The service child: the registry, the cluster, the task the cluster runs and the load
/// balancer in front of it, placed in the network the parent passed down.
fn service_template() -> String {
    json!({
        "Parameters": {
            "VpcId": {"Type": "String"},
            "SubnetIds": {"Type": "CommaDelimitedList"},
            "AlbSecurityGroupId": {"Type": "String"},
            "TaskSecurityGroupId": {"Type": "String"}
        },
        "Resources": {
            "Repository": {
                "Type": "AWS::ECR::Repository",
                "Properties": {
                    "RepositoryName": "platform/web",
                    "ImageScanningConfiguration": {"ScanOnPush": true},
                    "ImageTagMutability": "MUTABLE",
                    "LifecyclePolicy": {
                        "LifecyclePolicyText": "{\"rules\":[{\"rulePriority\":1,\"description\":\"keep ten\",\"selection\":{\"tagStatus\":\"any\",\"countType\":\"imageCountMoreThan\",\"countNumber\":10},\"action\":{\"type\":\"expire\"}}]}"
                    },
                    "Tags": [{"Key": "Platform", "Value": "web"}]
                }
            },
            "Cluster": {
                "Type": "AWS::ECS::Cluster",
                "Properties": {"ClusterName": "platform-cluster"}
            },
            "LogGroup": {
                "Type": "AWS::Logs::LogGroup",
                "Properties": {"LogGroupName": "/ecs/platform-web", "RetentionInDays": 7}
            },
            "ExecutionRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": "platform-web-execution",
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": {"Service": "ecs-tasks.amazonaws.com"},
                            "Action": "sts:AssumeRole"
                        }]
                    },
                    "ManagedPolicyArns": [
                        "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy"
                    ],
                    "Policies": [{
                        "PolicyName": "write-its-own-logs",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": ["logs:CreateLogStream", "logs:PutLogEvents"],
                                "Resource": {"Fn::GetAtt": ["LogGroup", "Arn"]}
                            }]
                        }
                    }]
                }
            },
            "Task": {
                "Type": "AWS::ECS::TaskDefinition",
                "Properties": {
                    "Family": "platform-web",
                    "Cpu": "256",
                    "Memory": "512",
                    "NetworkMode": "awsvpc",
                    "RequiresCompatibilities": ["FARGATE"],
                    "RuntimePlatform": {
                        "CpuArchitecture": "X86_64",
                        "OperatingSystemFamily": "LINUX"
                    },
                    "ExecutionRoleArn": {"Fn::GetAtt": ["ExecutionRole", "Arn"]},
                    "ContainerDefinitions": [{
                        "Name": "app",
                        "Image": {
                            "Fn::Join": [
                                ":",
                                [{"Fn::GetAtt": ["Repository", "RepositoryUri"]}, "latest"]
                            ]
                        },
                        "Essential": true,
                        "PortMappings": [{"ContainerPort": 80, "Protocol": "tcp"}],
                        "LogConfiguration": {
                            "LogDriver": "awslogs",
                            "Options": {
                                "awslogs-group": {"Ref": "LogGroup"},
                                "awslogs-region": {"Ref": "AWS::Region"},
                                "awslogs-stream-prefix": "app"
                            }
                        }
                    }]
                }
            },
            "Alb": {
                "Type": "AWS::ElasticLoadBalancingV2::LoadBalancer",
                "Properties": {
                    "Name": "platform-alb",
                    "Type": "application",
                    "Scheme": "internet-facing",
                    "Subnets": {"Ref": "SubnetIds"},
                    "SecurityGroups": [{"Ref": "AlbSecurityGroupId"}],
                    "LoadBalancerAttributes": [
                        {"Key": "idle_timeout.timeout_seconds", "Value": "60"}
                    ]
                }
            },
            "Tg": {
                "Type": "AWS::ElasticLoadBalancingV2::TargetGroup",
                "Properties": {
                    "Name": "platform-tg",
                    "Port": 80,
                    "Protocol": "HTTP",
                    "TargetType": "ip",
                    "VpcId": {"Ref": "VpcId"},
                    "HealthCheckPath": "/",
                    "HealthCheckProtocol": "HTTP",
                    "HealthCheckIntervalSeconds": 30,
                    "HealthCheckTimeoutSeconds": 5,
                    "HealthyThresholdCount": 2,
                    "UnhealthyThresholdCount": 3,
                    "Matcher": {"HttpCode": "200"},
                    "TargetGroupAttributes": [
                        {"Key": "deregistration_delay.timeout_seconds", "Value": "30"}
                    ]
                }
            },
            "Listener": {
                "Type": "AWS::ElasticLoadBalancingV2::Listener",
                "Properties": {
                    "LoadBalancerArn": {"Ref": "Alb"},
                    "Port": 80,
                    "Protocol": "HTTP",
                    "DefaultActions": [{"Type": "forward", "TargetGroupArn": {"Ref": "Tg"}}]
                }
            },
            "WebService": {
                "Type": "AWS::ECS::Service",
                "DependsOn": "Listener",
                "Properties": {
                    "ServiceName": "platform-web",
                    "Cluster": {"Ref": "Cluster"},
                    "TaskDefinition": {"Ref": "Task"},
                    "DesiredCount": 1,
                    "LaunchType": "FARGATE",
                    "PlatformVersion": "LATEST",
                    "HealthCheckGracePeriodSeconds": 60,
                    "DeploymentConfiguration": {
                        "MaximumPercent": 200,
                        "MinimumHealthyPercent": 100
                    },
                    "NetworkConfiguration": {
                        "AwsvpcConfiguration": {
                            "AssignPublicIp": "ENABLED",
                            "Subnets": {"Ref": "SubnetIds"},
                            "SecurityGroups": [{"Ref": "TaskSecurityGroupId"}]
                        }
                    },
                    "LoadBalancers": [{
                        "ContainerName": "app",
                        "ContainerPort": 80,
                        "TargetGroupArn": {"Ref": "Tg"}
                    }]
                }
            }
        },
        "Outputs": {
            "RepositoryUri": {"Value": {"Fn::GetAtt": ["Repository", "RepositoryUri"]}},
            "ClusterName": {"Value": {"Ref": "Cluster"}},
            "ServiceName": {"Value": {"Fn::GetAtt": ["WebService", "Name"]}},
            "ServiceArn": {"Value": {"Ref": "WebService"}},
            "LogGroupName": {"Value": {"Ref": "LogGroup"}},
            "AlbDnsName": {"Value": {"Fn::GetAtt": ["Alb", "DNSName"]}},
            "AlbFullName": {"Value": {"Fn::GetAtt": ["Alb", "LoadBalancerFullName"]}},
            "TargetGroupArn": {"Value": {"Ref": "Tg"}},
            "ListenerArn": {"Value": {"Ref": "Listener"}},
            "ServiceEndpoint": {
                "Value": {"Fn::Join": ["", ["http://", {"Fn::GetAtt": ["Alb", "DNSName"]}]]}
            }
        }
    })
    .to_string()
}

/// The `Period`, in seconds, the observability child's alarm is declared with — a value the
/// template states and CloudWatch has to keep.
const ALARM_PERIOD: u32 = 300;

/// The observability child: the log group the tasks write to and the alarm on the load
/// balancer the service child published the full name of.
fn observability_template() -> String {
    json!({
        "Parameters": {
            "ClusterName": {"Type": "String"},
            "ServiceName": {"Type": "String"},
            "AlbFullName": {"Type": "String"},
            "LogGroupName": {"Type": "String"}
        },
        "Resources": {
            "Alerts": {
                "Type": "AWS::SNS::Topic",
                "Properties": {"TopicName": "platform-web-alerts"}
            },
            "ErrorFilter": {
                "Type": "AWS::Logs::MetricFilter",
                "Properties": {
                    "LogGroupName": {"Ref": "LogGroupName"},
                    "FilterPattern": "ERROR",
                    "MetricTransformations": [{
                        "MetricNamespace": "Platform/Web",
                        "MetricName": "ErrorCount",
                        "MetricValue": "1"
                    }]
                }
            },
            "CpuAlarm": {
                "Type": "AWS::CloudWatch::Alarm",
                "Properties": {
                    "AlarmName": "platform-web-cpu",
                    "Namespace": "AWS/ECS",
                    "MetricName": "CPUUtilization",
                    "ComparisonOperator": "GreaterThanThreshold",
                    "EvaluationPeriods": 2,
                    "Period": ALARM_PERIOD,
                    "Statistic": "Average",
                    "Threshold": 80,
                    "TreatMissingData": "notBreaching",
                    "DatapointsToAlarm": 1,
                    "AlarmActions": [{"Ref": "Alerts"}],
                    "Dimensions": [
                        {"Name": "ClusterName", "Value": {"Ref": "ClusterName"}},
                        {"Name": "ServiceName", "Value": {"Ref": "ServiceName"}}
                    ]
                }
            },
            "Alarm": {
                "Type": "AWS::CloudWatch::Alarm",
                "Properties": {
                    "AlarmName": "platform-alb-5xx",
                    "Namespace": "AWS/ApplicationELB",
                    "MetricName": "HTTPCode_ELB_5XX_Count",
                    "ComparisonOperator": "GreaterThanThreshold",
                    "EvaluationPeriods": 1,
                    "Period": ALARM_PERIOD,
                    "Statistic": "Sum",
                    "Threshold": 0,
                    "Dimensions": [{"Name": "LoadBalancer", "Value": {"Ref": "AlbFullName"}}]
                }
            }
        },
        "Outputs": {
            "TopicArn": {"Value": {"Ref": "Alerts"}},
            "FilterName": {"Value": {"Ref": "ErrorFilter"}},
            "CpuAlarmArn": {"Value": {"Fn::GetAtt": ["CpuAlarm", "Arn"]}},
            "AlarmName": {"Value": {"Ref": "Alarm"}},
            "AlarmArn": {"Value": {"Fn::GetAtt": ["Alarm", "Arn"]}},
            "WatchedService": {
                "Value": {"Fn::Join": ["/", [{"Ref": "ClusterName"}, {"Ref": "ServiceName"}]]}
            }
        }
    })
    .to_string()
}

/// The parent: the three children, each wired to the one before it by `Fn::GetAtt` over its
/// outputs, and one declared output per value the estate publishes.
fn parent_template(
    network_key: &str,
    service_key: &str,
    observability_key: &str,
    registry_key: &str,
) -> String {
    let mut outputs = serde_json::Map::new();
    for row in rows() {
        outputs.insert(
            row.parent_key.to_string(),
            json!({
                "Value": {"Fn::GetAtt": [row.child, format!("Outputs.{}", row.child_key)]},
                "Description": row.case
            }),
        );
    }
    json!({
        "Resources": {
            NETWORK: {
                "Type": STACK_RESOURCE_TYPE,
                "Properties": {"TemplateURL": staged_url(network_key)}
            },
            REGISTRY: {
                "Type": STACK_RESOURCE_TYPE,
                "Properties": {"TemplateURL": staged_url(registry_key)}
            },
            SERVICE: {
                "Type": STACK_RESOURCE_TYPE,
                "Properties": {
                    "TemplateURL": staged_url(service_key),
                    "Parameters": {
                        "VpcId": {"Fn::GetAtt": [NETWORK, "Outputs.VpcId"]},
                        "SubnetIds": {"Fn::GetAtt": [NETWORK, "Outputs.SubnetIds"]},
                        "AlbSecurityGroupId": {
                            "Fn::GetAtt": [NETWORK, "Outputs.AlbSecurityGroupId"]
                        },
                        "TaskSecurityGroupId": {
                            "Fn::GetAtt": [NETWORK, "Outputs.TaskSecurityGroupId"]
                        }
                    }
                }
            },
            OBSERVABILITY: {
                "Type": STACK_RESOURCE_TYPE,
                "Properties": {
                    "TemplateURL": staged_url(observability_key),
                    "Parameters": {
                        "ClusterName": {"Fn::GetAtt": [SERVICE, "Outputs.ClusterName"]},
                        "ServiceName": {"Fn::GetAtt": [SERVICE, "Outputs.ServiceName"]},
                        "AlbFullName": {"Fn::GetAtt": [SERVICE, "Outputs.AlbFullName"]},
                        "LogGroupName": {"Fn::GetAtt": [SERVICE, "Outputs.LogGroupName"]}
                    }
                }
            }
        },
        "Outputs": outputs
    })
    .to_string()
}

/// One row: an output the parent declares over one output of one child.
struct Row {
    case: &'static str,
    /// The `OutputKey` the PARENT declares.
    parent_key: &'static str,
    /// The logical id of the child it reads.
    child: &'static str,
    /// The `OutputKey` of that child it reads.
    child_key: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an identifier the owning service minted, from the child that waits for \
                   nothing",
            parent_key: "VpcId",
            child: NETWORK,
            child_key: "VpcId",
        },
        Row {
            case: "a second identifier of that same child",
            parent_key: "SubnetAId",
            child: NETWORK,
            child_key: "SubnetAId",
        },
        Row {
            case: "a list of identifiers an intrinsic joined",
            parent_key: "SubnetIds",
            child: NETWORK,
            child_key: "SubnetIds",
        },
        Row {
            case: "a value from a SECOND child that waits for nothing, so two children come \
                   up at once",
            parent_key: "SharedRepositoryUri",
            child: REGISTRY,
            child_key: "SharedRepositoryUri",
        },
        Row {
            case: "a second value from that child",
            parent_key: "BuildLogsArn",
            child: REGISTRY,
            child_key: "BuildLogsArn",
        },
        Row {
            case: "a registry address, from the child parameterised by another child's \
                   outputs",
            parent_key: "RepositoryUri",
            child: SERVICE,
            child_key: "RepositoryUri",
        },
        Row {
            case: "a physical name the declaration stated",
            parent_key: "ClusterName",
            child: SERVICE,
            child_key: "ClusterName",
        },
        Row {
            case: "an attribute of a resource created only once another resource was ready",
            parent_key: "ServiceName",
            child: SERVICE,
            child_key: "ServiceName",
        },
        Row {
            case: "an identifier embedding another resource's",
            parent_key: "ServiceArn",
            child: SERVICE,
            child_key: "ServiceArn",
        },
        Row {
            case: "an address the owning service publishes for traffic",
            parent_key: "AlbDnsName",
            child: SERVICE,
            child_key: "AlbDnsName",
        },
        Row {
            case: "a value an intrinsic composed from such an address",
            parent_key: "ServiceEndpoint",
            child: SERVICE,
            child_key: "ServiceEndpoint",
        },
        Row {
            case: "a listener's identifier, which nests inside its load balancer's",
            parent_key: "ListenerArn",
            child: SERVICE,
            child_key: "ListenerArn",
        },
        Row {
            case: "a name from the child two links down the chain",
            parent_key: "LogGroupName",
            child: SERVICE,
            child_key: "LogGroupName",
        },
        Row {
            case: "a name minted for a resource the chain's last child declared",
            parent_key: "FilterName",
            child: OBSERVABILITY,
            child_key: "FilterName",
        },
        Row {
            case: "an ARN from that same child",
            parent_key: "AlarmArn",
            child: OBSERVABILITY,
            child_key: "AlarmArn",
        },
        Row {
            case: "a value that child composed from the parameters the chain passed down",
            parent_key: "WatchedService",
            child: OBSERVABILITY,
            child_key: "WatchedService",
        },
    ]
}

/// The parent's newest events, formatted for a diagnostic: a deployment that did not come up
/// says which resource failed and why.
async fn stack_events(router: &MultiServiceRouter, stack: &str) -> String {
    let (_, body) = call(router, "DescribeStackEvents", &[member("StackName", stack)]).await;
    let newest: Vec<String> = crate::harness::xml_blocks(&body, "member")
        .iter()
        .take(40)
        .map(|event| {
            format!(
                "{:?} {:?} {:?}",
                xml_text(event, "LogicalResourceId"),
                xml_text(event, "ResourceStatus"),
                xml_text(event, "ResourceStatusReason")
            )
        })
        .collect();
    format!("{newest:?}")
}

/// Every stack the Region holds, with its status and its events: the whole estate's account
/// of itself, for the diagnostic of a deploy that did not come up.
async fn estate(router: &MultiServiceRouter) -> String {
    let (_, body) = call(router, "ListStacks", &[]).await;
    let mut reported = Vec::new();
    for summary in crate::harness::xml_blocks(&body, "member") {
        let name = xml_text(&summary, "StackName").unwrap_or_default();
        let id = xml_text(&summary, "StackId").unwrap_or_default();
        reported.push(format!(
            "{name} {:?} {:?} events {}",
            xml_text(&summary, "StackStatus"),
            xml_text(&summary, "StackStatusReason"),
            stack_events(router, &id).await
        ));
    }
    reported.join("\n  ")
}

/// The stack id of the child the parent declared under `logical_id`, which the parent reports
/// as that resource's `PhysicalResourceId`.
async fn child_stack_id(router: &MultiServiceRouter, parent: &str, logical_id: &str) -> String {
    let (code, body) = call(
        router,
        "DescribeStackResource",
        &[
            member("StackName", parent),
            member("LogicalResourceId", logical_id),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "the parent's {logical_id} resource must be readable, got {code} {body}; the parent \
         reported {}",
        stack_events(router, parent).await
    );
    xml_text(&body, "PhysicalResourceId").unwrap_or_default()
}

/// Every output a settled stack reports, with the status it settled on.
async fn settled_outputs(
    case: &str,
    router: &MultiServiceRouter,
    stack: &str,
) -> (String, Vec<WireOutput>) {
    let body = settled_stack(case, router, REGION, stack).await;
    (
        xml_text(&body, "StackStatus").unwrap_or_default(),
        wire_outputs(&body),
    )
}

/// RULE: every declared output of a stack that settled `*_COMPLETE` carries a resolved
/// `OutputValue` — including one reading a nested stack's own output, however many children
/// the value travelled through and whichever service publishes it.
#[tokio::test]
async fn every_declared_output_of_a_settled_nested_platform_carries_a_value() {
    let (router, _engine) = router_with_engine();
    create_staging_bucket(&router).await;
    let network_key = "platform-network.json";
    let service_key = "platform-service.json";
    let observability_key = "platform-observability.json";
    let registry_key = "platform-registry.json";
    stage(&router, network_key, &network_template()).await;
    stage(&router, service_key, &service_template()).await;
    stage(&router, observability_key, &observability_template()).await;
    stage(&router, registry_key, &registry_template()).await;

    let parent = "platform-parent";
    let (code, body) = create_stack(
        &router,
        parent,
        &parent_template(network_key, service_key, observability_key, registry_key),
        // The IAM role the service child declares is covered by the PARENT's own
        // acknowledgement: "if you have IAM resources in nested stacks, you must specify
        // CAPABILITY_IAM" on the parent operation. The stack-level tags are propagated to
        // every resource the estate creates, the nested stacks among them.
        &acknowledging_iam(&[
            member("Tags.member.1.Key", "Platform"),
            member("Tags.member.1.Value", "web"),
            member("Tags.member.2.Key", "Owner"),
            member("Tags.member.2.Value", "platform-team"),
        ]),
    )
    .await;
    assert_eq!(code, 200, "the parent must be created, got {code} {body}");

    let (parent_status, parent_outputs) = settled_outputs("the parent", &router, parent).await;
    assert_eq!(
        parent_status,
        status::CREATE_COMPLETE,
        "expected the parent complete, the estate reported\n  {}",
        estate(&router).await
    );

    for row in rows() {
        let reported = output_for(&parent_outputs, row.parent_key).unwrap_or_else(|| {
            panic!(
                "case {:?}: expected the parent to report an output {:?}, got {parent_outputs:?}",
                row.case, row.parent_key
            )
        });
        let value = reported.value.clone().unwrap_or_default();
        assert!(
            !value.is_empty(),
            "case {:?}: expected output {:?} to carry a resolved OutputValue, got {:?}",
            row.case,
            row.parent_key,
            reported.value
        );

        // ...and the value is the CHILD's own, read back through that stack's own
        // `DescribeStacks`: the parent publishes what the child published, never a value of
        // its own making.
        let child_id = child_stack_id(&router, parent, row.child).await;
        let (child_status, child_outputs) = settled_outputs(row.case, &router, &child_id).await;
        assert_eq!(
            child_status,
            status::CREATE_COMPLETE,
            "case {:?}: expected the child {:?} complete, its events were {}",
            row.case,
            row.child,
            stack_events(&router, &child_id).await
        );
        let published = output_for(&child_outputs, row.child_key)
            .and_then(|output| output.value.clone())
            .unwrap_or_default();
        assert_eq!(
            value, published,
            "case {:?}: expected the parent's {:?} to be the child's {:?}, got {value:?} \
             against {published:?}",
            row.case, row.parent_key, row.child_key
        );
    }
}

/// RULE: the rule holds however the template reached the stack. A change set over the SAME
/// parent, created with `IncludeNestedStacks` so CloudFormation plans the children too, and
/// then executed, deploys the same estate — so the stack it leaves reports the same resolved
/// outputs, which is the path `aws cloudformation deploy` takes.
#[tokio::test]
async fn a_change_set_including_the_nested_stacks_deploys_the_same_platform() {
    let (router, _engine) = router_with_engine();
    create_staging_bucket(&router).await;
    let network_key = "changeset-platform-network.json";
    let service_key = "changeset-platform-service.json";
    let observability_key = "changeset-platform-observability.json";
    let registry_key = "changeset-platform-registry.json";
    stage(&router, network_key, &network_template()).await;
    stage(&router, service_key, &service_template()).await;
    stage(&router, observability_key, &observability_template()).await;
    stage(&router, registry_key, &registry_template()).await;
    let parent_key = "changeset-platform-parent.json";
    stage(
        &router,
        parent_key,
        &parent_template(network_key, service_key, observability_key, registry_key),
    )
    .await;

    let parent = "changeset-platform-parent";
    let change_set = "deploy-the-platform";
    let (code, body) = create_change_set(
        &router,
        parent,
        change_set,
        &acknowledging_iam(&[
            member("ChangeSetType", "CREATE"),
            member("TemplateURL", &staged_url(parent_key)),
            member("IncludeNestedStacks", "true"),
        ]),
    )
    .await;
    assert_eq!(
        code, 200,
        "the change set must be created, got {code} {body}"
    );
    await_the_change_set(&router, parent, change_set).await;
    let (code, body) = execute_change_set(&router, change_set, Some(parent)).await;
    assert_eq!(
        code, 200,
        "the change set must be executed, got {code} {body}"
    );

    let (parent_status, parent_outputs) = settled_outputs("the parent", &router, parent).await;
    assert_eq!(
        parent_status,
        status::CREATE_COMPLETE,
        "expected the parent complete, the estate reported\n  {}",
        estate(&router).await
    );
    for row in rows() {
        let reported = output_for(&parent_outputs, row.parent_key).unwrap_or_else(|| {
            panic!(
                "case {:?}: expected the parent to report an output {:?}, got {parent_outputs:?}",
                row.case, row.parent_key
            )
        });
        assert!(
            reported.value.as_deref().is_some_and(|value| !value.is_empty()),
            "case {:?}: expected output {:?} to carry a resolved OutputValue, got {:?}",
            row.case,
            row.parent_key,
            reported.value
        );
    }
}

/// Poll `DescribeChangeSet` the way a client's waiter does, until the plan is one the change
/// set can be executed from.
async fn await_the_change_set(router: &MultiServiceRouter, parent: &str, change_set: &str) {
    let deadline = std::time::Instant::now() + crate::harness::SETTLE_DEADLINE;
    loop {
        let (code, body) = describe_change_set(router, change_set, Some(parent)).await;
        let seen = xml_text(&body, "Status").unwrap_or_default();
        if code == 200 && !seen.ends_with("_PENDING") && !seen.ends_with("_IN_PROGRESS") {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "expected the change set to be planned, last saw {seen:?} ({code} {body})"
        );
        tokio::time::sleep(crate::harness::SETTLE_POLL_INTERVAL).await;
    }
}

/// The YAML network child: the same network, written the way a template author writes one —
/// short-form intrinsics, and resources the template does not NAME, so every identifier is
/// one CloudFormation generated for a child stack whose own name already carries its
/// parent's.
///
/// Like its JSON sibling it attaches an INTERNET GATEWAY to the VPC, which the public load
/// balancer of the service child placed in these subnets needs.
fn yaml_network_template() -> &'static str {
    concat!(
        "Resources:\n",
        "  Vpc:\n",
        "    Type: AWS::EC2::VPC\n",
        "    Properties:\n",
        "      CidrBlock: 10.30.0.0/16\n",
        "  Gateway:\n",
        "    Type: AWS::EC2::InternetGateway\n",
        "  GatewayAttachment:\n",
        "    Type: AWS::EC2::VPCGatewayAttachment\n",
        "    Properties:\n",
        "      VpcId: !Ref Vpc\n",
        "      InternetGatewayId: !Ref Gateway\n",
        "  SubnetA:\n",
        "    Type: AWS::EC2::Subnet\n",
        "    Properties:\n",
        "      VpcId: !Ref Vpc\n",
        "      CidrBlock: 10.30.1.0/24\n",
        "      AvailabilityZone: !Select [0, !GetAZs \"\"]\n",
        "  SubnetB:\n",
        "    Type: AWS::EC2::Subnet\n",
        "    Properties:\n",
        "      VpcId: !Ref Vpc\n",
        "      CidrBlock: 10.30.2.0/24\n",
        "      AvailabilityZone: !Select [1, !GetAZs \"\"]\n",
        "  TaskSecurityGroup:\n",
        "    Type: AWS::EC2::SecurityGroup\n",
        "    Properties:\n",
        "      GroupDescription: the group the tasks run in\n",
        "      VpcId: !Ref Vpc\n",
        "Outputs:\n",
        "  VpcId:\n",
        "    Value: !Ref Vpc\n",
        "  SubnetIds:\n",
        "    Value: !Join [\",\", [!Ref SubnetA, !Ref SubnetB]]\n",
        "  SecurityGroupIds:\n",
        "    Value: !GetAtt TaskSecurityGroup.GroupId\n",
    )
}

/// The YAML service child: the same workload with nothing named and its network taken through
/// AWS-specifically TYPED parameters, which CloudFormation resolves against Amazon EC2 before
/// the deployment runs.
fn yaml_service_template() -> &'static str {
    concat!(
        "Parameters:\n",
        "  VpcId:\n",
        "    Type: AWS::EC2::VPC::Id\n",
        "  SubnetIds:\n",
        "    Type: List<AWS::EC2::Subnet::Id>\n",
        "  SecurityGroupIds:\n",
        "    Type: List<AWS::EC2::SecurityGroup::Id>\n",
        "Resources:\n",
        "  Repository:\n",
        "    Type: AWS::ECR::Repository\n",
        "  Cluster:\n",
        "    Type: AWS::ECS::Cluster\n",
        "  LogGroup:\n",
        "    Type: AWS::Logs::LogGroup\n",
        "  ExecutionRole:\n",
        "    Type: AWS::IAM::Role\n",
        "    Properties:\n",
        "      AssumeRolePolicyDocument:\n",
        "        Version: \"2012-10-17\"\n",
        "        Statement:\n",
        "          - Effect: Allow\n",
        "            Principal:\n",
        "              Service: [ecs-tasks.amazonaws.com]\n",
        "            Action: sts:AssumeRole\n",
        "  Task:\n",
        "    Type: AWS::ECS::TaskDefinition\n",
        "    Properties:\n",
        "      Cpu: \"256\"\n",
        "      Memory: \"512\"\n",
        "      NetworkMode: awsvpc\n",
        "      RequiresCompatibilities: [FARGATE]\n",
        "      ExecutionRoleArn: !GetAtt ExecutionRole.Arn\n",
        "      ContainerDefinitions:\n",
        "        - Name: app\n",
        "          Image: !Sub \"${Repository.RepositoryUri}:latest\"\n",
        "          PortMappings:\n",
        "            - ContainerPort: 80\n",
        "          LogConfiguration:\n",
        "            LogDriver: awslogs\n",
        "            Options:\n",
        "              awslogs-group: !Ref LogGroup\n",
        "              awslogs-region: !Ref AWS::Region\n",
        "              awslogs-stream-prefix: app\n",
        "  Alb:\n",
        "    Type: AWS::ElasticLoadBalancingV2::LoadBalancer\n",
        "    Properties:\n",
        "      Subnets: !Ref SubnetIds\n",
        "      SecurityGroups: !Ref SecurityGroupIds\n",
        "  Tg:\n",
        "    Type: AWS::ElasticLoadBalancingV2::TargetGroup\n",
        "    Properties:\n",
        "      Port: 80\n",
        "      Protocol: HTTP\n",
        "      TargetType: ip\n",
        "      VpcId: !Ref VpcId\n",
        "  Listener:\n",
        "    Type: AWS::ElasticLoadBalancingV2::Listener\n",
        "    Properties:\n",
        "      LoadBalancerArn: !Ref Alb\n",
        "      Port: 80\n",
        "      Protocol: HTTP\n",
        "      DefaultActions:\n",
        "        - Type: forward\n",
        "          TargetGroupArn: !Ref Tg\n",
        "  WebService:\n",
        "    Type: AWS::ECS::Service\n",
        "    DependsOn: Listener\n",
        "    Properties:\n",
        "      Cluster: !Ref Cluster\n",
        "      TaskDefinition: !Ref Task\n",
        "      DesiredCount: 1\n",
        "      LaunchType: FARGATE\n",
        "      NetworkConfiguration:\n",
        "        AwsvpcConfiguration:\n",
        "          AssignPublicIp: ENABLED\n",
        "          Subnets: !Ref SubnetIds\n",
        "          SecurityGroups: !Ref SecurityGroupIds\n",
        "      LoadBalancers:\n",
        "        - ContainerName: app\n",
        "          ContainerPort: 80\n",
        "          TargetGroupArn: !Ref Tg\n",
        "Outputs:\n",
        "  ClusterName:\n",
        "    Value: !Ref Cluster\n",
        "  ServiceName:\n",
        "    Value: !GetAtt WebService.Name\n",
        "  RepositoryUri:\n",
        "    Value: !GetAtt Repository.RepositoryUri\n",
        "  LogGroupName:\n",
        "    Value: !Ref LogGroup\n",
        "  AlbDnsName:\n",
        "    Value: !GetAtt Alb.DNSName\n",
        "  AlbFullName:\n",
        "    Value: !GetAtt Alb.LoadBalancerFullName\n",
        "  TaskDefinitionArn:\n",
        "    Value: !GetAtt Task.TaskDefinitionArn\n",
        "  ListenerArn:\n",
        "    Value: !GetAtt Listener.ListenerArn\n",
    )
}

/// The YAML parent: two children wired by the short `!GetAtt <Child>.Outputs.<Key>` form, one
/// output reading each value the estate publishes, and one composed by `Fn::Sub` over a
/// child's output rather than read straight out of it.
fn yaml_parent_template(network_key: &str, service_key: &str) -> String {
    let mut parent = String::from("Resources:\n");
    parent.push_str(&format!(
        concat!(
            "  {network}:\n",
            "    Type: {stack_type}\n",
            "    Properties:\n",
            "      TemplateURL: {network_url}\n",
            "  {service}:\n",
            "    Type: {stack_type}\n",
            "    Properties:\n",
            "      TemplateURL: {service_url}\n",
            "      Parameters:\n",
            "        VpcId: !GetAtt {network}.Outputs.VpcId\n",
            "        SubnetIds: !GetAtt {network}.Outputs.SubnetIds\n",
            "        SecurityGroupIds: !GetAtt {network}.Outputs.SecurityGroupIds\n",
            "Outputs:\n",
        ),
        network = NETWORK,
        service = SERVICE,
        stack_type = STACK_RESOURCE_TYPE,
        network_url = staged_url(network_key),
        service_url = staged_url(service_key),
    ));
    for (key, child) in yaml_rows() {
        parent.push_str(&format!(
            "  {key}:\n    Value: !GetAtt {child}.Outputs.{key}\n"
        ));
    }
    // ...and one the parent COMPOSES from a child's output rather than passing through.
    parent.push_str(&format!(
        "  ServiceEndpoint:\n    Value: !Sub \"http://${{{SERVICE}.Outputs.AlbDnsName}}\"\n"
    ));
    parent
}

/// The outputs the YAML parent reads off its children, each keyed the same on both sides.
fn yaml_rows() -> Vec<(&'static str, &'static str)> {
    vec![
        ("VpcId", NETWORK),
        ("SubnetIds", NETWORK),
        ("SecurityGroupIds", NETWORK),
        ("ClusterName", SERVICE),
        ("ServiceName", SERVICE),
        ("RepositoryUri", SERVICE),
        ("LogGroupName", SERVICE),
        ("AlbDnsName", SERVICE),
        ("AlbFullName", SERVICE),
        ("TaskDefinitionArn", SERVICE),
        ("ListenerArn", SERVICE),
    ]
}

/// RULE: the same rule, over the other half of what it generalises over — a YAML template with
/// short-form intrinsics, resources the template does not NAME (so every identifier is
/// CloudFormation's own generated one, inside a child stack whose name already carries its
/// parent's), a child taking its network through AWS-specifically typed parameters, and a
/// parent output the template COMPOSES from a child's output.
#[tokio::test]
async fn the_rule_holds_for_a_yaml_platform_whose_resources_are_unnamed() {
    let (router, _engine) = router_with_engine();
    create_staging_bucket(&router).await;
    let network_key = "yaml-platform-network.yaml";
    let service_key = "yaml-platform-service.yaml";
    stage(&router, network_key, yaml_network_template()).await;
    stage(&router, service_key, yaml_service_template()).await;

    let parent = "yaml-platform-parent";
    let (code, body) = create_stack(
        &router,
        parent,
        &yaml_parent_template(network_key, service_key),
        // The role this child declares carries no name of its own, which is the form
        // `CAPABILITY_IAM` alone acknowledges — "if you have IAM resources, you can specify
        // either capability".
        &[member("Capabilities.member.1", "CAPABILITY_IAM")],
    )
    .await;
    assert_eq!(code, 200, "the parent must be created, got {code} {body}");

    let (parent_status, parent_outputs) = settled_outputs("the parent", &router, parent).await;
    assert_eq!(
        parent_status,
        status::CREATE_COMPLETE,
        "expected the parent complete, the estate reported\n  {}",
        estate(&router).await
    );

    let mut expected: Vec<(&str, String)> = Vec::new();
    for (key, child) in yaml_rows() {
        let reported = output_for(&parent_outputs, key).unwrap_or_else(|| {
            panic!("case {key:?}: expected the parent to report it, got {parent_outputs:?}")
        });
        let value = reported.value.clone().unwrap_or_default();
        assert!(
            !value.is_empty(),
            "case {key:?}: expected a resolved OutputValue, got {:?}",
            reported.value
        );
        let child_id = child_stack_id(&router, parent, child).await;
        let (_, child_outputs) = settled_outputs(key, &router, &child_id).await;
        let published = output_for(&child_outputs, key)
            .and_then(|output| output.value.clone())
            .unwrap_or_default();
        assert_eq!(
            value, published,
            "case {key:?}: expected the parent's value to be the child's, got {value:?} \
             against {published:?}"
        );
        expected.push((key, value));
    }

    // The composed output carries the child's address inside the text the template wrote
    // around it, which is only possible once that address resolved.
    let address = expected
        .iter()
        .find(|(key, _)| *key == "AlbDnsName")
        .map(|(_, value)| value.clone())
        .unwrap_or_default();
    let composed = output_for(&parent_outputs, "ServiceEndpoint")
        .and_then(|output| output.value.clone())
        .unwrap_or_default();
    assert_eq!(
        composed,
        format!("http://{address}"),
        "case \"an output Fn::Sub composed from a child's output\": expected the child's \
         address inside it, got {composed:?}"
    );
}
