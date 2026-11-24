// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation `AWS::ElasticBeanstalk::Environment` whose template declares the
//! `OptionSettings` a real deployment carries is CREATED, not rolled back.
//!
//! RULE: a resource's declared properties go to the owning service as that service's own
//! request, so every configuration option the Elastic Beanstalk option reference defines is
//! one a stack may declare — Elastic Beanstalk resolves each `(Namespace, OptionName)`
//! against the catalogue the platform version publishes, and an option that catalogue does
//! not carry is reported as naming no configuration setting, which fails the resource and
//! rolls the stack back. The catalogue is therefore a FIDELITY requirement of the stack path
//! and not only of `DescribeConfigurationOptions`: one missing option name turns a template
//! AWS deploys into a `ROLLBACK_COMPLETE`.
//!
//! The suite spans the shape of the declaration rather than one template: the network
//! options of a virtual private cloud deployment, the instances' own, the environment's and
//! the health reporting's, and an environment property whose NAME the caller chooses. Two
//! crates have to agree for the rule to hold (CloudFormation makes the call, Elastic
//! Beanstalk serves it), so the suite lives here.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::json;
use service_com_amazonaws_elasticbeanstalk_awselasticbeanstalkservice as eb;
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{elements, form_encode, opt_element};

/// The Region the stack deploys in.
const REGION: &str = "us-west-2";

/// Elastic Beanstalk's API version, as its model declares it.
const EB_API_VERSION: &str = "2010-12-01";

/// Long enough for both services' clock-owed spans to come due, derived from Elastic
/// Beanstalk's own constant so the two cannot drift apart. Only a stack that NEVER leaves
/// `CREATE_IN_PROGRESS` fails on it.
const DEADLINE: Duration = eb::state::STATUS_TRANSITION_SPAN
    .saturating_mul(40)
    .saturating_add(Duration::from_secs(60));

/// How often the stack is looked at again while it is still in progress.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The emulator with the two services this suite exercises, mounted on ONE engine.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(eb::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`],
/// which is what routes the request; the signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one awsQuery operation of `service`, returning its status and body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// The option settings a deployment of this shape declares: `(Namespace, OptionName, Value)`.
///
/// Source: Elastic Beanstalk's "general options for all environments" reference, plus one
/// environment property, whose NAME is the caller's own. The values are the ones a stack
/// resolves its references to — an id of the network it created, the profile it created —
/// written here as literals, since what the rule is about is the SPECIFICATION each setting
/// names.
const DECLARED_OPTIONS: &[(&str, &str, &str)] = &[
    ("aws:ec2:vpc", "AssociatePublicIpAddress", "true"),
    (
        "aws:autoscaling:launchconfiguration",
        "InstanceType",
        "t3.small",
    ),
    ("aws:autoscaling:asg", "MinSize", "1"),
    ("aws:autoscaling:asg", "MaxSize", "1"),
    (
        "aws:elasticbeanstalk:environment",
        "EnvironmentType",
        "SingleInstance",
    ),
    (
        "aws:elasticbeanstalk:healthreporting:system",
        "SystemType",
        "enhanced",
    ),
    (
        "aws:elasticbeanstalk:application:environment",
        "DEPLOYMENT_STAGE",
        "qa",
    ),
    (
        "aws:elasticbeanstalk:container:python",
        "WSGIPath",
        "application:application",
    ),
    (
        "aws:elasticbeanstalk:container:python:staticfiles",
        "/assets",
        "assets",
    ),
    (
        "aws:elasticbeanstalk:environment:process:default",
        "HealthCheckPath",
        "/health",
    ),
    ("aws:elasticbeanstalk:environment:proxy", "ProxyServer", "nginx"),
    ("aws:elb:policies", "ConnectionDrainingEnabled", "true"),
    (
        "aws:autoscaling:launchconfiguration",
        "DisableDefaultEC2SecurityGroup",
        "true",
    ),
];

/// The settings whose value is a REFERENCE the stack resolves — the network it created, the
/// instance profile it created, the security group it created — which is how a template
/// names them.
const REFERRED_OPTIONS: &[(&str, &str, &str)] = &[
    ("aws:ec2:vpc", "VPCId", "Vpc"),
    ("aws:ec2:vpc", "Subnets", "Subnet"),
    (
        "aws:autoscaling:launchconfiguration",
        "IamInstanceProfile",
        "InstanceProfile",
    ),
    (
        "aws:autoscaling:launchconfiguration",
        "SecurityGroups",
        "InstanceSg",
    ),
];

