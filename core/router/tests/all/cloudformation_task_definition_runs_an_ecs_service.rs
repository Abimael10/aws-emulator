// SPDX-License-Identifier: CC0-1.0

//! The task-definition handle a CloudFormation stack publishes is one Amazon ECS RESOLVES:
//! `CreateService` against it creates a service that runs that revision.
//!
//! The rule under test: **the physical id a stack reports for a declared resource is the
//! identifier the owning service assigned to a resource it really holds, so every call of that
//! service which takes such an identifier resolves it** — for an `AWS::ECS::TaskDefinition`
//! that identifier is the registered revision's ARN ("Ref returns the Amazon Resource Name
//! (ARN) of the task definition"), and `CreateService` takes "the family and revision
//! (`family:revision`) or full ARN of the task definition to run in your service". A stack that
//! reported the resource created while nothing registered the revision publishes a handle that
//! resolves to nothing, so the first ECS call a client makes with what the stack handed it is a
//! not-found that points at the caller rather than at the stack.
//!
//! Three crates have to agree for that to hold — CloudFormation makes the registration,
//! Amazon ECS serves it and resolves the handle afterwards, and Amazon EC2 and Elastic Load
//! Balancing own the network and the target group such a service is placed into and bound to —
//! so the suite lives here: a crate-local test can only ever see one side of the call.
//!
//! The rows differ along what the rule generalises over: which of the three handle forms the
//! ECS call spells the stack's revision as (the published ARN, `family:revision`, the family
//! alone), whether the template NAMES the family or leaves it to CloudFormation, the launch
//! type and network mode of the revision (Fargate over `awsvpc`, EC2 over `bridge`), whether
//! the service binds a load balancer the stack also declared, the desired count, and both
//! template spellings (JSON and YAML).
//!
//! The second test is the same rule read the other way: a well-formed handle of a revision
//! NOTHING registered is still refused, so what makes the stack's handle work is the state the
//! deployment wrote and not the shape of the string.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{
    elements, form_encode, members_of, opt_element, outputs_of, physical_ids_of,
};

/// The region every request here is signed for, and so the region every resource and ARN
/// below belongs to.
const REGION: &str = "us-east-1";

/// The account id every ARN this emulator mints carries.
const ACCOUNT_ID: &str = "000000000000";

/// The `X-Amz-Target` prefix Amazon ECS's model fixes (service shape
/// `AmazonEC2ContainerServiceV20141113`), whose `sigv4Name` is `ecs`. It speaks awsJson1.1.
const ECS_TARGET_PREFIX: &str = "AmazonEC2ContainerServiceV20141113";

/// The segment a task-definition ARN carries its family and revision after
/// (`arn:aws:ecs:<region>:<account>:task-definition/<family>:<revision>`).
const TASK_DEFINITION_ARN_SEGMENT: &str = "task-definition/";

/// How a stack read waits for the clock-owed spans of a deployment to come due: look again,
/// with a deadline generous enough that a loaded host still reaches it.
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const POLL_ATTEMPTS: usize = 240;

