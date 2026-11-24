// SPDX-License-Identifier: CC0-1.0

//! Every configuration option Elastic Beanstalk's published option reference defines in a
//! namespace this service's catalogue DESCRIBES resolves, driven end to end over the
//! service's awsQuery surface.
//!
//! RULE: a namespace the catalogue describes any option of states that namespace's option
//! NAMES, so a specification naming an option the catalogue leaves out of such a namespace is
//! reported as naming no configuration setting at all
//! (`all/configuration_setting_validation.rs` is the suite that owns that refusal). The
//! catalogue a platform version publishes therefore has to carry every option the platform
//! DEFINES in those namespaces — not only the ones this emulator derives behaviour from —
//! because an option left out is a configuration the real service accepts and this one
//! refuses. A whole class of callers sends exactly those settings: a CloudFormation
//! `AWS::ElasticBeanstalk::Environment` carries its `OptionSettings` straight into
//! `CreateEnvironment`, so one unlisted option name fails the stack.
//!
//! The rows below are the option reference's own "general options for all environments"
//! (`https://docs.aws.amazon.com/elasticbeanstalk/latest/dg/command-options-general.html`),
//! one per option of every namespace the catalogue describes, each with a value that option
//! takes. The source is named here because the list is written by hand: the catalogue is what
//! the emulator publishes, so the list it is checked against cannot be read from it.
//!
//! Two halves, because both surfaces read the same resolution: `ValidateConfigurationSettings`
//! REPORTS an unresolved specification, and `CreateEnvironment` REFUSES a configuration
//! carrying one.
//!
//! The HTTP harness (`Emulator`, `Answer`) is `all/lifecycle.rs`'s and the option-setting
//! encoder `all/configuration_templates.rs`'s, so this suite keeps no second copy of either.

use crate::configuration_templates::{
    as_params, created, option_params, published_stack, published_stack_of_category,
};
use crate::harness::{Answer, Emulator};

/// The application the configuration sets below belong to.
const APPLICATION: &str = "general-options-app";
/// The stored configuration template the settings are validated against.
const TEMPLATE: &str = "general-options-template";

/// The fixed half of the sentence an unresolved specification is reported with, which is
/// what these rows must NOT earn.
const UNKNOWN_SETTING: &str = "Unknown configuration setting.";