/// The template the stack deploys: an application, and an environment on it declaring
/// [`DECLARED_OPTIONS`].
fn template() -> String {
    let mut settings: Vec<serde_json::Value> = DECLARED_OPTIONS
        .iter()
        .map(|(namespace, option_name, value)| {
            json!({ "Namespace": namespace, "OptionName": option_name, "Value": value })
        })
        .collect();
    settings.extend(REFERRED_OPTIONS.iter().map(|(namespace, option_name, logical)| {
        json!({
            "Namespace": namespace,
            "OptionName": option_name,
            "Value": { "Ref": logical },
        })
    }));
    let stack = eb::state::SOLUTION_STACKS
        .iter()
        .find(|stack| stack.platform_category == "Python")
        .expect("the service publishes a Python platform version")
        .name();
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Vpc": {
                "Type": "AWS::EC2::VPC",
                "Properties": { "CidrBlock": "10.0.0.0/16" }
            },
            "Igw": { "Type": "AWS::EC2::InternetGateway" },
            "Attach": {
                "Type": "AWS::EC2::VPCGatewayAttachment",
                "Properties": {
                    "VpcId": { "Ref": "Vpc" },
                    "InternetGatewayId": { "Ref": "Igw" }
                }
            },
            "Subnet": {
                "Type": "AWS::EC2::Subnet",
                "Properties": {
                    "VpcId": { "Ref": "Vpc" },
                    "CidrBlock": "10.0.1.0/24"
                }
            },
            "InstanceSg": {
                "Type": "AWS::EC2::SecurityGroup",
                "Properties": {
                    "GroupDescription": "the instances of the environment",
                    "VpcId": { "Ref": "Vpc" }
                }
            },
            "InstanceRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "ec2.amazonaws.com" },
                            "Action": "sts:AssumeRole"
                        }]
                    }
                }
            },
            "InstanceProfile": {
                "Type": "AWS::IAM::InstanceProfile",
                "Properties": { "Roles": [{ "Ref": "InstanceRole" }] }
            },
            "Application": { "Type": "AWS::ElasticBeanstalk::Application" },
            "Environment": {
                "Type": "AWS::ElasticBeanstalk::Environment",
                "Properties": {
                    "ApplicationName": { "Ref": "Application" },
                    "SolutionStackName": stack,
                    "OptionSettings": settings
                }
            }
        },
        "Outputs": {
            "ApplicationName": { "Value": { "Ref": "Application" } },
            "EnvironmentName": { "Value": { "Ref": "Environment" } }
        }
    })
    .to_string()
}

/// Look again until `stack_name` leaves `CREATE_IN_PROGRESS`, and answer the
/// `DescribeStacks` body that reported it. The CONDITION is what is waited on.
async fn settled(router: &MultiServiceRouter, stack_name: &str) -> String {
    let started = Instant::now();
    loop {
        let (status, described) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStacks",
            &[("StackName", stack_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "expected HTTP 200 from DescribeStacks, got {status} with body {described}"
        );
        if !opt_element(&described, "StackStatus")
            .unwrap_or_default()
            .ends_with("_IN_PROGRESS")
        {
            return described;
        }
        assert!(
            started.elapsed() < DEADLINE,
            "the stack never settled, last body {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every `(LogicalResourceId, ResourceStatus, ResourceStatusReason)` the stack's events
/// carry a failure in, which is what names the declaration a rolled-back stack choked on.
async fn failures(router: &MultiServiceRouter, stack_name: &str) -> Vec<String> {
    let (_, events) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "DescribeStackEvents",
        &[("StackName", stack_name)],
    )
    .await;
    super::query_wire::members_of(&events)
        .into_iter()
        .filter(|member| {
            opt_element(member, "ResourceStatus")
                .unwrap_or_default()
                .ends_with("_FAILED")
        })
        .map(|member| {
            format!(
                "{}: {}",
                opt_element(member, "LogicalResourceId").unwrap_or_default(),
                opt_element(member, "ResourceStatusReason").unwrap_or_default()
            )
        })
        .collect()
}

/// RULE: every option the published reference defines is one a stack may declare, so the
/// stack reaches `CREATE_COMPLETE` and the environment Elastic Beanstalk then holds carries
/// each declared value.
#[tokio::test]
async fn a_stack_declaring_the_published_option_settings_is_created() {
    let router = build_router();
    let stack_name = "eb-option-settings-stack";
    let (status, created) = query(
        &router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", stack_name),
            ("TemplateBody", &template()),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from CreateStack, got {status} with body {created}"
    );

    let described = settled(&router, stack_name).await;
    let stack_status = opt_element(&described, "StackStatus").unwrap_or_default();
    assert_eq!(
        stack_status,
        "CREATE_COMPLETE",
        "expected the stack to be created, got {stack_status:?}; failures {:?}",
        failures(&router, stack_name).await
    );

    // The environment really exists in the service that owns it, and holds what the template
    // declared: the stack path and the API path write one configuration.
    let environment = super::query_wire::outputs_of(&described)
        .get("EnvironmentName")
        .cloned()
        .unwrap_or_default();
    assert!(
        !environment.is_empty(),
        "expected the stack to publish the environment's name, got body {described}"
    );
    let application = super::query_wire::outputs_of(&described)
        .get("ApplicationName")
        .cloned()
        .unwrap_or_default();
    let (status, settings) = query(
        &router,
        "elasticbeanstalk",
        EB_API_VERSION,
        "DescribeConfigurationSettings",
        &[
            ("ApplicationName", &application),
            ("EnvironmentName", &environment),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from DescribeConfigurationSettings, got {status} with body {settings}"
    );
    for (namespace, option_name, value) in DECLARED_OPTIONS {
        let carried = super::query_wire::members_of(&settings).into_iter().any(|member| {
            opt_element(member, "Namespace").as_deref() == Some(*namespace)
                && opt_element(member, "OptionName").as_deref() == Some(*option_name)
                && opt_element(member, "Value").as_deref() == Some(*value)
        });
        assert!(
            carried,
            "case {namespace:?}/{option_name:?}: expected the environment's configuration to \
             carry {value:?}, got {}",
            elements(&settings, "OptionName").join(", ")
        );
    }
}
