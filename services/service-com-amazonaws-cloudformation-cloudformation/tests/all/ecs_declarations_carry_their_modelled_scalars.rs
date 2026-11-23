// SPDX-License-Identifier: CC0-1.0

//! The TYPES a declared `AWS::ECS::*` property value reaches Amazon ECS as.
//!
//! RULE under test: **a property value a stack sends to the service that owns the resource
//! carries the type that service's own model gives the member it is passed to** — so a scalar
//! the template states as TEXT, which is every value that reaches a property through a
//! template parameter ("all property values in a template are strings", and a parameter's
//! value travels as text whatever `Type` its declaration states), arrives at
//! `CreateService$desiredCount` as the integer that member is modelled as and at
//! `CreateService$enableECSManagedTags` as the boolean that one is, while a member the model
//! types as text — a task definition's own `cpu` and `memory`, "a string using CPU units" —
//! keeps the text it was written as.
//!
//! The defect this pins: a Fargate/`awsvpc` stack whose `DesiredCount` came from a `Number`
//! parameter had that text sent to `CreateService` verbatim, Amazon ECS refused the create,
//! and the whole stack rolled back instead of reaching `CREATE_COMPLETE` — so the service was
//! never created and the tasks its desired count asks for never existed.
//!
//! The rows span what the rule generalises over: whether the value arrives through a template
//! PARAMETER (declared `Number`, `String` or an AWS-specific id type) or is quoted in the
//! template itself, the modelled type it reaches (integer, boolean, and the text a size keeps),
//! how deep in the request the member stands (the request's own member, a member of a nested
//! structure, a member of a list entry), which call carries it (`CreateService` and
//! `RegisterTaskDefinition`) and the launch type the service runs under. Every case drives the
//! real request path — the CloudFormation calls over the awsQuery wire, the Amazon ECS reads
//! over `awsJson1_1` — so what is asserted is what a client that deployed the template and
//! then read the resources back observes.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudwatch_alarm_and_ecs_service_resources::ecs;
use crate::dynamodb_table_resources::failure_reasons;
use crate::stack_lifecycle::{
    acknowledging_iam, create_stack, ec2, output_value, router, settled_stack, xml_text,
};

/// How long a case waits for the tasks a service's desired count owes, and how often it looks
/// again. The wait ends on the CONDITION; the deadline is only here so a count that never
/// materialises fails the case instead of hanging it.
const DEADLINE: Duration = Duration::from_secs(60);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(50);

/// One row: the stack it deploys, the template's own parameter and resource declarations, and
/// what Amazon ECS must then report.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The `Parameters` section the template declares, as YAML lines.
    parameters: &'static str,
    /// The `AWS::ECS::TaskDefinition` properties, as YAML lines indented six spaces.
    task_definition: &'static str,
    /// The `AWS::ECS::Service` properties, as YAML lines indented six spaces.
    service: &'static str,
    /// The `(key, value)` parameter pairs the create supplies, beside the subnet and the
    /// security group every row supplies from the network it made.
    supplied: &'static [(&'static str, &'static str)],
    /// The `desiredCount` Amazon ECS must hold for the service, as the integer it models.
    desired_count: u64,
    /// Whether the tasks that count asks for must materialise. A row that asks for none has
    /// none to wait for.
    materialises: bool,
    /// The `(member, value)` pairs `DescribeServices` must report for the service, read at the
    /// top level of its record.
    service_members: &'static [(&'static str, &'static str)],
    /// The `(member, value)` pairs the service's `deploymentConfiguration` must report.
    deployment_members: &'static [(&'static str, &'static str)],
    /// The `(member, value)` pairs `DescribeTaskDefinition` must report for the revision.
    revision_members: &'static [(&'static str, &'static str)],
    /// The `(member, value)` pairs its first container definition must report.
    container_members: &'static [(&'static str, &'static str)],
}