/// One option of the published reference: where it lives, what it is called, and a value it
/// takes.
type Option3 = (&'static str, &'static str, &'static str);

/// Every option the reference defines in the namespaces this service's catalogue describes,
/// with a value each one takes.
///
/// Source: Elastic Beanstalk's "general options for all environments" reference. The
/// namespaces whose option names the CALLER chooses (the environment properties, the template
/// parameters) and the ones scoped to a resource of their own are not here: the catalogue
/// describes no option of those, so it states none of their names and resolves whatever a
/// setting in one is called — `all/configuration_setting_validation.rs` covers that half.
const GENERAL_OPTIONS: &[Option3] = &[
    // aws:autoscaling:asg — the Auto Scaling group.
    ("aws:autoscaling:asg", "Availability Zones", "Any"),
    ("aws:autoscaling:asg", "Cooldown", "360"),
    ("aws:autoscaling:asg", "Custom Availability Zones", "us-west-2a"),
    ("aws:autoscaling:asg", "MaxSize", "4"),
    ("aws:autoscaling:asg", "MinSize", "1"),
    // aws:autoscaling:launchconfiguration — the instances themselves.
    (
        "aws:autoscaling:launchconfiguration",
        "BlockDeviceMappings",
        "/dev/sdj=:100:true:gp2",
    ),
    (
        "aws:autoscaling:launchconfiguration",
        "DisableDefaultEC2SecurityGroup",
        "true",
    ),
    ("aws:autoscaling:launchconfiguration", "DisableIMDSv1", "true"),
    ("aws:autoscaling:launchconfiguration", "EC2KeyName", "deploy-key"),
    (
        "aws:autoscaling:launchconfiguration",
        "IamInstanceProfile",
        "aws-elasticbeanstalk-ec2-role",
    ),
    (
        "aws:autoscaling:launchconfiguration",
        "ImageId",
        "ami-0a1b2c3d4e5f60718",
    ),
    ("aws:autoscaling:launchconfiguration", "InstanceType", "t3.small"),
    (
        "aws:autoscaling:launchconfiguration",
        "MonitoringInterval",
        "1 minute",
    ),
    ("aws:autoscaling:launchconfiguration", "RootVolumeIOPS", "3000"),
    ("aws:autoscaling:launchconfiguration", "RootVolumeSize", "20"),
    (
        "aws:autoscaling:launchconfiguration",
        "RootVolumeThroughput",
        "125",
    ),
    ("aws:autoscaling:launchconfiguration", "RootVolumeType", "gp3"),
    (
        "aws:autoscaling:launchconfiguration",
        "SecurityGroups",
        "sg-0a1b2c3d4e5f60718",
    ),
    (
        "aws:autoscaling:launchconfiguration",
        "SSHSourceRestriction",
        "tcp,22,22,10.0.0.0/8",
    ),
    // aws:autoscaling:updatepolicy:rollingupdate — how a change rolls across the group.
    (
        "aws:autoscaling:updatepolicy:rollingupdate",
        "MaxBatchSize",
        "1",
    ),
    (
        "aws:autoscaling:updatepolicy:rollingupdate",
        "MinInstancesInService",
        "1",
    ),
    (
        "aws:autoscaling:updatepolicy:rollingupdate",
        "PauseTime",
        "PT30S",
    ),
    (
        "aws:autoscaling:updatepolicy:rollingupdate",
        "RollingUpdateEnabled",
        "true",
    ),
    (
        "aws:autoscaling:updatepolicy:rollingupdate",
        "RollingUpdateType",
        "Health",
    ),
    (
        "aws:autoscaling:updatepolicy:rollingupdate",
        "Timeout",
        "PT30M",
    ),
    // aws:ec2:instances — the instance types the group may launch.
    ("aws:ec2:instances", "EnableSpot", "false"),
    ("aws:ec2:instances", "InstanceTypes", "t3.micro,t3.small"),
    ("aws:ec2:instances", "SpotFleetOnDemandBase", "0"),
    (
        "aws:ec2:instances",
        "SpotFleetOnDemandAboveBasePercentage",
        "0",
    ),
    ("aws:ec2:instances", "SpotMaxPrice", "0.05"),
    ("aws:ec2:instances", "SupportedArchitectures", "x86_64"),
    // aws:ec2:vpc — where the environment runs.
    ("aws:ec2:vpc", "AssociatePublicIpAddress", "true"),
    ("aws:ec2:vpc", "DBSubnets", "subnet-0a1b2c3d4e5f60718"),
    ("aws:ec2:vpc", "ELBScheme", "public"),
    ("aws:ec2:vpc", "ELBSubnets", "subnet-0a1b2c3d4e5f60718"),
    ("aws:ec2:vpc", "Subnets", "subnet-0a1b2c3d4e5f60718"),
    ("aws:ec2:vpc", "VPCId", "vpc-0a1b2c3d4e5f60718"),
    // aws:elasticbeanstalk:application — the application's health check path.
    (
        "aws:elasticbeanstalk:application",
        "Application Healthcheck URL",
        "/health",
    ),
    // aws:elasticbeanstalk:cloudwatch:logs — instance log streaming.
    (
        "aws:elasticbeanstalk:cloudwatch:logs",
        "DeleteOnTerminate",
        "false",
    ),
    ("aws:elasticbeanstalk:cloudwatch:logs", "RetentionInDays", "14"),
    ("aws:elasticbeanstalk:cloudwatch:logs", "StreamLogs", "true"),
    // aws:elasticbeanstalk:cloudwatch:logs:health — environment health streaming.
    (
        "aws:elasticbeanstalk:cloudwatch:logs:health",
        "DeleteOnTerminate",
        "false",
    ),
    (
        "aws:elasticbeanstalk:cloudwatch:logs:health",
        "HealthStreamingEnabled",
        "true",
    ),
    (
        "aws:elasticbeanstalk:cloudwatch:logs:health",
        "RetentionInDays",
        "7",
    ),
    // aws:elasticbeanstalk:command — how a version is deployed.
    ("aws:elasticbeanstalk:command", "BatchSize", "30"),
    ("aws:elasticbeanstalk:command", "BatchSizeType", "Percentage"),
    ("aws:elasticbeanstalk:command", "DeploymentPolicy", "Rolling"),
    ("aws:elasticbeanstalk:command", "IgnoreHealthCheck", "false"),
    ("aws:elasticbeanstalk:command", "Timeout", "600"),
    // aws:elasticbeanstalk:environment — the environment itself.
    (
        "aws:elasticbeanstalk:environment",
        "EnvironmentType",
        "LoadBalanced",
    ),
    (
        "aws:elasticbeanstalk:environment",
        "ExternalExtensionsS3Bucket",
        "deploy-extensions",
    ),
    (
        "aws:elasticbeanstalk:environment",
        "ExternalExtensionsS3Key",
        "extensions.zip",
    ),
    (
        "aws:elasticbeanstalk:environment",
        "LoadBalancerType",
        "classic",
    ),
    (
        "aws:elasticbeanstalk:environment",
        "ServiceRole",
        "aws-elasticbeanstalk-service-role",
    ),
    // aws:elasticbeanstalk:healthreporting:system — basic or enhanced health.
    (
        "aws:elasticbeanstalk:healthreporting:system",
        "ConfigDocument",
        "{\"Version\":1}",
    ),
    (
        "aws:elasticbeanstalk:healthreporting:system",
        "EnhancedHealthAuthEnabled",
        "true",
    ),
    (
        "aws:elasticbeanstalk:healthreporting:system",
        "HealthCheckSuccessThreshold",
        "Ok",
    ),
    (
        "aws:elasticbeanstalk:healthreporting:system",
        "SystemType",
        "enhanced",
    ),
    // aws:elasticbeanstalk:hostmanager — instance log publication.
    (
        "aws:elasticbeanstalk:hostmanager",
        "LogPublicationControl",
        "false",
    ),
    // aws:elasticbeanstalk:managedactions — managed platform updates.
    (
        "aws:elasticbeanstalk:managedactions",
        "ManagedActionsEnabled",
        "false",
    ),
    (
        "aws:elasticbeanstalk:managedactions",
        "PreferredStartTime",
        "Tue:09:00",
    ),
    (
        "aws:elasticbeanstalk:managedactions",
        "ServiceRoleForManagedUpdates",
        "aws-elasticbeanstalk-service-role",
    ),
    (
        "aws:elasticbeanstalk:managedactions:platformupdate",
        "InstanceRefreshEnabled",
        "false",
    ),
    (
        "aws:elasticbeanstalk:managedactions:platformupdate",
        "UpdateLevel",
        "minor",
    ),
    // aws:elasticbeanstalk:monitoring — what the service does with a failing instance.
    (
        "aws:elasticbeanstalk:monitoring",
        "Automatically Terminate Unhealthy Instances",
        "true",
    ),
    // aws:elasticbeanstalk:sns:topics — environment event notifications.
    (
        "aws:elasticbeanstalk:sns:topics",
        "Notification Endpoint",
        "ops@example.com",
    ),
    (
        "aws:elasticbeanstalk:sns:topics",
        "Notification Protocol",
        "email",
    ),
    (
        "aws:elasticbeanstalk:sns:topics",
        "Notification Topic Name",
        "orders-env-events",
    ),
    // aws:elasticbeanstalk:xray — the X-Ray daemon.
    ("aws:elasticbeanstalk:xray", "XRayEnabled", "false"),
    // aws:elb:healthcheck / aws:elb:loadbalancer — the classic load balancer.
    ("aws:elb:healthcheck", "HealthyThreshold", "3"),
    ("aws:elb:healthcheck", "Interval", "30"),
    ("aws:elb:healthcheck", "Target", "HTTP:80/"),
    ("aws:elb:healthcheck", "Timeout", "5"),
    ("aws:elb:healthcheck", "UnhealthyThreshold", "5"),
    ("aws:elb:loadbalancer", "CrossZone", "true"),
    ("aws:elb:loadbalancer", "LoadBalancerHTTPPort", "80"),
    ("aws:elb:loadbalancer", "LoadBalancerHTTPSPort", "OFF"),
    ("aws:elb:loadbalancer", "LoadBalancerPortProtocol", "HTTP"),
    ("aws:elb:loadbalancer", "LoadBalancerSSLPortProtocol", "HTTPS"),
    (
        "aws:elb:loadbalancer",
        "ManagedSecurityGroup",
        "sg-0a1b2c3d4e5f60718",
    ),
    ("aws:elb:loadbalancer", "SecurityGroups", "sg-0a1b2c3d4e5f60718"),
];

/// The settings a CloudFormation `AWS::ElasticBeanstalk::Environment` of a virtual private
/// cloud carries: the network it runs in, the instances' profile and security group, and the
/// health reporting it asks for. The shape a stack really deploys, which is why the WRITE
/// half below is asserted on it.
const VPC_ENVIRONMENT_OPTIONS: &[Option3] = &[
    ("aws:ec2:vpc", "VPCId", "vpc-0a1b2c3d4e5f60718"),
    ("aws:ec2:vpc", "Subnets", "subnet-0a1b2c3d4e5f60718"),
    ("aws:ec2:vpc", "AssociatePublicIpAddress", "true"),
    (
        "aws:autoscaling:launchconfiguration",
        "IamInstanceProfile",
        "aws-elasticbeanstalk-ec2-role",
    ),
    (
        "aws:autoscaling:launchconfiguration",
        "SecurityGroups",
        "sg-0a1b2c3d4e5f60718",
    ),
    ("aws:autoscaling:launchconfiguration", "InstanceType", "t3.small"),
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
    ("aws:elasticbeanstalk:managedactions", "ManagedActionsEnabled", "false"),
];

/// One option setting in the form the shared encoder takes, with no `ResourceName`.
fn setting(option: &Option3) -> (&'static str, &'static str, &'static str, &'static str) {
    (option.0, option.1, option.2, "")
}

/// An emulator holding the application and the configuration template the rows are
/// validated against.
async fn emulator_with_a_template() -> Emulator {
    let emulator = Emulator::new();
    created(
        &emulator,
        "CreateApplication",
        &[("ApplicationName", APPLICATION)],
    )
    .await;
    created(
        &emulator,
        "CreateConfigurationTemplate",
        &[
            ("ApplicationName", APPLICATION),
            ("TemplateName", TEMPLATE),
            ("SolutionStackName", &published_stack(0).name()),
        ],
    )
    .await;
    emulator
}

/// Every message `ValidateConfigurationSettings` reported for `settings`.
async fn validated(emulator: &Emulator, settings: &[(&str, &str, &str, &str)]) -> Answer {
    let mut params = vec![
        ("ApplicationName".to_string(), APPLICATION.to_string()),
        ("TemplateName".to_string(), TEMPLATE.to_string()),
    ];
    params.extend(option_params(settings));
    created(
        emulator,
        "ValidateConfigurationSettings",
        &as_params(&params),
    )
    .await
}

/// RULE: every option the published reference defines in a described namespace RESOLVES, so
/// validating it reports no unknown-setting finding against it.
#[tokio::test]
async fn every_published_general_option_resolves() {
    let emulator = emulator_with_a_template().await;
    for option in GENERAL_OPTIONS {
        let case = format!("{} / {}", option.0, option.1);
        let answer = validated(&emulator, &[setting(option)]).await;
        let unresolved: Vec<String> = answer
            .elements_at("Messages/member")
            .iter()
            .filter_map(|member| member.value("Message").map(str::to_string))
            .filter(|message| message.contains(UNKNOWN_SETTING))
            .collect();
        assert!(
            unresolved.is_empty(),
            "case {case:?}: expected the published option to resolve, got {unresolved:?}"
        );
    }
}

/// The configuration template a static-file mapping is validated against: one of a PYTHON
/// platform version, whose catalogue describes that namespace at all — the platform the rule
/// is decidable on.
const PYTHON_TEMPLATE: &str = "general-options-python-template";

/// RULE: a namespace whose option NAME is the caller's own — a static-file mapping, named by
/// the virtual path it serves — states no set of names, so a mapping of any path resolves and
/// not only the one the platform version ships.
#[tokio::test]
async fn a_static_file_mapping_of_any_path_resolves() {
    let emulator = emulator_with_a_template().await;
    created(
        &emulator,
        "CreateConfigurationTemplate",
        &[
            ("ApplicationName", APPLICATION),
            ("TemplateName", PYTHON_TEMPLATE),
            (
                "SolutionStackName",
                &published_stack_of_category("Python").name(),
            ),
        ],
    )
    .await;
    // The rows differ along the path the mapping names: the one the platform version's own
    // catalogue carries, a sibling of it, a nested one, and one of another name entirely.
    let paths = ["/static", "/assets", "/static/img", "/public"];
    for path in paths {
        let option: Option3 = (
            "aws:elasticbeanstalk:container:python:staticfiles",
            path,
            "static",
        );
        let mut params = vec![
            ("ApplicationName".to_string(), APPLICATION.to_string()),
            ("TemplateName".to_string(), PYTHON_TEMPLATE.to_string()),
        ];
        params.extend(option_params(&[setting(&option)]));
        let answer = created(
            &emulator,
            "ValidateConfigurationSettings",
            &as_params(&params),
        )
        .await;
        let unresolved: Vec<String> = answer
            .elements_at("Messages/member")
            .iter()
            .filter_map(|member| member.value("Message").map(str::to_string))
            .filter(|message| message.contains(UNKNOWN_SETTING))
            .collect();
        assert!(
            unresolved.is_empty(),
            "case {path:?}: expected a mapping of that path to resolve, got {unresolved:?}"
        );
    }
}

/// RULE: the same resolution gates a WRITE, so `CreateEnvironment` accepts a configuration
/// of published options — the one a CloudFormation stack deploys — and the environment then
/// reports those values.
#[tokio::test]
async fn an_environment_accepts_a_configuration_of_published_options() {
    let emulator = emulator_with_a_template().await;
    let settings: Vec<(&str, &str, &str, &str)> =
        VPC_ENVIRONMENT_OPTIONS.iter().map(setting).collect();
    let mut params = vec![
        ("ApplicationName".to_string(), APPLICATION.to_string()),
        ("EnvironmentName".to_string(), "vpc-env".to_string()),
        (
            "SolutionStackName".to_string(),
            published_stack(0).name(),
        ),
    ];
    params.extend(option_params(&settings));
    let answer = emulator
        .call("CreateEnvironment", &as_params(&params))
        .await;
    assert_eq!(
        answer.status, 200,
        "expected the stack-shaped configuration to be accepted, got {} and body {}",
        answer.status, answer.body
    );

    let described = created(
        &emulator,
        "DescribeConfigurationSettings",
        &[
            ("ApplicationName", APPLICATION),
            ("EnvironmentName", "vpc-env"),
        ],
    )
    .await;
    let reported: Vec<(String, String, String)> = described
        .elements_at("ConfigurationSettings/member/OptionSettings/member")
        .iter()
        .map(|member| {
            (
                member.value("Namespace").unwrap_or_default().to_string(),
                member.value("OptionName").unwrap_or_default().to_string(),
                member.value("Value").unwrap_or_default().to_string(),
            )
        })
        .collect();
    for option in VPC_ENVIRONMENT_OPTIONS {
        let expected = (
            option.0.to_string(),
            option.1.to_string(),
            option.2.to_string(),
        );
        assert!(
            reported.contains(&expected),
            "case {:?}: expected the configuration to report {expected:?}, got {reported:?}",
            format!("{} / {}", option.0, option.1)
        );
    }
}