/// The emulator with the services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process callback CloudFormation makes signs its request either way (the
/// credential scope is what routes it).
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    // Amazon EC2 owns the VPC, the subnets and the security group the declarations place the
    // service's tasks into, and both generations of Elastic Load Balancing are mounted because
    // the CLASSIC service is the front door that routes an ELBv2 envelope to the ELBv2 model
    // (the two share one endpoint and one signing name) — exactly as the router mounts them.
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_elasticloadbalancing_elasticloadbalancing_v7::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    // AWS Identity and Access Management, for the execution role one declaration hands the
    // revision: the stack really creates it there, so the service that owns it is mounted
    // beside the rest or that resource's create is refused and the stack fails.
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request. The signature is never checked: this router runs with request
/// authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one Amazon ECS operation over its awsJson1_1 wire.
async fn ecs(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ecs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{ECS_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization("ecs"))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// The error code an awsJson answer carries, without the namespace a `__type` may qualify it
/// with.
fn error_code(body: &Value) -> &str {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or("<no __type>")
}

/// Read a stack until it reports a status that is no longer in progress, and answer that
/// read's body. The deadline is only here so a failure is a failure rather than a hang.
async fn poll_to_terminal(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let mut described = String::new();
    for _ in 0..POLL_ATTEMPTS {
        let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        described = body;
        if !opt_element(&described, "StackStatus")
            .unwrap_or_default()
            .ends_with("_IN_PROGRESS")
        {
            return described;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    described
}

/// The reason each FAILED resource of the stack carries, as `(logical id, reason)` — read from
/// the event member that carries both, so no event's reason is paired with another's id.
async fn failures_of(router: &MultiServiceRouter, stack: &str) -> Vec<(String, String)> {
    let (_, body) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    members_of(&body)
        .into_iter()
        .filter_map(|member| {
            let status = elements(member, "ResourceStatus").into_iter().next()?;
            if !status.ends_with("_FAILED") {
                return None;
            }
            let id = elements(member, "LogicalResourceId").into_iter().next()?;
            let reason = elements(member, "ResourceStatusReason")
                .into_iter()
                .next()
                .unwrap_or_default();
            Some((id, reason))
        })
        .collect()
}

/// Create a stack from `template`, read it to a terminal status that must be `CREATE_COMPLETE`,
/// and answer that read's body.
async fn deploy(router: &MultiServiceRouter, case: &str, stack: &str, template: &str) -> String {
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", template),
            // A template that declares an IAM resource is only deployed by a caller that
            // acknowledges it ("you must explicitly acknowledge this by specifying one of these
            // capabilities", `CreateStack$Capabilities`), which is what a client does too.
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let described = poll_to_terminal(router, case, stack).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to complete; reason {:?}; failed resources {:?}",
        opt_element(&described, "StackStatusReason"),
        failures_of(router, stack).await,
    );
    described
}

/// The stack's logical-id -> physical-id map, which is where a client reads the identifier a
/// declared resource was created under.
async fn stack_resources(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> BTreeMap<String, String> {
    let (status, body) =
        cloudformation(router, "DescribeStackResources", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with body \
         {body}"
    );
    physical_ids_of(&body)
}

/// The value of the stack output `key`, which every assertion below reads a resource's own
/// identifier out of — an output reported with no value would otherwise travel into a request as
/// `null`.
fn output(outputs: &BTreeMap<String, String>, case: &str, key: &str) -> String {
    outputs.get(key).cloned().unwrap_or_else(|| {
        panic!("case {case:?}: no {key:?} output with a value, got {outputs:?}")
    })
}

/// The ARN Amazon ECS identifies revision `revision` of `family` by.
fn task_definition_arn(family: &str, revision: i64) -> String {
    format!("arn:aws:ecs:{REGION}:{ACCOUNT_ID}:task-definition/{family}:{revision}")
}

/// The `(family, revision)` a task-definition ARN carries, which is what the two short handle
/// forms are spelled from.
fn family_and_revision(arn: &str) -> (String, String) {
    let path = arn
        .split_once(TASK_DEFINITION_ARN_SEGMENT)
        .map(|(_, path)| path)
        .unwrap_or_default();
    match path.rsplit_once(':') {
        Some((family, revision)) => (family.to_string(), revision.to_string()),
        None => (path.to_string(), String::new()),
    }
}

/// How one row spells the stack's revision on the ECS call — the three forms `CreateService`
/// documents for `taskDefinition`: "The family and revision (family:revision) or full ARN of
/// the task definition to run in your service. If a revision isn't specified, the latest ACTIVE
/// revision is used."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handle {
    /// The ARN the stack published, which is the recorded case.
    PublishedArn,
    /// `family:revision`, read off that ARN.
    FamilyRevision,
    /// The family alone, which is the latest ACTIVE revision in it.
    Family,
}

/// One deployment: the template, how the ECS call then names the revision it registered, and
/// what the created service must carry.
struct Row {
    case: &'static str,
    stack: &'static str,
    template: &'static str,
    /// The logical id of the declared `AWS::ECS::TaskDefinition`.
    task_definition: &'static str,
    /// The family the template states, or `None` where CloudFormation computes the name (the
    /// row then reads the family off the ARN the stack published).
    family: Option<&'static str>,
    /// How the ECS call spells the revision.
    handle: Handle,
    /// The service the ECS call creates, and the count it asks for.
    service_name: &'static str,
    desired_count: i64,
    /// The launch type the service runs on.
    launch_type: &'static str,
    /// The container the revision declares, and the port it publishes, for the load-balancer
    /// binding one row makes. `None` for a row that binds none.
    binding: Option<(&'static str, i64)>,
    /// Whether the service is placed with an `awsvpc` network configuration taken from the
    /// stack's own `SubnetIds` and `SecurityGroupId` outputs.
    awsvpc: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "the recorded shape: a Fargate revision the template names, placed in the stack's \
               own network and bound to the target group its listener forwards to, named by the \
               ARN the stack published",
        stack: "cfn-ecs-service-estate",
        // The load balancer the template declares is an INTERNAL one: the stack attaches no
        // internet gateway to the VPC it declares, and an internet-facing load balancer can
        // only be placed in a VPC that has one.
        template: r#"{
          "Resources": {
            "Vpc": {
              "Type": "AWS::EC2::VPC",
              "Properties": { "CidrBlock": "10.42.0.0/16" }
            },
            "SubnetA": {
              "Type": "AWS::EC2::Subnet",
              "Properties": {
                "VpcId": { "Ref": "Vpc" },
                "CidrBlock": "10.42.1.0/24",
                "AvailabilityZone": "us-east-1a"
              }
            },
            "SubnetB": {
              "Type": "AWS::EC2::Subnet",
              "Properties": {
                "VpcId": { "Ref": "Vpc" },
                "CidrBlock": "10.42.2.0/24",
                "AvailabilityZone": "us-east-1b"
              }
            },
            "TaskSecurityGroup": {
              "Type": "AWS::EC2::SecurityGroup",
              "Properties": {
                "GroupDescription": "the group that the stack places tasks in",
                "VpcId": { "Ref": "Vpc" }
              }
            },
            "LoadBalancer": {
              "Type": "AWS::ElasticLoadBalancingV2::LoadBalancer",
              "Properties": {
                "Name": "cfn-ecs-alb",
                "Scheme": "internal",
                "Subnets": [{ "Ref": "SubnetA" }, { "Ref": "SubnetB" }]
              }
            },
            "BlockedTargetGroup": {
              "Type": "AWS::ElasticLoadBalancingV2::TargetGroup",
              "Properties": {
                "Name": "cfn-ecs-blocked",
                "Port": 80,
                "Protocol": "HTTP",
                "TargetType": "ip",
                "VpcId": { "Ref": "Vpc" }
              }
            },
            "Listener": {
              "Type": "AWS::ElasticLoadBalancingV2::Listener",
              "Properties": {
                "LoadBalancerArn": { "Ref": "LoadBalancer" },
                "Port": 80,
                "Protocol": "HTTP",
                "DefaultActions": [{
                  "Type": "forward",
                  "TargetGroupArn": { "Ref": "BlockedTargetGroup" }
                }]
              }
            },
            "Cluster": {
              "Type": "AWS::ECS::Cluster",
              "Properties": { "ClusterName": "cfn-ecs-service-cluster" }
            },
            "ExecutionRole": {
              "Type": "AWS::IAM::Role",
              "Properties": {
                "AssumeRolePolicyDocument": {
                  "Version": "2012-10-17",
                  "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "Service": "ecs-tasks.amazonaws.com" },
                    "Action": "sts:AssumeRole"
                  }]
                }
              }
            },
            "BlockedTaskDef": {
              "Type": "AWS::ECS::TaskDefinition",
              "Properties": {
                "Family": "cfn-ecs-blocked-app",
                "Cpu": "256",
                "Memory": "512",
                "NetworkMode": "awsvpc",
                "RequiresCompatibilities": ["FARGATE"],
                "ExecutionRoleArn": { "Fn::GetAtt": ["ExecutionRole", "Arn"] },
                "ContainerDefinitions": [{
                  "Name": "app",
                  "Image": "public.ecr.aws/nginx/nginx:latest",
                  "Essential": true,
                  "PortMappings": [{ "ContainerPort": 80, "Protocol": "tcp" }]
                }]
              }
            }
          },
          "Outputs": {
            "ClusterName": { "Value": { "Ref": "Cluster" } },
            "BlockedTaskDefinitionArn": { "Value": { "Ref": "BlockedTaskDef" } },
            "BlockedTargetGroupArn": { "Value": { "Ref": "BlockedTargetGroup" } },
            "BlockedSecurityGroupId": { "Value": { "Ref": "TaskSecurityGroup" } },
            "SubnetIds": {
              "Value": { "Fn::Join": [",", [{ "Ref": "SubnetA" }, { "Ref": "SubnetB" }]] }
            }
          }
        }"#,
        task_definition: "BlockedTaskDef",
        family: Some("cfn-ecs-blocked-app"),
        handle: Handle::PublishedArn,
        service_name: "blocked-app",
        desired_count: 1,
        launch_type: "FARGATE",
        binding: Some(("app", 80)),
        awsvpc: true,
    },
    Row {
        case: "a Fargate revision whose family CloudFormation computes, placed in the stack's \
               network with no load balancer, named as family:revision in YAML",
        stack: "cfn-ecs-unnamed-family",
        template: concat!(
            "Resources:\n",
            "  Vpc:\n",
            "    Type: AWS::EC2::VPC\n",
            "    Properties:\n",
            "      CidrBlock: 10.43.0.0/16\n",
            "  SubnetA:\n",
            "    Type: AWS::EC2::Subnet\n",
            "    Properties:\n",
            "      VpcId: !Ref Vpc\n",
            "      CidrBlock: 10.43.1.0/24\n",
            "      AvailabilityZone: us-east-1a\n",
            "  TaskSecurityGroup:\n",
            "    Type: AWS::EC2::SecurityGroup\n",
            "    Properties:\n",
            "      GroupDescription: the group that the stack places tasks in\n",
            "      VpcId: !Ref Vpc\n",
            "  Cluster:\n",
            "    Type: AWS::ECS::Cluster\n",
            "  Worker:\n",
            "    Type: AWS::ECS::TaskDefinition\n",
            "    Properties:\n",
            "      Cpu: \"512\"\n",
            "      Memory: \"1024\"\n",
            "      NetworkMode: awsvpc\n",
            "      RequiresCompatibilities:\n",
            "        - FARGATE\n",
            "      ContainerDefinitions:\n",
            "        - Name: worker\n",
            "          Image: public.ecr.aws/nginx/nginx:latest\n",
            "          Essential: true\n",
            "Outputs:\n",
            "  ClusterName:\n",
            "    Value: !Ref Cluster\n",
            "  BlockedTaskDefinitionArn:\n",
            "    Value: !Ref Worker\n",
            "  BlockedSecurityGroupId:\n",
            "    Value: !Ref TaskSecurityGroup\n",
            "  SubnetIds:\n",
            "    Value: !Ref SubnetA\n",
        ),
        task_definition: "Worker",
        family: None,
        handle: Handle::FamilyRevision,
        service_name: "worker",
        desired_count: 2,
        launch_type: "FARGATE",
        binding: None,
        awsvpc: true,
    },
    Row {
        case: "a bridge revision the template names, run on EC2 capacity at a desired count of \
               zero and named by its family alone",
        stack: "cfn-ecs-bridge-family",
        template: concat!(
            "Resources:\n",
            "  Cluster:\n",
            "    Type: AWS::ECS::Cluster\n",
            "    Properties:\n",
            "      ClusterName: cfn-ecs-bridge-cluster\n",
            "  Batch:\n",
            "    Type: AWS::ECS::TaskDefinition\n",
            "    Properties:\n",
            "      Family: cfn-ecs-bridge-batch\n",
            "      NetworkMode: bridge\n",
            "      ContainerDefinitions:\n",
            "        - Name: batch\n",
            "          Image: public.ecr.aws/nginx/nginx:latest\n",
            "          Memory: 128\n",
            "          Essential: true\n",
            "Outputs:\n",
            "  ClusterName:\n",
            "    Value: !Ref Cluster\n",
            "  BlockedTaskDefinitionArn:\n",
            "    Value: !Ref Batch\n",
        ),
        task_definition: "Batch",
        family: Some("cfn-ecs-bridge-batch"),
        handle: Handle::Family,
        service_name: "batch",
        desired_count: 0,
        launch_type: "EC2",
        binding: None,
        awsvpc: false,
    },
];