/// Each expected value is spelled as the JSON it must BE, so a row states the type as well as
/// the value: `2` is the integer, `"256"` the text, `true` the flag.
const ROWS: &[Row] = &[
    Row {
        case: "a Fargate service whose task count arrives through a Number parameter",
        stack: "ecs-modelled-number-parameter",
        parameters: r#"  DesiredCount:
    Type: Number
  Subnet1:
    Type: AWS::EC2::Subnet::Id
  SecurityGroupId:
    Type: AWS::EC2::SecurityGroup::Id
"#,
        task_definition: r#"      RequiresCompatibilities:
        - FARGATE
      NetworkMode: awsvpc
      Cpu: '256'
      Memory: '512'
      ContainerDefinitions:
        - Name: app
          Image: public.ecr.aws/docker/library/busybox:latest
          Essential: true
"#,
        service: r#"      DesiredCount: !Ref DesiredCount
      LaunchType: FARGATE
      NetworkConfiguration:
        AwsvpcConfiguration:
          Subnets:
            - !Ref Subnet1
          SecurityGroups:
            - !Ref SecurityGroupId
          AssignPublicIp: ENABLED
"#,
        supplied: &[("DesiredCount", "2")],
        desired_count: 2,
        materialises: true,
        service_members: &[("launchType", "\"FARGATE\"")],
        deployment_members: &[],
        revision_members: &[("cpu", "\"256\""), ("memory", "\"512\"")],
        container_members: &[("essential", "true")],
    },
    Row {
        case: "quoted scalars written in the template itself, and a flag through a String \
               parameter",
        stack: "ecs-modelled-quoted-literals",
        parameters: r#"  ManagedTags:
    Type: String
  Subnet1:
    Type: AWS::EC2::Subnet::Id
  SecurityGroupId:
    Type: AWS::EC2::SecurityGroup::Id
"#,
        task_definition: r#"      RequiresCompatibilities:
        - FARGATE
      NetworkMode: awsvpc
      Cpu: '512'
      Memory: '1024'
      EphemeralStorage:
        SizeInGiB: '42'
      ContainerDefinitions:
        - Name: app
          Image: public.ecr.aws/docker/library/busybox:latest
          Essential: 'true'
          StopTimeout: '30'
"#,
        service: r#"      DesiredCount: '3'
      LaunchType: FARGATE
      EnableECSManagedTags: !Ref ManagedTags
      DeploymentConfiguration:
        MaximumPercent: '200'
        MinimumHealthyPercent: '100'
      NetworkConfiguration:
        AwsvpcConfiguration:
          Subnets:
            - !Ref Subnet1
          SecurityGroups:
            - !Ref SecurityGroupId
          AssignPublicIp: ENABLED
"#,
        supplied: &[("ManagedTags", "true")],
        desired_count: 3,
        materialises: true,
        service_members: &[("enableECSManagedTags", "true")],
        deployment_members: &[("maximumPercent", "200"), ("minimumHealthyPercent", "100")],
        revision_members: &[("cpu", "\"512\""), ("memory", "\"1024\"")],
        container_members: &[("essential", "true"), ("stopTimeout", "30")],
    },
    Row {
        case: "a container's own reservations through parameters, beside the task size that \
               stays text",
        stack: "ecs-modelled-container-reservations",
        parameters: r#"  ContainerCpu:
    Type: String
  ContainerMemory:
    Type: Number
  TaskCpu:
    Type: String
  Subnet1:
    Type: AWS::EC2::Subnet::Id
  SecurityGroupId:
    Type: AWS::EC2::SecurityGroup::Id
"#,
        task_definition: r#"      NetworkMode: bridge
      Cpu: !Ref TaskCpu
      Memory: '512'
      ContainerDefinitions:
        - Name: app
          Image: public.ecr.aws/docker/library/busybox:latest
          Cpu: !Ref ContainerCpu
          Memory: !Ref ContainerMemory
          MemoryReservation: '64'
          Essential: 'true'
          PortMappings:
            - ContainerPort: '8080'
              HostPort: '0'
              Protocol: tcp
"#,
        service: r#"      DesiredCount: '0'
      LaunchType: EC2
"#,
        supplied: &[
            ("ContainerCpu", "128"),
            ("ContainerMemory", "256"),
            ("TaskCpu", "256"),
        ],
        desired_count: 0,
        materialises: false,
        service_members: &[("launchType", "\"EC2\"")],
        deployment_members: &[],
        revision_members: &[("cpu", "\"256\""), ("memory", "\"512\"")],
        container_members: &[
            ("cpu", "128"),
            ("memory", "256"),
            ("memoryReservation", "64"),
            ("essential", "true"),
        ],
    },
];

