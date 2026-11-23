// SPDX-License-Identifier: CC0-1.0

//! The TYPES a CHANGED `AWS::ECS::Service` declaration reaches `UpdateService` as.
//!
//! RULE under test: the rule the create path is held to by
//! `ecs_declarations_carry_their_modelled_scalars.rs` holds for the UPDATE a changed
//! declaration applies too — **a property value a stack sends to the service that owns the
//! resource carries the type that service's own model gives the member it is passed to**. A
//! task count supplied as a template PARAMETER is text on the wire whatever `Type` its
//! declaration states, and `UpdateService$desiredCount` is an integer, so the second deploy
//! reaches that call as the integer and the service really scales rather than the update
//! failing and the stack rolling back.
//!
//! One case, deliberately: what it adds to the create suite is the OTHER call the same
//! catalogue is read for, driven over the same awsQuery/`awsJson1_1` wires — a changed
//! parameter value, nothing else about the template different, so the only thing the second
//! deploy can fail on is the type that value travels as.

use std::time::{Duration, Instant};

use serde_json::json;

use crate::cloudwatch_alarm_and_ecs_service_resources::ecs;
use crate::dynamodb_table_resources::failure_reasons;
use crate::ecs_declarations_carry_their_modelled_scalars::{
    member, placement, running_task_count,
};
use crate::stack_lifecycle::{
    acknowledging_iam, call, create_stack, output_value, router, settled_stack, xml_text,
};

/// The stack the case deploys and then updates, and the names its resources take.
const STACK: &str = "ecs-modelled-scalar-update";

/// How long the case waits for the tasks the new count owes, and how often it looks again.
const DEADLINE: Duration = Duration::from_secs(60);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(50);

/// The template both deploys use: a Fargate service whose task count is the one the `Number`
/// parameter states.
const TEMPLATE: &str = r#"Parameters:
  DesiredCount:
    Type: Number
  Subnet1:
    Type: AWS::EC2::Subnet::Id
  SecurityGroupId:
    Type: AWS::EC2::SecurityGroup::Id
Resources:
  Cluster:
    Type: AWS::ECS::Cluster
    Properties:
      ClusterName: ecs-modelled-scalar-update-cluster
  TaskDef:
    Type: AWS::ECS::TaskDefinition
    Properties:
      Family: ecs-modelled-scalar-update-family
      RequiresCompatibilities:
        - FARGATE
      NetworkMode: awsvpc
      Cpu: '256'
      Memory: '512'
      ContainerDefinitions:
        - Name: app
          Image: public.ecr.aws/docker/library/busybox:latest
          Essential: true
  Service:
    Type: AWS::ECS::Service
    Properties:
      Cluster: !Ref Cluster
      ServiceName: ecs-modelled-scalar-update-service
      TaskDefinition: !Ref TaskDef
      DesiredCount: !Ref DesiredCount
      LaunchType: FARGATE
      NetworkConfiguration:
        AwsvpcConfiguration:
          Subnets:
            - !Ref Subnet1
          SecurityGroups:
            - !Ref SecurityGroupId
          AssignPublicIp: ENABLED
Outputs:
  ServiceName:
    Value: !GetAtt Service.Name
"#;

/// The `Parameters.member.N.*` pairs one deploy carries, for the task count it asks for and
/// the placement the service runs in.
fn supplied(count: &str, subnet: &str, group: &str) -> Vec<(String, String)> {
    let mut members = acknowledging_iam(&[]);
    for (position, (key, value)) in [
        ("DesiredCount", count),
        ("Subnet1", subnet),
        ("SecurityGroupId", group),
    ]
    .into_iter()
    .enumerate()
    {
        let slot = position + 1;
        members.push((
            format!("Parameters.member.{slot}.ParameterKey"),
            key.to_string(),
        ));
        members.push((
            format!("Parameters.member.{slot}.ParameterValue"),
            value.to_string(),
        ));
    }
    members
}

/// The `desiredCount` Amazon ECS holds for the stack's service, as the JSON value it IS.
async fn held_desired_count(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    service: &str,
) -> String {
    let (code, document) = ecs(
        router,
        "DescribeServices",
        &json!({"cluster": "ecs-modelled-scalar-update-cluster", "services": [service]}),
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected HTTP 200 from DescribeServices, got {code} with {document}"
    );
    member(&document["services"][0], "desiredCount")
}

#[tokio::test]
async fn a_changed_declaration_reaches_update_service_as_the_type_its_model_declares() {
    let case = "a task count raised through a changed Number parameter";
    let router = router();
    let (subnet, group) = placement(&router, case).await;

    let (status, body) = create_stack(
        &router,
        STACK,
        TEMPLATE,
        &supplied("1", &subnet, &group)[..],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {body}"
    );
    let described = settled_stack(&router, case, STACK).await;
    let reported = xml_text(&described, "StackStatus");
    assert_eq!(
        reported.as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to create, got {reported:?} because {:?}",
        failure_reasons(&router, STACK).await
    );
    let service = output_value(&described, "ServiceName").unwrap_or_default();
    let created = held_desired_count(&router, case, &service).await;
    assert_eq!(
        created, "1",
        "case {case:?}: expected the first deploy's count as the integer the model declares, \
         got {created}"
    );

    // The SAME template, with the parameter stating a different count: what reaches
    // `UpdateService` is that count as the integer its member is modelled as.
    let mut members = vec![
        ("StackName".to_string(), STACK.to_string()),
        ("TemplateBody".to_string(), TEMPLATE.to_string()),
    ];
    members.extend(supplied("3", &subnet, &group));
    let (status, body) = call(&router, "UpdateStack", &members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with {body}"
    );
    let updated = settled_stack(&router, case, STACK).await;
    let reported = xml_text(&updated, "StackStatus");
    assert_eq!(
        reported.as_deref(),
        Some("UPDATE_COMPLETE"),
        "case {case:?}: expected the update to complete, got {reported:?} because {:?}",
        failure_reasons(&router, STACK).await
    );
    let held = held_desired_count(&router, case, &service).await;
    assert_eq!(
        held, "3",
        "case {case:?}: expected the changed count in force as the integer the model \
         declares, got {held}"
    );

    // ...and the scheduler keeps that many tasks, which is what the count is FOR.
    let deadline = Instant::now() + DEADLINE;
    let mut running = running_task_count(
        &router,
        case,
        "ecs-modelled-scalar-update-cluster",
        &service,
    )
    .await;
    while running != 3 && Instant::now() < deadline {
        ::tokio::time::sleep(LOOK_AGAIN_AFTER).await;
        running = running_task_count(
            &router,
            case,
            "ecs-modelled-scalar-update-cluster",
            &service,
        )
        .await;
    }
    assert_eq!(
        running, 3,
        "case {case:?}: expected the raised count to materialise 3 RUNNING tasks, got {running}"
    );
}