#[tokio::test]
async fn a_stacks_task_definition_handle_creates_an_ecs_service() {
    let router = build_router();
    for row in ROWS {
        let described = deploy(&router, row.case, row.stack, row.template).await;
        let outputs = outputs_of(&described);
        let published = output(&outputs, row.case, "BlockedTaskDefinitionArn");
        let cluster = output(&outputs, row.case, "ClusterName");

        // The handle the stack published is the ARN of a revision of the declared family, at
        // the revision Amazon ECS allocated for it (the first of a family this stack created).
        let (family, revision) = family_and_revision(&published);
        if let Some(declared) = row.family {
            assert_eq!(
                published,
                task_definition_arn(declared, 1),
                "case {:?}: expected the stack to publish the registered revision's ARN, got \
                 {published:?}",
                row.case
            );
        } else {
            assert_eq!(
                (published.as_str(), revision.as_str()),
                (task_definition_arn(&family, 1).as_str(), "1"),
                "case {:?}: expected a task-definition ARN of the computed family, got \
                 {published:?}",
                row.case
            );
            assert!(
                family.starts_with(&format!("{}-{}-", row.stack, row.task_definition)),
                "case {:?}: expected CloudFormation's computed family, got {family:?}",
                row.case
            );
        }
        // And it is the identifier the stack reports the resource was created under.
        let resources = stack_resources(&router, row.case, row.stack).await;
        assert_eq!(
            resources.get(row.task_definition).map(String::as_str),
            Some(published.as_str()),
            "case {:?}: expected the stack resource to report the revision it registered, got \
             {resources:?}",
            row.case
        );

        // Amazon ECS's own read of that handle: the revision is there, ACTIVE.
        let (status, reported) = ecs(
            &router,
            "DescribeTaskDefinition",
            &json!({ "taskDefinition": published }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected ecs DescribeTaskDefinition on {published:?} to answer 200, got \
             {status} with body {reported}",
            row.case
        );
        assert_eq!(
            (
                reported["taskDefinition"]["taskDefinitionArn"].as_str(),
                reported["taskDefinition"]["family"].as_str(),
                reported["taskDefinition"]["status"].as_str(),
            ),
            (Some(published.as_str()), Some(family.as_str()), Some("ACTIVE")),
            "case {:?}: expected the registered revision, got {reported}",
            row.case
        );

        // The call the issue filed: a service that RUNS the stack's revision, named by this
        // row's handle form.
        let handle = match row.handle {
            Handle::PublishedArn => published.clone(),
            Handle::FamilyRevision => format!("{family}:{revision}"),
            Handle::Family => family.clone(),
        };
        let mut request = json!({
            "cluster": cluster,
            "serviceName": row.service_name,
            "taskDefinition": handle,
            "desiredCount": row.desired_count,
            "launchType": row.launch_type,
        });
        let network = if row.awsvpc {
            let subnets: Vec<&str> = outputs
                .get("SubnetIds")
                .map(String::as_str)
                .unwrap_or_default()
                .split(',')
                .filter(|subnet| !subnet.is_empty())
                .collect();
            assert!(
                !subnets.is_empty(),
                "case {:?}: expected the stack to publish its subnets, got {outputs:?}",
                row.case
            );
            let group = output(&outputs, row.case, "BlockedSecurityGroupId");
            let network = json!({
                "awsvpcConfiguration": {
                    "subnets": subnets,
                    "securityGroups": [group],
                    "assignPublicIp": "ENABLED",
                }
            });
            request["networkConfiguration"] = network.clone();
            Some(network)
        } else {
            None
        };
        let bound_group = row
            .binding
            .map(|_| output(&outputs, row.case, "BlockedTargetGroupArn"));
        if let Some((container, port)) = row.binding {
            request["loadBalancers"] = json!([{
                "targetGroupArn": bound_group,
                "containerName": container,
                "containerPort": port,
            }]);
        }
        let (status, created) = ecs(&router, "CreateService", &request).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected ecs CreateService on {handle:?} to answer 200, got {status} with \
             body {created}",
            row.case
        );
        // Whichever form named it, the created service runs the revision the stack registered,
        // reported under the ARN Amazon ECS knows that revision by.
        assert_eq!(
            (
                created["service"]["taskDefinition"].as_str(),
                created["service"]["serviceName"].as_str(),
                created["service"]["desiredCount"].as_i64(),
                created["service"]["launchType"].as_str(),
            ),
            (
                Some(published.as_str()),
                Some(row.service_name),
                Some(row.desired_count),
                Some(row.launch_type),
            ),
            "case {:?}: expected the service to run the stack's revision, got {created}",
            row.case
        );
        if let Some((container, port)) = row.binding {
            assert_eq!(
                (
                    created["service"]["loadBalancers"][0]["targetGroupArn"].as_str(),
                    created["service"]["loadBalancers"][0]["containerName"].as_str(),
                    created["service"]["loadBalancers"][0]["containerPort"].as_i64(),
                ),
                (
                    bound_group.as_deref(),
                    Some(container),
                    Some(port)
                ),
                "case {:?}: expected the service to bind the stack's target group, got {created}",
                row.case
            );
        }

        // And it is there on the read a client polls it with afterwards, running the same
        // revision: the create wrote state rather than answering out of the request.
        let (status, read) = ecs(
            &router,
            "DescribeServices",
            &json!({ "cluster": cluster, "services": [row.service_name] }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected ecs DescribeServices to answer 200, got {status} with body {read}",
            row.case
        );
        assert_eq!(
            (
                read["services"][0]["serviceName"].as_str(),
                read["services"][0]["taskDefinition"].as_str(),
                read["failures"].as_array().map(Vec::len),
            ),
            (
                Some(row.service_name),
                Some(published.as_str()),
                Some(0usize)
            ),
            "case {:?}: expected the stored service to read back running the stack's revision, \
             got {read}",
            row.case
        );

        // The rule holds for every ECS call that takes such a handle, not only the one the
        // issue filed: `RunTask` takes the same "family and revision (family:revision) or full
        // ARN of the task definition to run", so the stack's handle runs a task of that
        // revision too. Driven only where the row's revision needs no registered container
        // instance to be placed on (Fargate), since what is under test is the handle.
        if let Some(network) = network.as_ref() {
            let (status, ran) = ecs(
                &router,
                "RunTask",
                &json!({
                    "cluster": cluster,
                    "taskDefinition": handle,
                    "count": 1,
                    "launchType": row.launch_type,
                    "networkConfiguration": network,
                }),
            )
            .await;
            assert_eq!(
                (
                    status,
                    ran["tasks"][0]["taskDefinitionArn"].as_str(),
                    ran["failures"].as_array().map(Vec::len),
                ),
                (200, Some(published.as_str()), Some(0usize)),
                "case {:?}: expected ecs RunTask on {handle:?} to run the stack's revision, got \
                 {status} with body {ran}",
                row.case
            );
        }
    }
}

/// The stack the refusals below are taken against: one cluster and one revision, so what each
/// refused handle differs from the accepted one in is the revision it names.
const REFUSED_STACK: &str = "cfn-ecs-unregistered-handles";
const REFUSED_CLUSTER: &str = "cfn-ecs-unregistered-cluster";
const REFUSED_FAMILY: &str = "cfn-ecs-registered-once";

fn refusal_template() -> String {
    json!({
        "Resources": {
            "Cluster": {
                "Type": "AWS::ECS::Cluster",
                "Properties": { "ClusterName": REFUSED_CLUSTER }
            },
            "App": {
                "Type": "AWS::ECS::TaskDefinition",
                "Properties": {
                    "Family": REFUSED_FAMILY,
                    "NetworkMode": "bridge",
                    "ContainerDefinitions": [{
                        "Name": "app",
                        "Image": "public.ecr.aws/nginx/nginx:latest",
                        "Memory": 128,
                        "Essential": true
                    }]
                }
            }
        },
        "Outputs": { "TaskDefinitionArn": { "Value": { "Ref": "App" } } }
    })
    .to_string()
}

/// What makes the stack's handle work is the revision the deployment REGISTERED, so a handle of
/// the same shape naming a revision nothing registered is still refused — and refused as the
/// client fault Amazon ECS classifies an unresolvable `taskDefinition` as on a call that would
/// put it to work.
#[tokio::test]
async fn a_handle_no_deployment_registered_is_still_refused() {
    let router = build_router();
    let case = "a stack that registered exactly one revision";
    let described = deploy(&router, case, REFUSED_STACK, &refusal_template()).await;
    let registered = output(&outputs_of(&described), case, "TaskDefinitionArn");
    assert_eq!(
        registered,
        task_definition_arn(REFUSED_FAMILY, 1),
        "case {case:?}: expected the stack to publish the revision it registered, got \
         {registered:?}"
    );

    // Each row is a handle of one of the three documented forms that names no stored revision:
    // a family nothing registered, a revision of the registered family past the one that
    // exists, and the published ARN with its revision moved on.
    let rows: [(&str, String); 4] = [
        (
            "a family no deployment registered",
            "cfn-ecs-never-registered".to_string(),
        ),
        (
            "a revision of the registered family that does not exist",
            format!("{REFUSED_FAMILY}:2"),
        ),
        (
            "the published ARN with another revision",
            task_definition_arn(REFUSED_FAMILY, 7),
        ),
        (
            "an ARN of a family no deployment registered",
            task_definition_arn("cfn-ecs-never-registered", 1),
        ),
    ];
    for (index, (row, handle)) in rows.iter().enumerate() {
        let (status, refused) = ecs(
            &router,
            "CreateService",
            &json!({
                "cluster": REFUSED_CLUSTER,
                "serviceName": format!("unregistered-{index}"),
                "taskDefinition": handle,
                "desiredCount": 1,
                "launchType": "EC2",
            }),
        )
        .await;
        assert_eq!(
            (
                status,
                error_code(&refused),
                refused["message"].as_str().or(refused["Message"].as_str())
            ),
            (400, "ClientException", Some("TaskDefinition not found.")),
            "case {row:?}: expected ecs CreateService on {handle:?} to be refused, got {status} \
             with body {refused}"
        );
    }

    // The revision the deployment DID register is accepted on the same cluster, so the refusals
    // above are about the state behind the handle and not about the call.
    let (status, created) = ecs(
        &router,
        "CreateService",
        &json!({
            "cluster": REFUSED_CLUSTER,
            "serviceName": "registered",
            "taskDefinition": registered,
            "desiredCount": 0,
            "launchType": "EC2",
        }),
    )
    .await;
    assert_eq!(
        (status, created["service"]["taskDefinition"].as_str()),
        (200, Some(registered.as_str())),
        "case {case:?}: expected the stack's own revision to be accepted, got {status} with body \
         {created}"
    );
}