/// The template one row deploys: the cluster, the revision its tasks run and the service that
/// keeps them, plus the outputs a client reads the three back by.
fn template(row: &Row) -> String {
    format!(
        "Parameters:\n{parameters}Resources:\n  Cluster:\n    Type: AWS::ECS::Cluster\n    \
         Properties:\n      ClusterName: {stack}-cluster\n  TaskDef:\n    Type: \
         AWS::ECS::TaskDefinition\n    Properties:\n      Family: {stack}-family\n\
         {task_definition}  Service:\n    Type: AWS::ECS::Service\n    Properties:\n      \
         Cluster: !Ref Cluster\n      ServiceName: {stack}-service\n      TaskDefinition: \
         !Ref TaskDef\n{service}Outputs:\n  ClusterArn:\n    Value: !GetAtt Cluster.Arn\n  \
         ServiceName:\n    Value: !GetAtt Service.Name\n  TaskDefArn:\n    Value: !Ref \
         TaskDef\n",
        parameters = row.parameters,
        stack = row.stack,
        task_definition = row.task_definition,
        service = row.service,
    )
}

/// The subnet and the security group a row's service is placed in: the account's DEFAULT VPC
/// and a security group of it, picked out of band through Amazon EC2's own API the way a
/// client picks an existing placement before it deploys the stack, and supplied to the stack
/// as parameter values.
///
/// The default VPC is the placement because it comes with the internet path AWS gives it (an
/// internet gateway, and a main route table that sends internet-bound traffic to it), which is
/// what a task with a public address fetches its container image over — so a row whose desired
/// count has to MATERIALISE is placed somewhere its tasks can really start.
pub(crate) async fn placement(router: &MultiServiceRouter, case: &str) -> (String, String) {
    let described = ec2(
        router,
        "DescribeVpcs",
        &[("Filter.1.Name", "isDefault"), ("Filter.1.Value.1", "true")],
    )
    .await
    .1;
    let vpc = xml_text(&described, "vpcId").unwrap_or_else(|| {
        panic!("case {case:?}: expected the account to hold a default VPC, got {described}")
    });
    let subnets = ec2(
        router,
        "DescribeSubnets",
        &[("Filter.1.Name", "vpc-id"), ("Filter.1.Value.1", &vpc)],
    )
    .await
    .1;
    let subnet = xml_text(&subnets, "subnetId").unwrap_or_else(|| {
        panic!("case {case:?}: expected the default VPC to hold a subnet, got {subnets}")
    });
    let grouped = ec2(
        router,
        "CreateSecurityGroup",
        &[
            ("VpcId", &vpc),
            ("GroupName", "modelled-scalars"),
            ("GroupDescription", "the placement a declared service runs in"),
        ],
    )
    .await
    .1;
    let group = xml_text(&grouped, "groupId").unwrap_or_else(|| {
        panic!("case {case:?}: expected Amazon EC2 to mint a security group, got {grouped}")
    });
    (subnet, group)
}

/// One record's member, as the JSON value it IS — so an assertion states the TYPE as well as
/// the value.
pub(crate) fn member(record: &Value, name: &str) -> String {
    record
        .get(name)
        .cloned()
        .unwrap_or(Value::Null)
        .to_string()
}

#[tokio::test]
async fn a_declared_ecs_property_reaches_amazon_ecs_as_the_type_its_model_declares() {
    for row in ROWS.iter() {
        let case = row.case;
        let router = router();
        let (subnet, group) = placement(&router, case).await;

        let mut extra = acknowledging_iam(&[]);
        let mut supplied: Vec<(String, String)> = row
            .supplied
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        supplied.push(("Subnet1".to_string(), subnet.clone()));
        supplied.push(("SecurityGroupId".to_string(), group.clone()));
        for (position, (key, value)) in supplied.into_iter().enumerate() {
            let slot = position + 1;
            extra.push((format!("Parameters.member.{slot}.ParameterKey"), key));
            extra.push((format!("Parameters.member.{slot}.ParameterValue"), value));
        }

        let (status, body) = create_stack(&router, row.stack, &template(row), &extra).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {body}"
        );
        let described = settled_stack(&router, case, row.stack).await;
        let reported = xml_text(&described, "StackStatus");
        assert_eq!(
            reported.as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to create, got {reported:?} because {:?}",
            failure_reasons(&router, row.stack).await
        );

        let cluster = format!("{}-cluster", row.stack);
        let service_name = output_value(&described, "ServiceName").unwrap_or_default();
        assert_eq!(
            service_name,
            format!("{}-service", row.stack),
            "case {case:?}: expected the stack to publish the service it declared, got \
             {service_name:?}"
        );

        // What Amazon ECS itself holds for the service: the desired count as the INTEGER its
        // model declares, and each other member as the type that model gives it.
        let (code, document) = ecs(
            &router,
            "DescribeServices",
            &json!({"cluster": &cluster, "services": [&service_name]}),
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected HTTP 200 from DescribeServices, got {code} with {document}"
        );
        let service = document["services"][0].clone();
        assert_eq!(
            service["desiredCount"].to_string(),
            row.desired_count.to_string(),
            "case {case:?}: expected the desired count {} as the integer the model declares, \
             got {service}",
            row.desired_count
        );
        for (name, expected) in row.service_members {
            let held = member(&service, name);
            assert_eq!(
                held, *expected,
                "case {case:?}: expected the service's {name} to be {expected}, got {held}"
            );
        }
        let deployment = service["deploymentConfiguration"].clone();
        for (name, expected) in row.deployment_members {
            let held = member(&deployment, name);
            assert_eq!(
                held, *expected,
                "case {case:?}: expected the deployment configuration's {name} to be \
                 {expected}, got {held}"
            );
        }

        // ...and for the revision its tasks run.
        let revision = output_value(&described, "TaskDefArn").unwrap_or_default();
        let (code, defined) = ecs(
            &router,
            "DescribeTaskDefinition",
            &json!({"taskDefinition": &revision}),
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected HTTP 200 from DescribeTaskDefinition, got {code} with \
             {defined}"
        );
        let held_revision = defined["taskDefinition"].clone();
        for (name, expected) in row.revision_members {
            let held = member(&held_revision, name);
            assert_eq!(
                held, *expected,
                "case {case:?}: expected the revision's {name} to be {expected}, got {held}"
            );
        }
        let container = held_revision["containerDefinitions"][0].clone();
        for (name, expected) in row.container_members {
            let held = member(&container, name);
            assert_eq!(
                held, *expected,
                "case {case:?}: expected the container definition's {name} to be {expected}, \
                 got {held}"
            );
        }

        // The count is the service scheduler's to materialise, so a row that asks for tasks
        // waits for them the way a client polling `ListTasks` does.
        if !row.materialises {
            continue;
        }
        let deadline = Instant::now() + DEADLINE;
        let mut listed = running_task_count(&router, case, &cluster, &service_name).await;
        while listed != row.desired_count && Instant::now() < deadline {
            ::tokio::time::sleep(LOOK_AGAIN_AFTER).await;
            listed = running_task_count(&router, case, &cluster, &service_name).await;
        }
        assert_eq!(
            listed, row.desired_count,
            "case {case:?}: expected the declared desired count to materialise {} RUNNING \
             task(s), got {listed}",
            row.desired_count
        );
    }
}

/// How many RUNNING tasks the service keeps, as a client reads them: `ListTasks` narrowed to
/// the service and the desired status, then `DescribeTasks` over what it answered.
pub(crate) async fn running_task_count(
    router: &MultiServiceRouter,
    case: &str,
    cluster: &str,
    service: &str,
) -> u64 {
    let (code, listed) = ecs(
        router,
        "ListTasks",
        &json!({"cluster": cluster, "serviceName": service, "desiredStatus": "RUNNING"}),
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected HTTP 200 from ListTasks, got {code} with {listed}"
    );
    let arns = listed["taskArns"].as_array().cloned().unwrap_or_default();
    if arns.is_empty() {
        return 0;
    }
    let (code, described) = ecs(
        router,
        "DescribeTasks",
        &json!({"cluster": cluster, "tasks": arns}),
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected HTTP 200 from DescribeTasks, got {code} with {described}"
    );
    described["tasks"]
        .as_array()
        .map(|tasks| {
            tasks
                .iter()
                .filter(|task| task["lastStatus"].as_str() == Some("RUNNING"))
                .count() as u64
        })
        .unwrap_or_default()
}
